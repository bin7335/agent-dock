use std::collections::HashMap;

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
    /// 마지막 한도·인증·실행 오류 요약. 추정 상태의 근거를 투명하게 보여 준다 (PRD 7·11장)
    pub last_error: Option<String>,
    /// 다음 자동 재검사 시각 (epoch s): cooldown 만료 시각 또는 주기 probe
    pub next_check_at: Option<i64>,
    /// 리셋 시각 경과로 복귀한 시각. 복귀 직후 재발 판정에 쓴다 (PRD 10장)
    pub recovered_at: Option<i64>,
    pub version: Option<String>,
    /// 레지스트리 사용 여부 (PRD 6장). 꺼진 CLI는 상태바·라우팅·probe에서 빠진다.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// 고사용량(임박) 임계치. 선제 handoff 임계치와 같은 값 (PRD 6장)
pub const HIGH_USAGE_THRESHOLD: f64 = 0.95;
/// 주기 probe 간격. 경량 명령(version·login status)만 실행한다 (가벼움 우선, PRD 5장)
pub const PROBE_INTERVAL_SECS: i64 = 600;
/// 사용량 신호 없이 한도 오류만 관측됐을 때의 보수적 추정 쿨다운 (PRD 11장)
pub const DEFAULT_COOLDOWN_SECS: i64 = 30 * 60;
/// 리셋 직후 재발 시 장기 쿨다운 — 다른 윈도우 한도로 간주 (PRD 10장)
pub const LONG_COOLDOWN_SECS: i64 = 6 * 3600;
/// 복귀 후 이 시간 안에 다시 한도 오류가 나면 "재발"로 본다
pub const RELAPSE_WINDOW_SECS: i64 = 10 * 60;
/// 공식 신호 없이 추정한 쿨다운을 담는 가상 윈도우 이름
pub const ESTIMATED_WINDOW: &str = "estimated";

impl AvailabilitySnapshot {
    pub fn unknown(cli: CliId, now: i64) -> Self {
        Self {
            cli,
            state: AvailabilityState::Unknown,
            evidence: Evidence::Estimated,
            windows: Vec::new(),
            checked_at: now,
            last_error: None,
            next_check_at: Some(now),
            recovered_at: None,
            version: None,
            enabled: true,
        }
    }

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

    fn has_official_windows(&self) -> bool {
        self.windows.iter().any(|w| w.name != ESTIMATED_WINDOW)
    }

    /// 공식 사용률로 상태를 유도한다: 1.0 이상은 cooldown, 임계치 이상은 임박(degraded).
    fn state_from_windows(&self) -> AvailabilityState {
        match self.max_utilization() {
            Some(u) if u >= 1.0 => AvailabilityState::Cooldown,
            Some(u) if u >= HIGH_USAGE_THRESHOLD => AvailabilityState::Degraded,
            _ => AvailabilityState::Available,
        }
    }

    /// 아직 지나지 않은 리셋 시각 중 가장 이른 것
    fn earliest_pending_reset(&self, now: i64) -> Option<i64> {
        self.windows
            .iter()
            .filter_map(|w| w.resets_at)
            .filter(|t| *t > now)
            .min()
    }
}

/// 실행 실패 원문의 분류. 한도 오류와 코드·권한 오류를 구분해야 무작정 CLI를 바꾸지 않는다 (PRD 10장).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    RateLimit,
    Auth,
    Network,
    Other,
}

