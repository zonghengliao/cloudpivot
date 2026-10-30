#!/bin/bash
# =============================================================================
# W2c-2 真机验收：库存巡检（不变式③）+ 移仓函数（不变式④）
#
# 用法（在 144 上以 root 跑；自动建一次性库并起隔离实例）：
#
#   bash scripts/verify/w2c2_audit.sh                    # 全跑，跑完保留库看数据
#   W2C2_KEEP=0 bash scripts/verify/w2c2_audit.sh         # 跑完删库
#   REPO=/root/w2c2 W2C2_BIN=/root/w2c2-target/debug/cloudpivot \
#       W2C2_DB=cloudpivot_w2c2_verify W2C2_PORT=18088 bash scripts/verify/w2c2_audit.sh
#
# 依赖：curl / jq / psql / 应用二进制 / 环境文件 /etc/cloudpivot/cloudpivot-dev.env
#
# 覆盖（每条都打真实 HTTP，不是单测替代）：
#   ① 隔离实例就绪 + 迁移到 V0143（max(version::int)=143）
#   ② V0143 落库：business:erp:inventory:audit 在 sys_menu 且挂在 0955 下、超管已授权
#   ③ 夹具：单位 / 两仓 / 一物一码物料 + 按数量物料
#   ④ 批量生成 3 件 → 账面 qty=3，巡检 mismatched=0（干净基线）
#   ⑤ 手工 UPDATE qty=5（造「账多 2 件」）→ audit?dry_run=true 报 diff=2
#   ⑥ dry_run=true 不改数据（qty 仍是 5）
#   ⑦ audit?dry_run=false → fixed=1，qty 修正回 3，再巡检 mismatched=0
#   ⑧ 反向：删掉 biz_inventory 整行 → 巡检报 book_qty=0 / actual_qty=3（不写成 0）
#   ⑨ 反向脏账 dry_run=false → 重新建档，qty=3，mismatched=0
#   ⑩ dry_run 缺省 = true（不带参数只报不修）、非法值 dry_run=treu → 400 + 66006
#   ⑪ 权限：有码能过 / 无码账号 → 403
#
# 幂等：开头重建库 + 杀旧实例；夹具编码带 `W2C2-E2E` 前缀。
# 说明：不变式④（move_serials）**没有 HTTP 接口**（卡面明确「本卡只写函数」），
#       它的行为由 3 条真库单测钉住（`cargo test --lib business::erp::inventory_audit`），
#       本脚本只验它「不该出现在 HTTP 面」这件事不成立（无对应路由 = 404，不算失败）。
# =============================================================================
set -u

REPO=${REPO:-/opt/cloudpivot}
W2C2_DB=${W2C2_DB:-cloudpivot_w2c2_verify}
W2C2_PORT=${W2C2_PORT:-18088}
W2C2_BIN=${W2C2_BIN:-$REPO/backend/target/debug/cloudpivot}
ENV_FILE=${ENV_FILE:-/etc/cloudpivot/cloudpivot-dev.env}
ADMIN_BOOT_PW=${W2C2_ADMIN_BOOT_PW:-W2c2Admin@2026}
ADMIN_FINAL_PW=${W2C2_ADMIN_FINAL_PW:-W2c2Admin@2026!}
NOPERM_PW=${W2C2_NOPERM_PW:-W2c2NoPerm@2026}
PID_FILE=/tmp/w2c2_verify_server.pid
LOG=/tmp/w2c2_verify_server.log
BODY=/tmp/w2c2_verify_body.json
BASE="http://127.0.0.1:$W2C2_PORT"
PREFIX="W2C2-E2E"
SUFFIX="$$"

PASS=0
FAIL=0
TOKEN=""
FAILED_CASES=()

ok()   { PASS=$((PASS + 1)); printf '[PASS] %s\n' "$1"; }
bad()  { FAIL=$((FAIL + 1)); FAILED_CASES+=("$1"); printf '[FAIL] %s  << %s\n' "$1" "${2:-}"; }
note() { printf '       %s\n' "$1"; }

