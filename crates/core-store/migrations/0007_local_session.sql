-- 批次十四：本地会话（kind='local' 时 host/port/username/auth_type 为占位值，
-- shell/workdir 生效；command 复用为「启动命令」，在 shell 内执行后保持交互）
ALTER TABLE sessions ADD COLUMN kind TEXT NOT NULL DEFAULT 'ssh'; -- ssh|local
ALTER TABLE sessions ADD COLUMN shell TEXT;   -- local：powershell|pwsh|cmd 或自定义路径；NULL = 自动
ALTER TABLE sessions ADD COLUMN workdir TEXT; -- local：启动目录；NULL = 用户主目录
