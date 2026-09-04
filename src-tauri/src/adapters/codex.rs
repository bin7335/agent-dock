use serde_json::Value;

use super::{AgentEvent, CliAdapter};
use crate::models::{CliId, CommandSpec, Job};

/// Codex 어댑터.
/// 실측 근거: `codex exec --json` JSONL (스파이크 0, 2026-09-02).
/// 사용량 %는 exec 스트림에 없고 app-server의 account/rateLimits/read로 읽는다 (1단계 검증 예정).
pub struct CodexAdapter;

impl CliAdapter for CodexAdapter {
    fn id(&self) -> CliId {
        CliId::Codex
    }

    fn probe_command(&self) -> CommandSpec {
        // "Logged in using ChatGPT" 여부로 auth_required 판별
        CommandSpec {
            program: "codex".into(),
            args: vec!["login".into(), "status".into()],
            env: vec![],
            cwd: String::new(),
        }
    }

    fn build_command(&self, job: &Job) -> CommandSpec {
        let sandbox = if job.allow_writes { "workspace-write" } else { "read-only" };
        CommandSpec {
            program: "codex".into(),
            args: vec![
                "exec".into(),
                "--json".into(),
                "--skip-git-repo-check".into(),
                "--sandbox".into(),
                sandbox.into(),
                "-C".into(),
                job.project_dir.clone(),
                job.request.clone(),
            ],
            env: vec![],
            cwd: job.project_dir.clone(),
        }
    }

    fn parse_event(&self, line: &str) -> Vec<AgentEvent> {
        // 주의: codex는 stderr에 진단 로그를 섞어 낸다. stdout 라인만 이 함수로 들어와야 한다.
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "thread.started" => vec![AgentEvent::SessionStarted {
                session_id: v
                    .get("thread_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }],
            "item.completed" => {
                let item = match v.get("item") {
                    Some(i) => i,
                    None => return vec![],
                };
                match item.get("type").and_then(Value::as_str).unwrap_or("") {
                    "agent_message" => vec![AgentEvent::Message {
                        text: item
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        delta: false,
                    }],
                    "file_change" => {
                        let ok = item.get("status").and_then(Value::as_str) != Some("failed");
                        item.get("changes")
                            .and_then(Value::as_array)
                            .map(|changes| {
                                changes
                                    .iter()
                                    .map(|c| AgentEvent::FileChange {
                                        path: c
                                            .get("path")
                                            .and_then(Value::as_str)
                                            .unwrap_or_default()
                                            .to_string(),
                                        ok,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    }
                    "command_execution" => vec![AgentEvent::ToolUse {
                        tool: "command".into(),
                        detail: item
                            .get("command")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    }],
                    _ => vec![],
                }
            }
            "turn.completed" => vec![AgentEvent::Completed {
                ok: true,
                summary: String::new(),
            }],
            "turn.failed" => vec![AgentEvent::Completed {
                ok: false,
                summary: v.to_string(),
            }],
            _ => vec![],
        }
    }

    /// `codex exec [OPTIONS] resume <thread_id> <prompt>` — 옵션은 서브커맨드 앞에 둔다 (exec --help 실측).
    fn build_resume_command(&self, job: &Job, session_id: &str) -> Option<CommandSpec> {
        let sandbox = if job.allow_writes { "workspace-write" } else { "read-only" };
        Some(CommandSpec {
            program: "codex".into(),
            args: vec![
                "exec".into(),
                "--json".into(),
                "--skip-git-repo-check".into(),
                "--sandbox".into(),
                sandbox.into(),
                "-C".into(),
                job.project_dir.clone(),
                "resume".into(),
                session_id.to_string(),
                job.request.clone(),
            ],
            env: vec![],
            cwd: job.project_dir.clone(),
        })
    }
}
