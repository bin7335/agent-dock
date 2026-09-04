import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import "./App.css";
import type { CliId, CliStatus, Evidence, RunEvent } from "./types";

const CLI_OPTIONS: { id: CliId; label: string }[] = [
  { id: "codex", label: "Codex" },
  { id: "claude", label: "Claude" },
  { id: "gemini", label: "Gemini" },
];

// availability monitor 배선 전까지는 unknown에서 시작하고,
// 실행 중 도착하는 rate_limit 이벤트(Claude 공식 신호)로만 갱신된다.
const initialStatuses: CliStatus[] = CLI_OPTIONS.map(({ id, label }) => ({
  id,
  label,
  state: "unknown",
  evidence: "estimated",
  windows: [],
}));

const STATE_LABEL: Record<CliStatus["state"], string> = {
  available: "Ready",
  degraded: "Degraded",
  cooldown: "Cooldown",
  auth_required: "로그인 필요",
  unavailable: "사용 불가",
  unknown: "Unknown",
};

const EVIDENCE_BADGE: Record<Evidence, string> = {
  official: "공식",
  cli_reported: "CLI 제시",
  estimated: "추정",
};

const STORAGE_DIR = "agentdock.projectDir";
const STORAGE_CLI = "agentdock.cli";

type Role = "user" | "assistant" | "tool" | "system" | "error";

interface ChatItem {
  id: number;
  role: Role;
  text: string;
  runId: number | null;
  ts: string;
}

type ConvState = "idle" | "running" | "failed";

/** 하나의 CLI 세션과 그 위에서 오간 메시지들. 후속 메시지는 sessionId로 이어진다. */
interface Conversation {
  id: number;
  cli: CliId;
  projectDir: string;
  allowWrites: boolean;
  title: string;
  sessionId: string | null;
  state: ConvState;
  activeRunId: number | null;
  items: ChatItem[];
}

let itemSeq = 0;

function now(): string {
  return new Date().toTimeString().slice(0, 8);
}

function item(role: Role, text: string, runId: number | null): ChatItem {
  itemSeq += 1;
  return { id: itemSeq, role, text, runId, ts: now() };
}

function applyEvent(conv: Conversation, ev: RunEvent): Conversation {
  const { run_id, event } = ev;
  const items = conv.items;
  switch (event.kind) {
    case "session_started":
      return conv.sessionId ? conv : { ...conv, sessionId: event.session_id };
    case "message": {
      if (!event.text) return conv;
      const last = items[items.length - 1];
      if (event.delta && last && last.role === "assistant" && last.runId === run_id) {
        return { ...conv, items: [...items.slice(0, -1), { ...last, text: last.text + event.text }] };
      }
      return { ...conv, items: [...items, item("assistant", event.text, run_id)] };
    }
    case "tool_use":
      return { ...conv, items: [...items, item("tool", `${event.tool} ${event.detail.slice(0, 160)}`, run_id)] };
    case "file_change":
      return {
        ...conv,
        items: [...items, item("tool", `${event.ok ? "파일 변경" : "변경 실패"}: ${event.path}`, run_id)],
      };
    case "stderr":
      return { ...conv, items: [...items, item("system", event.text, run_id)] };
    case "completed": {
      if (!event.ok) {
        return {
          ...conv,
          state: "failed",
          items: [...items, item("error", `실패: ${event.summary || "(사유 없음)"}`, run_id)],
        };
      }
      // Claude의 result는 마지막 assistant 텍스트와 같으므로, 본문이 없었을 때만 요약을 표시
      const hasAssistant = items.some((i) => i.role === "assistant" && i.runId === run_id);
      if (event.summary && !hasAssistant) {
        return { ...conv, items: [...items, item("assistant", event.summary, run_id)] };
      }
      return conv;
    }
    case "process_exited": {
      const abnormal = event.code !== null && event.code !== 0;
      const failed = conv.state === "failed" || abnormal;
      return {
        ...conv,
        state: failed ? "failed" : "idle",
        activeRunId: null,
        items:
          abnormal && conv.state !== "failed"
            ? [...items, item("error", `프로세스 비정상 종료 (code ${event.code})`, run_id)]
            : items,
      };
    }
    default:
      return conv;
  }
}

function pct(u: number | null): string {
  return u === null ? "?" : `${Math.round(u * 100)}%`;
}

function resetClock(epoch: number | null): string {
  if (epoch === null) return "재검사 대기";
  const d = new Date(epoch * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")} ↻`;
}

function maxUtil(cli: CliStatus): number | null {
  const vals = cli.windows.map((w) => w.utilization).filter((v): v is number => v !== null);
  return vals.length ? Math.max(...vals) : null;
}

function loadStored(key: string, fallback: string): string {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* 저장 불가 환경은 무시 */
  }
}

