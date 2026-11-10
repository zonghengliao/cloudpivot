#!/usr/bin/env bash
# ============================================================================
# B1-7 常驻闸联跑：在"全链（含本卡三件迁移）"的干净库上跑
#   ① scripts/verify/menu_invariants.sh      菜单结构 8 条不变量
#   ② scripts/verify/perm_menu_gate.py       闸⑥ 死权限码（handler 用到的码必须有菜单承载）
#   ③ scripts/verify/orphan_button_gate.py   闸⑦ 孤儿按钮（授了按钮必须授可见父页）
#
# 另外跑一遍 **基线对照**（先 down 掉 V0378，再跑 ①②③），用来证明：
#   `① 孤儿 parent = 9` 和 `③ 2 个 receivable/statement 孤儿` 都是**本卡之前就有的**基线问题，
#   与本卡 26 条菜单 / 48 条授权零关系（down 前后数字与名单完全一致）。
#
# 用法：bash 04_gates.sh [仓库根]     默认 /opt/cloudpivot
# ============================================================================
set -uo pipefail
REPO="${1:-/opt/cloudpivot}"
MROOT="$REPO/backend/migrations"
DBHOST="${DBHOST:-127.0.0.1}"; DBPORT="${DBPORT:-5432}"
DBUSER="${DBUSER:-alun}";      DBPASS="${DBPASS:-alun}"
DB="mig_costb17_gate_$(date +%m%d%H%M%S)"
export PGPASSWORD="$DBPASS"
q(){ psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "${2:-$DB}" -tAc "$1" 2>&1; }
cleanup(){ su - postgres -c "dropdb --force $DB" >/dev/null 2>&1 || q "DROP DATABASE IF EXISTS $DB" postgres >/dev/null 2>&1; }
trap cleanup EXIT

su - postgres -c "createdb -O $DBUSER $DB" >/dev/null 2>&1 || { echo "FAIL: 建库失败"; exit 2; }
echo "==> 闸门库：$DB（全链重放 $REPO）"
n=0
while IFS=$'\t' read -r ver f; do
    PGOPTIONS='-c client_min_messages=warning' \
      psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 \
           --single-transaction -f "$f" >/dev/null 2>&1 || { echo "FAIL: v$ver"; exit 2; }
    n=$((n+1))
done < <(find "$MROOT" -name 'V*__*.up.sql' | awk -F'/V' '{split($NF,a,"__"); print a[1]+0 "\t" $0}' | sort -n -k1,1)
echo "  迁移数：$n"

run_gates() { # $1 = 标签
    echo
    echo "########## [$1] ① menu_invariants.sh"
    PGPASSWORD="$DBPASS" PGHOST="$DBHOST" PGPORT="$DBPORT" PGUSER="$DBUSER" \
        bash "$REPO/scripts/verify/menu_invariants.sh" "$DB"; echo "rc=$?"
    echo "########## [$1] ② perm_menu_gate.py（闸⑥ 死权限码）"
    NIK_DATABASE_HOST="$DBHOST" NIK_DATABASE_PORT="$DBPORT" NIK_DATABASE_USER="$DBUSER" \
    NIK_DATABASE_NAME="$DB"      NIK_DATABASE_PASSWORD="$DBPASS" \
        python3 "$REPO/scripts/verify/perm_menu_gate.py"; echo "rc=$?"
    echo "########## [$1] ③ orphan_button_gate.py（闸⑦ 孤儿按钮）"
    python3 "$REPO/scripts/verify/orphan_button_gate.py" \
        --dsn "postgres://$DBUSER:$DBPASS@$DBHOST:$DBPORT/$DB"; echo "rc=$?"
    echo "########## [$1] 形态"
    q "select '菜单总数=' || count(*) from sys_menu where deleted_at is null"
    q "select '成本块=' || count(*) from sys_menu where id::text like '20000000-0000-4000-8000-00000000c%' and deleted_at is null"
    q "select '成本授权=' || count(*) from sys_role_menu rm join sys_menu m on m.id=rm.menu_id where m.id::text like '20000000-0000-4000-8000-00000000c%'"
}

run_gates "全链（含 V0378 成本菜单）"

echo
echo "########## 基线对照：down 掉 V0380/V0379/V0378 后再跑一遍（证明红线都是基线既有）"
for f in V0380__cost_lot_backfill.down.sql V0379__cost_seq_and_dict.down.sql V0378__cost_menu_and_perm.down.sql; do
    PGOPTIONS='-c client_min_messages=warning' \
      psql -h "$DBHOST" -p "$DBPORT" -U "$DBUSER" -d "$DB" -q -X -v ON_ERROR_STOP=1 \
           --single-transaction -f "$MROOT/business/$f" >/dev/null 2>&1 || { echo "down 失败：$f"; exit 2; }
done
run_gates "基线（已 down 掉本卡三件）"