# 断言工具必须先定义、后使用（否则分支里调用会 command not found 且断言被静默跳过）
api() { # method path [json]
    local method=$1 path=$2 data=${3:-}
    local -a args=(-s -o "$BODY" -w '%{http_code}' -X "$method" -H 'Content-Type: application/json')
    [ -n "$TOKEN" ] && args+=(-H "Authorization: Bearer $TOKEN")
    [ -n "$data" ] && args+=(-d "$data")
    HTTP=$(curl "${args[@]}" "$BASE/api$path")
    BODY_TEXT=$(cat "$BODY")
}
expect() { # name want_http want_code needle
    local name=$1 want_http=$2 want_code=$3 needle=${4:-}
    local got_code
    got_code=$(printf '%s' "$BODY_TEXT" | jq -r '.code // "-"' 2>/dev/null || echo "-")
    if [ "$HTTP" != "$want_http" ]; then
        bad "$name" "HTTP $HTTP（期望 $want_http）$BODY_TEXT"; return 1
    fi
    if [ "$want_code" != "-" ] && [ "$got_code" != "$want_code" ]; then
        bad "$name" "code=$got_code（期望 $want_code）$BODY_TEXT"; return 1
    fi
    if [ -n "$needle" ] && ! printf '%s' "$BODY_TEXT" | grep -q -- "$needle"; then
        bad "$name" "响应里找不到「$needle」：$BODY_TEXT"; return 1
    fi
    ok "$name"
    return 0
}
jget() { printf '%s' "$BODY_TEXT" | jq -r "$1"; }
# Decimal 回读是 "5.0000"：去掉尾随小数位再比
dec() { printf '%s' "$1" | sed -E 's/\.0+$//'; }

# ---------------------------------------------------------------- 0. 库与实例
echo "== 0) 准备隔离库 $W2C2_DB + 隔离实例 :$W2C2_PORT"
if [ -f "$PID_FILE" ]; then
    kill "$(cat "$PID_FILE")" 2>/dev/null || true
    sleep 2
fi
su - postgres -c "psql -Atc \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$W2C2_DB'\"" >/dev/null 2>&1
su - postgres -c "dropdb --if-exists $W2C2_DB" >/dev/null 2>&1
su - postgres -c "createdb -O cloudpivot $W2C2_DB" || { echo "建库失败"; exit 1; }

set -a
# shellcheck disable=SC1090
. "$ENV_FILE"
set +a
export NIK_DATABASE_NAME="$W2C2_DB"
export NIK_SERVER_LISTEN="127.0.0.1:$W2C2_PORT"
export NIK_REDIS_URL="redis://127.0.0.1:6379/9"
export NIK_ADMIN_PASSWORD="$ADMIN_BOOT_PW"
export RUST_LOG=info

cd "$REPO/backend" || exit 1
[ -x "$W2C2_BIN" ] || { echo "找不到二进制：$W2C2_BIN"; exit 1; }
rm -f "$LOG"
setsid nohup "$W2C2_BIN" >"$LOG" 2>&1 </dev/null &
echo $! >"$PID_FILE"

for _ in $(seq 1 60); do
    sleep 2
    grep -q "cloudpivot 启动完成" "$LOG" && break
    kill -0 "$(cat "$PID_FILE")" 2>/dev/null || { echo "实例启动失败："; tail -8 "$LOG"; exit 1; }
done
HC=$(curl -s -o /dev/null -w '%{http_code}' "$BASE/health")
[ "$HC" = "200" ] && ok "隔离实例就绪（/health=200）" || { bad "隔离实例就绪" "health=$HC"; tail -5 "$LOG"; exit 1; }
note "$(grep -o '数据库迁移完成.*' "$LOG" | head -1)"

export PGPASSWORD="$NIK_DATABASE_PASSWORD"
psql_q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$W2C2_DB" -Atc "$1" 2>&1; }
one() { printf '%s\n' "$1" | head -1; }   # psql 多行输出只取第一行（避免带命令标签）

cleanup() {
    kill "$(cat "$PID_FILE" 2>/dev/null)" 2>/dev/null || true
    sleep 2
    kill -9 "$(cat "$PID_FILE" 2>/dev/null)" 2>/dev/null || true
    if [ "${W2C2_KEEP:-1}" = "0" ]; then
        su - postgres -c "dropdb --if-exists $W2C2_DB" >/dev/null 2>&1
        note "已删除隔离库 $W2C2_DB（W2C2_KEEP=0）"
    else
        note "保留隔离库 $W2C2_DB（W2C2_KEEP=0 可自动删）"
    fi
}
trap cleanup EXIT

