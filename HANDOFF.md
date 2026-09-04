# Agent Dock — 핸드오프 노트 (2026-09-04)

PRD·설계 근거·스파이크 실측·구현 현황의 원본은 위키 `wiki/【프로그래밍】 AI CLI 오케스트레이터 PRD.md`다. 이 문서는 코드 저장소에서 바로 이어받기 위한 요약이다.

## 먼저 할 일

1. 개발 환경 (2026-09-03 D:로 이전 완료)
   - Rust: `RUSTUP_HOME=D:\tools\rustup`, `CARGO_HOME=D:\tools\cargo`, PATH에 `D:\tools\cargo\bin`. 새 PowerShell 창이면 그대로 `cargo`가 잡힌다.
   - 사용자 `TEMP`/`TMP` = `D:\temp`. VS Build Tools 2022는 `D:\tools\VS2022BuildTools`. `src-tauri\target`(7.4 GB)은 D:. C: 여유 5.1 GB.
2. 실행: 새 PowerShell 창에서
   ```powershell
   cd D:\dev\agent-dock
   npm run tauri dev
   ```
   (옛 셸에 `C:\Users\User\.cargo\bin` PATH가 남아 있으면 `$env:PATH = "D:\tools\cargo\bin;$env:PATH"`)
3. 검증: `npx tsc --noEmit` (프론트), `cd src-tauri; cargo test` (22개, 경고 0). 실제 claude를 태우는 통합 테스트는 `cargo test real_claude -- --ignored --nocapture`.
4. git: `main`, 커밋 9개 (기준선 `da632ec` → 가용성 모니터 `2e992b0` → HANDOFF → Codex 사용량 `dfc1d11` → 드래그 우선순위). `core.autocrlf=false`. 원격 없음 — 올린다면 private.

## 현재 상태 (2026-09-04, E2E 통과)

