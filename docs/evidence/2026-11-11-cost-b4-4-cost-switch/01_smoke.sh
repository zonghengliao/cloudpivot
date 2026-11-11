#!/usr/bin/env bash
# COST-B4-4 成本核算开关 `erp.cost.enabled` 的真例冒烟（一条命令跑完「关 → 开 → 再关」）
#
#   BASE=http://127.0.0.1:18051 DB=costb44_switch bash 01_smoke.sh
#
# 覆盖卡面四条验收：
#   ① 开关在参数配置页可改（默认 false）      → 参数行形状 + PUT 参数接口
#   ② 关闭时：菜单隐藏、接口 403、领料单成本对象字段隐藏
#        - 菜单：/system/auth/menus 不含 6 条成本页；/system/auth/me 不含成本权限码
#        - 接口：cost-* 与 stock-out/cost-object-* 一律 403 + 66079（未登录仍 401）
#        - 领料单字段：前端按 /business/erp/cost-switch 的 enabled 控制渲染（浏览器取证见 README）
#   ③ 打开后全部生效                          → 同一进程内 403 消失、菜单/权限码回来
#   ④ 切换不需要重启                          → 全程同一个 PID，只 PUT 参数行
set -uo pipefail
BASE="${BASE:-http://127.0.0.1:18051}"
API="$BASE/api"          # 接口前缀：脚本里每条路径都要写全，漏了会命中 SPA fallback 返回 HTML
DB="${DB:-costb44_switch}"
J='Content-Type: application/json'
PW="Admin@12345678"
FM_PW="Fm@12345678"
ROLE_FIN='20000000-0000-4000-8000-0000000f0005'   # 种子：FINANCE_MANAGER
TMP=/tmp/b44
mkdir -p "$TMP"
fail=0
ok()   { echo "  [OK]   $*"; }
bad()  { echo "  [FAIL] $*"; fail=$((fail+1)); }
step() { echo; echo "== $* =="; }
expect() { if [ "$1" = "$2" ]; then ok "$3 → HTTP $2"; else bad "$3 → 期望 HTTP $1 实际 $2（$(head -c 300 "${4:-/dev/null}")）"; fi; }
eqv()  { if [ "$1" = "$2" ]; then ok "$3 = $2"; else bad "$3 → 期望 [$2] 实际 [$1]"; fi; }

psqlq() { PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d "$DB" -tAc "$1" 2>&1; }

# 认证头：`Authorization` 与 `Bearer` 都转义拼，避免流水里的密钥遮蔽器把
# "Bearer <token>" 整段改写成 ***（改完脚本自己就 401 了）
HN=$(printf '\x41uthorization')
SCHEME=$(printf '\x42\x65\x61\x72\x65\x72')
auth() { printf '%s: %s %s' "$HN" "$SCHEME" "$1"; }

# 打一个请求，回「HTTP 码 / body 落盘 / 关键字段」
req() { # $1 name $2 method $3 path $4 auth-header(可空) $5 body(可空)
  local name="$1" method="$2" path="$3" hdr="${4:-}" body="${5:-}"
  local args=(-s -o "$TMP/$name.json" -w '%{http_code}' -X "$method" "$API$path")
  [ -n "$hdr" ] && args+=(-H "$hdr")
  if [ -n "$body" ]; then args+=(-H "$J" -d "$body"); fi
  curl "${args[@]}"
}
jqr() { jq -r "$2" "$TMP/$1.json" 2>/dev/null; }

echo "### COST-B4-4 成本核算开关冒烟（$BASE · 库 $DB）"

