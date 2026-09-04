use serde_json::Value;

use super::{probe_detail, AgentEvent, CliAdapter, LoginFlow, ModelListing};
use crate::availability::{Evidence, ProbeOutcome};
use crate::models::{CliId, CommandSpec, Job, ModelOption};

/// Antigravity CLI(`agy`) 어댑터 — Google AI Pro/Ultra 구독의 Gemini 접근 경로 (2026-06-18부터 Gemini CLI 개인 계정 대체).
/// 근거: `agy --help`(1.1.26)와 공식 헤드리스 문서(2026-09-04 확인).
/// - 실행: `agy -p <프롬프트> --output-format stream-json --mode plan|accept-edits [--dangerously-skip-permissions] [--model X]`
///   이벤트: `init`(conversation_id) / `step_update`(tool_info·텍스트) / `result`(status·response·usage)
/// - 재개: `--conversation <id>`
/// - probe: `agy models` — 미로그인이면 "Please sign in" (인증 서브커맨드 없음, 첫 대화형 실행에서 브라우저 로그인)
/// - 모델: `agy models` 목록. 사용량: TUI `/usage`만 있고 헤드리스 신호 없음 → 추정 경로
/// - 스킬: 워크스페이스 `.agents/skills/`(위키 저장소 구조와 일치), 전역 `~/.gemini/antigravity-cli/skills/`
/// agy.exe는 네이티브 실행 파일이라 여러 줄 프롬프트를 인자로 넘길 수 있다.
pub struct AntigravityAdapter;

const PRINT_TIMEOUT: &str = "30m";

impl AntigravityAdapter {
    fn common_args(job: &Job) -> Vec<String> {
        let mut args = vec![
            "--output-format".to_string(),
            "stream-json".into(),
            "--mode".into(),
            if job.allow_writes { "accept-edits" } else { "plan" }.into(),
            "--print-timeout".into(),
            PRINT_TIMEOUT.into(),
        ];
        if job.allow_writes {
            // 헤드리스에서는 명령 실행 권한 요청이 soft-deny되므로, 쓰기 허용 작업은 자동 승인으로
            args.push("--dangerously-skip-permissions".into());
        }
        if let Some(m) = job.model.as_deref().filter(|m| !m.is_empty()) {
            args.push("--model".into());
            args.push(m.to_string());
        }
        args
    }
}

impl CliAdapter for AntigravityAdapter {
    fn id(&self) -> CliId {
        CliId::Antigravity
    }

    fn probe_command(&self) -> CommandSpec {
        CommandSpec {
            program: "agy".into(),
            args: vec!["models".into()],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        }
    }

