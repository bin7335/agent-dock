use serde_json::Value;

use super::{probe_detail, strip_ansi, AgentEvent, CliAdapter, LoginFlow, ModelListing};
use crate::availability::{AccountInfo, Evidence, ProbeOutcome};
use crate::models::{CliId, CommandSpec, Job, ModelOption};

/// `opencode auth list`의 "●  OpenCode Zen api" 줄들 → 제공자 목록 (이메일은 주지 않는다)
fn providers_from_auth_list(text: &str) -> Option<AccountInfo> {
    let mut names = Vec::new();
    let mut methods = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('●') else {
            continue;
        };
        let rest = rest.trim();
        let (name, method) = match rest.rsplit_once(' ') {
            Some((n, m)) if ["api", "oauth", "key"].contains(&m) => (n.trim(), Some(m)),
            _ => (rest, None),
        };
        if !name.is_empty() {
            names.push(name.to_string());
        }
        if let Some(m) = method {
            if !methods.contains(&m.to_string()) {
                methods.push(m.to_string());
            }
        }
    }
    if names.is_empty() {
        return None;
    }
    Some(AccountInfo {
        label: names.join(", "),
        plan: None,
        method: if methods.is_empty() {
            None
        } else {
            Some(methods.join("/"))
        },
    })
}

/// OpenCode 어댑터 (1.18.5 실측, 2026-09-04).
/// - 실행: `opencode run --format json --dir <폴더> [--agent plan|build] [--auto] [-m provider/model] <프롬프트>`
///   이벤트: `step_start` / `text`(part.text) / `step_finish`(part.reason) — 모든 이벤트에 `sessionID`
/// - 재개: `--session <id>`
/// - probe: `opencode auth list` → "N credentials"
/// - 로그인: `opencode auth login`(대화형 TUI, 콘솔 창). Claude 구독 OAuth는 연결하지 않는다 (PRD 설계 결정).
/// - 모델: `opencode models` → `provider/model` 한 줄씩
/// 사용량 신호는 없다(추정 경로). 프롬프트는 인자 전달이라 줄바꿈이 든 메시지는 거부된다 (TODO).
pub struct OpenCodeAdapter;

/// 줄바꿈이 든 프롬프트는 .cmd 셔임(cmd.exe) 인자로 못 넘기므로 임시 파일에 써서 `-f`로 첨부하고,
/// 메시지 자리에는 첨부를 읽으라는 한 줄 지시를 둔다 (1.18.5 실측: `-f`는 배열 옵션이라 메시지 뒤에 둬야 함).
/// 한 줄 프롬프트는 그대로 위치 인자로.
fn prompt_args(request: &str) -> Vec<String> {
    if !request.contains('\n') && !request.contains('\r') {
        return vec![request.to_string()];
    }
    let dir = std::env::temp_dir().join("agent-dock");
    let _ = std::fs::create_dir_all(&dir);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("opencode-prompt-{nanos}.md"));
    if std::fs::write(&path, request).is_err() {
        // 파일을 못 쓰면 줄바꿈을 공백으로 눌러서라도 보낸다
        return vec![request.replace(['\r', '\n'], " ")];
    }
    vec![
        "The user's message is in the attached file. Read it in full and respond to it exactly as if it had been typed here."
            .into(),
        "-f".into(),
        path.to_string_lossy().into_owned(),
    ]
}

impl OpenCodeAdapter {
    fn common_args(job: &Job) -> Vec<String> {
        let mut args = vec![
            "run".to_string(),
            "--format".into(),
            "json".into(),
            "--dir".into(),
            job.project_dir.clone(),
            "--agent".into(),
            // 내장 에이전트: plan(읽기 전용) / build(기본). 쓰기 허용 시 권한 프롬프트가 헤드리스를 막지 않도록 --auto
            if job.allow_writes { "build" } else { "plan" }.into(),
        ];
        if job.allow_writes {
            args.push("--auto".into());
        }
        if let Some(m) = job.model.as_deref().filter(|m| !m.is_empty()) {
            args.push("-m".into());
            args.push(m.to_string());
        }
        args
    }
}

impl CliAdapter for OpenCodeAdapter {
    fn id(&self) -> CliId {
        CliId::Opencode
    }

