#!/usr/bin/env bash
# D2：V0205 迁移的 down/up 往返 + 「存活配置时拒绝回滚」安全阀。
#
#   bash scripts/verify/d2_migrate.sh
#
# 顺序（每步都有断言，任一失败即 exit 1）：
#   ① 有存活规则时 down 必须中止（表一删配置就没了）；
#   ② 软删后 down 通过（表与索引都没了）；
#   ③ up 重放通过（表 + 5 个 CHECK + 1 个 UNIQUE + 2 个索引都在）；
#   ④ 重启隔离实例 → 日志出现「应用 0 个迁移」（= 指纹校验通过，迁移文件没被改过）。
set -uo pipefail
export PATH=/root/.cargo/bin:$PATH

REPO=${REPO:-/opt/cloudpivot}
DB=${D2_DB:-cloudpivot_d2}
MIG="$REPO/backend/migrations/business"
export PGPASSWORD=alun
PSQL="psql -h 127.0.0.1 -U alun -d $DB -At"

PASS=0
FAIL=0
ok()   { PASS=$((PASS + 1)); echo "[PASS] $1  —— $2"; }
bad()  { FAIL=$((FAIL + 1)); echo "[FAIL] $1  —— $2"; }
eq()   { if [ "$2" = "$3" ]; then ok "$1" "$2"; else bad "$1" "期望 [$3] 实际 [$2]"; fi; }

echo "== D2 迁移 V0205 down/up 往返（库 $DB）=="

if [ ! -f "$MIG/V0205__flow_linkage_rule.up.sql" ]; then
  echo "[FAIL] 找不到迁移文件 V0205"; exit 1
fi

# ---------- ① 有存活规则时 down 必须中止 ----------
$PSQL -c "DELETE FROM biz_flow_linkage_rule WHERE name = 'D2MIG-live'" >/dev/null 2>&1
$PSQL -c "INSERT INTO biz_flow_linkage_rule (id, name, trigger_entity, trigger_status, action_code, action_params) \
          VALUES (gen_random_uuid(), 'D2MIG-live', 'TASK', 'DOING', 'SEND_NOTICE', '{}'::jsonb)" >/dev/null
out=$(psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -f "$MIG/V0205__flow_linkage_rule.down.sql" 2>&1)
if printf '%s' "$out" | grep -q "V0205 down 中止"; then
  ok "① 有存活规则时 down 中止（安全阀）" "$(printf '%s' "$out" | grep -o 'V0205 down 中止.*' | head -1)"
else
  bad "① 有存活规则时 down 必须中止" "$(printf '%s' "$out" | tail -2 | tr '\n' ' ')"
fi
eq "①b 表仍在" "$($PSQL -c "select count(*) from pg_tables where tablename='biz_flow_linkage_rule'")" "1"

# ---------- ② 软删后 down 通过 ----------
$PSQL -c "UPDATE biz_flow_linkage_rule SET deleted_at = now() WHERE name = 'D2MIG-live'" >/dev/null
out=$(psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -f "$MIG/V0205__flow_linkage_rule.down.sql" 2>&1)
if [ $? -eq 0 ]; then ok "② 软删后 down 通过" "$(printf '%s' "$out" | grep -c NOTICE) 条 NOTICE"; else bad "② down 失败" "$out"; fi
eq "②b 表已删除" "$($PSQL -c "select count(*) from pg_tables where tablename='biz_flow_linkage_rule'")" "0"

# ---------- ③ up 重放 ----------
out=$(psql -h 127.0.0.1 -U alun -d "$DB" -v ON_ERROR_STOP=1 -f "$MIG/V0205__flow_linkage_rule.up.sql" 2>&1)
if [ $? -eq 0 ]; then ok "③ up 重放通过" "$(printf '%s' "$out" | grep -o 'V0205 完成.*' | head -1)"; else bad "③ up 失败" "$out"; fi
eq "③b 列数 13" "$($PSQL -c "select count(*) from information_schema.columns where table_name='biz_flow_linkage_rule'")" "13"
eq "③c 约束 7（5 CHECK + 1 UNIQUE + 1 PK；NOT NULL 在 PG18 里也算 pg_constraint，不计入）" \
  "$($PSQL -c "select count(*) from pg_constraint where conrelid='biz_flow_linkage_rule'::regclass and contype in ('c','u','p')")" "7"
eq "③d 索引 2（业务索引）" \
  "$($PSQL -c "select count(*) from pg_indexes where tablename='biz_flow_linkage_rule' and indexname like 'idx_flow_linkage%'")" "2"
eq "③e 表为空（空表 = 行为不变）" "$($PSQL -c "select count(*) from biz_flow_linkage_rule")" "0"
# 清掉 ① 造的软删行（down 已经删表，这里只为防止有人把 down 改成保留数据）
$PSQL -c "DELETE FROM biz_flow_linkage_rule WHERE name LIKE 'D2MIG-%'" >/dev/null 2>&1

# ---------- ④ 重启实例：指纹校验通过 ----------
bash "$REPO/scripts/verify/d2_instance.sh" stop >/dev/null 2>&1
if bash "$REPO/scripts/verify/d2_instance.sh" start >/tmp/d2-migrate-restart.log 2>&1; then
  line=$(grep -m1 "数据库迁移完成" "${D2_LOG:-/root/d2-e2e-server.log}" || true)
  if printf '%s' "$line" | grep -q "应用 0 个迁移"; then
    ok "④ 重启「应用 0 个迁移」（指纹校验通过）" "$line"
  else
    bad "④ 重启应显示「应用 0 个迁移」" "$line"
  fi
else
  bad "④ 重启失败" "$(tail -3 /tmp/d2-migrate-restart.log | tr '\n' ' ')"
fi

echo
echo "==> 汇总：PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] || exit 1
