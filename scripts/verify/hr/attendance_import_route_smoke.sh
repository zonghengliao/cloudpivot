#!/usr/bin/env bash
# 设计 §5.2 #23 考勤导入路由（`POST /business/hr/attendance/import`）HTTP 冒烟
# （真实例 + 真库 + 真 REST；看板卡 t_df5d6d1c）。
#
# 验的是**契约面**：设计 §5.2 #23 点的那个接口现在真的存在，而且行为与通用 impexp 三段一致。
#
# | 覆盖 | 断言 |
# |---|---|
# | 未登录 | `POST /api/business/hr/attendance/import` → 401 |
# | 无权限 | 只有销售/员工角色的账号 → 403（证明权限码真实存在且被校验） |
# | 有权限 | admin → 200 + `report.total=2 / error=0`，回执与通用 upload 同构 |
# | 委托没绕开重算 | 确认入库（impexp 的 `{id}/confirm`）后直接查期间头：`employee_count=2 / should_days_total=44.00 / actual_days_total=40.00`，**全程不调 recalc** |
# | 锁定期间 | 再导 → 确认入库 422 + 报文含「已锁定」与「Excel 第 2 行」，期间头与记录数不动 |
# | biz_type 固定 | 库直查 `sys_import_task.biz_type = 'hr.attendance'`（客户端没法覆盖） |
#
# 用法（库连接走 libpq 标准环境变量，脚本用它建夹具 / 直查证据）：
#   BASE=http://127.0.0.1:18248 ADMIN_PASSWORD='Admin@123456' \
#   DEMO_USER=demo_sales DEMO_PASSWORD='ChangeMe@2026' \
#   PGHOST=127.0.0.1 PGPORT=5432 PGUSER=alun PGPASSWORD=alun PGDATABASE=cloudpivot_test_hr34 \
#     bash scripts/verify/hr/attendance_import_route_smoke.sh
#
# `PREP_ADMIN_FLAG=1`：用 SQL 清掉 admin / 演示账号的 `must_change_password`
# （演练库用法；空库首启的账号带这个标志时，任何业务接口都会 403「首次登录必须先修改初始密码」，
#  那会把「缺权限 403」的断言污染成假绿 —— 与 HR-23 冒烟脚本同一取舍）。
#
# 退出码：0 全通过；1 有断言失败。
set -uo pipefail

BASE="${BASE:-http://127.0.0.1:18248}"
USER_NAME="${USER_NAME:-admin}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-Admin@123456}"
DEMO_USER="${DEMO_USER:-demo_sales}"
DEMO_PASSWORD="${DEMO_PASSWORD:-ChangeMe@2026}"
PREP_ADMIN_FLAG="${PREP_ADMIN_FLAG:-1}"
XLSX_PY="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/hr_e2e_xlsx.py"

PASS=0
FAIL=0
SKIP=0
TOKEN=""

pass() { PASS=$((PASS + 1)); echo "[PASS] $1"; }
fail() { FAIL=$((FAIL + 1)); echo "[FAIL] $1"; }
skip() { SKIP=$((SKIP + 1)); echo "[SKIP] $1"; }
expect_eq() { if [ "$1" = "$2" ]; then pass "$3（$1）"; else fail "$3：期望 $1，实际 $2"; fi; }
contains() { case "$1" in *"$2"*) pass "$3（含「$2」）" ;; *) fail "$3：不含「$2」，实际：$1" ;; esac; }

psql_q() { psql -At -F'|' -c "$1" 2>/dev/null; }
psql_ok=0
if command -v psql >/dev/null 2>&1 && [ "$(psql_q 'select 1')" = "1" ]; then psql_ok=1; fi
if [ "$psql_ok" != 1 ]; then
  echo "!! 需要 libpq 连接（夹具与证据直查都靠它）：PGHOST/PGPORT/PGUSER/PGPASSWORD/PGDATABASE"
  exit 1
fi

SUFFIX="$(date +%H%M%S)"
YEAR=$((2400 + $(date +%s) % 400))
CODE="$YEAR-$(printf '%02d' $(( ( $(date +%s) % 12 ) + 1 )))"
EMP_A="HR34SM-A-$SUFFIX"
EMP_B="HR34SM-B-$SUFFIX"
RUN_DIR="${RUN_DIR:-/root/hr34-smoke-$SUFFIX}"
mkdir -p "$RUN_DIR"

