use serde_json::Value;

use super::{model_opt, probe_detail, AgentEvent, CliAdapter, LoginFlow, ModelListing};
use crate::availability::{Evidence, ProbeOutcome};
use crate::models::{CliId, CommandSpec, Job};

/// Claude Code 어댑터.
/// 실측 근거: `claude -p --output-format stream-json --verbose` (스파이크 0, 2026-09-02),
/// stdin 프롬프트 전달과 `claude auth status` JSON, `claude auth login`, `--model` 별칭(fable/opus/sonnet) (2026-09-04 실측).
pub struct ClaudeAdapter;

impl ClaudeAdapter {
    /// 프롬프트는 인자가 아니라 stdin으로 넘긴다 — 여러 줄·특수문자 프롬프트를 cmd 이스케이프에 태우지 않기 위해.
    fn base_args(job: &Job) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--permission-mode".into(),
            // 파일 쓰기 허용 여부 → --permission-mode 매핑 (스파이크 2차 결과)
            if job.allow_writes { "acceptEdits" } else { "plan" }.into(),
        ];
        if let Some(m) = job.model.as_deref().filter(|m| !m.is_empty()) {
            args.push("--model".into());
            args.push(m.to_string());
        }
        args
    }
}

impl CliAdapter for ClaudeAdapter {
    fn id(&self) -> CliId {
        CliId::Claude
    }

    /// `claude auth status`는 `{"loggedIn": true, "subscriptionType": "max", ...}` JSON을 돌려준다 (2.1.259 실측)
    fn probe_command(&self) -> CommandSpec {
        CommandSpec {
            program: "claude".into(),
            args: vec!["auth".into(), "status".into()],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        }
    }

    fn build_command(&self, job: &Job) -> CommandSpec {
        CommandSpec {
            program: "claude".into(),
            args: Self::base_args(job),
            env: vec![],
            cwd: job.project_dir.clone(),
            stdin: Some(job.request.clone()),
        }
    }

