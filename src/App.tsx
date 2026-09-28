import { useEffect, useRef, useState, type ReactElement } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import "./App.css";
import "./theme.css";
import type { AvailabilitySnapshot, CliId, Evidence, ModelOption, RunEvent } from "./types";
import { useTelemetry } from "./useTelemetry";
import { QuotaFooter, ResourcePanel, UsagePanel } from "./DockPanels";
import { CrewPanel } from "./CrewPanel";
import { ProviderPriority } from "./ProviderPriority";
import { openUrl } from "@tauri-apps/plugin-opener";

const CLI_LABEL: Record<CliId, string> = {
  codex: "Codex",
  claude: "Claude",
  gemini: "Gemini",
  opencode: "OpenCode",
  antigravity: "Antigravity",
};

// 백엔드 get_availability가 오기 전까지의 표시 순서 (라우팅 기본 체인과 동일)
const FALLBACK_ORDER: CliId[] = ["codex", "claude", "antigravity", "gemini", "opencode"];

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
const STORAGE_CHAIN = "agentdock.chain";
const STORAGE_ENABLED = "agentdock.enabled";
const STORAGE_MODEL_PREFIX = "agentdock.model.";
const STORAGE_OPACITY = "agentdock.opacity";

const ALL_CLIS: CliId[] = ["codex", "claude", "gemini", "opencode", "antigravity"];

function parseCliList(raw: string): CliId[] {
  return raw
    .split(",")
    .map((s) => s.trim())
    .filter((s): s is CliId => (ALL_CLIS as string[]).includes(s));
}

type Role = "user" | "assistant" | "tool" | "system" | "error" | "permission";

/** CLI의 도구 승인 요청 (Claude control_request). resolved가 null이면 답을 기다리는 중 */
interface PermInfo {
  requestId: string;
  tool: string;
  description: string;
  input: string;
  canRemember: boolean;
  resolved: { allowed: boolean; auto: boolean } | null;
}

interface ChatItem {
  id: number;
  role: Role;
  text: string;
  runId: number | null;
  ts: string;
  /** 이 항목을 만든 CLI (assistant·tool·system). 사용자 항목은 없음 */
  cli?: CliId;
  /** role === "permission"일 때의 승인 요청 내용 */
  perm?: PermInfo;
}

type ConvState = "idle" | "running" | "failed";

/** CLI 하나의 세션 상태. syncedUpTo = 이 CLI가 이미 알고 있는 대화 항목 수 (그 뒤의 항목은 다음 메시지에 넘겨준다) */
interface CliSession {
  sessionId: string | null;
  syncedUpTo: number;
}

/**
 * 하나의 대화. cli는 지금 메시지를 받을 CLI이며 대화 중 바꿀 수 있다.
 * CLI마다 자기 세션(sessions)을 유지하고, 다른 CLI에서 오간 항목은 handoff 문단으로 넘겨 문맥을 잇는다 (PRD 5장 작업 단위 전환).
 */
interface Conversation {
  id: number;
  cli: CliId;
  projectDir: string;
  allowWrites: boolean;
  title: string;
  sessions: Partial<Record<CliId, CliSession>>;
  state: ConvState;
  activeRunId: number | null;
  items: ChatItem[];
}

let itemSeq = 0;

function now(): string {
  return new Date().toTimeString().slice(0, 8);
}

function item(role: Role, text: string, runId: number | null, cli?: CliId): ChatItem {
  itemSeq += 1;
  return { id: itemSeq, role, text, runId, ts: now(), cli };
}

function sessionOf(conv: Conversation, cli: CliId): CliSession {
  return conv.sessions[cli] ?? { sessionId: null, syncedUpTo: 0 };
}

/** 다른 CLI에서 오간 대화 항목을 새 CLI(또는 돌아온 CLI)에게 넘기는 문단 (PRD 6장 handoff 패킷의 대화판) */
function buildHandoff(conv: Conversation, target: CliId, carried: ChatItem[], resuming: boolean): string {
  const from = Array.from(new Set(carried.map((i) => i.cli).filter((c): c is CliId => !!c && c !== target)))
    .map((c) => CLI_LABEL[c])
    .join(", ");
  const lines = carried.map((i) => {
    if (i.role === "user") return `사용자: ${i.text}`;
    if (i.role === "assistant") return `${i.cli ? CLI_LABEL[i.cli] : "AI"}: ${i.text}`;
    return `(도구) ${i.text}`;
  });
  let body = lines.join("\n\n");
  if (body.length > 16000) body = "(앞부분 생략)\n" + body.slice(-16000);
  const head = resuming
    ? `[Agent Dock] 이 대화는 당신(${CLI_LABEL[target]})의 세션에서 잠시 다른 AI CLI(${from || "다른 CLI"})로 넘어갔다가 돌아왔습니다. 그사이 오간 대화를 먼저 읽고, 마지막의 새 요청에 이어서 작업하세요. 그사이 파일이 바뀌었을 수 있으니 현재 파일 상태를 확인하세요.`
    : `[Agent Dock] 이 대화는 다른 AI CLI(${from || "다른 CLI"})에서 진행되다가 지금부터 당신(${CLI_LABEL[target]})이 이어받습니다. 프로젝트 폴더는 ${conv.projectDir}입니다. 아래 대화 기록을 읽고 마지막의 새 요청에 이어서 작업하세요. 이미 변경된 파일은 다시 만들지 말고 현재 상태를 확인하세요.`;
  return `${head}\n\n--- 그동안의 대화 ---\n${body}\n--- 대화 끝 ---\n\n새 요청:\n`;
}

