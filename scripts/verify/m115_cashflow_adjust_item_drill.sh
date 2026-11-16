#!/usr/bin/env bash
# ============================================================================
# M1-15 / V0416 真库往返演练（间接法调整项配置）
#
# 卡：t_ce938a2a（M1-15【M1·财务】现金流底稿：间接法调整项配置）
#
# 做的事（全部对**临时演练库**，跑完就删）：
#   1. 从模板库 `cloudpivot_test` 克隆演练库（库名含 drill，脚本自己 DROP）
#   2. V0414 / V0415 前置     —— 底稿表 + 底稿权限码（模板库还没跑到时补上）
#   3. V0416 up ×2            —— 第一次建表 + 12 行模板 + 2 个权限码；第二次证明幂等
#   4. 断言                    —— 12 行模板 / 4 个口径 / 2 行 MANUAL / 全部启用
#                                2 个按钮挂「现金流量表」页面 / 无 path / sort 30·40 唯一
#                                授权齐（超管 + 财务经理 × 2 码）/ 码唯一
#   5. 幂等反证                —— 管理员改过的名称 / 口径 / 顺序，重放后**必须保得住**
#   6. 约束反证                —— MANUAL 行带口径 / 非法口径 / 空名称 必须被库层拒
#   7. V0416 down             —— 无自建配置时可回滚；断言表 / 节点 / 授权都清干净
#   8. down 护栏反证           —— 先造一行自建配置，再跑 down 必须 RAISE EXCEPTION 拒绝
#   9. V0416 up 再放           —— 证明回滚后可重放
#
# 用法：
#   bash scripts/verify/m115_cashflow_adjust_item_drill.sh              # 默认 99 → 145
#   PG_HOST=127.0.0.1 bash scripts/verify/m115_cashflow_adjust_item_drill.sh   # 在 145 上跑
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
UP0416="$ROOT/backend/app/migrations/business/V0416__cashflow_adjust_item.up.sql"
DOWN0416="$ROOT/backend/app/migrations/business/V0416__cashflow_adjust_item.down.sql"

PG_HOST="${PG_HOST:-172.18.10.145}"
PG_PORT="${PG_PORT:-5432}"
PG_USER="${PG_USER:-cloudpivot}"
export PGPASSWORD="${PGPASSWORD:-fbb1263691ff75c31ec99eaf258138b4}"
TEMPLATE_DB="${TEMPLATE_DB:-cloudpivot_test}"
DB="cp_m115_adjust_drill_$$"

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

echo "== M1-15 / V0416 真库往返演练（间接法调整项配置）=="
echo "   PG=${PG_HOST}:${PG_PORT} 模板=${TEMPLATE_DB} 演练库=${DB}"
echo

# ---------- 0) 建演练库 ----------
if ! createdb -h "$PG_HOST" -p "$PG_PORT" -U "$PG_USER" -T "$TEMPLATE_DB" "$DB" 2>"$TMP/m115_createdb.err"; then
    echo "  ⛔ 建库失败：$(cat "$TMP/m115_createdb.err")"
    exit 1
fi
info "演练库已从 $TEMPLATE_DB 克隆"

# ---------- 1) 前置（模板库可能还没跑到 V0414 / V0415） ----------
if [ "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_worksheet';")" = "0" ]; then
    runfile "$UP0414" "$TMP/m115_up0414.log" && ok "V0414 前置已应用（底稿表建好）" \
        || { bad "V0414 前置失败：$(tail -3 "$TMP/m115_up0414.log")"; exit 1; }
fi
if [ "$(psql_q "select count(*) from sys_menu where id='20000000-0000-0000-0000-000000001770'")" = "0" ]; then
    runfile "$UP0415" "$TMP/m115_up0415.log" && ok "V0415 前置已应用（底稿两码）" \
        || { bad "V0415 前置失败：$(tail -3 "$TMP/m115_up0415.log")"; exit 1; }
fi
assert_eq "0" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_adjust_item';")" \
    "演练前 biz_cashflow_adjust_item 不存在（本卡新建）"

# ---------- 2) V0416 up ----------
if runfile "$UP0416" "$TMP/m115_up1.log"; then
    ok "V0416 第一次应用成功"
    grep -q "V0416 自检通过" "$TMP/m115_up1.log" && ok "自检 NOTICE 已打印（自检块真的跑了）" \
        || bad "没看到 V0416 自检通过 NOTICE"
    grep -q "MANUAL 行带口径被 CHECK 拒绝" "$TMP/m115_up1.log" \
        && ok "自检内的坏形状反证被执行（CHECK 真的在拦）" || bad "自检内的坏形状反证没跑"
else
    bad "V0416 应用失败：$(tail -5 "$TMP/m115_up1.log")"
fi

# ---------- 3) 结构断言 ----------
info "结构"
assert_eq "1" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_adjust_item';")" \
    "表已建出"
assert_eq "9" "$(psql_q "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_cashflow_adjust_item' and column_name in ('id','item_code','item_name','source_type','calc_basis','account_pattern','sort_order','is_active','remark');")" \
    "9 个业务列齐（无金额列）"