# ---------------------------------------------------------------- 1. 迁移与权限码落库
echo "== 1) 迁移 V0143 + 权限码落库"
VER=$(one "$(psql_q "SELECT max(version::int) FROM schema_migrations")")
[ "$VER" = "143" ] && ok "迁移已应用到 V0143（max(version::int)=$VER）" || bad "迁移版本" "实际 $VER（期望 143）"
MENU=$(one "$(psql_q "SELECT menu_type || '~' || coalesce(parent_id::text,'-') FROM sys_menu WHERE auth_code='business:erp:inventory:audit' AND deleted_at IS NULL")")
[ "$MENU" = "3~20000000-0000-0000-0000-000000000955" ] \
    && ok "权限码落在 sys_menu（按钮 3 / 挂在 0955 即时库存下）" || bad "权限码落库" "实际 $MENU"
n_same=$(one "$(psql_q "SELECT count(*) FROM sys_menu WHERE auth_code='business:erp:inventory:audit' AND deleted_at IS NULL")")
[ "$n_same" = "1" ] && ok "  权限码恰好 1 行（无重复菜单）" || bad "  权限码重复" "实际 $n_same 行"

# ---------------------------------------------------------------- 2. 登录
echo "== 2) 登录（admin，首登会强制改密）"
login() { # password
    local p=$1
    api POST /system/auth/login "$(jq -nc --arg u admin --arg p "$p" '{username:$u,password:$p}')"
    if [ "$HTTP" = "200" ]; then
        TOKEN=$(jq -r '.data.access_token // empty' "$BODY")
        MUST=$(jq -r '.data.must_change_password // false' "$BODY")
    fi
}
MUST=false
login "$ADMIN_FINAL_PW"
[ -z "$TOKEN" ] && login "$ADMIN_BOOT_PW"
[ -n "$TOKEN" ] && ok "admin 登录（must_change_password=${MUST:-unknown}）" || { bad "admin 登录" "$BODY_TEXT"; exit 1; }
if [ "${MUST:-false}" = "true" ]; then
    api PUT /system/auth/password "$(jq -nc --arg o "$ADMIN_BOOT_PW" --arg n "$ADMIN_FINAL_PW" '{old_password:$o,new_password:$n}')"
    expect "PUT /system/auth/password 首登改密" 200 0 || note "改密响应：$BODY_TEXT"
    TOKEN=""
    login "$ADMIN_FINAL_PW"
    [ -n "$TOKEN" ] && ok "改密后重新登录" || { bad "改密后重新登录" "$BODY_TEXT"; exit 1; }
fi

# ---------------------------------------------------------------- 2.5 授权补齐（重放迁移）
echo "== 2.5) V0143 授权：空库首启时角色还不存在（迁移先于 bootstrap）→ bootstrap 后重放补齐"
REPLAY_LOG=/tmp/w2c2_v0143_replay.log
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$W2C2_DB" -q \
     -f "$REPO/backend/migrations/business/V0143__erp_audit_permission.up.sql" >"$REPLAY_LOG" 2>&1
REPLAY_RC=$?
grep -E "NOTICE" "$REPLAY_LOG" | sed 's/^/       /' | tail -3
[ "$REPLAY_RC" = "0" ] && ok "V0143 重放（幂等）：exit=0" || bad "V0143 重放" "exit=$REPLAY_RC $(tail -3 "$REPLAY_LOG")"
GRANT=$(one "$(psql_q "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id WHERE r.role_code='SUPER_ADMIN' AND rm.menu_id='20000000-0000-0000-0000-000000000986'")")
[ "$GRANT" = "1" ] && ok "超管已授权巡检按钮（重放后补齐）" || bad "超管授权" "实际 $GRANT"
DUP=$(one "$(psql_q "SELECT count(*) FROM (SELECT menu_id FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id WHERE r.role_code='SUPER_ADMIN' GROUP BY menu_id HAVING count(*)>1) t")")
[ "$DUP" = "0" ] && ok "  重放不产生重复授权" || bad "  重复授权" "实际 $DUP 组"
# 重放不能把菜单标题改回去（ON CONFLICT DO NOTHING 的意义）
psql_q "UPDATE sys_menu SET title='W2C2 手工改过的标题' WHERE id='20000000-0000-0000-0000-000000000986'" >/dev/null
psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -d "$W2C2_DB" -q \
     -f "$REPO/backend/migrations/business/V0143__erp_audit_permission.up.sql" >/dev/null 2>&1