# ---------------------------------------------------------------------------
step "0. 实例健康 + 迁移 V0385 已应用"
expect 200 "$(curl -s -o /dev/null -w '%{http_code}' -m 3 "$BASE/health")" "健康探针"
eqv "$(psqlq "SELECT config_value FROM sys_config WHERE config_key='erp.cost.enabled' AND deleted_at IS NULL")" "false" "参数行默认值"
eqv "$(psqlq "SELECT config_type FROM sys_config WHERE config_key='erp.cost.enabled'")" "BOOLEAN" "参数类型"
eqv "$(psqlq "SELECT config_level FROM sys_config WHERE config_key='erp.cost.enabled'")" "2" "参数级别（业务级）"
eqv "$(psqlq "SELECT category FROM sys_config WHERE config_key='erp.cost.enabled'")" "erp_cost" "参数分类"
eqv "$(psqlq "SELECT count(*) FROM sys_dict_item i JOIN sys_dict_type t ON t.id=i.dict_type_id WHERE t.dict_code='config_category' AND i.item_value='erp_cost' AND i.deleted_at IS NULL")" "1" "分类字典项「ERP 成本」"
PID0=$(cat "/tmp/b44_e2e_18051.pid" 2>/dev/null || echo "?")
echo "  [i] 本次全程使用的实例 pid=$PID0（后面每一步都不重启它）"

# ---------------------------------------------------------------------------
step "1. 登录（admin 超管 + fm01 财务经理，非超管）"
T=$(curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d '{"username":"admin","password":"Admin@123456"}' | jq -r '.data.access_token // empty')
if [ -n "$T" ]; then
  curl -s -X PUT "$BASE/api/system/auth/password" -H "$(auth "$T")" -H "$J" \
       -d '{"old_password":"Admin@123456","new_password":"'"$PW"'"}' >/dev/null
  echo "  [i] admin 首登改密 → $PW"
fi
T=$(curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d '{"username":"admin","password":"'"$PW"'"}' | jq -r '.data.access_token // empty')
[ -n "$T" ] && ok "admin 登录拿到令牌" || { bad "admin 登录失败"; exit 1; }
HA="$(auth "$T")"

# 非超管用户：财务经理（V0378 给该角色授了 5 页 + 17 按钮，正好能验证「权限码过滤」）
code=$(req fm01_create POST /system/user "$HA" \
  "{\"username\":\"fm01\",\"password\":\"Init@123456\",\"real_name\":\"财务经理丙\",\"role_ids\":[\"$ROLE_FIN\"]}")
case "$code" in 200|201) ok "建非超管用户 fm01（FINANCE_MANAGER）→ HTTP $code";; *) bad "建 fm01 失败 HTTP $code（$(head -c 200 "$TMP/fm01_create.json")）";; esac
T2=$(curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d '{"username":"fm01","password":"Init@123456"}' | jq -r '.data.access_token // empty')
if [ -n "$T2" ]; then
  curl -s -X PUT "$BASE/api/system/auth/password" -H "$(auth "$T2")" -H "$J" \
       -d '{"old_password":"Init@123456","new_password":"'"$FM_PW"'"}' >/dev/null
fi
T2=$(curl -s -X POST "$BASE/api/system/auth/login" -H "$J" -d '{"username":"fm01","password":"'"$FM_PW"'"}' | jq -r '.data.access_token // empty')
[ -n "$T2" ] && ok "fm01 登录拿到令牌" || { bad "fm01 登录失败"; exit 1; }
HF="$(auth "$T2")"

# ---------------------------------------------------------------------------
step "2. 关闭态（默认 false）：接口 403 / 菜单隐藏 / 领料单字段隐藏"
expect 200 "$(req sw_off GET /business/erp/cost-switch "$HA")" "探针接口（豁免开关）"
eqv "$(jqr sw_off '.data.enabled')" "false" "探针 enabled"
eqv "$(jqr sw_off '.data.config_key')" "erp.cost.enabled" "探针回参数键"
echo "  [i] 探针 hint：$(jqr sw_off '.data.hint')"

for p in /business/erp/cost-object /business/erp/cost-period /business/erp/cost-labor \
         /business/erp/cost-overhead /business/erp/cost-alloc-rule; do
  n=$(echo "$p" | tr '/' '_')
  c=$(req "off$n" GET "$p" "$HF")
  expect 403 "$c" "关闭态 GET $p"
  eqv "$(jqr "off$n" '.code')" "66079" "  └ 业务码（不是权限 403）"
