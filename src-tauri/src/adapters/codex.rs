use serde_json::Value;

use super::{probe_detail, AgentEvent, CliAdapter};
use crate::availability::{Evidence, ProbeOutcome};
use crate::models::{CliId, CommandSpec, Job};

/// Codex 어댑터.
/// 실측 근거: `codex exec --json` JSONL (스파이크 0, 2026-09-02).
/// 사용량 %는 exec 스트림에 없고 app-server의 account/rateLimits/read로 읽는다 (1단계 검증 예정).
/// 프롬프트는 아직 인자로 넘기므로 줄바꿈이 든 메시지는 실행 단계에서 거부된다 (TODO: stdin 전달 실측).
pub struct CodexAdapter;

impl CodexAdapter {
    fn common_args(job: &Job) -> Vec<String> {
        let sandbox = if job.allow_writes {
            "workspace-write"
        } else {
            "read-only"
        };
        vec![
            "exec".into(),
            "--json".into(),
            "--skip-git-repo-check".into(),
            "--sandbox".into(),
            sandbox.into(),
            "-C".into(),
            job.project_dir.clone(),
        ]
    }
}

impl CliAdapter for CodexAdapter {
    fn id(&self) -> CliId {
        CliId::Codex
    }

    /// `codex login status` → "Logged in using ChatGPT" / "Not logged in" (0.152 실측)
    fn probe_command(&self) -> CommandSpec {
        CommandSpec {
            program: "codex".into(),
            args: vec!["login".into(), "status".into()],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        }
    }

    fn build_command(&self, job: &Job) -> CommandSpec {
        let mut args = Self::common_args(job);
        args.push(job.request.clone());
        CommandSpec {
            program: "codex".into(),
            args,
            env: vec![],
            cwd: job.project_dir.clone(),
            stdin: None,
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
        let mut args = Self::common_args(job);
        args.push("resume".into());
        args.push(session_id.to_string());
        args.push(job.request.clone());
        Some(CommandSpec {
            program: "codex".into(),
            args,
            env: vec![],
            cwd: job.project_dir.clone(),
            stdin: None,
        })
    }

    fn interpret_probe(&self, code: Option<i32>, stdout: &str, stderr: &str) -> ProbeOutcome {
        let text = format!("{stdout}\n{stderr}").to_ascii_lowercase();
        if text.contains("not logged in") {
            ProbeOutcome::AuthRequired {
                detail: probe_detail(code, stdout, stderr),
            }
        } else if text.contains("logged in") {
            ProbeOutcome::Ready {
                evidence: Evidence::CliReported,
                version: None,
            }
        } else if code == Some(0) {
            ProbeOutcome::Ready {
                evidence: Evidence::Estimated,
                version: None,
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

    #[test]
    fn login_status_is_interpreted() {
        assert_eq!(
            CodexAdapter.interpret_probe(Some(0), "Logged in using ChatGPT\n", ""),
            ProbeOutcome::Ready {
                evidence: Evidence::CliReported,
                version: None
            }
        );
        assert!(matches!(
            CodexAdapter.interpret_probe(Some(1), "Not logged in\n", ""),
            ProbeOutcome::AuthRequired { .. }
        ));
        assert!(matches!(
            CodexAdapter.interpret_probe(None, "", "not recognized"),
            ProbeOutcome::Unavailable { .. }
        ));
    }
}
