-- ============================================================================
-- E2ERES3 / 10：前置校验 + 判据视图（**只读断言 + DDL，不改业务数据**）
-- ----------------------------------------------------------------------------
-- 卡：t_44766ab3【P0-FIX-3】生产库混入 E2E 残留
--
-- 本包处置什么（2026-11-05 生产实测，383 行 + 6 个账号）：
--   biz_task 9 / biz_task_comment 221 / sys_notice 105 / sys_file 43 /
--   sys_export_task 2 / biz_flow_instance 1 / sys_announcement 1 /
--   sys_help_category 1 / biz_warehouse 0（4 个 E2E-W4A-* 早已软删 = 确认干净）
--   仍在启用的测试账号 6 个（cl_43870 / laoliu_test / qa.limited / qa.mutex /
--   qa.viewer / verify-ai）
--
-- 判据的**唯一实现**在 `scripts/verify/e2e_marker.py::TABLE_SPECS`；本文件是它的
-- SQL 镜像（视图 `datafix_e2eres3_spec` 一行一张表）。两边不一致会被 apply.sh
-- 末尾的 `e2e_marker.py --assert-clean` 抓住 —— 那是本包的收尾交叉断言：
-- **本包删干净了，但标记判据认为还有残留 ⇒ 直接红**（防止「跟着自己的窄判据绿」）。
--
-- 为什么不是「一个 %E2E% 全表扫」
--   生产库实测有 6 条真实内容正文提到 e2e（laoliu 的 4 条开发汇报挂在 T8-A/P4-B 等
--   真实任务、verify-ai 的 2 条真实阻塞通知），按正文匹配会误删真实数据。
--   所以评论判据是「父任务带标记」，通知判据是「标题带标记」（不看正文）。
--
-- 幂等：本文件只建表/视图（if not exists / or replace），可重复跑。
-- ============================================================================

\set ON_ERROR_STOP on

-- ---------------------------------------------------------------- 判据视图
create or replace view datafix_e2eres3_spec as
          select 'biz_task'::text as tbl,
                 '(title like ''[E2E]%'' or title like ''[E9E2E]%'')'::text as pred,
                 'alive'::text as scope
union all select 'biz_task_comment',
                 'task_id in (select id from biz_task where title like ''[E2E]%'' or title like ''[E9E2E]%'')',
                 'alive'
union all select 'biz_flow_instance',
                 '(title ilike ''%E2E%'')',
                 'alive'
union all select 'sys_notice',
                 '(title ilike ''%E2E%'')',
                 'alive'
union all select 'sys_announcement',
                 '(title ilike ''%E2E%'')',
                 'alive'
union all select 'sys_file',
                 '(original_name ilike ''e2e%'' or original_name ilike ''e9-%'')',
                 'alive'
union all select 'sys_export_task',
                 '(export_name ilike ''e2e%'' or file_name ilike ''e2e%'')',
                 'alive'
union all select 'sys_help_category',
                 '(code ilike ''e2e%'' or name ilike ''%E2E%'')',
                 'alive'
union all select 'biz_warehouse',
                 '(warehouse_code like ''E2E-%'' or warehouse_name ilike ''%E2E%'')',
                 'alive';

-- ---------------------------------------------------------------- 快照 / 账本
-- 快照：**每一行被改之前**的完整行（jsonb）—— 回滚只依赖它，不依赖「再算一次判据」
create table if not exists datafix_e2eres3_snapshot (
    batch       text        not null,
    tbl         text        not null,
    row_id      text        not null,
    before      jsonb       not null,
    captured_at timestamptz not null default now(),
    primary key (batch, tbl, row_id)
);
comment on table datafix_e2eres3_snapshot is
    'E2ERES3 改动前的行级快照（to_jsonb 整行）；20 down 按它逐行写回，不重算判据';

-- 账本：每张表删了多少 / 账务表与审计表改动前后行数（30_verify 拿它对账）
create table if not exists datafix_e2eres3_guard (
    batch       text        not null,
    item        text        not null,
    val         text        not null,
    captured_at timestamptz not null default now(),
    primary key (batch, item)
);
comment on table datafix_e2eres3_guard is
    'E2ERES3 账本：deleted_<表>=本批软删行数、before_<表>=改前总量、disabled_users=禁用账号数';

-- ---------------------------------------------------------------- 只读断言
do $$
declare
    v_batch   constant text := 'E2ERES3-20261105';
    r         record;
    v_alive   bigint;
    v_bad     text := '';
    v_n       bigint;
    v_bad_rows bigint;
