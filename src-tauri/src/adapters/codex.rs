use serde_json::{json, Value};

use super::{
    line_has_id, probe_detail, AgentEvent, CliAdapter, FollowUp, LineExchange, LoginFlow,
    ModelListing, PermissionContext, PermissionDecision,
};
use crate::availability::{
    window_name_for_minutes, AccountInfo, Evidence, ProbeOutcome, RateLimitReading,
};
use crate::models::{CliId, CommandSpec, Job, ModelOption};

/// Codex 어댑터 — 대화도 `codex app-server`(stdio JSON-RPC, jsonrpc 필드 없음)로 돌린다.
/// 실측(2026-09-04): `initialize` → `initialized` → `thread/start{cwd, approvalPolicy:"on-request", sandbox, model}`
/// → 응답 `result.thread.id` → `turn/start{threadId, input:[{type:"text", text}]}` → 알림
/// `item/agentMessage/delta`·`item/started|completed`(commandExecution·fileChange·agentMessage)·`account/rateLimits/updated`
/// → 샌드박스 밖 명령이면 서버 요청 `item/commandExecution/requestApproval{id, params{reason, command, cwd, availableDecisions}}`
/// (응답 `{id, result:{decision:"accept"|"acceptForSession"|"decline"|"cancel"}}`, decline이면 턴이 이어진다)
/// → `turn/completed{turn.status}`. stdin을 닫으면 종료 코드 0. 사용량·계정·모델 목록도 같은 채널로 읽는다.
/// 로그인은 `codex login`(브라우저). 프롬프트는 JSON에 실리므로 여러 줄도 그대로 간다.
pub struct CodexAdapter;

const RL_INIT_ID: u64 = 1;
const RL_READ_ID: u64 = 2;
/// 한도 교환에 같이 실어 보내는 `account/read` (2026-09-04 실측: `result.account{type,email,planType}`)
const ACCOUNT_READ_ID: u64 = 3;
const MODEL_LIST_ID: u64 = 2;
/// 대화 실행: thread/start 또는 thread/resume 요청 id. 응답의 thread.id로 turn/start(TURN_REQ_ID)를 이어 보낸다
const THREAD_REQ_ID: u64 = 2;
const TURN_REQ_ID: u64 = 3;

fn app_server_spec() -> CommandSpec {
    CommandSpec {
        program: "codex".into(),
        args: vec!["app-server".into()],
        env: vec![],
        cwd: String::new(),
        stdin: None,
    }
}

fn app_server_inputs(request: Value) -> Vec<String> {
    vec![
        json!({
            "id": RL_INIT_ID,
            "method": "initialize",
            "params": {"clientInfo": {"name": "agent-dock", "title": "Agent Dock", "version": env!("CARGO_PKG_VERSION")}}
        })
        .to_string(),
        json!({"method": "initialized", "params": {}}).to_string(),
        request.to_string(),
    ]
}

impl CodexAdapter {
    /// thread/start·thread/resume 공통 파라미터: 작업 폴더, 승인 정책(on-request = 샌드박스 밖 명령은 앱에 묻는다), 샌드박스, 모델
    fn thread_params(job: &Job) -> Value {
        let mut p = json!({
            "cwd": job.project_dir,
            "approvalPolicy": "on-request",
            "sandbox": if job.allow_writes { "workspace-write" } else { "read-only" },
        });
        if let Some(m) = job.model.as_deref().filter(|m| !m.is_empty()) {
            p["model"] = Value::String(m.to_string());
        }
        p
    }

    /// app-server를 띄우고 initialize → initialized → open(thread/start|resume)까지 stdin으로 넣는다.
    /// turn/start는 thread id를 알아야 하므로 stdin_follow_up이 응답을 보고 이어 보낸다.
    fn run_spec(job: &Job, open: Value) -> CommandSpec {
        let mut stdin = app_server_inputs(open).join("\n");
        stdin.push('\n');
        CommandSpec {
            program: "codex".into(),
            args: vec!["app-server".into()],
            env: vec![],
            cwd: job.project_dir.clone(),
            stdin: Some(stdin),
        }
    }

    fn turn_start_line(thread_id: &str, request: &str) -> String {
        json!({
            "id": TURN_REQ_ID,
            "method": "turn/start",
            "params": {"threadId": thread_id, "input": [{"type": "text", "text": request}]}
        })
        .to_string()
    }

