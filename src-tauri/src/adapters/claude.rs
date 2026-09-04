use serde_json::Value;

use super::{AgentEvent, CliAdapter};
use crate::models::{CliId, CommandSpec, Job};

/// Claude Code 어댑터.
/// 실측 근거: `claude -p --output-format stream-json --verbose` (스파이크 0, 2026-09-02)
pub struct ClaudeAdapter;

impl CliAdapter for ClaudeAdapter {
    fn id(&self) -> CliId {
        CliId::Claude
    }

    fn probe_command(&self) -> CommandSpec {
        CommandSpec {
            program: "claude".into(),
            args: vec!["--version".into()],
            env: vec![],
            cwd: String::new(),
        }
    }

    fn build_command(&self, job: &Job) -> CommandSpec {
        let mut args = vec![
            "-p".to_string(),
            job.request.clone(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--permission-mode".into(),
        ];
        // 파일 쓰기 허용 여부 → --permission-mode 매핑 (스파이크 2차 결과)
        args.push(if job.allow_writes { "acceptEdits".into() } else { "plan".into() });
        CommandSpec {
            program: "claude".into(),
            args,
            env: vec![],
            cwd: job.project_dir.clone(),
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
            // 핵심 한도 신호: 윈도우별 utilization·resetsAt (five_hour / seven_day)
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
}
