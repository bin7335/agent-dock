import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Telemetry } from "./useTelemetry";
import type { ModelOption } from "./types";
import { quotaWindows, remaining } from "./DockPanels";

const KEY = "agentdock.providerPriority";
const MODEL_KEY = "agentdock.providerTargetModels";

export function providerOrder(available: string[], preferred: string[]): string[] {
  return [...new Set([...preferred.filter(id => available.includes(id)), ...available])];
}
function readArray(key: string): string[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(key) ?? "[]");
    return Array.isArray(value) ? value.filter((id): id is string => typeof id === "string") : [];
  } catch { return []; }
}
function readModels(): Record<string, string> {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(MODEL_KEY) ?? "{}");
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    return Object.fromEntries(Object.entries(value).filter((row): row is [string, string] => typeof row[1] === "string"));
  } catch { return {}; }
}

export function ProviderPriority({ telemetry, enabled, models, currentModel, onRouteModel }: {
  telemetry: Telemetry;
  enabled: boolean;
  models: ModelOption[];
  currentModel: string;
  onRouteModel: (model: string | null) => void;
}) {
  const [preferred, setPreferred] = useState(() => readArray(KEY));
  const [selectedModels, setSelectedModels] = useState(readModels);
  const [dragging, setDragging] = useState<string | null>(null);
  const [over, setOver] = useState<string | null>(null);
  const [routeStatus, setRouteStatus] = useState("");
  const [saveError, setSaveError] = useState("");
  const { data: quotaData, error: quotaError } = telemetry.quotas;
  const { data: providerData, error: providerError } = telemetry.providers;
  const available = providerData?.providers.filter(p => !p.disabled).map(p => p.name) ?? quotaData?.providers ?? [];
  const order = providerOrder(available, preferred);
  const modelProvider = (model: ModelOption) => model.provider || (model.id.includes("/") ? model.id.split("/", 1)[0] : "openai");
  const optionsFor = (provider: string) => models.filter(model => modelProvider(model) === provider);
  const currentProvider = models.find(model => model.id === currentModel)?.provider;
  const selectedFor = (provider: string) => {
    const options = optionsFor(provider);
    const saved = selectedModels[provider];
    if (saved && options.some(model => model.id === saved)) return saved;
    if (currentProvider === provider && options.some(model => model.id === currentModel)) return currentModel;
    return options[0]?.id ?? "";
  };
  const targets = order.map(provider => {
    const selected = selectedFor(provider);
    const option = optionsFor(provider).find(model => model.id === selected);
    return { provider, model: option?.route_model || selected };
  }).filter(target => target.model);
  const targetSignature = JSON.stringify(targets);

  useEffect(() => {
    let disposed = false;
    if (!enabled) {
      onRouteModel(null);
      setRouteStatus("자동 전환 꺼짐");
      return () => { disposed = true; };
    }
    if (targets.length < 2) {
      onRouteModel(null);
      setRouteStatus("모델이 있는 AI 제공자 2개 이상이 필요합니다.");
      return () => { disposed = true; };
    }
    setRouteStatus("자동 전환 적용 중…");
    void invoke<{ model: string }>("configure_ocx_auto_handoff", { targets }).then(result => {
      if (!disposed) { onRouteModel(result.model); setRouteStatus(`적용됨 · ${result.model}`); }
    }).catch(error => {
      if (!disposed) { onRouteModel(null); setRouteStatus(`적용 실패 · ${String(error)}`); }
    });
    return () => { disposed = true; };
  }, [enabled, targetSignature, onRouteModel]);

  function saveOrder(next: string[]) {
    setPreferred(next);
    try { localStorage.setItem(KEY, JSON.stringify(next)); setSaveError(""); }
    catch { setSaveError("순서를 저장하지 못했습니다. 이번 실행에만 적용됩니다."); }
  }
  function move(from: string, target: string) {
    if (from === target || !order.includes(from) || !order.includes(target)) return;
    const next = order.filter(id => id !== from);
    next.splice(order.indexOf(target), 0, from);
    saveOrder([...next, ...preferred.filter(id => !next.includes(id))]);
  }
  function selectModel(provider: string, model: string) {
    const next = { ...selectedModels, [provider]: model };
    setSelectedModels(next);
    try { localStorage.setItem(MODEL_KEY, JSON.stringify(next)); setSaveError(""); }
    catch { setSaveError("대상 모델을 저장하지 못했습니다. 이번 실행에만 적용됩니다."); }
  }

  return <section className="provider-priority" aria-label="AI 우선순위">
    <div className="section-heading"><h2>AI 우선순위</h2><span className="subtle">드래그 또는 화살표로 변경</span></div>
    <p className="settings-help muted">위에서부터 요청하며, 한도·인증·일시 오류가 발생하면 다음 AI와 선택 모델로 자동 전환합니다.</p>
    {(providerError || quotaError) && !providerData && !quotaData && <p className="inline-error" role="alert">연결된 제공자 목록 조회 실패</p>}
    {!providerData && !quotaData && !providerError && !quotaError && <p role="status">연결된 AI를 불러오는 중…</p>}
    {(providerData || quotaData) && !order.length && <p className="muted">인식된 활성 제공자가 없습니다. 계정 관리에서 연결하세요.</p>}
    <div className="provider-priority-list">{order.map((id, index) => {
      const report = quotaData?.reports.find(row => row.provider === id);
      const values = quotaWindows(report).map(w => remaining(w)).filter((value): value is number => value !== null);
      const left = values.length ? Math.min(...values) : null;
      const stale = !!quotaError || !!report && Date.now() - report.updatedAt > 600000;
      const options = optionsFor(id);
      return <article key={id} className={`provider-priority-card${dragging === id ? " dragging" : ""}${over === id && dragging !== id ? " drag-over" : ""}`}
        draggable onDragStart={e => { e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", id); setDragging(id); }}
        onDragOver={e => { if (dragging) { e.preventDefault(); setOver(id); } }}
        onDrop={e => { e.preventDefault(); if (dragging) move(dragging, id); setDragging(null); setOver(null); }}
        onDragEnd={() => { setDragging(null); setOver(null); }}>
        <span className="prio">{index + 1}</span><div className="provider-priority-main"><strong>{report?.label ?? id}</strong>
          <span>{left === null ? "한도 정보 없음" : `${stale ? "이전 값 · " : ""}${Math.round(left)}% 남음`}</span>
          <select aria-label={`${report?.label ?? id} 자동 전환 모델`} value={selectedFor(id)} disabled={!options.length} onChange={e => selectModel(id, e.currentTarget.value)}>
            {!options.length && <option value="">사용 가능한 모델 없음</option>}
            {options.map(model => <option value={model.id} key={model.id}>{model.label}</option>)}
          </select>
        </div>
        <div className="provider-priority-actions"><button className="small" aria-label={`${report?.label ?? id} 순위 올리기`} disabled={index === 0} onClick={() => move(id, order[index - 1])}>↑</button>
          <button className="small" aria-label={`${report?.label ?? id} 순위 내리기`} disabled={index === order.length - 1} onClick={() => move(id, order[index + 1])}>↓</button></div>
      </article>;
    })}</div>
    <p className={`route-status ${enabled && routeStatus.startsWith("적용됨") ? "ok" : ""}`} role="status">{routeStatus}</p>
    {saveError && <p className="inline-error" role="alert">{saveError}</p>}
  </section>;
}
