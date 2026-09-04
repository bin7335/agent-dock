// 1단계 스캐폴딩: 아직 배선 전인 모듈이 있어 dead_code 경고를 잠재운다.
#![allow(dead_code)]

mod adapters;
mod availability;
mod db;
mod models;
mod runner;
mod scheduler;

use std::sync::Arc;

use adapters::{AgentEvent, CliAdapter};
use models::{CliId, CommandSpec, Job, JobStatus};
use tauri::Emitter;

struct AppState {
    runner: Arc<runner::Runner>,
}

/// 프론트로 흘려보내는 실행 이벤트. listen("agent-event")로 수신한다.
/// cli를 함께 실어 프론트가 run_id→cli 매핑을 타이밍에 의존하지 않게 한다.
#[derive(Clone, serde::Serialize)]
struct RunEvent {
    run_id: u64,
    cli: CliId,
    event: AgentEvent,
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

async fn spawn_run(
    app: tauri::AppHandle,
    state: &AppState,
    cli: CliId,
    adapter: Arc<dyn CliAdapter>,
    spec: CommandSpec,
) -> Result<u64, String> {
    let sink: runner::EventSink = Arc::new(move |run_id, event| {
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
    app: tauri::AppHandle,
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
    app: tauri::AppHandle,
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            runner: Arc::new(runner::Runner::new()),
        })
        .invoke_handler(tauri::generate_handler![start_job, continue_job, cancel_run])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
