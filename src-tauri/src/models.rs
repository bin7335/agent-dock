use serde::{Deserialize, Serialize};

/// 등록 가능한 에이전트형 CLI (PRD 6장 레지스트리)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CliId {
    Claude,
    Codex,
    Gemini,
    Opencode,
}

/// 작업 상태 머신 (PRD 6장)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Starting,
    Running,
    WaitingApproval,
    Succeeded,
    Failed,
    Cooldown,
    HandoffPending,
    Cancelled,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: i64,
    pub title: String,
    pub request: String,
    pub project_dir: String,
    pub profile: String,
    /// 파일 쓰기 허용 여부 → CLI별 권한 플래그로 매핑 (PRD 15장 스파이크 결과)
    pub allow_writes: bool,
    /// 무인(자리 비움) 자동 handoff 허용 (PRD 5장 무인 정책)
    pub unattended_ok: bool,
    pub status: JobStatus,
}

/// 어댑터가 조립하는 실행 명령. 프로세스 생성은 runner가 담당한다.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: String,
}

/// 다음 CLI로 넘기는 최소 정보 (PRD 6장 Handoff 패킷)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffPacket {
    pub original_request: String,
    pub project_dir: String,
    pub stop_reason: String,
    pub done_summary: String,
    pub changed_files: Vec<String>,
    pub next_steps: Vec<String>,
}