    fn build_command(&self, job: &Job) -> CommandSpec {
        let mut args = vec!["-p".to_string(), job.request.clone()];
        args.extend(Self::common_args(job));
        CommandSpec {
            program: "agy".into(),
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
        match v.get("event").and_then(Value::as_str).unwrap_or("") {
            "init" => v
                .get("conversation_id")
                .and_then(Value::as_str)
                .map(|id| {
                    vec![AgentEvent::SessionStarted {
                        session_id: id.to_string(),
                    }]
                })
                .unwrap_or_default(),
            "step_update" => {
                let Some(su) = v.get("step_update") else {
                    return vec![];
                };
                let mut out = vec![];
                if let Some(tool) = su.get("tool_info") {
                    let name = tool.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let detail = tool
                        .get("parameters")
                        .map(|p| p.to_string())
                        .unwrap_or_default();
                    // 같은 도구 단계가 상태 전이마다 반복되므로 DONE만 보고한다
                    if su.get("state").and_then(Value::as_str) == Some("DONE") {
                        out.push(AgentEvent::ToolUse {
                            tool: name.to_string(),
                            detail,
                        });
                    }
                }
                // 실측(1.1.26): agent_response 단계의 본문은 `text_delta`로 온다
                for key in ["text_delta", "text", "content", "delta"] {
                    if let Some(t) = su.get(key).and_then(Value::as_str) {
                        if !t.is_empty() {
                            out.push(AgentEvent::Message {
                                text: t.to_string(),
                                delta: true,
                            });
                        }
                        break;
                    }
                }
                out
            }
            "result" => {
                let r = v.get("result").unwrap_or(&v);
                let status = r.get("status").and_then(Value::as_str).unwrap_or("");
                let ok = status == "SUCCESS";
                let response = r
                    .get("response")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let error = r
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let mut out = vec![];
                if let Some(id) = r.get("conversation_id").and_then(Value::as_str) {
                    out.push(AgentEvent::SessionStarted {
                        session_id: id.to_string(),
                    });
                }
                out.push(AgentEvent::Completed {
                    ok,
                    // 프론트는 본문 스트리밍이 없었을 때만 summary를 표시한다
                    summary: if ok {
                        response
                    } else if error.is_empty() {
                        format!("{status}: {response}")
                    } else {
                        error
                    },
                });
                out
            }
            _ => vec![],
        }
    }

    fn build_resume_command(&self, job: &Job, session_id: &str) -> Option<CommandSpec> {
        let mut spec = self.build_command(job);
        spec.args.push("--conversation".into());
        spec.args.push(session_id.to_string());
        Some(spec)
    }

    /// 대화형 `agy`를 콘솔 창에서 띄우면 브라우저 로그인이 시작된다. 로그인 후 `/exit`로 나오면 창이 닫히며 재검사.
    fn login_flow(&self) -> Option<LoginFlow> {
        Some(LoginFlow::Console {
            spec: CommandSpec {
                program: "agy".into(),
                args: vec![],
                env: vec![],
                cwd: String::new(),
                stdin: None,
            },
            hint: "콘솔 창의 agy가 브라우저를 엽니다. AI Pro를 쓰는 Google 계정으로 로그인한 뒤 /exit로 나오세요.".into(),
        })
    }

    fn model_listing(&self) -> ModelListing {
        ModelListing::Command(self.probe_command())
    }

    /// `agy models` 출력의 각 줄에서 첫 토큰을 모델 id로 본다 (헤더·안내 줄 제외)
    fn parse_models(&self, lines: &[String]) -> Vec<ModelOption> {
        lines
            .iter()
            .map(|l| super::strip_ansi(l).trim().to_string())
            .filter(|l| !l.is_empty())
            .filter(|l| {
                let low = l.to_ascii_lowercase();
                !low.starts_with("fetching")
                    && !low.starts_with("available")
                    && !low.starts_with("error")
                    && !low.starts_with("please")
                    && !low.starts_with("model")
                    && !l.starts_with('-')
            })
            .filter_map(|l| {
                let first = l.split_whitespace().next()?;
                let id = first.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' || c == '/'));
                if id.is_empty() || !id.chars().any(|c| c.is_ascii_alphabetic()) {
                    return None;
                }
                let rest = l[first.len()..].trim().trim_start_matches(['-', ':', '|']).trim();
                let is_default = l.to_ascii_lowercase().contains("default") || l.contains('*');
                Some(ModelOption {
                    id: id.to_string(),
                    label: if rest.is_empty() {
                        id.to_string()
                    } else {
                        format!("{id} — {rest}")
                    },
                    is_default,
                })
            })
            .collect()
    }

    fn interpret_probe(&self, code: Option<i32>, stdout: &str, stderr: &str) -> ProbeOutcome {
        let text = super::strip_ansi(&format!("{stdout}\n{stderr}"));
        let low = text.to_ascii_lowercase();
        if low.contains("sign in") || low.contains("authentication required") || low.contains("log in") {
            return ProbeOutcome::AuthRequired {
                detail: probe_detail(code, stdout, stderr),
            };
        }
        match code {
            Some(0) => ProbeOutcome::Ready {
                evidence: Evidence::CliReported,
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
            request: "line one\nline two".into(),
            project_dir: "D:\\dev".into(),
            profile: "코딩 작업".into(),
            allow_writes,
            unattended_ok: false,
            status: JobStatus::Starting,
            model: Some("gemini-3.1-pro".into()),
        }
    }

    #[test]
    fn commands_are_assembled() {
        let spec = AntigravityAdapter.build_command(&job(false));
        assert_eq!(spec.program, "agy");
        assert_eq!(&spec.args[..2], ["-p", "line one\nline two"]);
        assert!(spec.args.windows(2).any(|w| w == ["--output-format", "stream-json"]));
        assert!(spec.args.windows(2).any(|w| w == ["--mode", "plan"]));
        assert!(!spec.args.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(spec.args.windows(2).any(|w| w == ["--model", "gemini-3.1-pro"]));

        let spec = AntigravityAdapter.build_resume_command(&job(true), "conv-1").unwrap();
        assert!(spec.args.windows(2).any(|w| w == ["--mode", "accept-edits"]));
        assert!(spec.args.iter().any(|a| a == "--dangerously-skip-permissions"));
        assert!(spec.args.windows(2).any(|w| w == ["--conversation", "conv-1"]));
    }

    /// 공식 헤드리스 문서의 이벤트 예시 형태
    #[test]
    fn events_are_parsed() {
        let init = r#"{"event":"init","conversation_id":"c-1","init":{"cwd":"D:/dev","tools":[],"permission_mode":"request-review"}}"#;
        assert!(matches!(&AntigravityAdapter.parse_event(init)[0], AgentEvent::SessionStarted { session_id } if session_id == "c-1"));
        let delta = r#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"AGY_OK"}}"#;
        assert!(matches!(&AntigravityAdapter.parse_event(delta)[0], AgentEvent::Message { text, delta: true } if text == "AGY_OK"));
        let user = r#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":0,"state":"DONE","step_type":"user_input"}}"#;
        assert!(AntigravityAdapter.parse_event(user).is_empty());
        let tool = r#"{"event":"step_update","step_update":{"conversation_id":"c-1","step_index":1,"state":"DONE","step_type":"tool","tool_info":{"name":"read_file","parameters":{"path":"a.rs"},"output":"..."}}}"#;
        assert!(matches!(&AntigravityAdapter.parse_event(tool)[0], AgentEvent::ToolUse { tool, .. } if tool == "read_file"));
        let result = r#"{"event":"result","result":{"conversation_id":"c-1","status":"SUCCESS","response":"AGY_OK","duration_seconds":7.1,"num_turns":1,"usage":{"input_tokens":10,"output_tokens":2}}}"#;
        let evs = AntigravityAdapter.parse_event(result);
        assert!(evs.iter().any(|e| matches!(e, AgentEvent::Completed { ok: true, summary } if summary == "AGY_OK")));
        let failed = r#"{"event":"result","result":{"conversation_id":"c-1","status":"ERROR","response":"","error":"quota exceeded"}}"#;
        assert!(AntigravityAdapter.parse_event(failed).iter().any(|e| matches!(e, AgentEvent::Completed { ok: false, summary } if summary == "quota exceeded")));
    }

    #[test]
    fn probe_and_models() {
        assert!(matches!(
            AntigravityAdapter.interpret_probe(Some(1), "Fetching available models...\n", "Error: Please sign in to view available models. Launch the CLI without arguments to sign in."),
            ProbeOutcome::AuthRequired { .. }
        ));
        assert!(matches!(
            AntigravityAdapter.interpret_probe(Some(0), "gemini-3.1-pro  Gemini 3.1 Pro\n", ""),
            ProbeOutcome::Ready { evidence: Evidence::CliReported, .. }
        ));
        // 실측 출력은 탭 구분 "id	Name"
        let models = AntigravityAdapter.parse_models(&[
            "Fetching available models...".into(),
            "gemini-3.1-pro-high	Gemini 3.1 Pro (High)".into(),
            "claude-opus-4-6-thinking	Claude Opus 4.6 (Thinking)".into(),
            "".into(),
        ]);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gemini-3.1-pro-high");
        assert_eq!(models[0].label, "gemini-3.1-pro-high — Gemini 3.1 Pro (High)");
        assert_eq!(models[1].id, "claude-opus-4-6-thinking");
    }
}
