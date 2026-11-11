#!/usr/bin/env bash
# =============================================================================
# HR-16 HTTP 冒烟：员工薪资标准 2 接口 + 生效区间不重叠（含边界相等）
# =============================================================================
#
# 为什么还要有它（真库用例已经证明了区间判定与收口）
# ------------------------------------------------
# `cargo test` 走 handler 里的**业务函数**，不经过 axum 路由、权限宏、JSON 反序列化。
# 这几件事只有起真实例才验得到：
#
#   1. 两条路由真的注册上了（`GET/POST /employee/{id}/salary` 不会被 `/employee/{id}` 抢走）；
#   2. 未登录是 401（不是「薪资无人管」）；
#   3. 权限码 `business:hr:salary:detail` / `:calc` 真能过（V0387 种子 + 超管短路）；
#   4. JSON 字段名与前端要消费的形状对得上（`written` / `closed` / `canceled` / `current`）；
#   5. 409 的**报文**点得出「哪个薪资项、哪两条区间」（前端把它直接铺在表单项下面）；
#   6. 员工详情的 `salary_items[]`（HR-12）与 #16 是同一份数据。
#
# 用法（隔离实例）：
#   BASE_URL=http://127.0.0.1:18098 ADMIN_PASSWORD='Admin@123456' \
#     PSQL_URL=postgres://alun:alun@127.0.0.1:5432/hr16_http \
#     bash scripts/verify/hr/salary_http_smoke.sh
#
# 环境变量
#   BASE_URL        后端根地址（默认 http://127.0.0.1:18098）
#   ADMIN_PASSWORD  超管密码（**没有默认值**：仓库不留默认口令）
#   PSQL_URL        实例所连的库（用于造 / 清薪资项夹具；薪资项写接口归 HR-31，本卡没有）
#   KEEP=1          结束后不清理夹具（留着排查）
#
# 注意（空库首启）：`admin` 的 `must_change_password=true`，脚本会先改密再重登，
# 新口令是 `${ADMIN_PASSWORD}-changed` —— **第二次**跑就要传新口令。
# =============================================================================
set -uo pipefail

BASE_URL=${BASE_URL:-http://127.0.0.1:18098}
ADMIN_PASSWORD=${ADMIN_PASSWORD:-}
PSQL_URL=${PSQL_URL:-}
KEEP=${KEEP:-0}
TAG=$(date +%H%M%S%N | cut -c1-12)
OUT=$(mktemp)
CODE=0
TOKEN=""
pass=0
fail=0

# 夹具薪资项（17 个种子项归 HR-31，本卡自造两个）
ITEM_A="HR16S${TAG}"
ITEM_B="HR16T${TAG}"

pass_line() { printf '  [PASS] %s\n' "$1"; pass=$((pass + 1)); }
fail_line() { printf '  [FAIL] %s\n' "$1"; fail=$((fail + 1)); }

expect_eq() { # name expect actual
  if [ "$2" = "$3" ]; then pass_line "$1（$3）"; else fail_line "$1：期望 $2，实际 $3"; fi
}

expect_contains() { # name haystack needle
  case "$2" in
    *"$3"*) pass_line "$1（含 $3）" ;;
    *) fail_line "$1：期望包含 $3，实际 ${2:0:300}" ;;
  esac
}

jqv() { jq -r "$1" "$OUT" 2>/dev/null; }

req() { # METHOD PATH [BODY]
  local method=$1 path=$2 body=${3:-}
  local args=(-s -o "$OUT" -w '%{http_code}' -X "$method" "$BASE_URL$path" -H 'Content-Type: application/json')
  [ -n "${TOKEN:-}" ] && args+=(-H "Authorization: Bearer ${TOKEN}")
  [ -n "$body" ] && args+=(-d "$body")
  CODE=$(curl "${args[@]}")
}

login() {
  local user=$1 password=$2
  req POST /api/system/auth/login "{\"username\":\"$user\",\"password\":\"$password\"}"
  local must
  must=$(jqv '.data.must_change_password // false')
  TOKEN=$(jqv '.data.access_token // empty')
  [ -n "$TOKEN" ] || return 1
  if [ "$must" = "true" ]; then
    req PUT /api/system/auth/password \
      "{\"old_password\":\"$password\",\"new_password\":\"${password}-changed\"}"
    TOKEN=""
    req POST /api/system/auth/login "{\"username\":\"$user\",\"password\":\"${password}-changed\"}"
    TOKEN=$(jqv '.data.access_token // empty')
    [ -n "$TOKEN" ] || return 1
  fi
  return 0
}

echo "== HR-16 员工薪资标准 HTTP 冒烟（$BASE_URL，tag=$TAG）=="

# ---- 0. 前置 ----
if [ -z "$ADMIN_PASSWORD" ]; then
  echo "FATAL: 必须显式给 ADMIN_PASSWORD（仓库不留默认口令）" >&2
  exit 2
fi
if [ -z "$PSQL_URL" ]; then
  echo "FATAL: 必须给 PSQL_URL（造薪资项夹具用；薪资项写接口归 HR-31）" >&2
  exit 2
