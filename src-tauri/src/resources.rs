use serde::Serialize;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSnapshot {
    cpu_percent: Option<f64>,
    memory_used: u64,
    memory_total: u64,
    ocx_memory: Option<u64>,
    ocx_private: Option<u64>,
    ocx_cpu_percent: Option<f64>,
}

fn percent_delta(busy: u64, previous_busy: u64, total: u64, previous_total: u64) -> Option<f64> {
    let elapsed = total.checked_sub(previous_total)?;
    let used = busy.checked_sub(previous_busy)?;
    (elapsed > 0 && used <= elapsed).then(|| used as f64 / elapsed as f64 * 100.0)
}

#[cfg(windows)]
#[tauri::command]
pub fn get_system_resources(ocx_pid: Option<u32>) -> Result<ResourceSnapshot, String> {
    use std::sync::{Mutex, OnceLock};
    use windows::Win32::{
        Foundation::{CloseHandle, FILETIME},
        System::{
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX},
            SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
            Threading::{GetProcessTimes, GetSystemTimes, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ},
        },
    };
    #[derive(Clone, Copy)]
    struct Sample { total: u64, busy: u64, process: Option<(u32, u64, u64)> }
    static PREVIOUS: OnceLock<Mutex<Option<Sample>>> = OnceLock::new();
    let mut previous = PREVIOUS.get_or_init(|| Mutex::new(None)).lock().map_err(|_| "리소스 상태 잠금 실패")?;
    let ticks = |v: FILETIME| (u64::from(v.dwHighDateTime) << 32) | u64::from(v.dwLowDateTime);
    // All calls are read-only; every successful OpenProcess is paired with CloseHandle.
    unsafe {
        let mut memory = MEMORYSTATUSEX::default();
        memory.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        GlobalMemoryStatusEx(&mut memory).map_err(|e| format!("메모리 조회 실패: {e}"))?;
        let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
        GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).map_err(|e| format!("CPU 조회 실패: {e}"))?;
        let total = ticks(kernel) + ticks(user);
        let busy = total.saturating_sub(ticks(idle));
        let (mut process_sample, mut ocx_memory, mut ocx_private) = (None, None, None);
        if let Some(pid) = ocx_pid.filter(|pid| *pid > 0) {
            if let Ok(process) = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) {
                let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();
                counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
                if K32GetProcessMemoryInfo(process, (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX).cast(), counters.cb).as_bool() {
                    ocx_memory = Some(counters.WorkingSetSize as u64);
                    ocx_private = Some(counters.PrivateUsage as u64);
                }
                let (mut created, mut exited, mut pk, mut pu) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
                if GetProcessTimes(process, &mut created, &mut exited, &mut pk, &mut pu).is_ok() {
                    process_sample = Some((pid, ticks(created), ticks(pk) + ticks(pu)));
                }
                let _ = CloseHandle(process);
            }
        }
        let cpu_percent = previous.as_ref().and_then(|p| percent_delta(busy, p.busy, total, p.total));
        let ocx_cpu_percent = previous.as_ref().and_then(|p| {
            let (pid, created, used) = process_sample?;
            let (old_pid, old_created, old_used) = p.process?;
            if pid != old_pid || created != old_created { return None; }
            percent_delta(used, old_used, total, p.total)
        });
        *previous = Some(Sample { total, busy, process: process_sample });
        Ok(ResourceSnapshot {
            cpu_percent, memory_used: memory.ullTotalPhys.saturating_sub(memory.ullAvailPhys),
            memory_total: memory.ullTotalPhys, ocx_memory, ocx_private, ocx_cpu_percent,
        })
    }
}

#[cfg(not(windows))]
#[tauri::command]
pub fn get_system_resources(_ocx_pid: Option<u32>) -> Result<ResourceSnapshot, String> {
    Err("현재 리소스 모니터는 Windows에서 지원됩니다".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_uses_deltas_and_rejects_reset_counters() {
        assert_eq!(percent_delta(70, 20, 200, 100), Some(50.0));
        assert_eq!(percent_delta(20, 20, 100, 100), None);
        assert_eq!(percent_delta(10, 20, 200, 100), None);
        assert_eq!(percent_delta(250, 20, 200, 100), None);
    }
    #[cfg(windows)]
    #[test]
    fn system_memory_is_real_and_bounded() {
        let sample = get_system_resources(None).unwrap();
        assert!(sample.memory_total > 0);
        assert!(sample.memory_used <= sample.memory_total);
        assert_eq!(sample.ocx_memory, None);
    }
}
