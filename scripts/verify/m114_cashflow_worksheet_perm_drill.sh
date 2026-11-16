#!/usr/bin/env bash
# ============================================================================
# M1-14 权限码迁移 V0415 真库往返演练（现金流底稿：录入 + 勾稽校验 · 直接法）
#
# 卡：t_9e14a327
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. V0414 up      —— 底稿表（M1-13；模板库还没跑到 V0414 时补上）
#   3. V0415 up ×2   —— 第一次种两个权限码 + 授权，第二次证明幂等（零重复行、title 不变）
#   4. 断言           —— 两个节点在位 / 父节点 = 现金流量表页面 / path 为 NULL /
#                       同父 sort 唯一 / 三角色 × 2 码授权齐 / 码唯一
#   5. 反证           —— 手工删掉一个授权 → 重放 V0415 必须补回来（证明自检真的看授权）
#   6. V0415 down    —— 无底稿数据时可回滚；断言两行菜单与授权都清干净
#   7. down 护栏反证  —— 先造一行底稿，再跑 down 必须 RAISE EXCEPTION 拒绝且菜单不动
#   8. V0415 up 再放  —— 证明回滚后可重放
#
# 用法：
#   bash scripts/verify/m114_cashflow_worksheet_perm_drill.sh              # 默认 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m114_cashflow_worksheet_perm_drill.sh   # 在 145 上跑
#
# 环境变量（默认 = 开发机 99 连 145 的 PG，见卡面「数据库约束」）：
#   PG_HOST (172.18.10.145) / PG_PORT (5432) / PG_USER (cloudpivot) /
#   PGPASSWORD（默认取 145 开发库口令，仓库内既有脚本同值）/ TEMPLATE_DB (cloudpivot_test)
#
# 约定（AGENTS.md / 卡面）：演练库只建在 145；库名必须含 drill/e2e/test；脚本自己 DROP。
# ============================================================================
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
UP0414="$ROOT/backend/app/migrations/business/V0414__cashflow_worksheet.up.sql"
UP0415="$ROOT/backend/app/migrations/business/V0415__cashflow_worksheet_perm.up.sql"
DOWN0415="$ROOT/backend/app/migrations/business/V0415__cashflow_worksheet_perm.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m114_v0415_drill_$$"

TMP="${TMPDIR:-/tmp}"
PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); printf '  \033[32mPASS\033[0m %s\n' "$1"; }
bad()  { FAIL=$((FAIL+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; }
info() { printf '  ---- %s\n' "$1"; }

psql_q() { psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -tAc "$1" 2>&1; }
runfile() { # 文件 日志
    psql -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -1 -f "$1" >"$2" 2>&1
}
assert_eq() { # 期望 实际 说明
    if [ "$1" = "$2" ]; then ok "$3（=$2）"; else bad "$3（期望 $1，实际 $2）"; fi
}

cleanup() {
    dropdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" --if-exists "$DB" >/dev/null 2>&1
}
trap cleanup EXIT

echo "== M1-14 / V0415 真库往返演练（现金流底稿权限码）=="
echo "   PG=${PG_HOST}:${PG_PORT} 模板=${TEMPLATE_DB} 演练库=${DB}"
echo

# ---------- 0) 建演练库 ----------
if ! createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>"$TMP/m114_createdb.err"; then
    echo "  ⛔ 建库失败：$(cat "$TMP/m114_createdb.err")"
    exit 1
fi
info "演练库已从 $TEMPLATE_DB 克隆"

# ---------- 1) V0414 前置（模板库可能还没跑到） ----------
if [ "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_worksheet';")" = "0" ]; then
    if runfile "$UP0414" "$TMP/m114_up0414.log"; then
        ok "V0414 前置已应用（底稿表建好）"
    else
        bad "V0414 前置应用失败：$(tail -3 "$TMP/m114_up0414.log")"
        echo; echo "== 汇总：PASS=$PASS FAIL=$FAIL =="; exit 1
    fi
else
    ok "模板库已有 biz_cashflow_worksheet（V0414 已跑过）"
fi

# ---------- 2) V0415 up ×2 ----------
echo
echo "== 2) V0415 up 第一次 =="
if runfile "$UP0415" "$TMP/m114_up1.log"; then
    ok "V0415 up 第一次成功（种码 + 授权 + 自检块通过）"
    grep -E "NOTICE:.*(前置表齐备|自检通过)" "$TMP/m114_up1.log" | sed 's/^/       /'
else
    bad "V0415 up 第一次失败：$(tail -5 "$TMP/m114_up1.log")"
    echo; echo "== 汇总：PASS=$PASS FAIL=$FAIL =="; exit 1
fi

echo
echo "== 3) V0415 up 第二次（幂等）=="
if runfile "$UP0415" "$TMP/m114_up2.log"; then
    ok "V0415 up 第二次成功（重放零报错）"
else
    bad "V0415 up 第二次失败：$(tail -5 "$TMP/m114_up2.log")"
fi

echo
echo "== 4) 结构与授权断言 =="
assert_eq "2" "$(psql_q "select count(*) from sys_menu where id in ('20000000-0000-0000-0000-000000001770','20000000-0000-0000-0000-000000001780') and deleted_at is null;")" \
    "两个按钮节点在位"
assert_eq "2" "$(psql_q "select count(*) from sys_menu where auth_code in ('business:erp:cashflow-worksheet:list','business:erp:cashflow-worksheet:save') and deleted_at is null;")" \
    "两个权限码各一行（无重复）"
assert_eq "2" "$(psql_q "select count(*) from sys_menu where auth_code in ('business:erp:cashflow-worksheet:list','business:erp:cashflow-worksheet:save') and parent_id = '20000000-0000-0000-0000-000000001018' and menu_type = 3 and path is null;")" \
    "两行都挂「现金流量表」页面下、menu_type=3、无 path"
assert_eq "0" "$(psql_q "select count(*) from (select sort_order from sys_menu where parent_id='20000000-0000-0000-0000-000000001018' and menu_type=3 and deleted_at is null group by sort_order having count(*)>1) d;")" \
    "同父 sort_order 唯一"
assert_eq "底稿查看|底稿录入" "$(psql_q "select string_agg(title, '|' order by sort_order) from sys_menu where id in ('20000000-0000-0000-0000-000000001770','20000000-0000-0000-0000-000000001780');")" \
    "title 是 4 字菜单名（底稿查看 / 底稿录入）"
ROLES=$(psql_q "select count(*) from sys_role where deleted_at is null and role_code in ('SUPER_ADMIN','FINANCE_MANAGER');")
assert_eq "$((ROLES * 2))" "$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.deleted_at is null where rm.menu_id in ('20000000-0000-0000-0000-000000001770','20000000-0000-0000-0000-000000001780') and r.role_code in ('SUPER_ADMIN','FINANCE_MANAGER');")" \
    "授权齐（${ROLES} 个目标角色 × 2 个码）"
assert_eq "0" "$(psql_q "select count(*) from (select role_id, menu_id from sys_role_menu where menu_id in ('20000000-0000-0000-0000-000000001770','20000000-0000-0000-0000-000000001780') group by 1,2 having count(*)>1) d;")" \
    "授权无重复行（重放不攒重复）"

echo
echo "== 5) 反证：删掉一个授权后重放 V0415 必须补回 =="
psql_q "delete from sys_role_menu rm using sys_role r where r.id=rm.role_id and r.role_code='FINANCE_MANAGER' and rm.menu_id='20000000-0000-0000-0000-000000001780';" >/dev/null
assert_eq "0" "$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id where r.role_code='FINANCE_MANAGER' and rm.menu_id='20000000-0000-0000-0000-000000001780';")" \
    "授权被手工删掉（反证的前提）"
