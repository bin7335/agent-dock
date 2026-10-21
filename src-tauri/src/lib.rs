mod adapters;
mod availability;
// DB ?ㅽ궎留덈뒗 SQLite ?곸냽??諛곗꽑 ?꾧퉴吏 李몄“?섏? ?딅뒗??
#[allow(dead_code)]
mod db;
mod models;
mod runner;
mod resources;
mod openrouter_quota;
mod scheduler;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use adapters::{
    line_has_id, AgentEvent, CliAdapter, FollowUp, LoginFlow, ModelListing, PermissionContext,
    PermissionDecision,
};
use availability::{
    AvailabilityMonitor, AvailabilitySnapshot, AvailabilityState, FailureKind, ProbeOutcome,
};
use models::{CliId, CommandSpec, Job, JobStatus, ModelOption};
use scheduler::RoutingProfile;
use tauri::{AppHandle, Emitter, Manager};

/// probe 紐낅졊(version쨌login status) ?곹븳. ?섍린硫?二쎌씠怨?unavailable濡?蹂몃떎.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// 紐⑤뜽 紐⑸줉 議고쉶 ?곹븳 (app-server쨌ACP 湲곕룞 ?ы븿)
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// 濡쒓렇???먮쫫 ?곹븳 ??釉뚮씪?곗? 濡쒓렇?몄쓣 湲곕떎由곕떎
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// 濡쒓렇??肄섏넄???대젮 ?덈뒗 ?숈븞 ?ш???二쇨린. 釉뚮씪?곗? 濡쒓렇?몄씠 ?앸굹硫?李쎌쓣 ?レ? ?딆븘??怨?珥덈줉?쇰줈 諛붾먮떎.
const LOGIN_POLL: Duration = Duration::from_secs(8);
/// ?뱀씤 ?붿껌 臾댁쓳???곹븳. 吏?섎㈃ ?먮룞 嫄곕???CLI媛 硫덉떠 ?덉? ?딄쾶 ?쒕떎 (PRD 5??臾댁씤 ?뺤콉??理쒖냼 ?덉쟾?μ튂)
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// 紐⑤땲????媛꾧꺽. ?깅쭏??荑⑤떎??留뚮즺쨌?ш????덉젙留??뺤씤?섎?濡?媛蹂띾떎.
const TICK_INTERVAL: Duration = Duration::from_secs(30);
/// 留덉?留?媛?⑹꽦 ?ㅻ깄??蹂댁〈 ?뚯씪 (???곗씠???대뜑). ?ъ떆????Claude??怨듭떇 ?ъ슜瑜좎쓣 ?껋? ?딄린 ?꾪빐.
const STORE_FILE: &str = "availability.json";

struct AppState {
    runner: Arc<runner::Runner>,
    monitor: Arc<Mutex<AvailabilityMonitor>>,
    /// ?쇱슦???꾨줈?? ?곷떒 移대뱶 ?쒕옒洹몃줈 chain??諛붾먮떎 (set_routing_chain).
    profile: Mutex<RoutingProfile>,
    /// ?듭쓣 湲곕떎由щ뒗 ?뱀씤 ?붿껌 (run_id, request_id) ???먮낯. respond_permission??爰쇰궡 ?대떎
    pending: PendingMap,
}

type PendingMap = Arc<Mutex<HashMap<(u64, String), PendingApproval>>>;

/// ?뱀씤 ?붿껌 ?먮낯. ?꾧뎄 ?낅젰쨌?쒖븞? JSON 臾몄옄??洹몃?濡?蹂닿????묐떟???뚮젮以
struct PendingApproval {
    cli: CliId,
    input: String,
    suggestions: String,
}


/// ?꾨줎?몃줈 ?섎젮蹂대궡???ㅽ뻾 ?대깽?? listen("agent-event")濡??섏떊?쒕떎.
/// cli瑜??④퍡 ?ㅼ뼱 ?꾨줎?멸? run_id?뭖li 留ㅽ븨????대컢???섏〈?섏? ?딄쾶 ?쒕떎.
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
        .ok_or_else(|| "?깅줉?섏? ?딆? CLI".to_string())
}

fn make_job(
    request: String,
    project_dir: String,
    allow_writes: bool,
    model: Option<String>,
) -> Job {
    Job {
        id: 0,
        title: request.chars().take(40).collect(),
        request,
        project_dir,
        profile: "肄붾뵫 ?묒뾽".into(),
        allow_writes,
        unattended_ok: false,
        status: JobStatus::Starting,
        model: model.filter(|m| !m.trim().is_empty()),
    }
}

fn app_data_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    let dir = app.path().app_data_dir().ok()?;
    let _ = std::fs::create_dir_all(&dir);
    Some(dir)
}

fn save_store(app: &AppHandle, snaps: &[AvailabilitySnapshot]) {
    let Some(path) = app_data_dir(app).map(|d| d.join(STORE_FILE)) else {
        return;
    };
    if let Ok(json) = serde_json::to_string_pretty(snaps) {
        let _ = std::fs::write(path, json);
    }
}

fn load_store(app: &AppHandle) -> Vec<AvailabilitySnapshot> {
    app_data_dir(app)
        .and_then(|d| std::fs::read_to_string(d.join(STORE_FILE)).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn snapshots_of(monitor: &Mutex<AvailabilityMonitor>) -> Vec<AvailabilitySnapshot> {
    monitor.lock().map(|m| m.snapshots()).unwrap_or_default()
}

/// 媛?⑹꽦 蹂?붾? ?꾨줎?몃줈 ?뚮━怨??뚯씪?먮룄 蹂댁〈?쒕떎. listen("availability-changed")
fn emit_availability(app: &AppHandle, monitor: &Mutex<AvailabilityMonitor>) {
    let snaps = snapshots_of(monitor);
    save_store(app, &snaps);
    let _ = app.emit("availability-changed", snaps);
}

/// ?ㅽ뻾 ?ㅽ듃由쇱뿉???쒕룄쨌?몄쬆 ?좏샇瑜?戮묒븘 紐⑤땲?곗뿉 諛섏쁺?쒕떎 (PRD 6?? ?ㅽ듃由쇱뿉???쒕룄 ?좏샇 ?섏쭛).
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
        // stderr?먮뒗 吏꾨떒 濡쒓렇媛 ?욎씠誘濡??쒕룄쨌?몄쬆泥섎읆 ?뺤떎???⑦꽩留?諛섏쁺?쒕떎
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
    // 以??⑥쐞 援먰솚??probe (Gemini ACP: session/new ?깃났 = ?몄쬆 OK)
    if let Some(ex) = adapter.probe_exchange() {
        let done_id = ex.done_id;
        return match runner::exchange_lines(
            &ex.spec,
            &ex.inputs,
            move |line| line_has_id(line, done_id),
            PROBE_TIMEOUT,
        )
        .await
        {
            Ok(lines) => adapter.interpret_probe_lines(&lines),
            Err(e) => ProbeOutcome::Unavailable {
                detail: format!("?ㅽ뻾 ?ㅽ뙣: {e}"),
            },
        };
    }
    let spec = adapter.probe_command();
    match runner::run_capture(&spec, PROBE_TIMEOUT).await {
        Ok(out) if out.timed_out => ProbeOutcome::Unavailable {
            detail: format!("probe ?쒓컙 珥덇낵: {}", out.stderr),
        },
        Ok(out) => adapter.interpret_probe(out.code, &out.stdout, &out.stderr),
        Err(e) => ProbeOutcome::Unavailable {
            detail: format!("?ㅽ뻾 ?ㅽ뙣: {e}"),
        },
    }
}

/// 吏??CLI?ㅼ쓣 ?쒖꽌?濡?probe?섍퀬 寃곌낵留덈떎 ?꾨줎?몄뿉 ?뚮┛??
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
        // 踰꾩쟾? 蹂꾨룄 紐낅졊?쇰줈 (probe媛 踰꾩쟾????二쇰뒗 CLI)
        if ready {
            if let Some(spec) = adapter.version_command() {
                if let Ok(out) = runner::run_capture(&spec, Duration::from_secs(15)).await {
                    if let Some(v) = adapters::first_line(&out.stdout).filter(|_| out.code == Some(0)) {
                        if let Ok(mut m) = monitor.lock() {
                            m.set_version(*cli, v);
                        }
                    }
                }
            }
        }
        // ?ㅼ튂쨌濡쒓렇?몄씠 ?뺤씤??CLI留?怨듭떇 ?ъ슜?됱쓣 ?쎈뒗??(Codex app-server ??
        if ready {
            if let Some(ex) = adapter.rate_limit_exchange() {
                let done_id = ex.done_id;
                let done = move |line: &str| line_has_id(line, done_id);
                if let Ok(lines) =
                    runner::exchange_lines(&ex.spec, &ex.inputs, done, PROBE_TIMEOUT).await
                {
                    let readings = adapter.parse_rate_limits(&lines);
                    let account = adapter.parse_account(&lines);
                    if let Ok(mut m) = monitor.lock() {
                        for r in &readings {
                            m.apply_rate_limit(*cli, &r.window, r.utilization, r.resets_at, now());
                        }
                        if let Some(acc) = account {
                            m.set_account(*cli, acc);
                        }
                    }
                }
            }
        }
        emit_availability(app, monitor);
    }
}

