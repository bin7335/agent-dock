mod adapters;
mod availability;
// DB 스키마는 SQLite 영속화 배선 전까지 참조되지 않는다.
#[allow(dead_code)]
mod db;
mod models;
mod runner;
mod scheduler;

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use adapters::{AgentEvent, CliAdapter};
use availability::{AvailabilityMonitor, AvailabilitySnapshot, FailureKind, ProbeOutcome};
use models::{CliId, CommandSpec, Job, JobStatus};
use scheduler::RoutingProfile;
use tauri::{AppHandle, Emitter, Manager};

/// probe 명령(version·login status) 상한. 넘기면 죽이고 unavailable로 본다.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// 마지막 가용성 스냅샷 보존 파일 (앱 데이터 폴더). 재시작 후 Claude의 공식 사용률을 잃지 않기 위해.
const STORE_FILE: &str = "availability.json";

fn store_path(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().app_data_dir().ok().map(|d| d.join(STORE_FILE))
}

fn save_store(app: &AppHandle, snaps: &[AvailabilitySnapshot]) {
    let Some(path) = store_path(app) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(snaps) {
        let _ = std::fs::write(path, json);
    }
}

fn load_store(app: &AppHandle) -> Vec<AvailabilitySnapshot> {
    store_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
/// 모니터 틱 간격. 틱마다 쿨다운 만료·재검사 예정만 확인하므로 가볍다.
const TICK_INTERVAL: Duration = Duration::from_secs(30);

struct AppState {
    runner: Arc<runner::Runner>,
    monitor: Arc<Mutex<AvailabilityMonitor>>,
    /// 라우팅 프로필. 상단 카드 드래그로 chain이 바뀐다 (set_routing_chain).
    profile: Mutex<RoutingProfile>,
}

/// 프론트로 흘려보내는 실행 이벤트. listen("agent-event")로 수신한다.
/// cli를 함께 실어 프론트가 run_id→cli 매핑을 타이밍에 의존하지 않게 한다.
#[derive(Clone, serde::Serialize)]
struct RunEvent {
    run_id: u64,
    cli: CliId,
    event: AgentEvent,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn adapter_for(cli: CliId) -> Result<Arc<dyn CliAdapter>, String> {
    adapters::registry()
        .into_iter()
        .find(|a| a.id() == cli)
        .ok_or_else(|| "등록되지 않은 CLI".to_string())
}

fn make_job(request: String, project_dir: String, allow_writes: bool) -> Job {
    Job {
        id: 0,
        title: request.chars().take(40).collect(),
        request,
        project_dir,
        profile: "코딩 작업".into(),
        allow_writes,
        unattended_ok: false,
        status: JobStatus::Starting,
    }
}

fn snapshots_of(monitor: &Mutex<AvailabilityMonitor>) -> Vec<AvailabilitySnapshot> {
    monitor.lock().map(|m| m.snapshots()).unwrap_or_default()
}

/// 가용성 변화를 프론트로 알리고 파일에도 보존한다. listen("availability-changed")
fn emit_availability(app: &AppHandle, monitor: &Mutex<AvailabilityMonitor>) {
    let snaps = snapshots_of(monitor);
    save_store(app, &snaps);
    let _ = app.emit("availability-changed", snaps);
}

/// 실행 스트림에서 한도·인증 신호를 뽑아 모니터에 반영한다 (PRD 6장: 스트림에서 한도 신호 수집).
fn observe_run_event(
    app: &AppHandle,
    monitor: &Mutex<AvailabilityMonitor>,
    cli: CliId,
    event: &AgentEvent,
) {
    let changed = match event {
        AgentEvent::RateLimit {
            window,
            utilization,
            resets_at,
        } => monitor
            .lock()
            .map(|mut m| m.apply_rate_limit(cli, window, *utilization, *resets_at, now()))
            .unwrap_or(false),
        AgentEvent::Completed { ok: false, summary } => monitor
            .lock()
            .map(|mut m| m.apply_failure(cli, summary, now()).is_some())
            .unwrap_or(false),
        // stderr에는 진단 로그가 섞이므로 한도·인증처럼 확실한 패턴만 반영한다
        AgentEvent::Stderr { text } => match availability::classify_failure(text) {
            FailureKind::RateLimit | FailureKind::Auth => monitor
                .lock()
                .map(|mut m| m.apply_failure(cli, text, now()).is_some())
                .unwrap_or(false),
            _ => false,
        },
        _ => false,
    };
    if changed {
        emit_availability(app, monitor);
    }
}

async fn probe_one(adapter: &dyn CliAdapter) -> ProbeOutcome {
    let spec = adapter.probe_command();
    match runner::run_capture(&spec, PROBE_TIMEOUT).await {
        Ok(out) if out.timed_out => ProbeOutcome::Unavailable {
            detail: format!("probe 시간 초과: {}", out.stderr),
        },
        Ok(out) => adapter.interpret_probe(out.code, &out.stdout, &out.stderr),
        Err(e) => ProbeOutcome::Unavailable {
            detail: format!("실행 실패: {e}"),
        },
    }
}

/// 지정 CLI들을 순서대로 probe하고 결과마다 프론트에 알린다.
async fn run_probes(app: &AppHandle, monitor: &Mutex<AvailabilityMonitor>, clis: &[CliId]) {
    for cli in clis {
        let Ok(adapter) = adapter_for(*cli) else {
            continue;
        };
        let outcome = probe_one(adapter.as_ref()).await;
        let ready = matches!(outcome, ProbeOutcome::Ready { .. });
        if let Ok(mut m) = monitor.lock() {
            m.apply_probe(*cli, outcome, now());
        }
        // 설치·로그인이 확인된 CLI만 공식 사용량을 읽는다 (Codex app-server 등)
        if ready {
            if let Some(ex) = adapter.rate_limit_exchange() {
                let done_id = ex.done_id;
                let done = move |line: &str| {
                    serde_json::from_str::<serde_json::Value>(line)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|i| i.as_u64()))
                        == Some(done_id)
                };
                if let Ok(lines) =
                    runner::exchange_lines(&ex.spec, &ex.inputs, done, PROBE_TIMEOUT).await
                {
                    let readings = adapter.parse_rate_limits(&lines);
                    if let Ok(mut m) = monitor.lock() {
                        for r in &readings {
                            m.apply_rate_limit(*cli, &r.window, r.utilization, r.resets_at, now());
                        }
                    }
                }
            }
        }
        emit_availability(app, monitor);
    }
}

