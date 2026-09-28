import { useState } from "react";
import type { Telemetry } from "./useTelemetry";
import { quotaWindows, remaining } from "./DockPanels";

const KEY = "agentdock.providerPriority";
export function providerOrder(available: string[], preferred: string[]): string[] {
  return [...new Set([...preferred.filter(id => available.includes(id)), ...available])];
}
function savedOrder(): string[] {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(value) ? value.filter((id): id is string => typeof id === "string") : [];
  } catch { return []; }
}

export function ProviderPriority({ telemetry }: { telemetry: Telemetry }) {
  const [preferred, setPreferred] = useState(savedOrder);
  const [dragging, setDragging] = useState<string | null>(null);
  const [over, setOver] = useState<string | null>(null);
  const [saveError, setSaveError] = useState("");
  const { data: quotaData, error: quotaError } = telemetry.quotas;
  const { data: providerData, error: providerError } = telemetry.providers;
  const available = providerData?.providers.filter(p => !p.disabled).map(p => p.name) ?? quotaData?.providers ?? [];
  const order = providerOrder(available, preferred);
  function move(from: string, target: string) {
    if (from === target || !order.includes(from) || !order.includes(target)) return;
    const next = order.filter(id => id !== from);
    next.splice(order.indexOf(target), 0, from);
    // Retain absent provider preferences across temporary disconnects.
    const stored = [...next, ...preferred.filter(id => !next.includes(id))];
    setPreferred(stored);
    try { localStorage.setItem(KEY, JSON.stringify(stored)); setSaveError(""); }
    catch { setSaveError("순서를 저장하지 못했습니다. 이번 실행에만 적용됩니다."); }
  }
  return <section aria-label="AI 우선순위">
    <div className="section-heading"><h2>AI 우선순위</h2><span className="subtle">드래그로 선호 순서 변경</span></div>
    <p className="settings-help muted">Opencodex에서 활성화한 제공자를 계정 묶음으로 표시합니다. 이 순서는 위젯의 선호 순서이며 자동 전환에는 아직 적용되지 않습니다.</p>
    {(providerError || quotaError) && !providerData && !quotaData && <p className="inline-error" role="alert">연결된 제공자 목록 조회 실패</p>}
    {!providerData && !quotaData && !providerError && !quotaError && <p role="status">연결된 AI를 불러오는 중…</p>}
    {(providerData || quotaData) && !order.length && <p className="muted">인식된 활성 제공자가 없습니다. 설정의 계정 관리에서 연결하세요.</p>}
    <div className="cli-row">{order.map((id, index) => {
      const report = quotaData?.reports.find(row => row.provider === id);
      const values = quotaWindows(report).map(w => remaining(w)).filter((value): value is number => value !== null);
      const left = values.length ? Math.min(...values) : null;
      const stale = !!quotaError || !!report && Date.now() - report.updatedAt > 600000;
      return <div key={id} className={`cli-card${dragging === id ? " dragging" : ""}${over === id && dragging !== id ? " drag-over" : ""}`}
        draggable onDragStart={e => { e.dataTransfer.effectAllowed = "move"; e.dataTransfer.setData("text/plain", id); setDragging(id); }}
        onDragOver={e => { if (dragging) { e.preventDefault(); setOver(id); } }}
        onDrop={e => { e.preventDefault(); if (dragging) move(dragging, id); setDragging(null); setOver(null); }}
        onDragEnd={() => { setDragging(null); setOver(null); }}>
        <span className="prio">{index + 1}</span><span className="cli-name">{report?.label ?? id}</span>
        <span className="cli-state">{left === null ? "한도 정보 없음" : `${stale ? "이전 값 · " : ""}${Math.round(left)}% 남음`}</span>
        <button className="small" aria-label={`${report?.label ?? id} 순위 올리기`} disabled={index === 0} onClick={() => move(id, order[index - 1])}>↑</button>
        <button className="small" aria-label={`${report?.label ?? id} 순위 내리기`} disabled={index === order.length - 1} onClick={() => move(id, order[index + 1])}>↓</button>
      </div>;
    })}</div>
    {saveError && <p role="alert">{saveError}</p>}
  </section>;
}
