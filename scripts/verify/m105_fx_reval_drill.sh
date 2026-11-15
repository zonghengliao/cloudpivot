#!/usr/bin/env bash
# =============================================================================
# m105_fx_reval_drill.sh —— V0410（M1-05 期末调汇）迁移级演练
#
# 卡：t_02c8407a。**psql 手动往返**（不等引擎），覆盖引擎看不到的路径：
#   A) up 连跑两次（幂等：菜单行一条、授权不重复）
#   B) down（删节点 + 授权，凭证/期间一行不动）
#   C) down 后再 up（可反复）
#   D) down 护栏反证：库里存在 FX_REVAL_ACCOUNT 凭证时，down 必须 RAISE EXCEPTION
#   E) 结构 / 口径断言：节点 type=3、父=会计期间、sort 同父唯一、title ≤12 字、授权到
#      SUPER_ADMIN + FINANCE_MANAGER
#
# 用法：bash scripts/verify/m105_fx_reval_drill.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 演练库名含 drill，脚本自己 DROP（模板库 cloudpivot_test 只读克隆）。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB=cp_m105_drill
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
UP="$REPO_ROOT/backend/app/migrations/business/V0410__period_fx_revaluation.up.sql"
DOWN="$REPO_ROOT/backend/app/migrations/business/V0410__period_fx_revaluation.down.sql"

PASS=0
FAIL=0
ok()  { PASS=$((PASS+1)); echo "  PASS  $*"; }
bad() { FAIL=$((FAIL+1)); echo "  FAIL  $*"; }

psql_admin() { psql -h "$PG_HOST" -U "$PG_USER" -d postgres -tAc "$1"; }
q()          { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -tAc "$1"; }
run_sql()    { psql -h "$PG_HOST" -U "$PG_USER" -d "$DB" -v ON_ERROR_STOP=1 -q -f "$1" 2>&1; }

cleanup() { psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1; }
trap cleanup EXIT

echo "== 准备：克隆模板库 → $DB（$PG_HOST）=="
psql_admin "DROP DATABASE IF EXISTS $DB" >/dev/null 2>&1
if ! psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null 2>&1; then
    echo "  克隆失败（模板被占用？）5 秒后重试"
    sleep 5
    psql_admin "CREATE DATABASE $DB TEMPLATE $TEMPLATE" >/dev/null || { echo "FATAL: 建演练库失败"; exit 2; }
fi
ok "演练库 $DB 已从 $TEMPLATE 克隆"

echo
echo "== A) up 连跑两次（幂等）=="
OUT1="$(run_sql "$UP")"; RC1=$?
echo "$OUT1" | grep -E "NOTICE:  V0410 自检通过" >/dev/null && ok "第一次 up：自检通过" || { bad "第一次 up 失败"; echo "$OUT1" | tail -5; }
[[ $RC1 -eq 0 ]] || bad "第一次 up 退出码 $RC1"
OUT2="$(run_sql "$UP")"; RC2=$?
[[ $RC2 -eq 0 ]] && ok "第二次 up：退出码 0（幂等重放）" || { bad "第二次 up 退出码 $RC2"; echo "$OUT2" | tail -5; }
N="$(q "select count(*) from sys_menu where auth_code='business:erp:period:fx-reval'")"
[[ "$N" == "1" ]] && ok "菜单节点只有 1 条（重放不重复插）" || bad "菜单节点 $N 条"
N="$(q "select count(*) from sys_role_menu where menu_id='20000000-0000-0000-0000-000000001730'::uuid")"
[[ "$N" == "2" ]] && ok "授权 2 行（SUPER_ADMIN + FINANCE_MANAGER，重放不重复）" || bad "授权 $N 行"

echo
echo "== B) 结构 / 口径断言 =="
ROW="$(q "select menu_type||'/'||coalesce(parent_id::text,'-')||'/'||sort_order||'/'||status||'/'||is_visible||'/'||title
           from sys_menu where auth_code='business:erp:period:fx-reval'")"
