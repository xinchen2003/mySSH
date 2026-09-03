-- 登录后切换用户（su）：su_user 存档案（明文，非秘密）；su 密码存 credentials，
-- kind='su_password'。为此 credentials 主键从 session_id 改为 (session_id, kind)，
-- 每会话可并存多条凭据（登录密码/私钥 passphrase/su 密码）。
ALTER TABLE sessions ADD COLUMN su_user TEXT;

CREATE TABLE credentials_new (
    session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,              -- password|key_passphrase|su_password
    blob        BLOB NOT NULL,
    updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (session_id, kind)
);
INSERT INTO credentials_new SELECT session_id, kind, blob, updated_at FROM credentials;
DROP TABLE credentials;
ALTER TABLE credentials_new RENAME TO credentials;
