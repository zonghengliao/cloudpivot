-- ======================================================
-- V0110 回滚：依赖约束 / 索引 / 权限节点（批次 C+ · T4）
--
-- 顺序：先撤授权行 → 删菜单节点 → 删索引 → 删约束 → 删列。
-- （`sys_role_menu` 若对 `sys_menu` 是 ON DELETE CASCADE 则第一句是冗余的，
--   但回滚脚本宁可多一句 DELETE，也不要赌外键行为 —— V0109 的 down 同款口径。）
--
-- 注意：**`lag_days` 列会被删掉**，依赖记录的滞后天数值随之丢失。
-- 这是本迁移唯一的破坏性操作，与 up 一一对应（up 加的列 down 必须能删，
-- 否则「空库 up→down→up」验证不成立）。
-- ======================================================

-- ---------- ① 授权行 ----------
DELETE FROM sys_role_menu
 WHERE menu_id IN ('20000000-0000-0000-0000-000000000770'::uuid,
                   '20000000-0000-0000-0000-000000000771'::uuid);

-- ---------- ② 菜单节点 ----------
DELETE FROM sys_menu
 WHERE id IN ('20000000-0000-0000-0000-000000000770'::uuid,
              '20000000-0000-0000-0000-000000000771'::uuid);

-- ---------- ③ 索引 ----------
DROP INDEX IF EXISTS uk_biz_task_dependency_triplet;
DROP INDEX IF EXISTS idx_biz_task_dependency_on;

-- ---------- ④ 约束（只删本迁移加的 lag 约束；反自反是 V0100 的，保留） ----------
ALTER TABLE biz_task_dependency DROP CONSTRAINT IF EXISTS ck_biz_task_dependency_lag;

-- ---------- ⑤ 列 ----------
ALTER TABLE biz_task_dependency DROP COLUMN IF EXISTS lag_days;
