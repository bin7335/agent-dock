pub mod claude;
pub mod codex;
pub mod gemini;

use crate::availability::{Evidence, ProbeOutcome, RateLimitReading};
use crate::models::{CliId, CommandSpec, Job};

/// 공식 사용량을 읽기 위한 줄 단위 교환: 명령, 보낼 줄들, 마지막 요청의 id(이 응답을 받으면 끝).
pub struct RateLimitExchange {
    pub spec: CommandSpec,
    pub inputs: Vec<String>,
    pub done_id: u64,
}

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
}

/// PRD 9장 어댑터 계약.
/// 1단계 범위: 명령 조립 + 이벤트 파싱 + probe 해석. 프로세스 생성·중지는 runner가 맡는다.
pub trait CliAdapter: Send + Sync {
    fn id(&self) -> CliId;

    /// 설치·인증 확인용 경량 probe 명령
    fn probe_command(&self) -> CommandSpec;

    /// 헤드리스 구조화 실행 명령 (구조화 모드 기본, PRD 8장)
    fn build_command(&self, job: &Job) -> CommandSpec;

    /// stdout 한 줄(JSON)을 공통 이벤트들로 변환. 모르는 줄은 빈 Vec.
    fn parse_event(&self, line: &str) -> Vec<AgentEvent>;

    /// 기존 세션을 이어가는 후속 메시지 명령. 세션 재개를 지원하지 않는 CLI는 None.
    fn build_resume_command(&self, _job: &Job, _session_id: &str) -> Option<CommandSpec> {
        None
    }

    /// 공식 사용량을 읽는 교환. 없으면 None — Claude는 실행 스트림의 rate_limit_event로, Gemini는 신호가 없다.
    fn rate_limit_exchange(&self) -> Option<RateLimitExchange> {
        None
    }

    /// rate_limit_exchange 응답 줄들을 윈도우 읽기로 변환
    fn parse_rate_limits(&self, _lines: &[String]) -> Vec<RateLimitReading> {
        Vec::new()
    }

    /// probe 명령의 종료 코드·출력을 가용성 판단으로 해석한다.
    /// 기본: 정상 종료면 설치 확인(추정 근거), 첫 줄을 버전으로 본다.
    fn interpret_probe(&self, code: Option<i32>, stdout: &str, stderr: &str) -> ProbeOutcome {
        match code {
            Some(0) => ProbeOutcome::Ready {
                evidence: Evidence::Estimated,
                version: first_line(stdout),
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

/// 라우팅 후보 순서(Codex → Claude → Gemini)와 같게 둔다.
pub fn registry() -> Vec<std::sync::Arc<dyn CliAdapter>> {
    vec![
        std::sync::Arc::new(codex::CodexAdapter),
        std::sync::Arc::new(claude::ClaudeAdapter),
        std::sync::Arc::new(gemini::GeminiAdapter),
    ]
}
