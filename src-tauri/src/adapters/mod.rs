pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod gemini;
pub mod opencode;

use serde_json::Value;

use crate::availability::{AccountInfo, Evidence, ProbeOutcome, RateLimitReading};
use crate::models::{CliId, CommandSpec, Job, ModelOption};

/// CLI별 스트림에서 파싱된 공통 이벤트.
/// 변형들은 스파이크 0 실측 이벤트 형태를 기준으로 한다 (PRD 15장).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionStarted { session_id: String },
    /// delta=true면 직전 assistant 메시지에 이어 붙이는 조각 (Gemini 스트리밍 실측)
    Message { text: String, delta: bool },
    ToolUse { tool: String, detail: String },
    FileChange { path: String, ok: bool },
    RateLimit { window: String, utilization: f64, resets_at: i64 },
    Completed { ok: bool, summary: String },
    /// stderr 라인 (Codex가 진단 로그를 stderr에 섞는 실측 반영 — stdout과 분리 수집)
    Stderr { text: String },
    /// 프로세스 종료. Completed와 별개로 runner가 항상 마지막에 보낸다.
    /// cancelled=true면 사용자 중지로 끝난 것이라 비정상 종료로 다루지 않는다.
    ProcessExited { code: Option<i32>, cancelled: bool },
    /// CLI가 도구 사용 승인을 요청함 (Claude `control_request/can_use_tool`, 2026-09-04 실측).
    /// 앱이 respond_permission으로 답할 때까지 CLI는 기다린다. input·suggestions는 JSON 문자열(응답에 그대로 돌려줌)
    PermissionRequest {
        request_id: String,
        tool: String,
        description: String,
        input: String,
        /// "세션 동안 허용" 제안(permission_suggestions)이 있는지
        can_remember: bool,
        suggestions: String,
    },
    /// 승인 요청이 처리됨. auto=true면 응답 시간 초과로 앱이 자동 거부한 것
    PermissionResolved {
        request_id: String,
        allowed: bool,
        auto: bool,
    },
}

/// 실행 중 stdout 한 줄에 대한 어댑터의 반응: 이어 보낼 줄(Codex turn/start, Gemini session/prompt),
/// 이 줄의 이벤트를 버릴지(Gemini session/load가 과거 대화를 다시 흘려보낼 때)
#[derive(Default)]
pub struct LineReaction {
    pub send: Option<String>,
    pub drop_events: bool,
}

/// 실행 중 stdout 한 줄을 보고 반응을 정하는 콜백. 상태가 필요하면 클로저 안에 Atomic으로 둔다
pub type FollowUp = Box<dyn Fn(&str) -> LineReaction + Send + Sync>;

/// 승인 응답을 만들 때 넘기는 원본 요청 (request_id, 도구 입력 JSON, 제안 JSON)
pub struct PermissionContext<'a> {
    pub request_id: &'a str,
    pub input: &'a str,
    pub suggestions: &'a str,
}

/// 사용자의 승인 결정. remember=true면 CLI가 제안한 범위(세션 동안 같은 종류 허용)를 함께 적용한다.
#[derive(Debug, Clone)]
pub struct PermissionDecision {
    pub allow: bool,
    pub remember: bool,
    pub message: Option<String>,
}

/// 줄 단위 교환(stdio JSON-RPC 등): 명령, 보낼 줄들, 마지막 요청의 id(이 응답을 받으면 끝).
/// 용도: Codex app-server(한도·모델 목록), Gemini ACP(모델 목록·로그인).
pub struct LineExchange {
    pub spec: CommandSpec,
    pub inputs: Vec<String>,
    pub done_id: u64,
}

