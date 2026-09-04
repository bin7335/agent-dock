/// 최소 데이터 모델 (PRD 8장). 1단계에서 tauri-plugin-sql 또는 sqlx 연결 시 사용한다.
pub const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS cli_installations (
    id            TEXT PRIMARY KEY,          -- claude | codex | gemini | opencode
    path          TEXT NOT NULL,
    version       TEXT,
    last_checked  INTEGER,                   -- epoch seconds
    auth_state    TEXT NOT NULL DEFAULT 'unknown'
);

CREATE TABLE IF NOT EXISTS routing_profiles (
    name              TEXT PRIMARY KEY,
    chain_json        TEXT NOT NULL,         -- ["codex","claude",...]
    max_auto_handoffs INTEGER NOT NULL DEFAULT 2
);

-- 한도 윈도우 복수화: CLI당 윈도우별 1행 (PRD 8·11장)
CREATE TABLE IF NOT EXISTS availability_snapshots (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    cli          TEXT NOT NULL,
    state        TEXT NOT NULL,
    evidence     TEXT NOT NULL,              -- official | cli_reported | estimated
    window_name  TEXT,                       -- five_hour | seven_day | NULL(윈도우 무관)
    utilization  REAL,
    resets_at    INTEGER,
    checked_at   INTEGER NOT NULL,
    raw_event_id INTEGER
);

CREATE TABLE IF NOT EXISTS jobs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    title         TEXT NOT NULL,
    request       TEXT NOT NULL,
    project_dir   TEXT NOT NULL,
    profile       TEXT NOT NULL,
    allow_writes  INTEGER NOT NULL DEFAULT 0,
    unattended_ok INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL DEFAULT 'queued',
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS runs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id      INTEGER NOT NULL REFERENCES jobs(id),
    cli         TEXT NOT NULL,
    command     TEXT NOT NULL,
    pid         INTEGER,
    exit_code   INTEGER,
    started_at  INTEGER,
    finished_at INTEGER
);

CREATE TABLE IF NOT EXISTS handoffs (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    from_run_id  INTEGER REFERENCES runs(id),
    to_run_id    INTEGER REFERENCES runs(id),
    reason       TEXT NOT NULL,
    packet_json  TEXT NOT NULL,              -- HandoffPacket, 비밀값 제외
    approved     INTEGER                     -- NULL=대기, 0=거부, 1=승인(무인 자동 포함)
);

-- 비밀값을 제거한 상태 변화·오류·승인 이벤트
CREATE TABLE IF NOT EXISTS events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id     INTEGER REFERENCES runs(id),
    kind       TEXT NOT NULL,
    body_json  TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
"#;