if runfile "$UP0415" "$TMP/m114_up3.log"; then
    ok "V0415 重放成功（自检看到授权缺失）"
    assert_eq "1" "$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id where r.role_code='FINANCE_MANAGER' and rm.menu_id='20000000-0000-0000-0000-000000001780';")" \
        "重放把 FINANCE_MANAGER 的写码授权补回来了"
else
    bad "V0415 重放失败：$(tail -5 "$TMP/m114_up3.log")"
fi

echo
echo "== 6) V0415 down（无底稿数据）=="
if runfile "$DOWN0415" "$TMP/m114_down1.log"; then
    ok "down 执行成功（无底稿数据，护栏放行）"
    assert_eq "0" "$(psql_q "select count(*) from sys_menu where auth_code in ('business:erp:cashflow-worksheet:list','business:erp:cashflow-worksheet:save');")" \
        "两个权限节点已删除"
    assert_eq "0" "$(psql_q "select count(*) from sys_role_menu where menu_id in ('20000000-0000-0000-0000-000000001770','20000000-0000-0000-0000-000000001780');")" \
        "授权已清干净"
    if runfile "$DOWN0415" "$TMP/m114_down2.log"; then
        ok "down 第二次也成功（幂等）"
    else
        bad "down 第二次失败：$(tail -3 "$TMP/m114_down2.log")"
    fi
else
    bad "down 失败：$(tail -5 "$TMP/m114_down1.log")"
fi

echo
echo "== 7) down 护栏反证：有底稿数据必须拒绝回滚 =="
runfile "$UP0415" "$TMP/m114_up4.log" || bad "重放 up 失败（护栏反证的前提）"
psql_q "insert into biz_cashflow_worksheet (period_code, line_type, item_code, item_name, activity, direction, amount, sort_order) values ('2026-11','FLOW','OPER_SALES_RECEIPTS','销售商品、提供劳务收到的现金','OPERATING','INFLOW',1200.0000,101);" >/dev/null
assert_eq "1" "$(psql_q "select count(*) from biz_cashflow_worksheet where period_code='2026-11';")" \
    "已造一行底稿（护栏反证的前提）"
if runfile "$DOWN0415" "$TMP/m114_down3.log"; then
    bad "有底稿数据时 down 竟然成功了（护栏没生效 → 回滚会让底稿变成谁也改不了的死数据）"
else
    if grep -q "拒绝回滚" "$TMP/m114_down3.log"; then
        ok "有底稿数据时 down 被拒（护栏生效）"
    else
        bad "down 失败但不是护栏拦的：$(tail -3 "$TMP/m114_down3.log")"
    fi
fi
assert_eq "2" "$(psql_q "select count(*) from sys_menu where auth_code in ('business:erp:cashflow-worksheet:list','business:erp:cashflow-worksheet:save') and deleted_at is null;")" \
    "被拒的 down 没动菜单（两个节点还在）"
psql_q "delete from biz_cashflow_worksheet where period_code='2026-11';" >/dev/null

echo
echo "== 8) 收尾断言（库内零残留探针） =="
assert_eq "0" "$(psql_q "select count(*) from biz_cashflow_worksheet;")" "底稿表零残留（探针已清）"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