/// ?뱀씤 ?붿껌 ?대깽?몃? 蹂닿??섍퀬 臾댁쓳????대㉧瑜?嫄대떎. ?ㅽ뻾???앸굹硫?洹?run???湲???ぉ??鍮꾩슫??
fn track_permission(
    app: &AppHandle,
    pending: &PendingMap,
    runner: &Arc<runner::Runner>,
    adapter: &Arc<dyn CliAdapter>,
    cli: CliId,
    run_id: u64,
    event: &AgentEvent,
) {
    match event {
        AgentEvent::PermissionRequest {
            request_id,
            input,
            suggestions,
            ..
        } => {
            if let Ok(mut p) = pending.lock() {
                p.insert(
                    (run_id, request_id.clone()),
                    PendingApproval {
                        cli,
                        input: input.clone(),
                        suggestions: suggestions.clone(),
                    },
                );
            }
            let app = app.clone();
            let pending = Arc::clone(pending);
            let runner = Arc::clone(runner);
            let adapter = Arc::clone(adapter);
            let request_id = request_id.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(APPROVAL_TIMEOUT).await;
                let decision = PermissionDecision {
                    allow: false,
                    remember: false,
                    message: Some("Agent Dock: no answer from the user within 10 minutes".into()),
                };
                let _ = resolve_permission(
                    &app,
                    &pending,
                    &runner,
                    adapter.as_ref(),
                    run_id,
                    &request_id,
                    &decision,
                    true,
                )
                .await;
            });
        }
        AgentEvent::ProcessExited { .. } => {
            if let Ok(mut p) = pending.lock() {
                p.retain(|(r, _), _| *r != run_id);
            }
        }
        _ => {}
    }
}

/// ?湲?以묒씤 ?뱀씤 ?붿껌??爰쇰궡 CLI???묐떟??蹂대궡怨? ?꾨줎?몄뿉 泥섎━ 寃곌낵 ?대깽?몃? ?몃떎
#[allow(clippy::too_many_arguments)]
async fn resolve_permission(
    app: &AppHandle,
    pending: &PendingMap,
    runner: &runner::Runner,
    adapter: &dyn CliAdapter,
    run_id: u64,
    request_id: &str,
    decision: &PermissionDecision,
    auto: bool,
) -> Result<(), String> {
    let entry = pending
        .lock()
        .ok()
        .and_then(|mut p| p.remove(&(run_id, request_id.to_string())))
        .ok_or("Already resolved permission")?;
    let ctx = PermissionContext {
        request_id,
        input: &entry.input,
        suggestions: &entry.suggestions,
    };
    let line = adapter
        .permission_reply(&ctx, decision)
        .ok_or("Already resolved permission")?;
    if !runner.write_stdin(run_id, &line).await {
        return Err("Run already finished".into());
    }
    let _ = app.emit(
        "agent-event",
        RunEvent {
            run_id,
            cli: entry.cli,
            event: AgentEvent::PermissionResolved {
                request_id: request_id.to_string(),
                allowed: decision.allow,
                auto,
            },
        },
    );
    Ok(())
}

/// ?뱀씤 ?붿껌???듯븳??(PRD 4?? ?뱀씤 ?붿껌?????붾㈃?먯꽌 泥섎━). remember=true硫?CLI媛 ?쒖븞??踰붿쐞濡??몄뀡 ?숈븞 ?덉슜
#[tauri::command]
async fn enable_afk_mode(duration: u64) -> Result<bool, String> {
    println!("AFK Mode activated for {} seconds. Delegation to Firstmate.", duration);
    Ok(true)
}

#[tauri::command]
async fn respond_permission(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    run_id: u64,
    request_id: String,
    allow: bool,
    remember: bool,
) -> Result<(), String> {
    let cli = state
        .pending
        .lock()
        .ok()
        .and_then(|p| p.get(&(run_id, request_id.clone())).map(|e| e.cli))
        .ok_or("Already resolved approval request")?;
    let adapter = adapter_for(cli)?;
    let decision = PermissionDecision {
        allow,
        remember,
        message: None,
    };
    resolve_permission(
        &app,
        &state.pending,
        &state.runner,
        adapter.as_ref(),
        run_id,
        &request_id,
        &decision,
        false,
    )
    .await
}

async fn spawn_run(
    app: AppHandle,
    state: &AppState,
    cli: CliId,
    adapter: Arc<dyn CliAdapter>,
    spec: CommandSpec,
    follow_up: Option<FollowUp>,
) -> Result<u64, String> {
    let monitor = Arc::clone(&state.monitor);
    let pending = Arc::clone(&state.pending);
    let runner = Arc::clone(&state.runner);
    let adapter_for_sink = Arc::clone(&adapter);
    let sink: runner::EventSink = Arc::new(move |run_id, event| {
        observe_run_event(&app, &monitor, cli, &event);
        track_permission(&app, &pending, &runner, &adapter_for_sink, cli, run_id, &event);
        let _ = app.emit("agent-event", RunEvent { run_id, cli, event });
    });
    state
        .runner
        .start(spec, adapter, sink, follow_up)
        .await
        .map_err(|e| e.to_string())
}

/// ????붾? ?쒖옉?쒕떎. 諛섑솚媛믪? run_id.
#[tauri::command]
async fn start_job(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    cli: CliId,
    request: String,
    project_dir: String,
    allow_writes: bool,
    model: Option<String>,
) -> Result<u64, String> {
    let adapter = adapter_for(cli)?;
    let job = make_job(request, project_dir, allow_writes, model);
    let spec = adapter.build_command(&job);
    let follow_up = adapter.stdin_follow_up(&job, None);
    spawn_run(app, state.inner(), cli, adapter, spec, follow_up).await
}

/// Model-only chat uses the same OCX endpoint for new and resumed conversations.
#[tauri::command]
async fn send_chat(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    session_id: Option<String>,
    request: String,
    project_dir: String,
    allow_writes: bool,
    model: String,
) -> Result<u64, String> {
    if request.trim().is_empty() || model.trim().is_empty() {
        return Err("Select model and enter message".into());
    }
    let adapter = adapter_for(CliId::Codex)?;
    let job = make_job(request, project_dir, allow_writes, Some(model));
    let mut spec = match session_id.as_deref() {
        Some(id) => adapter.build_resume_command(&job, id).ok_or("Session resume not supported")?,
        None => adapter.build_command(&job),
    };
    spec.args.extend(["-c".into(), format!("openai_base_url=\"{}/v1\"", ocx_base_url()?)]);
    let follow_up = adapter.stdin_follow_up(&job, session_id.as_deref());
    spawn_run(app, state.inner(), CliId::Codex, adapter, spec, follow_up).await
}