begin
    -- ① 判据表必须都能落地（缺表说明这个库还没跑到对应模块 → 不能瞎判）
    for r in select tbl from datafix_e2eres3_spec loop
        if to_regclass('public.' || r.tbl) is null then
            raise exception 'E2ERES3/10: 缺表 % —— 本库没跑到该模块，别在生产上瞎跑', r.tbl;
        end if;
    end loop;

    -- ② 不允许「E2E 父任务 + 非 E2E 活子任务」：软删父任务会让真实子任务变成孤儿
    select count(*) into v_bad_rows from biz_task c
      join biz_task p on p.id = c.parent_id
     where p.deleted_at is null
       and (p.title like '[E2E]%' or p.title like '[E9E2E]%')
       and c.deleted_at is null
       and not (c.title like '[E2E]%' or c.title like '[E9E2E]%');
    if v_bad_rows > 0 then
        raise exception 'E2ERES3/10: 有 % 个**真实**子任务挂在 E2E 父任务下 —— 软删父任务会孤立它们，先人工定性', v_bad_rows;
    end if;

    -- ③ E2E 任务不许还有在飞流程（软删会把审批挂在半空）
    select count(*) into v_n from biz_task t
      join biz_flow_instance i on i.id = t.flow_instance_id
     where t.deleted_at is null
       and (t.title like '[E2E]%' or t.title like '[E9E2E]%')
       and i.deleted_at is null and i.status in ('PENDING', 'RUNNING');
    if v_n > 0 then
        raise exception 'E2ERES3/10: 有 % 个 E2E 任务还挂着在飞审批实例 —— 先撤回再清', v_n;
    end if;

    -- ④ 目标流程实例不许被**真实业务单据**引用（引用即证明它不是测试数据）
    select count(*) into v_n
      from biz_flow_instance i
     where i.deleted_at is null and i.title ilike '%E2E%'
       and (exists (select 1 from biz_contract       x where x.approval_instance_id = i.id)
         or exists (select 1 from biz_payable        x where x.cancel_instance_id = i.id
                                                    or x.payment_approval_instance_id = i.id)
         or exists (select 1 from biz_payment        x where x.approval_instance_id = i.id)
         or exists (select 1 from biz_purchase_order x where x.approval_instance_id = i.id)
         or exists (select 1 from biz_stock_in       x where x.approval_instance_id = i.id)
         or exists (select 1 from biz_stock_take     x where x.approval_instance_id = i.id)
         or exists (select 1 from biz_stock_transfer x where x.approval_instance_id = i.id)
         or exists (select 1 from biz_voucher        x where x.pending_reverse_instance_id = i.id));
    if v_n > 0 then
        raise exception 'E2ERES3/10: 有 % 个 E2E 流程实例被真实业务单据引用 —— 不是测试数据，停', v_n;
    end if;

    -- ⑤ 仓库口径确认：E2E 仓库应**早已软删**（本包不承担删它，只确认干净）
    select count(*) into v_alive from biz_warehouse
     where deleted_at is null
       and (warehouse_code like 'E2E-%' or warehouse_name ilike '%E2E%');
    if v_alive > 0 then
        raise notice 'E2ERES3/10: 有 % 个**活跃** E2E 仓库（比评审口径多）—— 会在 20 里一并软删', v_alive;
    else
        insert into datafix_e2eres3_guard(batch, item, val)
        values (v_batch, 'warehouse_already_clean', 'true')
        on conflict (batch, item) do update set val = excluded.val, captured_at = now();
    end if;

    -- ⑥ 测试账号：只处理「还在用」的；已软删的不碰（历史 create_by 引用要留着）
    select count(*) into v_n from sys_user
     where deleted_at is null and status = 1
       and username in ('laoliu_test','qa.viewer','qa.limited','qa.mutex','cl_43870','verify-ai');
    raise notice 'E2ERES3/10: 在用测试账号 % 个（将置 status=0，不删行）', v_n;

    -- ⑦ 现状盘点（人看；账本由 20 写）
    raise notice '---- E2ERES3/10 现状 ----';
    for r in select tbl, pred from datafix_e2eres3_spec order by tbl loop
        execute format('select count(*) from %I where deleted_at is null and %s', r.tbl, r.pred)
           into v_alive;
        raise notice '  % alive=%', rpad(r.tbl, 21), v_alive;
    end loop;
end $$;

-- 账本：改前的账务/审计基线（30_verify 用它证明「软删没动账」）
insert into datafix_e2eres3_guard(batch, item, val)
select 'E2ERES3-20261105', 'before_'||t, v
  from (select 'biz_voucher' as t, count(*)::text as v from biz_voucher
        union all select 'biz_voucher_entry', count(*)::text from biz_voucher_entry
        union all select 'biz_payable', count(*)::text from biz_payable
        union all select 'biz_payment', count(*)::text from biz_payment
        union all select 'biz_inventory', count(*)::text from biz_inventory
        union all select 'biz_inventory_txn', count(*)::text from biz_inventory_txn
        union all select 'biz_accounting_period', count(*)::text from biz_accounting_period
        union all select 'biz_opening_balance', count(*)::text from biz_opening_balance
        union all select 'biz_status_log', count(*)::text from biz_status_log
        union all select 'biz_task_all', count(*)::text from biz_task) b
on conflict (batch, item) do update set val = excluded.val, captured_at = now();

select '10_precheck OK' as result,
       (select count(*) from datafix_e2eres3_spec) as spec_tables,
       (select val from datafix_e2eres3_guard where batch='E2ERES3-20261105' and item='before_biz_voucher') as voucher_before;
