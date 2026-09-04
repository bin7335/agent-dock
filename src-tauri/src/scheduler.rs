use serde::{Deserialize, Serialize};

use crate::availability::{AvailabilitySnapshot, AvailabilityState, HIGH_USAGE_THRESHOLD};
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
            chain: vec![
                CliId::Codex,
                CliId::Claude,
                CliId::Antigravity,
                CliId::Gemini,
                CliId::Opencode,
            ],
            max_auto_handoffs: 2,
        }
    }
}

/// 새 작업마다 체인 최우선부터 재평가해 첫 available CLI를 고른다 (PRD 6장).
/// 회복된 우선 CLI가 자동으로 다시 선택되는 것은 이 재평가로 구현된다.
/// degraded(임박)는 후보로 남기되 available보다 뒤에 둔다.
pub fn pick_candidate(
    profile: &RoutingProfile,
    snapshots: &[AvailabilitySnapshot],
) -> Option<CliId> {
    // 레지스트리에서 꺼진 CLI는 후보가 아니다
    let state_of = |cli: CliId| {
        snapshots
            .iter()
            .find(|s| s.cli == cli && s.enabled)
            .map(|s| s.state)
    };
    profile
        .chain
        .iter()
        .copied()
        .find(|cli| state_of(*cli) == Some(AvailabilityState::Available))
        .or_else(|| {
            profile
                .chain
                .iter()
                .copied()
                .find(|cli| state_of(*cli) == Some(AvailabilityState::Degraded))
        })
}

/// 선제 handoff 임계치 (PRD 6장). 요약 작성 자체가 사용량을 쓰므로 100%에 붙이지 않는다.
pub const PREEMPTIVE_HANDOFF_THRESHOLD: f64 = HIGH_USAGE_THRESHOLD;

/// 실행 중 CLI가 임계치를 넘었는지 판정. 모든 윈도우 중 최고 사용률 기준. 2단계 자동 폴백에서 배선한다.
#[allow(dead_code)]
pub fn should_preemptive_handoff(snapshot: &AvailabilitySnapshot) -> bool {
    snapshot
        .max_utilization()
        .map_or(false, |u| u >= PREEMPTIVE_HANDOFF_THRESHOLD)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::{AvailabilityMonitor, Evidence, ProbeOutcome};

    #[test]
    fn recovered_primary_is_reselected() {
        let mut m = AvailabilityMonitor::new(&[CliId::Codex, CliId::Claude, CliId::Gemini], 0);
        let ready = || ProbeOutcome::Ready {
            evidence: Evidence::CliReported,
            version: None,
            account: None,
        };
        m.apply_probe(CliId::Codex, ready(), 0);
        m.apply_probe(CliId::Claude, ready(), 0);
        let profile = RoutingProfile::default();
        assert_eq!(pick_candidate(&profile, &m.snapshots()), Some(CliId::Codex));

        m.apply_failure(CliId::Codex, "429 too many requests", 10);
        assert_eq!(pick_candidate(&profile, &m.snapshots()), Some(CliId::Claude));

        m.tick(10 + 31 * 60);
        assert_eq!(
            pick_candidate(&profile, &m.snapshots()),
            Some(CliId::Codex),
            "쿨다운 만료 후 우선 CLI로 복귀"
        );
    }

    #[test]
    fn degraded_is_last_resort() {
        let mut m = AvailabilityMonitor::new(&[CliId::Codex, CliId::Claude], 0);
        m.apply_rate_limit(CliId::Codex, "five_hour", 0.96, 1000, 0);
        assert_eq!(
            pick_candidate(&RoutingProfile::default(), &m.snapshots()),
            Some(CliId::Codex)
        );
        m.apply_probe(
            CliId::Claude,
            ProbeOutcome::Ready {
                evidence: Evidence::CliReported,
                version: None,
                account: None,
            },
            0,
        );
        assert_eq!(
            pick_candidate(&RoutingProfile::default(), &m.snapshots()),
            Some(CliId::Claude)
        );
    }
}