function applyEvent(conv: Conversation, ev: RunEvent): Conversation {
  const { run_id, event, cli } = ev;
  const items = conv.items;
  switch (event.kind) {
    case "session_started": {
      const sess = sessionOf(conv, cli);
      if (sess.sessionId) return conv;
      return { ...conv, sessions: { ...conv.sessions, [cli]: { ...sess, sessionId: event.session_id } } };
    }
    case "message": {
      if (!event.text) return conv;
      const last = items[items.length - 1];
      if (event.delta && last && last.role === "assistant" && last.runId === run_id) {
        return { ...conv, items: [...items.slice(0, -1), { ...last, text: last.text + event.text }] };
      }
      return { ...conv, items: [...items, item("assistant", event.text, run_id, cli)] };
    }
    case "tool_use":
      return { ...conv, items: [...items, item("tool", `${event.tool} ${event.detail.slice(0, 160)}`, run_id, cli)] };
    case "file_change":
      return {
        ...conv,
        items: [...items, item("tool", `${event.ok ? "파일 변경" : "변경 실패"}: ${event.path}`, run_id, cli)],
      };
    case "stderr":
      return { ...conv, items: [...items, item("system", event.text, run_id, cli)] };
    case "permission_request": {
      const it = item("permission", `승인 요청: ${event.tool}`, run_id, cli);
      it.perm = {
        requestId: event.request_id,
        tool: event.tool,
        description: event.description,
        input: event.input,
        canRemember: event.can_remember,
        resolved: null,
      };
      return { ...conv, items: [...items, it] };
    }
    case "permission_resolved":
      return {
        ...conv,
        items: items.map((i) =>
          i.perm && i.perm.requestId === event.request_id
            ? { ...i, perm: { ...i.perm, resolved: { allowed: event.allowed, auto: event.auto } } }
            : i,
        ),
      };
    case "completed": {
      if (!event.ok) {
        return {
          ...conv,
          state: "failed",
          items: [...items, item("error", `실패: ${event.summary || "(사유 없음)"}`, run_id, cli)],
        };
      }
      // Claude의 result는 마지막 assistant 텍스트와 같으므로, 본문이 없었을 때만 요약을 표시
      const hasAssistant = items.some((i) => i.role === "assistant" && i.runId === run_id);
      if (event.summary && !hasAssistant) {
        return { ...conv, items: [...items, item("assistant", event.summary, run_id, cli)] };
      }
      return conv;
    }
    case "process_exited": {
      let next: Conversation;
      if (event.cancelled) {
        next = { ...conv, state: "idle", activeRunId: null, items: [...items, item("system", "중지됨", run_id, cli)] };
      } else {
        const abnormal = event.code !== null && event.code !== 0;
        const failed = conv.state === "failed" || abnormal;
        next = {
          ...conv,
          state: failed ? "failed" : "idle",
          activeRunId: null,
          items:
            abnormal && conv.state !== "failed"
              ? [...items, item("error", `프로세스 비정상 종료 (code ${event.code})`, run_id, cli)]
              : items,
        };
      }
      // 이 CLI의 세션은 여기까지의 대화를 알고 있다
      const sess = sessionOf(next, cli);
      return { ...next, sessions: { ...next.sessions, [cli]: { ...sess, syncedUpTo: next.items.length } } };
    }
    default:
      return conv;
  }
}

/** 승인 카드에 보여줄 도구 입력 요약: 명령·파일 경로를 앞세우고 전체 JSON은 접어 둔다 */
function describeInput(raw: string): { headline: string; body: string } {
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return { headline: raw.slice(0, 300), body: "" };
  }
  if (!v || typeof v !== "object") return { headline: String(v), body: "" };
  const o = v as Record<string, unknown>;
  const pretty = JSON.stringify(o, null, 2);
  const body = pretty.length > 4000 ? pretty.slice(0, 4000) + " …" : pretty;
  if (typeof o.command === "string") return { headline: `$ ${o.command}`, body };
  if (typeof o.file_path === "string") {
    const extra =
      typeof o.content === "string" ? ` (새 내용 ${o.content.length}자)` : typeof o.new_string === "string" ? " (편집)" : "";
    return { headline: `${o.file_path}${extra}`, body };
  }
  return { headline: "", body };
}

