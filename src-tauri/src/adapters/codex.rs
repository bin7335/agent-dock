use serde_json::{json, Value};

use super::{probe_detail, AgentEvent, CliAdapter, RateLimitExchange};
use crate::availability::{window_name_for_minutes, Evidence, ProbeOutcome, RateLimitReading};
use crate::models::{CliId, CommandSpec, Job};

/// Codex 어댑터.
/// 실측 근거: `codex exec --json` JSONL (스파이크 0, 2026-09-02).
/// 사용량 %는 exec 스트림에 없고 `codex app-server`(stdio JSON-RPC)의 `account/rateLimits/read`로 읽는다
/// (2026-09-04 실측: initialize → initialized → 요청, 응답 `result.rateLimits.primary{usedPercent,windowDurationMins,resetsAt}`).
/// 프롬프트는 아직 인자로 넘기므로 줄바꿈이 든 메시지는 실행 단계에서 거부된다 (TODO: stdin 전달 실측).
pub struct CodexAdapter;

const RL_INIT_ID: u64 = 1;
const RL_READ_ID: u64 = 2;

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

    fn rate_limit_exchange(&self) -> Option<RateLimitExchange> {
        Some(RateLimitExchange {
            spec: CommandSpec {
                program: "codex".into(),
                args: vec!["app-server".into()],
                env: vec![],
                cwd: String::new(),
                stdin: None,
            },
            inputs: vec![
                json!({
                    "id": RL_INIT_ID,
                    "method": "initialize",
                    "params": {"clientInfo": {"name": "agent-dock", "title": "Agent Dock", "version": env!("CARGO_PKG_VERSION")}}
                })
                .to_string(),
                json!({"method": "initialized", "params": {}}).to_string(),
                json!({"id": RL_READ_ID, "method": "account/rateLimits/read", "params": {}}).to_string(),
            ],
            done_id: RL_READ_ID,
        })
    }

    /// 계정 단위 `rateLimits`의 primary/secondary만 쓴다. 모델별 한도(`rateLimitsByLimitId`)는 MVP에서 제외.
    /// `rateLimitReachedType`이 채워져 있으면 한도 도달로 보고 primary 사용률을 100%로 올린다.
    fn parse_rate_limits(&self, lines: &[String]) -> Vec<RateLimitReading> {
        for line in lines {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if v.get("id").and_then(Value::as_u64) != Some(RL_READ_ID) {
                continue;
            }
            let Some(rl) = v.pointer("/result/rateLimits") else {
                return vec![];
            };
            let reached = rl
                .get("rateLimitReachedType")
                .map_or(false, |t| !t.is_null());
            let mut out = vec![];
            for (i, key) in ["primary", "secondary"].iter().enumerate() {
                let Some(w) = rl.get(*key).filter(|w| !w.is_null()) else {
                    continue;
                };
                let mins = w.get("windowDurationMins").and_then(Value::as_i64).unwrap_or(0);
                let mut utilization =
                    w.get("usedPercent").and_then(Value::as_f64).unwrap_or(0.0) / 100.0;
                if reached && i == 0 {
                    utilization = utilization.max(1.0);
                }
                out.push(RateLimitReading {
                    window: window_name_for_minutes(mins),
                    utilization,
                    resets_at: w.get("resetsAt").and_then(Value::as_i64).unwrap_or(0),
                });
            }
            return out;
        }
        vec![]
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
        assert_eq!(ex.inputs.len(), 3);
        assert!(ex.inputs[2].contains("account/rateLimits/read"));
    }
}
