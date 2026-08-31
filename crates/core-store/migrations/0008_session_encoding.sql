-- 会话级终端编码：encoding_rs 标签（utf-8|gbk|gb18030|big5|shift_jis|euc-kr）；
-- 'utf-8' = 数据通路直通不转码。SSH 与本地（ConPTY）会话共用此列。
ALTER TABLE sessions ADD COLUMN encoding TEXT NOT NULL DEFAULT 'utf-8';
