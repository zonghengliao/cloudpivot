-- ============================================================================
-- E2ERES3 / 30：自检（**证明**清干净了，且账没被动过）
-- ----------------------------------------------------------------------------
-- 断言分三类：
--   A 残留 = 0      —— 每张标记表 alive 命中数都必须是 0
--   B 账号已禁用    —— 6 个测试账号 status=0、**行还在**（deleted_at is null）
--   C 账务 / 审计零影响 —— 凭证/应付/库存/期间/余额/状态日志 行数与改前一致
--
-- 任何一条 FAIL → 末尾 RAISE EXCEPTION → psql 退出码 1（apply.sh 直接失败）。
-- 只读（不写任何业务行；账本里只写 verify 结论）。
-- ============================================================================

\set ON_ERROR_STOP on

drop table if exists pg_temp.e2eres3_verify;
create temp table e2eres3_verify(ord int, item text, expect text, got text, ok boolean);

-- ---------------------------------------------------------------- A 残留 = 0
do $$
declare
    r record;
    v_alive bigint;
begin
    delete from e2eres3_verify;
    for r in select tbl, pred from datafix_e2eres3_spec order by tbl loop
        execute format('select count(*) from %I where deleted_at is null and %s', r.tbl, r.pred)
           into v_alive;
        insert into e2eres3_verify values (100, 'A 残留=0 ' || r.tbl, '0', v_alive::text, v_alive = 0);
    end loop;

    -- B 账号：禁用但没删
    insert into e2eres3_verify
    select 200, 'B 测试账号 status=0', '6',
           count(*) filter (where status = 0)::text,
           count(*) filter (where status = 0) = 6
      from sys_user
     where deleted_at is null
       and username in ('laoliu_test','qa.viewer','qa.limited','qa.mutex','cl_43870','verify-ai');

    insert into e2eres3_verify
    select 201, 'B 测试账号行仍在（未删）', '6',
           count(*)::text, count(*) = 6
      from sys_user
     where deleted_at is null
       and username in ('laoliu_test','qa.viewer','qa.limited','qa.mutex','cl_43870','verify-ai');

    -- C 账务/审计：行数与「改前」一致（账本 before_*）
    insert into e2eres3_verify
    select 300 + row_number() over (order by item), 'C 行数不变 ' || replace(item, 'before_', ''), val,
           (case item
              when 'before_biz_voucher'         then (select count(*) from biz_voucher)
              when 'before_biz_voucher_entry'   then (select count(*) from biz_voucher_entry)
              when 'before_biz_payable'         then (select count(*) from biz_payable)
              when 'before_biz_payment'         then (select count(*) from biz_payment)
              when 'before_biz_inventory'       then (select count(*) from biz_inventory)
              when 'before_biz_inventory_txn'   then (select count(*) from biz_inventory_txn)
              when 'before_biz_accounting_period' then (select count(*) from biz_accounting_period)
              when 'before_biz_opening_balance' then (select count(*) from biz_opening_balance)
              when 'before_biz_status_log'      then (select count(*) from biz_status_log)
              when 'before_biz_task_all'        then (select count(*) from biz_task)
            end)::text,
           (val = (case item
              when 'before_biz_voucher'         then (select count(*) from biz_voucher)
              when 'before_biz_voucher_entry'   then (select count(*) from biz_voucher_entry)
              when 'before_biz_payable'         then (select count(*) from biz_payable)
              when 'before_biz_payment'         then (select count(*) from biz_payment)
              when 'before_biz_inventory'       then (select count(*) from biz_inventory)
              when 'before_biz_inventory_txn'   then (select count(*) from biz_inventory_txn)
              when 'before_biz_accounting_period' then (select count(*) from biz_accounting_period)
              when 'before_biz_opening_balance' then (select count(*) from biz_opening_balance)
              when 'before_biz_status_log'      then (select count(*) from biz_status_log)
              when 'before_biz_task_all'        then (select count(*) from biz_task)
            end)::text)
      from datafix_e2eres3_guard
     where batch = 'E2ERES3-20261105' and item like 'before\_%';

    -- D 快照与账本自洽：删了多少行就得有多少快照（回滚才有据可依）
    insert into e2eres3_verify
    select 400, 'D 快照覆盖本批改动', 'deleted_total=' || coalesce(g.val,'-'),
           'snapshot=' || (select count(*)::text from datafix_e2eres3_snapshot
                            where batch='E2ERES3-20261105' and tbl <> 'sys_user'),
           (select count(*) from datafix_e2eres3_snapshot
             where batch='E2ERES3-20261105' and tbl <> 'sys_user') >= coalesce(g.val::bigint, 0)
      from datafix_e2eres3_guard g
     where g.batch = 'E2ERES3-20261105' and g.item = 'deleted_total';
end $$;

-- ---------------------------------------------------------------- 报告
\echo ''
\echo '---- E2ERES3 / 30 自检 ----'
\pset footer off
select case when ok then 'PASS' else 'FAIL' end as verdict, item, expect, got
  from e2eres3_verify order by ord, item;

do $$
declare
    v_fail bigint;
begin
    select count(*) into v_fail from e2eres3_verify where not ok;
    insert into datafix_e2eres3_guard(batch, item, val)
    values ('E2ERES3-20261105', 'verify_fail', v_fail::text)
    on conflict (batch, item) do update set val = excluded.val, captured_at = now();
    if v_fail > 0 then
        raise exception 'E2ERES3/30 ✗ 自检失败 % 条（见上表 FAIL 行）', v_fail;
    end if;
    raise notice 'E2ERES3/30 ✓ 自检全部通过（% 条断言）', (select count(*) from e2eres3_verify);
end $$;

select '30_verify OK' as result,
       (select count(*) from e2eres3_verify) as assertions,
       (select count(*) from e2eres3_verify where ok) as passed;