TITLE=$(one "$(psql_q "SELECT title FROM sys_menu WHERE id='20000000-0000-0000-0000-000000000986'")")
[ "$TITLE" = "W2C2 手工改过的标题" ] && ok "  重放不覆盖运维改过的标题" || bad "  重放覆盖了标题" "实际 $TITLE"
psql_q "UPDATE sys_menu SET title='库存巡检（账面 vs 序列号件数）' WHERE id='20000000-0000-0000-0000-000000000986'" >/dev/null

# ---------------------------------------------------------------- 3. 夹具
echo "== 3) 夹具：单位 / 两仓 / 一物一码物料 + 按数量物料"
api POST /business/erp/unit "$(jq -nc --arg c "$PREFIX-U" '{unit_code:$c,unit_name:"W2C2 个",is_base:true,status:1,sort_order:1}')"
expect "POST /business/erp/unit" 200 0 && UNIT_ID=$(jget '.data.id')

api POST /business/erp/warehouse "$(jq -nc --arg c "$PREFIX-WA" '{warehouse_code:$c,warehouse_name:"W2C2 A 仓",warehouse_type:"NORMAL",status:1,sort_order:1,parent_id:null,level:1}')"
expect "POST /business/erp/warehouse（A 仓）" 200 0 && WH_A=$(jget '.data.id')
api POST /business/erp/warehouse "$(jq -nc --arg c "$PREFIX-WB" '{warehouse_code:$c,warehouse_name:"W2C2 B 仓",warehouse_type:"NORMAL",status:1,sort_order:2,parent_id:null,level:1}')"
expect "POST /business/erp/warehouse（B 仓）" 200 0 && WH_B=$(jget '.data.id')

api POST /business/erp/material "$(jq -nc --arg c "$PREFIX-M-SN" --arg u "$UNIT_ID" --arg w "$WH_A" \
    '{material_code:$c,material_name:"W2C2 巡检设备",spec:"SN-1",unit_id:$u,material_type:"FINISHED",default_warehouse_id:$w,safety_stock:"0.0000",costing_method:"MOVING_AVG",tax_rate:"0.1300",status:1,serial_required:true}')"
expect "POST material（serial_required=true）" 200 0 && M_SN=$(jget '.data.id')
api GET "/business/erp/material/$M_SN"
[ "$(jget '.data.serial_required')" = "true" ] && ok "  serial_required=true 已落库" || bad "  serial_required 回读" "$BODY_TEXT"

api POST /business/erp/material "$(jq -nc --arg c "$PREFIX-M-PLAIN" --arg u "$UNIT_ID" --arg w "$WH_A" \
    '{material_code:$c,material_name:"W2C2 按数量物料",spec:"P-1",unit_id:$u,material_type:"RAW",default_warehouse_id:$w,safety_stock:"0.0000",costing_method:"MOVING_AVG",tax_rate:"0.1300",status:1,serial_required:false}')"
expect "POST material（按数量管理）" 200 0 && M_PLAIN=$(jget '.data.id')

# ---------------------------------------------------------------- 4. 干净基线
echo "== 4) 批量生成 3 件 → 账面 3、巡检零不一致"
api POST /business/erp/serial/batch-generate "$(jq -nc --arg m "$M_SN" --arg w "$WH_A" '{material_id:$m,warehouse_id:$w,count:3}')"
expect "POST serial/batch-generate（3 件）" 200 0
QTY=$(one "$(psql_q "SELECT qty FROM biz_inventory WHERE material_id='$M_SN'::uuid AND warehouse_id='$WH_A'::uuid")")
[ "$(dec "$QTY")" = "3" ] && ok "  账面 qty=3（由序列号派生）" || bad "  账面 qty" "实际 $QTY"

api GET "/business/erp/inventory/audit?dry_run=true"
expect "GET inventory/audit?dry_run=true（干净基线）" 200 0
MIS=$(jget '.data.mismatched'); CHK=$(jget '.data.checked'); FIXED=$(jget '.data.fixed')
[ "$MIS" = "0" ] && ok "  mismatched=0" || bad "  mismatched" "实际 $MIS（checked=$CHK items=$(jget '.data.items|length')）"
[ "$CHK" -ge 1 ] && ok "  checked=$CHK（真检查过组合）" || bad "  checked" "实际 $CHK"
[ "$FIXED" = "0" ] && ok "  fixed=0（dry_run）" || bad "  fixed" "实际 $FIXED"
[ "$(jget '.data.dry_run')" = "true" ] && ok "  dry_run 回显 true" || bad "  dry_run 回显" "$BODY_TEXT"

