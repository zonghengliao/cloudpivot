-- ============================================================================
-- E2ERES2 / 25：跨批次账本 —— 把本批次删除量登记进上一张卡（E2ERES）的基线表
-- ----------------------------------------------------------------------------
-- 为什么需要：
--   scripts/datafix/e2e_residue/30_verify.sql（卡 t_61dca492）里有 9 条断言是
--   「静态表总量 == 删除前 - 本卡删除数」。本包又在同 9 张表上物理删了
--   36 定义 / 39 版本 / 98 节点 / 55 边 / 12 审批人 / 16 实例 / 37 实例节点 / 8 待办 / 0 抄送，
--   那条断言的 expected 必须跟着往下减 —— 否则 `e2e_residue/apply.sh --check`
--   会永久 FAIL（假红），那张卡的哨兵就废了。
--
-- 做法：
--   本批次**实际删除行数**（datafix_e2eres2_guard 的 deleted_*，up 里逐表与基线硬对账过）
--   写成 E2ERES 基线表的 followup_deleted_<表名> 项；
--   e2e_residue/30_verify.sql 的 expected 已同步改成「before - deleted_self - followup_deleted」。
--   down（回滚）会把 followup_deleted_* 归零。
--
-- 幂等：on conflict do update；up 是幂等空转（0 行删除）时这里也照样把账本对齐成实际删除量。
-- 单独成文件（而不是塞进 20 up）的原因：up 幂等空转时会提前 return，走不到账本那一段，
-- 而账本要能在任何时刻重算对齐。
-- ============================================================================

\set ON_ERROR_STOP on

do $$
declare
    c_self constant text := 'E2ERES2-20261104';
    c_prev constant text := 'E2ERES-20261104';
    c_tbls constant text[] := array[
        'biz_flow_def', 'biz_flow_def_version', 'biz_flow_node', 'biz_flow_edge',
        'biz_flow_node_assignee', 'biz_flow_instance', 'biz_flow_instance_node',
        'biz_flow_task', 'biz_flow_cc'];
    t text;
    v_n int;
    v_written int := 0;
begin
    if to_regclass('public.datafix_e2eres2_guard') is null then
        raise exception 'E2ERES2/25: 没有 datafix_e2eres2_guard —— 先跑 10_precheck.sql';
    end if;

    if to_regclass('public.datafix_e2e_residue_guard') is null then
        raise notice 'E2ERES2/25: 本库没有 datafix_e2e_residue_guard（上一张卡没跑过），跳过跨批次账本';
        return;
    end if;

    foreach t in array c_tbls loop
        select coalesce(val::int, 0) into v_n from datafix_e2eres2_guard
         where batch = c_self and item = 'deleted_' || t;
        v_n := coalesce(v_n, 0);
        insert into datafix_e2e_residue_guard (batch, item, val)
        values (c_prev, 'followup_deleted_' || t, v_n::text)
        on conflict (batch, item) do update set val = excluded.val, captured_at = now();
        v_written := v_written + 1;
        raise notice 'E2ERES2/25: followup_deleted_% = %', t, v_n;
    end loop;

    raise notice 'E2ERES2/25 完成：登记 % 项；上一张卡（%）的 30_verify 现在会把本批次删除量一起减掉', v_written, c_prev;
end $$;

select item, val from datafix_e2e_residue_guard
 where batch = 'E2ERES-20261104' and item like 'followup_deleted_%' order by item;