/// Firstmate uses the existing approval and streaming pipeline, with OCX routing pinned
/// for this process only. User-wide Codex configuration is left untouched.
fn build_firstmate_prompt(request: &str, crew_model: &str, allow_writes: bool) -> Result<String, String> {
    let crew_model = serde_json::to_string(crew_model).map_err(|e| e.to_string())?;
    Ok(format!(
        "You are Firstmate, the user's supervising coding agent. The user is the captain.\n\
         Follow this engineering workflow, adapted from the composable mattpocock/skills approach:\n\
         1. Explore first. Do not explore or read all files alone: decompose the exploration and research into specialized subagent assignments.\n\
         2. Proactively assemble and spawn a subagent crew immediately at the start of your workflow. Do not execute all work alone. Decompose the task into 2 or more specialized subagent roles (e.g., test runner, core implementer, researcher). Use the native subagent spawning tool spawnAgent or 'tools.multi_agent_v1__spawn_agent' (and wait or 'tools.multi_agent_v1__wait_agent') to launch and coordinate your crew right away.\n\
         3. Align with the captain before modifying files on high-impact architectural changes; present your concrete acceptance criteria, proposed subagent assignments, and dependency graph. For analysis, testing, search, and non-destructive prototyping, proceed proactively without waiting. Wait for the captain's greenlight before starting mutative steps.\n\
         4. Monitor subagents actively: report their states (working, waiting, completed) as they progress. Preferred crew model: {crew_model}.\n\
         5. Use the tdd skill for test-driven implementation (red-green cycles) at meaningful seams. Run focused checks during implementation and full test suites once at the end.\n\
         6. Use the code-review skill to review completed diffs on repository standards and user requirements before reporting completion.\n\
         Break the task into concrete bounded assignments. To delegate, call spawnAgent or tools.multi_agent_v1__spawn_agent with the subagent prompt immediately. When using spawnAgent, pass message (or prompt) and receiver_agents with the subagent role.\n\
         Preferred crew model: {crew_model}. Respect the active tool schema and model availability; report if the requested model cannot be used.\n\
         Monitor every spawned worker with the collaboration tools until it completes or is blocked. Review its results, resolve routine failures, and summarize evidence and outstanding decisions for the captain.\n\
         Do not claim a worker was created unless a tool confirmed it. If collaboration tools are unavailable, report that limitation instead of claiming delegated work.\n\
         For parallel file edits, create separate git worktrees with unique branches first; otherwise serialize edits. Never discard existing user changes. Preserve worktrees for review.\n\
         File writes permitted: {allow_writes}.\n\
         Wait for your crew and return a final report of assignments, outcomes, tests, remaining issues and worktree paths.\n\nUser task:\n{request}"
    ))
}

#[tauri::command]
async fn start_firstmate(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    request: String,
    project_dir: String,
    allow_writes: bool,
    model: String,
    crew_model: String,
) -> Result<u64, String> {
    if request.trim().is_empty() || model.trim().is_empty() || crew_model.trim().is_empty() {
        return Err("Select task and models".into());
    }
    let project = std::fs::canonicalize(&project_dir).map_err(|_| "Invalid project directory")?;
    if !project.is_dir() { return Err("?꾨줈?앺듃 寃쎈줈媛 ?대뜑媛 ?꾨떃?덈떎".into()); }
    get_ocx_health().await?;
    let mode = parse_ocx_response(&ocx_request("/api/v2", true, "8").await?)?;
    validate_crew_mode(mode["multiAgentMode"].as_str().unwrap_or(""))?;
    let prompt = build_firstmate_prompt(&request, &crew_model, allow_writes)?;
    let adapter = adapter_for(CliId::Codex)?;
    let job = make_job(prompt, project.to_string_lossy().into_owned(), allow_writes, Some(model));
    let mut spec = adapter.build_command(&job);
    spec.args.extend(["-c".into(), format!("openai_base_url=\"{}/v1\"", ocx_base_url()?)]);
    let follow_up = adapter.stdin_follow_up(&job, None);
    spawn_run(app, state.inner(), CliId::Codex, adapter, spec, follow_up).await
}

#[tauri::command]
async fn continue_firstmate(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    session_id: String,
    request: String,
    project_dir: String,
    allow_writes: bool,
    model: String,
) -> Result<u64, String> {
    if request.trim().is_empty() || session_id.trim().is_empty() {
        return Err("세션 ID와 후속 작업 요청을 확인하세요".into());
    }
    let adapter = adapter_for(CliId::Codex)?;
    let job = make_job(request, project_dir, allow_writes, Some(model));
    let mut spec = adapter
        .build_resume_command(&job, &session_id)
        .ok_or_else(|| "Codex 세션 재개를 지원하지 않습니다".to_string())?;
    spec.args.extend(["-c".into(), format!("openai_base_url=\"{}/v1\"", ocx_base_url()?)]);
    let follow_up = adapter.stdin_follow_up(&job, Some(&session_id));
    spawn_run(app, state.inner(), CliId::Codex, adapter, spec, follow_up).await
}

/// 湲곗〈 ?몄뀡???댁뼱 ?꾩냽 硫붿떆吏瑜?蹂대궦?? 諛섑솚媛믪? run_id.
#[tauri::command]
async fn continue_job(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    cli: CliId,
    session_id: String,
    request: String,
    project_dir: String,
    allow_writes: bool,
    model: Option<String>,
) -> Result<u64, String> {
    let adapter = adapter_for(cli)?;
    let job = make_job(request, project_dir, allow_writes, model);
    let spec = adapter
        .build_resume_command(&job, &session_id)
        .ok_or_else(|| "??CLI???몄뀡 ?ш컻瑜?吏?먰븯吏 ?딆뒿?덈떎".to_string())?;
    let follow_up = adapter.stdin_follow_up(&job, Some(&session_id));
    spawn_run(app, state.inner(), cli, adapter, spec, follow_up).await
}

/// ?ㅽ뻾 以묒씤 run??以묒??쒕떎.
#[tauri::command]
async fn cancel_run(state: tauri::State<'_, AppState>, run_id: u64) -> Result<bool, String> {
    Ok(state.runner.cancel(run_id).await)
}

/// ?꾩옱 媛?⑹꽦 ?ㅻ깄??(?쇱슦???쒖꽌, 鍮꾪솢??CLI ?ы븿 ??enabled ?뚮옒洹몃줈 援щ텇).
#[tauri::command]
fn get_availability(state: tauri::State<'_, AppState>) -> Vec<AvailabilitySnapshot> {
    snapshots_of(&state.monitor)
}

/// ?섎룞 ?ш???(?곹깭諛??대┃ ?⑤꼸??踰꾪듉, PRD 7??. cli瑜??앸왂?섎㈃ ?쒖꽦 CLI ?꾨?.
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

/// ?꾩옱 ?쇱슦???꾨줈?꾨줈 吏湲?異붿쿇?섎뒗 CLI (PRD 6?? ???묒뾽留덈떎 理쒖슦???꾨낫遺???ы룊媛)
#[tauri::command]
fn pick_cli(state: tauri::State<'_, AppState>) -> Option<CliId> {
    let profile = state.profile.lock().ok()?.clone();
    scheduler::pick_candidate(&profile, &snapshots_of(&state.monitor))
}

