import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type { RunEvent } from "./types";
import { reduceCrewEvent, cleanRoleName, type CrewTask, type EngineeringPhase, type CrewWorkerItem } from "./crewState";

interface OcxModel { id: string; namespaced?: string; label?: string; display_name?: string; disabled?: boolean; }
type CrewMode = "v1" | "default" | "v2";
interface ModeResponse { multiAgentMode: CrewMode; warnings?: string[]; }
const modes: { id: CrewMode; label: string }[] = [
  { id: "v1", label: "v1 · 기본" },
  { id: "default", label: "base · 자동" },
  { id: "v2", label: "v2 · 고급" }
];

const read = (key: string) => { try { return localStorage.getItem(key) ?? ""; } catch { return ""; } };
const save = (key: string, value: string) => { try { localStorage.setItem(key, value); } catch { /* ignore */ } };
const readTasks = (): CrewTask[] => {
  try {
    const raw = localStorage.getItem("agentdock.crewTasks");
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    // 앱이 꺼졌다가 켜진 경우, 실행 중이던 작업은 'cancelled' 또는 이전 완료 상태로 안전하게 복원
    return parsed.map((t: CrewTask) => {
      const isDone = t.status === "done" || t.phase === "complete";
      const planItems = isDone && t.planItems ? t.planItems.map(p => ({ ...p, done: true })) : t.planItems;
      return {
        ...t,
        status: ["starting", "running", "approval"].includes(t.status) ? "cancelled" : t.status,
        permissions: [],
        planItems,
      };
    });
  } catch {
    return [];
  }
};
const saveTasks = (tasks: CrewTask[]) => {
  try {
    // 최근 20개 작업만 영속화하여 스토리지 초과 방지
    const safe = tasks.slice(0, 20).map(t => ({
      ...t,
      output: t.output.slice(-20000), // 최근 20KB 출력만 보존
      activity: t.activity.slice(-50),
      permissions: [],
    }));
    localStorage.setItem("agentdock.crewTasks", JSON.stringify(safe));
  } catch {
    /* ignore */
  }
};
const labels = {
  running: "감시 중",
  starting: "시작 중",
  approval: "승인 필요",
  done: "완료",
  failed: "실패",
  cancelled: "중지됨"
};

const phaseInfo: Record<EngineeringPhase, { num: number; title: string; desc: string }> = {
  explore: { num: 1, title: "탐색 (Search-First)", desc: "저장소 및 기존 라이브러리/도구 탐색" },
  plan: { num: 2, title: "계획 (Planning)", desc: "수용 기준 정의 및 작업 의존성 분할" },
  execute: { num: 3, title: "실행 (TDD Red-Green)", desc: "단주기 테스트 및 크루 위임 구현" },
  review: { num: 4, title: "리뷰 (Dual Review)", desc: "코딩 표준 및 사용자 요구사항 검증" },
  complete: { num: 5, title: "완료 보고", desc: "결과 브리핑 및 산출물 정리" }
};

const quickPrompts = [
  "서브에이전트 크루를 즉시 편성하여 역할을 분담하고 작업을 시작해줘",
  "search-first로 기존 도구 탐색 후 최적 방안 제안해줘",
  "TDD로 테스트를 먼저 작성하고 버그를 수정해줘",
  "작업을 단계별 의존성 그래프로 분할해줘",
  "코드 표준 및 요구사항 충족 여부를 듀얼 리뷰해줘"
];

function getFolderBasename(fullPath: string): string {
  const normalized = fullPath.replace(/[\\/]+$/, "");
  const parts = normalized.split(/[\\/]/);
  return parts[parts.length - 1] || fullPath;
}

const workerLabels: Record<string, { text: string; icon: string }> = {
  running: { text: "일하는 중", icon: "🏃" },
  waiting: { text: "기다리는 중", icon: "⏳" },
  done: { text: "완료됨", icon: "✅" },
  failed: { text: "실패", icon: "❌" },
};

function formatDuration(ms: number): string {
  const totalSec = Math.max(0, Math.floor(ms / 1000));
  const min = Math.floor(totalSec / 60);
  const sec = totalSec % 60;
  if (min === 0) return `${sec}초`;
  return `${min}분 ${sec}초`;
}

