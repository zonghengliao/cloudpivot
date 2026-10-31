-- 10_cancel_orphan_tasks.down.sql
-- P11 数据修复①的回滚：把本脚本改成 CANCELLED 的待办改回 PENDING。
--
-- 判据（三条同时成立才回滚，绝不误伤真实的 CANCELLED 待办）：
--   1. status = 'CANCELLED'
--   2. comment 正好是本脚本填的文案（原文为 NULL 时才被 COALESCE 写上）
--   3. 所属实例仍是终态（说明它本来就是孤儿）
--
-- ⚠️ 已知边界（写清楚，不装作无损）：
--   - 如果这条待办**原本就**有一条非空 comment，up 里 COALESCE 不会覆盖它，
--     于是 down 认不出来，这条不会被回滚 → 这类行请用备份（pg_dump）恢复；
--   - 回滚后这些行会重新变成孤儿待办（这正是修复前的状态）。
--     回滚的用途是「证明可逆 / 复现问题」，不是日常操作。
--   - ck_biz_flow_task_done 要求 (status='PENDING') = (completed_at IS NULL)，
--     所以必须同时把 completed_at 清回 NULL。

update biz_flow_task ft
   set status = 'PENDING',
       completed_at = null,
       comment = null
  from biz_flow_instance i
 where ft.instance_id = i.id
   and ft.status = 'CANCELLED'
   and ft.comment = '实例已结束，待办级联作废（P11 数据修复）'
   and i.status in ('CANCELLED', 'APPROVED', 'REJECTED', 'WITHDRAWN')
   and i.deleted_at is null;
