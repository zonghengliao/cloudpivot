-- 10_cancel_orphan_tasks.up.sql
-- P11 数据修复①：实例已终态，但待办还挂在 PENDING（孤儿待办）
--
-- 背景（老板 2026-10-31 报 TSK-202610-000149）：
--   任务被推 CANCELLED 时只级联取消了子孙任务，没有撤它的审批实例与待办，
--   于是审批人（henry.liao）的「待我审批」里有一条点不动的孤儿待办。
--   代码修复见 backend/src/business/task/handler.rs（change_status 的 CANCELLED 分支）
--   与 backend/src/business/flow/task_bridge.rs（cancel_on_task_cancelled）。
--
-- 口径（严格按「实例已终态」这一组合，绝不批量清全库待办）：
--   biz_flow_task.status = 'PENDING'             ← 只动还挂着的人
--   biz_flow_instance.status IN (终态四态)         ← 且实例确实已经结束
--   biz_flow_instance.deleted_at IS NULL         ← 软删的实例由 T12 的删除路径处理，这里不碰
--
-- 红线：
--   1. APPROVED / REJECTED / SKIPPED / TRANSFERRED 的历史待办一个字都不改（审计事实）；
--   2. 实例还在 PENDING 的待办一行都不改（那是真待办）；
--   3. ck_biz_flow_task_done 要求 (status='PENDING') = (completed_at IS NULL)，所以必须同写 completed_at；
--   4. biz_flow_task 没有 updated_at 列（只有 created_at / completed_at），别照抄别的表的写法。
--
-- 幂等：跑第二遍 0 行受影响（第一遍之后不再有 PENDING + 终态实例的组合）。
-- 回滚：见同名 .down.sql。

update biz_flow_task ft
   set status = 'CANCELLED',
       comment = coalesce(ft.comment, '实例已结束，待办级联作废（P11 数据修复）'),
       completed_at = now()
  from biz_flow_instance i
 where ft.instance_id = i.id
   and ft.status = 'PENDING'
   and i.status in ('CANCELLED', 'APPROVED', 'REJECTED', 'WITHDRAWN')
   and i.deleted_at is null;

-- 自检：本语句执行后，「PENDING 待办 + 终态实例」必须为 0 行。
-- 用 DO 块而不是 SELECT，是为了让「还剩脏行」直接报错（数据修复脚本不许静默留残）。
do $$
declare
  n bigint;
begin
  select count(*) into n
    from biz_flow_task ft
    join biz_flow_instance i on i.id = ft.instance_id
   where ft.status = 'PENDING'
     and i.status in ('CANCELLED', 'APPROVED', 'REJECTED', 'WITHDRAWN')
     and i.deleted_at is null;
  if n > 0 then
    raise exception 'P11 数据修复失败：仍有 % 条「实例已终态 + 待办 PENDING」', n;
  end if;
end $$;
