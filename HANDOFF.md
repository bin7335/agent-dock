# Agent Dock — 핸드오프 노트 (2026-09-02)

PRD·설계 근거·스파이크 실측·구현 현황의 원본은 위키 `wiki/【프로그래밍】 AI CLI 오케스트레이터 PRD.md`다. 이 문서는 코드 저장소에서 바로 이어받기 위한 요약이다.

## 먼저 할 일

1. 개발 환경 위치 (2026-09-03 D:로 이전 완료)
   - Rust: `RUSTUP_HOME=D:\tools\rustup`, `CARGO_HOME=D:\tools\cargo`, PATH에 `D:\tools\cargo\bin` (사용자 환경변수). 새 PowerShell 창이면 그대로 `cargo`가 잡힌다.
   - 사용자 `TEMP`/`TMP` = `D:\temp`
   - VS Build Tools 2022: `D:\tools\VS2022BuildTools` (C: 인스턴스는 제거됨). 설치 캐시 보관 정책 `HKLM\SOFTWARE\Microsoft\VisualStudio\Setup\KeepDownloadedPayloads=0`. Windows SDK는 MSI 고정 경로라 `C:\Program Files (x86)\Windows Kits\10`(1.7 GB)에 남는 게 정상
   - C: 여유 0 GB → 5.5 GB 회복. 남은 선택 정리: 옛 `C:\Users\User\AppData\Local\Temp` 1.6 GB, VS 설치 캐시 잔여 1.0 GB(`C:\ProgramData\Microsoft\VisualStudio\Packages`)
   - 참고: 이 PC에는 VS 2022 Community(C++ 워크로드 없음)도 있음 — 빌드와 무관
   - `src-tauri\target`(6.2 GB)은 D:
2. 실행: 새 PowerShell 창에서
   ```powershell
   cd D:\dev\agent-dock
   npm run tauri dev
   ```
   (이전 셸에 옛 `C:\Users\User\.cargo\bin` PATH가 남아 있으면 `$env:PATH = "D:\tools\cargo\bin;$env:PATH"`)
3. 검증: `npx tsc --noEmit` (프론트), `cd src-tauri; cargo test` (러너 단위 테스트). 실제 claude를 태우는 통합 테스트는 `cargo test real_claude -- --ignored --nocapture`.

## 현재 상태

- 백엔드(`src-tauri/src`): `models` · `availability`(윈도우별 한도) · `scheduler`(우선순위 재평가, 95% 선제 handoff 판정) · `db`(스키마) · `adapters/{claude,codex,gemini}` · `runner`(tokio 프로세스 실행, stderr 분리, 중지, ProcessExited 보장)
- Tauri 커맨드: `start_job` · `continue_job`(세션 재개) · `cancel_run`. 이벤트는 `agent-event`로 emit(`{run_id, cli, event}`)
- 프론트(`src/App.tsx`): 채팅형 UI — 대화 목록 / 트랜스크립트 / 입력창(Enter 전송). 폴더 선택은 `@tauri-apps/plugin-dialog`, 선택값·CLI는 localStorage 저장. `types.ts`는 Rust 이벤트 직렬화 형태와 1:1
- 실측으로 확인된 것: 3대 CLI 헤드리스 구조화 실행, GUI 프로세스 안에서 러너의 claude stdout 수신, Claude `rate_limit_event`(5시간/7일 사용률·리셋)

## 아직 눈으로 확인 안 된 것 (첫 E2E)

- 채팅 UI에 세션 시작→응답→완료가 그려지는지 (이전 "프로세스 종료만 표시" 증상은 프론트 매핑 레이스로 판정·수정했으나 육안 재확인 전)
- 폴더 선택 대화상자 (`capabilities/default.json`에 `dialog:default` 추가됨)
- 후속 메시지의 세션 재개: Claude `--resume <id>` / Codex `exec … resume <thread_id> <prompt>` / Gemini `--resume latest`
- 중지 버튼이 프로세스를 실제로 kill하는지

## 알려진 한계·TODO

- Gemini 세션 재개는 UUID를 못 받아 `latest`에 의존 (같은 프로젝트에서 다른 gemini 세션이 끼면 어긋남)
- `runner::os_command`의 `cmd /c` 래핑: 인자에 cmd 특수문자(`& ^ |`)가 오면 이스케이프 보강 필요
- Codex stderr 진단 로그(MCP auth 등)가 채팅에 system 라인으로 노출됨 — 필터/접기 필요
- 저장소가 아직 git 초기화 전. private로 초기화 후 첫 커밋 권장
- `lib.rs`의 `#![allow(dead_code)]`는 스캐폴딩용 — availability/scheduler/db 배선 후 제거

## 다음 단계 (PRD 14장 "구현 현황"과 동일)

1. C: 확보 → E2E 육안 확인
2. availability monitor(probe 주기, rate_limit 수집, cooldown·복귀) → 상태바 실데이터화
3. 승인 실시간 중계(`--input-format stream-json`), Codex app-server `account/rateLimits/read`
4. 트레이 상주, SQLite 영속화, 라우팅 프로필·자동 폴백·handoff
