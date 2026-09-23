# Agent Dock — 핸드오프 노트

## 최신 저장: 2026-09-22 — 다음 세션은 여기부터

현재 작업 경로는 `C:\PROJECT\agent-dock`이다. 아래 과거 기록의 D: 경로·창 크기·AFK 완료 주장은 현재 상태와 다르며, 이 절을 우선한다. 진행 상황 저장 후 사용자가 **커밋·푸시까지 요청**했다. 작업 결과와 이 문서를 `origin/main`에 게시하며, 완료 여부와 커밋은 Git 기록으로 확인한다. 참고용 `_opencodex_review/` 복제 저장소는 커밋 대상에서 제외한다.

### 완료한 작업

1. **Opencodex 연동**: Rust 비동기 curl 호출, 인증 헤더 stdin 전달, 시간 제한·HTTP/JSON 오류 처리. 사용량 기간을 서버 계약에 맞는 `all`/`7d`/`30d`로 수정. HTTP 200의 `read_failed`도 오류 처리하고 미산정 요청·기록 잘림을 표시한다. 환경 변수와 OCX 홈·포트 설정을 읽는다.
2. **실제 리소스 표시**: `src-tauri/src/resources.rs`의 Windows API로 PC CPU·RAM 및 OCX 프로세스 CPU·메모리 조회. 첫 CPU 표본·연결 실패는 미확인 표시. 프론트 `src/useTelemetry.ts`에서 리소스 2초, 사용량 10초, 상태 15초, 한도 60초 간격으로 조회한다(응답 완료 후 다음 조회; 서버 한도 캐시는 별도).
3. **전체 UI 개편**: 개요·대화·설정 탭, 컴팩트 비용·토큰·리소스, 하단 AI 한도. `src/DockPanels.tsx`에서 기본은 제공자명·남은 비율·막대, 마우스/포커스 시 기간별 상세·리셋 시간. 사용률을 남은 비율로 변환하며 미제공·만료 표본을 100%로 추정하지 않는다. 실제 동작하지 않는 OAuth·AFK·자동 전환 버튼은 제거했다. AFK 백엔드는 여전히 스텁이다.
4. **최근 사용자 피드백 반영**: 마우스 이탈 시 접힘 대기 180ms를 제거했다. Tauri `resizable: true`, 최소 380×188, 오른쪽 아래 드래그 손잡이와 `allow-start-resize-dragging` 권한 추가. 기본 컴팩트 380×188, 한도 상세 높이 최소 440, 펼침 520×740. 접힘/펼침 크기를 메모리에 따로 기억하여 한도 조회·제공자 전환 때 수동 크기가 초기화되지 않게 수정했다. 앱 재시작 이후까지 크기를 저장하는 기능은 없다.
5. **검토 문서**: `OPENCODEX_REVIEW.md`에 upstream `coseung2/opencodex`의 `f6735fd552b04ad7ef59abbd99aa54a707121113` (2.8.0-cs.32) 기준 수정 완료 항목과 남은 후보를 기록했다. 참고용 로컬 복제는 `_opencodex_review/`이다.

### 검증 상태 — 최근 수정 전후를 구분

- 2026-09-22 커밋 준비 최종 검사: `npm run build`, `node scripts/check-quota.cjs`, Rust 전체 테스트 **56개 통과·2개 제외**. 개인 경로가 있는 미추적 `launch.cmd`는 참고 복제 저장소와 함께 로컬에 남긴다.

- UI 개편 후 `npm run build` 통과. Rust 전체 테스트 54개 통과·외부 실행 2개 제외. 이후 추가한 오류·메타데이터 테스트 포함 `cargo test --manifest-path src-tauri/Cargo.toml ocx_tests` 6개 통과·실서버 테스트 1개 제외.
- `node scripts/check-quota.cjs` 통과: 남은 한도 변환, 0/100%, 미제공, 만료, 초/밀리초 리셋, 크레딧, 모델별 윈도우.
- UI 개편은 실제 Tauri WebView에서 사용량·리소스·AI 한도 렌더링, 마우스 진입/이탈 크기 변경, 세 탭의 가로 넘침 없음을 확인했다. 이 확인은 **180ms 제거·수동 크기 조절 수정 이전**이다.
- 최근 즉시 접힘·수동 크기 조절 수정 후에는 `npm run build`와 `cargo check --manifest-path src-tauri/Cargo.toml` 통과. 실제 모서리 드래그·크기 복원은 아직 재검증하지 못했다. CDP `127.0.0.1:9223` 재접속이 거부되어 기존 자동 화면 검증을 실행하지 못했다. 실행 중 앱이 없다는 뜻은 아니다.
- 커밋 준비 중 `src-tauri/capabilities/default.json`의 혼합 줄바꿈·들여쓰기를 정리했다. 서버 중단·복구 전체 E2E와 유료 대화 실행은 이번 작업에서 하지 않았다.

### 다음 작업 순서

1. 최신 앱에서 모서리 드래그, 접기/펴기 후 수동 크기 유지, 한도 상세에서 마우스를 벗어날 때 즉시 접힘을 직접 검증한다. 필요하면 `npm run tauri dev`로 재시작하되 실행 중 대화나 다른 프로세스를 일괄 종료하지 않는다.
2. 크기를 빠르게 전환하거나 창을 작게 줄일 때 레이아웃·자동 크기 변경 충돌이 없는지 확인한다. 키보드 포커스로 상세를 여닫는 동작도 확인한다.
3. `OPENCODEX_REVIEW.md`의 실행 수명 문제를 우선 처리한다: 이전 run 이벤트의 새 실행 덮어쓰기, run_id 없는 승인 결과 매칭, 취소·자식 프로세스 종료, 프로젝트별 동시 실행 제어. 이 문제들은 아직 수정하지 않았다.

