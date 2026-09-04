// Rust 쪽 models.rs · availability.rs · adapters/mod.rs와 1:1로 대응하는 프론트 타입.
// Tauri 이벤트/커맨드의 serde 직렬화 형태(snake_case)를 그대로 따른다.

export type CliId = "codex" | "claude" | "gemini" | "opencode" | "antigravity";

export type AvailabilityState =
  | "available"
  | "degraded"
  | "cooldown"
  | "auth_required"
  | "unavailable"
  | "unknown";

/** 상태 근거 (PRD 5장): 공식 값 / CLI 제시 값 / 추정 */
export type Evidence = "official" | "cli_reported" | "estimated";

export interface RateWindow {
  name: string; // five_hour | seven_day | seven_day_overage_included | estimated
  utilization: number | null; // 0~1, null = 신호 없음
  resets_at: number | null; // epoch seconds
}

/** Rust availability::AvailabilitySnapshot */
export interface AvailabilitySnapshot {
  cli: CliId;
  state: AvailabilityState;
  evidence: Evidence;
  windows: RateWindow[];
  checked_at: number;
  last_error: string | null;
  next_check_at: number | null;
  recovered_at: number | null;
  version: string | null;
  /** 레지스트리 사용 여부. 꺼진 CLI는 상단·상태바·라우팅에서 빠진다 */
  enabled: boolean;
  /** 로그인된 계정 요약 (토큰 없음) */
  account: AccountInfo | null;
}

/** Rust availability::AccountInfo */
export interface AccountInfo {
  label: string;
  plan: string | null;
  method: string | null;
}

/** Rust models::ModelOption — CLI별 모델 선택지 */
export interface ModelOption {
  id: string;
  label: string;
  is_default: boolean;
}

export type JobStatus =
  | "queued"
  | "starting"
  | "running"
  | "waiting_approval"
  | "succeeded"
  | "failed"
  | "cooldown"
  | "handoff_pending"
  | "cancelled"
  | "blocked";

export interface Job {
  id: number;
  title: string;
  projectDir: string;
  profile: string;
  status: JobStatus;
  assignedCli?: CliId;
  allowWrites: boolean;
  unattendedOk: boolean;
}

/** Rust adapters::AgentEvent의 serde 직렬화 형태 (tag = "kind", snake_case) */
export type AgentEvent =
  | { kind: "session_started"; session_id: string }
  | { kind: "message"; text: string; delta: boolean }
  | { kind: "tool_use"; tool: string; detail: string }
  | { kind: "file_change"; path: string; ok: boolean }
  | { kind: "rate_limit"; window: string; utilization: number; resets_at: number }
  | { kind: "completed"; ok: boolean; summary: string }
  | { kind: "stderr"; text: string }
  | { kind: "process_exited"; code: number | null; cancelled: boolean }
  | {
      kind: "permission_request";
      request_id: string;
      tool: string;
      description: string;
      input: string;
      can_remember: boolean;
      suggestions: string;
    }
  | { kind: "permission_resolved"; request_id: string; allowed: boolean; auto: boolean };

/** Tauri "agent-event" 페이로드 (Rust RunEvent) */
export interface RunEvent {
  run_id: number;
  cli: CliId;
  event: AgentEvent;
}