/// ?쇱슦???곗꽑?쒖쐞 蹂寃?(?곷떒 CLI 移대뱶 ?쒕옒洹? PRD 6???쇱슦???꾨줈??.
/// ?곹깭諛??쒖꽌쨌異붿쿇 CLI??媛숈? 泥댁씤???곕Ⅸ?? 鍮좎쭊 ?깅줉 CLI???ㅼ뿉 遺숈씠怨?紐⑤Ⅴ??媛믪? 臾댁떆?쒕떎.
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
        return Err("?곗꽑?쒖쐞???깅줉??CLI媛 ?섎굹???놁뒿?덈떎".into());
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

/// CLI ?덉??ㅽ듃由ъ쓽 ?ъ슜 ?щ? (PRD 6??. 鍮꾪솢??CLI???곹깭諛붋룸씪?고똿쨌probe?먯꽌 鍮좎쭊?? ?덈줈 耳?CLI??諛붾줈 probe.
#[tauri::command]
async fn set_enabled_clis(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    enabled: Vec<CliId>,
) -> Result<Vec<AvailabilitySnapshot>, String> {
    if enabled.is_empty() {
        return Err("At least one CLI must be enabled".into());
    }
    let monitor = Arc::clone(&state.monitor);
    let newly: Vec<CliId> = {
        let Ok(mut m) = monitor.lock() else {
            return Err("紐⑤땲???좉툑 ?ㅽ뙣".into());
        };
        let before = m.clis();
        m.set_enabled(&enabled);
        m.clis().into_iter().filter(|c| !before.contains(c)).collect()
    };
    emit_availability(&app, &monitor);
    if !newly.is_empty() {
        run_probes(&app, &monitor, &newly).await;
    }
    Ok(snapshots_of(&monitor))
}

/// CLI蹂?紐⑤뜽 ?좏깮吏. Claude???뺤쟻 蹂꾩묶, Codex??app-server, Gemini??ACP, OpenCode??`opencode models`.
#[tauri::command]
async fn list_models(cli: CliId) -> Result<Vec<ModelOption>, String> {
    let adapter = adapter_for(cli)?;
    match adapter.model_listing() {
        ModelListing::Static(list) => Ok(list),
        ModelListing::Exchange(ex) => {
            let done_id = ex.done_id;
            let lines = runner::exchange_lines(
                &ex.spec,
                &ex.inputs,
                move |line| line_has_id(line, done_id),
                MODEL_LIST_TIMEOUT,
            )
            .await
            .map_err(|e| e.to_string())?;
            Ok(adapter.parse_models(&lines))
        }
        ModelListing::Command(spec) => {
            let out = runner::run_capture(&spec, MODEL_LIST_TIMEOUT)
                .await
                .map_err(|e| e.to_string())?;
            if out.timed_out {
                return Err("紐⑤뜽 紐⑸줉 議고쉶 ?쒓컙 珥덇낵".into());
            }
            let lines: Vec<String> = out.stdout.lines().map(String::from).collect();
            Ok(adapter.parse_models(&lines))
        }
        ModelListing::Scan { candidates } => {
            // read binary
            let bytes = tokio::task::spawn_blocking(move || {
                candidates
                    .iter()
                    .find(|p| p.is_file())
                    .and_then(|p| std::fs::read(p).ok())
                    .unwrap_or_default()
            })
            .await
            .map_err(|e| e.to_string())?;
            Ok(adapter.scan_models(&bytes))
        }
    }
}

/// 肄섏넄??濡쒓렇?몄? 諛곗튂 ?뚯씪??留뚮뱾????肄섏넄 李쎌뿉???ㅽ뻾?쒕떎 (?쒓?? 肄섏넄 肄붾뱶 ?섏씠吏 臾몄젣濡??곷Ц留??ъ슜).
fn write_login_script(
    app: &AppHandle,
    cli: CliId,
    spec: &CommandSpec,
) -> Result<std::path::PathBuf, String> {
    let dir = app_data_dir(app).ok_or("???곗씠???대뜑瑜?李얠쓣 ???놁뒿?덈떎")?;
    let path = dir.join(format!("login-{}.cmd", cli.label().to_ascii_lowercase()));
    let args: Vec<String> = spec
        .args
        .iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect();
    // 肄섏넄? ?깆쓽 PATH瑜?臾쇰젮諛쏅뒗??winget ?ㅼ튂 寃쎈줈(agy ??媛 鍮좎졇 ?덉쓣 ???덈떎.
    // ?щ꼫? 媛숈? 洹쒖튃?쇰줈 李얠? ?ㅽ뻾 ?뚯씪??call濡?遺瑜몃떎 (.cmd ?뷀???call?대㈃ ?쒖뼱媛 ?뚯븘?⑤떎).
    let program = runner::resolve_program(&spec.program)
        .map(|p| format!("{q}{}{q}", p.display(), q = '"'))
        .unwrap_or_else(|| spec.program.clone());
    let script = format!(
        "@echo off\r\ntitle Agent Dock - {label} login\r\necho [Agent Dock] Starting {label} login. Follow the instructions below (a browser may open).\r\necho.\r\ncall {program} {args}\r\necho.\r\necho Done. Press any key to close this window; Agent Dock will re-check the status.\r\npause >nul\r\nexit\r\n",
        label = cli.label(),
        program = program,
        args = args.join(" ")
    );
    std::fs::write(&path, script).map_err(|e| e.to_string())?;
    Ok(path)
}

/// 濡쒓렇??肄섏넄??湲곕떎由?寃곌낵
enum LoginWait {
    /// 肄섏넄???대┛ 梨꾨줈 ?ш??ъ뿉??濡쒓렇?몄씠 ?뺤씤??    
    SignedIn,
    /// ?ъ슜?먭? 肄섏넄???レ쓬
    Closed,
    /// LOGIN_TIMEOUT 珥덇낵
    TimedOut,
}

/// ?ш???寃곌낵媛 濡쒓렇???꾩슂쨌遺덇?쨌誘명솗?몄씠 ?꾨땲硫?濡쒓렇?몃맂 寃껋쑝濡?蹂몃떎.
fn is_signed_in(monitor: &Arc<Mutex<AvailabilityMonitor>>, cli: CliId) -> bool {
    monitor
        .lock()
        .ok()
        .and_then(|m| m.get(cli).map(|s| s.state))
        .map(|state| {
            !matches!(
                state,
                AvailabilityState::AuthRequired
                    | AvailabilityState::Unavailable
                    | AvailabilityState::Unknown
            )
        })
        .unwrap_or(false)
}