pub fn classify_failure(text: &str) -> FailureKind {
    let t = text.to_ascii_lowercase();
    const RATE: &[&str] = &[
        "rate limit",
        "rate_limit",
        "ratelimit",
        "429",
        "too many requests",
        "quota",
        "usage limit",
        "limit reached",
        "limit exceeded",
        "overloaded",
        "resets at",
        // Gemini CLI: "You have exhausted your daily quota on this model." (0.54 번들 실측)
        "exhausted",
        "daily quota",
    ];
    const AUTH: &[&str] = &[
        "not logged in",
        "unauthorized",
        "401",
        "authentication",
        "invalid api key",
        "token expired",
        "log in",
        "login",
        "credential",
    ];
    const NET: &[&str] = &[
        "econnrefused",
        "enotfound",
        "econnreset",
        "etimedout",
        "network",
        "fetch failed",
        "timed out",
        "socket hang up",
        "502",
        "503",
    ];
    if RATE.iter().any(|p| t.contains(p)) {
        FailureKind::RateLimit
    } else if AUTH.iter().any(|p| t.contains(p)) {
        FailureKind::Auth
    } else if NET.iter().any(|p| t.contains(p)) {
        FailureKind::Network
    } else {
        FailureKind::Other
    }
}

/// 오류 원문에 리셋 시각이 epoch(초)로 실려 있으면 뽑는다.
/// 예: Claude의 `usage limit reached|1725436800`.
pub fn extract_reset_epoch(text: &str) -> Option<i64> {
    text.split(|c: char| !c.is_ascii_digit())
        .filter(|s| s.len() == 10)
        .filter_map(|s| s.parse::<i64>().ok())
        .find(|n| (1_600_000_000..2_200_000_000).contains(n))
}

/// 공식 프로토콜에서 읽은 한도 윈도우 하나 (예: Codex app-server RateLimitSnapshot의 primary/secondary)
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitReading {
    pub window: String,
    /// 0.0~1.0
    pub utilization: f64,
    /// epoch seconds, 0 = 미상
    pub resets_at: i64,
}

