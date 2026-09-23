import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type CrewStatus = "queued" | "running" | "approval" | "done";
interface CrewTask { id: number; title: string; worker: string; status: CrewStatus; worktree: string; }
const STATUS_LABEL: Record<CrewStatus, string> = { queued: "대기", running: "작업 중", approval: "승인 필요", done: "완료" };
interface OcxModel { id: string; namespaced?: string; label?: string; display_name?: string; disabled?: boolean; provider?: string; }
const readSetting = (key: string, fallback: string) => { try { return localStorage.getItem(key) ?? fallback; } catch { return fallback; } };
const saveSetting = (key: string, value: string) => { try { localStorage.setItem(key, value); } catch { /* unavailable */ } };

export function CrewPanel({ projectDir }: { projectDir: string }) {
  const [draft, setDraft] = useState(""); const [tasks, setTasks] = useState<CrewTask[]>([]);
  const [models, setModels] = useState<OcxModel[]>([]); const [modelError, setModelError] = useState("");
  const [captainModel, setCaptainModel] = useState(() => readSetting("agentdock.captainModel", ""));
  const [crewModel, setCrewModel] = useState(() => readSetting("agentdock.crewModel", "")); const [taskModel, setTaskModel] = useState("");
  const modelLabel = (id: string) => models.find((model) => (model.namespaced ?? model.id) === id)?.display_name ?? models.find((model) => (model.namespaced ?? model.id) === id)?.label ?? id;
  useEffect(() => {
    invoke<OcxModel[]>("get_ocx_models").then((rows) => {
      const available = rows.filter((model) => !model.disabled && (model.namespaced || model.id));
      setModels(available);
      if (available.length) {
        const first = available[0].namespaced ?? available[0].id;
        setCaptainModel((current) => current || first); setCrewModel((current) => current || first);
      }
    }).catch((error) => setModelError(String(error)));
  }, []);
  const modelOptions = models.map((model) => ({ id: model.namespaced ?? model.id, label: model.display_name ?? model.label ?? model.namespaced ?? model.id }));
  const addTask = () => { const title = draft.trim(); if (!title) return; const id = Date.now(); setTasks((current) => [...current, { id, title, worker: "대기 중", status: "queued", worktree: `worktrees/task-${id}` }]); setDraft(""); };
  const startTask = (id: number) => setTasks((current) => current.map((task) => task.id === id ? { ...task, worker: "Codex worker", status: "running" } : task));
  return <section className="crew-panel" aria-label="Crew 작업">
    <div className="crew-intro"><div><span className="eyebrow">CAPTAIN / CREW</span><h2>작업을 나눠 맡깁니다</h2><p>Captain 대화에서 정한 일을 격리된 worktree의 worker에게 배정합니다.</p></div><span className="crew-count">{tasks.length}개 작업</span></div>
    <div className="crew-compose"><input value={draft} onChange={(e) => setDraft(e.currentTarget.value)} onKeyDown={(e) => { if (e.key === "Enter") addTask(); }} placeholder="예: 로그인 실패 케이스를 조사해줘" aria-label="새 Crew 작업" /><button onClick={addTask} disabled={!draft.trim()}>작업 추가</button></div>
    <div className="crew-models"><label><span>Captain 모델</span><select value={captainModel} onChange={(e) => { setCaptainModel(e.currentTarget.value); saveSetting("agentdock.captainModel", e.currentTarget.value); }} disabled={!modelOptions.length}><option value="">{modelError ? "Opencodex 연결 실패" : "모델 목록 불러오는 중"}</option>{modelOptions.map((model) => <option key={model.id} value={model.id}>{model.label}</option>)}</select></label><label><span>Crew 기본 모델</span><select value={crewModel} onChange={(e) => { setCrewModel(e.currentTarget.value); saveSetting("agentdock.crewModel", e.currentTarget.value); }} disabled={!modelOptions.length}><option value="">{modelError ? "Opencodex 연결 실패" : "모델 목록 불러오는 중"}</option>{modelOptions.map((model) => <option key={model.id} value={model.id}>{model.label}</option>)}</select></label><label><span>이번 작업 override</span><select value={taskModel} onChange={(e) => setTaskModel(e.currentTarget.value)} disabled={!modelOptions.length}><option value="">Crew 기본값 ({crewModel ? modelLabel(crewModel) : "미지정"})</option>{modelOptions.map((model) => <option key={model.id} value={model.id}>{model.label}</option>)}</select></label></div>
    {modelError && <p className="crew-model-error">Opencodex 모델 목록을 불러오지 못했습니다: {modelError}</p>}
    {tasks.length === 0 ? <div className="crew-empty"><span className="crew-empty-mark">+</span><strong>아직 Crew 작업이 없습니다</strong><p>작업을 추가하면 worker와 worktree가 여기에 표시됩니다.</p></div> : <div className="crew-list">{tasks.map((task) => <article className="crew-task" key={task.id}><div className="crew-task-top"><span className={`crew-status crew-status-${task.status}`}><i />{STATUS_LABEL[task.status]}</span><span className="crew-worker">{task.worker}</span></div><h3>{task.title}</h3><div className="crew-task-meta"><span>⌘ {task.worktree}</span><span>{projectDir || "프로젝트 폴더 미선택"}</span></div><div className="crew-task-model">모델 · {taskModel ? modelLabel(taskModel) : modelLabel(crewModel)}</div>{task.status === "queued" && <button className="small crew-start" onClick={() => startTask(task.id)}>worker 시작</button>}{task.status === "running" && <span className="crew-hint">작업 이벤트를 기다리는 중</span>}</article>)}</div>}
  </section>;
}
