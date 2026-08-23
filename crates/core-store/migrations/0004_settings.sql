-- M5：应用设置（KV；值存 JSON 文本，键如 theme / font.size / keymap.custom）
CREATE TABLE IF NOT EXISTS settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);