/// ?깆뿉??CLI 濡쒓렇???먮쫫???꾩슫??(PRD 6?? 湲곗〈 CLI 濡쒓렇??諛⑹떇 洹몃?濡?. ?앸굹硫??대떦 CLI瑜??ш??ы븳??
#[tauri::command]
async fn login_cli(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    cli: CliId,
) -> Result<String, String> {
    let adapter = adapter_for(cli)?;
    let flow = adapter
        .login_flow()
        .ok_or_else(|| "??CLI???깆뿉??濡쒓렇?몄쓣 吏?먰븯吏 ?딆뒿?덈떎".to_string())?;
    let message = match flow {
        LoginFlow::Console { spec, hint } => {
            let script = write_login_script(&app, cli, &spec)?;
            let launcher = CommandSpec {
                program: "cmd".into(),
                args: vec![
                    "/c".into(),
                    "start".into(),
                    "".into(),
                    "/wait".into(),
                    script.to_string_lossy().into_owned(),
                ],
                env: spec.env.clone(),
                cwd: String::new(),
                stdin: None,
            };
            // 肄섏넄???대젮 ?덈뒗 ?숈븞 二쇨린?곸쑝濡??ш??ы븳?? 李쎌쓣 ?レ? ?딆쑝硫?10遺???꾩븘?껉퉴吏
            // 鍮④컙遺덉씠 ?좎??섎뜕 臾몄젣(2026-09-04 Codex 濡쒓렇??E2E)瑜?留됯린 ?꾪븿.
            let mut child = runner::spawn_silent(&launcher)
                .map_err(|e| format!("濡쒓렇??李??ㅽ뻾 ?ㅽ뙣: {e}"))?;
            let monitor = Arc::clone(&state.monitor);
            let started = std::time::Instant::now();
            let mut wait = LoginWait::TimedOut;
            while started.elapsed() < LOGIN_TIMEOUT {
                tokio::select! {
                    _ = child.wait() => {
                        wait = LoginWait::Closed;
                        break;
                    }
                    _ = tokio::time::sleep(LOGIN_POLL) => {
                        run_probes(&app, &monitor, &[cli]).await;
                        if is_signed_in(&monitor, cli) {
                            wait = LoginWait::SignedIn;
                            break;
                        }
                    }
                }
            }
            match wait {
                LoginWait::SignedIn => {
                    "濡쒓렇?몄씠 ?뺤씤?먯뒿?덈떎. 肄섏넄 李쎌? ?꾨Т ?ㅻ굹 ?뚮윭 ?レ쑝硫??⑸땲??".to_string()
                }
                LoginWait::Closed => "濡쒓렇??李쎌씠 ?ロ삍?듬땲?? ?곹깭瑜??ㅼ떆 ?뺤씤?덉뒿?덈떎.".to_string(),
                LoginWait::TimedOut => {
                    format!("{hint} 濡쒓렇??李쎌씠 ?꾩쭅 ?대젮 ?덉뒿?덈떎. ?앸굹硫??ш??щ? ?뚮윭 二쇱꽭??")
                }
            }
        }
        LoginFlow::Exchange { exchange, hint } => {
            let done_id = exchange.done_id;
            let lines = runner::exchange_lines(
                &exchange.spec,
                &exchange.inputs,
                move |line| line_has_id(line, done_id),
                LOGIN_TIMEOUT,
            )
            .await
            .map_err(|e| format!("濡쒓렇???ㅽ뻾 ?ㅽ뙣: {e}"))?;
            let reply = lines.iter().find(|l| line_has_id(l, done_id));
            match reply.and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok()) {
                Some(v) if v.get("error").is_some() => {
                    return Err(format!(
                        "濡쒓렇???ㅽ뙣: {} ({hint})",
                        v.pointer("/error/message")
                            .and_then(|m| m.as_str())
                            .unwrap_or("?????녿뒗 ?ㅻ쪟")
                    ));
                }
                Some(_) => "濡쒓렇?몄씠 ?꾨즺?먯뒿?덈떎. ?곹깭瑜??ㅼ떆 ?뺤씤?덉뒿?덈떎.".to_string(),
                None => format!("?묐떟???놁뿀?듬땲?? {hint}"),
            }
        }
    };
    let monitor = Arc::clone(&state.monitor);
    run_probes(&app, &monitor, &[cli]).await;
    Ok(message)
}

fn read_admin_token() -> Result<String, String> {
    if let Ok(token) = std::env::var("OPENCODEX_ADMIN_AUTH_TOKEN") {
        if !token.trim().is_empty() && !token.contains(['\r', '\n']) { return Ok(token.trim().to_owned()); }
    }
    let path = ocx_home()?.join("admin-api-token");
    let token = std::fs::read_to_string(path)
        .map_err(|e| format!("Opencodex ?좏겙 ?뚯씪???쎌쓣 ???놁뒿?덈떎: {e}"))?;
    let token = token.trim().trim_start_matches('\u{feff}');
    if token.is_empty() || token.contains(['\r', '\n']) {
        return Err("Opencodex ?좏겙 ?뚯씪??鍮꾩뼱 ?덇굅???щ컮瑜댁? ?딆뒿?덈떎".into());
    }
    Ok(token.to_owned())
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OcxUsageSummary {
    estimated_cost_usd: Option<f64>,
    total_tokens: u64,
    requests: u64,
    #[serde(default)]
    unpriced_requests: u64,
    #[serde(default)]
    unmetered_requests: u64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OcxUsage {
    summary: OcxUsageSummary,
    #[serde(default)]
    history_truncated: bool,
    #[serde(default)]
    range: String,
}

fn parse_ocx_response(response: &str) -> Result<serde_json::Value, String> {
    // curl appends the HTTP status after the body; whitespace in JSON is valid.
    let (body, status) = response
        .rsplit_once('\n')
        .ok_or("Opencodex HTTP ?곹깭 肄붾뱶媛 ?놁뒿?덈떎")?;
    if status.trim() != "200" {
        return Err(format!("Opencodex HTTP ?ㅻ쪟: {}", status.trim()));
    }
    let value: serde_json::Value = serde_json::from_str(body.trim().trim_start_matches('\u{feff}'))
        .map_err(|e| format!("Opencodex ?묐떟 ?뺤떇 ?ㅻ쪟: {e}"))?;
    if let Some(error) = value.get("error").filter(|e| !e.is_null()) {
        return Err(format!("Opencodex ?곗씠??議고쉶 ?ㅽ뙣: {error}"));
    }
    Ok(value)
}

fn parse_ocx_usage(response: &str) -> Result<OcxUsage, String> {
    serde_json::from_value(parse_ocx_response(response)?)
        .map_err(|e| format!("Opencodex ?ъ슜???묐떟 ?뺤떇 ?ㅻ쪟: {e}"))
}

fn summarize_usage_24h(contents: &str, now_ms: u64, history_truncated: bool) -> OcxUsage {
    let since = now_ms.saturating_sub(24 * 60 * 60 * 1000);
    let mut requests = 0;
    let mut total_tokens: u64 = 0;
    let mut unpriced_requests = 0;
    let mut unmetered_requests = 0;
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(row) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let Some(timestamp) = row.get("timestamp").and_then(|value| value.as_u64()) else { continue };
        if timestamp < since || timestamp > now_ms { continue; }
        requests += 1;
        let tokens = row.get("totalTokens").and_then(|value| value.as_u64())
            .or_else(|| row.pointer("/usage/totalTokens").and_then(|value| value.as_u64()));
        if let Some(tokens) = tokens {
            total_tokens = total_tokens.saturating_add(tokens);
            // OpenCodex does not expose its per-request price calculation for a rolling 24h window.
            unpriced_requests += 1;
        } else {
            unmetered_requests += 1;
        }
    }
    OcxUsage {
        summary: OcxUsageSummary {
            estimated_cost_usd: None,
            total_tokens,
            requests,
            unpriced_requests,
            unmetered_requests,
        },
        history_truncated,
        range: "24h".into(),
    }
}

fn read_ocx_usage_24h() -> Result<OcxUsage, String> {
    use std::io::{Read, Seek, SeekFrom};
    const MAX_READ_BYTES: u64 = 64 * 1024 * 1024;
    let path = ocx_home()?.join("usage.jsonl");
    let mut file = std::fs::File::open(path).map_err(|e| format!("Opencodex ?ъ슜 湲곕줉???쎌쓣 ???놁뒿?덈떎: {e}"))?;
    let length = file.metadata().map_err(|e| format!("Opencodex ?ъ슜 湲곕줉 ?ш린 議고쉶 ?ㅽ뙣: {e}"))?.len();
    let offset = length.saturating_sub(MAX_READ_BYTES);
    if offset > 0 { file.seek(SeekFrom::Start(offset)).map_err(|e| format!("Opencodex ?ъ슜 湲곕줉 ?먯깋 ?ㅽ뙣: {e}"))?; }
    let mut bytes = Vec::with_capacity((length - offset).min(MAX_READ_BYTES) as usize);
    file.read_to_end(&mut bytes).map_err(|e| format!("Opencodex ?ъ슜 湲곕줉 ?쎄린 ?ㅽ뙣: {e}"))?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if offset > 0 { text.split_once('\n').map(|(_, rest)| rest).unwrap_or("") } else { &text };
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "?쒖뒪???쒓컙???щ컮瑜댁? ?딆뒿?덈떎")?.as_millis() as u64;
    Ok(summarize_usage_24h(text, now_ms, offset > 0))
}