assert_eq "0" "$(psql_q "select count(*) from information_schema.columns where table_schema='public' and table_name='biz_cashflow_adjust_item' and data_type='numeric';")" \
    "零 NUMERIC 列（配置表不存金额）"
assert_eq "6" "$(psql_q "select count(*) from pg_constraint where conrelid='public.biz_cashflow_adjust_item'::regclass and conname like 'ck_biz_cashflow_adjust_%';")" \
    "6 条 CHECK 约束"
assert_eq "1" "$(psql_q "select count(*) from pg_constraint where conrelid='public.biz_cashflow_adjust_item'::regclass and contype='u';")" \
    "item_code 唯一约束"
assert_eq "0" "$(psql_q "select count(*) from pg_constraint where conrelid='public.biz_cashflow_adjust_item'::regclass and contype='f';")" \
    "零物理外键（弱引用）"
assert_eq "1" "$(psql_q "select count(*) from pg_indexes where schemaname='public' and tablename='biz_cashflow_adjust_item' and indexname='idx_biz_cashflow_adjust_order';")" \
    "排序索引在位"

# ---------- 4) 种子模板 ----------
info "模板"
assert_eq "12" "$(psql_q "select count(*) from biz_cashflow_adjust_item;")" "12 行标准模板"
assert_eq "12" "$(psql_q "select count(*) from biz_cashflow_adjust_item where is_active=1;")" "全部启用"
assert_eq "2" "$(psql_q "select count(*) from biz_cashflow_adjust_item where source_type='MANUAL';")" "2 行手工填列（递延所得税 / 其他）"
assert_eq "4" "$(psql_q "select count(distinct calc_basis) from biz_cashflow_adjust_item where source_type='ACCOUNT';")" \
    "4 个取数口径全被用到"
assert_eq "10" "$(psql_q "select min(sort_order) from biz_cashflow_adjust_item;")" "顺序从 10 起"
assert_eq "120" "$(psql_q "select max(sort_order) from biz_cashflow_adjust_item;")" "顺序到 120 收口"
assert_eq "0" "$(psql_q "select count(*) from biz_cashflow_adjust_item where source_type='MANUAL' and (calc_basis is not null or account_pattern is not null);")" \
    "MANUAL 行口径 / 模式皆空"
assert_eq "2" "$(psql_q "select count(*) from biz_cashflow_adjust_item where item_code in ('ADJ_DEFERRED_TAX','ADJ_OTHER');")" \
    "两个手工行按编码在位"

# ---------- 5) 权限节点 / 授权 ----------
info "权限"
assert_eq "2" "$(psql_q "select count(*) from sys_menu where id in ('20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800') and menu_type=3 and parent_id='20000000-0000-0000-0000-000000001018' and path is null and status=1 and deleted_at is null and auth_code=name;")" \
    "两个按钮节点在位（挂「现金流量表」页面 · menu_type=3 · 无 path）"
assert_eq "2" "$(psql_q "select count(distinct auth_code) from sys_menu where auth_code in ('business:erp:cashflow-adjust:list','business:erp:cashflow-adjust:manage') and deleted_at is null;")" \
    "两个权限码各一行"
assert_eq "30,40" "$(psql_q "select string_agg(sort_order::text, ',' order by sort_order) from sys_menu where id in ('20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800');")" \
    "同父 sort 30 / 40"
assert_eq "0" "$(psql_q "select count(*) from (select sort_order from sys_menu where parent_id='20000000-0000-0000-0000-000000001018' and menu_type=3 and deleted_at is null group by sort_order having count(*)>1) d;")" \
    "「现金流量表」页面下无重复 sort"
ROLES="$(psql_q "select count(*) from sys_role where deleted_at is null and role_code in ('SUPER_ADMIN','FINANCE_MANAGER');")"
GRANT="$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.deleted_at is null where rm.menu_id in ('20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800') and r.role_code in ('SUPER_ADMIN','FINANCE_MANAGER');")"
assert_eq "$((ROLES * 2))" "$GRANT" "授权齐（${ROLES} 个目标角色 × 2 码）"
assert_eq "0" "$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id where rm.menu_id in ('20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800') and r.role_code='FINANCE_DIRECTOR';")" \
    "FINANCE_DIRECTOR 没授（无页码 ⇒ 会成孤儿按钮）"

# ---------- 6) 幂等 + 不覆盖管理员改动 ----------
info "幂等"
psql_q "update biz_cashflow_adjust_item set item_name='折旧（管理员改过）', sort_order=25 where item_code='ADJ_DEPRECIATION';" >/dev/null
if runfile "$UP0416" "$TMP/m115_up2.log"; then
    ok "V0416 第二次应用成功（重放不报错）"
else
    bad "V0416 重放失败：$(tail -3 "$TMP/m115_up2.log")"