fi
CODE=$(curl -s -o /dev/null -w '%{http_code}' -m 5 "$BASE_URL/health")
expect_eq "0. /health 就绪" 200 "$CODE"

# 未登录 → 401（先验，再登录）
TOKEN=""
req GET "/api/business/hr/employee/00000000-0000-0000-0000-000000000001/salary"
expect_eq "0. 未登录读薪资 → 401" 401 "$CODE"

if ! login admin "$ADMIN_PASSWORD"; then
  echo "FATAL: admin 登录失败（检查 ADMIN_PASSWORD / 实例库）" >&2
  exit 2
fi
pass_line "0. admin 登录（含必改密处理）"

# ---- 1. 夹具：部门 + 员工 + 两个薪资项 ----
req GET /api/system/dept/tree
DEPT_ID=$(jqv '.data[0].id // empty')
if [ -z "$DEPT_ID" ]; then
  echo "FATAL: 库里没有部门，无法建员工" >&2
  exit 2
fi

psql "$PSQL_URL" -q -v ON_ERROR_STOP=1 \
  -c "INSERT INTO biz_salary_item (org_id, item_code, item_name, item_type, calc_type, source, sort_order, debit_account_code, credit_account_code)
      VALUES ('00000000-0000-0000-0000-000000000000', '$ITEM_A', '冒烟基本工资', 'EARNING', 'FIXED', 'EMPLOYEE_STANDARD', 960001, '5602.10', '2211.01'),
             ('00000000-0000-0000-0000-000000000000', '$ITEM_B', '冒烟餐补', 'EARNING', 'FIXED', 'EMPLOYEE_STANDARD', 960002, '5602.10', '2211.01')" \
  || { echo "FATAL: 造薪资项夹具失败" >&2; exit 2; }
pass_line "1. 夹具薪资项 $ITEM_A / $ITEM_B（[FIXTURE] 薪资项写接口归 HR-31）"

req POST /api/business/hr/employee "{
  \"name\":\"薪资冒烟员工$TAG\",\"gender\":1,\"dept_id\":\"$DEPT_ID\",
  \"hire_date\":\"2024-03-01\"
}"
expect_eq "1. 建员工" 200 "$CODE"
EMP_ID=$(jqv '.data.id // empty')
[ -n "$EMP_ID" ] || { echo "FATAL: 建员工失败：$(cat "$OUT")" >&2; exit 2; }

# ---- 2. 空表：现行 / 历史都空 ----
req GET "/api/business/hr/employee/$EMP_ID/salary"
expect_eq "2. 读薪资标准" 200 "$CODE"
expect_eq "2. items 空" 0 "$(jqv '.data.items | length')"
expect_eq "2. current 空" 0 "$(jqv '.data.current | length')"
expect_eq "2. 回执带工号" "false" "$(jqv '.data.emp_no == null or .data.emp_no == ""')"

# ---- 3. 首次录入（现行标准：至今）----
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2026-01-01\",
  \"items\":[{\"item_code\":\"$ITEM_A\",\"amount\":5602.10,\"remark\":\"冒烟首录\"}]
}"
expect_eq "3. 首次录入" 200 "$CODE"
expect_eq "3. written 1 条" 1 "$(jqv '.data.written | length')"
expect_eq "3. closed 0 条（首次没有旧行）" 0 "$(jqv '.data.closed | length')"
expect_eq "3. 金额（字符串定点，NUMERIC(18,4) 4 位）" "5602.1000" "$(jqv '.data.written[0].amount')"
expect_eq "3. 无终止日 = 至今" "null" "$(jqv '.data.written[0].effective_to')"
expect_eq "3. 现行 1 条" 1 "$(jqv '.data.current | length')"

# ---- 4. 调薪：新行至今 + 旧行收口到 06-30 ----
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2026-07-01\",
  \"items\":[{\"item_code\":\"$ITEM_A\",\"amount\":6800}]
}"
expect_eq "4. 调薪" 200 "$CODE"
expect_eq "4. 收口 1 条旧行" 1 "$(jqv '.data.closed | length')"
expect_eq "4. 旧行止于新生效日前一天" "2026-06-30" "$(jqv '.data.closed[0].effective_to')"
expect_eq "4. 台账两个版本" 2 "$(jqv '.data.items | length')"
expect_eq "4. 现行是 6800" "6800.0000" "$(jqv '.data.current[0].amount')"

# ---- 5. 重叠被拒：落在已收口区间内部（04-01 ~ 至今 撞 01-01 ~ 06-30）----
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2026-04-01\",
  \"items\":[{\"item_code\":\"$ITEM_A\",\"amount\":7000}]
}"
expect_eq "5. 区间重叠 → 409" 409 "$CODE"
expect_eq "5. 业务码 68007" 68007 "$(jqv '.code')"
MSG=$(jqv '.msg')
expect_contains "5. 报文点名薪资项" "$MSG" "$ITEM_A"
expect_contains "5. 报文点名本次区间" "$MSG" "2026-04-01 ~ 至今"
expect_contains "5. 报文点名冲突区间" "$MSG" "2026-01-01 ~ 2026-06-30"
expect_contains "5. 报文写明「边界相等也算重叠」" "$MSG" "边界相等也算重叠"

