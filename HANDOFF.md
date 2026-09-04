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
4. git: `main`, 커밋 5개 (기준선 `da632ec` → 가용성 모니터 `2e992b0` → HANDOFF → Codex 사용량 `dfc1d11` → 드래그 우선순위). `core.autocrlf=false`. 원격 없음 — 올린다면 private.

## 현재 상태 (2026-09-04, E2E 통과)

- 백엔드(`src-tauri/src`): `models` · `availability`(CLI별 스냅샷 상태 머신 + 오류 분류) · `scheduler`(`pick_candidate` 배선) · `db`(스키마만, 미배선) · `adapters/{claude,codex,gemini}`(명령 조립·이벤트 파싱·probe 해석) · `runner`(tokio 실행, stdin 전달, 중지 플래그, `run_capture` 타임아웃, PATH에서 .exe/.cmd 해석)
- Tauri 커맨드: `start_job` · `continue_job` · `cancel_run` · `get_availability` · `recheck_availability` · `pick_cli`. 이벤트: `agent-event`(`{run_id, cli, event}`), `availability-changed`(스냅샷 배열)
- 가용성 모니터(`lib.rs` setup): 시작 시 전체 probe → 30초 틱. probe = claude `auth status`(JSON `loggedIn`) / codex `login status` / gemini `--version`. 실행 스트림의 `rate_limit_event`(공식)와 실패 원문(한도·인증·네트워크 분류, 리셋 epoch 힌트 추출)을 반영. 주기 probe 10분, 추정 쿨다운 30분, 리셋 직후 10분 내 재발 시 6시간 장기 쿨다운, 모든 윈도우 리셋 시에만 복귀
- 프로세스 실행: Windows에서 PATH를 뒤져 실제 파일 경로(.exe → .cmd → .bat)로 실행. .cmd는 Rust std가 cmd.exe 경유 + 안전 이스케이프를 맡는다(CVE-2024-24576 대응). 줄바꿈 인자는 그 단계에서 거부되므로 Claude 프롬프트는 stdin으로 넘긴다(실측: `echo … | claude -p` 정상)
- Codex 공식 사용량: `codex app-server`(stdio JSON-RPC)에 `initialize` → `initialized` → `account/rateLimits/read` 3줄을 일괄 전송(`runner::exchange_lines`, 1.2초). 응답 `rateLimits.primary/secondary{usedPercent, windowDurationMins, resetsAt}`를 five_hour/seven_day 이름으로 정규화해 `apply_rate_limit` → 상태바 "46% · 리셋 ↻ · 공식". `rateLimitReachedType`이 있으면 100%로. probe가 Ready인 CLI만 읽는다
- 우선순위 드래그: 상단 카드(순위 번호)를 HTML5 드래그로 옮기면 `set_routing_chain`이 라우팅 프로필·모니터 순서·추천 CLI를 갱신하고 localStorage(`agentdock.chain`)에 저장, 시작 시 복원. Windows 웹뷰에서 HTML 드래그가 되려면 `tauri.conf.json` 창의 `dragDropEnabled:false` 필요(적용됨). 기본 창 1100×760
- 프론트(`src/App.tsx`): 채팅 UI(세션 재개·폴더 고정) + 상단 카드·하단 상태바 실데이터 + 상태바 클릭 상세 패널(윈도우별 사용률·리셋·근거·버전·갱신·다음 재검사·마지막 오류·재검사 버튼) + 툴바 "추천: CLI" + 중지된 실행은 "중지됨"
- E2E 실측(2026-09-04, 캡처 `D:\temp\claude\d--OneDrive-----------0bin\<session>\scratchpad\agentdock-2x.png`): 채팅 시작→응답, `--resume` 후속 질문, 여러 줄 프롬프트(stdin) → 두 줄 응답, Claude rate_limit → 상태바 "14% · 17:40 ↻ · 공식", 중지 → "중지됨", 폴더 대화상자(D:\dev에서 열림), probe → Codex/Claude "CLI 제시", Gemini "추정"

## 알려진 한계·TODO

- Codex·Gemini 프롬프트는 아직 인자로 전달 → 줄바꿈이 든 메시지는 spawn 단계에서 오류("batch file arguments are invalid" 계열). stdin 전달 실측 후 어댑터 전환
- Claude·Codex probe는 버전을 안 돌려줘 상세 패널이 "버전 ?" → `--version` 별도 probe 추가 여지
- Gemini 세션 재개는 UUID를 못 받아 `--resume latest` 의존
- Codex stderr 진단 로그가 채팅 system 라인으로 노출 — 필터/접기 필요
- Codex 모델별 한도(`rateLimitsByLimitId`, 5시간·7일 윈도우)는 아직 표시하지 않고 계정 단위 `rateLimits`만 쓴다
- 라우팅 체인은 localStorage에만 저장(SQLite 배선 전). 폴더 잠금(`Runner::running_count`)도 미배선
- 폴더당 동시 1개 잠금(`Runner::running_count`)·SQLite 영속화 미배선
- E2E 자동화 메모: DPI 비인식 프로세스의 `CopyFromScreen`은 125% 모니터에서 캡처가 잘린다(`SetProcessDPIAware` 선행). PowerShell 변수는 대소문자를 구분하지 않아 `$h`/`$H`가 충돌한다. 한글 IME 상태의 `SendKeys`는 자모로 입력되므로 `Set-Clipboard` + `^v`로 붙여넣는다

## 다음 단계 (PRD 14장 "구현 현황"과 동일)

1. 승인 실시간 중계(`--input-format stream-json`) 검증
2. 2단계 자동 폴백: cooldown·429 시 handoff 패킷(`referenced_files` 포함) 생성 → 다음 ready CLI 실행, 무인 정책(쓰기 작업 Git 자동 체크포인트), 서킷 브레이커(동일 오류 3회 → blocked)
3. Codex·Gemini 프롬프트 stdin 전달 실측·전환, Claude/Codex `--version` probe, Codex stderr 진단 로그 접기, 모델별 한도 표시 여부
4. 트레이 상주, SQLite 영속화(라우팅 체인·대화·작업·스냅샷), 폴더 잠금
