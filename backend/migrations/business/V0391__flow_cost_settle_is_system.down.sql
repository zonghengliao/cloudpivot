-- ============================================================================
-- V0391__flow_cost_settle_is_system.down.sql
--
-- 回滚【ALERT t_e9d59061】：把 cost_settle_approve 的 `is_system` 改回 FALSE。
--
-- 说明：
--   · 这正是 up 的逆操作，值回到 V0381 种下时的样子（DEFAULT FALSE）；
--   · 幂等：`AND is_system` 保证重复执行 0 行变更；
--   · 回滚**不**动 `updated_at`（与 V0220 / V0233 同口径）：回滚本身就是一次
--     变动，保留当前时间比造一个假的历史时间更诚实；
--   · 回滚之后 H5/H6 会重新报错（模板库/流程定义页面上这条「标准」流程又会
--     显示成客户自建）—— 所以 down 只用于「完整回退本卡 + 本迁移」，
--     不要单独在生产跑。
-- ============================================================================

UPDATE biz_flow_def
   SET is_system = FALSE
 WHERE flow_code = 'cost_settle_approve'
   AND deleted_at IS NULL
   AND is_system;
