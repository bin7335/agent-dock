use serde::{Deserialize, Serialize};

use crate::availability::{AvailabilitySnapshot, AvailabilityState};
use crate::models::CliId;

/// 라우팅 프로필 (PRD 6장)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingProfile {
    pub name: String,
    pub chain: Vec<CliId>,
    /// 같은 작업의 자동 handoff 상한, 기본 2 (PRD 10장)
    pub max_auto_handoffs: u32,
}

impl Default for RoutingProfile {
    fn default() -> Self {
        Self {
            name: "코딩 작업".into(),
            chain: vec![CliId::Codex, CliId::Claude, CliId::Gemini, CliId::Opencode],
            max_auto_handoffs: 2,
        }
    }
}

/// 새 작업마다 체인 최우선부터 재평가해 첫 available CLI를 고른다 (PRD 6장).
/// 회복된 우선 CLI가 자동으로 다시 선택되는 것은 이 재평가로 구현된다.
pub fn pick_candidate(
    profile: &RoutingProfile,
    snapshots: &[AvailabilitySnapshot],
) -> Option<CliId> {
    profile.chain.iter().copied().find(|cli| {
        snapshots
            .iter()
            .any(|s| s.cli == *cli && s.state == AvailabilityState::Available)
    })
}

/// 선제 handoff 임계치 (PRD 6장). 요약 작성 자체가 사용량을 쓰므로 100%에 붙이지 않는다.
pub const PREEMPTIVE_HANDOFF_THRESHOLD: f64 = 0.95;

/// 실행 중 CLI가 임계치를 넘었는지 판정. 모든 윈도우 중 최고 사용률 기준.
pub fn should_preemptive_handoff(snapshot: &AvailabilitySnapshot) -> bool {
    snapshot
        .max_utilization()
        .map_or(false, |u| u >= PREEMPTIVE_HANDOFF_THRESHOLD)
}