/// 앱에서 띄우는 로그인 흐름 (PRD 6장: 기존 CLI 로그인 방식 그대로 사용).
/// Console은 보이는 콘솔 창에서 CLI의 로그인 명령을 그대로 실행하고 창이 닫히면 재검사한다.
/// Exchange는 stdio 프로토콜(Gemini ACP `authenticate`)로 진행한다.
pub enum LoginFlow {
    Console { spec: CommandSpec, hint: String },
    /// 현재 사용처 없음(Gemini 개인 OAuth 종료). ACP authenticate 방식 CLI가 생기면 다시 쓴다
    #[allow(dead_code)]
    Exchange { exchange: LineExchange, hint: String },
}

/// 모델 목록 출처
pub enum ModelListing {
    Static(Vec<ModelOption>),
    Exchange(LineExchange),
    Command(CommandSpec),
    /// CLI 실행 파일을 읽어 어댑터의 scan_models로 모델 id를 추출한다 (목록 명령이 없는 Claude Code용).
    /// candidates 중 처음 존재하는 파일을 쓴다. 하나도 없으면 빈 바이트로 호출한다.
    Scan { candidates: Vec<std::path::PathBuf> },
}

/// JSON-RPC 계열 응답 줄이 특정 id의 응답인지
pub fn line_has_id(line: &str, id: u64) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("id").and_then(Value::as_u64))
        == Some(id)
}

pub fn model_opt(id: &str, label: &str, is_default: bool) -> ModelOption {
    ModelOption {
        id: id.into(),
        label: label.into(),
        is_default,
    }
}

/// PRD 9장 어댑터 계약.
/// 1단계 범위: 명령 조립 + 이벤트 파싱 + probe 해석 + (선택) 사용량 교환·로그인·모델 목록.
/// 프로세스 생성·중지는 runner가 맡는다.
pub trait CliAdapter: Send + Sync {
    fn id(&self) -> CliId;

    /// 설치·인증 확인용 경량 probe 명령
    fn probe_command(&self) -> CommandSpec;

    /// probe를 줄 단위 교환으로 하는 CLI(Gemini ACP)는 이것을 돌려준다. Some이면 probe_command 대신 쓰고
    /// 결과는 interpret_probe_lines로 해석한다.
    fn probe_exchange(&self) -> Option<LineExchange> {
        None
    }

    /// probe_exchange 응답 줄들의 해석
    fn interpret_probe_lines(&self, _lines: &[String]) -> ProbeOutcome {
        ProbeOutcome::Unavailable {
            detail: "probe 교환 해석 미구현".into(),
        }
    }

    /// 헤드리스 구조화 실행 명령 (구조화 모드 기본, PRD 8장)
    fn build_command(&self, job: &Job) -> CommandSpec;

    /// stdout 한 줄(JSON)을 공통 이벤트들로 변환. 모르는 줄은 빈 Vec.
    fn parse_event(&self, line: &str) -> Vec<AgentEvent>;

    /// 기존 세션을 이어가는 후속 메시지 명령. 세션 재개를 지원하지 않는 CLI는 None.
    fn build_resume_command(&self, _job: &Job, _session_id: &str) -> Option<CommandSpec> {
        None
    }

    /// 공식 사용량을 읽는 교환. 없으면 None — Claude는 실행 스트림의 rate_limit_event로, Gemini·OpenCode는 신호가 없다.
    fn rate_limit_exchange(&self) -> Option<LineExchange> {
        None
    }

    /// rate_limit_exchange 응답 줄들을 윈도우 읽기로 변환
    fn parse_rate_limits(&self, _lines: &[String]) -> Vec<RateLimitReading> {
        Vec::new()
    }

    /// rate_limit_exchange 응답 줄들에서 계정 정보 추출 (Codex `account/read` 등). 없으면 None.
    fn parse_account(&self, _lines: &[String]) -> Option<AccountInfo> {
        None
    }

    /// 앱에서 띄울 로그인 흐름. None이면 앱 밖에서 로그인해야 한다.
    fn login_flow(&self) -> Option<LoginFlow> {
        None
    }