req GET "/api/business/hr/employee/$EMP_ID/salary"
expect_eq "5. 被拒后行数不变（整笔回滚）" 2 "$(jqv '.data.items | length')"

# ---- 6. 边界相等被拒：旧止 06-30，新生效日也 06-30 ----
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2026-06-30\",
  \"items\":[{\"item_code\":\"$ITEM_A\",\"amount\":7000}]
}"
expect_eq "6. 边界相等（06-30 vs 06-30）→ 409" 409 "$CODE"
expect_eq "6. 业务码 68007" 68007 "$(jqv '.code')"

# ---- 7. 相邻放行：明确区间 [2025-01-01, 2025-06-30]（在 2026 之前，不搭界）----
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2025-01-01\",
  \"items\":[{\"item_code\":\"$ITEM_A\",\"amount\":5000,\"effective_to\":\"2025-06-30\"}]
}"
expect_eq "7. 不重叠的历史区间 → 200" 200 "$CODE"
expect_eq "7. 历史 + 现状 = 3 个版本" 3 "$(jqv '.data.items | length')"
expect_eq "7. 没动任何旧行" 0 "$(jqv '.data.closed | length')"

# ---- 8. 修正（同生效日再提交一次 = 改金额，不新增行）----
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2026-07-01\",
  \"items\":[{\"item_code\":\"$ITEM_A\",\"amount\":6900}]
}"
expect_eq "8. 同生效日修正 → 200" 200 "$CODE"
expect_eq "8. 修正后金额" "6900.0000" "$(jqv '.data.written[0].amount')"
expect_eq "8. 版本数不变（3 个）" 3 "$(jqv '.data.items | length')"

# ---- 9. 取消：自 2026-12-01 起不再有餐补（先登记再取消）----
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2026-01-01\",
  \"items\":[{\"item_code\":\"$ITEM_B\",\"amount\":800}]
}"
expect_eq "9. 登记餐补" 200 "$CODE"
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2026-12-01\",
  \"items\":[{\"item_code\":\"$ITEM_B\"}]
}"
expect_eq "9. 取消餐补" 200 "$CODE"
expect_eq "9. canceled 点名该项" "$ITEM_B" "$(jqv '.data.canceled[0]')"
expect_eq "9. 收口到 11-30" "2026-11-30" "$(jqv '.data.closed[0].effective_to')"

# 重复取消 → 68 段业务闸（本次什么都没改）
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2026-12-01\",
  \"items\":[{\"item_code\":\"$ITEM_B\"}]
}"
expect_eq "9. 重复取消 → 409" 409 "$CODE"
expect_eq "9. 业务码 68008" 68008 "$(jqv '.code')"

# ---- 10. 入参守卫 ----
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2026-12-01\",
  \"items\":[{\"item_code\":\"NO_SUCH_ITEM\",\"amount\":1}]
}"
expect_eq "10. 薪资项不存在 → 400" 400 "$CODE"
req POST "/api/business/hr/employee/$EMP_ID/salary" "{\"items\":[]}"
expect_eq "10. 缺 effective_from → 400" 400 "$CODE"
req POST "/api/business/hr/employee/$EMP_ID/salary" "{
  \"effective_from\":\"2026-12-01\",
  \"items\":[{\"item_code\":\"$ITEM_A\",\"amount\":-1}]
}"
expect_eq "10. 负数金额 → 400" 400 "$CODE"

# ---- 11. 与员工详情同源（HR-12 的 salary_items[]）----
req GET "/api/business/hr/employee/$EMP_ID"
expect_eq "11. 读员工详情" 200 "$CODE"
expect_eq "11. 详情 salary_items 与 #16 同源（4 行）" 4 "$(jqv '.data.salary_items | length')"

# ---- 12. 清理 ----
if [ "$KEEP" != "1" ]; then
  req DELETE "/api/business/hr/employee/$EMP_ID"
  [ "$CODE" = "200" ] && pass_line "12. 软删冒烟员工" || fail_line "12. 软删冒烟员工失败（$CODE）"
  psql "$PSQL_URL" -q -c "DELETE FROM biz_employee_salary WHERE employee_id = '$EMP_ID'::uuid" >/dev/null 2>&1
  psql "$PSQL_URL" -q -c "DELETE FROM biz_employee WHERE id = '$EMP_ID'::uuid" >/dev/null 2>&1
  psql "$PSQL_URL" -q -c "DELETE FROM biz_salary_item WHERE item_code IN ('$ITEM_A','$ITEM_B')" >/dev/null 2>&1
  pass_line "12. 夹具已清（薪资项 + 员工 + 标准行）"
else
  echo "  [KEEP] 保留夹具：employee=$EMP_ID items=$ITEM_A/$ITEM_B"
fi

echo "== 结果：通过 $pass 项 / 失败 $fail 项 =="
[ "$fail" -eq 0 ] || exit 1
