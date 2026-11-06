#!/usr/bin/env bash
# =============================================================================
# V0267 迁移回归：重放幂等 + down/up 往返（真 psql，不走框架记账）
# =============================================================================
#   bash scripts/verify/so25_migration_roundtrip.sh
#
# 验三件事（对应「迁移可重复执行 / 可回滚」）：
#   ① up 重放：在已应用过的库上再跑一次 up.sql ⇒ 授权行数、菜单行数、角色数**一行不多**；
#   ② down：授权行按确定性 id 精确删除（只删本迁移插入的），页面 parent_id 回 NULL、
#      sort_order 回 76…85，目录节点删除，SALES_CLERK（无绑定）删除；
#   ③ 再 up：库回到 ①之前的状态（逐项计数与 ①完全一致）。
# 证据落 docs/evidence/20261106-so25-sales-menu/migration-roundtrip.txt
# =============================================================================
set -uo pipefail
DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$DIR/../.." && pwd)
DB=${SO25_DB:-cloudpivot_so25}
MIG="$ROOT/backend/migrations/business"
OUT="$ROOT/docs/evidence/20261106-so25-sales-menu/migration-roundtrip.txt"
export PGPASSWORD=alun
Q() { psql -h 127.0.0.1 -U alun -d "$DB" -tAF'|' -c "$1"; }
PASS=0; FAIL=0
ok()  { PASS=$((PASS+1)); printf '  ✅ %s\n' "$*"; }
bad() { FAIL=$((FAIL+1)); printf '  ❌ %s\n' "$*"; }
chk() { if [ "$2" = "$3" ]; then ok "$1 = $2"; else bad "$1 = $2（期望 $3）"; fi; }

snap() { # 关键计数快照
  Q "select
       (select count(*) from sys_role_menu rm
          join sys_role r on r.id=rm.role_id
          join sys_menu m on m.id=rm.menu_id
         where r.role_code in ('SALES_SUPERVISOR','SALES_CLERK','WAREHOUSE_MANAGER','FINANCE_MANAGER','GENERAL_MANAGER')
           and (m.auth_code like 'business:erp:customer:%' or m.auth_code like 'business:erp:sale-order:%'
             or m.auth_code like 'business:erp:shipment:%' or m.auth_code like 'business:erp:receivable:%'
             or m.auth_code like 'business:erp:receipt:%'  or m.auth_code like 'business:erp:invoice:%'
             or m.id='20000000-0000-0000-0000-000000002701'))
       || '/' || (select count(*) from sys_menu where parent_id='20000000-0000-0000-0000-000000002701'::uuid and deleted_at is null)
       || '/' || (select count(*) from sys_menu where id='20000000-0000-0000-0000-000000002701'::uuid)
       || '/' || (select count(*) from sys_role where role_code='SALES_CLERK' and deleted_at is null)"
}

{
echo "=================================================================="
echo "V0267 迁移回归（库=$DB） $(date '+%F %T')"
echo "=================================================================="

echo; echo "== ① 基线（up 已应用） =="
S0=$(snap); echo "  (销售授权行/目录子页/目录节点/SALES_CLERK) = $S0"

echo; echo "== ② up 重放（幂等：应 0 行新增） =="
psql -h 127.0.0.1 -U alun -d "$DB" -q -v ON_ERROR_STOP=1 -f "$MIG/V0267__sales_menu.up.sql" 2>&1 | sed 's/^/    /'
S1=$(snap)
chk "重放后快照" "$S1" "$S0"

echo; echo "== ③ down（回滚） =="
psql -h 127.0.0.1 -U alun -d "$DB" -q -v ON_ERROR_STOP=1 -f "$MIG/V0267__sales_menu.down.sql" 2>&1 | sed 's/^/    /'
S2=$(snap); echo "  down 后快照 = $S2"
# SALES_CLERK 保留是**设计行为**：验收夹具 so25_clerk 还绑着它，down 不拆在岗人员的组织关系
# （与 V0281 down 同口径：有绑定 → NOTICE 保留）。授权行/目录/子页必须归零。
chk "down 后销售授权行=0 / 目录子页=0 / 目录节点=0 / SALES_CLERK 仍 1（有用户绑定，保留）" "$S2" "0/0/0/1"
rest=$(Q "select string_agg(m.sort_order||':'||coalesce(m.parent_id::text,'NULL'), ',' order by m.sort_order)
            from sys_menu m where m.id in ('20000000-0000-0000-0000-000000002601','20000000-0000-0000-0000-000000002651',
              '20000000-0000-0000-0000-000000002841','20000000-0000-0000-0000-000000002880',
              '20000000-0000-0000-0000-000000002890','20000000-0000-0000-0000-000000002895')")
chk "六页面 sort:parent 全还原" "$rest" "76:NULL,77:NULL,78:NULL,79:NULL,80:NULL,85:NULL"

echo; echo "== ④ 再 up（正向恢复） =="
psql -h 127.0.0.1 -U alun -d "$DB" -q -v ON_ERROR_STOP=1 -f "$MIG/V0267__sales_menu.up.sql" 2>&1 | sed 's/^/    /'
S3=$(snap)
chk "再 up 后快照与基线一致" "$S3" "$S0"

echo; echo "=================================================================="
echo "PASS=$PASS FAIL=$FAIL"
[ "$FAIL" -eq 0 ] && echo "结果：PASS" || echo "结果：FAIL"
} 2>&1 | tee "$OUT"

grep -q "结果：PASS" "$OUT"
