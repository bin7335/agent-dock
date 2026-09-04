use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Value};

use super::{
    line_has_id, AgentEvent, CliAdapter, FollowUp, LineExchange, LineReaction, LoginFlow,
    ModelListing, PermissionContext, PermissionDecision,
};
use crate::availability::{AccountInfo, Evidence, ProbeOutcome};
use crate::models::{CliId, CommandSpec, Job, ModelOption};

/// Gemini CLI 어댑터 — 대화도 ACP 모드(`gemini --acp`, JSON-RPC 2.0)로 돌린다.
/// 실측(2026-09-04, 0.54.4): `initialize` → `session/new{cwd, mcpServers}`(응답 sessionId·modes·models) →
/// `session/prompt{sessionId, prompt:[{type:"text", text}]}` → 알림 `session/update{update.sessionUpdate:
/// agent_message_chunk|agent_thought_chunk|tool_call|tool_call_update|user_message_chunk|available_commands_update}`
/// → 승인이 필요하면 서버 요청 `session/request_permission{id, params{options[{optionId, name, kind:
/// allow_always|allow_once|reject_once}], toolCall{toolCallId, title, kind, content[diff], locations[path]}}}`
/// (응답 `{id, result:{outcome:{outcome:"selected", optionId}}}`) → prompt 응답 `{stopReason}`.
/// 재개는 `session/load{sessionId}`(과거 대화를 session/update로 다시 흘려보내므로 응답 전 알림은 버린다).
/// stdin을 닫아도 프로세스가 안 끝나므로 러너가 유예 뒤 정리한다.
/// 사용량 신호는 없어 항상 추정(Estimated) — retrieveUserQuota는 대화형 화면 전용 (2026-09-04 재확인).
/// probe·모델 목록도 같은 ACP 교환(`session/new` 성공 = 인증 OK, 미인증이면 "Authentication required")을 쓴다.
/// 계정 표시는 `~/.gemini/settings.json`의 `security.auth.selectedType`을 따른다.
/// 주의: 이 계정(무료 API 키)은 429 백오프로 한 턴에 2분 넘게 걸리기도 한다 — 모델을 flash로 고정해도 그렇다.
pub struct GeminiAdapter;

const ACP_INIT_ID: u64 = 1;
const ACP_SECOND_ID: u64 = 2;
/// 대화 실행에서 session/prompt 요청 id. 응답이 오면 턴이 끝난 것
const ACP_PROMPT_ID: u64 = 3;

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

impl GeminiAdapter {
    /// ACP 대화 실행: `gemini --acp --approval-mode … [-m]` + stdin(initialize, open). session/prompt는 follow-up이 보낸다.
    /// 쓰기 허용 = 편집 자동 승인·명령은 앱에 묻기(auto_edit), 읽기 전용 = plan (ACP modes 실측: default/autoEdit/yolo/plan)
    fn run_spec(job: &Job, open: Value) -> CommandSpec {
        let mut args = vec![
            "--acp".to_string(),
            "--approval-mode".into(),
            if job.allow_writes { "auto_edit" } else { "plan" }.into(),
        ];
        if let Some(m) = job.model.as_deref().filter(|m| !m.is_empty()) {
            args.push("-m".into());
            args.push(m.to_string());
        }
        let mut stdin = acp_initialize();
        stdin.push('\n');
        stdin.push_str(&open.to_string());
        stdin.push('\n');
        CommandSpec {
            program: "gemini".into(),
            args,
            // 비신뢰 폴더 헤드리스 거부(exit 55) 우회 — 스파이크 0 실측
            env: vec![("GEMINI_CLI_TRUST_WORKSPACE".into(), "true".into())],
            cwd: job.project_dir.clone(),
            stdin: Some(stdin),
        }
    }

