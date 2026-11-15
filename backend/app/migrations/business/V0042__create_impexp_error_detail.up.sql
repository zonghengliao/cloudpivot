-- V0042：导入错误明细表（批次 A · A3 导入导出引擎）
--
-- 依据：docs/批次A-技术架构.md §7 附录 A · V0041 的 sys_import_task_error 原文
--       + §3.6「JSONB 摘要（前 100 条）+ 明细表」双写取舍
--
-- 为什么明细单独建表而不是塞进 JSONB：
--   ① 要按行号分页回显（10 万错误不能一次塞给前端）；
--   ② 要按 error_code 统计（JSONB 做不了索引统计）；
--   ③ 「下载错误行」要按 row_no 流式扫描。
--   摘要（前 100 条）留在 sys_import_task.error_summary，列表页一次查询即可回显。

CREATE TABLE sys_import_task_error (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(), -- 主键
    task_id UUID NOT NULL REFERENCES sys_import_task(id) ON DELETE CASCADE, -- 所属任务（任务删则明细删）
    row_no INT NOT NULL, -- Excel 真实行号（含表头，从 1 开始，与用户在 Excel 里看到的一致）
    column_key VARCHAR(64), -- 列键（业务定义里的字段名；整行级错误为空）
    column_header VARCHAR(100), -- 列中文名（回显用，定义改名后历史明细仍可读）
    raw_value TEXT, -- 触发错误的单元格原文（截断 200 字符）
    error_code VARCHAR(32) NOT NULL, -- REQUIRED / TYPE / LENGTH / ENUM / RANGE / DATE / DICT / UNIQUE / FK_NOT_FOUND / RULE / ROW_LIMIT
    error_message VARCHAR(500) NOT NULL, -- 面向用户的文案
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP -- 创建时间
);

COMMENT ON TABLE sys_import_task_error IS '导入错误明细：行号 + 列 + 原因，供前端逐行回显与导出「错误行」Excel';
COMMENT ON COLUMN sys_import_task_error.row_no IS 'Excel 行号（含表头，从 1 开始，与用户在 Excel 里看到的行号一致）';
COMMENT ON COLUMN sys_import_task_error.column_key IS '列键（业务定义里的字段名，整行级错误为空）';
COMMENT ON COLUMN sys_import_task_error.error_code IS '错误码：REQUIRED / TYPE / LENGTH / ENUM / RANGE / DATE / DICT / UNIQUE / FK_NOT_FOUND / RULE / ROW_LIMIT';

CREATE INDEX idx_sys_import_task_error_task ON sys_import_task_error (task_id, row_no);
