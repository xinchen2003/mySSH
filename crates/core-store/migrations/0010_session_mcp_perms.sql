-- 会话级 MCP 工具权限覆盖：稀疏 JSON 映射 {"ssh_exec": false, ...}，
-- 缺省 {} = 全部跟随全局设置（mcp.allow.*）。键名与全局分组一致：
-- list_sessions|ssh_exec|sftp_read|sftp_write|sftp_transfer。
ALTER TABLE sessions ADD COLUMN mcp_perms TEXT NOT NULL DEFAULT '{}';
