#!/usr/bin/env bash
# D1：把「配置表 → 真机行为」的 psql 证据一次性打出来（只读 + 演示后清理）。
# 需要隔离实例在跑（scripts/verify/d1_instance.sh start）。
set -uo pipefail
DB=${D1_DB:-cloudpivot_d1e2e}
PSQL="env PGPASSWORD=${D1_PGPASS:-alun} psql -h 127.0.0.1 -U alun -d $DB"

echo "===== ① 迁移版本（V0204 = flow_state_edge）====="
$PSQL -At -F'|' -c "select max(version::int) as max_version, count(*) as migrations from schema_migrations"
$PSQL -At -F'|' -c "select version, description from schema_migrations where version::int >= 203 order by version"

echo
echo "===== ② biz_flow_state_edge 结构 ====="
$PSQL -At -F'|' -c "select column_name, data_type, is_nullable, coalesce(column_default,'') from information_schema.columns
                     where table_name='biz_flow_state_edge' order by ordinal_position"

echo
echo "===== ③ 约束 / 索引 ====="
$PSQL -At -F'|' -c "select conname, pg_get_constraintdef(oid) from pg_constraint where conrelid='biz_flow_state_edge'::regclass order by conname"
$PSQL -At -F'|' -c "select indexdef from pg_indexes where tablename='biz_flow_state_edge' order by indexname"

echo
echo "===== ④ 空表 = 行为与改动前一致（存活 0 行）====="
$PSQL -At -c "select count(*) from biz_flow_state_edge where deleted_at is null"

echo
echo "===== ⑤ 配置一条 disabled（真机 e2e 里的那一条）====="
$PSQL -At -c "insert into biz_flow_state_edge (entity, from_status, to_status, enabled)
               values ('PURCHASE_ORDER','SUBMITTED','CLOSED', false)
               on conflict (entity, from_status, to_status) do update set enabled=false, deleted_at=null"
$PSQL -At -F'|' -c "select entity, from_status, to_status, enabled, requires_reason, coalesce(label,'(BASE)'), coalesce(auth_code,'(无)')
                     from biz_flow_state_edge where deleted_at is null order by entity, from_status, to_status"

echo
echo "===== ⑥ 生效判据（合并后这条边应当不在有效边里）====="
echo "（由真机 e2e 的 ② 断言：PATCH /purchase-order/{id}/status status=CLOSED → 409「已被状态机配置禁用」）"

echo
echo "===== ⑦ 清理（软删配置 → 回到 BASE_EDGES）====="
$PSQL -At -c "update biz_flow_state_edge set deleted_at = now() where deleted_at is null"
$PSQL -At -c "select count(*) as alive_after from biz_flow_state_edge where deleted_at is null"
$PSQL -At -c "select count(*) as total_rows from biz_flow_state_edge"
