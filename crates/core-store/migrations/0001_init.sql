-- core-store 初始 schema（M2）。
-- 约定：audit 表 append-only（代码层不提供 UPDATE/DELETE）；凭据密文只在 credentials。

CREATE TABLE meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- 会话档案（不含任何秘密材料；密码/passphrase 在 credentials，私钥存路径引用）
CREATE TABLE sessions (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    host        TEXT NOT NULL,
    port        INTEGER NOT NULL DEFAULT 22,
    username    TEXT NOT NULL,
    auth_type   TEXT NOT NULL,              -- password|publickey|keyboard-interactive|agent
    key_path    TEXT,                       -- publickey 时的私钥文件路径
    group_path  TEXT NOT NULL DEFAULT '',   -- '/' 分隔的分组树
    tags        TEXT NOT NULL DEFAULT '[]', -- JSON array
    command     TEXT,                       -- 登录后执行（缺省 shell）
    created_at  TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE tunnels (
    id          TEXT PRIMARY KEY,
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,              -- local|remote|dynamic
    bind_host   TEXT NOT NULL,
    bind_port   INTEGER NOT NULL,
    target_host TEXT,                       -- dynamic 无固定目标
    target_port INTEGER,
    autostart   INTEGER NOT NULL DEFAULT 0,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 凭据保险库：DPAPI(CurrentUser) 加密密文；schema 标记在 meta.vault_scheme
CREATE TABLE credentials (
    session_id  TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,              -- password|key_passphrase
    blob        BLOB NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- append-only 审计（安全模型第 5 条；无外键——会话删除不抹审计）
CREATE TABLE audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          TEXT NOT NULL DEFAULT (datetime('now')),
    actor       TEXT NOT NULL,              -- gui|cli|mcp
    session_id  TEXT,
    action      TEXT NOT NULL,
    detail      TEXT NOT NULL DEFAULT '{}'  -- JSON
);

CREATE INDEX idx_audit_ts ON audit(ts);
CREATE INDEX idx_sessions_group ON sessions(group_path);