async fn spawn_run(
    app: AppHandle,
    state: &AppState,
    cli: CliId,
    adapter: Arc<dyn CliAdapter>,
    spec: CommandSpec,
) -> Result<u64, String> {
    let monitor = Arc::clone(&state.monitor);
    let sink: runner::EventSink = Arc::new(move |run_id, event| {
        observe_run_event(&app, &monitor, cli, &event);
        let _ = app.emit("agent-event", RunEvent { run_id, cli, event });
    });
    state
        .runner
        .start(spec, adapter, sink)
        .await
        .map_err(|e| e.to_string())
}

/// 새 대화를 시작한다. 반환값은 run_id.
#[tauri::command]
async fn start_job(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    cli: CliId,
    request: String,
    project_dir: String,
    allow_writes: bool,
) -> Result<u64, String> {
    let adapter = adapter_for(cli)?;
    let spec = adapter.build_command(&make_job(request, project_dir, allow_writes));
    spawn_run(app, state.inner(), cli, adapter, spec).await
}

/// 기존 세션을 이어 후속 메시지를 보낸다. 반환값은 run_id.
#[tauri::command]
async fn continue_job(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    cli: CliId,
    session_id: String,
    request: String,
    project_dir: String,
    allow_writes: bool,
) -> Result<u64, String> {
    let adapter = adapter_for(cli)?;
    let spec = adapter
        .build_resume_command(&make_job(request, project_dir, allow_writes), &session_id)
        .ok_or_else(|| "이 CLI는 세션 재개를 지원하지 않습니다".to_string())?;
    spawn_run(app, state.inner(), cli, adapter, spec).await
}

/// 실행 중인 run을 중지한다.
#[tauri::command]
async fn cancel_run(state: tauri::State<'_, AppState>, run_id: u64) -> Result<bool, String> {
    Ok(state.runner.cancel(run_id).await)
}

