#!/bin/bash
# CODE-03 验收：99 本机隔离实例（profile=test，库 cloudpivot_code03，端口 8043）
#             真实 token curl —— 物料 / 仓库 自动取号 + 手工覆盖 + 重复
#
# 用法：bash 01_curl.sh
#   前置：bash 00_start_instance.sh 已把实例拉起来（/health 200），库是**全新空库**
#   落证：同目录 ../raw/ 下每个请求的原始响应 + ../raw/00_summary.txt 断言汇总
#
# 为什么用全新空库：卡面写的是「首个自动号 = MAT-0001 / WH-01」。老库早就有
# MAT-xxxx / WH-xx 形态的历史编码，`next_master_code_free` 会按设计撞号顺延，
# 只有空库才能逐字验证卡面示例（这一点 CODE-02 已在 145 上吃过一次偏差）。
set -uo pipefail

BASE=${BASE:-http://127.0.0.1:8043}
ADMIN_USER=${ADMIN_USER:-admin}
ADMIN_PASS=${ADMIN_PASS:-Admin@123456}
HERE=$(cd "$(dirname "$0")" && pwd)
RAW="$HERE/../raw"
mkdir -p "$RAW"
SUM="$RAW/00_summary.txt"
: > "$SUM"

say() { echo "$@" | tee -a "$SUM"; }

FAILED=0
check() { # $1=描述 $2=期望 $3=实际
  if [ "$2" = "$3" ]; then say "  PASS  $1 = $3"; else say "  FAIL  $1 期望[$2] 实际[$3]"; FAILED=1; fi
}
check_ne() { # 两值必须不同
  if [ "$2" != "$3" ]; then say "  PASS  $1（$2 ≠ $3）"; else say "  FAIL  $1 两者相同[$2]"; FAILED=1; fi
}

# ---------- ① 登录拿真 token ----------
# 全新库的首登有 `must_change_password` 闸（403「首次登录必须先修改初始密码」），
# 只放行改密/登出接口。所以这里先把初始口令改成 ADMIN_PASS2，再用新口令登录。
# 会话状态有 60s 缓存（`session_cache::load_session_flags`），改密后最多等一轮缓存过期。
ADMIN_PASS2=${ADMIN_PASS2:-Admin@123456x}

login() { # $1=密码 → 打印 token（失败打印空）
  curl -s -X POST "$BASE/api/system/auth/login" \
    -H 'Content-Type: application/json' \
    -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$1\"}" > "$RAW/00_login.json"
  jq -r '.data.access_token // empty' "$RAW/00_login.json"
}

TOKEN=$(login "$ADMIN_PASS2")
CHANGED=0
if [ -z "$TOKEN" ]; then
  TOKEN=$(login "$ADMIN_PASS")
  if [ -z "$TOKEN" ]; then
    say "FATAL: 登录失败（初始口令与新口令都不对）"; cat "$RAW/00_login.json" | tee -a "$SUM"; exit 1
  fi
  HTTP=$(curl -s -o "$RAW/00_change_password.json" -w '%{http_code}' \
    -X PUT "$BASE/api/system/auth/password" \
    -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
    -d "{\"old_password\":\"$ADMIN_PASS\",\"new_password\":\"$ADMIN_PASS2\"}")
  say "首登改密（$ADMIN_PASS → $ADMIN_PASS2）→ HTTP $HTTP"
  CHANGED=1
  sleep 65   # 等会话状态缓存（uv:{user_id}，60s）过期
  TOKEN=$(login "$ADMIN_PASS2")
  if [ -z "$TOKEN" ]; then
    say "FATAL: 改密后仍登录不上"; cat "$RAW/00_login.json" | tee -a "$SUM"; exit 1
  fi
fi
say "login ok, access_token 长度 = ${#TOKEN}（首登改密 = $CHANGED）"
AUTH=(-H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json')

# POST：$1=文件名 $2=路径 $3=body → 打印 HTTP 码
post() {
  local name=$1 path=$2 body=$3
  curl -s -o "$RAW/$name.json" -w '%{http_code}' \
    -X POST "$BASE$path" "${AUTH[@]}" -d "$body"
}

# ---------- ② 先建一个基本计量单位（物料必须挂基本单位） ----------
say ""
say "=== 前置：基本计量单位 biz_unit ==="
HTTP=$(post 01_unit "/api/business/erp/unit" \
  '{"unit_code":"CODE03-U01","unit_name":"CODE03 个","is_base":true,"status":1}')
UNIT_ID=$(jq -r '.data.id // empty' "$RAW/01_unit.json")
say "建单位 → HTTP $HTTP, id=$UNIT_ID"
if [ -z "$UNIT_ID" ]; then say "FATAL: 单位没建出来"; exit 1; fi

# ================= ③ 物料 biz_material =================
say ""
say "=== 物料 biz_material（MAT / 4 位） ==="

HTTP=$(post 02_mat_absent "/api/business/erp/material" \
  "{\"material_name\":\"CODE03 物料（字段缺席）\",\"unit_id\":\"$UNIT_ID\"}")
MAT1=$(jq -r '.data.material_code // empty' "$RAW/02_mat_absent.json")
say "① 不传 material_code        → HTTP $HTTP, material_code=$MAT1"
check "① 字段缺席自动取号（4 位）" "MAT-0001" "$MAT1"

HTTP=$(post 03_mat_empty "/api/business/erp/material" \
  "{\"material_code\":\"\",\"material_name\":\"CODE03 物料（空串）\",\"unit_id\":\"$UNIT_ID\"}")
MAT2=$(jq -r '.data.material_code // empty' "$RAW/03_mat_empty.json")
say "② 传 material_code=\"\"      → HTTP $HTTP, material_code=$MAT2"
check "② 空串自动取号且连续 +1" "MAT-0002" "$MAT2"

HTTP=$(post 04_mat_null "/api/business/erp/material" \
  "{\"material_code\":null,\"material_name\":\"CODE03 物料（null）\",\"unit_id\":\"$UNIT_ID\"}")
MAT3=$(jq -r '.data.material_code // empty' "$RAW/04_mat_null.json")
say "③ 传 material_code=null     → HTTP $HTTP, material_code=$MAT3"
check "③ null 自动取号且连续 +1" "MAT-0003" "$MAT3"
check_ne "③ 三个自动号两两不同（首尾）" "$MAT1" "$MAT3"

HTTP=$(post 05_mat_manual "/api/business/erp/material" \
  "{\"material_code\":\"YB-001-02\",\"material_name\":\"CODE03 物料（按分类自编号）\",\"unit_id\":\"$UNIT_ID\"}")
MAT4=$(jq -r '.data.material_code // empty' "$RAW/05_mat_manual.json")
say "④ 传 material_code=\"YB-001-02\" → HTTP $HTTP, material_code=$MAT4"
check "④ 手工编码原样保留（不强制 MAT-xxx）" "YB-001-02" "$MAT4"

HTTP=$(post 06_mat_dup "/api/business/erp/material" \
  "{\"material_code\":\"YB-001-02\",\"material_name\":\"CODE03 物料（重复编码）\",\"unit_id\":\"$UNIT_ID\"}")
DUP_CODE=$(jq -r '.code // empty' "$RAW/06_mat_dup.json")
DUP_MSG=$(jq -r '.msg // empty' "$RAW/06_mat_dup.json")
say "⑤ 重复 material_code        → HTTP $HTTP, code=$DUP_CODE, msg=$DUP_MSG"
check "⑤ 重复编码 HTTP = 409" "409" "$HTTP"
check "⑤ 重复编码业务码 = 66001" "66001" "$DUP_CODE"
case "$DUP_MSG" in
  *YB-001-02*) say "  PASS  ⑤ 文案点名具体编码" ;;
  *) say "  FAIL  ⑤ 文案没点名编码：$DUP_MSG"; FAILED=1 ;;