function App() {
  const [statuses, setStatuses] = useState<CliStatus[]>(initialStatuses);
  const [convs, setConvs] = useState<Conversation[]>([]);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [projectDir, setProjectDir] = useState<string>(() => loadStored(STORAGE_DIR, ""));
  const [cli, setCli] = useState<CliId>(() => loadStored(STORAGE_CLI, "claude") as CliId);
  const [allowWrites, setAllowWrites] = useState(false);
  const [autoSwitch, setAutoSwitch] = useState(true);
  const [input, setInput] = useState("");
  const [error, setError] = useState("");
  const runMapRef = useRef<Record<number, number>>({});
  const pendingRef = useRef<Record<number, RunEvent[]>>({});
  const endRef = useRef<HTMLDivElement | null>(null);

  function dispatch(convId: number, ev: RunEvent) {
    setConvs((prev) => prev.map((c) => (c.id === convId ? applyEvent(c, ev) : c)));
  }

  useEffect(() => {
    const un = listen<RunEvent>("agent-event", ({ payload }) => {
      const { run_id, cli: evCli, event } = payload;

      if (event.kind === "rate_limit") {
        setStatuses((prev) =>
          prev.map((s) => {
            if (s.id !== evCli) return s;
            const others = s.windows.filter((w) => w.name !== event.window);
            return {
              ...s,
              state: "available",
              evidence: "official",
              windows: [
                ...others,
                { name: event.window, utilization: event.utilization, resetsAt: event.resets_at },
              ],
            };
          }),
        );
      }

      // invoke가 run_id를 돌려주기 전에 도착한 이벤트는 보류했다가 매핑 후 재생한다.
      const convId = runMapRef.current[run_id];
      if (convId === undefined) {
        const queue = pendingRef.current[run_id] ?? [];
        queue.push(payload);
        pendingRef.current[run_id] = queue;
        return;
      }
      dispatch(convId, payload);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  useEffect(() => {
    store(STORAGE_CLI, cli);
  }, [cli]);

  const current = convs.find((c) => c.id === selectedId) ?? null;
  const running = current?.state === "running";

  useEffect(() => {
    endRef.current?.scrollIntoView({ block: "end" });
  }, [current?.items.length, selectedId]);

  async function pickFolder() {
    try {
      const dir = await openDialog({ directory: true, multiple: false, defaultPath: projectDir || undefined });
      if (typeof dir === "string") {
        setProjectDir(dir);
        store(STORAGE_DIR, dir);
      }
    } catch (e) {
      setError(String(e));
    }
  }

  async function send() {
    const text = input.trim();
    if (!text) return;
    if (!projectDir) {
      setError("프로젝트 폴더를 먼저 선택하세요.");
      return;
    }
    setError("");

    let conv = current;
    if (conv && conv.state === "running") return;
    if (!conv) {
      const fresh: Conversation = {
        id: Date.now(),
        cli,
        projectDir,
        allowWrites,
        title: text.slice(0, 30),
        sessionId: null,
        state: "running",
        activeRunId: null,
        items: [],
      };
      conv = fresh;
      setConvs((prev) => [fresh, ...prev]);
      setSelectedId(fresh.id);
    }
    const convId = conv.id;
    setConvs((prev) =>
      prev.map((c) => (c.id === convId ? { ...c, state: "running", items: [...c.items, item("user", text, null)] } : c)),
    );
    setInput("");

    try {
      const args = { cli: conv.cli, request: text, projectDir: conv.projectDir, allowWrites: conv.allowWrites };
      const runId = conv.sessionId
        ? await invoke<number>("continue_job", { ...args, sessionId: conv.sessionId })
        : await invoke<number>("start_job", args);
      runMapRef.current[runId] = convId;
      setConvs((prev) => prev.map((c) => (c.id === convId ? { ...c, activeRunId: runId } : c)));
      const queued = pendingRef.current[runId] ?? [];
      delete pendingRef.current[runId];
      queued.forEach((ev) => dispatch(convId, ev));
    } catch (e) {
      setConvs((prev) =>
        prev.map((c) =>
          c.id === convId ? { ...c, state: "failed", items: [...c.items, item("error", String(e), null)] } : c,
        ),
      );
    }
  }

  async function stop() {
    if (!current?.activeRunId) return;
    try {
      await invoke<boolean>("cancel_run", { runId: current.activeRunId });
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="app">
      <header className="cli-row">
        {statuses.map((s) => (
          <div key={s.id} className={`cli-card state-${s.state}`}>
            <span className="dot" />
            <span className="cli-name">{s.label}</span>
            <span className="cli-state">{STATE_LABEL[s.state]}</span>
            <span className={`evidence evidence-${s.evidence}`}>{EVIDENCE_BADGE[s.evidence]}</span>
          </div>
        ))}
      </header>

      <main className="main">
        <section className="panel queue">
          <div className="panel-head">
            <h2>대화</h2>
            <button className="small" onClick={() => setSelectedId(null)}>
              ＋ 새 대화
            </button>
          </div>
          {convs.length === 0 && <p className="empty">아래에 메시지를 입력하면 새 대화가 시작됩니다.</p>}
          <ul>
            {convs.map((c) => (
              <li key={c.id} className={c.id === selectedId ? "selected" : ""} onClick={() => setSelectedId(c.id)}>
                <span className="job-title">
                  [{c.cli}] {c.title}
                </span>
                <span className={`job-status js-${c.state === "running" ? "running" : c.state === "failed" ? "failed" : "queued"}`}>
                  {c.state === "running" ? "실행 중" : c.state === "failed" ? "실패" : "대기"}
                </span>
              </li>
            ))}
          </ul>
        </section>

        <section className="panel chat">
          {current ? (
            <p className="run-meta">
              [{current.cli}] {current.projectDir}
              {current.sessionId ? ` · 세션 ${current.sessionId.slice(0, 8)}…` : ""}
              {current.allowWrites ? " · 쓰기 허용" : " · 읽기 전용"}
            </p>
          ) : (
            <p className="run-meta">
              새 대화 · CLI {cli} · {projectDir || "폴더 미선택"} · {allowWrites ? "쓰기 허용" : "읽기 전용"}
            </p>
          )}
          <div className="transcript">
            {current?.items.map((it) => (
              <div key={it.id} className={`msg role-${it.role}`}>
                <span className="msg-ts">{it.ts}</span>
                <div className="msg-text">{it.text}</div>
              </div>
            ))}
            {running && (
              <div className="msg role-system">
                <span className="msg-ts">…</span>
                <div className="msg-text">응답 대기 중</div>
              </div>
            )}
            {!current && <p className="empty">메시지를 입력하면 위 설정으로 새 대화가 시작됩니다.</p>}
            <div ref={endRef} />
          </div>
          <div className="composer">
            <textarea
              value={input}
              onChange={(e) => setInput(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void send();
                }
              }}
              placeholder={running ? "응답을 기다리는 중…" : "메시지 입력 (Enter 전송, Shift+Enter 줄바꿈)"}
              rows={3}
              disabled={running}
            />
            <button onClick={() => void send()} disabled={running || !input.trim()}>
              보내기
            </button>
          </div>
          {error && <p className="error">{error}</p>}
        </section>
      </main>

      <div className="toolbar">
        <div className="settings">
          <button className="folder-btn" onClick={() => void pickFolder()} title="프로젝트 폴더 선택 (새 대화에 적용)">
            📁 {projectDir || "프로젝트 폴더 선택"}
          </button>
          <select value={cli} onChange={(e) => setCli(e.currentTarget.value as CliId)} title="새 대화에 쓸 CLI">
            {CLI_OPTIONS.map((o) => (
              <option key={o.id} value={o.id}>
                {o.label}
              </option>
            ))}
          </select>
          <label className="check">
            <input type="checkbox" checked={allowWrites} onChange={(e) => setAllowWrites(e.currentTarget.checked)} />
            파일 쓰기 허용
          </label>
        </div>
        <div className="actions">
          <button onClick={() => void stop()} disabled={!running}>
            중지
          </button>
          <button className={autoSwitch ? "toggle on" : "toggle"} onClick={() => setAutoSwitch((v) => !v)}>
            자동 전환: {autoSwitch ? "켬" : "끔"}
          </button>
        </div>
      </div>

      <footer className="statusbar">
        {statuses.map((s) => {
          const u = maxUtil(s);
          const primary = s.windows[0];
          return (
            <div
              key={s.id}
              className={`statusbar-item state-${s.state}`}
              title={s.windows.map((w) => `${w.name}: ${pct(w.utilization)}`).join(" · ") || "신호 없음"}
            >
              <span className="dot" />
              <span>{s.label}</span>
              <span className="sb-util">{pct(u)}</span>
              <span className="sb-reset">{resetClock(primary?.resetsAt ?? null)}</span>
              <span className={`evidence evidence-${s.evidence}`}>{EVIDENCE_BADGE[s.evidence]}</span>
            </div>
          );
        })}
      </footer>
    </div>
  );
}

export default App;
