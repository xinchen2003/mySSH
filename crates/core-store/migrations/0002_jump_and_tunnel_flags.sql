-- M2 收口：跳板链 + 隧道随会话建立
ALTER TABLE sessions ADD COLUMN jump_chain TEXT NOT NULL DEFAULT '[]'; -- JSON array of session id（就近→最远）
ALTER TABLE tunnels  ADD COLUMN with_session INTEGER NOT NULL DEFAULT 0; -- 随会话自动建立
