import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { OcxHealth, OcxUsage, QuotaResponse, ResourceSnapshot } from "./types";

function usePoll<T>(read: () => Promise<T>, interval: number, key = "") {
  const reader = useRef(read);
  reader.current = read;
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [updatedAt, setUpdatedAt] = useState<number | null>(null);
  useEffect(() => {
    let disposed = false;
    let timer: ReturnType<typeof setTimeout>;
    setData(null);
    setError(null);
    setUpdatedAt(null);
    const tick = async () => {
      try {
        const value = await reader.current();
        if (!disposed) { setData(value); setError(null); setUpdatedAt(Date.now()); }
      } catch (e) {
        if (!disposed) setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!disposed) timer = setTimeout(tick, interval);
      }
    };
    void tick();
    return () => { disposed = true; clearTimeout(timer); };
  }, [interval, key]);
  return { data, error, updatedAt };
}

export function useTelemetry(range: string, refresh: number) {
  const health = usePoll(() => invoke<OcxHealth>("get_ocx_health"), 15000, String(refresh));
  const pid = health.error ? null : health.data?.pid ?? null;
  const resources = usePoll(() => invoke<ResourceSnapshot>("get_system_resources", { ocxPid: pid }), 2000, String(pid));
  const usage = usePoll(() => invoke<OcxUsage>("get_ocx_usage", { range }), 10000, `${range}:${refresh}`);
  const quotas = usePoll(() => invoke<QuotaResponse>("get_ocx_quotas"), 60000, String(refresh));
  return { health, resources, usage, quotas };
}

export type Telemetry = ReturnType<typeof useTelemetry>;