- 백엔드(`src-tauri/src`): `models` · `availability`(CLI별 스냅샷 상태 머신 + 오류 분류) · `scheduler`(`pick_candidate` 배선) · `db`(스키마만, 미배선) · `adapters/{claude,codex,gemini}`(명령 조립·이벤트 파싱·probe 해석) · `runner`(tokio 실행, stdin 전달, 중지 플래그, `run_capture` 타임아웃, PATH에서 .exe/.cmd 해석)
- Tauri 커맨드: `start_job`/`continue_job`(model 인자 포함) · `cancel_run` · `get_availability` · `recheck_availability` · `pick_cli` · `set_routing_chain` · `set_enabled_clis` · `list_models` · `login_cli`. 이벤트: `agent-event`(`{run_id, cli, event}`), `availability-changed`(스냅샷 배열, `enabled` 플래그 포함)
- 어댑터 5종: codex · claude · **antigravity**(`agy` 1.1.26, Google AI Pro 구독 경로 — 2026-06-18부터 Gemini CLI가 개인 계정을 끊고 Antigravity CLI로 이전시킴. `agy -p <프롬프트> --output-format stream-json --mode plan|accept-edits [--dangerously-skip-permissions] [--model X] [--conversation <id>]`, 이벤트 init/step_update(text_delta·tool_info)/result(status·response·usage), probe·모델 목록 `agy models`(탭 구분, 14종: gemini-3.8/3.7/3.6-flash high·medium·low, gemini-3.1-pro high·low, claude-sonnet-4-6, claude-opus-4-6-thinking, gpt-oss-120b), 로그인은 대화형 `agy` 첫 실행의 브라우저 흐름(자격증명은 Windows 자격 증명 관리자), 설치 `winget install Google.AntigravityCLI`(포터블 링크 `%LOCALAPPDATA%\Microsoft\WinGet\Linksgy.exe` — runner가 예비 경로로 탐색). 스킬은 워크스페이스 `.agents/skills`(위키 구조와 동일)·전역 `~/.gemini/antigravity-cli/skills`) · gemini(API 키, 무료 티어라 flash만) · **opencode**(1.18.5 실측: `run --format json --dir … --agent plan|build [--auto] [-m provider/model]`, 이벤트 `text`/`step_finish`, `--session` 재개, probe `auth list`의 "N credentials"). 어댑터 계약에 `login_flow`(Console=콘솔 창에서 CLI 로그인 명령, Exchange=ACP authenticate) · `model_listing`(Static/Exchange/Command) · `parse_models` 추가
- CLI 레지스트리 패널(⚙ CLI 설정): 사용 여부(끄면 상태바·라우팅·probe 제외, localStorage `agentdock.enabled`), 로그인 버튼, 모델 선택(localStorage `agentdock.model.<cli>`, 툴바에도 현재 CLI용 선택). 모델 목록: Claude는 목록 명령이 없어 설치된 네이티브 바이너리(`%APPDATA%
pm
ode_modules\@anthropic-ai\claude-codein\claude.exe`, 220MB)를 스캔해 이 CLI가 아는 정식 id(`claude-<계열>-<메이저>[-<마이너>]`, 날짜·v1 변형 제외)를 별칭 4개 뒤에 나열(`ModelListing::Scan`, 계정 가용 여부는 실행 시 확인), Codex app-server `model/list`(limit 필요), Gemini ACP `session/new`의 availableModels, OpenCode `opencode models`
- 로그인 흐름: Claude `claude auth login`·Codex `codex login`·OpenCode `opencode auth login`은 앱 데이터 폴더에 `login-<cli>.cmd`를 만들어 `cmd /c start "" /wait`로 새 콘솔 창에서 실행하고 창이 닫히면 재검사(상한 10분). Gemini는 ACP `authenticate{methodId:oauth-personal}`. **로그인 흐름 실사용 E2E는 아직 미수행**(계정이 이미 로그인 상태라 미검증)
- 가용성 모니터(`lib.rs` setup): 시작 시 전체 probe → 30초 틱. probe = claude `auth status`(JSON `loggedIn`) / codex `login status` / gemini `--version`. 실행 스트림의 `rate_limit_event`(공식)와 실패 원문(한도·인증·네트워크 분류, 리셋 epoch 힌트 추출)을 반영. 주기 probe 10분, 추정 쿨다운 30분, 리셋 직후 10분 내 재발 시 6시간 장기 쿨다운, 모든 윈도우 리셋 시에만 복귀
- 프로세스 실행: Windows에서 PATH를 뒤져 실제 파일 경로(.exe → .cmd → .bat)로 실행. .cmd는 Rust std가 cmd.exe 경유 + 안전 이스케이프를 맡는다(CVE-2024-24576 대응). 줄바꿈 인자는 그 단계에서 거부되므로 Claude 프롬프트는 stdin으로 넘긴다(실측: `echo … | claude -p` 정상)
- Codex 공식 사용량: `codex app-server`(stdio JSON-RPC)에 `initialize` → `initialized` → `account/rateLimits/read` 3줄을 일괄 전송(`runner::exchange_lines`, 1.2초). 응답 `rateLimits.primary/secondary{usedPercent, windowDurationMins, resetsAt}`를 five_hour/seven_day 이름으로 정규화해 `apply_rate_limit` → 상태바 "46% · 리셋 ↻ · 공식". `rateLimitReachedType`이 있으면 100%로. probe가 Ready인 CLI만 읽는다
- 우선순위 드래그: 상단 카드(순위 번호)를 HTML5 드래그로 옮기면 `set_routing_chain`이 라우팅 프로필·모니터 순서·추천 CLI를 갱신하고 localStorage(`agentdock.chain`)에 저장, 시작 시 복원. Windows 웹뷰에서 HTML 드래그가 되려면 `tauri.conf.json` 창의 `dragDropEnabled:false` 필요(적용됨). 기본 창 1100×760
- 스냅샷 보존: 가용성이 바뀔 때마다 `%APPDATA%\com.user.agent-dockvailability.json`에 저장하고 시작 시 `import`로 복원(리셋 지난 윈도우 폐기, 진행 중 쿨다운 유지). Claude 사용률은 실행 스트림에서만 오므로 이 파일이 없으면 재시작 후 다음 Claude 실행까지 "?"다. Gemini는 CLI가 사용량을 제공하지 않아 항상 "?"(추정)이며 429 관측 시에만 추정 쿨다운
- 상단 카드는 순위·사용 가능 여부만 표시(사용률·근거는 상태바·상세 패널) — 2026-09-04 사용자 요청
- 계정 표시: 스냅샷 `account{label, plan, method}` — Claude `auth status`(email·subscriptionType·authMethod), Codex app-server `account/read`(한도 교환에 id 3으로 동승, email·planType), OpenCode `auth list`의 제공자 이름, Gemini는 `~/.gemini/settings.json`의 `security.auth.selectedType`(gemini-api-key → "Gemini API 키", oauth-personal → google_accounts.json의 active/old). 상태바 상세 패널과 레지스트리 표에 표시. 토큰은 읽지 않는다
- Gemini probe는 ACP 교환(`probe_exchange`: initialize → session/new). 성공 = 인증 OK(CLI 제시, agentInfo.version), `Authentication required` 오류 = 로그인 필요(빨강). 주기 probe 5분 + 창 포커스 복귀 시 즉시 재검사(30초 스로틀)
- 대화 중 CLI 전환(2026-09-04): 대화는 CLI별 세션(`sessions[cli] = {sessionId, syncedUpTo}`)을 유지하고, 툴바 CLI 선택을 바꾸면 현재 대화의 `cli`가 바뀐다. 다음 메시지에 그 CLI가 모르는 항목(다른 CLI에서 오간 사용자·응답·파일 변경)을 `buildHandoff` 문단으로 앞에 붙여 보낸다(새 CLI면 start_job, 기존 세션이면 continue_job + "그사이 대화"). 실행 종료 시 `syncedUpTo`를 갱신. 16,000자 초과분은 앞부분 생략
- 여러 줄 프롬프트 전달(전환·handoff 전제): Claude stdin, Gemini stdin + `-p ""`, OpenCode는 줄바꿈 있을 때 임시 파일 `-f` 첨부(메시지 뒤에), Codex는 네이티브 exe 인자(PATH 해석을 .exe 우선으로 변경 — cmd 셔임 경유 시 첫 줄만 전달되는 실측)
- 프론트(`src/App.tsx`): 채팅 UI(세션 재개·폴더 고정) + 상단 카드·하단 상태바 실데이터 + 상태바 클릭 상세 패널(윈도우별 사용률·리셋·근거·버전·갱신·다음 재검사·마지막 오류·재검사 버튼) + 툴바 "추천: CLI" + 중지된 실행은 "중지됨"
- E2E 실측(2026-09-04, 캡처 `D:\temp\claude\d--OneDrive-----------0bin\<session>\scratchpad\agentdock-2x.png`): 채팅 시작→응답, `--resume` 후속 질문, 여러 줄 프롬프트(stdin) → 두 줄 응답, Claude rate_limit → 상태바 "14% · 17:40 ↻ · 공식", 중지 → "중지됨", 폴더 대화상자(D:\dev에서 열림), probe → Codex/Claude "CLI 제시", Gemini "추정"

