import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { RunEvent } from "./types";
import { reduceCrewEvent, type CrewTask } from "./crewState";

interface OcxModel { id: string; namespaced?: string; label?: string; display_name?: string; disabled?: boolean; }
const read = (key: string) => { try { return localStorage.getItem(key) ?? ""; } catch { return ""; } };
const save = (key: string, value: string) => { try { localStorage.setItem(key, value); } catch { /* storage unavailable */ } };
const labels = { running: "감시 중", starting: "시작 중", approval: "승인 필요", done: "완료", failed: "실패", cancelled: "중지됨" };

export function CrewPanel({ projectDir, onBusy }: { projectDir: string; onBusy: (busy: boolean) => void }) {
  const [draft, setDraft] = useState("");
  const [tasks, setTasks] = useState<CrewTask[]>([]);
  const [models, setModels] = useState<OcxModel[]>([]);
  const [error, setError] = useState("");
  const [ready, setReady] = useState(false);
  const [firstmate, setFirstmate] = useState(() => read("agentdock.captainModel"));
  const [crew, setCrew] = useState(() => read("agentdock.crewModel"));
  const [allowWrites, setAllowWrites] = useState(false);
  const starting = useRef(false);
  const pending = useRef<RunEvent[]>([]);
  const owned = useRef(new Map<number, number>());
  const busy = tasks.some(t => ["starting", "running", "approval"].includes(t.status));
  useEffect(() => { onBusy(busy); }, [busy, onBusy]);
  useEffect(() => {
    let disposed = false;
    const off = listen<RunEvent>("agent-event", ({ payload }) => {
      const id = owned.current.get(payload.run_id);
      if (id === undefined) {
        if (starting.current) pending.current.push(payload);
        return;
      }
      setTasks(rows => rows.map(t => t.id === id ? reduceCrewEvent(t, payload.event) : t));
      if (payload.event.kind === "process_exited") owned.current.delete(payload.run_id);
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
  async function start() {
    if (starting.current || busy || !ready || !draft.trim() || !projectDir || !firstmate || !crew) return;
    starting.current = true;
    pending.current = [];
    const task: CrewTask = { id: Date.now(), title: draft.trim(), projectDir, model: firstmate, crewModel: crew, status: "starting", output: "", activity: [], permissions: [] };
    setTasks(rows => [...rows, task]);
    setError("");
    try {
      const runId = await invoke<number>("start_firstmate", { request: task.title, projectDir: task.projectDir, model: task.model, crewModel: task.crewModel, allowWrites });
      owned.current.set(runId, task.id);
      const buffered = pending.current.filter(e => e.run_id === runId);
      setTasks(rows => rows.map(t => t.id === task.id ? buffered.reduce((value, e) => reduceCrewEvent(value, e.event), { ...t, runId, status: "running" } as CrewTask) : t));
      if (buffered.some(e => e.event.kind === "process_exited")) owned.current.delete(runId);
      setDraft("");
    } catch (e) {
      setTasks(rows => rows.map(t => t.id === task.id ? { ...t, status: "failed", error: String(e) } : t));
    } finally { starting.current = false; pending.current = []; }
  }
  async function decide(task: CrewTask, requestId: string, allow: boolean) {
    try { await invoke("respond_permission", { runId: task.runId, requestId, allow, remember: false }); }
    catch (e) { setError(String(e)); }
  }
  async function stop(task: CrewTask) {
    try { await invoke("cancel_run", { runId: task.runId }); }
    catch (e) { setError(String(e)); }
  }
  function picker(label: string, value: string, update: (v: string) => void, key: string) {
    return <label><span>{label}</span><select value={value} disabled={busy || !models.length} onChange={e => { update(e.target.value); save(key, e.target.value); }}>
      <option value="">모델 선택</option>{models.map(m => <option key={m.namespaced ?? m.id} value={m.namespaced ?? m.id}>{m.display_name ?? m.label ?? m.namespaced ?? m.id}</option>)}
    </select></label>;
  }
  return <section className="crew-panel" aria-label="Firstmate 작업">
    <div className="crew-intro"><div><span className="eyebrow">FIRSTMATE / CREW</span><h2>Firstmate에게 맡기기</h2><p>작업 배분·크루 감시·결과 검토를 Firstmate에게 맡깁니다.</p></div></div>
    <div className="crew-models">{picker("Firstmate 모델", firstmate, setFirstmate, "agentdock.captainModel")}{picker("Crew 선호 모델", crew, setCrew, "agentdock.crewModel")}</div>
    <p className="settings-help">프로젝트: {projectDir || "대화 탭에서 프로젝트 폴더를 선택하세요"}</p>
    <label className="check"><input type="checkbox" checked={allowWrites} disabled={busy} onChange={e => setAllowWrites(e.target.checked)} />파일 변경 허용</label>
    <div className="crew-compose"><input value={draft} disabled={busy} onChange={e => setDraft(e.target.value)} onKeyDown={e => { if (e.key === "Enter" && !e.nativeEvent.isComposing) void start(); }} placeholder="예: 로그인 오류를 조사하고 수정해줘" aria-label="Firstmate에게 할 일" /><button disabled={busy || !ready || !draft.trim() || !projectDir || !firstmate || !crew} onClick={() => void start()}>맡기기</button></div>
    <p className="settings-help">동시에 한 지시를 실행합니다. 크루 생성 여부와 결과는 실행 기록으로 확인하세요. 앱 종료 후 자동 감시 복구는 아직 지원하지 않습니다.</p>
    {error && <p role="alert" className="crew-model-error">{error}</p>}
    <div className="crew-list">{tasks.map(task => <article className="crew-task" key={task.id}>
      <div className="crew-task-top"><span className={`crew-status crew-status-${task.status}`}>{labels[task.status]}</span><span>Firstmate</span></div>
      <h3>{task.title}</h3><div className="crew-task-model">{task.model} → {task.crewModel}</div>
      <small>{task.projectDir}</small>
      {task.output && <pre className="crew-output">{task.output}</pre>}
      {task.activity.length > 0 && <details><summary>실행 기록 ({task.activity.length})</summary><pre className="crew-output">{task.activity.join("\n")}</pre></details>}
      {task.error && <p className="crew-model-error" role="alert">{task.error}</p>}
      {task.permissions.map(p => <div key={p.request_id}><p>{p.description || p.tool}</p><pre className="crew-output">{p.input}</pre><button onClick={() => void decide(task, p.request_id, true)}>허용</button><button onClick={() => void decide(task, p.request_id, false)}>거절</button></div>)}
      {task.runId && ["running", "approval"].includes(task.status) && <button onClick={() => void stop(task)}>중지</button>}
    </article>)}</div>
  </section>;
}
