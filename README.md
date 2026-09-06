# Agent Dock

PC에 설치·로그인된 여러 AI 코딩 CLI(Codex, Claude Code, Antigravity, Gemini CLI, OpenCode)를 한 창에서 실행하고, 가용성(한도·로그인 상태)·우선순위·승인 요청을 관리하는 로컬 Tauri 앱입니다. 앱은 토큰을 저장하지 않고 각 CLI의 자체 로그인 상태를 그대로 씁니다.

설계·실측 기록은 `HANDOFF.md`(저장소 요약)와 위키 PRD(비공개)를 참고하세요.

## 다른 컴퓨터에서 실행하기 (Windows)

현재 실행 방식(콘솔 로그인 창, `.cmd` 셔틀 해석, winget 경로)이 Windows 전용이라 **Windows 10/11**에서만 동작합니다.

### 1. 개발 도구

| 도구 | 설치 | 비고 |
|---|---|---|
| Git | `winget install Git.Git` | |
| GitHub CLI | `winget install GitHub.cli` → `gh auth login` | 비공개 저장소를 받으려면 `bin7335` 계정으로 로그인 |
| Node.js 22 LTS | `winget install OpenJS.NodeJS.LTS` | npm 포함 |
| Rust | `winget install Rustlang.Rustup` → 새 창에서 `rustup default stable-msvc` | Rust 1.98에서 개발함 |
| VS Build Tools 2022 | `winget install Microsoft.VisualStudio.2022.BuildTools` 후 설치 관리자에서 **"C++를 사용한 데스크톱 개발"** 워크로드 선택(MSVC + Windows SDK) | Tauri/Rust 링크에 필요 |
| WebView2 | Windows 11은 기본 포함. 없으면 Microsoft "WebView2 Evergreen Runtime" 설치 | |

디스크는 8 GB 이상 비워 두세요(`src-tauri\target`이 6~7 GB). C: 여유가 적으면 설치 전에 사용자 환경 변수 `RUSTUP_HOME`, `CARGO_HOME`을 다른 드라이브(예: `D:\tools\rustup`, `D:\tools\cargo`)로 잡고 `CARGO_HOME\bin`을 PATH에 넣습니다.

### 2. 사용할 CLI 설치·로그인

앱은 설치되어 로그인된 CLI만 씁니다. 하나만 있어도 동작하며, 안 쓰는 CLI는 앱의 ⚙ CLI 설정에서 끌 수 있습니다.

| CLI | 설치 | 로그인 |
|---|---|---|
| Claude Code | `npm i -g @anthropic-ai/claude-code` | `claude auth login` (Anthropic 구독) |
| Codex | `npm i -g @openai/codex` | `codex login` (ChatGPT 계정) |
| Antigravity (Google AI Pro) | `winget install Google.AntigravityCLI` | 터미널에서 `agy` 첫 실행 → 브라우저 로그인 |
| Gemini CLI (선택) | `npm i -g @google/gemini-cli` | `gemini` → `/auth` (개인 Google 로그인은 종료되어 API 키만 가능) |
| OpenCode (선택) | `npm i -g opencode-ai` | `opencode auth login` (자체 제공자 키) |

새 터미널에서 `claude --version`, `codex --version`, `agy --version`이 찍히면 됩니다. 앱은 PATH 외에 `%LOCALAPPDATA%\Microsoft\WinGet\Links`도 찾아보므로 winget으로 설치한 `agy`는 PATH를 따로 손대지 않아도 됩니다.

### 3. 받아서 실행

```powershell
gh repo clone bin7335/agent-dock
cd agent-dock
npm install
npm run tauri dev
```

첫 실행은 Rust 의존성을 내려받아 컴파일하느라 5~10분 걸립니다. 이후에는 `src-tauri` 변경 시 자동으로 재빌드·재시작됩니다.

설치 파일이 필요하면 `npm run tauri build` → `src-tauri\target\release\bundle\`(msi·nsis)에 생깁니다.

### 4. 검증

```powershell
npx tsc --noEmit          # 프론트 타입 검사
cd src-tauri; cargo test  # 백엔드 단위 테스트 (48개)
```

### 5. 처음 켰을 때

1. 상단 카드 5개의 상태를 봅니다. 초록 = 사용 가능, 빨강 = 로그인 필요(카드 아래 상태바 → 상세 패널 → 로그인 버튼으로 그 CLI의 로그인 콘솔을 띄울 수 있음), 주황 = 한도 쿨다운.
2. ⚙ CLI 설정에서 안 쓰는 CLI를 끄고, 모델을 고릅니다.
3. 하단 툴바에서 프로젝트 폴더를 고른 뒤 메시지를 보냅니다. 대화 중 툴바에서 CLI를 바꾸면 그때까지의 대화가 요약되어 넘어갑니다.
4. CLI가 도구 승인을 요청하면(Claude·Codex·Gemini) 대화 안에 승인 카드가 뜹니다. 10분 동안 답이 없으면 자동 거부됩니다.

### 데이터 위치

- 가용성 스냅샷: `%APPDATA%\com.user.agent-dock\availability.json`
- 로그인 콘솔 스크립트: 같은 폴더의 `login-<cli>.cmd`
- 우선순위·사용 여부·모델 선택: 앱 웹뷰의 localStorage (컴퓨터마다 따로)
- 자격증명: 각 CLI의 자체 위치(앱은 읽거나 복사하지 않음)

### 문제 해결

- `link.exe not found` / 링크 오류 → VS Build Tools의 C++ 워크로드가 빠진 것. 설치 후 새 터미널에서 다시 `npm run tauri dev`.
- 카드가 전부 빨강·회색 → 해당 CLI가 PATH에 없거나 로그인 전. 새 터미널에서 `<cli> --version`으로 확인.
- 창이 안 뜨고 종료 → WebView2 런타임 없음.
- 빌드 중 디스크 부족 → `CARGO_HOME`/`RUSTUP_HOME`을 옮기거나 `src-tauri\target`을 지우고 다시.
