# Agent Dock — 핸드오프 노트 (2026-09-04 23:20 기준)

PRD·설계 근거·스파이크 실측·구현 현황의 원본은 위키 `wiki/【프로그래밍】 AI CLI 오케스트레이터 PRD.md`(14장 구현 현황, 15장 실측)다. 이 문서는 코드 저장소에서 바로 이어받기 위한 요약이다.

## 다음 세션 시작 절차

1. 이 문서 → `git log --oneline | head` → PRD 14장 "다음 작업" 순으로 읽는다. 위키 메모리(`project-agent-dock`)에도 같은 요약이 있다.
2. 개발 환경 (2026-09-03 D:로 이전 완료): Rust `RUSTUP_HOME=D:\tools\rustup`, `CARGO_HOME=D:\tools\cargo`, PATH에 `D:\tools\cargo\bin`. 사용자 `TEMP`/`TMP`=`D:\temp`. VS Build Tools 2022는 `D:\tools\VS2022BuildTools`. `src-tauri\target`은 D:.
3. 실행: 새 PowerShell 창에서 `cd D:\dev\agent-dock; npm run tauri dev` (src-tauri 변경 시 자동 재빌드·재시작). 옛 셸에 `C:\Users\User\.cargo\bin`이 남아 있으면 `$env:PATH = "D:\tools\cargo\bin;$env:PATH"`.
4. 검증: `npx tsc --noEmit`(프론트), `cd src-tauri; cargo test`(48개, 무시 1). 실제 claude를 태우는 통합 테스트는 `cargo test real_claude -- --ignored --nocapture`.
5. git: `main`, 원격 `origin` = GitHub **private** `bin7335/agent-dock`(2026-09-06 최초 푸시, https://github.com/bin7335/agent-dock). `core.autocrlf=false`. 작업 끝에 `git push`. 위키(raw/wiki)는 이 저장소에 넣지 않는다.
6. **바로 할 일 = 2단계 자동 폴백**(아래 "다음 단계" 1번). 앱 E2E는 화면 자동 조작 스크립트(세션 scratchpad `ad.ps1`: Snap/Click/LoginConsoles)로 했다 — 새 세션이면 다시 만든다(요령은 맨 아래).

## 현재 상태

### 구조
- 백엔드 `src-tauri/src`: `models`(CliId 5종·Job·CommandSpec) · `availability`(CLI별 스냅샷 상태 머신, 오류 분류, 리셋·retry-after 추출, import/export) · `scheduler`(`pick_candidate`) · `db`(스키마만, 미배선) · `runner`(tokio 실행, stdin 유지·후속 쓰기, 중지, `run_capture`·`exchange_lines`, PATH에서 .exe 우선 해석 + winget 예비 경로, stderr ANSI 제거) · `adapters/{claude,codex,gemini,antigravity,opencode}`
- Tauri 커맨드: `start_job`/`continue_job`(model 포함) · `cancel_run` · `respond_permission` · `get_availability` · `recheck_availability` · `pick_cli` · `set_routing_chain` · `set_enabled_clis` · `list_models` · `login_cli`. 이벤트 `agent-event{run_id, cli, event}`, `availability-changed`(스냅샷 배열)
- 어댑터 계약(`adapters/mod.rs`): probe_command/probe_exchange+interpret · build_command/build_resume_command · parse_event · `keeps_stdin_open` · `stdin_follow_up(job, session_id) -> LineReaction{send, drop_events}` · `permission_reply` · `filter_stderr` · rate_limit_exchange/parse_rate_limits/parse_account · login_flow · version_command · model_listing/parse_models/scan_models
- 프론트 `src/App.tsx`: 대화 목록(CLI별 세션 `sessions[cli]{sessionId, syncedUpTo}`, 대화 중 CLI 전환 시 `buildHandoff` 문단 자동 첨부) · 승인 카드(허용/세션 동안 허용/거부, 헤더 "승인 대기 N") · 상단 카드 드래그=라우팅 우선순위 · 하단 상태바+상세 패널(사용률·리셋·근거·계정·버전·재검사·로그인) · ⚙ CLI 레지스트리(사용 여부·순위·상태·계정·버전·로그인·모델) · localStorage(`agentdock.projectDir/cli/chain/enabled/model.<cli>`)

### CLI별 대화 전송 방식과 승인 중계 (2026-09-04 전부 실측)
| CLI | 실행 | 재개 | 승인 중계 | 사용량 |
|---|---|---|---|---|
| Claude 2.1.259 | `claude -p --output-format stream-json --input-format stream-json --permission-prompt-tool stdio --permission-mode acceptEdits\|plan [--model]`, 프롬프트는 stdin의 user 메시지 JSON | `--resume <id>` | `control_request/can_use_tool` → `control_response{allow(updatedInput[,updatedPermissions])\|deny}`. 앱 E2E 통과(허용·거부·세션 동안 허용, `--resume`에도 유지) | 스트림 `rate_limit_event`(공식) |
| Codex 0.152.1 | `codex app-server`(stdio JSON-RPC, jsonrpc 필드 없음): initialize → initialized → `thread/start{cwd, approvalPolicy:"on-request", sandbox, model}` → 응답 thread.id → follow-up `turn/start` | `thread/resume{threadId}` | 서버 요청 `item/commandExecution\|fileChange/requestApproval` → `{id, result:{decision: accept\|acceptForSession\|decline}}`(acceptForSession은 availableDecisions에 있을 때만). 앱 E2E 통과 | 턴 중 `account/rateLimits/updated` + probe `account/rateLimits/read`(공식) |
| Gemini 0.54.4 | `gemini --acp --approval-mode auto_edit\|plan [-m]`: initialize → `session/new{cwd}` → follow-up `session/prompt` | `session/load{sessionId}`(응답 전 재생 알림은 drop) | `session/request_permission{options}` → `{outcome:{selected, optionId}}`. 프로브·단위 테스트 확인, **앱 E2E는 일일 쿼터 소진으로 미완**(오류 → Cooldown 전환은 확인) | 없음(추정). 429·"exhausted your daily quota" 반응형 쿨다운 |
| Antigravity 1.1.26 | `agy -p … --output-format stream-json --mode plan\|accept-edits [--dangerously-skip-permissions] [--model]` | `--conversation <id>` | 없음(플랜 모드는 헤드리스에서 자동 거부) | 없음(TUI `/usage`뿐) |
| OpenCode 1.18.5 | `opencode run --format json --dir … --agent plan\|build [--auto] [-m]`, 여러 줄은 임시 파일 `-f` | `--session <id>` | 없음. `opencode acp`가 같은 ACP로 동작함을 실측(빠름, configOptions `model`·`mode` build/plan, `usage_update`)하나 기본 설정에선 승인을 묻지 않아 보류 | 없음 |

- 승인 공통: 대기 요청은 `AppState.pending`(run_id, request_id)에 보관, 10분 무응답 자동 거부. 러너는 결과(`Completed`)가 오면 stdin을 닫고 5초 뒤에도 살아 있으면 죽인 뒤 정상 종료(0)로 보고(Gemini ACP는 stdin을 닫아도 안 끝남)
- CLI 동작 메모: Claude acceptEdits는 파일시스템 Bash(`echo >`, mkdir)도 자동 승인, plan은 읽기 전용 명령 자동 실행 → 카드는 python·npm·git 쓰기 등에서 뜬다. Codex는 샌드박스가 막은 명령을 on-request로 다시 묻는다(읽기 전용 샌드박스에서 파일 쓰기 등)
- 로그인: Claude·Codex·OpenCode·Antigravity·Gemini 모두 앱 데이터 폴더의 `login-<cli>.cmd`를 새 콘솔로 띄우고(러너가 찾은 실행 파일 경로를 `call`, 끝에 `exit`), 콘솔이 열린 동안 8초마다 재검사·10분 상한. Codex 로그아웃→버튼→브라우저→Ready 복귀 E2E 통과. Gemini 버튼은 `/auth` API 키 안내(개인 Google OAuth는 2026-06-18 종료 → AI Pro는 Antigravity)
- 가용성 모니터: 시작 시 전체 probe → 30초 틱, 주기 probe 5분, 창 포커스 복귀 시 재검사(30초 스로틀), 추정 쿨다운 30분, 재발 시 6시간, 모든 윈도우 리셋 시 복귀. 스냅샷은 `%APPDATA%\com.user.agent-dock\availability.json`에 보존·복원. 로그인 안 됨=빨강, 쿨다운=주황
- 모델 목록: Claude는 네이티브 바이너리(`%APPDATA%\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe`) 스캔, Codex `model/list`, Gemini ACP session/new, Antigravity `agy models`, OpenCode `opencode models`

### 오늘(2026-09-04) 통과한 앱 E2E
채팅·재개·중지·폴더 선택 → 상태바 실데이터 → 드래그 우선순위 → 레지스트리·로그인 버튼(Codex 실제 재로그인) → 대화 중 CLI 전환(Claude↔Antigravity, OpenCode) → 승인 카드(Claude 4회, Codex 2회) → Gemini 쿨다운 전환. 캡처는 세션 scratchpad `agentdock-NN*.png`(세션이 끝나면 사라짐; 결과는 PRD 14장에 기록)

## 알려진 한계·TODO

- 자동 폴백 미구현: 쿨다운·한도 시 다음 CLI로 넘기는 건 아직 사용자가 툴바에서 CLI를 바꿔야 한다(handoff 문단 자동 첨부는 됨)
- Gemini CLI: 이 계정은 무료 API 키(pro 한도 0, 429 백오프, 일일 쿼터)라 느리거나 실패한다. 필요 없으면 CLI 설정에서 끄기. 텔레메트리(`GEMINI_TELEMETRY_*` 로컬 OTLP: api_error 429의 quotaId·retryDelay, flash_fallback, api_response 토큰)로 모델별 한도 감지 가능 — 미구현(PRD 15장)
- Antigravity: 사용량 신호 없음(추정 경로), plan 모드 명령 허용 규칙(`permissions.allow`) 미검토
- Codex 모델별 한도(`rateLimitsByLimitId`)는 표시하지 않음(계정 단위만)
- OpenCode `--agent plan`이 읽기 전용이라는 전제. `/이름` 슬래시는 run 모드에서 미확장(Claude·Gemini 커스텀 명령은 동작, Codex는 `$이름`)
- 라우팅 체인·대화는 localStorage/메모리뿐(SQLite 미배선). 폴더당 동시 1개 잠금(`Runner::running_count`) 미배선
- 보류(사용자 결정): 설정 패널 "CLI 추가"(범용 어댑터, CliId enum → 문자열 id 리팩터링 선행), 위젯/컴팩트 도크 창(트레이 단계에서 재검토)
- (선택) Claude 로그인 버튼 실제 재로그인 E2E — 개발 세션(Claude Code)이 같은 로그인을 써서 생략함

## 다음 단계 (PRD 14장 "다음 작업"과 동일)

1. **2단계 자동 폴백**: 실행이 한도·429로 실패해 그 CLI가 Cooldown이 되면, 이미 있는 `buildHandoff`(참조 파일·변경 파일 요약 보강)로 다음 Ready CLI에 자동 재개. 무인 정책(쓰기 작업은 Git 자동 체크포인트 뒤에만), 서킷 브레이커(동일 오류 3회 → blocked), retry-after가 짧으면 재시도·길면 handoff
2. 트레이 상주, SQLite 영속화(라우팅 체인·대화·작업·스냅샷), 폴더당 동시 1개 잠금
3. Codex 모델별 한도 표시 여부, Antigravity plan 모드 명령 허용 규칙, (선택) Claude 로그인 재로그인 E2E
4. (선택) OpenCode를 `opencode acp`로 전환(Gemini ACP 코드 공유, `permission.bash=ask`면 승인 중계, `usage_update` 토큰 장부)
5. Gemini 텔레메트리 기반 모델별 한도 감지

## E2E 자동화 요령 (PowerShell)
- `SetProcessDPIAware` 뒤 `PrintWindow(hwnd, hdc, 2)`로 창을 캡처(125% 모니터, 포커스 불필요). 클릭은 `SetCursorPos`+`mouse_event`, 좌표는 캡처 픽셀 = 창 기준(창 원점 GetWindowRect 더함)
- 한글 IME 때문에 `SendKeys`로 글자를 치지 말고 `Set-Clipboard` + `^v`. 네이티브 `<select>`는 클릭 후 `{HOME}{DOWN}…{ENTER}`
- PowerShell 변수는 대소문자 무시(`$h`/`$H` 충돌), `-match`도 무시(`-cmatch`). 콘솔 창은 conhost 소유라 cmd의 MainWindowHandle이 0. 앱 재시작 직후 첫 클릭은 포커스에만 쓰인다
- Bash 도구는 heredoc이 ~16KB를 넘으면 잘리고 `\n`·`\a` 같은 백슬래시 시퀀스를 바꾸므로, 긴 패치는 Write 도구로 .py를 만들어 실행한다
