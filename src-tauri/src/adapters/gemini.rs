use serde_json::{json, Value};

use super::{line_has_id, AgentEvent, CliAdapter, LineExchange, LoginFlow, ModelListing};
use crate::availability::{AccountInfo, Evidence, ProbeOutcome};
use crate::models::{CliId, CommandSpec, Job, ModelOption};

/// Gemini CLI 어댑터.
/// 실측 근거: `gemini -p -o stream-json --approval-mode ...` (스파이크 0, 2026-09-02).
/// 사용량 신호가 없어 항상 추정(Estimated) 경로로 다룬다 — CLI 내부의 retrieveUserQuota는 대화형 화면 전용 (2026-09-04 재확인).
/// probe·모델 목록·로그인은 ACP 모드(`gemini --acp`, JSON-RPC 2.0)로:
/// `initialize`(agentInfo.version) → `session/new`(성공 = 인증 OK, 미인증이면 "Authentication required" 오류 — 번들의
/// RequestError.authRequired 실측), `result.models.availableModels`, `authenticate{methodId:"oauth-personal"}`.
/// 계정 표시는 `~/.gemini/settings.json`의 `security.auth.selectedType`(gemini-api-key / oauth-personal / vertex-ai)을 따른다.
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

fn acp_session_new() -> String {
    let cwd = std::env::temp_dir().to_string_lossy().into_owned();
    json!({"jsonrpc": "2.0", "id": ACP_SECOND_ID, "method": "session/new", "params": {"cwd": cwd, "mcpServers": []}})
        .to_string()
}

/// initialize + session/new — probe와 모델 목록이 같은 교환을 쓴다
fn acp_session_exchange() -> LineExchange {
    LineExchange {
        spec: acp_spec(),
        inputs: vec![acp_initialize(), acp_session_new()],
        done_id: ACP_SECOND_ID,
    }
}

fn gemini_home() -> Option<std::path::PathBuf> {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).ok()?;
    Some(std::path::Path::new(&home).join(".gemini"))
}

/// settings.json의 `security.auth.selectedType`와 google_accounts.json으로 계정 요약을 만든다.
/// - gemini-api-key / vertex-ai / gateway: 키 기반 → 계정 이메일 없음
/// - oauth-personal: google_accounts.json의 active, 없으면 old의 마지막 항목을 "이전 로그인 기록"으로
pub fn account_from_files(settings_json: Option<&str>, accounts_json: Option<&str>) -> Option<AccountInfo> {
    let selected = settings_json
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| {
            v.pointer("/security/auth/selectedType")
                .or_else(|| v.get("selectedAuthType"))
                .and_then(Value::as_str)
                .map(String::from)
        });
    match selected.as_deref() {
        Some("gemini-api-key") => Some(AccountInfo {
            label: "Gemini API 키".into(),
            plan: None,
            method: Some("gemini-api-key".into()),
        }),
        Some("vertex-ai") => Some(AccountInfo {
            label: "Vertex AI 키".into(),
            plan: None,
            method: Some("vertex-ai".into()),
        }),
        Some("gateway") => Some(AccountInfo {
            label: "AI API Gateway".into(),
            plan: None,
            method: Some("gateway".into()),
        }),
        _ => {
            let v = serde_json::from_str::<Value>(accounts_json?).ok()?;
            let method = Some(selected.unwrap_or_else(|| "oauth-personal".into()));
            if let Some(active) = v.get("active").and_then(Value::as_str) {
                return Some(AccountInfo {
                    label: active.to_string(),
                    plan: None,
                    method,
                });
            }
            let old = v
                .get("old")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(Value::as_str)
                .last()?;
            Some(AccountInfo {
                label: format!("{old} (이전 로그인 기록)"),
                plan: None,
                method,
            })
        }
    }
}

fn local_account() -> Option<AccountInfo> {
    let home = gemini_home()?;
    let settings = std::fs::read_to_string(home.join("settings.json")).ok();
    let accounts = std::fs::read_to_string(home.join("google_accounts.json")).ok();
    account_from_files(settings.as_deref(), accounts.as_deref())
}

impl CliAdapter for GeminiAdapter {
    fn id(&self) -> CliId {
        CliId::Gemini
    }

    /// probe_exchange가 있으므로 실제로는 쓰이지 않는다 (계약상 필요)
    fn probe_command(&self) -> CommandSpec {
        CommandSpec {
            program: "gemini".into(),
            args: vec!["--version".into()],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        }
    }

    /// 인증 상태는 ACP `session/new`가 성공하는지로 판별한다 (미인증 → "Authentication required" 오류)
    fn probe_exchange(&self) -> Option<LineExchange> {
        Some(acp_session_exchange())
    }