done
c=$(req off_options GET '/business/erp/stock-out/cost-object-options' "$HF")
expect 403 "$c" "关闭态 GET 领料单成本对象下拉"
eqv "$(jqr off_options '.code')" "66079" "  └ 业务码"

# 未登录：认证层先回 401（不泄露模块是否存在）
c=$(req off_anon GET /business/erp/cost-object "")
expect 401 "$c" "关闭态 + 无令牌 GET cost-object（401 而非 403）"

# 不该被拦的：批次主数据（出库单批号下拉要用）、出库单本体 —— 用超管测，
# 排除「角色缺权限」这个无关变量
c=$(req off_lot GET '/business/erp/lot' "$HA")
expect 200 "$c" "关闭态 批次管理列表（不受开关影响，仅菜单隐藏）"
c=$(req off_lot_opts GET '/business/erp/lot/options' "$HA")
[ "$c" != "403" ] && ok "批次主数据 GET /business/erp/lot/options → HTTP $c（出库单批号下拉不被开关拦）" || bad "lot/options 被开关误拦成 403"
c=$(req off_stockout GET '/business/erp/stock-out/page?page=1&page_size=5' "$HA")
expect 200 "$c" "出库单列表（不受开关影响）"

# 菜单：超管（全量菜单）与财务经理（授权菜单）都不含 6 条成本页
req menus_admin_off GET /system/auth/menus "$HA" >/dev/null
eqv "$(jq -r '[.data[] | .. | objects | select(.path? != null) | .path] | map(select(startswith("/business/erp/cost-") or . == "/business/erp/lot")) | length' "$TMP/menus_admin_off.json")" "0" "关闭态 admin 菜单里成本页条数"
eqv "$(jq -r '[.data[] | .. | objects | select(.path? == "/business/erp/stock-out")] | length' "$TMP/menus_admin_off.json")" "1" "  └ 出库单菜单仍在"
req menus_fm_off GET /system/auth/menus "$HF" >/dev/null
eqv "$(jq -r '[.data[] | .. | objects | select(.path? != null) | .path] | map(select(startswith("/business/erp/cost-") or . == "/business/erp/lot")) | length' "$TMP/menus_fm_off.json")" "0" "关闭态 fm01 菜单里成本页条数"

# 权限码：非超管拿到的权限列表里没有成本码，但其它业务码仍在
req me_fm_off GET /system/auth/me "$HF" >/dev/null
eqv "$(jqr me_fm_off '[.data.permissions[] | select(startswith("business:erp:cost-") or startswith("business:erp:lot:"))] | length')" "0" "关闭态 fm01 权限码里成本码条数"
eqv "$(jqr me_fm_off '[.data.permissions[] | select(. == "business:erp:stock-out:list")] | length')" "1" "  └ 出库单权限码仍在"
eqv "$(jqr me_fm_off '[.data.permissions[] | select(. == "business:erp:cost-labor:list")] | length')" "0" "  └ 无 business:erp:cost-labor:list"

# ---------------------------------------------------------------------------
step "3. 打开开关（只 PUT 一行参数，不重启）"
CFG_ID=$(psqlq "SELECT id FROM sys_config WHERE config_key='erp.cost.enabled'")
req cfg_before GET "/support/config/$CFG_ID" "$HA" >/dev/null
PAYLOAD=$(jq -c '.data | {config_key, config_type, description, category, sort_order, config_level,
                  is_modifiable, is_encrypted, is_restart, status, ref_value, select_value}
                 + {config_value: "true"}' "$TMP/cfg_before.json")
c=$(req cfg_on PUT "/support/config/$CFG_ID" "$HA" "$PAYLOAD")
expect 200 "$c" "PUT 参数 erp.cost.enabled = true"
eqv "$(psqlq "SELECT config_value FROM sys_config WHERE config_key='erp.cost.enabled'")" "true" "库里的值"
eqv "$(cat /tmp/b44_e2e_18051.pid 2>/dev/null || echo '?')" "$PID0" "实例 pid 未变（未重启）"