fi
assert_eq "12" "$(psql_q "select count(*) from biz_cashflow_adjust_item;")" "重放不攒行（仍 12 行）"
assert_eq "折旧（管理员改过）" "$(psql_q "select item_name from biz_cashflow_adjust_item where item_code='ADJ_DEPRECIATION';")" \
    "重放不覆盖管理员改过的名称（ON CONFLICT DO NOTHING）"
assert_eq "25" "$(psql_q "select sort_order from biz_cashflow_adjust_item where item_code='ADJ_DEPRECIATION';")" \
    "重放不覆盖管理员调过的顺序"
assert_eq "4" "$(psql_q "select count(*) from sys_role_menu rm join sys_role r on r.id=rm.role_id and r.deleted_at is null where rm.menu_id in ('20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800') and r.role_code in ('SUPER_ADMIN','FINANCE_MANAGER');")" \
    "重放不攒重复授权（仍 $((ROLES * 2)) 行）"

# ---------- 7) 库层约束反证 ----------
info "约束反证"
psql_q "insert into biz_cashflow_adjust_item (item_code,item_name,source_type,calc_basis,account_pattern) values ('X_MANUAL_BASIS','坏行','MANUAL','CREDIT_NET','1602');" >"$TMP/m115_bad1.log" 2>&1 \
    && bad "MANUAL 行带口径竟然插进去了（形状约束没生效）" || ok "MANUAL 行带口径被库层拒"
psql_q "insert into biz_cashflow_adjust_item (item_code,item_name,source_type,calc_basis,account_pattern) values ('X_ACCOUNT_NO_BASIS','坏行','ACCOUNT',NULL,'1602');" >"$TMP/m115_bad2.log" 2>&1 \
    && bad "ACCOUNT 行缺口径竟然插进去了" || ok "ACCOUNT 行缺口径被库层拒"
psql_q "insert into biz_cashflow_adjust_item (item_code,item_name,source_type,calc_basis) values ('X_BAD_BASIS','坏行','ACCOUNT','NET_PROFIT');" >"$TMP/m115_bad3.log" 2>&1 \
    && bad "非法口径竟然插进去了" || ok "非法口径被库层拒"
psql_q "insert into biz_cashflow_adjust_item (item_code,item_name,source_type,calc_basis,account_pattern) values ('X_EMPTY_NAME','   ','MANUAL',NULL,NULL);" >"$TMP/m115_bad4.log" 2>&1 \
    && bad "空名称竟然插进去了" || ok "空名称被库层拒"
psql_q "insert into biz_cashflow_adjust_item (item_code,item_name,source_type,calc_basis,account_pattern) values ('ADJ_OTHER','重复码','MANUAL',NULL,NULL);" >"$TMP/m115_bad5.log" 2>&1 \
    && bad "重复编码竟然插进去了（唯一约束没生效）" || ok "重复编码被唯一约束拒"

# ---------- 8) down（无自建配置 → 可回滚） ----------
info "回滚"
if runfile "$DOWN0416" "$TMP/m115_down1.log"; then
    ok "无自建配置时 down 成功"
    assert_eq "0" "$(psql_q "select count(*) from information_schema.tables where table_schema='public' and table_name='biz_cashflow_adjust_item';")" \
        "down 后表已删"
    assert_eq "0" "$(psql_q "select count(*) from sys_menu where id in ('20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800');")" \
        "down 后按钮节点已清"
    assert_eq "0" "$(psql_q "select count(*) from sys_role_menu where menu_id in ('20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800');")" \
        "down 后授权已清"
else
    bad "无自建配置时 down 失败：$(tail -3 "$TMP/m115_down1.log")"
fi
if runfile "$DOWN0416" "$TMP/m115_down2.log"; then
    ok "down 重放幂等（第二次也成功）"
else
    bad "down 重放失败：$(tail -3 "$TMP/m115_down2.log")"
fi

# ---------- 9) down 护栏反证 ----------
info "护栏"
runfile "$UP0416" "$TMP/m115_up3.log" || bad "重放 up 失败：$(tail -3 "$TMP/m115_up3.log")"
psql_q "insert into biz_cashflow_adjust_item (item_code,item_name,source_type,calc_basis,account_pattern,sort_order) values ('ADJ_CUSTOM_ONE','自建调整项','ACCOUNT','CREDIT_NET','1601',130);" >/dev/null
if runfile "$DOWN0416" "$TMP/m115_down3.log"; then
    bad "有自建配置时 down 竟然成功（护栏没生效）"
else
    grep -q "拒绝执行" "$TMP/m115_down3.log" && ok "有自建配置时 down 被拒（护栏生效）" \
        || bad "down 失败但不是护栏拦的：$(tail -3 "$TMP/m115_down3.log")"
fi
assert_eq "1" "$(psql_q "select count(*) from biz_cashflow_adjust_item where item_code='ADJ_CUSTOM_ONE';")" \
    "护栏拒绝后数据原样还在"
assert_eq "2" "$(psql_q "select count(*) from sys_menu where id in ('20000000-0000-0000-0000-000000001790','20000000-0000-0000-0000-000000001800');")" \
    "护栏拒绝后菜单节点没被提前删掉"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[ "$FAIL" -eq 0 ] || exit 1