    fn parse_event(&self, line: &str) -> Vec<AgentEvent> {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "system" => {
                if v.get("subtype").and_then(Value::as_str) == Some("init") {
                    vec![AgentEvent::SessionStarted {
                        session_id: v
                            .get("session_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    }]
                } else {
                    vec![]
                }
            }
            // 핵심 한도 신호: 윈도우별 utilization·resetsAt (five_hour / seven_day / seven_day_overage_included)
            "rate_limit_event" => {
                let mut out = vec![];
                if let Some(windows) = v
                    .pointer("/rate_limit_info/unifiedWindows")
                    .and_then(Value::as_object)
                {
                    for (name, w) in windows {
                        out.push(AgentEvent::RateLimit {
                            window: name.clone(),
                            utilization: w
                                .get("utilization")
                                .and_then(Value::as_f64)
                                .unwrap_or(0.0),
                            resets_at: w.get("resetsAt").and_then(Value::as_i64).unwrap_or(0),
                        });
                    }
                }
                out
            }
            "assistant" => {
                let mut out = vec![];
                if let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) {
                    for b in blocks {
                        match b.get("type").and_then(Value::as_str) {
                            Some("tool_use") => out.push(AgentEvent::ToolUse {
                                tool: b
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                detail: b.get("input").map(|i| i.to_string()).unwrap_or_default(),
                            }),
                            Some("text") => out.push(AgentEvent::Message {
                                text: b
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                delta: false,
                            }),
                            _ => {}
                        }
                    }
                }
                out
            }
            "result" => vec![AgentEvent::Completed {
                ok: !v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                summary: v
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }],
            _ => vec![],
        }
    }

    /// `--resume <session_id>`로 같은 대화를 이어간다 (claude --help 실측).
    fn build_resume_command(&self, job: &Job, session_id: &str) -> Option<CommandSpec> {
        let mut spec = self.build_command(job);
        spec.args.push("--resume".into());
        spec.args.push(session_id.to_string());
        Some(spec)
    }

    fn login_flow(&self) -> Option<LoginFlow> {
        Some(LoginFlow::Console {
            spec: CommandSpec {
                program: "claude".into(),
                args: vec!["auth".into(), "login".into()],
                env: vec![],
                cwd: String::new(),
                stdin: None,
            },
            hint: "브라우저가 열리면 Anthropic 계정으로 로그인하세요. 콘솔 창에 코드 입력을 요구하면 그대로 따릅니다.".into(),
        })
    }

    /// `--model`은 별칭(fable/opus/sonnet/haiku) 또는 전체 이름을 받는다 (claude --help 실측)
    fn model_listing(&self) -> ModelListing {
        ModelListing::Static(vec![
            model_opt("fable", "Fable (최신)", false),
            model_opt("opus", "Opus", false),
            model_opt("sonnet", "Sonnet", false),
            model_opt("haiku", "Haiku", false),
        ])
    }

    fn interpret_probe(&self, code: Option<i32>, stdout: &str, stderr: &str) -> ProbeOutcome {
        if let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) {
            return match v.get("loggedIn").and_then(Value::as_bool) {
                Some(true) => ProbeOutcome::Ready {
                    evidence: Evidence::CliReported,
                    version: None,
                },
                Some(false) => ProbeOutcome::AuthRequired {
                    detail: "claude auth status: loggedIn=false".into(),
                },
                None => ProbeOutcome::Ready {
                    evidence: Evidence::Estimated,
                    version: None,
                },
            };
        }
        let text = format!("{stdout}\n{stderr}").to_ascii_lowercase();
        if text.contains("not logged in") || text.contains("login") {
            ProbeOutcome::AuthRequired {
                detail: probe_detail(code, stdout, stderr),
            }
        } else {
            ProbeOutcome::Unavailable {
                detail: probe_detail(code, stdout, stderr),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::JobStatus;

    fn job(allow_writes: bool, model: Option<&str>) -> Job {
        Job {
            id: 0,
            title: "t".into(),
            request: "line one\nline two".into(),
            project_dir: "D:\\x".into(),
            profile: "코딩 작업".into(),
            allow_writes,
            unattended_ok: false,
            status: JobStatus::Starting,
            model: model.map(String::from),
        }
    }

    #[test]
    fn prompt_goes_through_stdin_not_args() {
        let spec = ClaudeAdapter.build_command(&job(false, None));
        assert_eq!(spec.stdin.as_deref(), Some("line one\nline two"));
        assert!(!spec.args.iter().any(|a| a.contains("line one")));
        assert!(spec
            .args
            .windows(2)
            .any(|w| w == ["--permission-mode", "plan"]));
        assert!(!spec.args.iter().any(|a| a == "--model"));
        let spec = ClaudeAdapter.build_command(&job(true, Some("fable")));
        assert!(spec
            .args
            .windows(2)
            .any(|w| w == ["--permission-mode", "acceptEdits"]));
        assert!(spec.args.windows(2).any(|w| w == ["--model", "fable"]));
    }

    #[test]
    fn resume_appends_session() {
        let spec = ClaudeAdapter
            .build_resume_command(&job(false, None), "abc")
            .unwrap();
        assert_eq!(&spec.args[spec.args.len() - 2..], ["--resume", "abc"]);
        assert!(spec.stdin.is_some());
    }

    #[test]
    fn probe_json_is_interpreted() {
        let ok = ClaudeAdapter.interpret_probe(
            Some(0),
            "{\"loggedIn\": true, \"subscriptionType\": \"max\"}",
            "",
        );
        assert_eq!(
            ok,
            ProbeOutcome::Ready {
                evidence: Evidence::CliReported,
                version: None
            }
        );
        let no = ClaudeAdapter.interpret_probe(Some(0), "{\"loggedIn\": false}", "");
        assert!(matches!(no, ProbeOutcome::AuthRequired { .. }));
        let missing = ClaudeAdapter.interpret_probe(None, "", "'claude' is not recognized");
        assert!(matches!(missing, ProbeOutcome::Unavailable { .. }));
    }

    #[test]
    fn rate_limit_event_yields_all_windows() {
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"unifiedWindows":{"five_hour":{"utilization":0.08,"resetsAt":1788511200},"seven_day":{"utilization":0.01,"resetsAt":1789099200}}}}"#;
        let evs = ClaudeAdapter.parse_event(line);
        assert_eq!(evs.len(), 2);
        assert!(evs.iter().any(|e| matches!(
            e,
            AgentEvent::RateLimit { window, utilization, resets_at }
                if window == "five_hour" && (*utilization - 0.08).abs() < 1e-9 && *resets_at == 1788511200
        )));
    }

    #[test]
    fn login_and_models() {
        assert!(matches!(ClaudeAdapter.login_flow(), Some(LoginFlow::Console { .. })));
        match ClaudeAdapter.model_listing() {
            ModelListing::Static(v) => assert!(v.iter().any(|m| m.id == "fable")),
            _ => panic!("정적 목록이어야 함"),
        }
    }
}
