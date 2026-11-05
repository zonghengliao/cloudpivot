-- ============================================================================
-- E2ERES3 / 20 up：软删 E2E 标记数据 + 禁用在用测试账号（**单事务，可逆**）
-- ----------------------------------------------------------------------------
-- 处置口径（与卡 t_44766ab3 的验收要求逐条对应）：
--
--   ① E2E 任务 → **软删**（deleted_at = now()），保审计留痕（行还在，可查可回滚）
--   ② 任务评论 / 通知 / 公告 / 上传件 / 导出任务 / 帮助分类 / E2E 流程实例
--      → 同样软删（它们都是「挂在 E2E 任务或 E2E 用例上」的产物）
--   ③ 测试账号 → **禁用**（status = 0），**绝不删行**：历史 create_by / 审计
--      引用指向它们，删了会让操作日志的主人变成「查无此人」
--   ④ 不改 update_by：谁动的库由本包的账本（datafix_e2eres3_guard）+ 快照
--      （datafix_e2eres3_snapshot）+ git commit 交代；伪造业务操作人更糟
--
-- 幂等：判据是「alive 且带标记」，跑第二遍命中 0 行 = 空转（账本仍会重算对齐）
-- 可逆：down 按快照逐行写回 deleted_at / status 原值
-- ============================================================================

\set ON_ERROR_STOP on

begin;
set local statement_timeout = '180s';

do $$
declare
    v_batch constant text := 'E2ERES3-20261105';
    r       record;
    n       bigint;
    tot     bigint := 0;
begin
    if to_regclass('public.datafix_e2eres3_spec') is null then
        raise exception 'E2ERES3/20: 没有 datafix_e2eres3_spec —— 先跑 10_precheck.sql';
    end if;

    -- ---------------------------------------------------------------- ① 行快照
    -- 先拍**所有将被改的行**（改之前），down 只认它，不重新算判据
    for r in select tbl, pred from datafix_e2eres3_spec order by tbl loop
        execute format($f$
            insert into datafix_e2eres3_snapshot(batch, tbl, row_id, before)
            select %L, %L, t.id::text, to_jsonb(t)
              from %I t
             where t.deleted_at is null and %s
            on conflict (batch, tbl, row_id) do nothing
        $f$, v_batch, r.tbl, r.tbl, r.pred);
    end loop;

    insert into datafix_e2eres3_snapshot(batch, tbl, row_id, before)
    select v_batch, 'sys_user', u.id::text, to_jsonb(u)
      from sys_user u
     where u.deleted_at is null and u.status = 1
       and u.username in ('laoliu_test','qa.viewer','qa.limited','qa.mutex','cl_43870','verify-ai')
    on conflict (batch, tbl, row_id) do nothing;

    -- ---------------------------------------------------------------- ② 软删
    for r in select tbl, pred from datafix_e2eres3_spec order by tbl loop
        execute format('update %I set deleted_at = now() where deleted_at is null and %s',
                       r.tbl, r.pred);
        get diagnostics n = row_count;
        tot := tot + n;
        raise notice '  soft-delete %  % 行（本次命中）', rpad(r.tbl, 21), n;
    end loop;

    -- 账本口径：**按快照统计**，不是「本次命中」。
    -- 为什么要这样：本包是可复跑的（幂等），第二次跑命中 0 行；若账本写「本次命中」，
    -- 生产账本就会被一次复核跑成 `deleted_total = 0`，后人再查就以为这包什么都没删过
    -- （2026-11-05 生产实测踩到过）。快照里有多少行 = 本批**真正处置过**多少行，
    -- 与跑几遍无关。
    insert into datafix_e2eres3_guard(batch, item, val)
    select v_batch, 'deleted_'||tbl, count(*)::text
      from datafix_e2eres3_snapshot
     where batch = v_batch and tbl <> 'sys_user'
     group by tbl
    on conflict (batch, item) do update set val = excluded.val, captured_at = now();

    insert into datafix_e2eres3_guard(batch, item, val)
    select v_batch, 'deleted_total', count(*)::text
      from datafix_e2eres3_snapshot
     where batch = v_batch and tbl <> 'sys_user'
    on conflict (batch, item) do update set val = excluded.val, captured_at = now();

    -- ---------------------------------------------------------------- ③ 禁用账号
    update sys_user set status = 0, updated_at = now()
     where deleted_at is null and status = 1
       and username in ('laoliu_test','qa.viewer','qa.limited','qa.mutex','cl_43870','verify-ai');
    get diagnostics n = row_count;
    -- 同样按快照口径（快照里的 6 个账号 = 本批处置过的账号数）
    insert into datafix_e2eres3_guard(batch, item, val)
    select v_batch, 'disabled_users', count(*)::text
      from datafix_e2eres3_snapshot
     where batch = v_batch and tbl = 'sys_user'
    on conflict (batch, item) do update set val = excluded.val, captured_at = now();
    raise notice '  disable %  % 个账号（本次命中，账本按快照口径记）', rpad('test accounts', 21), n;

    insert into datafix_e2eres3_guard(batch, item, val)
    values (v_batch, 'applied_at', to_char(now(), 'YYYY-MM-DD HH24:MI:SS'))
    on conflict (batch, item) do update set val = excluded.val, captured_at = now();
end $$;

commit;

-- ---------------------------------------------------------------- 现状复查
select '20 up 完成' as result,
       (select val from datafix_e2eres3_guard where batch='E2ERES3-20261105' and item='deleted_total') as deleted_total,
       (select val from datafix_e2eres3_guard where batch='E2ERES3-20261105' and item='disabled_users') as disabled_users,
       (select count(*) from datafix_e2eres3_snapshot where batch='E2ERES3-20261105') as snapshot_rows;
