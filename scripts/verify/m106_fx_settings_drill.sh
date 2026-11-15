#!/usr/bin/env bash
# =============================================================================
# m106_fx_settings_drill.sh —— V0411（M1-06 汇兑损益科目配置）迁移级演练
#
# 卡：t_dc8892cb。**psql 手动往返**（不等引擎），覆盖引擎看不到的路径：
#   A) up 连跑两次（幂等：参数行一条、菜单行一条、授权不重复）
#   B) 结构 / 口径断言：参数键 type=STRING/可维护/启用；菜单 type=3、父=汇率管理、
#      sort=60、title ≤12 字；同父 sort 唯一；授权到 SUPER_ADMIN + FINANCE_MANAGER
#   C) 幂等反证：把参数值改成 5601 后重放 up —— **不许被种子里覆盖回 5603**
#      （运维改过的配置被迁移顶回去 = 静默改账务口径）
#   D) down → 断言菜单/授权/参数行都清掉；再 up → 回来（可反复）
#   E) down 护栏：参数值已改成非 5603 时，down **保留参数行**并打 NOTICE
#      （删了就是把人的配置意图丢了）
#
# 用法：bash scripts/verify/m106_fx_settings_drill.sh [PG_HOST]
#   默认 PG_HOST=172.18.10.145（99 上跑）；145 上跑传 127.0.0.1。
# 演练库名含 drill，脚本自己 DROP（模板库 cloudpivot_test 只读克隆）。
# =============================================================================
set -uo pipefail

PG_HOST="${1:-172.18.10.145}"
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
PG_USER=cloudpivot
TEMPLATE=cloudpivot_test
DB=cp_m106_drill
REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
UP="$REPO_ROOT/backend/app/migrations/business/V0411__fx_gain_loss_account.up.sql"
DOWN="$REPO_ROOT/backend/app/migrations/business/V0411__fx_gain_loss_account.down.sql"
NODE_ID='20000000-0000-0000-0000-000000001740'
PAGE_ID='20000000-0000-0000-0000-000000003168'
KEY='erp.fx.gain_loss_account'

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
echo "$OUT1" | grep -E "NOTICE:  V0411 自检通过" >/dev/null && ok "第一次 up：自检通过" || { bad "第一次 up 失败"; echo "$OUT1" | tail -8; }
[[ $RC1 -eq 0 ]] || bad "第一次 up 退出码 $RC1"
OUT2="$(run_sql "$UP")"; RC2=$?
[[ $RC2 -eq 0 ]] && ok "第二次 up：退出码 0（幂等重放）" || { bad "第二次 up 退出码 $RC2"; echo "$OUT2" | tail -8; }

N="$(q "select count(*) from sys_config where config_key='$KEY' and deleted_at is null")"
[[ "$N" == "1" ]] && ok "参数行只有 1 条（重放不重复插）" || bad "参数行 $N 条"
N="$(q "select count(*) from sys_menu where auth_code='business:erp:fx-settings:update' and deleted_at is null")"
[[ "$N" == "1" ]] && ok "菜单节点只有 1 条" || bad "菜单节点 $N 条"
N="$(q "select count(*) from sys_role_menu where menu_id='$NODE_ID'::uuid")"
[[ "$N" == "2" ]] && ok "授权 2 行（SUPER_ADMIN + FINANCE_MANAGER，重放不重复）" || bad "授权 $N 行"

echo
echo "== B) 结构 / 口径断言 =="
CFG="$(q "select config_value||'/'||config_type||'/'||is_modifiable||'/'||status||'/'||category from sys_config where config_key='$KEY' and deleted_at is null")"
echo "  参数行：$CFG"
[[ "$CFG" == "5603/STRING/true/1/erp_fx" ]] \
    && ok "参数：值 5603 / STRING / 可维护 / 启用 / 分类 erp_fx" || bad "参数行口径不对：[$CFG]"

ROW="$(q "select menu_type||'/'||coalesce(parent_id::text,'-')||'/'||sort_order||'/'||status||'/'||is_visible||'/'||title
           from sys_menu where auth_code='business:erp:fx-settings:update'")"
echo "  节点：$ROW"
[[ "$ROW" == "3/$PAGE_ID/60/1/true/汇兑损益科目" ]] \
    && ok "按钮行：type=3 / 父=汇率管理 / sort=60 / 启用 / 可见 / title=汇兑损益科目" || bad "节点口径不对：[$ROW]"
LEN="$(q "select length(title) from sys_menu where auth_code='business:erp:fx-settings:update'")"
[[ "${LEN:-99}" -le 12 ]] && ok "title 长度 $LEN ≤ 12（菜单规范）" || bad "title 长度 $LEN > 12"
DUP="$(q "select count(*) from (select sort_order from sys_menu
           where parent_id='$PAGE_ID'::uuid and menu_type=3 and deleted_at is null
           group by sort_order having count(*)>1) x")"