# ---------------------------------------------------------------- 5. 账多 2 件
echo "== 5) 造脏：手工把 qty 改成 5（账多 2 件）→ dry_run 只报不修"
psql_q "UPDATE biz_inventory SET qty=5 WHERE material_id='$M_SN'::uuid AND warehouse_id='$WH_A'::uuid" >/dev/null
api GET "/business/erp/inventory/audit?dry_run=true"
expect "GET inventory/audit?dry_run=true（账多）" 200 0
MIS=$(jget '.data.mismatched')
ITEM=$(printf '%s' "$BODY_TEXT" | jq -r --arg m "$M_SN" '.data.items[] | select(.material_id==$m) | "\(.book_qty)~\(.actual_qty)~\(.diff)~\(.material_code)"')
[ "$MIS" = "1" ] && ok "  mismatched=1（查出来了）" || bad "  mismatched" "实际 $MIS $BODY_TEXT"
WANT="5.0000~3.0000~2.0000~${PREFIX}-M-SN"
[ "$ITEM" = "$WANT" ] && ok "  book=5 / actual=3 / diff=+2 / 物料定位准确" || bad "  明细口径" "实际 $ITEM（期望 $WANT）"
QTY=$(one "$(psql_q "SELECT qty FROM biz_inventory WHERE material_id='$M_SN'::uuid AND warehouse_id='$WH_A'::uuid")")
[ "$(dec "$QTY")" = "5" ] && ok "  dry_run=true 没改库（qty 仍是 5）" || bad "  dry_run 改了库" "qty=$QTY"

# 缺省 dry_run 也必须是 true（红线③）
api GET "/business/erp/inventory/audit"
MIS=$(jget '.data.mismatched'); FIXED=$(jget '.data.fixed')
[ "$MIS" = "1" ] && [ "$FIXED" = "0" ] && ok "  不带 dry_run 参数 → 缺省只报不修（mismatched=1 / fixed=0）" \
    || bad "  缺省 dry_run" "mismatched=$MIS fixed=$FIXED"

# ---------------------------------------------------------------- 6. 修正
echo "== 6) audit?dry_run=false → 修正回 3，再巡检归零"
api GET "/business/erp/inventory/audit?dry_run=false"
expect "GET inventory/audit?dry_run=false" 200 0
FIXED=$(jget '.data.fixed'); MIS=$(jget '.data.mismatched'); DRY=$(jget '.data.dry_run')
[ "$FIXED" = "1" ] && ok "  fixed=1（实际修正 1 行）" || bad "  fixed" "实际 $FIXED $BODY_TEXT"
[ "$MIS" = "1" ] && ok "  报告仍记录修正前的 1 处不一致" || bad "  修正前快照" "mismatched=$MIS"
[ "$DRY" = "false" ] && ok "  dry_run 回显 false" || bad "  dry_run 回显" "$DRY"
QTY=$(one "$(psql_q "SELECT qty FROM biz_inventory WHERE material_id='$M_SN'::uuid AND warehouse_id='$WH_A'::uuid")")
[ "$(dec "$QTY")" = "3" ] && ok "  修正后 qty=3（== count(IN_STOCK)）" || bad "  修正后 qty" "实际 $QTY"
api GET "/business/erp/inventory/audit?dry_run=true"
[ "$(jget '.data.mismatched')" = "0" ] && ok "  再巡检 mismatched=0" || bad "  再巡检" "$BODY_TEXT"

# ---------------------------------------------------------------- 7. 反向脏账
echo "== 7) 反向：删掉 biz_inventory 整行 → 巡检要能查出来（book=0）"
psql_q "DELETE FROM biz_inventory WHERE material_id='$M_SN'::uuid AND warehouse_id='$WH_A'::uuid" >/dev/null
api GET "/business/erp/inventory/audit?dry_run=true"
expect "GET inventory/audit?dry_run=true（反向）" 200 0
MIS=$(jget '.data.mismatched')
ITEM=$(printf '%s' "$BODY_TEXT" | jq -r --arg m "$M_SN" '.data.items[] | select(.material_id==$m) | "\(.book_qty)~\(.actual_qty)~\(.diff)~\(.fixable)"')
[ "$MIS" = "1" ] && ok "  mismatched=1（反向也报）" || bad "  反向 mismatched" "实际 $MIS $BODY_TEXT"
WANT="0.0000~3.0000~-3.0000~true"
[ "$ITEM" = "$WANT" ] && ok "  book=0 / actual=3 / diff=-3 / 可修" || bad "  反向明细" "实际 $ITEM（期望 $WANT）"
LEFT=$(one "$(psql_q "SELECT count(*) FROM biz_inventory WHERE material_id='$M_SN'::uuid")")
[ "$LEFT" = "0" ] && ok "  dry_run 没凭空建档" || bad "  dry_run 建了档" "剩 $LEFT 行"

