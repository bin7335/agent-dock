import type { AgentEvent } from "./types";
export interface CrewTask {
  id: number; title: string; projectDir: string; model: string; crewModel: string;
  status: "starting" | "running" | "approval" | "done" | "failed" | "cancelled";
  runId?: number; output: string; activity: string[]; error?: string;
  permissions: Extract<AgentEvent, { kind: "permission_request" }>[];
}
export function reduceCrewEvent(task: CrewTask, event: AgentEvent): CrewTask {
  switch (event.kind) {
    case "message": return { ...task, output: (event.delta ? task.output + event.text : task.output + "\n" + event.text).slice(-100000) };
    case "permission_request": return { ...task, status: "approval", permissions: [...task.permissions.filter(p => p.request_id !== event.request_id), event] };
    case "permission_resolved": {
      const permissions = task.permissions.filter(p => p.request_id !== event.request_id);
      return { ...task, permissions, status: task.status === "approval" ? (permissions.length ? "approval" : "running") : task.status };
    }
    case "completed": return { ...task, status: event.ok ? "done" : "failed", permissions: [], error: event.ok ? undefined : event.summary };
    case "process_exited": return { ...task, permissions: [], status: event.cancelled ? "cancelled" : ["done", "failed"].includes(task.status) ? task.status : "failed", error: !event.cancelled && !["done", "failed"].includes(task.status) ? "완료 보고 없이 실행이 종료되었습니다" : task.error };
    case "tool_use": return { ...task, activity: [...task.activity, `${event.tool}: ${event.detail}`].slice(-100) };
    case "stderr": return { ...task, activity: [...task.activity, event.text].slice(-100) };
    case "file_change": return { ...task, activity: [...task.activity, `${event.ok ? "변경" : "변경 실패"}: ${event.path}`].slice(-100) };
    default: return task;
  }
}