echo "== 考勤导入路由冒烟（设计 §5.2 #23）BASE=$BASE 期间=$CODE 运行目录=$RUN_DIR =="

# 0. 未登录 → 401（证明路由存在且挂了鉴权；404 说明路由没注册）
STATUS="$(curl -s -m 10 -o "$RUN_DIR/00_anon.json" -w '%{http_code}' -X POST \
  "$BASE/api/business/hr/attendance/import" -F "file=@/dev/null;filename=x.xlsx")"
expect_eq 401 "$STATUS" "未登录上传 → 401（不是 404）"

# 1. 清 must_change_password（夹具准备；理由见脚本头）
if [ "$PREP_ADMIN_FLAG" = "1" ]; then
  psql_q "UPDATE sys_user SET must_change_password = false WHERE username IN ('$USER_NAME','$DEMO_USER')" >/dev/null \
    && pass "夹具准备：清掉 $USER_NAME / $DEMO_USER 的 must_change_password" \
    || fail "夹具准备：清 must_change_password 失败"
fi

login() {
  curl -s -m 20 -X POST "$BASE/api/system/auth/login" -H 'Content-Type: application/json' \
    -d "{\"username\":\"$1\",\"password\":\"$2\"}" >"$RUN_DIR/login.json"
  python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("data",{}).get("access_token") or "")' \
    "$RUN_DIR/login.json" 2>/dev/null
}

TOKEN="$(login "$USER_NAME" "$ADMIN_PASSWORD")"
[ -n "$TOKEN" ] && pass "admin 登录成功" || { echo "[FAIL] admin 登录失败"; exit 1; }
DEMO_TOKEN="$(login "$DEMO_USER" "$DEMO_PASSWORD")"

# 2. 建期间（走真接口：期间是导入的前置，不是本卡要验的东西，但必须真实存在）
curl -s -m 20 -X POST "$BASE/api/business/hr/attendance/period" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d "{\"period_code\":\"$CODE\",\"period_start\":\"$YEAR-$(printf '%02d' $(( ( $(date +%s) % 12 ) + 1 )))-01\",\"period_end\":\"$YEAR-$(printf '%02d' $(( ( $(date +%s) % 12 ) + 1 )))-28\",\"remark\":\"HR34 冒烟\"}" \
  >"$RUN_DIR/01_period.json"
PID="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("data",{}).get("id") or "")' "$RUN_DIR/01_period.json")"
[ -n "$PID" ] && pass "建考勤期间 $CODE → id=$PID" || { echo "[FAIL] 建期间失败：$(cat "$RUN_DIR/01_period.json")"; exit 1; }

# 3. 两名员工（SQL 夹具：员工建档字段多且另有卡（HR-12）覆盖，本卡只验导入链路）
DEPT_ID="$(psql_q "select id from sys_dept where deleted_at is null order by sort_order, id limit 1")"
psql_q "INSERT INTO biz_employee (org_id, emp_no, name, dept_id, hire_date, status)
        VALUES ('00000000-0000-0000-0000-000000000000','$EMP_A','HR34 冒烟甲','$DEPT_ID','2024-03-01','ACTIVE'),
               ('00000000-0000-0000-0000-000000000000','$EMP_B','HR34 冒烟乙','$DEPT_ID','2024-03-01','ACTIVE')
        ON CONFLICT DO NOTHING" >/dev/null
CNT="$(psql_q "select count(*) from biz_employee where emp_no in ('$EMP_A','$EMP_B') and deleted_at is null")"
expect_eq 2 "$CNT" "夹具两名员工入库"

# 4. 造真 xlsx（2 行：22/20、22/20 → 应出勤 44.00 / 实际 40.00）
cat >"$RUN_DIR/rows.json" <<EOF
[["考勤月","工号","姓名","应出勤天数","实际出勤天数","带薪假天数","无薪假天数","病假天数","旷工天数","平时加班小时","休息日加班小时","法定节假日加班小时","迟到次数","早退次数","备注"],
 ["$CODE","$EMP_A","HR34 冒烟甲",22,20,0,0,0,0,0,0,0,0,0,"HR-34 冒烟"],
 ["$CODE","$EMP_B","HR34 冒烟乙",22,20,0,0,0,0,0,0,0,0,0,0,"HR-34 冒烟"]]