    fn probe_command(&self) -> CommandSpec {
        CommandSpec {
            program: "opencode".into(),
            args: vec!["auth".into(), "list".into()],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        }
    }

    fn build_command(&self, job: &Job) -> CommandSpec {
        let mut args = Self::common_args(job);
        args.extend(prompt_args(&job.request));
        CommandSpec {
            program: "opencode".into(),
            args,
            env: vec![],
            cwd: job.project_dir.clone(),
            stdin: None,
        }
    }

    fn parse_event(&self, line: &str) -> Vec<AgentEvent> {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let mut out = vec![];
        if let Some(sid) = v.get("sessionID").and_then(Value::as_str) {
            // 프론트는 첫 session_started만 기억하므로 매 이벤트마다 보내도 무해
            out.push(AgentEvent::SessionStarted {
                session_id: sid.to_string(),
            });
        }
        let part = v.get("part");
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                let text = part
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !text.is_empty() {
                    out.push(AgentEvent::Message {
                        text: text.to_string(),
                        delta: false,
                    });
                }
            }
            "tool" | "tool_use" | "tool_call" => out.push(AgentEvent::ToolUse {
                tool: part
                    .and_then(|p| p.get("tool"))
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string(),
                detail: part
                    .and_then(|p| p.get("state"))
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            }),
            "step_finish" => {
                let reason = part
                    .and_then(|p| p.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                out.push(AgentEvent::Completed {
                    ok: reason != "error",
                    summary: String::new(),
                });
            }
            "error" => out.push(AgentEvent::Completed {
                ok: false,
                summary: v.to_string(),
            }),
            _ => {}
        }
        out
    }

    fn build_resume_command(&self, job: &Job, session_id: &str) -> Option<CommandSpec> {
        let mut args = Self::common_args(job);
        args.push("--session".into());
        args.push(session_id.to_string());
        args.extend(prompt_args(&job.request));
        Some(CommandSpec {
            program: "opencode".into(),
            args,
            env: vec![],
            cwd: job.project_dir.clone(),
            stdin: None,
        })
    }

    fn version_command(&self) -> Option<CommandSpec> {
        Some(CommandSpec {
            program: "opencode".into(),
            args: vec!["--version".into()],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        })
    }

    fn login_flow(&self) -> Option<LoginFlow> {
        Some(LoginFlow::Console {
            spec: CommandSpec {
                program: "opencode".into(),
                args: vec!["auth".into(), "login".into()],
                env: vec![],
                cwd: String::new(),
                stdin: None,
            },
            hint: "제공자를 고르고 API 키를 입력하세요. Claude 구독(OAuth)은 약관상 OpenCode에 연결하지 않습니다.".into(),
        })
    }

    fn model_listing(&self) -> ModelListing {
        ModelListing::Command(CommandSpec {
            program: "opencode".into(),
            args: vec!["models".into()],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        })
    }

    fn parse_models(&self, lines: &[String]) -> Vec<ModelOption> {
        lines
            .iter()
            .map(|l| strip_ansi(l).trim().to_string())
            .filter(|l| !l.is_empty() && l.contains('/') && !l.contains(' '))
            .map(|l| ModelOption {
                id: l.clone(),
                label: l,
                is_default: false,
            })
            .collect()
    }

    /// `opencode auth list` → "… N credentials". 0이면 로그인 필요, 실행 실패면 미설치
    fn interpret_probe(&self, code: Option<i32>, stdout: &str, stderr: &str) -> ProbeOutcome {
        let text = strip_ansi(&format!("{stdout}\n{stderr}"));
        let count = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .find(|w| w[1].starts_with("credential"))
            .and_then(|w| w[0].parse::<u32>().ok());
        match (code, count) {
            (Some(0), Some(0)) => ProbeOutcome::AuthRequired {
                detail: "opencode auth list: 0 credentials".into(),
            },
            (Some(0), Some(_)) => ProbeOutcome::Ready {
                evidence: Evidence::CliReported,
                version: None,
                account: providers_from_auth_list(&text),
            },
            (Some(0), None) => ProbeOutcome::Ready {
                evidence: Evidence::Estimated,
                version: None,
                account: None,
            },
            _ => ProbeOutcome::Unavailable {
                detail: probe_detail(code, stdout, stderr),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::JobStatus;

    fn job(allow_writes: bool) -> Job {
        Job {
            id: 0,
            title: "t".into(),
            request: "hello".into(),
            project_dir: "D:\\dev".into(),
            profile: "코딩 작업".into(),
            allow_writes,
            unattended_ok: false,
            status: JobStatus::Starting,
            model: Some("opencode/gpt-5.3-codex".into()),
        }
    }

    /// 2026-09-04 실측 `opencode run --format json` 이벤트
    #[test]
    fn events_are_parsed() {
        let text = r#"{"type":"text","timestamp":1,"sessionID":"ses_1","part":{"id":"p","type":"text","text":"OC_OK"}}"#;
        let evs = OpenCodeAdapter.parse_event(text);
        assert!(matches!(&evs[0], AgentEvent::SessionStarted { session_id } if session_id == "ses_1"));
        assert!(matches!(&evs[1], AgentEvent::Message { text, .. } if text == "OC_OK"));
        let fin = r#"{"type":"step_finish","sessionID":"ses_1","part":{"type":"step-finish","reason":"stop","tokens":{"total":7468}}}"#;
        assert!(OpenCodeAdapter
            .parse_event(fin)
            .iter()
            .any(|e| matches!(e, AgentEvent::Completed { ok: true, .. })));
    }

    #[test]
    fn commands_and_probe() {
        let spec = OpenCodeAdapter.build_command(&job(false));
        assert_eq!(&spec.args[..3], ["run", "--format", "json"]);
        assert!(spec.args.windows(2).any(|w| w == ["--agent", "plan"]));
        assert!(!spec.args.iter().any(|a| a == "--auto"));
        assert!(spec.args.windows(2).any(|w| w == ["-m", "opencode/gpt-5.3-codex"]));
        assert_eq!(spec.args.last().map(String::as_str), Some("hello"));

        let spec = OpenCodeAdapter.build_resume_command(&job(true), "ses_9").unwrap();
        assert!(spec.args.windows(2).any(|w| w == ["--agent", "build"]));
        assert!(spec.args.iter().any(|a| a == "--auto"));
        assert!(spec.args.windows(2).any(|w| w == ["--session", "ses_9"]));

        // 여러 줄 프롬프트 → 임시 파일 첨부 (메시지 뒤에 -f)
        let mut multi = job(false);
        multi.request = "line one\nline two".into();
        let spec = OpenCodeAdapter.build_command(&multi);
        let f = spec.args.iter().position(|a| a == "-f").expect("-f 있어야 함");
        assert!(spec.args[f - 1].contains("attached file"));
        let path = &spec.args[f + 1];
        assert_eq!(std::fs::read_to_string(path).unwrap(), "line one\nline two");
        let _ = std::fs::remove_file(path);

        let listed = "\u{1b}[90m┌\u{1b}[39m  Credentials\n\u{1b}[34m●\u{1b}[39m  OpenCode Zen \u{1b}[90mapi\u{1b}[39m\n\u{1b}[34m●\u{1b}[39m  OpenCode Go api\n\u{1b}[90m└\u{1b}[39m  2 credentials\n";
        match OpenCodeAdapter.interpret_probe(Some(0), listed, "") {
            ProbeOutcome::Ready {
                evidence: Evidence::CliReported,
                account: Some(acc),
                ..
            } => {
                assert_eq!(acc.label, "OpenCode Zen, OpenCode Go");
                assert_eq!(acc.method.as_deref(), Some("api"));
            }
            other => panic!("예상 밖: {other:?}"),
        }
        assert!(matches!(
            OpenCodeAdapter.interpret_probe(Some(0), "0 credentials", ""),
            ProbeOutcome::AuthRequired { .. }
        ));
        let models = OpenCodeAdapter.parse_models(&[
            "opencode/gpt-5.3-codex".into(),
            "".into(),
            "opencode/claude-sonnet-5".into(),
        ]);
        assert_eq!(models.len(), 2);
        assert_eq!(models[1].id, "opencode/claude-sonnet-5");
    }
}
