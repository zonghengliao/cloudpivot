-- ======================================================
-- V0176 回滚：删除 `sys_token_blacklist`（零残留）
--
-- 注意：回滚本迁移等于**放弃**「Redis 不可用时仍能判吊销」的能力，
-- 认证层会退回到全站 fail-closed 401（现状）。回滚前必须确认：
--   1. 已评估 docs/adr-20261101-redis-degradation-mode.md 的安全口径；
--   2. `token_blacklist_cleanup` 任务代码已同步移除（否则任务会持续报错）。
-- ======================================================

DROP TABLE IF EXISTS sys_token_blacklist;
