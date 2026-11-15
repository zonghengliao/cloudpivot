-- ============================================================================
-- V0233__flow_seed_is_system_fix2.down.sql
--
-- 回滚【P2-6b-follow】：把 budget_approve / asset_disposal 的 `is_system` 改回 FALSE。
--
-- 说明：
--   · 这正是 up 的逆操作，值回到 V0228 / V0231 种下时的样子（DEFAULT FALSE）；
--   · 幂等：`AND is_system` 保证重复执行 0 行变更；
--   · 回滚**不**动 `updated_at`（与 V0220 同口径）：回滚本身就是一次变动，
--     保留当前时间比造一个假的历史时间更诚实；
--   · 回滚之后 P2-6b 收口过的 `GET /business/flow/def/templates`（默认 = 标准流程）
--     会重新漏掉这 2 条 —— 所以 down 只用于「回退本卡代码 + 回退本迁移」的完整回退，
--     不要单独在生产跑。
-- ============================================================================

UPDATE biz_flow_def
   SET is_system = FALSE
 WHERE flow_code IN ('budget_approve', 'asset_disposal')
   AND deleted_at IS NULL
   AND is_system;