[[ "$DUP" == "0" ]] && ok "「汇率管理」页同父 sort_order 无重复" || bad "同父 sort 有 $DUP 组重复"
ROLES="$(q "select string_agg(r.role_code, ',' order by r.role_code) from sys_role_menu rm
            join sys_role r on r.id=rm.role_id and r.deleted_at is null
            where rm.menu_id='$NODE_ID'::uuid")"
[[ "$ROLES" == "FINANCE_MANAGER,SUPER_ADMIN" ]] && ok "授权角色 = $ROLES" || bad "授权角色不对：[$ROLES]"

echo
echo "== C) 幂等反证：改了值再重放 up，不许把运维配置顶回 5603 =="
q "update sys_config set config_value='5601' where config_key='$KEY' and deleted_at is null" >/dev/null
OUT3="$(run_sql "$UP")"; RC3=$?
[[ $RC3 -eq 0 ]] && ok "改值后重放 up：退出码 0" || { bad "重放 up 退出码 $RC3"; echo "$OUT3" | tail -8; }
VAL="$(q "select config_value from sys_config where config_key='$KEY' and deleted_at is null")"
[[ "$VAL" == "5601" ]] && ok "重放后参数值仍是 5601（迁移不覆盖已存在的配置）" || bad "重放把配置改回了 [$VAL]"
q "update sys_config set config_value='5603' where config_key='$KEY' and deleted_at is null" >/dev/null

echo
echo "== D) down → 再 up（可反复）=="
OUT4="$(run_sql "$DOWN")"; RC4=$?
[[ $RC4 -eq 0 ]] && ok "down：退出码 0" || { bad "down 退出码 $RC4"; echo "$OUT4" | tail -8; }
N="$(q "select count(*) from sys_menu where auth_code='business:erp:fx-settings:update'")"
[[ "$N" == "0" ]] && ok "down 后菜单节点已删" || bad "down 后节点残留 $N 条"
N="$(q "select count(*) from sys_role_menu where menu_id='$NODE_ID'::uuid")"
[[ "$N" == "0" ]] && ok "down 后授权行已删" || bad "down 后授权残留 $N 行"
N="$(q "select count(*) from sys_config where config_key='$KEY'")"
[[ "$N" == "0" ]] && ok "down 后参数行已删（值仍是缺省 5603 ⇒ 无信息可留）" || bad "down 后参数行残留 $N 条"
OUT5="$(run_sql "$UP")"; RC5=$?
[[ $RC5 -eq 0 ]] && ok "down 后再 up：退出码 0（可反复）" || { bad "down→up 退出码 $RC5"; echo "$OUT5" | tail -8; }
N="$(q "select count(*) from sys_menu where auth_code='business:erp:fx-settings:update' and deleted_at is null")"
[[ "$N" == "1" ]] && ok "重新 up 后节点回到 1 条" || bad "重新 up 后节点 $N 条"
N="$(q "select count(*) from sys_config where config_key='$KEY' and deleted_at is null")"
[[ "$N" == "1" ]] && ok "重新 up 后参数行回到 1 条" || bad "重新 up 后参数行 $N 条"

echo
echo "== E) down 护栏：参数被改成非缺省值时保留参数行 =="
q "update sys_config set config_value='5602.01' where config_key='$KEY' and deleted_at is null" >/dev/null
OUT6="$(run_sql "$DOWN")"; RC6=$?
[[ $RC6 -eq 0 ]] && ok "改值后 down：退出码 0（不因参数被改过而报错）" || { bad "改值后 down 退出码 $RC6"; echo "$OUT6" | tail -8; }
echo "$OUT6" | grep -E "保留该参数行" >/dev/null && ok "down 打了「保留参数行」NOTICE" || bad "down 没说明为什么保留参数行"
VAL="$(q "select config_value from sys_config where config_key='$KEY' and deleted_at is null")"
[[ "$VAL" == "5602.01" ]] && ok "参数行保留且值仍是 5602.01（运维意图不丢）" || bad "改值后 down 把参数行动了：[${VAL:-空}]"
N="$(q "select count(*) from sys_menu where auth_code='business:erp:fx-settings:update'")"
[[ "$N" == "0" ]] && ok "改值后 down 仍清掉了菜单节点" || bad "改值后 down 菜单残留 $N 条"

echo
echo "== 汇总：PASS=$PASS FAIL=$FAIL =="
[[ "$FAIL" == "0" ]] || exit 1