    fn prompt_line(session_id: &str, request: &str) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": ACP_PROMPT_ID,
            "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": [{"type": "text", "text": request}]}
        })
        .to_string()
    }

    /// session/new·session/load 응답(id=ACP_SECOND_ID)을 기다렸다가 session/prompt를 보낸다.
    /// resume이면 load 응답 전에 재생되는 session/update는 화면에 다시 그리지 않는다.
    fn follow_up_for(request: String, resume: Option<String>) -> FollowUp {
        let opened = AtomicBool::new(false);
        Box::new(move |line: &str| {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                return LineReaction::default();
            };
            let is_open_reply = v.get("method").is_none()
                && v.get("id").and_then(Value::as_u64) == Some(ACP_SECOND_ID);
            if is_open_reply {
                opened.store(true, Ordering::Relaxed);
                if v.get("error").is_some() {
                    return LineReaction::default();
                }
                let sid = v
                    .pointer("/result/sessionId")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .or_else(|| resume.clone());
                return LineReaction {
                    send: sid.map(|s| Self::prompt_line(&s, &request)),
                    drop_events: false,
                };
            }
            let replaying = resume.is_some()
                && !opened.load(Ordering::Relaxed)
                && v.get("method").and_then(Value::as_str) == Some("session/update");
            LineReaction {
                send: None,
                drop_events: replaying,
            }
        })
    }
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

    fn build_command(&self, job: &Job) -> CommandSpec {
        Self::run_spec(
            job,
            json!({
                "jsonrpc": "2.0",
                "id": ACP_SECOND_ID,
                "method": "session/new",
                "params": {"cwd": job.project_dir, "mcpServers": []}
            }),
        )
    }

    /// ACP `session/load{sessionId}`로 같은 세션을 이어간다 (0.54.4 실측: loadSession 지원, 과거 대화 재생 뒤 응답)
    fn build_resume_command(&self, job: &Job, session_id: &str) -> Option<CommandSpec> {
        Some(Self::run_spec(
            job,
            json!({
                "jsonrpc": "2.0",
                "id": ACP_SECOND_ID,
                "method": "session/load",
                "params": {"sessionId": session_id, "cwd": job.project_dir, "mcpServers": []}
            }),
        ))
    }

    fn keeps_stdin_open(&self) -> bool {
        true
    }

    /// gemini는 오류를 여러 줄짜리 객체 덤프(`Error handling request { id: 3, ... message: '...' }`)로 stderr에 쏟는다.
    /// 제목 줄·code·message만 남기고 나머지 조각(`id: 3,`, `}`)은 버린다 (2026-09-04 실측: 일일 쿼터 소진 500 오류)
    fn filter_stderr(&self, line: &str) -> Option<String> {
        let t = line.trim();
        if t.is_empty() {
            return None;
        }
        let keep = t.starts_with("Error")
            || t.starts_with("message:")
            || t.starts_with("code:")
            || t.contains("Error:")
            || t.contains("error:");
        keep.then(|| t.to_string())
    }

    fn stdin_follow_up(&self, job: &Job, session_id: Option<&str>) -> Option<FollowUp> {
        Some(Self::follow_up_for(
            job.request.clone(),
            session_id.map(String::from),
        ))
    }

    /// 서버 요청 id에 options 중 하나를 골라 답한다: 허용은 allow_once(기억이면 allow_always), 거부는 reject_once
    fn permission_reply(
        &self,
        ctx: &PermissionContext,
        decision: &PermissionDecision,
    ) -> Option<String> {
        let id: u64 = ctx.request_id.parse().ok()?;
        let options: Vec<Value> = serde_json::from_str::<Value>(ctx.suggestions)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let pick = |kinds: &[&str]| {
            kinds.iter().find_map(|k| {
                options
                    .iter()
                    .find(|o| o.get("kind").and_then(Value::as_str) == Some(k))
                    .and_then(|o| o.get("optionId").and_then(Value::as_str))
                    .map(String::from)
            })
        };
        let option = if decision.allow {
            if decision.remember {
                pick(&["allow_always", "allow_once"])
            } else {
                pick(&["allow_once", "allow_always"])
            }
        } else {
            pick(&["reject_once", "reject_always"])
        };
        let outcome = match option {
            Some(option_id) => json!({"outcome": "selected", "optionId": option_id}),
            None => json!({"outcome": "cancelled"}),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": outcome}}).to_string())
    }

    fn parse_event(&self, line: &str) -> Vec<AgentEvent> {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let text = |o: &Value, k: &str| {
            o.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        // 요청 응답: session/new → 세션 id, session/prompt → 턴 종료, 오류 → 실패
        if v.get("method").is_none() {
            let Some(id) = v.get("id").and_then(Value::as_u64) else {
                return vec![];
            };
            if let Some(err) = v.get("error") {
                return vec![AgentEvent::Completed {
                    ok: false,
                    summary: format!("{} (요청 {id})", text(err, "message")),
                }];
            }
            if id == ACP_SECOND_ID {
                return v
                    .pointer("/result/sessionId")
                    .and_then(Value::as_str)
                    .map(|sid| {
                        vec![AgentEvent::SessionStarted {
                            session_id: sid.to_string(),
                        }]
                    })
                    .unwrap_or_default();
            }
            if id == ACP_PROMPT_ID {
                let stop = v
                    .pointer("/result/stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("end_turn");
                let ok = matches!(stop, "end_turn" | "max_turn_requests");
                return vec![AgentEvent::Completed {
                    ok,
                    summary: if ok {
                        String::new()
                    } else {
                        format!("stopReason {stop}")
                    },
                }];
            }
            return vec![];
        }
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        match v.get("method").and_then(Value::as_str).unwrap_or("") {
            "session/update" => {
                let u = params.get("update").cloned().unwrap_or(Value::Null);
                match text(&u, "sessionUpdate").as_str() {
                    "agent_message_chunk" => {
                        let t = u
                            .pointer("/content/text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        // 승인 뒤 오는 "[MODE_UPDATE] autoEdit" 같은 내부 알림은 본문이 아니다 (실측)
                        if t.is_empty() || t.starts_with("[MODE_UPDATE]") {
                            vec![]
                        } else {
                            vec![AgentEvent::Message {
                                text: t.to_string(),
                                delta: true,
                            }]
                        }
                    }
                    "tool_call" => vec![AgentEvent::ToolUse {
                        tool: text(&u, "kind"),
                        detail: text(&u, "title"),
                    }],
                    "tool_call_update" => {
                        let status = text(&u, "status");
                        if status != "completed" && status != "failed" {
                            return vec![];
                        }
                        u.get("locations")
                            .and_then(Value::as_array)
                            .map(|ls| {
                                ls.iter()
                                    .filter_map(|l| l.get("path").and_then(Value::as_str))
                                    .map(|p| AgentEvent::FileChange {
                                        path: p.to_string(),
                                        ok: status == "completed",
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    }
                    _ => vec![],
                }
            }
            "session/request_permission" => {
                let request_id = match v.get("id") {
                    Some(Value::Number(n)) => n.to_string(),
                    Some(Value::String(s)) => s.clone(),
                    _ => return vec![],
                };
                let tc = params.get("toolCall").cloned().unwrap_or(Value::Null);
                let options = params
                    .get("options")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                let can_remember = options
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .any(|o| o.get("kind").and_then(Value::as_str) == Some("allow_always"))
                    })
                    .unwrap_or(false);
                let kind = text(&tc, "kind");
                let title = text(&tc, "title");
                let path = tc.pointer("/locations/0/path").and_then(Value::as_str);
                let new_text = tc
                    .pointer("/content/0/newText")
                    .and_then(Value::as_str)
                    .map(|t| t.chars().take(2000).collect::<String>());
                let mut input = json!({"title": title, "kind": kind, "file_path": path, "new_text": new_text});
                if kind == "execute" {
                    input["command"] = Value::String(title.clone());
                }
                vec![AgentEvent::PermissionRequest {
                    request_id,
                    tool: if kind.is_empty() { "tool".into() } else { kind },
                    description: title,
                    input: input.to_string(),
                    can_remember,
                    suggestions: options.to_string(),
                }]
            }
            _ => vec![],
        }
    }

    /// Google 개인 계정 OAuth(oauth-personal)는 2026-06-18부로 Gemini CLI에서 종료(IneligibleTierError, Antigravity로 이전).
    /// 따라서 로그인 버튼은 대화형 `gemini`를 콘솔에서 띄워 `/auth`에서 API 키(gemini-api-key) 방식을 설정하게 한다.
    fn login_flow(&self) -> Option<LoginFlow> {
        Some(LoginFlow::Console {
            spec: CommandSpec {
                program: "gemini".into(),
                args: vec![],
                env: vec![],
                cwd: String::new(),
                stdin: None,
            },
            hint: "Gemini CLI는 개인 Google 로그인이 종료됐습니다(AI Pro는 Antigravity 항목을 쓰세요). 콘솔에서 /auth → Gemini API key를 고르고 키를 입력한 뒤 /quit로 나오세요.".into(),
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
        assert!(matches!(GeminiAdapter.login_flow(), Some(LoginFlow::Console { .. })));

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
        assert!(spec.args.windows(2).any(|w| w == ["--approval-mode", "plan"]));
        assert_eq!(spec.args[0], "--acp");
        assert!(GeminiAdapter.keeps_stdin_open());
        let stdin = spec.stdin.unwrap();
        let lines: Vec<&str> = stdin.lines().collect();
        assert_eq!(lines.len(), 2, "initialize + session/new; 프롬프트는 follow-up");
        let open: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(open["method"], "session/new");
        assert_eq!(open["params"]["cwd"], "D:\\x");
        assert!(!stdin.contains("\"hi\""));
    }

    fn job_with(request: &str, allow_writes: bool) -> Job {
        Job {
            id: 0,
            title: "t".into(),
            request: request.into(),
            project_dir: "D:\\dev".into(),
            profile: "코딩 작업".into(),
            allow_writes,
            unattended_ok: false,
            status: JobStatus::Starting,
            model: None,
        }
    }

    const SESSION_NEW: &str = r#"{"jsonrpc":"2.0","id":2,"result":{"sessionId":"66deb839-cc3f-47e4-b93a-ac8e22fd75c6","modes":{"availableModes":[],"currentModeId":"autoEdit"},"models":{"availableModels":[],"currentModelId":"auto"}}}"#;

    /// 2026-09-04 실측: session/new 응답 → session/prompt, session/load는 응답 전 재생 알림을 버린다
    #[test]
    fn acp_follow_up_sends_prompt_after_open() {
        let job = job_with("line one\nline two", true);
        let spec = GeminiAdapter.build_command(&job);
        assert!(spec.args.windows(2).any(|w| w == ["--approval-mode", "auto_edit"]));
        let follow = GeminiAdapter.stdin_follow_up(&job, None).unwrap();
        let r = follow(SESSION_NEW);
        let prompt: Value = serde_json::from_str(&r.send.unwrap()).unwrap();
        assert_eq!(prompt["id"], ACP_PROMPT_ID);
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["sessionId"], "66deb839-cc3f-47e4-b93a-ac8e22fd75c6");
        assert_eq!(prompt["params"]["prompt"][0]["text"], "line one\nline two");
        assert!(!r.drop_events);
        assert!(matches!(
            &GeminiAdapter.parse_event(SESSION_NEW)[..],
            [AgentEvent::SessionStarted { session_id }] if session_id == "66deb839-cc3f-47e4-b93a-ac8e22fd75c6"
        ));

        // 재개: load 응답 전 session/update는 버리고, 응답에는 sessionId가 없어 넘겨받은 id로 prompt를 보낸다
        let resume = GeminiAdapter.build_resume_command(&job, "66deb839").unwrap();
        let open: Value = serde_json::from_str(resume.stdin.unwrap().lines().nth(1).unwrap()).unwrap();
        assert_eq!(open["method"], "session/load");
        assert_eq!(open["params"]["sessionId"], "66deb839");
        let follow = GeminiAdapter.stdin_follow_up(&job, Some("66deb839")).unwrap();
        let replay = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"66deb839","update":{"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"old"}}}}"#;
        assert!(follow(replay).drop_events);
        let loaded = r#"{"jsonrpc":"2.0","id":2,"result":{"modes":{"availableModes":[],"currentModeId":"autoEdit"},"models":{"availableModels":[],"currentModelId":"auto"}}}"#;
        let r = follow(loaded);
        assert!(!r.drop_events);
        let prompt: Value = serde_json::from_str(&r.send.unwrap()).unwrap();
        assert_eq!(prompt["params"]["sessionId"], "66deb839");
        assert!(!follow(replay).drop_events, "load 응답 뒤의 알림은 정상 이벤트");
        assert!(GeminiAdapter.parse_event(loaded).is_empty());
    }

    /// 2026-09-04 실측 알림·서버 요청(요약)
    #[test]
    fn acp_updates_and_permission_become_events() {
        let chunk = GeminiAdapter.parse_event(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"done"}}}}"#,
        );
        assert!(matches!(&chunk[..], [AgentEvent::Message { text, delta: true }] if text == "done"));
        assert!(GeminiAdapter
            .parse_event(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"[MODE_UPDATE] autoEdit"}}}}"#)
            .is_empty());
        assert!(GeminiAdapter
            .parse_event(r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}}}"#)
            .is_empty());
        let call = GeminiAdapter.parse_event(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call","toolCallId":"write_file__call_1","status":"pending","title":"Writing to gemini_probe.txt","kind":"edit","locations":[{"path":"D:\\dev\\gemini_probe.txt"}]}}}"#,
        );
        assert!(matches!(&call[..], [AgentEvent::ToolUse { tool, detail }] if tool == "edit" && detail.starts_with("Writing")));
        let update = GeminiAdapter.parse_event(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"tool_call_update","toolCallId":"write_file__call_1","status":"completed","title":"Writing to gemini_probe.txt","locations":[{"path":"D:\\dev\\gemini_probe.txt"}]}}}"#,
        );
        assert!(matches!(&update[..], [AgentEvent::FileChange { path, ok: true }] if path.ends_with("gemini_probe.txt")));

        let perm = GeminiAdapter.parse_event(
            r#"{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{"sessionId":"s","options":[{"optionId":"proceed_always","name":"Allow for this session","kind":"allow_always"},{"optionId":"proceed_once","name":"Allow","kind":"allow_once"},{"optionId":"cancel","name":"Reject","kind":"reject_once"}],"toolCall":{"toolCallId":"write_file__call_1","status":"pending","title":"Writing to gemini_probe.txt","content":[{"type":"diff","path":"D:\\dev\\gemini_probe.txt","oldText":"","newText":"hi"}],"locations":[{"path":"D:\\dev\\gemini_probe.txt"}],"kind":"edit"}}}"#,
        );
        let (request_id, suggestions) = match &perm[..] {
            [AgentEvent::PermissionRequest { request_id, tool, description, input, can_remember, suggestions }] => {
                assert_eq!(request_id, "0");
                assert_eq!(tool, "edit");
                assert_eq!(description, "Writing to gemini_probe.txt");
                let i: Value = serde_json::from_str(input).unwrap();
                assert!(i["file_path"].as_str().unwrap().ends_with("gemini_probe.txt"));
                assert_eq!(i["new_text"], "hi");
                assert!(*can_remember);
                (request_id.clone(), suggestions.clone())
            }
            other => panic!("unexpected {other:?}"),
        };
        let ctx = PermissionContext { request_id: &request_id, input: "{}", suggestions: &suggestions };
        let reply = |allow, remember| {
            let line = GeminiAdapter
                .permission_reply(&ctx, &PermissionDecision { allow, remember, message: None })
                .unwrap();
            serde_json::from_str::<Value>(&line).unwrap()
        };
        assert_eq!(reply(true, false)["result"]["outcome"]["optionId"], "proceed_once");
        assert_eq!(reply(true, true)["result"]["outcome"]["optionId"], "proceed_always");
        assert_eq!(reply(false, false)["result"]["outcome"]["optionId"], "cancel");
        assert_eq!(reply(false, false)["id"], 0);
        assert_eq!(reply(false, false)["jsonrpc"], "2.0");

        let exec = GeminiAdapter.parse_event(
            r#"{"jsonrpc":"2.0","id":1,"method":"session/request_permission","params":{"sessionId":"s","options":[{"optionId":"proceed_once","name":"Allow","kind":"allow_once"},{"optionId":"cancel","name":"Reject","kind":"reject_once"}],"toolCall":{"toolCallId":"run_shell_command__call_2","status":"pending","title":"python -c \"print(7*6)\"","kind":"execute"}}}"#,
        );
        match &exec[..] {
            [AgentEvent::PermissionRequest { tool, input, can_remember: false, .. }] => {
                assert_eq!(tool, "execute");
                let i: Value = serde_json::from_str(input).unwrap();
                assert!(i["command"].as_str().unwrap().contains("print(7*6)"));
            }
            other => panic!("unexpected {other:?}"),
        }

        let done = GeminiAdapter.parse_event(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#);
        assert!(matches!(&done[..], [AgentEvent::Completed { ok: true, .. }]));
        let refused = GeminiAdapter.parse_event(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"refusal"}}"#);
        assert!(matches!(&refused[..], [AgentEvent::Completed { ok: false, summary }] if summary.contains("refusal")));
        let err = GeminiAdapter.parse_event(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"Authentication required."}}"#);
        assert!(matches!(&err[..], [AgentEvent::Completed { ok: false, summary }] if summary.contains("Authentication")));
    }

    /// 2026-09-04 실측 stderr 덤프(일일 쿼터 소진)
    #[test]
    fn stderr_object_dump_is_folded() {
        let dump = [
            "Error handling request {",
            "  id: 3,",
            "  jsonrpc: '2.0',",
            "  method: 'session/prompt',",
            "  params: {",
            "    prompt: [ [Object] ],",
            "  }",
            "} {",
            "  code: 500,",
            "  message: 'You have exhausted your daily quota on this model.',",
            "  data: undefined",
            "}",
        ];
        let kept: Vec<String> = dump.iter().filter_map(|l| GeminiAdapter.filter_stderr(l)).collect();
        assert_eq!(
            kept,
            vec![
                "Error handling request {".to_string(),
                "code: 500,".to_string(),
                "message: 'You have exhausted your daily quota on this model.',".to_string(),
            ]
        );
        assert_eq!(GeminiAdapter.filter_stderr("TypeError: boom").as_deref(), Some("TypeError: boom"));
    }
}