    /// `account/rateLimits/read` 응답과 `account/rateLimits/updated` 알림이 공유하는 rateLimits 객체 → 윈도우 읽기.
    /// 계정 단위 primary/secondary만 쓴다(모델별 `rateLimitsByLimitId`는 MVP 제외).
    /// `rateLimitReachedType`이 채워져 있으면 한도 도달로 보고 primary 사용률을 100%로 올린다.
    fn rate_limit_readings(rl: &Value) -> Vec<RateLimitReading> {
        let reached = rl
            .get("rateLimitReachedType")
            .map_or(false, |t| !t.is_null());
        let mut out = vec![];
        for (i, key) in ["primary", "secondary"].iter().enumerate() {
            let Some(w) = rl.get(*key).filter(|w| !w.is_null()) else {
                continue;
            };
            let mins = w.get("windowDurationMins").and_then(Value::as_i64).unwrap_or(0);
            let mut utilization = w.get("usedPercent").and_then(Value::as_f64).unwrap_or(0.0) / 100.0;
            if reached && i == 0 {
                utilization = utilization.max(1.0);
            }
            out.push(RateLimitReading {
                window: window_name_for_minutes(mins),
                utilization,
                resets_at: w.get("resetsAt").and_then(Value::as_i64).unwrap_or(0),
            });
        }
        out
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
        Self::run_spec(
            job,
            json!({"id": THREAD_REQ_ID, "method": "thread/start", "params": Self::thread_params(job)}),
        )
    }

    /// `thread/resume{threadId}`로 같은 스레드를 이어간다 (app-server 스키마 ThreadResumeParams).
    fn build_resume_command(&self, job: &Job, session_id: &str) -> Option<CommandSpec> {
        let mut params = Self::thread_params(job);
        params["threadId"] = Value::String(session_id.to_string());
        Some(Self::run_spec(
            job,
            json!({"id": THREAD_REQ_ID, "method": "thread/resume", "params": params}),
        ))
    }

    fn keeps_stdin_open(&self) -> bool {
        true
    }

    /// codex는 stderr에 tracing 로그(`2026-09-04T13:40:14Z ERROR codex_core::tools::router: error=...`)를 쏟는다.
    /// ERROR·WARN만 "Codex ERROR: 메시지"로 줄여 보이고 INFO 이하는 버린다. tracing 형식이 아니면 그대로 통과
    fn filter_stderr(&self, line: &str) -> Option<String> {
        let t = line.trim();
        let is_tracing = t.len() > 20
            && t.as_bytes()[4] == b'-'
            && t.as_bytes()[7] == b'-'
            && t.as_bytes()[10] == b'T'
            && t.as_bytes()[..4].iter().all(u8::is_ascii_digit);
        if !is_tracing {
            return Some(t.to_string());
        }
        let level = ["ERROR", "WARN"]
            .iter()
            .find(|l| t.contains(&format!(" {l} ")))?;
        let after_level = &t[t.find(level)? + level.len()..];
        let msg = after_level
            .split_once(": ")
            .map(|(_, m)| m)
            .unwrap_or(after_level)
            .trim();
        let msg = msg.strip_prefix("error=").unwrap_or(msg);
        Some(format!("Codex {level}: {msg}"))
    }

    /// thread/start·resume 응답(id=THREAD_REQ_ID)의 thread.id를 받으면 turn/start를 보낸다
    fn stdin_follow_up(&self, job: &Job) -> Option<FollowUp> {
        let request = job.request.clone();
        Some(Box::new(move |line: &str| {
            let v: Value = serde_json::from_str(line).ok()?;
            if v.get("method").is_some() || v.get("id").and_then(Value::as_u64) != Some(THREAD_REQ_ID) {
                return None;
            }
            let tid = v.pointer("/result/thread/id").and_then(Value::as_str)?;
            Some(Self::turn_start_line(tid, &request))
        }))
    }

    /// 서버 요청 id에 `{decision}`으로 답한다. remember는 CLI가 허용한 경우(availableDecisions에 acceptForSession)만 세션 승인
    fn permission_reply(
        &self,
        ctx: &PermissionContext,
        decision: &PermissionDecision,
    ) -> Option<String> {
        let id: u64 = ctx.request_id.parse().ok()?;
        let session_ok = serde_json::from_str::<Value>(ctx.suggestions)
            .ok()
            .and_then(|d| {
                d.as_array()
                    .map(|a| a.iter().any(|x| x.as_str() == Some("acceptForSession")))
            })
            .unwrap_or(false);
        let d = if !decision.allow {
            "decline"
        } else if decision.remember && session_ok {
            "acceptForSession"
        } else {
            "accept"
        };
        Some(json!({"id": id, "result": {"decision": d}}).to_string())
    }

