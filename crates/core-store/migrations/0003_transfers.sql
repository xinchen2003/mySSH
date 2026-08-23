-- M3：传输历史（断点续传状态落库；跨重启重入队由 UI/命令层按记录发起）
CREATE TABLE IF NOT EXISTS transfers (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL,
    direction   TEXT NOT NULL,             -- upload | download
    local       TEXT NOT NULL,
    remote      TEXT NOT NULL,
    bytes_done  INTEGER NOT NULL DEFAULT 0,
    bytes_total INTEGER NOT NULL DEFAULT 0,
    state       TEXT NOT NULL,             -- queued/running/paused/done/failed/canceled
    error       TEXT,
    updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_transfers_session ON transfers(session_id);
