import type { Telemetry } from "./useTelemetry";
import type { QuotaReport, QuotaWindow } from "./types";

export function bytes(value: number | null | undefined): string {
  if (value == null) return "—";
  return value >= 1024 ** 3 ? `${(value / 1024 ** 3).toFixed(1)} GB` : `${Math.round(value / 1024 ** 2)} MB`;
}
export function percentage(value: number | null | undefined): string {
  return value == null || !Number.isFinite(value) ? "—" : `${Math.round(value)}%`;
}
const number = (value?: number) => value == null ? "—" : Intl.NumberFormat("ko-KR", { notation: "compact", maximumFractionDigits: 1 }).format(value);

export function ResourcePanel({ telemetry, compact }: { telemetry: Telemetry; compact: boolean }) {
  const { resources, health } = telemetry;
  const data = resources.error ? null : resources.data;
  const usedPercent = data && data.memoryTotal > 0 ? data.memoryUsed / data.memoryTotal * 100 : null;
  if (compact) return <div className="resource-inline" title={resources.error ?? "이 PC의 실시간 리소스 · 2초마다 갱신"}>
    <span><i className="micro-dot" />CPU <b>{percentage(data?.cpuPercent)}</b></span>
    <span>RAM <b>{percentage(usedPercent)}</b></span>
    <span>OCX <b>{bytes(data?.ocxMemory)}</b></span>
  </div>;
  const cards = [
    { label: "PC CPU", value: percentage(data?.cpuPercent), detail: data?.cpuPercent == null ? "측정 대기 중" : "전체 프로세서 사용률", percent: data?.cpuPercent },
    { label: "PC 메모리", value: percentage(usedPercent), detail: `${bytes(data?.memoryUsed)} / ${bytes(data?.memoryTotal)}`, percent: usedPercent },
    { label: "OCX 메모리", value: bytes(data?.ocxMemory), detail: health.error ? "프로세스 연결 대기" : `전용 메모리 ${bytes(data?.ocxPrivate)}`, percent: null },
    { label: "OCX CPU", value: percentage(data?.ocxCpuPercent), detail: "PC 전체 CPU 기준", percent: data?.ocxCpuPercent },
  ];
  return <section className="dashboard-section" aria-label="리소스">
    <div className="section-heading"><h2>리소스</h2><span className="subtle">이 PC · 2초 갱신</span></div>
    <div className="resource-grid">{cards.map(card => <article className="metric-card" key={card.label}>
      <span className="metric-label">{card.label}</span><strong className="metric-value">{card.value}</strong>
      <span className="metric-description">{card.detail}</span>
      {card.percent != null && <div className={`meter ${card.percent > 85 ? "warning" : ""}`}><i style={{ width: `${Math.max(0, Math.min(100, card.percent))}%` }} /></div>}
    </article>)}</div>
    {resources.error && <p className="inline-error" role="alert">{resources.error}</p>}
  </section>;
}

export function UsagePanel({ telemetry, compact, range, onRange }: { telemetry: Telemetry; compact: boolean; range: string; onRange: (v: string) => void }) {
  const { data, error, updatedAt } = telemetry.usage;
  const cost = data?.summary.estimatedCostUsd;
  const formattedCost = cost == null ? "—" : `$${cost.toFixed(4)}`;
  if (compact) return <div className="compact-usage" title={error ?? "Opencodex를 통과한 요청의 추정 비용·토큰"}>
    <span className="subtle">{range === "all" ? "전체" : range === "7d" ? "7일" : "30일"} 사용량</span>
    <strong>{formattedCost}</strong><span className="compact-tokens">{number(data?.summary.totalTokens)} <small>tokens</small></span>
    {error && <span className="warning-text">갱신 실패</span>}
  </div>;
  return <section className="dashboard-section usage-section">
    <div className="section-heading"><h2>Opencodex 사용량</h2><select aria-label="사용량 기간" value={range} onChange={e => onRange(e.target.value)}>
      <option value="all">전체 기간</option><option value="7d">최근 7일</option><option value="30d">최근 30일</option>
    </select></div>
    <div className="usage-grid">
      <div><span className="metric-label">추정 비용</span><strong className="cost-value">{formattedCost}</strong><small>USD · 청구액과 다를 수 있음</small></div>
      <div><span className="metric-label">토큰</span><strong>{data?.summary.totalTokens.toLocaleString() ?? "—"}</strong><small>{data?.summary.requests.toLocaleString() ?? "—"}회 요청</small></div>
    </div>
    <div className="usage-footnote"><span>OCX 경유 요청</span><span>{updatedAt ? `${new Date(updatedAt).toLocaleTimeString("ko-KR", { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false })} 갱신` : "연결 대기"}</span></div>
    {data && data.summary.unpricedRequests + data.summary.unmeteredRequests > 0 && <p className="data-notice">비용 산정 제외 {data.summary.unpricedRequests + data.summary.unmeteredRequests}건</p>}
    {data?.historyTruncated && <p className="data-notice">서버 기록 일부만 집계되었습니다.</p>}
    {error && <p className="inline-error" role="alert">{data ? "이전 수치 표시 · " : ""}{error}</p>}
  </section>;
}

