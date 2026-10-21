import type { AgentEvent } from "./types";

export type EngineeringPhase = "explore" | "plan" | "execute" | "review" | "complete";

export interface CrewWorkerItem {
  id: string;
  name: string;
  tool: string;
  detail: string;
  status: "running" | "done" | "failed";
  timestamp: number;
  fullPrompt?: string;
}

export interface FileChangeItem {
  path: string;
  ok: boolean;
}

export interface PlanCheckItem {
  text: string;
  done: boolean;
}

export interface CrewTask {
  id: number;
  title: string;
  projectDir: string;
  model: string;
  crewModel: string;
  status: "starting" | "running" | "approval" | "done" | "failed" | "cancelled";
  runId?: number;
  sessionId?: string;
  startedAt?: number;
  endedAt?: number;
  totalActivityCount?: number;
  output: string;
  activity: string[];
  error?: string;
  permissions: Extract<AgentEvent, { kind: "permission_request" }>[];
  phase?: EngineeringPhase;
  workers?: CrewWorkerItem[];
  changes?: FileChangeItem[];
  planItems?: PlanCheckItem[];
}

const roleKoreanMap: Record<string, string> = {
  "backend security specialist": "백엔드 보안 담당",
  "frontend lifecycle specialist": "프론트엔드 수명주기 담당",
  "backend": "백엔드 담당",
  "frontend": "프론트엔드 담당",
  "security": "보안 검토 담당",
  "test runner": "테스트 담당",
  "core implementer": "핵심 로직 구현 담당",
  "researcher": "자료 분석 담당",
  "explorer": "탐색 담당",
  "implementer": "코드 구현 담당",
  "reviewer": "코드 검토 담당",
  "fixer": "오류 수정 담당",
  "specialist": "전문 담당",
};

export function cleanRoleName(rawRole: string): string {
  let cleaned = rawRole
    .replace(/^spawn_?agent\s*\(?/i, "")
    .replace(/\)$/, "")
    .replace(/subagent\s+for\s+the.*$/i, "")
    .replace(/agent\s+for\s+the.*$/i, "")
    .replace(/\s+subagent$/i, "")
    .replace(/\s+agent$/i, "")
    .trim();
  const lower = cleaned.toLowerCase();
  for (const [key, val] of Object.entries(roleKoreanMap)) {
    if (lower.includes(key)) return val;
  }
  return cleaned || "작업 서브에이전트";
}

function detectPhase(text: string, current?: EngineeringPhase): EngineeringPhase {
  const lower = text.toLowerCase();
  if (lower.includes("search-first") || lower.includes("탐색") || lower.includes("explore")) return "explore";
  if (lower.includes("수용 기준") || lower.includes("계획") || lower.includes("plan")) return "plan";
  if (lower.includes("tdd") || lower.includes("red-green") || lower.includes("구현") || lower.includes("execute")) return "execute";
  if (lower.includes("code-review") || lower.includes("리뷰") || lower.includes("review")) return "review";
  if (lower.includes("완료") || lower.includes("complete")) return "complete";
  return current ?? "explore";
}

function parsePlanChecklist(text: string): PlanCheckItem[] {
  const lines = text.split("\n");
  const items: PlanCheckItem[] = [];
  for (const line of lines) {
    const match = line.match(/^\s*-\s*\[([ xX])\]\s*(.+)$/);
    if (match) {
      items.push({
        done: match[1].toLowerCase() === "x",
        text: match[2].trim(),
      });
    }
  }
  return items;
}

