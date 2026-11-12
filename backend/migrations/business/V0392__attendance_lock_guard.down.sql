-- ============================================================================
-- V0392__attendance_lock_guard.down.sql
--
-- 回滚「考勤记录锁定守卫」（卡 HR-23）：触发器 → 函数，自底向上。
--
-- 幂等：三个 `IF EXISTS`，重复执行 0 行 0 异常。
-- 不回滚数据：本迁移**只加守卫、不动一行数据**，所以 down 不需要任何数据修复。
-- 回滚后的行为差异（写在证据里，别让人以为守卫从来没存在过）：
--   · 期间 LOCKED 后，**绕过 handler 的 SQL**（导入 upsert / bridge / 手工脚本）
--     又能改记录了 —— 只剩 HR-20 `UPDATE_SQL` 里那条语句级 `NOT EXISTS(… 'LOCKED')`。
-- ============================================================================

DROP TRIGGER IF EXISTS trg_biz_att_record_lock_guard ON biz_attendance_record;
DROP FUNCTION IF EXISTS fn_biz_att_record_lock_guard();