export function quotaWindows(report?: QuotaReport): QuotaWindow[] {
  if (!report) return [];
  const q = report.quota;
  const rows: QuotaWindow[] = [];
  if (q.fiveHourPercent != null) rows.push({ label: "5시간", percent: q.fiveHourPercent, resetAt: q.fiveHourResetAt });
  if (q.weeklyPercent != null) rows.push({ label: "주간", percent: q.weeklyPercent, resetAt: q.weeklyResetAt });
  if (q.monthlyPercent != null) rows.push({ label: "월간", percent: q.monthlyPercent, resetAt: q.monthlyResetAt });
  for (const window of q.customWindows ?? []) {
    if (window.segments?.length) rows.push(...window.segments.map(s => ({ ...s, label: `${window.label} · ${s.label}` })));
    else rows.push(window);
  }
  return rows;
}

export function remaining(window: QuotaWindow, now = Date.now()): number | null {
  if (window.valueLabel || window.percent == null || !Number.isFinite(window.percent)) return null;
  // An expired server sample does not prove that new quota is already available.
  if (window.resetAt && resetEpoch(window.resetAt) <= now) return null;
  return Math.max(0, Math.min(100, 100 - window.percent));
}
function resetEpoch(value: number) { return value < 10_000_000_000 ? value * 1000 : value; }
function resetText(value?: number) {
  if (!value) return "리셋 시각 미제공";
  const minutes = Math.ceil((resetEpoch(value) - Date.now()) / 60000);
  if (minutes <= 0) return "리셋 확인 중";
  if (minutes >= 1440) return `${Math.floor(minutes / 1440)}일 ${Math.floor(minutes % 1440 / 60)}시간 후 리셋`;
  return minutes >= 60 ? `${Math.floor(minutes / 60)}시간 ${minutes % 60}분 후 리셋` : `${minutes}분 후 리셋`;
}

export function QuotaFooter({ telemetry, active, onActive, compact }: { telemetry: Telemetry; active: string | null; onActive: (name: string | null) => void; compact: boolean }) {
  const { data, error } = telemetry.quotas;
  const providers = Array.from(new Set([...(data?.providers ?? []), ...(data?.reports.map(r => r.provider) ?? [])]));
  const reportFor = (name: string) => data?.reports.find(r => r.provider === name);
  const report = active ? reportFor(active) : undefined;
  const windows = quotaWindows(report);
  const show = (name: string | null) => onActive(name);
  const leave = () => onActive(null);
  return <footer className={`quota-footer ${compact ? "compact" : ""}`} aria-label="AI 남은 한도" onMouseLeave={leave}
    onBlur={e => { if (!e.currentTarget.contains(e.relatedTarget)) leave(); }}>
    <div className="quota-heading"><span>AI 남은 한도</span><span>{error ? (data ? "조회 실패 · 이전 값" : "조회 실패") : "마우스를 올려 상세 보기"}</span></div>
    <div className="quota-strip">{providers.map(name => {
      const r = reportFor(name);
      const values = quotaWindows(r).map(w => remaining(w)).filter((v): v is number => v !== null);
      const left = values.length ? Math.min(...values) : null;
      const stale = error || (r && Date.now() - r.updatedAt > 10 * 60000);
      return <button key={name} className={`quota-chip ${active === name ? "selected" : ""} ${left !== null && left < 20 ? "low" : ""}`} aria-expanded={active === name}
        onMouseEnter={() => show(name)} onFocus={() => show(name)} onClick={() => show(name)} aria-label={`${r?.label ?? name} 남은 한도 ${percentage(left)} 상세`}>
        <span className="quota-chip-name">{r?.label ?? name}</span><b>{stale ? "~" : ""}{percentage(left)}</b>
        <span className="meter"><i style={{ width: `${left ?? 0}%` }} /></span>
      </button>;
    })}</div>
    {!providers.length && <p className="quota-empty">{error ? "한도 조회 실패 · 개요에서 새로고침하세요" : data ? "연결된 AI 제공자가 없습니다" : "AI 한도 불러오는 중…"}</p>}
    {active && <div className="quota-popover" role="region" aria-label={`${report?.label ?? active} 한도 상세`}>
      <div className="section-heading"><h3>{report?.label ?? active}</h3><button className="icon-button" aria-label="한도 상세 닫기" onClick={() => show(null)}>×</button></div>
      <p className="quota-subtitle">{report?.aggregation ? "계정 풀 합산 · " : ""}사용 가능 비율 · 가장 적게 남은 한도를 요약 표시</p>
      {windows.map((w, i) => { const left = remaining(w); return <div className={`quota-window ${left !== null && left < 20 ? "low" : ""}`} key={`${w.label}:${i}`}>
        <div><span>{w.label}</span><strong>{w.valueLabel ?? (left === null ? "확인 중" : `${Math.round(left)}% 남음`)}</strong></div>
        {left !== null && <div className="meter"><i style={{ width: `${left}%` }} /></div>}
        <small>{resetText(w.resetAt)}</small>
      </div>; })}
      {!windows.length && <p className="quota-empty">이 제공자의 한도 정보가 없습니다. 사용 가능 여부와는 별개입니다.</p>}
      {(error || (report && Date.now() - report.updatedAt > 10 * 60000)) && <p className="data-notice">이전 조회 값입니다. 현재 한도와 다를 수 있습니다.</p>}
      {report && <small className="subtle">{new Date(report.updatedAt).toLocaleTimeString("ko-KR")} 기준 · OCX 한도 정보</small>}
    </div>}
  </footer>;
}