EOF
python3 "$XLSX_PY" "$RUN_DIR/attendance.xlsx" "$RUN_DIR/rows.json" >/dev/null || { echo "[FAIL] 造 xlsx 失败"; exit 1; }
pass "生成考勤 xlsx（2 行）"

jget() {
  python3 - "$1" "$2" <<'PY'
import json, sys
data = json.load(open(sys.argv[1]))
for part in sys.argv[2].split('.'):
    if part == '':
        continue
    if isinstance(data, list):
        data = data[int(part)]
    elif isinstance(data, dict):
        data = data.get(part)
    else:
        data = None
    if data is None:
        break
print('' if data is None else data)
PY
}

upload() { # <output.json> <token> <mode> <duplicate>
  curl -s -m 60 -o "$1" -w '%{http_code}' -X POST \
    "$BASE/api/business/hr/attendance/import?mode=$3&duplicate=$4" \
    -H "Authorization: Bearer $2" \
    -F "file=@$RUN_DIR/attendance.xlsx;type=application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
}

# 5. 无权限 → 403（权限码真实存在且被校验）
if [ -n "$DEMO_TOKEN" ]; then
  STATUS="$(upload "$RUN_DIR/02_forbidden.json" "$DEMO_TOKEN" STRICT UPSERT)"
  expect_eq 403 "$STATUS" "$DEMO_USER（无 HR 导入权限）上传 → 403"
  contains "$(cat "$RUN_DIR/02_forbidden.json")" "权限" "403 报文说明是权限问题"
else
  skip "无权限 403（$DEMO_USER 登录失败，演示账号口令可能不是默认值）"
fi

# 6. 有权限 → 200 + 同构回执
STATUS="$(upload "$RUN_DIR/03_upload.json" "$TOKEN" STRICT UPSERT)"
expect_eq 200 "$STATUS" "admin 走新路由上传 → 200"
expect_eq 2 "$(jget "$RUN_DIR/03_upload.json" data.report.total)" "report.total=2"
expect_eq 0 "$(jget "$RUN_DIR/03_upload.json" data.report.error)" "report.error=0"
TASK_ID="$(jget "$RUN_DIR/03_upload.json" data.task_id)"
[ -n "$TASK_ID" ] && pass "回执带 task_id=$TASK_ID" || fail "回执缺 task_id"
expect_eq "VALIDATED" "$(jget "$RUN_DIR/03_upload.json" data.status)" "任务状态 VALIDATED"

# 回执与通用 upload 同构（同一份文件、同一条内核 → 顶层字段集合必须一致）
curl -s -m 60 -o "$RUN_DIR/04_generic.json" -X POST \
  "$BASE/api/business/impexp/import/upload?biz_type=hr.attendance&mode=STRICT&duplicate=UPSERT" \
  -H "Authorization: Bearer $TOKEN" \
  -F "file=@$RUN_DIR/attendance.xlsx;type=application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" >/dev/null
GENERIC_TASK="$(jget "$RUN_DIR/04_generic.json" data.task_id)"
NEW_KEYS="$(python3 -c 'import json,sys;print(",".join(sorted(json.load(open(sys.argv[1]))["data"].keys())))' "$RUN_DIR/03_upload.json")"
OLD_KEYS="$(python3 -c 'import json,sys;print(",".join(sorted(json.load(open(sys.argv[1]))["data"].keys())))' "$RUN_DIR/04_generic.json")"
expect_eq "$OLD_KEYS" "$NEW_KEYS" "新路由回执字段与通用 upload 同构"
[ -n "$GENERIC_TASK" ] && pass "通用 upload 仍可用（对照任务 $GENERIC_TASK）" || fail "通用 upload 失败"

# 7. biz_type 固定
expect_eq "hr.attendance" "$(psql_q "select biz_type from sys_import_task where id = '$TASK_ID'")" \
  "任务行 biz_type=hr.attendance（客户端覆盖不了）"

# 8. 确认入库（impexp 通用接口）→ 期间头重算，全程不调 recalc
curl -s -m 60 -o "$RUN_DIR/05_confirm.json" -w '%{http_code}' -X POST \
  "$BASE/api/business/impexp/import/$TASK_ID/confirm" -H "Authorization: Bearer $TOKEN" \
  >"$RUN_DIR/05_confirm.status"
