import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import "./App.css";
import type { AvailabilitySnapshot, CliId, Evidence, RunEvent } from "./types";

const CLI_LABEL: Record<CliId, string> = {
  codex: "Codex",
  claude: "Claude",
  gemini: "Gemini",
  opencode: "OpenCode",
};

// 백엔드 get_availability가 오기 전까지의 표시 순서 (라우팅 기본 체인과 동일)
const FALLBACK_ORDER: CliId[] = ["codex", "claude", "gemini"];

const STATE_LABEL: Record<AvailabilitySnapshot["state"], string> = {
  available: "Ready",
  degraded: "임박",
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

const WINDOW_LABEL: Record<string, string> = {
  five_hour: "5시간",
  seven_day: "7일",
  seven_day_overage_included: "7일(추가 사용 포함)",
  estimated: "추정 쿨다운",
};

const STORAGE_DIR = "agentdock.projectDir";
const STORAGE_CLI = "agentdock.cli";
const STORAGE_CHAIN = "agentdock.chain";

const ALL_CLIS: CliId[] = ["codex", "claude", "gemini", "opencode"];

function parseChain(raw: string): CliId[] {
  return raw
    .split(",")
    .map((s) => s.trim())
    .filter((s): s is CliId => (ALL_CLIS as string[]).includes(s));
}

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
      if (event.cancelled) {
        return { ...conv, state: "idle", activeRunId: null, items: [...items, item("system", "중지됨", run_id)] };
      }
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

function clock(epoch: number | null): string {
  if (epoch === null) return "–";
  const d = new Date(epoch * 1000);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

function dateTime(epoch: number | null): string {
  if (epoch === null) return "–";
  const d = new Date(epoch * 1000);
  return `${d.getMonth() + 1}/${d.getDate()} ${clock(epoch)}`;
}

function maxUtil(s: AvailabilitySnapshot): number | null {
  const vals = s.windows.map((w) => w.utilization).filter((v): v is number => v !== null);
  return vals.length ? Math.max(...vals) : null;
}

/** 상태바의 시각 칸: 공식·CLI 제시는 리셋 시각, 쿨다운은 복귀 예정, 추정은 다음 재검사 (PRD 7장) */
function timeLabel(s: AvailabilitySnapshot): string {
  if (s.state === "cooldown") return `${clock(s.next_check_at)} 복귀 예정`;
  const resets = s.windows.map((w) => w.resets_at).filter((v): v is number => v !== null);
  if (s.evidence === "official" && resets.length) return `${clock(Math.min(...resets))} ↻`;
  return s.next_check_at === null ? "재검사 대기" : `${clock(s.next_check_at)} 재검사`;
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
  const [statuses, setStatuses] = useState<AvailabilitySnapshot[]>([]);
  const [recommended, setRecommended] = useState<CliId | null>(null);
  const [detailCli, setDetailCli] = useState<CliId | null>(null);
  const [rechecking, setRechecking] = useState(false);
  const [dragging, setDragging] = useState<CliId | null>(null);
  const [dragOver, setDragOver] = useState<CliId | null>(null);
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

  const cliOrder: CliId[] = statuses.length ? statuses.map((s) => s.cli) : FALLBACK_ORDER;

  function dispatch(convId: number, ev: RunEvent) {
    setConvs((prev) => prev.map((c) => (c.id === convId ? applyEvent(c, ev) : c)));
  }

  // 실행 이벤트 수신. 한도 신호는 백엔드 모니터가 소화하고 availability-changed로 다시 온다.
  useEffect(() => {
    const un = listen<RunEvent>("agent-event", ({ payload }) => {
      const { run_id } = payload;
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

  // 가용성: 시작 시 한 번 읽고(저장된 우선순위가 있으면 먼저 적용), 이후는 모니터가 밀어주는 이벤트로 갱신
  useEffect(() => {
    const stored = parseChain(loadStored(STORAGE_CHAIN, ""));
    const initial = stored.length
      ? invoke<AvailabilitySnapshot[]>("set_routing_chain", { chain: stored })
      : invoke<AvailabilitySnapshot[]>("get_availability");
    initial.then(setStatuses).catch((e) => setError(String(e)));
    const un = listen<AvailabilitySnapshot[]>("availability-changed", ({ payload }) => setStatuses(payload));
    return () => {
      un.then((f) => f());
    };
  }, []);

  useEffect(() => {
    invoke<CliId | null>("pick_cli")
      .then(setRecommended)
      .catch(() => setRecommended(null));
  }, [statuses]);

  useEffect(() => {
    store(STORAGE_CLI, cli);
  }, [cli]);

  const current = convs.find((c) => c.id === selectedId) ?? null;
  const running = current?.state === "running";
  const detail = detailCli ? (statuses.find((s) => s.cli === detailCli) ?? null) : null;

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

  /** 상단 카드 드래그 결과를 라우팅 체인으로 반영한다. 상태바·추천도 백엔드 응답 순서를 따른다. */
  async function applyChain(chain: CliId[]) {
    try {
      const snaps = await invoke<AvailabilitySnapshot[]>("set_routing_chain", { chain });
      setStatuses(snaps);
      store(STORAGE_CHAIN, snaps.map((s) => s.cli).join(","));
    } catch (e) {
      setError(String(e));
    }
  }

  function dropOn(target: CliId) {
    const from = dragging;
    setDragging(null);
    setDragOver(null);
    if (!from || from === target) return;
    const order = cliOrder.filter((c) => c !== from);
    order.splice(cliOrder.indexOf(target), 0, from);
    void applyChain(order);
  }

  async function recheck(target: CliId | null) {
    setRechecking(true);
    try {
      const snaps = await invoke<AvailabilitySnapshot[]>("recheck_availability", { cli: target });
      setStatuses(snaps);
    } catch (e) {
      setError(String(e));
    } finally {
      setRechecking(false);
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
      <header className="cli-row" title="카드를 드래그해 라우팅 우선순위를 바꿉니다">
        {cliOrder.map((id, i) => {
          const s = statuses.find((x) => x.cli === id);
          const state = s?.state ?? "unknown";
          const evidence = s?.evidence ?? "estimated";
          const u = s ? maxUtil(s) : null;
          return (
            <div
              key={id}
              className={`cli-card state-${state}${id === cli ? " current" : ""}${dragging === id ? " dragging" : ""}${dragOver === id && dragging !== id ? " drag-over" : ""}`}
              title={`${i + 1}순위${s?.version ? ` · 버전 ${s.version}` : ""} · 드래그해서 순서 변경`}
              draggable
              onDragStart={(e) => {
                e.dataTransfer.effectAllowed = "move";
                e.dataTransfer.setData("text/plain", id);
                setDragging(id);
              }}
              onDragOver={(e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                if (dragOver !== id) setDragOver(id);
              }}
              onDragLeave={() => setDragOver((v) => (v === id ? null : v))}
              onDrop={(e) => {
                e.preventDefault();
                dropOn(id);
              }}
              onDragEnd={() => {
                setDragging(null);
                setDragOver(null);
              }}
            >
              <span className="prio">{i + 1}</span>
              <span className="dot" />
              <span className="cli-name">{CLI_LABEL[id]}</span>
              <span className="cli-state">{STATE_LABEL[state]}</span>
              {u !== null && <span className="cli-util">{pct(u)}</span>}
              <span className={`evidence evidence-${evidence}`}>{EVIDENCE_BADGE[evidence]}</span>
            </div>
          );
        })}
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
            {cliOrder.map((id) => (
              <option key={id} value={id}>
                {CLI_LABEL[id]}
              </option>
            ))}
          </select>
          <label className="check">
            <input type="checkbox" checked={allowWrites} onChange={(e) => setAllowWrites(e.currentTarget.checked)} />
            파일 쓰기 허용
          </label>
          <span className="routing" title="라우팅 프로필 '코딩 작업' 기준, 지금 가용한 최우선 CLI">
            추천: {recommended ? CLI_LABEL[recommended] : "없음"}
          </span>
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
        {detail && (
          <div className="sb-detail">
            <div className="sb-detail-head">
              <strong>{CLI_LABEL[detail.cli]}</strong>
              <span className={`sb-state state-${detail.state}`}>
                <span className="dot" /> {STATE_LABEL[detail.state]}
              </span>
              <span className={`evidence evidence-${detail.evidence}`}>{EVIDENCE_BADGE[detail.evidence]}</span>
              <span className="sb-spacer" />
              <button className="small" onClick={() => void recheck(detail.cli)} disabled={rechecking}>
                {rechecking ? "재검사 중…" : "재검사"}
              </button>
              <button className="small" onClick={() => setDetailCli(null)}>
                닫기
              </button>
            </div>
            {detail.windows.length ? (
              <table>
                <tbody>
                  {detail.windows.map((w) => (
                    <tr key={w.name}>
                      <td>{WINDOW_LABEL[w.name] ?? w.name}</td>
                      <td className="num">{pct(w.utilization)}</td>
                      <td>{w.resets_at === null ? "리셋 시각 미상" : `${dateTime(w.resets_at)} 리셋`}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            ) : (
              <p className="muted">한도 윈도우 신호 없음 — 사용률은 표시하지 않습니다.</p>
            )}
            <p className="muted">
              라우팅 순위 {cliOrder.indexOf(detail.cli) + 1}/{cliOrder.length} · 버전 {detail.version ?? "?"} · 갱신{" "}
              {dateTime(detail.checked_at)} · 다음 재검사 {dateTime(detail.next_check_at)}
              {detail.recovered_at !== null && ` · 복귀 ${dateTime(detail.recovered_at)}`}
            </p>
            {detail.last_error && <p className="error">마지막 오류: {detail.last_error}</p>}
          </div>
        )}
        {cliOrder.map((id) => {
          const s = statuses.find((x) => x.cli === id);
          if (!s) {
            return (
              <div key={id} className="statusbar-item state-unknown">
                <span className="dot" />
                <span>{CLI_LABEL[id]}</span>
                <span className="sb-util">?</span>
                <span className="sb-reset">확인 중</span>
              </div>
            );
          }
          return (
            <div
              key={id}
              className={`statusbar-item state-${s.state}${id === cli ? " current" : ""}${detailCli === id ? " open" : ""}`}
              title="클릭하면 한도 근거·재검사"
              onClick={() => setDetailCli((v) => (v === id ? null : id))}
            >
              <span className="dot" />
              <span>{CLI_LABEL[id]}</span>
              <span className="sb-state-text">{STATE_LABEL[s.state]}</span>
              <span className="sb-util">{pct(maxUtil(s))}</span>
              <span className="sb-reset">{timeLabel(s)}</span>
              <span className={`evidence evidence-${s.evidence}`}>{EVIDENCE_BADGE[s.evidence]}</span>
            </div>
          );
        })}
      </footer>
    </div>
  );
}

export default App;