    fn interpret_probe_lines(&self, lines: &[String]) -> ProbeOutcome {
        let version = lines
            .iter()
            .find(|l| line_has_id(l, ACP_INIT_ID))
            .and_then(|l| serde_json::from_str::<Value>(l).ok())
            .and_then(|v| {
                v.pointer("/result/agentInfo/version")
                    .and_then(Value::as_str)
                    .map(String::from)
            });
        let Some(reply) = lines
            .iter()
            .find(|l| line_has_id(l, ACP_SECOND_ID))
            .and_then(|l| serde_json::from_str::<Value>(l).ok())
        else {
            return ProbeOutcome::Unavailable {
                detail: if lines.is_empty() {
                    "gemini --acp 응답 없음 (미설치 또는 시간 초과)".into()
                } else {
                    "gemini --acp: session/new 응답 없음".into()
                },
            };
        };
        if let Some(err) = reply.get("error") {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("알 수 없는 오류")
                .to_string();
            let lower = message.to_ascii_lowercase();
            if lower.contains("auth") || lower.contains("login") || lower.contains("credential") {
                return ProbeOutcome::AuthRequired {
                    detail: format!("gemini --acp: {message}"),
                };
            }
            return ProbeOutcome::Unavailable {
                detail: format!("gemini --acp: {message}"),
            };
        }
        ProbeOutcome::Ready {
            evidence: Evidence::CliReported,
            version,
            account: local_account(),
        }
    }

    /// 프롬프트는 stdin으로 넘긴다 (`-p ""`: "-p는 stdin 입력 뒤에 덧붙는다" 실측, 2026-09-04) —
    /// gemini.cmd 셔임은 줄바꿈 인자를 못 받으므로 여러 줄·handoff 프롬프트를 위해 필요하다.
    fn build_command(&self, job: &Job) -> CommandSpec {
        let approval = if job.allow_writes { "auto_edit" } else { "plan" };
        let mut args = vec![
            "-p".to_string(),
            String::new(),
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
            stdin: Some(job.request.clone()),
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
            hint: "브라우저가 열리면 Google 계정으로 로그인하세요. API 키를 쓰려면 터미널에서 `gemini`를 실행해 `/auth`에서 방식을 고릅니다.".into(),
        })
    }

    /// ACP `session/new` 응답의 `models.availableModels`(modelId·name)와 `currentModelId` (0.54.4 실측)
    fn model_listing(&self) -> ModelListing {
        ModelListing::Exchange(acp_session_exchange())
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
    use crate::models::JobStatus;

    const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"gemini-cli","version":"0.54.4"}}}"#;

    #[test]
    fn acp_probe_distinguishes_auth() {
        let ok = vec![
            INIT.to_string(),
            r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"s","models":{"availableModels":[],"currentModelId":"auto"}}}"#.to_string(),
        ];
        match GeminiAdapter.interpret_probe_lines(&ok) {
            ProbeOutcome::Ready {
                evidence: Evidence::CliReported,
                version,
                ..
            } => assert_eq!(version.as_deref(), Some("0.54.4")),
            other => panic!("예상 밖: {other:?}"),
        }
        let need = vec![
            INIT.to_string(),
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"Authentication required."}}"#.to_string(),
        ];
        assert!(matches!(
            GeminiAdapter.interpret_probe_lines(&need),
            ProbeOutcome::AuthRequired { .. }
        ));
        assert!(matches!(
            GeminiAdapter.interpret_probe_lines(&[]),
            ProbeOutcome::Unavailable { .. }
        ));
        assert!(GeminiAdapter.probe_exchange().is_some());
    }

    #[test]
    fn account_follows_selected_auth_type() {
        let api = account_from_files(
            Some(r#"{"security":{"auth":{"selectedType":"gemini-api-key"}}}"#),
            Some(r#"{"active":null,"old":["x@gmail.com"]}"#),
        )
        .unwrap();
        assert_eq!(api.label, "Gemini API 키");
        assert_eq!(api.method.as_deref(), Some("gemini-api-key"));

        let oauth = account_from_files(
            Some(r#"{"security":{"auth":{"selectedType":"oauth-personal"}}}"#),
            Some(r#"{"active":"a@gmail.com","old":[]}"#),
        )
        .unwrap();
        assert_eq!(oauth.label, "a@gmail.com");
        assert_eq!(oauth.method.as_deref(), Some("oauth-personal"));

        let old = account_from_files(None, Some(r#"{"active":null,"old":["x@gmail.com","y@gmail.com"]}"#)).unwrap();
        assert_eq!(old.label, "y@gmail.com (이전 로그인 기록)");
        assert!(account_from_files(None, Some(r#"{"active":null,"old":[]}"#)).is_none());
        assert!(account_from_files(None, None).is_none());
    }

    /// 2026-09-04 ACP session/new 실측 응답(요약)
    #[test]
    fn acp_models_are_parsed_and_model_flag_applied() {
        let lines = vec![
            INIT.to_string(),
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
        assert_eq!(spec.stdin.as_deref(), Some("hi"), "프롬프트는 stdin");
        assert!(spec.args.windows(2).any(|w| w == ["-p", ""]));
        assert!(!spec.args.iter().any(|a| a == "hi"));
    }
}