    /// 실행 중 stdin을 열어 두는지 (승인 응답 같은 후속 줄을 보내는 CLI). 기본 false — 프롬프트를 쓰고 닫는다
    fn keeps_stdin_open(&self) -> bool {
        false
    }

    /// stderr 한 줄을 화면에 보일지·어떻게 줄일지. None이면 버린다. 기본은 그대로 통과
    /// (Codex는 tracing 진단 로그를 stderr에 쏟으므로 ERROR·WARN만 남긴다)
    fn filter_stderr(&self, line: &str) -> Option<String> {
        Some(line.to_string())
    }

    /// keeps_stdin_open CLI가 실행 중 stdout 줄에 반응해 stdin으로 보낼 줄을 만드는 콜백. 기본 없음
    fn stdin_follow_up(&self, _job: &Job, _session_id: Option<&str>) -> Option<FollowUp> {
        None
    }

    /// 승인 요청에 대한 응답 줄(stdin으로 보냄). 승인 중계를 지원하지 않는 CLI는 None
    fn permission_reply(
        &self,
        _ctx: &PermissionContext,
        _decision: &PermissionDecision,
    ) -> Option<String> {
        None
    }

    /// 버전 확인 명령(첫 줄이 버전). probe가 버전을 못 주는 CLI용 — Ready로 판정된 뒤 한 번 실행한다.
    fn version_command(&self) -> Option<CommandSpec> {
        None
    }

    /// 모델 목록 출처
    fn model_listing(&self) -> ModelListing {
        ModelListing::Static(Vec::new())
    }

    /// Exchange/Command 방식 모델 목록의 출력 줄을 선택지로 변환
    fn parse_models(&self, _lines: &[String]) -> Vec<ModelOption> {
        Vec::new()
    }

    /// Scan 방식: 실행 파일 바이트에서 모델 선택지를 추출
    fn scan_models(&self, _bytes: &[u8]) -> Vec<ModelOption> {
        Vec::new()
    }

    /// probe 명령의 종료 코드·출력을 가용성 판단으로 해석한다.
    /// 기본: 정상 종료면 설치 확인(추정 근거), 첫 줄을 버전으로 본다.
    fn interpret_probe(&self, code: Option<i32>, stdout: &str, stderr: &str) -> ProbeOutcome {
        match code {
            Some(0) => ProbeOutcome::Ready {
                evidence: Evidence::Estimated,
                version: first_line(stdout),
                account: None,
            },
            _ => ProbeOutcome::Unavailable {
                detail: probe_detail(code, stdout, stderr),
            },
        }
    }
}

pub fn first_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(String::from)
}

pub fn probe_detail(code: Option<i32>, stdout: &str, stderr: &str) -> String {
    let msg = first_line(stderr)
        .or_else(|| first_line(stdout))
        .unwrap_or_default();
    match code {
        Some(c) => format!("exit {c}: {msg}"),
        None => format!("실행 불가 또는 시간 초과: {msg}"),
    }
}

/// ANSI 색상 코드 제거 (opencode 출력 등)
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// 라우팅 후보 순서(Codex → Claude → Antigravity → Gemini → OpenCode)와 같게 둔다.
pub fn registry() -> Vec<std::sync::Arc<dyn CliAdapter>> {
    vec![
        std::sync::Arc::new(codex::CodexAdapter),
        std::sync::Arc::new(claude::ClaudeAdapter),
        std::sync::Arc::new(antigravity::AntigravityAdapter),
        std::sync::Arc::new(gemini::GeminiAdapter),
        std::sync::Arc::new(opencode::OpenCodeAdapter),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_and_matches_ids() {
        assert_eq!(strip_ansi("\u{1b}[34m●\u{1b}[39m  OpenCode Zen \u{1b}[90mapi"), "●  OpenCode Zen api");
        assert!(line_has_id(r#"{"id":2,"result":{}}"#, 2));
        assert!(!line_has_id(r#"{"id":1,"result":{}}"#, 2));
        assert!(!line_has_id("not json", 2));
    }
}
