-- 登录宏：认证进 shell 后自动逐行执行的命令序列（纯文本，非秘密——与 su_user 同级）。
-- 执行语义见 .scratch/login-macro/spec.md：无 su 即发；有 su 则密码应答后发；重连重放。
ALTER TABLE sessions ADD COLUMN login_macro TEXT;