function pendingApprovals(c: Conversation): number {
  return c.items.filter((i) => i.perm && !i.perm.resolved).length;
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

function accountLine(s: AvailabilitySnapshot): string {
  if (!s.account) return "계정 정보 없음";
  const extra = [s.account.plan, s.account.method].filter((v): v is string => !!v);
  return extra.length ? `${s.account.label} (${extra.join(" · ")})` : s.account.label;
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

function loadModelChoices(): Record<string, string> {
  const out: Record<string, string> = {};
  for (const c of ALL_CLIS) out[c] = loadStored(STORAGE_MODEL_PREFIX + c, "");
  return out;
}

function App() {
  const [statuses, setStatuses] = useState<AvailabilitySnapshot[]>([]);
  const [detailCli, setDetailCli] = useState<CliId | null>(null);
  const [rechecking, setRechecking] = useState(false);
  const [tab, setTab] = useState<"overview" | "chat" | "crew" | "settings">("overview");
  const [crewBusy, setCrewBusy] = useState(false);
  const [modelChoice, setModelChoice] = useState<Record<string, string>>(() => loadModelChoices());
  const [modelOptions, setModelOptions] = useState<Record<string, ModelOption[]>>({});
  const [modelLoading, setModelLoading] = useState<Record<string, boolean>>({});
  const [convs, setConvs] = useState<Conversation[]>([]);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [projectDir, setProjectDir] = useState<string>(() => loadStored(STORAGE_DIR, ""));
  const cli: CliId = "codex";
  const [allowWrites, setAllowWrites] = useState(false);
  const [opacity, setOpacity] = useState(() => {
    const value = Number(loadStored(STORAGE_OPACITY, "1"));
    return Number.isFinite(value) ? Math.min(1, Math.max(0.55, value)) : 1;
  });
  const [theme, setTheme] = useState(() => loadStored("agentdock.theme", "system"));
  const [fontScale, setFontScale] = useState(() => loadStored("agentdock.fontScale", "100"));
  const [autoHandoff, setAutoHandoff] = useState(() => loadStored("agentdock.autoHandoff", "true") === "true");
  const [input, setInput] = useState("");
  const [error, setError] = useState("");
  const [chatMenuOpen, setChatMenuOpen] = useState(false);
  const [projectTree, setProjectTree] = useState<{ name: string; isDir: boolean; children?: { name: string; isDir: boolean; children?: any[] }[] }[]>([]);
  const [expandedFolders, setExpandedFolders] = useState<Set<string>>(new Set());
  const [sidebarWidth, setSidebarWidth] = useState(230);
  const [chatWidth, setChatWidth] = useState(390);
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [fileContent, setFileContent] = useState<string | null>(null);
  const [fileHtml, setFileHtml] = useState<string | null>(null);
  const [fileDirty, setFileDirty] = useState(false);
  const [fileSaving, setFileSaving] = useState(false);
  const [editingFile, setEditingFile] = useState(false);
  const runMapRef = useRef<Record<number, number>>({});
  const pendingRef = useRef<Record<number, RunEvent[]>>({});
  const endRef = useRef<HTMLDivElement | null>(null);
  const lastFocusCheckRef = useRef<number>(Date.now());

  // 레지스트리 전체 순서(설정 패널)와 활성 CLI 순서(상단·상태바·라우팅)
  const allOrder: CliId[] = statuses.length ? statuses.map((s) => s.cli) : FALLBACK_ORDER;
  const cliOrder: CliId[] = statuses.length ? statuses.filter((s) => s.enabled).map((s) => s.cli) : FALLBACK_ORDER;

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

  // 가용성: 시작 시 저장된 우선순위·사용 여부를 먼저 적용해 한 번 읽고, 이후는 모니터가 밀어주는 이벤트로 갱신
  useEffect(() => {
    const chain = parseCliList(loadStored(STORAGE_CHAIN, ""));
    const enabled = parseCliList(loadStored(STORAGE_ENABLED, ""));
    (async () => {
      let snaps = chain.length
        ? await invoke<AvailabilitySnapshot[]>("set_routing_chain", { chain })
        : await invoke<AvailabilitySnapshot[]>("get_availability");
      if (enabled.length) snaps = await invoke<AvailabilitySnapshot[]>("set_enabled_clis", { enabled });
      setStatuses(snaps);
    })().catch((e) => setError(String(e)));
    const un = listen<AvailabilitySnapshot[]>("availability-changed", ({ payload }) => setStatuses(payload));
    return () => {
      un.then((f) => f());
    };
  }, []);



  // 창이 포커스를 되찾으면 재검사 — 터미널에서 로그아웃·로그인한 결과가 바로 반영되도록 (30초 이내 반복은 생략)
  useEffect(() => {
    const onFocus = () => {
      const t = Date.now();
      if (t - lastFocusCheckRef.current < 30_000) return;
      lastFocusCheckRef.current = t;
      invoke<AvailabilitySnapshot[]>("recheck_availability", { cli: null })
        .then(setStatuses)
        .catch(() => {});
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, []);

  useEffect(() => {
    void loadModels(cli, false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [cli]);

  const current = convs.find((c) => c.id === selectedId) ?? null;
  const running = current?.state === "running";

  useEffect(() => {
    if (!projectDir) { setProjectTree([]); return; }
    invoke<{ name: string; isDir: boolean; children?: any[] }[]>("list_project_tree", { projectDir })
      .then(rows => { setProjectTree(rows); setExpandedFolders(new Set()); }).catch(() => setProjectTree([]));
  }, [projectDir]);

  function renderTree(rows: { name: string; isDir: boolean; children?: any[] }[], prefix = "", depth = 0): ReactElement[] {
    return rows.flatMap(entry => {
      const key = `${prefix}/${entry.name}`;
      const open = expandedFolders.has(key);
      const line = <button className="sidebar-file" style={{ paddingLeft: `${7 + depth * 12}px` }} key={key} onClick={() => {
        if (entry.isDir) setExpandedFolders(prev => { const next = new Set(prev); if (next.has(key)) next.delete(key); else next.add(key); return next; });
        else if (projectDir) { const relativePath = key.slice(1); setSelectedFile(relativePath); setFileContent(null); setFileHtml(null); setFileDirty(false); setEditingFile(false); invoke<string>("read_project_file", { projectDir, relativePath }).then(setFileContent).catch(e => setFileContent(String(e))); invoke<string>("highlight_project_file", { projectDir, relativePath }).then(setFileHtml).catch(() => {}); }
      }}>
        <span className="tree-chevron">{entry.isDir ? (open ? "⌄" : "›") : ""}</span><span className={entry.isDir ? "tree-folder" : "tree-file"}>{entry.isDir ? "▱" : "·"}</span><span>{entry.name}</span>
      </button>;
      return open && entry.children?.length ? [line, ...renderTree(entry.children, key, depth + 1)] : [line];
    });
  }

  async function saveFile() {
    if (!projectDir || !selectedFile || fileContent == null || !fileDirty) return;
    setFileSaving(true);
    try { await invoke("write_project_file", { projectDir, relativePath: selectedFile, content: fileContent }); setFileDirty(false); invoke<string>("highlight_project_file", { projectDir, relativePath: selectedFile }).then(setFileHtml).catch(() => {}); }
    catch (e) { setError(String(e)); }
    finally { setFileSaving(false); }
  }

  function startSidebarResize(e: React.MouseEvent) {
    e.preventDefault();
    const start = e.clientX; const initial = sidebarWidth;
    const move = (event: MouseEvent) => setSidebarWidth(Math.max(170, Math.min(420, initial + event.clientX - start)));
    const stop = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", stop); };
    window.addEventListener("mousemove", move); window.addEventListener("mouseup", stop);
  }
  function startChatResize(e: React.MouseEvent) {
    e.preventDefault();
    const start = e.clientX; const initial = chatWidth;
    const move = (event: MouseEvent) => setChatWidth(Math.max(300, Math.min(760, initial + start - event.clientX)));
    const stop = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", stop); };
    window.addEventListener("mousemove", move); window.addEventListener("mouseup", stop);
  }
  const awaiting = current ? pendingApprovals(current) > 0 : false;
  const pendingTotal = convs.reduce((n, c) => n + pendingApprovals(c), 0);
  const detail = detailCli ? (statuses.find((s) => s.cli === detailCli) ?? null) : null;

  useEffect(() => {
    endRef.current?.scrollIntoView({ block: "end" });
  }, [current?.items.length, selectedId]);

  async function loadModels(target: CliId, force: boolean) {
    if (!force && modelOptions[target]) return;
    setModelLoading((m) => ({ ...m, [target]: true }));
    try {
      const rows = await invoke<{ id: string; namespaced?: string; label?: string; display_name?: string; disabled?: boolean }[]>("get_ocx_models");
      const list: ModelOption[] = rows.filter(m => !m.disabled && (m.namespaced || m.id)).map(m => ({
        id: m.namespaced || m.id, label: m.label || m.display_name || m.namespaced || m.id, is_default: false,
      }));
      setModelOptions((m) => ({ ...m, [target]: list }));
    } catch (e) {
      setError(`Opencodex 모델 목록: ${String(e)}`);
    } finally {
      setModelLoading((m) => ({ ...m, [target]: false }));
    }
  }

  function chooseModel(target: CliId, id: string) {
    setModelChoice((m) => ({ ...m, [target]: id }));
    store(STORAGE_MODEL_PREFIX + target, id);
  }

  function modelLabel(target: CliId): string {
    const id = modelChoice[target];
    if (!id) return "기본";
    return modelOptions[target]?.find((m) => m.id === id)?.label.split(" — ")[0] ?? id;
  }

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

  async function copyChat(value: string) {
    try { await navigator.clipboard.writeText(value); setChatMenuOpen(false); }
    catch (e) { setError(`복사 실패: ${String(e)}`); }
  }

  function renameCurrent() {
    if (!current) return;
    const next = window.prompt("대화 이름", current.title)?.trim();
    if (!next) return;
    setConvs(prev => prev.map(c => c.id === current.id ? { ...c, title: next.slice(0, 80) } : c));
    setChatMenuOpen(false);
  }

  function archiveCurrent() {
    if (!current || !window.confirm("이 대화를 보관할까요?")) return;
    setConvs(prev => prev.filter(c => c.id !== current.id));
    setSelectedId(null);
    setChatMenuOpen(false);
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

  /** 선택한 모델로 Opencodex에 새 메시지를 보낸다. */
  async function send() {
    const text = input.trim();
    if (!text) return;
    if (!modelOptions[cli]?.some(m => m.id === modelChoice[cli])) {
      setError("사용 가능한 모델을 선택하세요. 목록이 없으면 새로고침해 주세요.");
      return;
    }
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
        sessions: {},
        state: "running",
        activeRunId: null,
        items: [],
      };
      conv = fresh;
      setConvs((prev) => [fresh, ...prev]);
      setSelectedId(fresh.id);
    }
    const convId = conv.id;
    const target = conv.cli;
    const sess = sessionOf(conv, target);
    // 이 CLI가 아직 모르는 항목(다른 CLI에서 오간 대화·파일 변경)만 넘긴다. 시스템·오류 줄은 제외
    const carried = conv.items
      .slice(sess.syncedUpTo)
      .filter((i) => i.role === "user" || i.role === "assistant" || (i.role === "tool" && i.text.startsWith("파일 변경")));
    const request = carried.length ? buildHandoff(conv, target, carried, sess.sessionId !== null) + text : text;

    setConvs((prev) =>
      prev.map((c) => (c.id === convId ? { ...c, state: "running", items: [...c.items, item("user", text, null)] } : c)),
    );
    setInput("");

    try {
      const model = modelChoice[target];
      const runId = await invoke<number>("send_chat", {
        request, projectDir: conv.projectDir, allowWrites: conv.allowWrites,
        model, sessionId: sess.sessionId,
      });
      runMapRef.current[runId] = convId;
      setConvs((prev) => prev.map((c) => (c.id === convId ? { ...c, activeRunId: runId } : c)));
      const queued = pendingRef.current[runId] ?? [];
      delete pendingRef.current[runId];
      queued.forEach((ev) => dispatch(convId, ev));
    } catch (e) {
      setConvs((prev) =>
        prev.map((c) =>
          c.id === convId ? { ...c, state: "failed", items: [...c.items, item("error", String(e), null, target)] } : c,
        ),
      );
    }
  }

  /** 승인 요청에 답한다. 백엔드가 CLI stdin으로 응답을 보내고 permission_resolved 이벤트로 카드를 갱신한다 */
  async function respond(it: ChatItem, allow: boolean, remember: boolean) {
    if (!it.perm || it.runId === null) return;
    try {
      await invoke("respond_permission", { runId: it.runId, requestId: it.perm.requestId, allow, remember });
    } catch (e) {
      setError(String(e));
    }
  }

  function renderPermission(it: ChatItem) {
    const p = it.perm!;
    const { headline, body } = describeInput(p.input);
    return (
      <div className="perm-card">
        <div className="perm-head">
          <strong>승인 요청 · {p.tool}</strong>
          {p.description && <span className="muted">{p.description}</span>}
        </div>
        {headline && <div className="perm-headline">{headline}</div>}
        {body && (
          <details className="perm-body">
            <summary>입력 전체</summary>
            <pre>{body}</pre>
          </details>
        )}
        {p.resolved ? (
          <div className={`perm-result ${p.resolved.allowed ? "ok" : "no"}`}>
            {p.resolved.allowed ? "허용됨" : "거부됨"}
            {p.resolved.auto ? " (10분 무응답 → 자동 거부)" : ""}
          </div>
        ) : (
          <div className="perm-actions">
            <button className="small primary" onClick={() => void respond(it, true, false)}>
              허용
            </button>
            {p.canRemember && (
              <button
                className="small"
                onClick={() => void respond(it, true, true)}
                title="이 세션에서 같은 종류를 다시 묻지 않음"
              >
                세션 동안 허용
              </button>
            )}
            <button className="small danger" onClick={() => void respond(it, false, false)}>
              거부
            </button>
          </div>
        )}
      </div>
    );
  }

  async function stop() {
    if (!current?.activeRunId) return;
    try {
      await invoke<boolean>("cancel_run", { runId: current.activeRunId });
    } catch (e) {
      setError(String(e));
    }
  }

  function renderModelSelect(target: CliId, compact: boolean) {
    const options = modelOptions[target] ?? [];
    const loading = modelLoading[target];
    return (
      <span className="model-pick">
        <select
          value={modelChoice[target] ?? ""}
          onChange={(e) => chooseModel(target, e.currentTarget.value)}
          title="Opencodex에 연결된 모델"
          aria-label="대화 모델"
          disabled={running || loading}
        >
          <option value="" disabled>{loading ? "모델 불러오는 중…" : "모델 선택"}</option>
          {options.map((m) => (
            <option key={m.id} value={m.id}>
              {compact ? m.label.split(" — ")[0] : m.label}
            </option>
          ))}
          {modelChoice[target] && !options.some((m) => m.id === modelChoice[target]) && (
            <option value={modelChoice[target]} disabled>{modelChoice[target]} (사용 가능 여부 확인 필요)</option>
          )}
        </select>
        <button className="small" onClick={() => void loadModels(target, true)} disabled={running || loading} title="모델 목록 다시 불러오기">
          {loading ? "…" : "↻"}
        </button>
      </span>
    );
  }

  const [isExpanded, setIsExpanded] = useState(false);
  const [quotaActive, setQuotaActive] = useState<string | null>(null);
  const [usageRange, setUsageRange] = useState("all");
  const [refresh, setRefresh] = useState(0);
  const telemetry = useTelemetry(usageRange, refresh);
  const online = !!telemetry.health.data && !telemetry.health.error;
  const windowSizes = useRef<Record<string, { width: number; height: number }>>({
    compact: { width: 380, height: 188 }, quota: { width: 420, height: 520 }, overview: { width: 820, height: 680 },
    chat: { width: 1280, height: 820 }, settings: { width: 760, height: 720 }, crew: { width: 900, height: 780 },
  });
  const previousWindowMode = useRef("compact");
  const windowMode = isExpanded ? `${tab}${quotaActive ? ":quota" : ""}` : quotaActive ? "quota" : "compact";

  useEffect(() => {
    const previous = previousWindowMode.current;
    if (previous === windowMode) return;
    if (previous === "compact" || previous.includes(":" ) === false) {
      windowSizes.current[previous] = { width: window.innerWidth, height: window.innerHeight };
    }
    previousWindowMode.current = windowMode;
    const baseKey = windowMode.split(":")[0];
    const base = windowSizes.current[baseKey] ?? windowSizes.current.compact;
    const size = { width: base.width, height: windowMode.endsWith(":quota") ? Math.max(base.height, 520) : base.height };
    let cancelled = false;
    void (async () => {
      const { getCurrentWindow, LogicalSize } = await import("@tauri-apps/api/window");
      if (!cancelled) {
        const window = getCurrentWindow();
        const position = await window.outerPosition();
        await window.setSize(new LogicalSize(size.width, size.height));
        try { await window.setPosition(position); } catch { /* 일부 Windows 환경에서는 위치 복원을 거부할 수 있음 */ }
      }
    })().catch(e => setError(`창 크기 변경 실패: ${String(e)}`));
    return () => { cancelled = true; };
  }, [windowMode]);

  useEffect(() => {
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (quotaActive) setQuotaActive(null); else setIsExpanded(false);
    };
    window.addEventListener("keydown", escape);
    return () => window.removeEventListener("keydown", escape);
  }, [quotaActive]);

  const toggleWidget = () => { setQuotaActive(null); setIsExpanded(v => !v); };
  const openDashboard = () => { void openUrl(telemetry.health.data?.url ?? "http://127.0.0.1:10100").catch(e => setError(String(e))); };
  const closeWidget = async () => {
    if (crewBusy) throw new Error("Firstmate 작업을 중지한 뒤 닫으세요");
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().close();
  };

  const startDrag = async (e: React.MouseEvent) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
    e.preventDefault();
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().startDragging();
    } catch (err) {
      setError(`창 이동 실패: ${String(err)}`);
    }
  };

  return (
    <div className={`widget-container ${isExpanded ? "expanded" : "collapsed"} ${quotaActive ? "quota-open" : ""} theme-${theme}`} style={{ opacity, fontSize: `${fontScale}%` }}>
      <div className="widget-header" onMouseDown={startDrag}>
        <span className="brand-mark" aria-hidden="true">a<span>·</span></span>
        <span className="widget-title">Agent Dock</span>
        <span className={`connection-badge ${online ? "online" : "offline"}`} title={telemetry.health.error ?? `Opencodex ${telemetry.health.data?.version ?? "연결 중"}`}><i />{online ? "LIVE" : telemetry.health.error ? "OFFLINE" : "연결 중"}</span>
        <div className="window-actions">
          <label className="opacity-control" title={`창 투명도 ${Math.round(opacity * 100)}%`} onMouseDown={e => e.stopPropagation()}>
            <span aria-hidden="true">◐</span>
            <input aria-label="창 투명도" type="range" min="0.55" max="1" step="0.05" value={opacity} onChange={e => { const next = Number(e.currentTarget.value); setOpacity(next); store(STORAGE_OPACITY, String(next)); }} />
          </label>
          <button className="icon-button" aria-label="Opencodex 대시보드 열기" title="Opencodex 대시보드" onClick={openDashboard}>↗</button>
          <button className="icon-button toggle-btn" onClick={toggleWidget} aria-label={isExpanded ? "위젯 접기" : "위젯 펼치기"} aria-expanded={isExpanded}>{isExpanded ? "⌃" : "⌄"}</button>
          <button className="icon-button close-button" aria-label="위젯 닫기" title="진행 중인 대화를 중지한 뒤 닫을 수 있습니다" disabled={convs.some(c => c.state === "running")} onClick={() => void closeWidget().catch(e => setError(String(e)))}>×</button>
        </div>
      </div>
      {!isExpanded && <div className="compact-body"><UsagePanel telemetry={telemetry} compact range={usageRange} onRange={setUsageRange} /><ResourcePanel telemetry={telemetry} compact /></div>}
      <div className="firstmate-view" hidden={!isExpanded || tab !== "crew"}><CrewPanel projectDir={projectDir} onBusy={setCrewBusy} /></div>
      {isExpanded && (
        <>
        <nav className="dock-tabs" aria-label="위젯 메뉴">{([['overview', '개요'], ['chat', '대화'], ['settings', '설정']] as const).map(([key, label]) => <button key={key} className={tab === key ? 'active' : ''} aria-current={tab === key ? 'page' : undefined} onClick={() => { setTab(key); setQuotaActive(null); if (key === "chat") setIsExpanded(true); }}>{label}{key === 'chat' && pendingTotal > 0 && <span className="count-badge">{pendingTotal}</span>}</button>)}<button className={tab === "crew" ? "active" : ""} onClick={() => { setTab("crew"); setQuotaActive(null); }}>Crew</button><button className="refresh-button" onClick={() => setRefresh(v => v + 1)} title="연결·사용량·한도 새로고침" aria-label="새로고침">↻</button></nav>
        <div className={`app tab-${tab}`}>
          {tab === "overview" && <><ResourcePanel telemetry={telemetry} compact={false} /><UsagePanel telemetry={telemetry} compact={false} range={usageRange} onRange={setUsageRange} /></>}

      <main className="main">
        <section className="panel queue chat-sidebar" style={{ flexBasis: sidebarWidth, width: sidebarWidth }}>
          <div className="sidebar-title"><span>탐색기</span></div>
          <button className="sidebar-folder" onClick={() => void pickFolder()} title={projectDir || "프로젝트 폴더 선택"}>📁 <strong>{projectDir || "프로젝트 폴더 선택"}</strong></button>
          <div className="sidebar-files" aria-label="프로젝트 파일 탐색기">
            {renderTree(projectTree)}
            {!projectDir && <p className="sidebar-empty">폴더를 선택하세요</p>}
          </div>
        </section>

        <div className="sidebar-resize-handle" onMouseDown={startSidebarResize} title="탐색기 너비 조절" />


        <section className="panel code-preview">
          <div className="code-tab"><span>{selectedFile ? `◇ ${selectedFile.split(/[\\/]/).pop()}${fileDirty ? " ●" : ""}` : "파일 미리보기"}</span><div>{selectedFile && <button className="small" onClick={() => setEditingFile(v => !v)}>{editingFile ? "미리보기" : "편집"}</button>}{editingFile && <button className="small" onClick={() => void saveFile()} disabled={!fileDirty || fileSaving}>{fileSaving ? "저장 중…" : "저장"}</button>}{selectedFile && <button className="icon-button" onClick={() => { setSelectedFile(null); setFileContent(null); setFileHtml(null); }} aria-label="파일 미리보기 닫기">×</button>}</div></div>
          {selectedFile && editingFile ? <textarea className="code-editor" value={fileContent ?? ""} onChange={e => { setFileContent(e.currentTarget.value); setFileDirty(true); }} onKeyDown={e => { if ((e.ctrlKey || e.metaKey) && e.key === "s") { e.preventDefault(); void saveFile(); } }} spellCheck={false} placeholder="파일을 불러오는 중…" /> : selectedFile && fileHtml ? <div className="code-highlight" dangerouslySetInnerHTML={{ __html: fileHtml }} /> : <pre>{selectedFile ? (fileContent ?? "파일을 불러오는 중…") : "탐색기에서 파일을 선택하면 여기에 표시됩니다."}</pre>}
        </section>

        <div className="chat-resize-handle" onMouseDown={startChatResize} title="대화창 너비 조절" />
        <section className="panel chat" style={{ flexBasis: chatWidth, width: chatWidth }}>
          <div className="chat-session-bar">
            <button className="chat-icon" aria-label="최근 대화 불러오기" title="최근 대화" onClick={() => { const latest = convs[0]; if (latest) setSelectedId(latest.id); }}>↶</button>
            <button className="chat-icon" aria-label="새 대화" title="새 대화" onClick={() => setSelectedId(null)}>✎</button>
            <div className="chat-menu-wrap">
              <button className="chat-icon" aria-label="대화 추가 메뉴" aria-expanded={chatMenuOpen} title="추가 메뉴" onClick={() => setChatMenuOpen(v => !v)}>···</button>
              {chatMenuOpen && <div className="chat-menu" role="menu">
                <button onClick={renameCurrent} disabled={!current}>✎ 이름 변경</button><button onClick={archiveCurrent} disabled={!current}>▣ 보관</button><button onClick={() => void copyChat(current?.items.map(i => i.text).join("\n\n") ?? "")} disabled={!current}>↥ 공유</button><hr /><button onClick={() => void copyChat(current?.projectDir ?? projectDir)} disabled={!current && !projectDir}>▣ 작업 중인 디렉터리 복사</button><button onClick={() => void copyChat(`agent-dock://conversation/${current?.id ?? "new"}`)}>▣ 딥링크 복사</button><button onClick={() => void copyChat(current?.items.map(i => `**${i.role}**\n${i.text}`).join("\n\n") ?? "")} disabled={!current}>▣ Markdown으로 복사</button>
              </div>}
            </div>
          </div>
          {current ? (
            <p className="run-meta">
              {current.projectDir}
              {current.allowWrites ? " · 쓰기 허용" : " · 읽기 전용"} · 모델 {modelLabel(current.cli)}
            </p>
          ) : (
            <p className="run-meta">
              새 대화 · {projectDir || "폴더 미선택"} · {allowWrites ? "쓰기 허용" : "읽기 전용"}
            </p>
          )}
          <div className="transcript">
            {current?.items.map((it) => (
              <div key={it.id} className={`msg role-${it.role}`}>
                <span className="msg-ts">{it.ts}</span>
                {it.perm ? renderPermission(it) : <div className="msg-text">{it.text}</div>}
              </div>
            ))}
            {running && (
              <div className="msg role-system">
                <span className="msg-ts">…</span>
                <div className="msg-text">{awaiting ? "승인 대기 중 — 위 요청에 답해 주세요" : "응답 대기 중"}</div>
              </div>
            )}
            {!current && <p className="empty">모델과 프로젝트 폴더를 선택하고 대화를 시작하세요.</p>}
            <div ref={endRef} />
          </div>
          <div className="composer">
            <textarea
              value={input}
              onChange={(e) => setInput(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                  e.preventDefault();
                  void send();
                }
              }}
              placeholder={running ? "응답을 기다리는 중…" : "메시지 입력 (Enter 전송, Shift+Enter 줄바꿈)"}
              rows={3}
              disabled={running}
            />
            <button onClick={() => void send()} disabled={running || !input.trim() || !modelOptions[cli]?.some(m => m.id === modelChoice[cli])}>
              보내기
            </button>
          </div>
          {error && <p className="error">{error}</p>}
        </section>
      </main>

      <div className="toolbar">
        <div className="settings">
          <label className="chat-model-control"><span>모델</span>{renderModelSelect(cli, false)}</label>
          <label className="check">
            <input type="checkbox" checked={current?.allowWrites ?? allowWrites} disabled={!!current} onChange={(e) => setAllowWrites(e.currentTarget.checked)} />
            파일 쓰기 허용
          </label>

        </div>
        <div className="actions">
          <button onClick={() => void stop()} disabled={!running}>
            중지
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
            <p className="muted">계정: {accountLine(detail)}</p>
            <p className="muted">
              선호 순위 {allOrder.indexOf(detail.cli) + 1}/{allOrder.length} · 버전 {detail.version ?? "?"} · 갱신{" "}
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

      {tab === "settings" && (
        <section className="registry-panel">
            <div className="panel-head">
              <h2>연결 및 설정</h2>
              <div className="actions">
                <button className="small" onClick={() => setRefresh(value => value + 1)}>
                  연결 새로고침
                </button>
              </div>
            </div>
            <div className="connection-card"><div><strong>Opencodex</strong><p className="muted">{telemetry.health.data?.url ?? "로컬 서버 연결 대기"}</p><small>{telemetry.health.data?.version ? `v${telemetry.health.data.version}` : "서버를 시작하면 자동 연결됩니다"}</small></div><button onClick={openDashboard}>계정 관리 ↗</button></div>
            <p className="muted settings-help">AI 제공자·계정·로그인은 Opencodex에서 관리합니다. 계정 관리 버튼으로 대시보드를 열 수 있습니다.</p>
            <div className="preference-card"><h3>화면 설정</h3><div className="theme-buttons" role="group" aria-label="테마"><button className={theme === "system" ? "active" : ""} onClick={() => { setTheme("system"); store("agentdock.theme", "system"); }}>시스템</button><button className={theme === "dark" ? "active" : ""} onClick={() => { setTheme("dark"); store("agentdock.theme", "dark"); }}>어두움</button><button className={theme === "light" ? "active" : ""} onClick={() => { setTheme("light"); store("agentdock.theme", "light"); }}>밝음</button></div><label className="font-size-setting"><span>글자 크기</span><div className="theme-buttons" role="group" aria-label="글자 크기"><button className={fontScale === "85" ? "active" : ""} onClick={() => { setFontScale("85"); store("agentdock.fontScale", "85"); }}>작게</button><button className={fontScale === "100" ? "active" : ""} onClick={() => { setFontScale("100"); store("agentdock.fontScale", "100"); }}>기본</button><button className={fontScale === "115" ? "active" : ""} onClick={() => { setFontScale("115"); store("agentdock.fontScale", "115"); }}>크게</button></div></label></div>
            <div className="preference-card handoff-setting"><div><h3>한도 도달 시 자동 전환</h3><p className="muted">현재 제공자의 계정 풀이 모두 소진되면 다음 제공자로 넘깁니다.</p></div><button className={`toggle-switch ${autoHandoff ? "on" : ""}`} aria-pressed={autoHandoff} onClick={() => { const next = !autoHandoff; setAutoHandoff(next); store("agentdock.autoHandoff", String(next)); }}>{autoHandoff ? "켜짐" : "꺼짐"}</button></div>
            {autoHandoff && <ProviderPriority telemetry={telemetry} />}
        </section>
      )}
        </div>
        </>
      )}
      {error && (!isExpanded || tab !== "chat") && <div className="dock-error" role="alert" title={error}>{error}<button className="icon-button" aria-label="오류 닫기" onClick={() => setError("")}>×</button></div>}
      <QuotaFooter telemetry={telemetry} active={quotaActive} onActive={setQuotaActive} compact={!isExpanded} onRefresh={() => setRefresh(v => v + 1)} />
      <div className="resize-grip" title="드래그해서 창 크기 조절" onMouseDown={e => {
        if (e.button !== 0) return;
        e.preventDefault();
        void import("@tauri-apps/api/window").then(({ getCurrentWindow }) => getCurrentWindow().startResizeDragging("SouthEast")).catch(e => setError(`창 크기 조절 실패: ${String(e)}`));
      }} />
    </div>
  );
}

export default App;
