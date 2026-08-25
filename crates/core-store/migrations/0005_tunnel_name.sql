-- 批次三：隧道显示名（模板预填/面板展示）；存量行为不变，默认空串
ALTER TABLE tunnels ADD COLUMN name TEXT NOT NULL DEFAULT '';