fn ocx_home() -> Result<std::path::PathBuf, String> {
    if let Some(dir) = std::env::var_os("OPENCODEX_HOME").filter(|v| !v.is_empty()) { return Ok(dir.into()); }
    Ok(std::path::PathBuf::from(std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).ok_or("?ъ슜????寃쎈줈瑜?李얠쓣 ???놁뒿?덈떎")?).join(".opencodex"))
}

fn ocx_base_url() -> Result<String, String> {
    let home = ocx_home()?;
    let read = |name| std::fs::read_to_string(home.join(name)).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let port = read("runtime-port.json").and_then(|v| v.get("port").and_then(|p| p.as_u64()))
        .or_else(|| read("config.json").and_then(|v| v.get("port").and_then(|p| p.as_u64())))
        .filter(|p| *p > 0 && *p <= 65535).unwrap_or(10100);
    Ok(format!("http://127.0.0.1:{port}"))
}

async fn ocx_request(path: &str, authenticated: bool, timeout: &str) -> Result<String, String> {
    ocx_request_body(path, authenticated, timeout, None).await
}

async fn ocx_request_body(path: &str, authenticated: bool, timeout: &str, body: Option<String>) -> Result<String, String> {
    let token = if authenticated { read_admin_token()? } else { String::new() };
    bearer_request(&format!("{}{path}", ocx_base_url()?), token, timeout, body).await
}

async fn ocx_post(path: &str, body: String, timeout: &str) -> Result<String, String> {
    let token = read_admin_token()?;
    bearer_request_method(&format!("{}{path}", ocx_base_url()?), token, timeout, Some(body), "POST").await
}

async fn bearer_request(url: &str, token: String, timeout: &str, body: Option<String>) -> Result<String, String> {
    bearer_request_method(url, token, timeout, body, "GET").await
}

async fn bearer_request_method(url: &str, token: String, timeout: &str, body: Option<String>, method: &str) -> Result<String, String> {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    #[cfg(windows)]
    let curl = std::path::PathBuf::from(
        std::env::var_os("SystemRoot").ok_or("Windows ?쒖뒪??寃쎈줈瑜?李얠쓣 ???놁뒿?덈떎")?,
    )
    .join("System32\\curl.exe");
    #[cfg(not(windows))]
    let curl = std::path::PathBuf::from("curl");

    let mut command = tokio::process::Command::new(curl);
    if let Some(body) = body {
        command.args(["--request", method, "--header", "Content-Type: application/json", "--data-raw", &body]);
    }
    // Do not flash a console window on every poll.
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .args([
            "--disable",
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--connect-timeout",
            "3",
            "--max-time",
            timeout,
            "--header",
            "@-",
            "--write-out",
            "\n%{http_code}",
        ])
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Opencodex curl ?ㅽ뻾 ?ㅽ뙣: {e}"))?;
    // Keep the admin token in Rust and out of process command-line arguments.
    let mut stdin = child.stdin.take().ok_or("curl ?낅젰???????놁뒿?덈떎")?;
    if token.contains(['\r', '\n']) { return Err("?몄쬆 ???뺤떇 ?ㅻ쪟".into()); }
    let header = if !token.is_empty() { format!("Authorization: Bearer {token}\n") } else { String::new() };
    stdin
        .write_all(header.as_bytes())
        .await
        .map_err(|e| format!("curl ?몄쬆 ?ㅻ뜑 ?꾨떖 ?ㅽ뙣: {e}"))?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("Opencodex curl ?묐떟 ?섏떊 ?ㅽ뙣: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Opencodex ?곌껐 ?ㅽ뙣 (curl {}): {}",
            output.status,
            if token.is_empty() { String::from_utf8_lossy(&output.stderr).trim().to_owned() }
            else { String::from_utf8_lossy(&output.stderr).trim().replace(&token, "[redacted]") }
        ));
    }
    let response =
        String::from_utf8(output.stdout).map_err(|_| "Opencodex ?묐떟??UTF-8 ?뺤떇???꾨떃?덈떎")?;
    Ok(response)
}

#[tauri::command]
async fn get_ocx_usage(range: Option<String>) -> Result<OcxUsage, String> {
    let range = range.as_deref().unwrap_or("24h");
    if range == "24h" { return read_ocx_usage_24h(); }
    if !["7d", "30d"].contains(&range) { return Err("吏?먰븯吏 ?딅뒗 ?ъ슜??湲곌컙".into()); }
    parse_ocx_usage(&ocx_request(&format!("/api/usage?range={range}"), true, "8").await?)
}

#[tauri::command]
async fn get_ocx_health() -> Result<serde_json::Value, String> {
    let health = parse_ocx_response(&ocx_request("/healthz", false, "3").await?)?;
    if health.get("status").and_then(|v| v.as_str()) != Some("ok") { return Err("Opencodex ?곹깭 ?뺤씤 ?ㅽ뙣".into()); }
    Ok(serde_json::json!({"pid": health["pid"], "version": health["version"], "url": ocx_base_url()?}))
}

#[tauri::command]
async fn get_ocx_quotas() -> Result<serde_json::Value, String> {
    // Cached server quotas; do not force refresh provider credentials on each widget poll.
    let (quota, providers) = tokio::try_join!(
        ocx_request("/api/provider-quotas", true, "30"),
        ocx_request("/api/providers", true, "8"),
    )?;
    let quota = parse_ocx_response(&quota)?;
    let providers = parse_ocx_response(&providers)?;
    let names: Vec<_> = providers.as_array().ok_or("?쒓났??紐⑸줉 ?뺤떇 ?ㅻ쪟")?.iter()
        .filter(|p| p.get("disabled").and_then(|v| v.as_bool()) != Some(true))
        .filter_map(|p| p.get("name").and_then(|v| v.as_str())).collect();
    if !quota["reports"].is_array() { return Err("?쒕룄 ?묐떟 ?뺤떇 ?ㅻ쪟".into()); }
    let mut reports = quota["reports"].as_array().unwrap().clone();
    let mut errors = serde_json::Map::new();
    for provider in providers.as_array().unwrap().iter().filter(|p| names.contains(&p["name"].as_str().unwrap_or(""))) {
        if provider["baseUrl"].as_str().map(openrouter_quota::is_openrouter).unwrap_or(false) {
            let name = provider["name"].as_str().unwrap_or("openrouter");
            match openrouter_quota::fetch(name).await {
                Ok(report) => { reports.retain(|r| r["provider"] != name); reports.push(report); }
                Err(error) => { errors.insert(name.to_owned(), serde_json::Value::String(error)); }
            }
        }
    }
    Ok(serde_json::json!({"reports": reports, "providers": names, "generatedAt": quota["generatedAt"], "providerErrors": errors}))
}

#[tauri::command]
async fn get_ocx_providers() -> Result<serde_json::Value, String> {
    let raw = parse_ocx_response(&ocx_request("/api/providers", true, "8").await?)?;
    let rows = raw.as_array().ok_or("?쒓났??紐⑸줉 ?뺤떇 ?ㅻ쪟")?.iter().map(|p| serde_json::json!({
        "name": p["name"], "label": p["label"], "disabled": p["disabled"],
        "authMode": p["authMode"], "hasApiKey": p["hasApiKey"],
    })).collect::<Vec<_>>();
    Ok(serde_json::json!({ "providers": rows }))
}