### 이어받기 메모

- 테스트 스크립트 `scripts/check-quota.cjs`는 저장소에 있다. 화면 확인용 임시 스크립트는 `%TEMP%\agent-dock-ui-check.mjs`; 화면은 `%TEMP%\dock-{compact,hover,overview,chat,settings}.png`에 남아 있을 수 있다. 임시 파일은 지속 보존을 보장하지 않는다.
- 사용자 요청으로 `C:\Users\SONG\.codex\skills\i-have-adhd\SKILL.md`를 적용했다. 한국어로 결과·다음 행동을 짧게 안내하고, 불필요한 재확인 없이 승인된 작업을 이어간다.
- 작업 시작 전부터 수정된 파일과 미추적 `launch.cmd`, 참고 저장소가 있었다. 기존 사용자 변경을 되돌리거나 전부 일괄 커밋하지 않는다. 아래 과거 기록의 자동 push 안내는 이번 저장 요청의 원격 게시 승인이 아니다.

## 2026-09-21: Opencodex 위젯 사용량 표시 수정

- 원인: 프론트 초기값은 펼침(`true`)인데 Tauri 창은 320×60이어서 사용량이 창 밖으로 잘렸다. 초기 상태를 접힘으로 맞추고 창을 320×100으로 변경했다. 펼침은 400×600이며 사용량 영역은 항상 본문 위에 표시된다.
- `get_ocx_usage`는 Rust에서 토큰을 읽고 Windows 시스템 `curl.exe`를 비동기로 호출한다. 인증 헤더는 stdin으로 전달하며, 연결 3초/전체 8초 제한과 HTTP·JSON 오류 보고를 추가했다. 반환값은 `summary`의 타입이 검증된 객체다.
- 프론트는 조회 완료 10초 후 다시 조회하며 언마운트 후 갱신을 막는다. 오류는 접힌 상태에서도 표시하고, 이전 수치가 남을 때는 갱신 실패임을 명시한다. 접기/펴기 버튼 클릭이 창 드래그로 처리되지 않도록 분리했다.
- 기존 `oauth-collect`가 `Error`의 `cause` 인자를 사용하여 빌드가 실패하던 문제는 `tsconfig.json`에 `ES2022.Error` 타입 라이브러리를 추가해 해결했다.
- 검증: `npm run build`, `cargo test --manifest-path src-tauri/Cargo.toml ocx_tests -- --nocapture`(4개 통과), `cargo test --manifest-path src-tauri/Cargo.toml live_ocx_usage -- --ignored --nocapture`(실제 로컬 API 조회 통과). 네이티브 WebView에서 비용·토큰 렌더링과 버튼 클릭에 따른 320×100 ↔ 400×600 전환을 확인했다.
- 검증 범위: HTTP 오류/잘못된 JSON은 Rust 테스트로 검증했다. 네이티브 IPC 함수는 읽기 전용이라 오류 주입에 의한 화면 복구 테스트는 수행하지 못했다. 정상 API 조회와 화면 표시는 실제 연결로 확인했다. 클린 빌드나 기존 프로세스 일괄 종료는 필요하지 않았다.

## 이전 핸드오프 (2026-09-04 23:20 기준)

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

### 2026-09-21 진행 상황 (Firstmate & oauth-collect 연동)

1. **빌드 환경 격리**: OneDrive 동기화 폴더(os error 5 잠금 에러 발생)에서 벗어나 로컬 D:\agent-dock으로 프로젝트를 복제하고 독립된 빌드 환경을 구축했습니다.
2. **Firstmate AFK 모드 (백엔드 완료)**:
   - UI 하단에 주황색 💤 /afk (자리비움) 버튼을 추가했습니다.
   - Rust 백엔드(src-tauri/src/lib.rs)에 enable_afk_mode IPC 명령어를 주입하여, 프론트엔드와 성공적으로 연동되도록 조치했습니다.
3. **oauth-collect 로그인 엔진 (진행 중)**:
   - UI 하단에 파란색 🔑 AI 일괄 로그인 (oauth) 버튼을 추가하고 oauth-collect 모듈을 동적 임포트하여 OAUTH_PROVIDERS["anthropic"].login 플로우를 연결했습니다.
   - **이슈**: 데스크톱 앱(Tauri) 샌드박스로 인해 window.open이 막혀 브라우저가 열리지 않는 현상을 발견했습니다.
   - **대응 방안**: Tauri 네이티브 플러그인(@tauri-apps/plugin-opener)으로 강제 브라우저 팝업을 시도하도록 패치했으며, 정확한 오류 지점 추적을 위해 1~6단계의 상세 디버그 Alert 창을 심어두었습니다 (여기서 일시 중지됨).

## Opencodex 연결 기준 (2026-09-22)

- 기준 저장소: https://github.com/coseung2/opencodex
- Windows 설치: npm install -g @coseung2/opencodex@next
- 실행: ocx start 또는 백그라운드 서비스 ocx service start
- 기본 프록시·대시보드: http://127.0.0.1:10100
- Agent Dock은 .opencodex/runtime-port.json의 포트를 읽고 /healthz, /api/usage, /api/provider-quotas, /api/providers를 조회한다.
- 인증 API는 .opencodex/admin-api-token을 Bearer 토큰으로 사용한다.
- 현재 실측: healthz와 provider quota API가 200 응답하며 OpenAI(Codex login) 주간 사용률 46%를 반환했다.
