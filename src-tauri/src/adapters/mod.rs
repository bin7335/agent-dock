pub mod claude;
pub mod codex;
pub mod gemini;

use crate::models::{CliId, CommandSpec, Job};

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
    ProcessExited { code: Option<i32> },
}

/// PRD 9장 어댑터 계약.
/// 1단계 범위: 명령 조립 + 이벤트 파싱. 프로세스 생성·중지는 runner가 맡는다.
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
}

pub fn registry() -> Vec<std::sync::Arc<dyn CliAdapter>> {
    vec![
        std::sync::Arc::new(codex::CodexAdapter),
        std::sync::Arc::new(claude::ClaudeAdapter),
        std::sync::Arc::new(gemini::GeminiAdapter),
    ]
}