# ---------------------------------------------------------------- 8. 反向修正
echo "== 8) 反向脏账 dry_run=false → 重新建档"
api GET "/business/erp/inventory/audit?dry_run=false"
expect "GET inventory/audit?dry_run=false（反向建档）" 200 0
[ "$(jget '.data.fixed')" = "1" ] && ok "  fixed=1（INSERT 建档）" || bad "  fixed" "$BODY_TEXT"
ROW=$(one "$(psql_q "SELECT qty::text || '~' || amount::text || '~' || avg_cost::text FROM biz_inventory WHERE material_id='$M_SN'::uuid AND warehouse_id='$WH_A'::uuid")")
[ "$(printf '%s' "$ROW" | cut -d'~' -f1 | sed -E 's/\.0+$//')" = "3" ] && ok "  重新建档 qty=3" || bad "  建档 qty" "实际 $ROW"
note "  建档后 qty~amount~avg_cost = $ROW"
api GET "/business/erp/inventory/audit?dry_run=true"
[ "$(jget '.data.mismatched')" = "0" ] && ok "  巡检归零（全库无脏账）" || bad "  巡检归零" "$BODY_TEXT"

# ---------------------------------------------------------------- 9. 入参校验
echo "== 9) 入参校验：非法 dry_run → 400 + 66006"
api GET "/business/erp/inventory/audit?dry_run=treu"
expect "GET inventory/audit?dry_run=treu → 400/66006" 400 66006 "dry_run"
for v in 1 0 yes no on off; do
    api GET "/business/erp/inventory/audit?dry_run=$v"
    [ "$HTTP" = "200" ] && ok "  dry_run=$v 被接受（兼容写法）" || bad "  dry_run=$v" "HTTP $HTTP $BODY_TEXT"
done

# ---------------------------------------------------------------- 10. 权限 403
echo "== 10) 权限：无 audit 码的账号 → 403"
api POST /system/role "$(jq -nc --arg c "$PREFIX-NOPERM-$SUFFIX" \
    '{role_code:$c,role_name:"W2C2 E2E 无权限角色",description:"w2c2 e2e",status:1,data_scope:1}')"
expect "POST 建无权限角色" 200 0 && ROLE_ID=$(jget '.data.id')
api POST /system/user "$(jq -nc --arg u "w2c2_noperm_$SUFFIX" --arg r "$ROLE_ID" \
    --arg p "$NOPERM_PW" '{username:$u,password:$p,real_name:"W2C2 E2E",status:1,role_ids:[$r]}')"
expect "POST 建无权限账号" 200 0
ADMIN_TOKEN="$TOKEN"
TOKEN=""
api POST /system/auth/login "$(jq -nc --arg u "w2c2_noperm_$SUFFIX" --arg p "$NOPERM_PW" '{username:$u,password:$p}')"
TOKEN=$(jq -r '.data.access_token // empty' "$BODY")
if [ -n "${TOKEN:-}" ]; then
    ok "无权限账号登录"
    api GET "/business/erp/inventory/audit?dry_run=true"
    [ "$HTTP" = "403" ] && ok "无 audit 码 → 403（有码才 200，见 §4）" || bad "无码打巡检" "HTTP $HTTP $BODY_TEXT"
else
    bad "无权限账号登录" "$BODY_TEXT"
fi
TOKEN="$ADMIN_TOKEN"

# ---------------------------------------------------------------- 11. 收尾
echo "== 11) 汇总"
printf '       PASS=%d  FAIL=%d\n' "$PASS" "$FAIL"
if [ "$FAIL" -gt 0 ]; then
    printf '       失败用例：\n'
    for c in "${FAILED_CASES[@]}"; do printf '         - %s\n' "$c"; done
    echo "== 结果：FAILED"
    exit 1
fi
echo "== 结果：ALL PASS"
if [ "${W2C2_KEEP:-1}" != "0" ]; then
    note "重跑前清理（可选）："
    note "  dropdb --if-exists $W2C2_DB"
fi