echo "  节点：$ROW"
[[ "$ROW" == "3/20000000-0000-0000-0000-000000000951/6/1/true/期末调汇" ]] \
    && ok "按钮行：type=3 / 父=会计期间 / sort=6 / 启用 / 可见 / title=期末调汇" || bad "节点口径不对：[$ROW]"
LEN="$(q "select length(title) from sys_menu where auth_code='business:erp:period:fx-reval'")"
[[ "${LEN:-99}" -le 12 ]] && ok "title 长度 $LEN ≤ 12（菜单规范）" || bad "title 长度 $LEN > 12"
DUP="$(q "select count(*) from (select sort_order from sys_menu
           where parent_id='20000000-0000-0000-0000-000000000951'::uuid and menu_type=3 and deleted_at is null
           group by sort_order having count(*)>1) x")"
[[ "$DUP" == "0" ]] && ok "「会计期间」页同父 sort_order 无重复" || bad "同父 sort 有 $DUP 组重复"
ROLES="$(q "select string_agg(r.role_code, ',' order by r.role_code) from sys_role_menu rm
            join sys_role r on r.id=rm.role_id and r.deleted_at is null
            where rm.menu_id='20000000-0000-0000-0000-000000001730'::uuid")"
[[ "$ROLES" == "FINANCE_MANAGER,SUPER_ADMIN" ]] && ok "授权角色 = $ROLES" || bad "授权角色不对：[$ROLES]"

echo
echo "== C) down → up（可反复）=="
OUT3="$(run_sql "$DOWN")"; RC3=$?
[[ $RC3 -eq 0 ]] && ok "down：退出码 0" || { bad "down 退出码 $RC3"; echo "$OUT3" | tail -5; }
N="$(q "select count(*) from sys_menu where auth_code='business:erp:period:fx-reval'")"
[[ "$N" == "0" ]] && ok "down 后菜单节点已删" || bad "down 后节点残留 $N 条"
N="$(q "select count(*) from sys_role_menu where menu_id='20000000-0000-0000-0000-000000001730'::uuid")"
[[ "$N" == "0" ]] && ok "down 后授权行已删" || bad "down 后授权残留 $N 行"
OUT4="$(run_sql "$UP")"; RC4=$?
[[ $RC4 -eq 0 ]] && ok "down 后再 up：退出码 0（可反复）" || { bad "down→up 退出码 $RC4"; echo "$OUT4" | tail -5; }
N="$(q "select count(*) from sys_menu where auth_code='business:erp:period:fx-reval'")"
[[ "$N" == "1" ]] && ok "重新 up 后节点回到 1 条" || bad "重新 up 后节点 $N 条"

echo
echo "== D) down 护栏反证：有 FX_REVAL_ACCOUNT 凭证时必须拒删 =="
q "insert into biz_voucher (voucher_word, voucher_no, period, voucher_date, total_debit, total_credit,
                            entry_count, status, source_type, source_no, remark)
   values ('记','M105-DRILL-V','2026-10',date '2026-10-31',1,1,2,'POSTED','FX_REVAL_ACCOUNT','2026-10','M1-05 down 护栏反证')
   on conflict do nothing" >/dev/null
N="$(q "select count(*) from biz_voucher where source_type='FX_REVAL_ACCOUNT'")"
[[ "$N" == "1" ]] && ok "已造 1 张调汇凭证（反证夹具）" || bad "反证夹具没造出来（$N）"
OUT5="$(run_sql "$DOWN")"; RC5=$?
[[ $RC5 -ne 0 ]] && ok "down 被护栏拦下（退出码 $RC5）" || bad "down 竟然执行成功 —— 记账依据会被抹掉"
echo "$OUT5" | grep -q "拒绝执行" && ok "护栏文案点名原因（拒绝执行）" || bad "护栏没有可说理的文案：$(echo "$OUT5" | tail -2)"
N="$(q "select count(*) from sys_menu where auth_code='business:erp:period:fx-reval'")"
[[ "$N" == "1" ]] && ok "被拦下时节点仍在（down 整笔回滚）" || bad "被拦下却删了节点（$N）"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[[ "$FAIL" == "0" ]] || exit 1