## 알려진 한계·TODO

- 승인 실시간 중계(2026-09-04): **Claude·Codex**. Codex는 대화 자체를 `codex app-server`(thread/start → turn/start, 재개 thread/resume, 서버 요청 `item/commandExecution/requestApproval` → `{decision}`)로 돌리며 `stdin_follow_up`으로 thread id를 받아 turn/start를 이어 보낸다. 턴 중 `account/rateLimits/updated`로 사용률 실시간 갱신. Claude는 `--permission-prompt-tool stdio` + stream-json 입력(프롬프트는 user 메시지 JSON). 러너가 stdin을 열어 두고 `result` 뒤 닫는다. `respond_permission`(허용·거부·세션 동안 허용=updatedPermissions), 10분 무응답 자동 거부. CLI 동작: acceptEdits는 파일시스템 Bash(`echo >`, mkdir)도 자동 승인, plan은 읽기 전용 명령(echo 등) 자동 실행 → 카드는 python·npm·git 쓰기 등에서 뜬다. Codex(app-server 전환 필요)·Gemini(ACP `session/request_permission`)·OpenCode(serve) 미지원
- Antigravity 앱 내 대화 E2E 통과(2026-09-04 21:16, 화면 자동 조작): 세션 재개·도구 호출 표시 확인. 읽기 전용(plan) 모드에서는 명령 실행 권한이 헤드리스에서 자동 거부되며 그 안내가 system 줄로 표시됨(쓰기 허용 시 --dangerously-skip-permissions). 사용량은 TUI `/usage`만 있어 헤드리스 신호 없음(추정 경로). AI Pro는 5시간 창 + 주간 상한
- Gemini CLI는 개인 Google 로그인이 막혀(IneligibleTierError) API 키·flash 전용으로 남김. 필요 없으면 CLI 설정에서 끄기
- 대화 중 CLI 전환 앱 E2E 통과: Claude(암호어 KIWI-77) → Antigravity 전환 후 암호어 정답 → Claude 복귀(기존 세션 재개, 그사이 대화 3항목 전달) → BACK_OK. OpenCode 앱 내 대화도 통과(OC_APP_OK)
- 버전은 어댑터별 `version_command`(`--version`) probe로 전 CLI 표시(2026-09-04)
- Gemini 세션 재개는 UUID를 못 받아 `--resume latest` 의존
- Gemini 텔레메트리(2026-09-04 검토, 미구현): `GEMINI_TELEMETRY_ENABLED=true` `GEMINI_TELEMETRY_TARGET=local` `GEMINI_TELEMETRY_OUTFILE=<경로>`(또는 프로젝트 settings.json `telemetry`)로 켜면 OTLP JSON에 `gemini_cli.api_error`(model_name, 429 본문 QuotaFailure.violations[].quotaId/…PerDay…|…PerMinute…, RetryInfo.retryDelay), `gemini_cli.api_response`(model, 토큰), `gemini_cli.flash_fallback`, `gemini_cli.model_routing` 기록. 이 계정은 무료 API 키라 pro가 limit 0 → 매번 429 후 flash 폴백. 모델별 한도 감지·장부의 재료
- Gemini 사용량은 능동 조회 불가(2026-09-04 재확인): CLI가 내부적으로 Code Assist `retrieveUserQuota`를 불러 대화형 화면에만 표시하고, 헤드리스 stream-json·ACP(`gemini --acp`: initialize/session/new 정상) 어디에도 노출하지 않는다. OAuth 토큰은 평문 파일에 없음. 앱은 429·"exhausted your daily quota" 문구와 retry-after 값으로 반응적 쿨다운만 잡는다. ACP는 `loadSession:true`·세션 id를 주므로 `--resume latest` 한계의 대안 후보
- Codex stderr tracing 로그는 `filter_stderr`로 ERROR·WARN만 "Codex ERROR: …"로 접어 표시(2026-09-04). 러너는 stderr의 ANSI를 제거
- Codex 모델별 한도(`rateLimitsByLimitId`, 5시간·7일 윈도우)는 아직 표시하지 않고 계정 단위 `rateLimits`만 쓴다
- 라우팅 체인은 localStorage에만 저장(SQLite 배선 전). 폴더 잠금(`Runner::running_count`)도 미배선
- 폴더당 동시 1개 잠금(`Runner::running_count`)·SQLite 영속화 미배선
- 로그인 버튼 E2E 통과(2026-09-04 21:30~22:01): Codex 로그아웃 → 전체 재검사(빨강) → 버튼 → 콘솔 `codex login` → 브라우저 로그인 → Ready 복귀. 콘솔이 열린 동안 8초마다 재검사(LOGIN_POLL), 스크립트 끝 `exit`로 창 자동 닫힘, 러너가 찾은 실행 파일 경로를 `call`(콘솔 PATH에 winget `agy` 없음). OpenCode(콘솔 열린 채 확인)·Antigravity(콘솔 닫힘) 경로도 통과. Claude는 이 개발 세션이 같은 로그인을 쓰고 있어 로그아웃 E2E 생략. Gemini 버튼은 `gemini` 콘솔 + `/auth` API 키 안내(개인 OAuth 종료)
- OpenCode `--agent plan`이 읽기 전용 내장 에이전트라는 전제
- 슬래시 명령·스킬(2026-09-04 실측): Claude 커스텀 명령·스킬은 stdin 프롬프트로도 동작(앱에서 `/trigger` 등 OK), Gemini 커스텀 명령 OK, Codex 스킬은 `$이름` 언급, OpenCode run 모드는 `/이름` 미확장. 내장 UI 명령(`/help` `/model` `/auth`…)은 전부 대화형 전용 → 앱 기능(모델 선택·로그인·상태바)으로 대체
- 보류: 설정 패널 "CLI 추가"(범용 사용자 정의 어댑터). CliId enum → 문자열 id 리팩터링이 선행 과제
- E2E 자동화 메모: DPI 비인식 프로세스의 `CopyFromScreen`은 125% 모니터에서 캡처가 잘린다(`SetProcessDPIAware` 선행). PowerShell 변수는 대소문자를 구분하지 않아 `$h`/`$H`가 충돌한다. 한글 IME 상태의 `SendKeys`는 자모로 입력되므로 `Set-Clipboard` + `^v`로 붙여넣는다. PowerShell `-match`는 대소문자를 무시하므로 "Not logged in"이 `Logged in`에 걸린다(`-cmatch`). 콘솔 창은 conhost 소유라 cmd 프로세스의 MainWindowHandle이 0이다

## 다음 단계 (PRD 14장 "구현 현황"과 동일)

1. 승인 중계 확장: Gemini 대화를 ACP(`session/prompt` + `session/request_permission`)로 전환, OpenCode serve 모드 검토
2. 2단계 자동 폴백: cooldown·429 시 handoff 패킷(`referenced_files` 포함) 생성 → 다음 ready CLI 실행, 무인 정책(쓰기 작업 Git 자동 체크포인트), 서킷 브레이커(동일 오류 3회 → blocked)
3. Codex 모델별 한도 표시 여부, Antigravity plan 모드 명령 허용 규칙 검토, (선택) Claude 로그인 버튼 실제 재로그인 E2E
4. 트레이 상주, SQLite 영속화(라우팅 체인·대화·작업·스냅샷), 폴더 잠금
