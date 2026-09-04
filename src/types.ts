// Rust 쪽 models.rs · availability.rs와 대응하는 프론트 타입.
// 백엔드 배선 시 Tauri 이벤트/커맨드의 직렬화 형태와 일치시킨다.

export type CliId = "codex" | "claude" | "gemini" | "opencode";

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
  name: string; // five_hour | seven_day 등
  utilization: number | null; // 0~1, null = 신호 없음
  resetsAt: number | null; // epoch seconds
}

export interface CliStatus {
  id: CliId;
  label: string;
  state: AvailabilityState;
  evidence: Evidence;
  windows: RateWindow[];
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

export interface LogLine {
  ts: string;
  kind: "message" | "tool_use" | "file_change" | "rate_limit" | "system";
  text: string;
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
  | { kind: "process_exited"; code: number | null };

/** Tauri "agent-event" 페이로드 (Rust RunEvent) */
export interface RunEvent {
  run_id: number;
  cli: CliId;
  event: AgentEvent;
}
