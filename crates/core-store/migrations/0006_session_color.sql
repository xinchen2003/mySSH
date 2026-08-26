-- 批次九：会话颜色标记（侧栏色点/环境区分）；NULL = 无色，存量行为不变
ALTER TABLE sessions ADD COLUMN color TEXT;
