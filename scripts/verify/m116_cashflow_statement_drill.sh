#!/usr/bin/env bash
# ============================================================================
# M1-16 / V0417 真库往返演练（由底稿生成现金流量表的两个权限码）
#
# 卡：t_74e707ca（M1-16【M1·财务】现金流底稿：由底稿生成现金流量表）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. V0414 / V0415 / V0416 前置   —— 底稿表 + 底稿两码 + 调整项两码（模板库还没跑到时补上）
#   3. V0417 up ×1                   —— 种「出表查看 / 出表导出」两个按钮码 + 授权
#   4. 断言                          —— 2 个节点挂「现金流量表」页面 / 无 path / sort 50·60
#                                       同父无重复 sort / 一码一节点 / 授权齐 /
#                                       FINANCE_DIRECTOR 没授（无页码 ⇒ 孤儿按钮）/
#                                       前置四个码（1770·1780·1790·1800）不受影响
#   5. 幂等反证                      —— 重放不攒行、不攒授权、不改已有行的 title/sort
#   6. V0417 down                    —— 两节点 + 授权都清干净，前置四码仍在
#   7. down 重放幂等 + V0417 up 再放 —— 证明回滚后可重放
#
# 用法：
#   bash scripts/verify/m116_cashflow_statement_drill.sh                 # 默认 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m116_cashflow_statement_drill.sh  # 在 145 上跑
#
# 环境变量：PG_HOST (172.18.10.145) / PG_PORT (5432) / PG_USER (cloudpivot) /
#           PGPASSWORD（默认取 145 开发库口令，仓库内既有脚本同值）/ TEMPLATE_DB (cloudpivot_test)
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MIG="$ROOT/backend/app/migrations/business"
UP0414="$MIG/V0414__cashflow_worksheet.up.sql"
UP0415="$MIG/V0415__cashflow_worksheet_perm.up.sql"
UP0416="$MIG/V0416__cashflow_adjust_item.up.sql"
UP0417="$MIG/V0417__cashflow_statement_perm.up.sql"
DOWN0417="$MIG/V0417__cashflow_statement_perm.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m116_stmt_drill_$$"

TMP="${TMPDIR:-/tmp}"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

psql_q() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
runfile() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$1" >"$2" 2>&1; }
assert_eq() { if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 $1，实际 $2）"; fi; }

cleanup() { dropdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --if-exists "$DB" >/dev/null 2>&1; }
trap cleanup EXIT

MENU_LIST='20000000-0000-0000-0000-000000001810'
MENU_EXPORT='20000000-0000-0000-0000-000000001820'
PAGE='20000000-0000-0000-0000-000000001018'
PRIOR="'20000000-0000-0000-0000-000000001770','20000000-0000-0000-0000-000000001780','20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800'"

echo "== M1-16 / V0417 真库往返演练（出表两个权限码）=="
echo "   PG=${PG_HOST}:${PG_PORT} 模板=${TEMPLATE_DB} 演练库=${DB}"
echo

# ---------- 0) 建演练库 ----------
if ! createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>"$TMP/m116_createdb.err"; then
    echo "  ⛔ 建库失败：$(cat "$TMP/m116_createdb.err")"
    exit 1
fi
info "演练库已从 $TEMPLATE_DB 克隆"

# ---------- 1) 前置 ----------
if [ "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_worksheet';")" = "0" ]; then
    runfile "$UP0414" "$TMP/m116_up0414.log" && ok "V0414 前置已应用" || { bad "V0414 前置失败：$(tail -3 "$TMP/m116_up0414.log")"; exit 1; }
fi
if [ "$(psql_q "select count(*) from sys_menu where id='20000000-0000-0000-0000-000000001770'")" = "0" ]; then
    runfile "$UP0415" "$TMP/m116_up0415.log" && ok "V0415 前置已应用（底稿两码）" || { bad "V0415 前置失败：$(tail -3 "$TMP/m116_up0415.log")"; exit 1; }
fi
if [ "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_adjust_item';")" = "0" ]; then
    runfile "$UP0416" "$TMP/m116_up0416.log" && ok "V0416 前置已应用（调整项两码）" || { bad "V0416 前置失败：$(tail -3 "$TMP/m116_up0416.log")"; exit 1; }
fi
assert_eq "0" "$(psql_q "select count(*) from sys_menu where id='$MENU_LIST';")" \
    "演练前出表两码不存在（本卡新增）"
assert_eq "4" "$(psql_q "select count(*) from sys_menu where parent_id='$PAGE' and menu_type=3 and deleted_at is null;")" \
    "演练前「现金流量表」页面下已有 4 个按钮（底稿 2 + 调整项 2）"

# ---------- 2) V0417 up ----------
if runfile "$UP0417" "$TMP/m116_up1.log"; then
    ok "V0417 第一次应用成功"
    grep -q "V0417 自检通过" "$TMP/m116_up1.log" && ok "自检 NOTICE 已打印（自检块真的跑了）" \
        || bad "没看到 V0417 自检通过 NOTICE"
else
    bad "V0417 应用失败：$(tail -5 "$TMP/m116_up1.log")"
fi

# ---------- 3) 节点 / 权限码断言 ----------
info "权限节点"
assert_eq "2" "$(psql_q "select count(*) from sys_menu where id in ('$MENU_LIST','$MENU_EXPORT') and menu_type=3 and parent_id='$PAGE' and path is null and status=1 and deleted_at is null and auth_code=name;")" \
    "两个按钮节点在位（挂「现金流量表」页面 · menu_type=3 · 无 path）"
assert_eq "business:erp:cashflow-statement:export,business:erp:cashflow-statement:list" \
    "$(psql_q "select string_agg(auth_code, ',' order by auth_code) from sys_menu where id in ('$MENU_LIST','$MENU_EXPORT');")" \
    "两个码与后端常量逐字一致"
assert_eq "2" "$(psql_q "select count(distinct auth_code) from sys_menu where auth_code in ('business:erp:cashflow-statement:list','business:erp:cashflow-statement:export') and deleted_at is null;")" \
    "一码一节点（无重复行）"
assert_eq "50,60" "$(psql_q "select string_agg(sort_order::text, ',' order by sort_order) from sys_menu where id in ('$MENU_LIST','$MENU_EXPORT');")" \
    "同父 sort 50 / 60（接在 10/20/30/40 之后）"
assert_eq "6" "$(psql_q "select count(*) from sys_menu where parent_id='$PAGE' and menu_type=3 and deleted_at is null;")" \
    "「现金流量表」页面下共 6 个按钮（底稿 2 + 调整项 2 + 出表 2）"
assert_eq "0" "$(psql_q "select count(*) from (select sort_order from sys_menu where parent_id='$PAGE' and menu_type=3 and deleted_at is null group by sort_order having count(*)>1) d;")" \
    "同父无重复 sort_order"
assert_eq "出表查看,出表导出" "$(psql_q "select string_agg(title, ',' order by sort_order) from sys_menu where id in ('$MENU_LIST','$MENU_EXPORT');")" \
    "title 为 4 字短名"
assert_eq "0" "$(psql_q "select count(*) from sys_menu where id in ('$MENU_LIST','$MENU_EXPORT') and (length(title) > 12 or title like '%，%' or title like '%（%');")" \
    "title 合菜单规范（≤12 字、无标点）"

info "授权"
ROLES="$(psql_q "select count(*) from sys_role where deleted_at is null and role_code in ('SUPER_ADMIN','FINANCE_MANAGER');")"
GRANT="$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.deleted_at is null where rm.menu_id in ('$MENU_LIST','$MENU_EXPORT') and r.role_code in ('SUPER_ADMIN','FINANCE_MANAGER');")"
assert_eq "$((ROLES * 2))" "$GRANT" "授权齐（${ROLES} 个目标角色 × 2 码）"
assert_eq "0" "$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id where rm.menu_id in ('$MENU_LIST','$MENU_EXPORT') and r.role_code='FINANCE_DIRECTOR';")" \
    "FINANCE_DIRECTOR 没授（无页码 ⇒ 会成孤儿按钮）"
