-- ======================================================
-- V0111 回滚：移除 sys_change_log.operator_type（批次 C+ · T5）
--
-- 顺序：删索引 → 删约束 → 删列（与 up 的 ③②① 反向）。
-- 分区表上的 DROP INDEX / DROP CONSTRAINT / DROP COLUMN 在父表执行会级联到全部
-- 分区，只写父表即可。
--
-- 注意：**`operator_type` 列会被删掉**，历史行上「这条是不是 AI 做的」这一信息
-- 随之丢失 —— 这是本迁移唯一的破坏性操作，与 up 一一对应（up 加的列 down 必须
-- 能删，否则「空库 up→down→up」验证不成立）。
--
-- 数据风险提示：若回滚时库里已有 `operator_type = 'ROBOT'` 的行，说明确有 AI 写操作
-- 发生过；删列会让这些行在审计上退化成「操作人未知类型」。生产回滚前应先把
-- `SELECT * FROM sys_change_log WHERE operator_type = 'ROBOT'` 导出留档。
-- ======================================================

-- ---------- ③ 部分索引 ----------
DROP INDEX IF EXISTS idx_sys_change_log_ai_operator;

-- ---------- ② CHECK ----------
ALTER TABLE sys_change_log DROP CONSTRAINT IF EXISTS ck_sys_change_log_operator_type;

-- ---------- ① 列 ----------
ALTER TABLE sys_change_log DROP COLUMN IF EXISTS operator_type;
