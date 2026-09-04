use serde::{Deserialize, Serialize};

use crate::models::CliId;

/// 상태 근거 구분 (PRD 5장 표): 공식 값 / CLI 제시 값 / 추정
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Evidence {
    Official,
    CliReported,
    Estimated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityState {
    Available,
    Degraded,
    Cooldown,
    AuthRequired,
    Unavailable,
    Unknown,
}

/// 한도 윈도우 하나 (예: five_hour, seven_day).
/// Claude·Codex 구독은 윈도우가 복수라서 단일 cooldown_until로는 부족하다 (PRD 8·11장).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateWindow {
    pub name: String,
    /// 0.0~1.0, None = 신호 없음(추정 상태)
    pub utilization: Option<f64>,
    /// epoch seconds, None = 미상
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailabilitySnapshot {
    pub cli: CliId,
    pub state: AvailabilityState,
    pub evidence: Evidence,
    pub windows: Vec<RateWindow>,
    pub checked_at: i64,
}

impl AvailabilitySnapshot {
    /// 모든 윈도우 중 최고 사용률. 선제 handoff 임계치(기본 0.95) 판정 기준 (PRD 6장).
    pub fn max_utilization(&self) -> Option<f64> {
        self.windows
            .iter()
            .filter_map(|w| w.utilization)
            .fold(None, |acc: Option<f64>, u| Some(acc.map_or(u, |a| a.max(u))))
    }

    /// 복귀 판단은 알려진 모든 윈도우가 리셋됐을 때만 true (PRD 10장).
    pub fn all_windows_reset(&self, now: i64) -> bool {
        self.windows
            .iter()
            .all(|w| w.resets_at.map_or(true, |t| t <= now))
    }
}