esac

# ================= ④ 仓库 biz_warehouse =================
say ""
say "=== 仓库 biz_warehouse（WH / 2 位） ==="

HTTP=$(post 07_wh_absent "/api/business/erp/warehouse" \
  '{"warehouse_name":"CODE03 仓库（字段缺席）","level":1}')
WH1=$(jq -r '.data.warehouse_code // empty' "$RAW/07_wh_absent.json")
say "① 不传 warehouse_code       → HTTP $HTTP, warehouse_code=$WH1"
check "① 字段缺席自动取号（2 位）" "WH-01" "$WH1"

HTTP=$(post 08_wh_empty "/api/business/erp/warehouse" \
  '{"warehouse_code":"","warehouse_name":"CODE03 仓库（空串）","level":1}')
WH2=$(jq -r '.data.warehouse_code // empty' "$RAW/08_wh_empty.json")
say "② 传 warehouse_code=\"\"     → HTTP $HTTP, warehouse_code=$WH2"
check "② 空串自动取号且连续 +1" "WH-02" "$WH2"

HTTP=$(post 09_wh_null "/api/business/erp/warehouse" \
  '{"warehouse_code":null,"warehouse_name":"CODE03 仓库（null）","level":1}')
WH3=$(jq -r '.data.warehouse_code // empty' "$RAW/09_wh_null.json")
say "③ 传 warehouse_code=null    → HTTP $HTTP, warehouse_code=$WH3"
check "③ null 自动取号且连续 +1" "WH-03" "$WH3"
check_ne "③ 三个自动号两两不同（首尾）" "$WH1" "$WH3"