    fn parse_event(&self, line: &str) -> Vec<AgentEvent> {
        // 주의: codex는 stderr에 진단 로그를 섞어 낸다. stdout 라인만 이 함수로 들어와야 한다.
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
        // 요청 응답: thread/start·resume → 세션 id, 오류 응답 → 실패
        if v.get("method").is_none() {
            if let Some(id) = v.get("id").and_then(Value::as_u64) {
                if let Some(err) = v.get("error") {
                    return vec![AgentEvent::Completed {
                        ok: false,
                        summary: format!("{} (요청 {id})", text(err, "message")),
                    }];
                }
                if id == THREAD_REQ_ID {
                    if let Some(tid) = v.pointer("/result/thread/id").and_then(Value::as_str) {
                        return vec![AgentEvent::SessionStarted {
                            session_id: tid.to_string(),
                        }];
                    }
                }
            }
            return vec![];
        }
        let method = v.get("method").and_then(Value::as_str).unwrap_or("");
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "item/agentMessage/delta" => vec![AgentEvent::Message {
                text: text(&params, "delta"),
                delta: true,
            }],
            "item/started" => {
                let item = params.get("item").cloned().unwrap_or(Value::Null);
                match item.get("type").and_then(Value::as_str) {
                    Some("commandExecution") => vec![AgentEvent::ToolUse {
                        tool: "command".into(),
                        detail: text(&item, "command"),
                    }],
                    _ => vec![],
                }
            }
            "item/completed" => {
                let item = params.get("item").cloned().unwrap_or(Value::Null);
                match item.get("type").and_then(Value::as_str) {
                    Some("fileChange") => {
                        let status = text(&item, "status");
                        let ok = status != "failed" && status != "declined";
                        item.get("changes")
                            .and_then(Value::as_array)
                            .map(|cs| {
                                cs.iter()
                                    .map(|c| AgentEvent::FileChange {
                                        path: text(c, "path"),
                                        ok,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    }
                    Some("commandExecution") if text(&item, "status") == "failed" => {
                        vec![AgentEvent::Stderr {
                            text: format!(
                                "명령 실패: {}",
                                item.get("aggregatedOutput")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .trim()
                            ),
                        }]
                    }
                    _ => vec![],
                }
            }
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                let request_id = match v.get("id") {
                    Some(Value::Number(n)) => n.to_string(),
                    Some(Value::String(s)) => s.clone(),
                    _ => return vec![],
                };
                let is_cmd = method == "item/commandExecution/requestApproval";
                // 파일 변경 승인은 스키마상 acceptForSession을 항상 받는다; 명령은 CLI가 알려준 목록을 따른다
                let decisions = params.get("availableDecisions").cloned().unwrap_or_else(|| {
                    if is_cmd {
                        json!(["accept", "decline", "cancel"])
                    } else {
                        json!(["accept", "acceptForSession", "decline", "cancel"])
                    }
                });
                let can_remember = decisions
                    .as_array()
                    .map(|a| a.iter().any(|d| d.as_str() == Some("acceptForSession")))
                    .unwrap_or(false);
                let input = if is_cmd {
                    json!({"command": params.get("command"), "cwd": params.get("cwd"), "reason": params.get("reason")})
                } else {
                    json!({"file_path": params.get("grantRoot"), "reason": params.get("reason")})
                };
                vec![AgentEvent::PermissionRequest {
                    request_id,
                    tool: if is_cmd { "command" } else { "file_change" }.into(),
                    description: text(&params, "reason"),
                    input: input.to_string(),
                    can_remember,
                    suggestions: decisions.to_string(),
                }]
            }
            "turn/completed" => {
                let status = v
                    .pointer("/params/turn/status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                let err = v
                    .pointer("/params/turn/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                vec![AgentEvent::Completed {
                    ok: status == "completed",
                    summary: if status == "completed" {
                        String::new()
                    } else if err.is_empty() {
                        format!("턴 {status}")
                    } else {
                        err.to_string()
                    },
                }]
            }
            // 턴 오류 알림(재시도 여부 포함). 한도·인증 문구는 observe_run_event가 stderr 분류로 잡는다
            "error" => vec![AgentEvent::Stderr {
                text: v
                    .pointer("/params/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("오류")
                    .to_string(),
            }],
            "account/rateLimits/updated" => params
                .get("rateLimits")
                .map(|rl| {
                    Self::rate_limit_readings(rl)
                        .into_iter()
                        .map(|r| AgentEvent::RateLimit {
                            window: r.window,
                            utilization: r.utilization,
                            resets_at: r.resets_at,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            _ => vec![],
        }
    }

    fn rate_limit_exchange(&self) -> Option<LineExchange> {
        let mut inputs = app_server_inputs(
            json!({"id": RL_READ_ID, "method": "account/rateLimits/read", "params": {}}),
        );
        inputs.push(json!({"id": ACCOUNT_READ_ID, "method": "account/read", "params": {}}).to_string());
        Some(LineExchange {
            spec: app_server_spec(),
            inputs,
            done_id: ACCOUNT_READ_ID,
        })
    }

    fn parse_account(&self, lines: &[String]) -> Option<AccountInfo> {
        let line = lines.iter().find(|l| line_has_id(l, ACCOUNT_READ_ID))?;
        let v = serde_json::from_str::<Value>(line).ok()?;
        let acc = v.pointer("/result/account")?;
        let text = |k: &str| acc.get(k).and_then(Value::as_str).map(String::from);
        Some(AccountInfo {
            label: text("email").unwrap_or_else(|| "ChatGPT 계정".into()),
            plan: text("planType"),
            method: text("type"),
        })
    }

    fn parse_rate_limits(&self, lines: &[String]) -> Vec<RateLimitReading> {
        for line in lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if v.get("id").and_then(Value::as_u64) != Some(RL_READ_ID) {
                continue;
            }
            return v
                .pointer("/result/rateLimits")
                .map(Self::rate_limit_readings)
                .unwrap_or_default();
        }
        vec![]
    }

    fn version_command(&self) -> Option<CommandSpec> {
        Some(CommandSpec {
            program: "codex".into(),
            args: vec!["--version".into()],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        })
    }

    fn login_flow(&self) -> Option<LoginFlow> {
        Some(LoginFlow::Console {
            spec: CommandSpec {
                program: "codex".into(),
                args: vec!["login".into()],
                env: vec![],
                cwd: String::new(),
                stdin: None,
            },
            hint: "브라우저가 열리면 ChatGPT 계정으로 로그인하세요.".into(),
        })
    }

    /// app-server `model/list`(2026-09-04 실측: params.limit 필요, `result.data[]{id, displayName, description, isDefault, hidden}`)
    fn model_listing(&self) -> ModelListing {
        ModelListing::Exchange(LineExchange {
            spec: app_server_spec(),
            inputs: app_server_inputs(
                json!({"id": MODEL_LIST_ID, "method": "model/list", "params": {"limit": 100}}),
            ),
            done_id: MODEL_LIST_ID,
        })
    }

    fn parse_models(&self, lines: &[String]) -> Vec<ModelOption> {
        let Some(line) = lines.iter().find(|l| line_has_id(l, MODEL_LIST_ID)) else {
            return vec![];
        };
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return vec![];
        };
        v.pointer("/result/data")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|m| !m.get("hidden").and_then(Value::as_bool).unwrap_or(false))
                    .filter_map(|m| {
                        let id = m.get("id").and_then(Value::as_str)?;
                        let name = m.get("displayName").and_then(Value::as_str).unwrap_or(id);
                        let desc = m.get("description").and_then(Value::as_str).unwrap_or("");
                        Some(ModelOption {
                            id: id.to_string(),
                            label: if desc.is_empty() {
                                name.to_string()
                            } else {
                                format!("{name} — {desc}")
                            },
                            is_default: m.get("isDefault").and_then(Value::as_bool).unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
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
                account: None,
            }
        } else if code == Some(0) {
            ProbeOutcome::Ready {
                evidence: Evidence::Estimated,
                version: None,
                account: None,
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

    #[test]
    fn login_status_is_interpreted() {
        assert_eq!(
            CodexAdapter.interpret_probe(Some(0), "Logged in using ChatGPT\n", ""),
            ProbeOutcome::Ready {
                evidence: Evidence::CliReported,
                version: None,
                account: None,
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

    /// 2026-09-04 실측 응답(요약)으로 파싱 검증
    #[test]
    fn app_server_rate_limits_are_parsed() {
        let lines = vec![
            r#"{"id":1,"result":{"userAgent":"agent-dock/0.152.1","codexHome":"C:\\Users\\User\\.codex"}}"#.to_string(),
            r#"{"method":"remoteControl/status/changed","params":{"status":"disabled"}}"#.to_string(),
            r#"{"id":2,"result":{"rateLimits":{"limitId":"codex","limitName":null,"primary":{"usedPercent":46,"windowDurationMins":10080,"resetsAt":1788748787},"secondary":null,"credits":{"hasCredits":false},"planType":"prolite","rateLimitReachedType":null},"rateLimitsByLimitId":{}}}"#.to_string(),
        ];
        let r = CodexAdapter.parse_rate_limits(&lines);
        assert_eq!(
            r,
            vec![RateLimitReading {
                window: "seven_day".into(),
                utilization: 0.46,
                resets_at: 1788748787
            }]
        );

        let reached = vec![
            r#"{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":100,"windowDurationMins":300,"resetsAt":1788517917},"secondary":{"usedPercent":52,"windowDurationMins":10080,"resetsAt":1789104717},"rateLimitReachedType":"primary"}}}"#.to_string(),
        ];
        let r = CodexAdapter.parse_rate_limits(&reached);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].window, "five_hour");
        assert!(r[0].utilization >= 1.0);
        assert_eq!(r[1].window, "seven_day");
        assert!((r[1].utilization - 0.52).abs() < 1e-9);

        assert!(CodexAdapter.parse_rate_limits(&[]).is_empty());
        let ex = CodexAdapter.rate_limit_exchange().unwrap();
        assert_eq!(ex.spec.args, vec!["app-server"]);
        assert_eq!(ex.inputs.len(), 4);
        assert!(ex.inputs[2].contains("account/rateLimits/read"));
        assert!(ex.inputs[3].contains("account/read"));
        assert_eq!(ex.done_id, 3);

        // 2026-09-04 실측 account/read 응답
        let acc_lines = vec![
            r#"{"id":3,"result":{"account":{"type":"chatgpt","email":"user@example.com","planType":"prolite"},"requiresOpenaiAuth":true}}"#.to_string(),
        ];
        assert_eq!(
            CodexAdapter.parse_account(&acc_lines),
            Some(AccountInfo {
                label: "user@example.com".into(),
                plan: Some("prolite".into()),
                method: Some("chatgpt".into()),
            })
        );
        assert_eq!(CodexAdapter.parse_account(&lines), None);
    }

    /// 2026-09-04 실측 `model/list` 응답(요약)
    #[test]
    fn model_list_is_parsed_and_model_flag_applied() {
        let lines = vec![
            r#"{"id":1,"result":{"userAgent":"x"}}"#.to_string(),
            r#"{"id":2,"result":{"data":[{"id":"gpt-5.6-sol","displayName":"GPT-5.6-Sol","description":"Reliable agentic workhorse.","hidden":false,"isDefault":true},{"id":"gpt-5.6-terra","displayName":"GPT-5.6-Terra","description":"Balanced.","hidden":false,"isDefault":false},{"id":"secret","displayName":"Hidden","hidden":true}]}}"#.to_string(),
        ];
        let models = CodexAdapter.parse_models(&lines);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-5.6-sol");
        assert!(models[0].is_default);
        assert!(models[0].label.starts_with("GPT-5.6-Sol"));
        match CodexAdapter.model_listing() {
            ModelListing::Exchange(ex) => assert!(ex.inputs[2].contains("model/list")),
            _ => panic!("교환 방식이어야 함"),
        }

        let job = Job {
            id: 0,
            title: "t".into(),
            request: "hi".into(),
            project_dir: "D:\\x".into(),
            profile: "코딩 작업".into(),
            allow_writes: false,
            unattended_ok: false,
            status: JobStatus::Starting,
            model: Some("gpt-5.6-terra".into()),
        };
        let spec = CodexAdapter.build_resume_command(&job, "thr_1").unwrap();
        assert_eq!(spec.args, vec!["app-server"]);
        let stdin = spec.stdin.unwrap();
        let open: Value = serde_json::from_str(stdin.lines().nth(2).unwrap()).unwrap();
        assert_eq!(open["method"], "thread/resume");
        assert_eq!(open["params"]["threadId"], "thr_1");
        assert_eq!(open["params"]["model"], "gpt-5.6-terra");
        assert_eq!(open["params"]["sandbox"], "read-only");
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

    /// 2026-09-04 실측: thread/start → 응답 thread.id → turn/start (follow-up), 여러 줄 프롬프트는 JSON에 실린다
    #[test]
    fn app_server_run_opens_thread_then_follows_up_with_turn_start() {
        let job = job_with("line one\nline two", true);
        let spec = CodexAdapter.build_command(&job);
        assert_eq!(spec.args, vec!["app-server"]);
        assert!(CodexAdapter.keeps_stdin_open());
        let stdin = spec.stdin.unwrap();
        let lines: Vec<&str> = stdin.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("\"initialize\""));
        assert!(lines[1].contains("\"initialized\""));
        let open: Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(open["id"], THREAD_REQ_ID);
        assert_eq!(open["method"], "thread/start");
        assert_eq!(open["params"]["cwd"], "D:\\dev");
        assert_eq!(open["params"]["approvalPolicy"], "on-request");
        assert_eq!(open["params"]["sandbox"], "workspace-write");
        assert!(open["params"].get("model").is_none());

        let follow = CodexAdapter.stdin_follow_up(&job).unwrap();
        let reply = r#"{"id":2,"result":{"thread":{"id":"01a06c9c-9a1b-7f81-82df-0c77b60d4096","cwd":"D:\\dev"},"model":"gpt-5.6-sol"}}"#;
        let turn: Value = serde_json::from_str(&follow(reply).unwrap()).unwrap();
        assert_eq!(turn["id"], TURN_REQ_ID);
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "01a06c9c-9a1b-7f81-82df-0c77b60d4096");
        assert_eq!(turn["params"]["input"][0]["text"], "line one\nline two");
        assert!(follow(r#"{"method":"thread/started","params":{"thread":{"id":"x"}}}"#).is_none());
        assert!(follow(r#"{"id":1,"result":{"userAgent":"x"}}"#).is_none());
        // 세션 id는 같은 응답에서 나온다
        assert!(matches!(
            &CodexAdapter.parse_event(reply)[..],
            [AgentEvent::SessionStarted { session_id }] if session_id == "01a06c9c-9a1b-7f81-82df-0c77b60d4096"
        ));
    }

    /// 2026-09-04 실측 알림·서버 요청(요약)
    #[test]
    fn app_server_notifications_become_events() {
        let delta = CodexAdapter.parse_event(
            r#"{"method":"item/agentMessage/delta","params":{"threadId":"t","turnId":"u","itemId":"m","delta":"done"},"emittedAtMs":1}"#,
        );
        assert!(matches!(&delta[..], [AgentEvent::Message { text, delta: true }] if text == "done"));

        let started = CodexAdapter.parse_event(
            r#"{"method":"item/started","params":{"item":{"type":"commandExecution","id":"exec-1","command":"powershell -Command dir","cwd":"D:\\dev","status":"inProgress"},"threadId":"t","turnId":"u","startedAtMs":1}}"#,
        );
        assert!(matches!(&started[..], [AgentEvent::ToolUse { tool, detail }] if tool == "command" && detail.contains("dir")));

        let approval = CodexAdapter.parse_event(
            r#"{"method":"item/commandExecution/requestApproval","id":0,"params":{"kind":"command","threadId":"t","turnId":"u","itemId":"exec-2","startedAtMs":1,"reason":"Allow creating approval_probe.txt?","command":"powershell -Command \"Set-Content x\"","cwd":"D:\\dev","availableDecisions":["accept",{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["Set-Content"]}},"cancel"]}}"#,
        );
        match &approval[..] {
            [AgentEvent::PermissionRequest { request_id, tool, description, input, can_remember, suggestions }] => {
                assert_eq!(request_id, "0");
                assert_eq!(tool, "command");
                assert!(description.starts_with("Allow creating"));
                let i: Value = serde_json::from_str(input).unwrap();
                assert!(i["command"].as_str().unwrap().contains("Set-Content"));
                assert_eq!(i["cwd"], "D:\\dev");
                assert!(!*can_remember, "acceptForSession이 없으면 세션 승인 버튼을 감춘다");
                assert!(suggestions.contains("accept"));
            }
            other => panic!("unexpected {other:?}"),
        }

        let file = CodexAdapter.parse_event(
            r#"{"method":"item/fileChange/requestApproval","id":4,"params":{"threadId":"t","turnId":"u","itemId":"patch-1","startedAtMs":1,"reason":"Apply changes outside workspace","grantRoot":"D:\\other"}}"#,
        );
        assert!(matches!(&file[..], [AgentEvent::PermissionRequest { request_id, tool, can_remember: true, .. }] if request_id == "4" && tool == "file_change"));

        let done = CodexAdapter.parse_event(
            r#"{"method":"turn/completed","params":{"threadId":"t","turn":{"id":"u","items":[],"itemsView":"summary","status":"completed","error":null}}}"#,
        );
        assert!(matches!(&done[..], [AgentEvent::Completed { ok: true, .. }]));
        let failed = CodexAdapter.parse_event(
            r#"{"method":"turn/completed","params":{"threadId":"t","turn":{"id":"u","status":"failed","error":{"message":"usage limit reached"}}}}"#,
        );
        assert!(matches!(&failed[..], [AgentEvent::Completed { ok: false, summary }] if summary == "usage limit reached"));

        let rl = CodexAdapter.parse_event(
            r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":52,"windowDurationMins":10080,"resetsAt":1788748787},"secondary":null,"planType":"prolite","rateLimitReachedType":null}}}"#,
        );
        assert!(matches!(&rl[..], [AgentEvent::RateLimit { window, utilization, resets_at: 1788748787 }] if window == "seven_day" && (*utilization - 0.52).abs() < 1e-9));

        let err = CodexAdapter.parse_event(r#"{"id":3,"error":{"code":-32600,"message":"thread not found"}}"#);
        assert!(matches!(&err[..], [AgentEvent::Completed { ok: false, summary }] if summary.contains("thread not found")));
        assert!(CodexAdapter.parse_event(r#"{"method":"thread/status/changed","params":{"threadId":"t","status":{"type":"idle"}}}"#).is_empty());
    }

    #[test]
    fn approval_replies_follow_available_decisions() {
        let ctx = PermissionContext {
            request_id: "0",
            input: "{}",
            suggestions: r#"["accept","cancel"]"#,
        };
        let allow = |remember| PermissionDecision { allow: true, remember, message: None };
        let v: Value = serde_json::from_str(&CodexAdapter.permission_reply(&ctx, &allow(false)).unwrap()).unwrap();
        assert_eq!(v["id"], 0);
        assert_eq!(v["result"]["decision"], "accept");
        // acceptForSession이 목록에 없으면 remember여도 accept로 낮춘다
        let v: Value = serde_json::from_str(&CodexAdapter.permission_reply(&ctx, &allow(true)).unwrap()).unwrap();
        assert_eq!(v["result"]["decision"], "accept");
        let ctx2 = PermissionContext { suggestions: r#"["accept","acceptForSession","decline","cancel"]"#, ..ctx };
        let v: Value = serde_json::from_str(&CodexAdapter.permission_reply(&ctx2, &allow(true)).unwrap()).unwrap();
        assert_eq!(v["result"]["decision"], "acceptForSession");
        let deny = PermissionDecision { allow: false, remember: false, message: None };
        let v: Value = serde_json::from_str(&CodexAdapter.permission_reply(&ctx2, &deny).unwrap()).unwrap();
        assert_eq!(v["result"]["decision"], "decline");
        assert!(CodexAdapter.permission_reply(&PermissionContext { request_id: "abc", ..ctx2 }, &deny).is_none());
    }

    /// 2026-09-04 실측 stderr 줄(ANSI 제거 후)
    #[test]
    fn stderr_diagnostics_are_folded() {
        let err = "2026-09-04T13:40:14.548893Z ERROR codex_core::tools::router: error=exec_command failed for `powershell`: CreateProcess { message: \"Rejected\" }";
        assert_eq!(
            CodexAdapter.filter_stderr(err).unwrap(),
            "Codex ERROR: exec_command failed for `powershell`: CreateProcess { message: \"Rejected\" }"
        );
        assert!(CodexAdapter
            .filter_stderr("2026-09-04T13:40:14.548893Z  INFO codex_core::rollout: recorder started")
            .is_none());
        assert_eq!(
            CodexAdapter.filter_stderr("2026-09-04T13:40:14Z  WARN codex_core::x: something odd").unwrap(),
            "Codex WARN: something odd"
        );
        assert_eq!(CodexAdapter.filter_stderr("plain text").unwrap(), "plain text");
    }
}