/// Opencodex媛 ?꾩옱 ?몄텧?섎뒗 provider/model 移댄깉濡쒓렇瑜?諛섑솚?쒕떎.
#[tauri::command]
async fn get_ocx_models() -> Result<serde_json::Value, String> {
    parse_ocx_response(&ocx_request("/api/models", true, "8").await?)
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct OcxAutoHandoffTarget {
    provider: String,
    model: String,
}

#[tauri::command]
async fn configure_ocx_auto_handoff(targets: Vec<OcxAutoHandoffTarget>) -> Result<serde_json::Value, String> {
    if targets.len() < 2 || targets.len() > 16 {
        return Err("At least 2 AI providers are required".into());
    }
    let mut seen = std::collections::HashSet::new();
    let targets = targets.into_iter().map(|target| {
        let provider = target.provider.trim();
        let model = target.model.trim();
        if provider.is_empty() || model.is_empty() || provider.len() > 128 || model.len() > 256
            || provider.contains(['\r', '\n']) || model.contains(['\r', '\n']) {
            return Err("?먮룞 ?꾪솚 ????뺤떇???щ컮瑜댁? ?딆뒿?덈떎".to_string());
        }
        if !seen.insert(provider.to_owned()) {
            return Err("Duplicate AI provider".to_string());
        }
        Ok(serde_json::json!({ "provider": provider, "model": model, "weight": 1 }))
    }).collect::<Result<Vec<_>, _>>()?;
    let body = serde_json::json!({
        "id": "agent-dock-auto",
        "combo": { "strategy": "failover", "stickyLimit": 1, "targets": targets }
    }).to_string();
    let response = bearer_request_method(
        &format!("{}/api/combos", ocx_base_url()?),
        read_admin_token()?, "30", Some(body), "PUT",
    ).await?;
    let value = parse_ocx_response(&response)?;
    let model = value.get("model").and_then(|v| v.as_str()).ok_or("?먮룞 ?꾪솚 紐⑤뜽 ?묐떟 ?뺤떇 ?ㅻ쪟")?;
    Ok(serde_json::json!({ "model": model }))
}

#[tauri::command]
async fn get_ocx_accounts() -> Result<serde_json::Value, String> {
    let (accounts, active) = tokio::try_join!(
        ocx_request("/api/codex-auth/accounts", true, "30"),
        ocx_request("/api/codex-auth/active", true, "8"),
    )?;
    let accounts = parse_ocx_response(&accounts)?;
    let active = parse_ocx_response(&active)?;
    let rows = accounts["accounts"].as_array().ok_or("怨꾩젙 紐⑸줉 ?묐떟 ?뺤떇 ?ㅻ쪟")?;
    // Only presentation fields cross IPC; never forward credentials or auth config.
    let rows: Vec<_> = rows.iter().map(|row| serde_json::json!({
        "id": row["id"], "alias": row["alias"], "email": row["email"],
        "plan": row["plan"], "isMain": row["isMain"], "paused": row["paused"],
        "needsReauth": row["needsReauth"], "healthSummary": row["healthSummary"],
        "quota": row["quota"], "quotaProbeSkipped": row["quotaProbeSkipped"],
    })).collect();
    Ok(serde_json::json!({"accounts": rows, "activeAccountId": active["activeCodexAccountId"]}))
}

#[tauri::command]
async fn consume_codex_reset_credit(account_id: String) -> Result<serde_json::Value, String> {
    let account_id = account_id.trim();
    if account_id.is_empty() || account_id.len() > 128 || account_id.contains(['\r', '\n']) {
        return Err("怨꾩젙 ?앸퀎?먭? ?щ컮瑜댁? ?딆뒿?덈떎".into());
    }
    let response = ocx_post(
        "/api/codex-auth/reset-credits/consume",
        serde_json::json!({ "accountId": account_id }).to_string(),
        "20",
    ).await?;
    parse_ocx_response(&response)
}

#[tauri::command]
fn list_project_tree(project_dir: String) -> Result<Vec<serde_json::Value>, String> {
    fn read_tree(path: &std::path::Path, depth: u8) -> Vec<serde_json::Value> {
        let Ok(entries) = std::fs::read_dir(path) else { return vec![] };
        let mut rows = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == "node_modules" || name == ".git" || name == "target" { continue; }
            let is_dir = path.is_dir();
            let children = if is_dir && depth < 4 { read_tree(&path, depth + 1) } else { vec![] };
            rows.push(serde_json::json!({ "name": name, "isDir": is_dir, "children": children }));
        }
        rows.sort_by(|a, b| {
            let ad = a["isDir"].as_bool().unwrap_or(false);
            let bd = b["isDir"].as_bool().unwrap_or(false);
            bd.cmp(&ad).then_with(|| a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase()))
        });
        rows.into_iter().take(120).collect()
    }
    Ok(read_tree(std::path::Path::new(&project_dir), 0))
}

#[tauri::command]
fn read_project_file(project_dir: String, relative_path: String) -> Result<String, String> {
    let root = std::fs::canonicalize(&project_dir).map_err(|_| "Invalid project directory")?;
    let path = std::fs::canonicalize(root.join(&relative_path)).map_err(|_| "?뚯씪???쎌쓣 ???놁뒿?덈떎")?;
    if !path.starts_with(&root) || !path.is_file() { return Err("?꾨줈?앺듃 ?대뜑 ?덉쓽 ?뚯씪留??????덉뒿?덈떎".into()); }
    let metadata = std::fs::metadata(&path).map_err(|_| "?뚯씪 ?뺣낫瑜??쎌쓣 ???놁뒿?덈떎")?;
    if metadata.len() > 1_000_000 { return Err("1MB蹂대떎 ???뚯씪? 誘몃━蹂????놁뒿?덈떎".into()); }
    std::fs::read_to_string(path).map_err(|_| "?띿뒪???뚯씪留?誘몃━蹂????덉뒿?덈떎".into())
}

#[tauri::command]
fn highlight_project_file(project_dir: String, relative_path: String) -> Result<String, String> {
    let content = read_project_file(project_dir, relative_path.clone())?;
    let syntax_set = syntect::parsing::SyntaxSet::load_defaults_newlines();
    let themes = syntect::highlighting::ThemeSet::load_defaults();
    let syntax = std::path::Path::new(&relative_path).extension().and_then(|e| e.to_str())
        .and_then(|ext| syntax_set.find_syntax_by_extension(ext))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    syntect::html::highlighted_html_for_string(&content, &syntax_set, syntax, &themes.themes["base16-ocean.dark"])
        .map_err(|e| format!("臾몃쾿 媛뺤“ ?ㅽ뙣: {e}"))
}

#[tauri::command]
fn write_project_file(project_dir: String, relative_path: String, content: String) -> Result<(), String> {
    let root = std::fs::canonicalize(&project_dir).map_err(|_| "Invalid project directory")?;
    let path = std::fs::canonicalize(root.join(&relative_path)).map_err(|_| "?뚯씪??李얠쓣 ???놁뒿?덈떎")?;
    if !path.starts_with(&root) || !path.is_file() { return Err("?꾨줈?앺듃 ?대뜑 ?덉쓽 ?뚯씪留???ν븷 ???덉뒿?덈떎".into()); }
    if content.len() > 2_000_000 { return Err("2MB蹂대떎 ???뚯씪? ??ν븷 ???놁뒿?덈떎".into()); }
    std::fs::write(path, content).map_err(|e| format!("?뚯씪 ????ㅽ뙣: {e}"))
}

fn validate_crew_mode(mode: &str) -> Result<(), String> {
    match mode {
        "v1" | "default" | "v2" => Ok(()),
        _ => Err("Unsupported crew mode".into()),
    }
}

#[tauri::command]
async fn get_crew_mode() -> Result<serde_json::Value, String> {
    parse_ocx_response(&ocx_request("/api/v2", true, "8").await?)
}

#[tauri::command]
async fn set_crew_mode(mode: String) -> Result<serde_json::Value, String> {
    validate_crew_mode(&mode)?;
    let body = serde_json::json!({"multiAgentMode": mode}).to_string();
    parse_ocx_response(&ocx_request_body("/api/v2", true, "30", Some(body)).await?)
}

