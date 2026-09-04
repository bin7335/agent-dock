use serde_json::Value;

use super::{AgentEvent, CliAdapter};
use crate::models::{CliId, CommandSpec, Job};

/// Gemini CLI 어댑터.
/// 실측 근거: `gemini -p -o stream-json --approval-mode ...` (스파이크 0, 2026-09-02).
/// 사용량 신호가 없어 항상 추정(Estimated) 경로로 다룬다. probe는 `--version`(기본 해석: 첫 줄 = 버전).
/// 프롬프트는 아직 인자로 넘기므로 줄바꿈이 든 메시지는 실행 단계에서 거부된다 (TODO: stdin 전달 실측).
pub struct GeminiAdapter;

impl CliAdapter for GeminiAdapter {
    fn id(&self) -> CliId {
        CliId::Gemini
    }

    fn probe_command(&self) -> CommandSpec {
        CommandSpec {
            program: "gemini".into(),
            args: vec!["--version".into()],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        }
    }

    fn build_command(&self, job: &Job) -> CommandSpec {
        let approval = if job.allow_writes { "auto_edit" } else { "plan" };
        CommandSpec {
            program: "gemini".into(),
            args: vec![
                "-p".into(),
                job.request.clone(),
                "-o".into(),
                "stream-json".into(),
                "--approval-mode".into(),
                approval.into(),
            ],
            // 비신뢰 폴더 헤드리스 거부(exit 55) 우회 — 스파이크 0 실측
            env: vec![("GEMINI_CLI_TRUST_WORKSPACE".into(), "true".into())],
            cwd: job.project_dir.clone(),
            stdin: None,
        }
    }

    fn parse_event(&self, line: &str) -> Vec<AgentEvent> {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "init" => vec![AgentEvent::SessionStarted {
                session_id: v
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }],
            "message" => {
                if v.get("role").and_then(Value::as_str) == Some("assistant") {
                    vec![AgentEvent::Message {
                        text: v
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        // Gemini는 assistant 메시지를 delta 조각으로 스트리밍한다 (스파이크 실측)
                        delta: v.get("delta").and_then(Value::as_bool).unwrap_or(false),
                    }]
                } else {
                    vec![]
                }
            }
            "tool_use" => vec![AgentEvent::ToolUse {
                tool: v
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                detail: v.get("parameters").map(|p| p.to_string()).unwrap_or_default(),
            }],
            "result" => vec![AgentEvent::Completed {
                ok: v.get("status").and_then(Value::as_str) == Some("success"),
                summary: String::new(),
            }],
            _ => vec![],
        }
    }

    /// Gemini CLI의 --resume은 UUID가 아니라 "latest"/인덱스만 받는다 (--help 실측).
    /// 같은 프로젝트의 직전 세션이 우리 실행이라는 전제로 latest를 쓴다 — MVP 한계.
    fn build_resume_command(&self, job: &Job, _session_id: &str) -> Option<CommandSpec> {
        let mut spec = self.build_command(job);
        spec.args.push("--resume".into());
        spec.args.push("latest".into());
        Some(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::{Evidence, ProbeOutcome};

    #[test]
    fn version_probe_uses_default_interpretation() {
        assert_eq!(
            GeminiAdapter.interpret_probe(Some(0), "0.54.4\n", ""),
            ProbeOutcome::Ready {
                evidence: Evidence::Estimated,
                version: Some("0.54.4".into())
            }
        );
        assert!(matches!(
            GeminiAdapter.interpret_probe(Some(1), "", "boom"),
            ProbeOutcome::Unavailable { .. }
        ));
    }
}
