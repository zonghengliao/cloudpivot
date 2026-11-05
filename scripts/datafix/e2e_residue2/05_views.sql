-- ============================================================================
-- E2ERES2 / 05：判据视图（**唯一来源**，10 / 20 / 30 / 00_backup.sh 都照这份口径）
-- ----------------------------------------------------------------------------
-- 单独成文件的原因：apply.sh --check 与 30_verify.sql 只读视图也要能看到目标集，
-- 而视图在 10_precheck.sql 里创建 —— 拆出来后「先建视图再快照」不再依赖 10 跑过。
-- 幂等：create or replace，可重复执行。
-- ============================================================================

\set ON_ERROR_STOP on

-- SET A-1：已软删 + biz_type 是业务值（上一张卡 t_61dca492 的 E2E_*/D2_SMOKE 口径不碰）
--          + flow_code 命中前缀枚举（e2e|diag|diag2|vrf|d5|acc|tn）
create or replace view datafix_e2eres2_v_target_def as
select d.id, d.flow_code, d.flow_name, d.biz_type, d.status, d.create_by, d.deleted_at
  from biz_flow_def d
 where d.deleted_at is not null
   and not (left(d.biz_type, 4) = 'E2E_' or d.biz_type = 'D2_SMOKE')
   and d.flow_code ~ '^(e2e|diag|diag2|vrf|d5|acc|tn)_';

-- SET A-2：活实例，但父定义已软删（孤儿实例）
create or replace view datafix_e2eres2_v_target_inst as
select i.id, i.flow_code, i.status, i.def_id
  from biz_flow_instance i
 where i.deleted_at is null
   and i.def_id in (select id from datafix_e2eres2_v_target_def);

comment on view datafix_e2eres2_v_target_def is
    'E2ERES2 目标定义判据（唯一来源）：已软删 + biz_type 是业务值 + flow_code ~ ^(e2e|diag|diag2|vrf|d5|acc|tn)_。'
    ' 前缀只是候选，能否删由 docs/evidence/20261104-e2eres2/README.md 的逐条判决决定。';
comment on view datafix_e2eres2_v_target_inst is
    'E2ERES2 目标实例判据（唯一来源）：活实例且父定义已软删（孤儿实例）。';