assert_eq "4" "$(psql_q "select count(*) from sys_menu where id in ($PRIOR) and deleted_at is null;")" \
    "前置四个码不受影响（底稿 2 + 调整项 2）"

# ---------- 4) 幂等 ----------
info "幂等"
psql_q "update sys_menu set title='出表查看_已改' where id='$MENU_LIST';" >/dev/null
if runfile "$UP0417" "$TMP/m116_up2.log"; then
    ok "V0417 第二次应用成功（重放不报错）"
else
    bad "V0417 重放失败：$(tail -3 "$TMP/m116_up2.log")"
fi
assert_eq "2" "$(psql_q "select count(*) from sys_menu where id in ('$MENU_LIST','$MENU_EXPORT');")" "重放不攒行（仍 2 行）"
assert_eq "出表查看_已改" "$(psql_q "select title from sys_menu where id='$MENU_LIST';")" \
    "重放不覆盖管理员改过的 title（ON CONFLICT DO NOTHING）"
assert_eq "$((ROLES * 2))" "$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.deleted_at is null where rm.menu_id in ('$MENU_LIST','$MENU_EXPORT') and r.role_code in ('SUPER_ADMIN','FINANCE_MANAGER');")" \
    "重放不攒重复授权（仍 $((ROLES * 2)) 行）"
psql_q "update sys_menu set title='出表查看' where id='$MENU_LIST';" >/dev/null

# ---------- 5) down ----------
info "回滚"
if runfile "$DOWN0417" "$TMP/m116_down1.log"; then
    ok "down 成功"
    assert_eq "0" "$(psql_q "select count(*) from sys_menu where id in ('$MENU_LIST','$MENU_EXPORT');")" "down 后两个节点已清"
    assert_eq "0" "$(psql_q "select count(*) from sys_role_menu where menu_id in ('$MENU_LIST','$MENU_EXPORT');")" "down 后授权已清"
    assert_eq "4" "$(psql_q "select count(*) from sys_menu where id in ($PRIOR) and deleted_at is null;")" "down 不动前置四个码"
else
    bad "down 失败：$(tail -3 "$TMP/m116_down1.log")"
fi
if runfile "$DOWN0417" "$TMP/m116_down2.log"; then
    ok "down 重放幂等（第二次也成功）"
else
    bad "down 重放失败：$(tail -3 "$TMP/m116_down2.log")"
fi

# ---------- 6) 再放 ----------
info "可重放"
if runfile "$UP0417" "$TMP/m116_up3.log"; then
    ok "down 之后 V0417 可重放"
    assert_eq "2" "$(psql_q "select count(*) from sys_menu where id in ('$MENU_LIST','$MENU_EXPORT');")" "重放后两节点回来"
    assert_eq "$((ROLES * 2))" "$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.deleted_at is null where rm.menu_id in ('$MENU_LIST','$MENU_EXPORT') and r.role_code in ('SUPER_ADMIN','FINANCE_MANAGER');")" \
        "重放后授权回来"
else
    bad "重放失败：$(tail -3 "$TMP/m116_up3.log")"
fi

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