/// 윈도우 길이(분)를 Claude의 윈도우 이름과 맞춰 상태바 라벨을 공유한다.
pub fn window_name_for_minutes(mins: i64) -> String {
    match mins {
        300 => "five_hour".into(),
        10080 => "seven_day".into(),
        m if m > 0 && m % 1440 == 0 => format!("{}d", m / 1440),
        m if m > 0 && m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

/// 오류 원문의 재시도 지연을 초 단위로 뽑는다.
/// 예: `retry in 32s`, `Retry-After: 45`, `"retryDelay": "17s"`, `retry after 2 minutes`.
pub fn extract_retry_after_secs(text: &str) -> Option<i64> {
    let t = text.to_ascii_lowercase();
    let keys = ["retry-after", "retrydelay", "retry in", "retry after", "try again in"];
    let start = keys.iter().filter_map(|k| t.find(k).map(|i| i + k.len())).min()?;
    let rest = &t[start..];
    let num_start = rest.find(|c: char| c.is_ascii_digit())?;
    let digits: String = rest[num_start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let value: f64 = digits.parse().ok()?;
    let unit = rest[num_start + digits.len()..].trim_start();
    let secs = if unit.starts_with("ms") {
        value / 1000.0
    } else if unit.starts_with('m') {
        value * 60.0
    } else if unit.starts_with('h') {
        value * 3600.0
    } else {
        value
    };
    let secs = secs.ceil() as i64;
    (secs > 0).then_some(secs)
}

/// probe 명령의 해석 결과 (어댑터가 만든다)
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeOutcome {
    /// 설치 확인. 로그인까지 확인됐으면 CliReported, 버전만 확인됐으면 Estimated
    Ready {
        evidence: Evidence,
        version: Option<String>,
    },
    AuthRequired {
        detail: String,
    },
    Unavailable {
        detail: String,
    },
}

/// CLI별 가용성 스냅샷을 관리하는 상태 머신 (PRD 6장 가용성·한도 모니터, 10장 정책).
/// 프로세스 실행이나 시계에 의존하지 않아 단위 테스트가 가능하다 — now는 호출자가 넘긴다.
pub struct AvailabilityMonitor {
    order: Vec<CliId>,
    map: HashMap<CliId, AvailabilitySnapshot>,
}

impl AvailabilityMonitor {
    pub fn new(clis: &[CliId], now: i64) -> Self {
        let map = clis
            .iter()
            .map(|c| (*c, AvailabilitySnapshot::unknown(*c, now)))
            .collect();
        Self {
            order: clis.to_vec(),
            map,
        }
    }

    /// probe·라우팅 대상 = 활성 CLI (순서 유지)
    pub fn clis(&self) -> Vec<CliId> {
        self.order
            .iter()
            .copied()
            .filter(|c| self.map.get(c).map_or(false, |s| s.enabled))
            .collect()
    }

    /// 레지스트리 사용 여부 갱신. 목록에 없는 CLI는 끈다.
    pub fn set_enabled(&mut self, enabled: &[CliId]) {
        for (cli, s) in self.map.iter_mut() {
            s.enabled = enabled.contains(cli);
        }
    }

    /// 저장된 스냅샷 복원(앱 재시작). Claude처럼 실행 스트림에서만 오는 공식 사용률을 잃지 않기 위한 것.
    /// 리셋이 지났거나 리셋 시각을 모르는 윈도우는 버리고, 남은 공식 윈도우로 상태를 다시 유도한다.
    /// 복원 직후 바로 probe하도록 next_check_at은 now로 둔다.
    pub fn import(&mut self, stored: Vec<AvailabilitySnapshot>, now: i64) {
        for mut s in stored {
            let Some(slot) = self.map.get_mut(&s.cli) else {
                continue;
            };
            s.windows.retain(|w| w.resets_at.map_or(false, |t| t > now));
            s.state = if s.state == AvailabilityState::Cooldown && !s.all_windows_reset(now) {
                AvailabilityState::Cooldown
            } else if s.has_official_windows() {
                s.state_from_windows()
            } else {
                AvailabilityState::Unknown
            };
            if s.state == AvailabilityState::Unknown {
                s.evidence = Evidence::Estimated;
            }
            s.next_check_at = Some(now);
            *slot = s;
        }
    }

    /// 라우팅 우선순위 변경에 맞춰 스냅샷 순서를 바꾼다. 목록에 없는 기존 CLI는 뒤에 붙이고 모르는 CLI는 무시한다.
    pub fn set_order(&mut self, order: &[CliId]) {
        let mut next: Vec<CliId> = Vec::new();
        for c in order {
            if self.map.contains_key(c) && !next.contains(c) {
                next.push(*c);
            }
        }
        for c in &self.order {
            if !next.contains(c) {
                next.push(*c);
            }
        }
        self.order = next;
    }

    /// 라우팅 체인 순서대로 스냅샷을 돌려준다 (상태바 순서 = 우선순위, PRD 7장)
    pub fn snapshots(&self) -> Vec<AvailabilitySnapshot> {
        self.order
            .iter()
            .filter_map(|c| self.map.get(c).cloned())
            .collect()
    }

    #[cfg(test)]
    pub fn get(&self, cli: CliId) -> Option<&AvailabilitySnapshot> {
        self.map.get(&cli)
    }

    /// 공식 한도 신호(Claude rate_limit_event 등) 반영. 윈도우별로 갱신하고 상태를 유도한다.
    pub fn apply_rate_limit(
        &mut self,
        cli: CliId,
        window: &str,
        utilization: f64,
        resets_at: i64,
        now: i64,
    ) -> bool {
        let Some(s) = self.map.get_mut(&cli) else {
            return false;
        };
        s.windows
            .retain(|w| w.name != window && w.name != ESTIMATED_WINDOW);
        s.windows.push(RateWindow {
            name: window.to_string(),
            utilization: Some(utilization),
            resets_at: if resets_at > 0 { Some(resets_at) } else { None },
        });
        s.evidence = Evidence::Official;
        s.state = s.state_from_windows();
        s.checked_at = now;
        s.next_check_at = match s.state {
            AvailabilityState::Cooldown => s
                .earliest_pending_reset(now)
                .or(Some(now + DEFAULT_COOLDOWN_SECS)),
            _ => Some(now + PROBE_INTERVAL_SECS),
        };
        if s.state != AvailabilityState::Cooldown {
            s.last_error = None;
        }
        true
    }

    /// probe 결과 반영. 한도 중이라고 알려진 CLI는 probe 성공으로 덮어쓰지 않는다.
    pub fn apply_probe(&mut self, cli: CliId, outcome: ProbeOutcome, now: i64) -> bool {
        let Some(s) = self.map.get_mut(&cli) else {
            return false;
        };
        s.checked_at = now;
        s.next_check_at = Some(now + PROBE_INTERVAL_SECS);
        match outcome {
            ProbeOutcome::Ready { evidence, version } => {
                if version.is_some() {
                    s.version = version;
                }
                if s.state == AvailabilityState::Cooldown && !s.all_windows_reset(now) {
                    // 한도 중(공식이든 추정이든): 설치·로그인 probe 성공은 한도 해제의 근거가 아니다
                    s.next_check_at = s.earliest_pending_reset(now);
                    return true;
                }
                if s.has_official_windows() {
                    s.state = s.state_from_windows();
                    s.evidence = Evidence::Official;
                } else {
                    s.state = AvailabilityState::Available;
                    s.evidence = evidence;
                }
                s.last_error = None;
            }
            ProbeOutcome::AuthRequired { detail } => {
                s.state = AvailabilityState::AuthRequired;
                s.evidence = Evidence::CliReported;
                s.last_error = Some(detail);
            }
            ProbeOutcome::Unavailable { detail } => {
                s.state = AvailabilityState::Unavailable;
                s.evidence = Evidence::Estimated;
                s.last_error = Some(detail);
            }
        }
        true
    }

    /// 실행 중 관측된 실패 원문 반영. 한도·인증·네트워크만 상태를 바꾸고 코드 오류는 건드리지 않는다.
    pub fn apply_failure(&mut self, cli: CliId, text: &str, now: i64) -> Option<FailureKind> {
        let kind = classify_failure(text);
        let Some(s) = self.map.get_mut(&cli) else {
            return None;
        };
        let summary: String = text.chars().take(200).collect();
        match kind {
            FailureKind::RateLimit => {
                let relapse = s
                    .recovered_at
                    .map_or(false, |t| now - t <= RELAPSE_WINDOW_SECS);
                let hinted = extract_reset_epoch(text)
                    .filter(|t| *t > now)
                    .or_else(|| extract_retry_after_secs(text).map(|d| now + d));
                let until = if relapse {
                    now + LONG_COOLDOWN_SECS
                } else if let Some(t) = hinted {
                    t
                } else if let Some(t) = s.earliest_pending_reset(now) {
                    t
                } else {
                    now + DEFAULT_COOLDOWN_SECS
                };
                if !s.has_official_windows() {
                    s.windows = vec![RateWindow {
                        name: ESTIMATED_WINDOW.into(),
                        utilization: None,
                        resets_at: Some(until),
                    }];
                    s.evidence = if hinted.is_some() {
                        Evidence::CliReported
                    } else {
                        Evidence::Estimated
                    };
                } else {
                    s.evidence = Evidence::CliReported;
                }
                s.state = AvailabilityState::Cooldown;
                s.next_check_at = Some(until);
                s.last_error = Some(if relapse {
                    format!("리셋 직후 한도 재발 → 장기 쿨다운: {summary}")
                } else {
                    summary
                });
            }
            FailureKind::Auth => {
                s.state = AvailabilityState::AuthRequired;
                s.evidence = Evidence::CliReported;
                s.last_error = Some(summary);
                s.next_check_at = Some(now + PROBE_INTERVAL_SECS);
            }
            FailureKind::Network => {
                s.state = AvailabilityState::Degraded;
                s.evidence = Evidence::Estimated;
                s.last_error = Some(summary);
                s.next_check_at = Some(now + 60);
            }
            FailureKind::Other => return None,
        }
        s.checked_at = now;
        Some(kind)
    }

    /// 주기 호출. 쿨다운이 끝난 CLI를 복귀시키고, 재검사가 필요한 CLI 목록을 돌려준다.
    pub fn tick(&mut self, now: i64) -> Vec<CliId> {
        let mut due = Vec::new();
        for cli in &self.order {
            let Some(s) = self.map.get_mut(cli) else {
                continue;
            };
            if !s.enabled {
                continue;
            }
            if s.state == AvailabilityState::Cooldown {
                if s.all_windows_reset(now) {
                    // 모든 윈도우 리셋 → 복귀. 공식 확인 전이므로 추정 상태로 표시하고 즉시 probe
                    s.state = AvailabilityState::Available;
                    s.evidence = Evidence::Estimated;
                    s.windows.retain(|w| w.name != ESTIMATED_WINDOW);
                    s.recovered_at = Some(now);
                    s.last_error = None;
                    s.next_check_at = Some(now);
                    due.push(*cli);
                } else if let Some(t) = s.earliest_pending_reset(now) {
                    s.next_check_at = Some(t);
                }
                continue;
            }
            if s.next_check_at.map_or(true, |t| t <= now) {
                s.next_check_at = Some(now + PROBE_INTERVAL_SECS);
                due.push(*cli);
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000;

    fn monitor() -> AvailabilityMonitor {
        AvailabilityMonitor::new(&[CliId::Codex, CliId::Claude, CliId::Gemini], T0)
    }

    #[test]
    fn official_windows_drive_state() {
        let mut m = monitor();
        m.apply_rate_limit(CliId::Claude, "five_hour", 0.5, T0 + 3600, T0);
        let s = m.get(CliId::Claude).unwrap();
        assert_eq!(s.state, AvailabilityState::Available);
        assert_eq!(s.evidence, Evidence::Official);

        m.apply_rate_limit(CliId::Claude, "seven_day", 0.97, T0 + 86_400, T0);
        assert_eq!(m.get(CliId::Claude).unwrap().state, AvailabilityState::Degraded);

        m.apply_rate_limit(CliId::Claude, "five_hour", 1.0, T0 + 3600, T0);
        let s = m.get(CliId::Claude).unwrap();
        assert_eq!(s.state, AvailabilityState::Cooldown);
        assert_eq!(
            s.next_check_at,
            Some(T0 + 3600),
            "쿨다운 재검사는 가장 이른 리셋 시각"
        );
    }

    #[test]
    fn cooldown_recovers_only_when_all_windows_reset() {
        let mut m = monitor();
        m.apply_rate_limit(CliId::Claude, "five_hour", 1.0, T0 + 3600, T0);
        m.apply_rate_limit(CliId::Claude, "seven_day", 1.0, T0 + 7200, T0);
        // 다른 CLI(Unknown)는 주기 재검사 대상으로 나올 수 있으니 Claude만 본다
        assert!(
            !m.tick(T0 + 3601).contains(&CliId::Claude),
            "주간 윈도우가 남아 있으면 복귀하지 않는다"
        );
        assert_eq!(m.get(CliId::Claude).unwrap().state, AvailabilityState::Cooldown);

        let due = m.tick(T0 + 7201);
        assert!(due.contains(&CliId::Claude), "복귀 즉시 재검사 대상: {due:?}");
        let s = m.get(CliId::Claude).unwrap();
        assert_eq!(s.state, AvailabilityState::Available);
        assert_eq!(
            s.evidence,
            Evidence::Estimated,
            "복귀 직후는 공식 확인 전이므로 추정"
        );
        assert_eq!(s.recovered_at, Some(T0 + 7201));
    }

    #[test]
    fn relapse_right_after_recovery_becomes_long_cooldown() {
        let mut m = monitor();
        m.apply_failure(CliId::Codex, "HTTP 429 Too Many Requests", T0);
        let s = m.get(CliId::Codex).unwrap();
        assert_eq!(s.state, AvailabilityState::Cooldown);
        assert_eq!(s.next_check_at, Some(T0 + DEFAULT_COOLDOWN_SECS));
        assert_eq!(s.windows[0].name, ESTIMATED_WINDOW);

        let t1 = T0 + DEFAULT_COOLDOWN_SECS + 1;
        assert!(m.tick(t1).contains(&CliId::Codex));
        assert_eq!(m.get(CliId::Codex).unwrap().state, AvailabilityState::Available);
        m.apply_failure(CliId::Codex, "rate limit exceeded", t1 + 60);
        let s = m.get(CliId::Codex).unwrap();
        assert_eq!(s.next_check_at, Some(t1 + 60 + LONG_COOLDOWN_SECS));
        assert!(s.last_error.as_deref().unwrap().contains("장기 쿨다운"));
    }

    #[test]
    fn failure_with_reset_hint_uses_it() {
        let mut m = monitor();
        let hint = T0 + 5000;
        m.apply_failure(
            CliId::Claude,
            &format!("Claude AI usage limit reached|{hint}"),
            T0,
        );
        let s = m.get(CliId::Claude).unwrap();
        assert_eq!(s.next_check_at, Some(hint));
        assert_eq!(s.evidence, Evidence::CliReported);
    }

    #[test]
    fn code_errors_do_not_change_state() {
        let mut m = monitor();
        m.apply_probe(
            CliId::Gemini,
            ProbeOutcome::Ready {
                evidence: Evidence::Estimated,
                version: Some("0.54.4".into()),
            },
            T0,
        );
        assert_eq!(
            m.apply_failure(CliId::Gemini, "error TS2322: type mismatch", T0),
            None
        );
        assert_eq!(m.get(CliId::Gemini).unwrap().state, AvailabilityState::Available);
    }

    #[test]
    fn probe_success_does_not_override_official_cooldown() {
        let mut m = monitor();
        m.apply_rate_limit(CliId::Claude, "five_hour", 1.0, T0 + 3600, T0);
        m.apply_probe(
            CliId::Claude,
            ProbeOutcome::Ready {
                evidence: Evidence::CliReported,
                version: None,
            },
            T0 + 10,
        );
        assert_eq!(m.get(CliId::Claude).unwrap().state, AvailabilityState::Cooldown);
    }

    #[test]
    fn probe_outcomes_map_to_states() {
        let mut m = monitor();
        m.apply_probe(
            CliId::Codex,
            ProbeOutcome::AuthRequired {
                detail: "Not logged in".into(),
            },
            T0,
        );
        assert_eq!(
            m.get(CliId::Codex).unwrap().state,
            AvailabilityState::AuthRequired
        );
        m.apply_probe(
            CliId::Gemini,
            ProbeOutcome::Unavailable {
                detail: "not found".into(),
            },
            T0,
        );
        assert_eq!(
            m.get(CliId::Gemini).unwrap().state,
            AvailabilityState::Unavailable
        );
        assert_eq!(m.snapshots().len(), 3);
        assert_eq!(m.snapshots()[0].cli, CliId::Codex, "체인 순서 유지");
    }

    #[test]
    fn tick_schedules_periodic_probes() {
        let mut m = monitor();
        assert_eq!(m.tick(T0).len(), 3, "초기 상태는 전부 재검사 대상");
        assert!(m.tick(T0 + 1).is_empty());
        assert_eq!(m.tick(T0 + PROBE_INTERVAL_SECS + 1).len(), 3);
    }

    #[test]
    fn classify_patterns() {
        assert_eq!(
            classify_failure("Error: 429 rate_limit_error"),
            FailureKind::RateLimit
        );
        assert_eq!(
            classify_failure("Claude AI usage limit reached|1725436800"),
            FailureKind::RateLimit
        );
        assert_eq!(
            classify_failure("Not logged in. Run codex login"),
            FailureKind::Auth
        );
        assert_eq!(
            classify_failure("fetch failed: ECONNRESET"),
            FailureKind::Network
        );
        assert_eq!(
            classify_failure("cargo build failed: E0308"),
            FailureKind::Other
        );
        assert_eq!(
            extract_reset_epoch("limit reached|1725436800 x"),
            Some(1_725_436_800)
        );
        assert_eq!(extract_reset_epoch("no epoch here 12345"), None);
        assert_eq!(
            classify_failure("You have exhausted your daily quota on this model."),
            FailureKind::RateLimit
        );
    }

    #[test]
    fn retry_after_is_parsed_and_used() {
        assert_eq!(extract_retry_after_secs("429: retry in 32s"), Some(32));
        assert_eq!(extract_retry_after_secs("Retry-After: 45"), Some(45));
        assert_eq!(extract_retry_after_secs(r#""retryDelay": "17s""#), Some(17));
        assert_eq!(extract_retry_after_secs("please retry after 2 minutes"), Some(120));
        assert_eq!(extract_retry_after_secs("retry in 1500ms"), Some(2));
        assert_eq!(extract_retry_after_secs("no hint"), None);

        let mut m = monitor();
        m.apply_failure(CliId::Gemini, "429 Too Many Requests, retry in 40s", T0);
        let s = m.get(CliId::Gemini).unwrap();
        assert_eq!(s.state, AvailabilityState::Cooldown);
        assert_eq!(s.next_check_at, Some(T0 + 40));
        assert_eq!(s.evidence, Evidence::CliReported);
    }

    #[test]
    fn import_keeps_pending_windows_and_drops_expired() {
        let mut src = monitor();
        src.apply_rate_limit(CliId::Claude, "five_hour", 0.3, T0 + 100, T0);
        src.apply_rate_limit(CliId::Claude, "seven_day", 0.6, T0 + 100_000, T0);

        let mut m = monitor();
        m.import(src.snapshots(), T0 + 200);
        let s = m.get(CliId::Claude).unwrap();
        assert_eq!(s.windows.len(), 1, "5시간 윈도우는 리셋이 지나 폐기");
        assert_eq!(s.windows[0].name, "seven_day");
        assert_eq!(s.state, AvailabilityState::Available);
        assert_eq!(s.evidence, Evidence::Official);
        assert_eq!(s.next_check_at, Some(T0 + 200));

        let mut all_expired = monitor();
        all_expired.import(src.snapshots(), T0 + 200_000);
        let s = all_expired.get(CliId::Claude).unwrap();
        assert_eq!(s.state, AvailabilityState::Unknown);
        assert!(s.windows.is_empty());

        // 아직 진행 중인 쿨다운은 유지
        let mut cd = monitor();
        cd.apply_failure(CliId::Codex, "429", T0);
        let mut m3 = monitor();
        m3.import(cd.snapshots(), T0 + 60);
        assert_eq!(m3.get(CliId::Codex).unwrap().state, AvailabilityState::Cooldown);
    }

    #[test]
    fn disabled_clis_are_skipped() {
        let mut m = monitor();
        m.set_enabled(&[CliId::Claude]);
        assert_eq!(m.clis(), vec![CliId::Claude]);
        assert_eq!(m.snapshots().len(), 3, "스냅샷 목록은 비활성도 포함(enabled 플래그)");
        assert_eq!(m.tick(T0 + 1), vec![CliId::Claude], "틱 재검사 대상은 활성만");
        assert!(!m.snapshots()[0].enabled);

        // 저장·복원을 거쳐도 사용 여부가 유지된다
        let mut m2 = monitor();
        m2.import(m.snapshots(), T0 + 2);
        assert_eq!(m2.clis(), vec![CliId::Claude]);
    }

    #[test]
    fn set_order_reorders_and_keeps_unlisted() {
        let mut m = monitor();
        m.set_order(&[CliId::Gemini, CliId::Opencode, CliId::Claude]);
        let order: Vec<CliId> = m.snapshots().iter().map(|s| s.cli).collect();
        assert_eq!(order, vec![CliId::Gemini, CliId::Claude, CliId::Codex]);
        assert_eq!(m.clis(), order);
    }

    #[test]
    fn window_names_match_claude_convention() {
        assert_eq!(window_name_for_minutes(300), "five_hour");
        assert_eq!(window_name_for_minutes(10080), "seven_day");
        assert_eq!(window_name_for_minutes(1440), "1d");
        assert_eq!(window_name_for_minutes(120), "2h");
        assert_eq!(window_name_for_minutes(45), "45m");
    }
}
