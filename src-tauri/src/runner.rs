use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};

use crate::adapters::{AgentEvent, CliAdapter};
use crate::models::CommandSpec;

/// 실행 이벤트 수신 콜백. run_id와 어댑터가 파싱한 이벤트를 받는다.
pub type EventSink = Arc<dyn Fn(u64, AgentEvent) + Send + Sync>;

struct RunControl {
    cancel_tx: mpsc::Sender<()>,
    /// 사용자가 중지를 요청했는지. 종료 이벤트에서 "중지됨"과 "비정상 종료"를 구분한다.
    cancelled: Arc<AtomicBool>,
}

/// CLI 프로세스 실행·스트림 파싱·중지를 담당한다 (PRD 8장의 runner).
/// 프로세스는 작업 시작 시에만 생성하고 종료 시 레지스트리에서 제거한다 (가벼움 우선).
#[derive(Default)]
pub struct Runner {
    next_id: AtomicU64,
    procs: Arc<Mutex<HashMap<u64, RunControl>>>,
}

/// 끝까지 실행해 모은 출력 (probe 같은 짧은 명령용)
#[derive(Debug, Clone)]
pub struct Captured {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

impl Runner {
    pub fn new() -> Self {
        Self::default()
    }

    /// spec을 실행하고 stdout 각 줄을 adapter.parse_event로 변환해 sink로 보낸다.
    /// spec.stdin이 있으면 프로세스 표준 입력으로 써 넣고 닫는다 (여러 줄 프롬프트 전달용).
    /// stderr는 파싱 없이 Stderr 이벤트로 분리 전달하고,
    /// 종료 시 항상 ProcessExited를 마지막으로 보낸다.
    pub async fn start(
        &self,
        spec: CommandSpec,
        adapter: Arc<dyn CliAdapter>,
        sink: EventSink,
    ) -> std::io::Result<u64> {
        let mut cmd = os_command(&spec);
        cmd.stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let run_id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;

        if let Some(input) = spec.stdin.clone() {
            let mut stdin = child.stdin.take().expect("stdin piped");
            tokio::spawn(async move {
                let _ = stdin.write_all(input.as_bytes()).await;
                let _ = stdin.shutdown().await;
            });
        }
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.procs.lock().await.insert(
            run_id,
            RunControl {
                cancel_tx,
                cancelled: Arc::clone(&cancelled),
            },
        );

        {
            let sink = Arc::clone(&sink);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    sink(run_id, AgentEvent::Stderr { text: line });
                }
            });
        }

        {
            let procs = Arc::clone(&self.procs);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    tokio::select! {
                        maybe = lines.next_line() => match maybe {
                            Ok(Some(line)) => {
                                for ev in adapter.parse_event(&line) {
                                    sink(run_id, ev);
                                }
                            }
                            _ => break,
                        },
                        _ = cancel_rx.recv() => {
                            let _ = child.start_kill();
                            break;
                        }
                    }
                }
                let code = child.wait().await.ok().and_then(|s| s.code());
                sink(
                    run_id,
                    AgentEvent::ProcessExited {
                        code,
                        cancelled: cancelled.load(Ordering::Relaxed),
                    },
                );
                procs.lock().await.remove(&run_id);
            });
        }

        Ok(run_id)
    }

    /// 실행 중 run에 중지 신호를 보낸다. 존재하지 않으면 false.
    pub async fn cancel(&self, run_id: u64) -> bool {
        if let Some(ctrl) = self.procs.lock().await.get(&run_id) {
            ctrl.cancelled.store(true, Ordering::Relaxed);
            ctrl.cancel_tx.send(()).await.is_ok()
        } else {
            false
        }
    }

    /// 동시 실행 수. 폴더당 1개 잠금(PRD 5장) 배선 시 사용한다.
    #[allow(dead_code)]
    pub async fn running_count(&self) -> usize {
        self.procs.lock().await.len()
    }
}