expect_eq 200 "$(cat "$RUN_DIR/05_confirm.status")" "确认入库 → 200"
expect_eq 2 "$(jget "$RUN_DIR/05_confirm.json" data.imported_rows)" "入库 2 行"
expect_eq "SUCCESS" "$(jget "$RUN_DIR/05_confirm.json" data.status)" "任务终态 SUCCESS"

# 期间分页（页面看到的那条数据）
curl -s -m 30 -o "$RUN_DIR/06_period_page.json" \
  "$BASE/api/business/hr/attendance/period/page?page=1&page_size=50&period_code=$CODE" \
  -H "Authorization: Bearer $TOKEN"
python3 - "$RUN_DIR/06_period_page.json" "$CODE" <<'PY' >"$RUN_DIR/06_period_row.txt"
import json, sys
rows = json.load(open(sys.argv[1])).get("data", {}).get("list") or []
row = next((r for r in rows if r.get("period_code") == sys.argv[2]), {})
print(f'{row.get("employee_count")}|{row.get("should_days_total")}|{row.get("actual_days_total")}')
PY
expect_eq "2|44.00|40.00" "$(cat "$RUN_DIR/06_period_row.txt")" \
  "期间分页：employee_count=2 / should_days_total=44.00 / actual_days_total=40.00（无 recalc）"
expect_eq 2 "$(psql_q "select count(*) from biz_attendance_record where period_id = '$PID'")" "台账 2 行"

# 9. 锁定期间 → 再导被拦（422 + Excel 行号），期间头不动
curl -s -m 20 -o /dev/null -X POST "$BASE/api/business/hr/attendance/period/$PID/status" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"to_status":"SUBMITTED"}'
STATUS="$(curl -s -m 20 -o /dev/null -w '%{http_code}' -X POST "$BASE/api/business/hr/attendance/period/$PID/status" \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' -d '{"to_status":"LOCKED"}')"
expect_eq 200 "$STATUS" "锁定期间 → 200"

STATUS="$(upload "$RUN_DIR/07_locked_upload.json" "$TOKEN" STRICT UPSERT)"
expect_eq 200 "$STATUS" "锁定期间上传仍 200 + 报告（锁定是入库期的事）"
LOCKED_TASK="$(jget "$RUN_DIR/07_locked_upload.json" data.task_id)"
STATUS="$(curl -s -m 60 -o "$RUN_DIR/08_locked_confirm.json" -w '%{http_code}' -X POST \
  "$BASE/api/business/impexp/import/$LOCKED_TASK/confirm" -H "Authorization: Bearer $TOKEN")"
expect_eq 422 "$STATUS" "锁定期间确认入库 → 422"
contains "$(cat "$RUN_DIR/08_locked_confirm.json")" "已锁定" "422 报文说明已锁定"
contains "$(cat "$RUN_DIR/08_locked_confirm.json")" "Excel 第 2 行" "422 报文给出 Excel 行号"
expect_eq "2|44.00|40.00" "$(psql_q "select employee_count||'|'||to_char(should_days_total,'FM999990.00')||'|'||to_char(actual_days_total,'FM999990.00') from biz_attendance_period where id = '$PID'")" \
  "锁定后期间头一个字节没动"
expect_eq 2 "$(psql_q "select count(*) from biz_attendance_record where period_id = '$PID'")" "锁定后台账仍是 2 行"

# 10. 清场（只删本次夹具，按 id / 工号精确匹配）
psql_q "delete from biz_attendance_record where period_id = '$PID'" >/dev/null
psql_q "delete from biz_status_log where entity_type='ATTENDANCE_PERIOD' and entity_id = '$PID'" >/dev/null
psql_q "delete from biz_attendance_period where id = '$PID'" >/dev/null
psql_q "delete from biz_employee where emp_no in ('$EMP_A','$EMP_B')" >/dev/null
for T in "$TASK_ID" "$LOCKED_TASK" "$GENERIC_TASK"; do
  [ -n "$T" ] || continue
  psql_q "delete from sys_import_task_error where task_id = '$T'" >/dev/null
  psql_q "delete from sys_import_task where id = '$T'" >/dev/null
done
pass "夹具已清理（期间 / 记录 / 员工 / 导入任务）"

echo "== 汇总：PASS=$PASS FAIL=$FAIL SKIP=$SKIP =="
[ "$FAIL" = 0 ] || exit 1
exit 0