/// 현재 가용성 스냅샷 (라우팅 순서). 앱 시작 직후 프론트가 한 번 읽고 이후는 이벤트로 받는다.
#[tauri::command]
fn get_availability(state: tauri::State<'_, AppState>) -> Vec<AvailabilitySnapshot> {
    snapshots_of(&state.monitor)
}

/// 수동 재검사 (상태바 클릭 패널의 버튼, PRD 7장). cli를 생략하면 전부.
#[tauri::command]
async fn recheck_availability(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    cli: Option<CliId>,
) -> Result<Vec<AvailabilitySnapshot>, String> {
    let monitor = Arc::clone(&state.monitor);
    let targets = match cli {
        Some(c) => vec![c],
        None => monitor.lock().map(|m| m.clis()).unwrap_or_default(),
    };
    run_probes(&app, &monitor, &targets).await;
    Ok(snapshots_of(&monitor))
}

/// 현재 라우팅 프로필로 지금 추천되는 CLI (PRD 6장: 새 작업마다 최우선 후보부터 재평가)
#[tauri::command]
fn pick_cli(state: tauri::State<'_, AppState>) -> Option<CliId> {
    let profile = state.profile.lock().ok()?.clone();
    scheduler::pick_candidate(&profile, &snapshots_of(&state.monitor))
}

/// 라우팅 우선순위 변경 (상단 CLI 카드 드래그, PRD 6장 라우팅 프로필).
/// 상태바 순서·추천 CLI도 같은 체인을 따른다. 빠진 등록 CLI는 뒤에 붙이고 모르는 값은 무시한다.
#[tauri::command]
fn set_routing_chain(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    chain: Vec<CliId>,
) -> Result<Vec<AvailabilitySnapshot>, String> {
    let registered: Vec<CliId> = adapters::registry().iter().map(|a| a.id()).collect();
    let mut ordered: Vec<CliId> = Vec::new();
    for c in chain {
        if registered.contains(&c) && !ordered.contains(&c) {
            ordered.push(c);
        }
    }
    if ordered.is_empty() {
        return Err("우선순위에 등록된 CLI가 하나도 없습니다".into());
    }
    for c in &registered {
        if !ordered.contains(c) {
            ordered.push(*c);
        }
    }
    if let Ok(mut p) = state.profile.lock() {
        p.chain = ordered.clone();
    }
    if let Ok(mut m) = state.monitor.lock() {
        m.set_order(&ordered);
    }
    emit_availability(&app, &state.monitor);
    Ok(snapshots_of(&state.monitor))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let profile = RoutingProfile::default();
    let registered: Vec<CliId> = adapters::registry().iter().map(|a| a.id()).collect();
    // 상태바 순서는 라우팅 프로필 순서를 따른다 (PRD 7장). 어댑터가 없는 후보(opencode)는 제외
    let ordered: Vec<CliId> = profile
        .chain
        .iter()
        .copied()
        .filter(|c| registered.contains(c))
        .collect();
    let monitor = Arc::new(Mutex::new(AvailabilityMonitor::new(&ordered, now())));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            runner: Arc::new(runner::Runner::new()),
            monitor: Arc::clone(&monitor),
            profile: Mutex::new(profile),
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            // 이전 실행의 스냅샷 복원 (리셋이 지난 윈도우는 import에서 폐기)
            let stored = load_store(&handle);
            if let Ok(mut m) = monitor.lock() {
                m.import(stored, now());
            }
            emit_availability(&handle, &monitor);
            // 가용성 모니터 루프: 시작 시 전체 probe → 30초마다 쿨다운 만료·재검사 예정 확인
            tauri::async_runtime::spawn(async move {
                let all = monitor.lock().map(|m| m.clis()).unwrap_or_default();
                run_probes(&handle, &monitor, &all).await;
                loop {
                    tokio::time::sleep(TICK_INTERVAL).await;
                    let due = monitor
                        .lock()
                        .map(|mut m| m.tick(now()))
                        .unwrap_or_default();
                    emit_availability(&handle, &monitor);
                    if !due.is_empty() {
                        run_probes(&handle, &monitor, &due).await;
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_job,
            continue_job,
            cancel_run,
            get_availability,
            recheck_availability,
            pick_cli,
            set_routing_chain
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