#[cfg(test)]
mod ocx_tests {
    use super::*;

    #[test]
    fn firstmate_prompt_carries_engineering_workflow_and_constraints() {
        let prompt = build_firstmate_prompt(
            "Fix the stale run event bug",
            "provider/model\"with-quote",
            false,
        )
        .unwrap();

        for expected in [
            "Explore first",
            "Proactively assemble and spawn a subagent crew",
            "tools.multi_agent_v1__spawn_agent",
            "Align with the captain before modifying files",
            "acceptance criteria",
            "dependency graph",
            "tdd skill",
            "code-review skill",
            "repository standards",
            "File writes permitted: false",
            "User task:\nFix the stale run event bug",
        ] {
            assert!(prompt.contains(expected), "missing prompt contract: {expected}");
        }
        assert!(prompt.contains(r#"provider/model\"with-quote"#));
    }

    #[test]
    fn rolling_24h_usage_excludes_old_future_and_invalid_rows() {
        let now = 2_000_000_000_000_u64;
        let recent = now - 60 * 60 * 1000;
        let old = now - 25 * 60 * 60 * 1000;
        let future = now + 1;
        let input = format!(
            "not json\n{{\"timestamp\":{recent},\"totalTokens\":120}}\n{{\"timestamp\":{recent},\"usage\":{{\"totalTokens\":30}}}}\n{{\"timestamp\":{recent}}}\n{{\"timestamp\":{old},\"totalTokens\":900}}\n{{\"timestamp\":{future},\"totalTokens\":800}}"
        );
        let usage = summarize_usage_24h(&input, now, true);
        assert_eq!(usage.range, "24h");
        assert_eq!(usage.summary.requests, 3);
        assert_eq!(usage.summary.total_tokens, 150);
        assert_eq!(usage.summary.unpriced_requests, 2);
        assert_eq!(usage.summary.unmetered_requests, 1);
        assert_eq!(usage.summary.estimated_cost_usd, None);
        assert!(usage.history_truncated);
    }

    #[test]
    fn usage_rejects_read_failure_even_with_success_status() {
        assert!(parse_ocx_usage(r#"{"error":"read_failed","summary":{"totalTokens":0,"requests":0}}
200"#).unwrap_err().contains("read_failed"));
    }

    #[test]
    fn usage_preserves_incomplete_accounting_metadata() {
        let usage = parse_ocx_usage(r#"{"range":"all","historyTruncated":true,"summary":{"totalTokens":42,"requests":3,"unpricedRequests":1,"unmeteredRequests":1}}
200"#).unwrap();
        assert!(usage.history_truncated);
        assert_eq!(usage.range, "all");
        assert_eq!(usage.summary.unpriced_requests, 1);
        assert_eq!(usage.summary.unmetered_requests, 1);
    }

    #[test]
    fn usage_accepts_whitespace_and_preserves_zero_counts() {
        let usage = parse_ocx_usage(
            "\u{feff} {\"summary\":{\"estimatedCostUsd\":0,\"totalTokens\":0,\"requests\":0}} \r
\n200",
        ).unwrap();
        assert_eq!(usage.summary.total_tokens, 0);
        assert_eq!(usage.summary.estimated_cost_usd, Some(0.0));
        let serialized = serde_json::to_value(usage).unwrap();
        assert_eq!(serialized["summary"]["totalTokens"], 0);
        assert_eq!(serialized["summary"]["estimatedCostUsd"], 0.0);
    }

    #[test]
    fn usage_reports_http_failures_before_parsing_body() {
        for status in ["401", "403", "500"] {
            let error = parse_ocx_usage(&format!("<html>failure</html>\n{status}")).unwrap_err();
            assert!(error.contains(status));
        }
    }

    #[test]
    fn usage_rejects_empty_invalid_or_missing_summary() {
        for body in [
            "",
            "not json",
            "null",
            "{}",
            "{\"summary\":{}}",
            "{\"summary\":{\"totalTokens\":\"10\",\"requests\":1}}",
        ] {
            assert!(parse_ocx_usage(&format!("{body}\n200")).is_err());
        }
    }

    #[test]
    fn usage_preserves_unknown_cost() {
        let usage = parse_ocx_usage(
            "{\"summary\":{\"estimatedCostUsd\":null,\"totalTokens\":42,\"requests\":1}}\n200",
        )
        .unwrap();
        assert_eq!(usage.summary.estimated_cost_usd, None);
        assert_eq!(usage.summary.total_tokens, 42);
    }

    #[tokio::test]
    #[ignore = "Requires the local Opencodex server and admin token; reads usage only"]
    async fn live_ocx_usage() {
        let usage = get_ocx_usage(None)
            .await
            .expect("live Opencodex usage request failed");
        println!("{}", serde_json::to_string(&usage).unwrap());
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let profile = RoutingProfile::default();
    let registered: Vec<CliId> = adapters::registry().iter().map(|a| a.id()).collect();
    // ?곹깭諛??쒖꽌???쇱슦???꾨줈???쒖꽌瑜??곕Ⅸ??(PRD 7??. ?대뙌?곌? ?녿뒗 ?꾨낫???쒖쇅
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
            pending: Arc::new(Mutex::new(HashMap::new())),
        })
        .setup(move |app| {
            let handle = app.handle().clone();
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
            // ?댁쟾 ?ㅽ뻾???ㅻ깄??蹂듭썝 (由ъ뀑??吏???덈룄?곕뒗 import?먯꽌 ?먭린, ?ъ슜 ?щ???蹂듭썝)
            let stored = load_store(&handle);
            if let Ok(mut m) = monitor.lock() {
                m.import(stored, now());
            }
            emit_availability(&handle, &monitor);
            // 媛?⑹꽦 紐⑤땲??猷⑦봽: ?쒖옉 ???쒖꽦 CLI ?꾩껜 probe ??30珥덈쭏??荑⑤떎??留뚮즺쨌?ш????덉젙 ?뺤씤
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
            start_firstmate,
            continue_firstmate,
            send_chat,
            continue_job,
            cancel_run,
            get_availability,
            recheck_availability,
            pick_cli,
            set_routing_chain,
            set_enabled_clis,
            list_models,
            login_cli,
            enable_afk_mode,
            respond_permission,
            get_ocx_usage,
            get_ocx_health,
            get_ocx_quotas,
            get_ocx_providers,
            get_ocx_models,
            configure_ocx_auto_handoff,
            get_ocx_accounts,
            consume_codex_reset_credit,
            list_project_tree,
            read_project_file,
            highlight_project_file,
            write_project_file,
            send_system_notification,
            get_crew_mode,
            set_crew_mode,
            resources::get_system_resources
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
#[tauri::command]
fn send_system_notification(title: String, body: String) {
    std::thread::spawn(move || {
        use std::process::Command;
        let clean_title: String = title.chars().map(|c| match c { '"' | '\'' | '\x60' | '$' | '\r' | '\n' => ' ', _ => c }).take(100).collect();
        let clean_body: String = body.chars().map(|c| match c { '"' | '\'' | '\x60' | '$' | '\r' | '\n' => ' ', _ => c }).take(200).collect();
        let script = format!(
            "[void] [System.Reflection.Assembly]::LoadWithPartialName('System.Windows.Forms'); \
             $notify = New-Object System.Windows.Forms.NotifyIcon; \
             $notify.Icon = [System.Drawing.SystemIcons]::Information; \
             $notify.BalloonTipTitle = \"{}\"; \
             $notify.BalloonTipText = \"{}\"; \
             $notify.Visible = $true; \
             $notify.ShowBalloonTip(5000); \
             Start-Sleep -Seconds 6; \
             $notify.Dispose();",
            clean_title,
            clean_body
        );
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output();
    });
}