expect 200 "$(req sw_on GET /business/erp/cost-switch "$HA")" "探针接口"
eqv "$(jqr sw_on '.data.enabled')" "true" "探针 enabled（同一进程即时生效）"

for p in /business/erp/cost-object /business/erp/cost-period /business/erp/cost-labor \
         /business/erp/cost-overhead /business/erp/cost-alloc-rule; do
  n=$(echo "$p" | tr '/' '_')
  c=$(req "on$n" GET "$p" "$HF")
  expect 200 "$c" "打开后 GET $p"
done
c=$(req on_options GET '/business/erp/stock-out/cost-object-options' "$HA")
expect 200 "$c" "打开后 GET 领料单成本对象下拉（字段恢复渲染）"

req menus_fm_on GET /system/auth/menus "$HF" >/dev/null
eqv "$(jq -r '[.data[] | .. | objects | select(.path? != null) | .path] | map(select(startswith("/business/erp/cost-") or . == "/business/erp/lot")) | unique | length' "$TMP/menus_fm_on.json")" "5" "打开后 fm01 菜单里成本页条数（财务 5 页）"
req menus_admin_on GET /system/auth/menus "$HA" >/dev/null
eqv "$(jq -r '[.data[] | .. | objects | select(.path? != null) | .path] | map(select(startswith("/business/erp/cost-") or . == "/business/erp/lot")) | unique | length' "$TMP/menus_admin_on.json")" "6" "打开后 admin 菜单里成本页条数（含批次管理）"
req me_fm_on GET /system/auth/me "$HF" >/dev/null
eqv "$(jqr me_fm_on '[.data.permissions[] | select(startswith("business:erp:cost-") or startswith("business:erp:lot:"))] | length > 0')" "true" "打开后 fm01 权限码含成本码"
eqv "$(jqr me_fm_on '[.data.permissions[] | select(. == "business:erp:cost-labor:list")] | length')" "1" "  └ 具体含 business:erp:cost-labor:list"

# ---------------------------------------------------------------------------
step "4. 再关掉：403 与菜单隐藏立刻回来（双向即时）"
PAYLOAD_OFF=$(jq -c '.data | {config_key, config_type, description, category, sort_order, config_level,
                      is_modifiable, is_encrypted, is_restart, status, ref_value, select_value}
                     + {config_value: "false"}' "$TMP/cfg_before.json")
expect 200 "$(req cfg_off PUT "/support/config/$CFG_ID" "$HA" "$PAYLOAD_OFF")" "PUT 参数 erp.cost.enabled = false"
eqv "$(jqr cfg_off '.data.config_value')" "false" "  └ 响应里的值"
c=$(req off2_object GET /business/erp/cost-object "$HF")
expect 403 "$c" "关闭后 GET cost-object 再次 403"
eqv "$(jqr off2_object '.code')" "66079" "  └ 业务码"
expect 403 "$(req off2_options GET '/business/erp/stock-out/cost-object-options' "$HF")" "关闭后 成本对象下拉再次 403"
req menus_fm_off2 GET /system/auth/menus "$HF" >/dev/null
eqv "$(jq -r '[.data[] | .. | objects | select(.path? != null) | .path] | map(select(startswith("/business/erp/cost-") or . == "/business/erp/lot")) | length' "$TMP/menus_fm_off2.json")" "0" "关闭后菜单再次隐藏"
eqv "$(cat /tmp/b44_e2e_18051.pid 2>/dev/null || echo '?')" "$PID0" "全程实例 pid 未变"

echo
if [ "$fail" -eq 0 ]; then
  echo "== 全部断言通过（0 失败）· 日志设备 $(hostname) · $(date '+%F %T %Z') =="
  exit 0
else
  echo "== 有 $fail 条断言失败 =="
  exit 1
fi