function TaskTimerBadge({ task }: { task: CrewTask }) {
  const [now, setNow] = useState(Date.now());
  const isRunning = ["starting", "running", "approval"].includes(task.status);

  useEffect(() => {
    if (!isRunning) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [isRunning, task.id]);

  const start = task.startedAt || task.id;
  // 실행 중(isRunning)이면 endedAt이 혹시 들어있더라도 현재 시간(now) 기준으로 실시간 증가
  const end = isRunning ? now : (task.endedAt || now);
  const elapsed = Math.max(0, end - start);

  if (isRunning) {
    return <span className="crew-timer-badge running">⏱️ {formatDuration(elapsed)} 경과</span>;
  }
  return <span className="crew-timer-badge done">⏱️ {formatDuration(elapsed)} 소요</span>;
}

function cleanMarkdownText(text: string): string {
  return text
    .replace(/\*\*(.+?)\*\*/g, "$1") // **볼드** 제거
    .replace(/`(.+?)`/g, "$1")         // `코드` 백틱 제거
    .replace(/^[0-9]+\.\s*/, "")       // 중복 숫자 제거 (예: 1. 1. 형태 방지)
    .trim();
}

function sanitizeWorker(w: CrewWorkerItem): { name: string; detail: string; hide: boolean } {
  const rawName = (w.name || "").toLowerCase();
  const rawTool = (w.tool || "").toLowerCase();
  if (rawName.includes("wait") || rawTool.includes("wait") || rawName.includes("close") || rawTool.includes("close")) {
    return { name: "", detail: "", hide: true };
  }

  let name = w.name;
  let detail = w.detail;

  try {
    if (detail && detail.startsWith("{")) {
      const raw = JSON.parse(detail);
      const agent = raw.receiver_agents?.[0] || raw.receiverAgents?.[0];
      if (agent?.agent_nickname || agent?.agentNickname) {
        name = agent.agent_nickname || agent.agentNickname;
      }
      if (raw.prompt) {
        const match = raw.prompt.match(/You are the ([^.\n]+?)(?:\s*\.|\n|$)/i);
        if (match) {
          const role = cleanRoleName(match[1].trim());
          name = name.startsWith("spawn") ? role : role + " (" + name + ")";
        }
        const lines = raw.prompt.split("\n").map((l: string) => l.trim()).filter(Boolean);
        const taskLine = lines.find((l: string) => !l.toLowerCase().startsWith("you are the")) || lines[0] || "";
        detail = taskLine.replace(/^You are the.*$/i, "지정 작업 수행 및 검증").slice(0, 80);
      }
    }
  } catch {
    // fallback
  }

  name = cleanRoleName(name);
  if (!name || name === "서브에이전트 크루") name = "작업 서브에이전트";
  if (detail === "ping") detail = "에이전트 연결 및 상태 확인 완료";
  if (!detail || detail.toLowerCase().startsWith("you are the")) {
    detail = "배정된 전문 영역 분석 및 구현";
  }

  return { name, detail, hide: false };
}

function WorkerCardWithPopover({
  worker,
  info,
}: {
  worker: CrewWorkerItem;
  info: { name: string; detail: string };
}) {
  const [showPopover, setShowPopover] = useState(false);

  // 원본 프롬프트 내용 추출
  let fullPrompt = worker.fullPrompt || "";
  try {
    if (worker.detail && worker.detail.startsWith("{")) {
      const raw = JSON.parse(worker.detail);
      if (raw.prompt) fullPrompt = raw.prompt;
    }
  } catch {
    /* ignore */
  }

  return (
    <div
      className={`crew-worker-card ${worker.status}`}
      style={{ position: "relative", cursor: "pointer" }}
      onMouseEnter={() => setShowPopover(true)}
      onMouseLeave={() => setShowPopover(false)}
      onClick={() => setShowPopover(prev => !prev)}
    >
      <div className="worker-header">
        <span className="worker-name">{info.name}</span>
        <span className={`worker-status ${worker.status}`}>
          {(workerLabels[worker.status] || { icon: "🤖", text: worker.status }).icon}{" "}
          {(workerLabels[worker.status] || { icon: "", text: worker.status }).text}
        </span>
      </div>
      <small className="worker-detail" title={info.detail}>
        {info.detail}
      </small>

      {showPopover && (
        <div className="worker-popover" role="tooltip">
          <div className="worker-popover-title">
            <strong>{info.name}</strong>
            <span className={`worker-status ${worker.status}`}>
              {(workerLabels[worker.status] || { icon: "", text: worker.status }).text}
            </span>
          </div>
          <p className="worker-popover-desc">
            {info.detail}
          </p>
          {fullPrompt && (
            <div className="worker-popover-prompt">
              <small>지시 프롬프트 전문:</small>
              <pre>{fullPrompt}</pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export function CrewPanel({
  projectDir,
  onProjectDirChange,
  onBusy
}: {
  projectDir: string;
  onProjectDirChange?: (dir: string) => void;
  onBusy: (busy: boolean) => void;
}) {
  const [draft, setDraft] = useState("");
  const [localDir, setLocalDir] = useState(projectDir);
  const [tasks, setTasks] = useState<CrewTask[]>(readTasks);
  const [models, setModels] = useState<OcxModel[]>([]);
  const [error, setError] = useState("");
  const [ready, setReady] = useState(false);
  const [mode, setMode] = useState<CrewMode | null>(null);
  const [modeSaving, setModeSaving] = useState(false);
  const [modeMessage, setModeMessage] = useState("");
  const modeLock = useRef(false);
  const [firstmate, setFirstmate] = useState(() => read("agentdock.captainModel"));
  const [crew, setCrew] = useState(() => read("agentdock.crewModel"));
  const [allowWrites, setAllowWrites] = useState(false);
  const [followUps, setFollowUps] = useState<Record<number, string>>({});
  const starting = useRef(false);
  const pending = useRef<RunEvent[]>([]);
  const owned = useRef(new Map<number, number>());
  const busy = tasks.some(t => ["starting", "running", "approval"].includes(t.status));

  useEffect(() => {
    setLocalDir(projectDir);
  }, [projectDir]);

  useEffect(() => { onBusy(busy); }, [busy, onBusy]);

  useEffect(() => {
    saveTasks(tasks);
  }, [tasks]);

  useEffect(() => {
    let disposed = false;
    invoke<ModeResponse>("get_crew_mode").then(result => {
      if (!disposed) setMode(modes.some(m => m.id === result.multiAgentMode) ? result.multiAgentMode : null);
    }).catch(e => { if (!disposed) setError(`협업 방식 조회 실패: ${String(e)}`); });

    const off = listen<RunEvent>("agent-event", ({ payload }) => {
      const id = owned.current.get(payload.run_id);
      if (id === undefined) {
        if (starting.current) pending.current.push(payload);
        return;
      }
      setTasks(rows => rows.map(t => t.id === id ? reduceCrewEvent(t, payload.event) : t));
      if (payload.event.kind === "process_exited") owned.current.delete(payload.run_id);
      if (payload.event.kind === "completed" && payload.event.ok) {
        void invoke("send_system_notification", {
          title: "Agent Dock - 작업 완료! ✅",
          body: "Firstmate가 요청한 작업을 성공적으로 완료했습니다."
        }).catch(() => {});
      }
    });

    off.then(() => { if (!disposed) setReady(true); }).catch(e => { if (!disposed) setError(String(e)); });

    invoke<OcxModel[]>("get_ocx_models").then(rows => {
      if (disposed) return;
      const available = rows.filter(m => !m.disabled && (m.namespaced || m.id));
      setModels(available);
      const valid = (id: string) => available.some(m => (m.namespaced ?? m.id) === id);
      const fallback = available[0]?.namespaced ?? available[0]?.id ?? "";
      setFirstmate(current => valid(current) ? current : fallback);
      setCrew(current => valid(current) ? current : fallback);
    }).catch(e => { if (!disposed) setError(String(e)); });

    return () => { disposed = true; void off.then(unlisten => unlisten()).catch(() => {}); };
  }, []);

  async function pickFolder() {
    try {
      const dir = await openDialog({ directory: true, multiple: false, defaultPath: localDir || undefined });
      if (typeof dir === "string") {
        setLocalDir(dir);
        onProjectDirChange?.(dir);
        save("agentdock.projectDir", dir);
      }
    } catch (e) {
      setError(`폴더 선택 실패: ${String(e)}`);
    }
  }

  function handleDirBlur() {
    const trimmed = localDir.trim();
    if (trimmed !== projectDir) {
      onProjectDirChange?.(trimmed);
      save("agentdock.projectDir", trimmed);
    }
  }

  async function changeMode(next: CrewMode) {
    if (busy || starting.current || modeLock.current) return;
    modeLock.current = true;
    setModeSaving(true);
    setModeMessage("");
    try {
      const result = await invoke<ModeResponse>("set_crew_mode", { mode: next });
      if (!modes.some(m => m.id === result.multiAgentMode)) throw new Error("협업 방식 응답을 확인할 수 없습니다");
      setMode(result.multiAgentMode);
      setModeMessage("저장됨 · 다음 작업부터 적용됩니다.");
      setError("");
    } catch (e) {
      setError(`협업 방식 변경 실패: ${String(e)}`);
      try {
        const current = await invoke<ModeResponse>("get_crew_mode");
        setMode(modes.some(m => m.id === current.multiAgentMode) ? current.multiAgentMode : null);
      } catch {
        setMode(null);
      }
    } finally {
      modeLock.current = false;
      setModeSaving(false);
    }
  }

  async function start() {
    const activeDir = localDir.trim() || projectDir.trim();
    if (starting.current || modeLock.current || !mode || !ready || !draft.trim() || !activeDir || !firstmate || !crew) return;
    starting.current = true;
    pending.current = [];
    const task: CrewTask = {
      id: Date.now(),
      startedAt: Date.now(),
      title: draft.trim(),
      projectDir: activeDir,
      model: firstmate,
      crewModel: crew,
      status: "starting",
      output: "",
      activity: [],
      permissions: [],
      phase: "explore",
      workers: [],
      changes: [],
      planItems: []
    };
    setTasks(rows => [task, ...rows]);
    setError("");
    try {
      const runId = await invoke<number>("start_firstmate", {
        request: task.title,
        projectDir: task.projectDir,
        model: task.model,
        crewModel: task.crewModel,
        allowWrites
      });
      owned.current.set(runId, task.id);
      const buffered = pending.current.filter(e => e.run_id === runId);
      setTasks(rows => rows.map(t => t.id === task.id ? buffered.reduce((value, e) => reduceCrewEvent(value, e.event), { ...t, runId, status: "running" } as CrewTask) : t));
      if (buffered.some(e => e.event.kind === "process_exited")) owned.current.delete(runId);
      setDraft("");
    } catch (e) {
      setTasks(rows => rows.map(t => t.id === task.id ? { ...t, status: "failed", error: String(e) } : t));
    } finally {
      starting.current = false;
      pending.current = [];
    }
  }

  async function decide(task: CrewTask, requestId: string, allow: boolean) {
    try { await invoke("respond_permission", { runId: task.runId, requestId, allow, remember: false }); }
    catch (e) { setError(String(e)); }
  }

  async function stop(task: CrewTask) {
    try { await invoke("cancel_run", { runId: task.runId }); }
    catch (e) { setError(String(e)); }
  }

  function removeTask(id: number) {
    setTasks(rows => rows.filter(t => t.id !== id));
  }

    function togglePlanItem(taskId: number, index: number) {
    setTasks(rows => rows.map(t => {
      if (t.id !== taskId || !t.planItems) return t;
      const updated = t.planItems.map((item, idx) =>
        idx === index ? { ...item, done: !item.done } : item
      );
      return { ...t, planItems: updated };
    }));
  }

  function clearDoneTasks() {
    setTasks(rows => rows.filter(t => ["starting", "running", "approval"].includes(t.status)));
  }

  function retryTask(task: CrewTask) {
    setDraft(task.title);
    setLocalDir(task.projectDir);
    setFirstmate(task.model);
    setCrew(task.crewModel);
  }

  async function copyOutput(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      alert("출력 로그가 클립보드에 복사되었습니다.");
    } catch {
      /* fallback */
    }
  }

  async function sendFollowUp(task: CrewTask) {
    const request = (followUps[task.id] ?? "").trim();
    if (starting.current || modeLock.current || !ready || !request || !task.sessionId) return;
    starting.current = true;
    pending.current = [];
    const previous = task;
    setTasks(rows => rows.map(t => t.id === task.id ? {
      ...t,
      status: "starting",
      error: undefined,
      permissions: [],
      output: (t.output + "\n\n▶ 후속 지시: " + request + "\n").slice(-100000)
    } : t));
    setError("");
    try {
      const runId = await invoke<number>("continue_firstmate", {
        sessionId: task.sessionId,
        request,
        projectDir: task.projectDir,
        model: task.model,
        allowWrites
      });
      owned.current.set(runId, task.id);
      const buffered = pending.current.filter(e => e.run_id === runId);
      setTasks(rows => rows.map(t => t.id === task.id ? buffered.reduce((value, e) => reduceCrewEvent(value, e.event), { ...t, runId, status: "running" } as CrewTask) : t));
      if (buffered.some(e => e.event.kind === "process_exited")) owned.current.delete(runId);
      setFollowUps(rows => ({ ...rows, [task.id]: "" }));
    } catch (e) {
      setTasks(rows => rows.map(t => t.id === task.id ? { ...previous, error: String(e) } : t));
    } finally {
      starting.current = false;
      pending.current = [];
    }
  }

  function picker(label: string, value: string, update: (v: string) => void, key: string) {
    return (
      <label>
        <span>{label}</span>
        <select value={value} disabled={busy || !models.length} onChange={e => { update(e.target.value); save(key, e.target.value); }}>
          <option value="">모델 선택</option>
          {models.map(m => (
            <option key={m.namespaced ?? m.id} value={m.namespaced ?? m.id}>
              {m.display_name ?? m.label ?? m.namespaced ?? m.id}
            </option>
          ))}
        </select>
      </label>
    );
  }

  const currentDir = localDir.trim() || projectDir.trim();
  const folderName = currentDir ? getFolderBasename(currentDir) : "";
  const activeTask = tasks.find(t => ["starting", "running", "approval"].includes(t.status)) || tasks[0];
  const activePhase = activeTask?.phase;

  return (
    <section className="crew-panel" aria-label="Firstmate 작업">
      <div className="crew-intro">
        <div>
          <span className="eyebrow">FIRSTMATE ORCHESTRATION</span>
          <h2>Firstmate 감독 허브</h2>
          <p>작업 분할 · 크루(서브에이전트) 감시 · TDD 검증 · 코드 리뷰를 자동 총괄합니다.</p>
        </div>
      </div>

      <div className="crew-config-grid">
        <div className="crew-project-selector">
          <label>
            <span>작업 프로젝트 폴더</span>
            <div className="project-input-row">
              <input
                type="text"
                value={localDir}
                disabled={busy}
                onChange={e => setLocalDir(e.target.value)}
                onBlur={handleDirBlur}
                placeholder="프로젝트 폴더 절대 경로 (예: C:\PROJECT\my-app)"
                title={localDir}
              />
              <button type="button" className="folder-pick-btn" disabled={busy} onClick={() => void pickFolder()}>
                📁 폴더 선택
              </button>
            </div>
          </label>
        </div>

        <div className="crew-models">
          {picker("Firstmate (감독 에이전트)", firstmate, setFirstmate, "agentdock.captainModel")}
          {picker("Crew (작업 서브에이전트)", crew, setCrew, "agentdock.crewModel")}
        </div>
      </div>

      <div className="crew-pipeline-flow" role="region" aria-label="엔지니어링 파이프라인 단계">
        {(Object.keys(phaseInfo) as EngineeringPhase[]).map(key => {
          const info = phaseInfo[key];
          const isCurrent = activePhase === key;
          const isDone = activeTask?.status === "done";
          return (
            <div key={key} className={`pipeline-step ${isCurrent ? "active" : ""} ${isDone && key === "complete" ? "active" : ""}`}>
              <span className="step-num">{info.num}</span>
              <div className="step-body">
                <strong>{info.title}</strong>
                <small>{info.desc}</small>
              </div>
            </div>
          );
        })}
      </div>

      <details className="crew-mode-settings">
        <summary>협업 엔진 방식: {mode === "default" ? "base (자동)" : mode ?? "확인 필요"}</summary>
        <div className="crew-mode-buttons" role="group" aria-label="협업 방식">
          {modes.map(item => (
            <button key={item.id} aria-pressed={mode === item.id} disabled={busy || modeSaving} onClick={() => void changeMode(item.id)}>
              {item.label}
            </button>
          ))}
        </div>
        <p className="settings-help">v1: 복수 벤더 모델 혼용 · base: 모델별 네이티브 · v2: 신규 협업 프로토콜</p>
        <p role="status">{modeSaving ? "저장 중…" : modeMessage}</p>
      </details>

      <div className="crew-compose-card">
        <div className="compose-top">
          <div className="compose-location" title={currentDir}>
            <span className="location-icon">📂</span>
            <span className="location-name">{folderName || "폴더를 선택하세요"}</span>
            {folderName && <span className="location-full">({currentDir})</span>}
          </div>
          <label className="check">
            <input type="checkbox" checked={allowWrites} disabled={busy} onChange={e => setAllowWrites(e.target.checked)} />
            파일 변경(쓰기) 허용
          </label>
        </div>

        <div className="crew-compose">
          <input
            value={draft}
            disabled={starting.current}
            onChange={e => setDraft(e.target.value)}
            onKeyDown={e => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void start(); }}
            placeholder="Firstmate에게 지시할 작업 입력..."
            aria-label="Firstmate에게 할 일"
          />
          <button
            className={`crew-submit-btn ${draft.trim() && currentDir && !starting.current ? "ready" : ""}`}
            disabled={starting.current || modeSaving || !mode || !ready || !draft.trim() || !currentDir || !firstmate || !crew}
            onClick={() => void start()}
          >
            {starting.current ? "시작 중…" : "Firstmate에게 위임"}
          </button>
        </div>

        <div className="quick-prompts">
          <span className="quick-label">추천 지시:</span>
          {quickPrompts.map((p, idx) => (
            <button
              key={idx}
              type="button"
              className="quick-chip"
              disabled={busy}
              onClick={() => setDraft(p)}
            >
              {p}
            </button>
          ))}
        </div>
      </div>

      {error && <p role="alert" className="crew-model-error">{error}</p>}

      {tasks.length === 0 && (
        <div className="crew-empty">
          <div className="crew-empty-mark">⚓</div>
          <strong>Firstmate가 대기 중입니다</strong>
          <p>지시를 입력하면 Firstmate가 작업을 탐색·계획하고 하위 크루를 조율합니다.</p>
        </div>
      )}

      {tasks.length > 0 && tasks.some(t => ["done", "failed", "cancelled"].includes(t.status)) && (
        <div className="crew-tasks-toolbar">
          <button type="button" className="crew-clear-btn" onClick={clearDoneTasks}>
            완료된 작업 기록 정리
          </button>
        </div>
      )}

      <div className="crew-list">
        {tasks.map(task => {
          const pInfo = task.phase ? phaseInfo[task.phase] : null;
          return (
            <article className="crew-task" key={task.id}>
              <div className="crew-task-top">
                <span className={`crew-status crew-status-${task.status}`}>{labels[task.status]}</span>
                {pInfo && <span className="crew-phase-badge">⚡ {pInfo.title}</span>}
                <TaskTimerBadge task={task} />
                <span className="crew-worker">Firstmate Supervisor</span>
                {!["starting", "running", "approval"].includes(task.status) && (
                  <button
                    type="button"
                    className="crew-task-close-btn"
                    onClick={() => removeTask(task.id)}
                    title="작업 기록 삭제"
                    aria-label="작업 기록 삭제"
                  >
                    ✕
                  </button>
                )}
              </div>
              <h3>{task.title}</h3>
              <div className="crew-task-model">감독: {task.model} → 크루: {task.crewModel}</div>
              <small title={task.projectDir}>{getFolderBasename(task.projectDir)} ({task.projectDir})</small>

              {task.planItems && task.planItems.length > 0 && (
                <div className="crew-subtasks">
                  <div className="subtasks-header">
                    <strong>작업 분할 및 진행 체크리스트</strong>
                    <span className="subtasks-progress">{task.planItems.filter(p => p.done).length} / {task.planItems.length} 완료</span>
                  </div>
                  <ul>
                    {task.planItems.map((item, idx) => (
                      <li key={idx} className={item.done ? "done" : ""}>
                        <input
                          type="checkbox"
                          checked={item.done}
                          onChange={() => togglePlanItem(task.id, idx)}
                          title={item.done ? "미완료로 변경" : "완료로 변경"}
                        />
                        <span
                          className="subtask-text"
                          onClick={() => togglePlanItem(task.id, idx)}
                          style={{ cursor: "pointer" }}
                        >
                          {cleanMarkdownText(item.text)}
                        </span>
                      </li>
                    ))}
                  </ul>
                </div>
              )}

              {task.workers && task.workers.length > 0 && (
                <div className="crew-subagents">
                  <strong>🤖 활성화된 서브에이전트 크루 ({task.workers.filter(w => !sanitizeWorker(w).hide).length})</strong>
                  <div className="crew-agent-grid">
                    {task.workers
                      .map(w => ({ ...w, info: sanitizeWorker(w) }))
                      .filter(w => !w.info.hide)
                      .map(w => (
                        <WorkerCardWithPopover key={w.id} worker={w} info={w.info} />
                      ))}
                  </div>
                </div>
              )}

              {task.changes && task.changes.length > 0 && (
                <div className="crew-file-changes">
                  <strong>파일 변경 사항 ({task.changes.length})</strong>
                  <ul>
                    {task.changes.map((c, idx) => (
                      <li key={idx} className={c.ok ? "ok" : "fail"}>
                        {c.ok ? "✓" : "✕"} {c.path}
                      </li>
                    ))}
                  </ul>
                </div>
              )}

              {task.output && <OutputBox output={task.output} onCopy={copyOutput} />}
              {task.activity.length > 0 && (
                <details>
                  <summary>
                    실시간 실행 기록 (누적 {task.totalActivityCount || task.activity.length}회{task.totalActivityCount && task.totalActivityCount > task.activity.length ? `, 최근 ${task.activity.length}개 보관` : ""})
                  </summary>
                  <pre className="crew-output">{task.activity.join("\n")}</pre>
                </details>
              )}
              {task.error && <p className="crew-model-error" role="alert">{task.error}</p>}
              {task.status === "failed" && (
                <button type="button" className="crew-retry-btn" onClick={() => retryTask(task)}>
                  🔄 이 설정으로 재시도
                </button>
              )}
              {task.permissions.map(p => (
                <div key={p.request_id} className="crew-perm-card">
                  <p><strong>승인 요청:</strong> {p.description || p.tool}</p>
                  <pre className="crew-output">{p.input}</pre>
                  <div className="crew-perm-actions">
                    <button onClick={() => void decide(task, p.request_id, true)}>허용</button>
                    <button onClick={() => void decide(task, p.request_id, false)}>거절</button>
                  </div>
                </div>
              ))}
              {task.runId && ["running", "approval"].includes(task.status) && (
                <button className="crew-stop-btn" onClick={() => void stop(task)}>작업 중지</button>
              )}
              <div className="crew-followup">
                {task.sessionId ? (
                  <>
                    <input
                      value={followUps[task.id] ?? ""}
                      disabled={task.status === "starting"}
                      onChange={e => setFollowUps(rows => ({ ...rows, [task.id]: e.target.value }))}
                      onKeyDown={e => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void sendFollowUp(task); }}
                      placeholder={task.status === "running" ? "Firstmate와 조율 중... 지시나 피드백 입력" : "이 작업에 이어서 피드백 및 후속 지시..."}
                      aria-label="Firstmate 피드백 세션"
                    />
                    <button
                      type="button"
                      disabled={busy || !ready || !(followUps[task.id] ?? "").trim()}
                      onClick={() => void sendFollowUp(task)}
                    >
                      전송
                    </button>
                  </>
                ) : (
                  <small>{task.status === "starting" ? "세션 연결 중..." : "세션 ID가 없어 후속 지시가 불가합니다."}</small>
                )}
              </div>
            </article>
          );
        })}
      </div>
    </section>
  );
}
function OutputBox({ output, onCopy }: { output: string; onCopy: (text: string) => void }) {
  const [isExpanded, setIsExpanded] = useState(true);
  const preRef = useRef<HTMLPreElement>(null);

  useEffect(() => {
    // 새 출력이 들어오면 자동으로 아래로 스크롤
    if (isExpanded && preRef.current) {
      preRef.current.scrollTop = preRef.current.scrollHeight;
    }
  }, [output, isExpanded]);

  return (
    <div className="crew-output-box">
      <div className="crew-output-header">
        <div className="crew-output-title-row" onClick={() => setIsExpanded(prev => !prev)} style={{ cursor: "pointer" }}>
          <span className="expand-indicator">{isExpanded ? "▼" : "▶"}</span>
          <span style={{ fontWeight: 600 }}>실행 결과 요약</span>
          <small style={{ color: "var(--muted)", marginLeft: "4px" }}>({isExpanded ? "접기" : "펼치기"})</small>
        </div>
        <button type="button" className="crew-copy-btn" onClick={() => onCopy(output)}>
          📋 복사
        </button>
      </div>
      {isExpanded && (
        <pre ref={preRef} className="crew-output" style={{ maxHeight: "280px", overflowY: "auto" }}>
          {output}
        </pre>
      )}
    </div>
  );
}
