-- ============================================================================
-- V0220__flow_standard_flag_fix.down.sql
--
-- 回滚【P2-6a】：把 contract_approve / stock_take_post 的 `is_system` 改回 FALSE。
--
-- 说明：
--   · 这正是 up 的逆操作，值回到 V0209 / V0211 种下时的样子（DEFAULT FALSE）；
--   · 幂等：`AND is_system` 保证重复执行 0 行变更；
--   · 回滚**不**动 `updated_at` —— 它是「最近变动时间」，回滚本身就是一次变动，
--     保留现在的时间比造一个假的历史时间更诚实；
--   · 回滚之后「模板库」会重新把这 2 条既露出来又不挂「标准」标签（up 之前的病态），
--     所以 down 只用于「回退本卡代码 + 回退本迁移」的完整回滚，不要单独在生产跑。
-- ============================================================================

UPDATE biz_flow_def
   SET is_system = FALSE
 WHERE flow_code IN ('contract_approve', 'stock_take_post')
   AND deleted_at IS NULL
   AND is_system;