/// spec을 끝까지 실행해 출력을 모은다. probe 등 짧은 명령용.
/// timeout이 지나면 프로세스를 죽이고 timed_out으로 돌려준다.
pub async fn run_capture(spec: &CommandSpec, timeout: Duration) -> std::io::Result<Captured> {
    let mut cmd = os_command(spec);
    cmd.stdin(if spec.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true);
    let mut child = cmd.spawn()?;
    if let Some(input) = spec.stdin.clone() {
        let mut stdin = child.stdin.take().expect("stdin piped");
        tokio::spawn(async move {
            let _ = stdin.write_all(input.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(out) => {
            let out = out?;
            Ok(Captured {
                code: out.status.code(),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                timed_out: false,
            })
        }
        Err(_) => Ok(Captured {
            code: None,
            stdout: String::new(),
            stderr: format!("{}초 안에 응답 없음", timeout.as_secs()),
            timed_out: true,
        }),
    }
}

/// 실행 명령 조립.
/// Windows에서 npm 계열 CLI(claude·gemini·opencode)는 실체가 .cmd 셔임이라 CreateProcess로 직접 실행되지
/// 않는다. PATH에서 실제 파일(.exe → .cmd → .bat)을 찾아 그 경로로 실행하면, .cmd/.bat는 Rust 표준 라이브러리가
/// cmd.exe 경유 + 인자 안전 이스케이프(CVE-2024-24576 대응)를 맡는다. 줄바꿈이 든 인자는 그 단계에서 거부되므로
/// 여러 줄 프롬프트는 CommandSpec.stdin으로 넘긴다. 못 찾는 이름(cmd 내장 명령 등)만 `cmd /c`로 폴백한다.
fn os_command(spec: &CommandSpec) -> Command {
    let mut cmd = match resolve_program(&spec.program) {
        Some(path) => {
            let mut c = Command::new(path);
            c.args(&spec.args);
            c
        }
        None if cfg!(windows) => {
            let mut c = Command::new("cmd");
            c.arg("/c").arg(&spec.program);
            c.args(&spec.args);
            c
        }
        None => {
            let mut c = Command::new(&spec.program);
            c.args(&spec.args);
            c
        }
    };
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: 콘솔 창 번쩍임 방지
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    if !spec.cwd.is_empty() {
        cmd.current_dir(&spec.cwd);
    }
    cmd
}

/// Windows에서만 PATH를 뒤진다. 경로 구분자나 확장자가 이미 있으면 그대로 둔다.
fn resolve_program(program: &str) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    if program.contains(['\\', '/']) || Path::new(program).extension().is_some() {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    find_in_dirs(program, std::env::split_paths(&path))
}

/// 디렉터리 순서대로, 각 디렉터리 안에서는 .exe → .cmd → .bat 순으로 찾는다.
pub(crate) fn find_in_dirs(
    program: &str,
    dirs: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    for dir in dirs {
        for ext in ["exe", "cmd", "bat"] {
            let candidate = dir.join(format!("{program}.{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::claude::ClaudeAdapter;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("agent-dock-test").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn find_in_dirs_prefers_dir_order_then_exe() {
        let a = temp_dir("find-a");
        let b = temp_dir("find-b");
        std::fs::write(a.join("foo.cmd"), "").unwrap();
        std::fs::write(b.join("foo.exe"), "").unwrap();
        std::fs::write(b.join("bar.cmd"), "").unwrap();
        std::fs::write(b.join("bar.exe"), "").unwrap();

        // 앞선 디렉터리의 .cmd가 뒤 디렉터리의 .exe보다 우선 (PATH 의미 유지)
        assert_eq!(
            find_in_dirs("foo", [a.clone(), b.clone()]),
            Some(a.join("foo.cmd"))
        );
        // 같은 디렉터리 안에서는 .exe 우선
        assert_eq!(
            find_in_dirs("bar", [a.clone(), b.clone()]),
            Some(b.join("bar.exe"))
        );
        assert_eq!(find_in_dirs("none", [a, b]), None);
    }

    #[test]
    fn resolve_program_leaves_paths_and_extensions_alone() {
        assert_eq!(resolve_program("C:\\x\\claude.cmd"), None);
        assert_eq!(resolve_program("claude.cmd"), None);
    }

    /// 실제 claude를 러너로 통과시켜 이벤트가 실제로 잡히는지 확인한다.
    /// 토큰을 쓰므로 기본 무시. 실행: `cargo test real_claude -- --ignored --nocapture`
    #[cfg(windows)]
    #[ignore]
    #[tokio::test]
    async fn real_claude_through_runner() {
        use crate::models::{Job, JobStatus};
        let job = Job {
            id: 0,
            title: "t".into(),
            request: "Reply with exactly: REPRO_OK".into(),
            project_dir: "D:\\dev\\gotgan".into(),
            profile: "코딩 작업".into(),
            allow_writes: false,
            unattended_ok: false,
            status: JobStatus::Starting,
        };
        let adapter = Arc::new(ClaudeAdapter);
        let spec = adapter.build_command(&job);
        eprintln!("SPEC: program={} args={:?}", spec.program, spec.args);

        let runner = Runner::new();
        let events: Arc<std::sync::Mutex<Vec<AgentEvent>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink: EventSink = {
            let events = Arc::clone(&events);
            Arc::new(move |_r, ev| {
                eprintln!("EVENT: {:?}", ev);
                events.lock().unwrap().push(ev);
            })
        };
        runner.start(spec, adapter, sink).await.unwrap();
        for _ in 0..300 {
            if events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, AgentEvent::ProcessExited { .. }))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        let n = events.lock().unwrap().len();
        eprintln!("TOTAL EVENTS: {}", n);
        assert!(n > 1, "process_exited 외 이벤트가 없음 — stdout 미수신");
    }

    /// Claude result 이벤트 한 줄을 파일로 만들어 `type`(cmd 내장, 폴백 경로)으로 출력시켜
    /// 프로세스 실행 → 스트림 파싱 → 종료 이벤트까지 전체 경로를 검증한다.
    #[cfg(windows)]
    #[tokio::test]
    async fn runner_parses_stdout_through_adapter() {
        let dir = temp_dir("runner");
        let file = dir.join("claude-result.jsonl");
        std::fs::write(
            &file,
            "{\"type\":\"result\",\"is_error\":false,\"result\":\"RUNNER_OK\"}\r\n",
        )
        .unwrap();

        let runner = Runner::new();
        let events: Arc<std::sync::Mutex<Vec<AgentEvent>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink: EventSink = {
            let events = Arc::clone(&events);
            Arc::new(move |_run_id, ev| events.lock().unwrap().push(ev))
        };

        let spec = CommandSpec {
            program: "type".into(),
            args: vec![file.to_string_lossy().into_owned()],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        };
        runner
            .start(spec, Arc::new(ClaudeAdapter), sink)
            .await
            .unwrap();

        for _ in 0..100 {
            let done = events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, AgentEvent::ProcessExited { .. }));
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        let collected = events.lock().unwrap();
        assert!(
            collected.iter().any(|e| matches!(
                e,
                AgentEvent::Completed { ok: true, summary } if summary == "RUNNER_OK"
            )),
            "수집된 이벤트: {:?}",
            *collected
        );
        assert!(collected.iter().any(|e| matches!(
            e,
            AgentEvent::ProcessExited {
                cancelled: false,
                ..
            }
        )));
    }

    /// PATH 해석 경로(실제 .exe 파일)로 끝까지 실행해 출력을 모으고, stdin이 전달되는지 본다.
    #[cfg(windows)]
    #[tokio::test]
    async fn run_capture_collects_output_and_feeds_stdin() {
        let spec = CommandSpec {
            program: "findstr".into(),
            args: vec!["CAP".into()],
            env: vec![],
            cwd: String::new(),
            stdin: Some("skip me\nCAP_IN line\n".into()),
        };
        let out = run_capture(&spec, Duration::from_secs(10)).await.unwrap();
        assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
        assert!(out.stdout.contains("CAP_IN"), "stdout: {}", out.stdout);
        assert!(!out.stdout.contains("skip me"));
        assert!(!out.timed_out);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn run_capture_times_out() {
        let spec = CommandSpec {
            program: "cmd".into(),
            args: vec![
                "/c".into(),
                "ping".into(),
                "-n".into(),
                "5".into(),
                "127.0.0.1".into(),
            ],
            env: vec![],
            cwd: String::new(),
            stdin: None,
        };
        let out = run_capture(&spec, Duration::from_millis(500))
            .await
            .unwrap();
        assert!(out.timed_out);
        assert_eq!(out.code, None);
    }
}
