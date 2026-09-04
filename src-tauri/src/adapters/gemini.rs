use serde_json::{json, Value};

use super::{line_has_id, AgentEvent, CliAdapter, LineExchange, LoginFlow, ModelListing};
use crate::models::{CliId, CommandSpec, Job, ModelOption};

/// Gemini CLI 어댑터.
/// 실측 근거: `gemini -p -o stream-json --approval-mode ...` (스파이크 0, 2026-09-02).
/// 사용량 신호가 없어 항상 추정(Estimated) 경로로 다룬다 — CLI 내부의 retrieveUserQuota는 대화형 화면 전용 (2026-09-04 재확인).
/// probe는 `--version`(기본 해석: 첫 줄 = 버전). 모델 목록·로그인은 ACP 모드(`gemini --acp`, JSON-RPC 2.0)로:
/// `initialize` → `session/new`의 `result.models.availableModels`, `authenticate{methodId:"oauth-personal"}`.
/// 프롬프트는 아직 인자로 넘기므로 줄바꿈이 든 메시지는 실행 단계에서 거부된다 (TODO: stdin 전달 실측).
pub struct GeminiAdapter;

const ACP_INIT_ID: u64 = 1;
const ACP_SECOND_ID: u64 = 2;

fn acp_spec() -> CommandSpec {
    CommandSpec {
        program: "gemini".into(),
        args: vec!["--acp".into()],
        env: vec![("GEMINI_CLI_TRUST_WORKSPACE".into(), "true".into())],
        cwd: String::new(),
        stdin: None,
    }
}

fn acp_initialize() -> String {
    json!({
        "jsonrpc": "2.0",
        "id": ACP_INIT_ID,
        "method": "initialize",
        "params": {"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}}}
    })
    .to_string()
}

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
        let mut args = vec![
            "-p".to_string(),
            job.request.clone(),
            "-o".into(),
            "stream-json".into(),
            "--approval-mode".into(),
            approval.into(),
        ];
        if let Some(m) = job.model.as_deref().filter(|m| !m.is_empty()) {
            args.push("-m".into());
            args.push(m.to_string());
        }
        CommandSpec {
            program: "gemini".into(),
            args,
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
    /// 같은 프로젝트의 직전 세션이 우리 실행이라는 전제로 latest를 쓴다 — MVP 한계 (ACP loadSession이 대안 후보).
    fn build_resume_command(&self, job: &Job, _session_id: &str) -> Option<CommandSpec> {
        let mut spec = self.build_command(job);
        spec.args.push("--resume".into());
        spec.args.push("latest".into());
        Some(spec)
    }

    /// ACP `authenticate` — initialize 응답의 authMethods에 `oauth-personal`("Log in with Google")이 있다 (0.54.4 실측)
    fn login_flow(&self) -> Option<LoginFlow> {
        Some(LoginFlow::Exchange {
            exchange: LineExchange {
                spec: acp_spec(),
                inputs: vec![
                    acp_initialize(),
                    json!({"jsonrpc": "2.0", "id": ACP_SECOND_ID, "method": "authenticate", "params": {"methodId": "oauth-personal"}})
                        .to_string(),
                ],
                done_id: ACP_SECOND_ID,
            },
            hint: "브라우저가 열리면 Google 계정으로 로그인하세요. 안 열리면 터미널에서 `gemini`를 실행해 `/auth`를 입력합니다.".into(),
        })
    }

    /// ACP `session/new` 응답의 `models.availableModels`(modelId·name)와 `currentModelId` (0.54.4 실측)
    fn model_listing(&self) -> ModelListing {
        let cwd = std::env::temp_dir().to_string_lossy().into_owned();
        ModelListing::Exchange(LineExchange {
            spec: acp_spec(),
            inputs: vec![
                acp_initialize(),
                json!({"jsonrpc": "2.0", "id": ACP_SECOND_ID, "method": "session/new", "params": {"cwd": cwd, "mcpServers": []}})
                    .to_string(),
            ],
            done_id: ACP_SECOND_ID,
        })
    }

    fn parse_models(&self, lines: &[String]) -> Vec<ModelOption> {
        let Some(line) = lines.iter().find(|l| line_has_id(l, ACP_SECOND_ID)) else {
            return vec![];
        };
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return vec![];
        };
        let current = v
            .pointer("/result/models/currentModelId")
            .and_then(Value::as_str)
            .unwrap_or("");
        v.pointer("/result/models/availableModels")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|m| {
                        let id = m.get("modelId").and_then(Value::as_str)?;
                        let name = m.get("name").and_then(Value::as_str).unwrap_or(id);
                        Some(ModelOption {
                            id: id.to_string(),
                            label: if name == id {
                                name.to_string()
                            } else {
                                format!("{name} ({id})")
                            },
                            is_default: id == current,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::{Evidence, ProbeOutcome};
    use crate::models::JobStatus;

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

    /// 2026-09-04 ACP session/new 실측 응답(요약)
    #[test]
    fn acp_models_are_parsed_and_model_flag_applied() {
        let lines = vec![
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}"#.to_string(),
            r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s","models":{"availableModels":[{"modelId":"auto","name":"Auto","description":"Let Gemini CLI decide"},{"modelId":"gemini-2.5-pro","name":"gemini-2.5-pro"}],"currentModelId":"auto"}}}"#.to_string(),
        ];
        let models = GeminiAdapter.parse_models(&lines);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "auto");
        assert!(models[0].is_default);
        assert_eq!(models[1].label, "gemini-2.5-pro");
        assert!(matches!(GeminiAdapter.login_flow(), Some(LoginFlow::Exchange { .. })));

        let job = Job {
            id: 0,
            title: "t".into(),
            request: "hi".into(),
            project_dir: "D:\\x".into(),
            profile: "코딩 작업".into(),
            allow_writes: false,
            unattended_ok: false,
            status: JobStatus::Starting,
            model: Some("gemini-2.5-pro".into()),
        };
        let spec = GeminiAdapter.build_command(&job);
        assert!(spec.args.windows(2).any(|w| w == ["-m", "gemini-2.5-pro"]));
    }
}
