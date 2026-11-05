-- ============================================================================
-- E2ERES3 / 20 down：按快照整行写回（回滚）
-- ----------------------------------------------------------------------------
-- 只回滚**本批次快照里登记过的行**，不重算判据 —— 回滚必须与「当时改了什么」严格对应，
-- 不能靠「现在看起来像夹具」再猜一遍（那是二次伤害的来源）。
--
--   · 业务表：把 deleted_at 写回快照里的原值（NULL → 重新可见）
--   · sys_user：把 status / is_locked 写回快照里的原值（当时可能有账号已经是 status=0）
--
-- 幂等：已经回滚过的行 deleted_at 与快照一致 → 命中 0 行，空转。
-- 快照与账本**不删**（审计要能回答「这一批动过哪些行」）。
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
    if to_regclass('public.datafix_e2eres3_snapshot') is null then
        raise exception 'E2ERES3/20 down: 没有 datafix_e2eres3_snapshot —— 无从回滚';
    end if;

    for r in select distinct tbl from datafix_e2eres3_snapshot where batch = v_batch order by tbl loop
        if r.tbl = 'sys_user' then
            execute format($f$
                update sys_user t
                   set status    = (s.before->>'status')::int,
                       is_locked = (s.before->>'is_locked')::boolean,
                       updated_at = now()
                  from datafix_e2eres3_snapshot s
                 where s.batch = %L and s.tbl = 'sys_user' and s.row_id = t.id::text
                   and t.deleted_at is null
                   and (t.status is distinct from (s.before->>'status')::int
                        or t.is_locked is distinct from (s.before->>'is_locked')::boolean)
            $f$, v_batch);
        else
            execute format($f$
                update %I t
                   set deleted_at = nullif(s.before->>'deleted_at', '')::timestamptz
                  from datafix_e2eres3_snapshot s
                 where s.batch = %L and s.tbl = %L and s.row_id = t.id::text
                   and t.deleted_at is not null
                   and (s.before->>'deleted_at') is null
            $f$, r.tbl, v_batch, r.tbl);
        end if;
        get diagnostics n = row_count;
        tot := tot + n;
        raise notice '  restore %  % 行', rpad(r.tbl, 21), n;
    end loop;

    insert into datafix_e2eres3_guard(batch, item, val)
    values (v_batch, 'rolled_back_rows', tot::text)
    on conflict (batch, item) do update set val = excluded.val, captured_at = now();
end $$;

commit;

select '20 down 完成（回滚）' as result,
       (select val from datafix_e2eres3_guard where batch='E2ERES3-20261105' and item='rolled_back_rows') as rolled_back_rows;