HTTP=$(post 10_wh_manual "/api/business/erp/warehouse" \
  '{"warehouse_code":"CK-001","warehouse_name":"CODE03 仓库（企业自有编码）","level":1}')
WH4=$(jq -r '.data.warehouse_code // empty' "$RAW/10_wh_manual.json")
say "④ 传 warehouse_code=\"CK-001\" → HTTP $HTTP, warehouse_code=$WH4"
check "④ 手工编码原样保留" "CK-001" "$WH4"

HTTP=$(post 11_wh_dup "/api/business/erp/warehouse" \
  '{"warehouse_code":"CK-001","warehouse_name":"CODE03 仓库（重复编码）","level":1}')
DUP_CODE=$(jq -r '.code // empty' "$RAW/11_wh_dup.json")
DUP_MSG=$(jq -r '.msg // empty' "$RAW/11_wh_dup.json")
say "⑤ 重复 warehouse_code       → HTTP $HTTP, code=$DUP_CODE, msg=$DUP_MSG"
check "⑤ 重复编码 HTTP = 409" "409" "$HTTP"
check "⑤ 重复编码业务码 = 66001" "66001" "$DUP_CODE"
case "$DUP_MSG" in
  *CK-001*) say "  PASS  ⑤ 文案点名具体编码" ;;
  *) say "  FAIL  ⑤ 文案没点名编码：$DUP_MSG"; FAILED=1 ;;
esac

# ================= ⑤ 格式校验（非法字符） =================
say ""
say "=== 格式校验：手工编码含空格 ==="
HTTP=$(post 12_mat_bad "/api/business/erp/material" \
  "{\"material_code\":\"BAD 001\",\"material_name\":\"CODE03 物料（非法字符）\",\"unit_id\":\"$UNIT_ID\"}")
BAD_CODE=$(jq -r '.code // empty' "$RAW/12_mat_bad.json")
BAD_MSG=$(jq -r '.msg // empty' "$RAW/12_mat_bad.json")
say "传 material_code=\"BAD 001\" → HTTP $HTTP, code=$BAD_CODE, msg=$BAD_MSG"
check "⑥ 非法字符 HTTP = 400" "400" "$HTTP"
check "⑥ 非法字符业务码 = 10001" "10001" "$BAD_CODE"

# ---------- 汇总 ----------
say ""
if [ "$FAILED" = "0" ]; then
  say "RESULT: ALL PASS"
else
  say "RESULT: 有 FAIL，见上"
fi
exit "$FAILED"