export function reduceCrewEvent(task: CrewTask, event: AgentEvent): CrewTask {
  const currentWorkers = task.workers ?? [];
  const currentChanges = task.changes ?? [];
  const currentPlan = task.planItems ?? [];

  switch (event.kind) {
    case "session_started": return { ...task, sessionId: event.session_id };
    case "message": {
      const phase = detectPhase(event.text, task.phase);
      const parsedPlan = parsePlanChecklist(event.text);
      let nextPlan = currentPlan;
      if (parsedPlan.length > 0) {
        nextPlan = parsedPlan.map((item, idx) => {
          const prev = currentPlan[idx];
          if (prev && prev.text === item.text && prev.done && !item.done) {
            return { ...item, done: true };
          }
          return item;
        });
      }
      return {
        ...task,
        output: event.delta ? task.output + event.text : event.text,
        phase,
        planItems: nextPlan,
        workers: currentWorkers,
        changes: currentChanges,
      };
    }
    case "permission_request":
      return {
        ...task,
        status: "approval",
        permissions: [...task.permissions.filter(p => p.request_id !== event.request_id), event],
      };
    case "permission_resolved": {
      const nextPerms = task.permissions.filter(p => p.request_id !== event.request_id);
      return {
        ...task,
        status: nextPerms.length > 0 ? "approval" : task.status === "approval" ? "running" : task.status,
        permissions: nextPerms,
      };
    }
    case "completed": {
      const nextStatus = event.ok ? "done" : "failed";
      const finalizedWorkers = currentWorkers.map(w => w.status === "running" ? { ...w, status: "done" as const } : w);
      const finalizedPlan = event.ok ? currentPlan.map(p => ({ ...p, done: true })) : currentPlan;
      return {
        ...task,
        status: nextStatus,
        endedAt: task.endedAt || Date.now(),
        error: event.ok ? undefined : (event.summary || task.error),
        phase: event.ok ? "complete" : task.phase,
        workers: finalizedWorkers,
        planItems: finalizedPlan,
      };
    }
    case "process_exited": {
      const endedAt = task.endedAt || Date.now();
      const finalizedWorkers = currentWorkers.map(w => w.status === "running" ? { ...w, status: "done" as const } : w);
      const isDone = task.status === "done";
      return {
        ...task,
        status: event.cancelled ? "cancelled" : (isDone ? "done" : "failed"),
        endedAt,
        error: !event.cancelled && !["done", "failed"].includes(task.status) ? "process exited unexpectedly" : task.error,
        phase: isDone ? "complete" : task.phase,
        workers: finalizedWorkers,
        planItems: isDone ? currentPlan.map(p => ({ ...p, done: true })) : currentPlan,
      };
    }
    case "tool_use": {
      let nextWorkers = [...currentWorkers];

      if (event.tool.startsWith("crew_done/close") || event.tool.startsWith("crew/close")) {
        try {
          const parsed = JSON.parse(event.detail);
          const targetId = parsed.id || parsed.receiver_agents?.[0] || parsed.receiverAgents?.[0];
          nextWorkers = nextWorkers.map(w => w.id === targetId || w.name.includes(targetId) ? { ...w, status: "done" as const } : w);
        } catch {
          // fallback
        }
        return { ...task, workers: nextWorkers };
      }

      if (event.tool.startsWith("crew_done/")) {
        try {
          const parsed = JSON.parse(event.detail);
          if (parsed.status === "failed") {
            const targetId = parsed.id;
            nextWorkers = nextWorkers.map(w => w.id === targetId ? { ...w, status: "failed" as const } : w);
          } else if (parsed.status === "done") {
            // 명시적 완료 이벤트인 경우만 전환
            const targetId = parsed.id;
            nextWorkers = nextWorkers.map(w => w.id === targetId ? { ...w, status: "done" as const } : w);
          }
        } catch {
          // fallback
        }
        return { ...task, workers: nextWorkers };
      }

      if (event.tool.startsWith("crew/")) {
        let workerName = event.tool.replace(/^crew\//, "");
        let workerDetail = "";
        let fullPrompt = "";
        let nickname = "";
        let matchedRole = "";
        let workerId = "w-" + Date.now() + "-" + currentWorkers.length;

        try {
          const raw = JSON.parse(event.detail);
          if (raw.id) workerId = raw.id;
          
          // 지원하는 닉네임 / 식별자 필드 확장 (Codex native, OpenCodex, collabAgent)
          const agent = raw.receiver_agents?.[0] || raw.receiverAgents?.[0];
          nickname = raw.nickname || raw.agent_nickname || raw.agentNickname || raw.name || agent?.agent_nickname || agent?.agentNickname || "";
          
          // 지원하는 프롬프트 필드 확장 (prompt, message, input, items)
          let extractedPrompt = raw.prompt || raw.message || raw.input || "";
          if (!extractedPrompt && Array.isArray(raw.items)) {
            extractedPrompt = raw.items.map((it: any) => it?.text || it?.content || "").filter(Boolean).join("\n");
          }
          if (typeof extractedPrompt === "object") {
            try { extractedPrompt = JSON.stringify(extractedPrompt); } catch { extractedPrompt = ""; }
          }

          if (extractedPrompt) {
            fullPrompt = extractedPrompt;
            const match = extractedPrompt.match(/You are the ([^.\n]+)/i) || extractedPrompt.match(/(?:역할|Role)\s*[:=]\s*([^.\n]+)/i);
            if (match) {
              matchedRole = match[1].trim();
            }
            const lines = extractedPrompt.split("\n").map((l: string) => l.trim()).filter(Boolean);
            const taskLine = lines.find((l: string) => !l.toLowerCase().startsWith("you are the") && !l.startsWith("Role:")) || lines[0] || "";
            workerDetail = taskLine.replace(/^You are the.*$/i, "지정된 서브태스크 분석 및 구현").slice(0, 100);
          }
        } catch {
          workerDetail = event.detail.slice(0, 80);
        }

        const roleCleaned = cleanRoleName(matchedRole || workerName);
        if (nickname) {
          workerName = roleCleaned !== "작업 서브에이전트" ? `${roleCleaned} (${nickname})` : nickname;
        } else {
          workerName = roleCleaned;
        }

        if (!workerDetail || workerDetail.toLowerCase().startsWith("you are the")) {
          workerDetail = "배정된 전문 영역 분석 및 구현";
        }

        const existingIdx = nextWorkers.findIndex(w => w.id === workerId);
        if (existingIdx >= 0) {
          nextWorkers[existingIdx] = {
            ...nextWorkers[existingIdx],
            name: workerName,
            detail: workerDetail || nextWorkers[existingIdx].detail,
            status: "running",
            fullPrompt: fullPrompt || nextWorkers[existingIdx].fullPrompt,
          };
        } else {
          nextWorkers.push({
            id: workerId,
            name: workerName,
            tool: event.tool,
            detail: workerDetail,
            status: "running",
            timestamp: Date.now(),
            fullPrompt,
          });
        }
      }

      return {
        ...task,
        totalActivityCount: (task.totalActivityCount || task.activity.length) + 1,
        activity: [...task.activity, event.tool + ": " + event.detail].slice(-300),
        workers: nextWorkers,
      };
    }
    case "stderr":
      return {
        ...task,
        totalActivityCount: (task.totalActivityCount || task.activity.length) + 1,
        activity: [...task.activity, event.text].slice(-300),
      };
    case "file_change":
      return {
        ...task,
        totalActivityCount: (task.totalActivityCount || task.activity.length) + 1,
        activity: [...task.activity, (event.ok ? "change: " : "change failed: ") + event.path].slice(-300),
        changes: [...currentChanges, { path: event.path, ok: event.ok }],
      };
    default:
      return task;
  }
}
