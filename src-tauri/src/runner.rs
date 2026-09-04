use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};

use crate::adapters::{AgentEvent, CliAdapter};
use crate::models::CommandSpec;

/// 실행 이벤트 수신 콜백. run_id와 어댑터가 파싱한 이벤트를 받는다.
pub type EventSink = Arc<dyn Fn(u64, AgentEvent) + Send + Sync>;

struct RunControl {
    cancel_tx: mpsc::Sender<()>,
}

/// CLI 프로세스 실행·스트림 파싱·중지를 담당한다 (PRD 8장의 runner).
/// 프로세스는 작업 시작 시에만 생성하고 종료 시 레지스트리에서 제거한다 (가벼움 우선).
#[derive(Default)]
pub struct Runner {
    next_id: AtomicU64,
    procs: Arc<Mutex<HashMap<u64, RunControl>>>,
}

impl Runner {
    pub fn new() -> Self {
        Self::default()
    }

    /// spec을 실행하고 stdout 각 줄을 adapter.parse_event로 변환해 sink로 보낸다.
    /// stderr는 파싱 없이 Stderr 이벤트로 분리 전달하고,
    /// 종료 시 항상 ProcessExited를 마지막으로 보낸다.
    pub async fn start(
        &self,
        spec: CommandSpec,
        adapter: Arc<dyn CliAdapter>,
        sink: EventSink,
    ) -> std::io::Result<u64> {
        let mut cmd = os_command(&spec);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        let run_id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let (cancel_tx, mut cancel_rx) = mpsc::channel::<()>(1);
        self.procs
            .lock()
            .await
            .insert(run_id, RunControl { cancel_tx });

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
                sink(run_id, AgentEvent::ProcessExited { code });
                procs.lock().await.remove(&run_id);
            });
        }

        Ok(run_id)
    }

    /// 실행 중 run에 중지 신호를 보낸다. 존재하지 않으면 false.
    pub async fn cancel(&self, run_id: u64) -> bool {
        if let Some(ctrl) = self.procs.lock().await.get(&run_id) {
            ctrl.cancel_tx.send(()).await.is_ok()
        } else {
            false
        }
    }

    pub async fn running_count(&self) -> usize {
        self.procs.lock().await.len()
    }
}

/// Windows에서 npm 계열 CLI(claude·gemini·opencode)는 실체가 .cmd/.ps1 셔임이라
/// CreateProcess로 직접 실행되지 않는다. cmd.exe /c로 감싸 PATH·PATHEXT 해석을 위임한다.
/// TODO: 인자에 cmd 특수문자(&, ^, | 등)가 들어가는 경우의 이스케이프 보강.
fn os_command(spec: &CommandSpec) -> Command {
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/c").arg(&spec.program);
        c.args(&spec.args);
        c
    } else {
        let mut c = Command::new(&spec.program);
        c.args(&spec.args);
        c
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::claude::ClaudeAdapter;

    /// Claude result 이벤트 한 줄을 파일로 만들어 `type`으로 출력시켜
    /// 프로세스 실행 → 스트림 파싱 → 종료 이벤트까지 전체 경로를 검증한다.
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

    #[cfg(windows)]
    #[tokio::test]
    async fn runner_parses_stdout_through_adapter() {
        let dir = std::env::temp_dir().join("agent-dock-test");
        std::fs::create_dir_all(&dir).unwrap();
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
    }
}
