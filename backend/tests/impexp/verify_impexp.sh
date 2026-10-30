#!/usr/bin/env bash
# A3「导入导出引擎」验收实测脚本（四条硬指标）
#
# 前置：
#   1. 测试实例已启动在 127.0.0.1:${PORT:-18081}（profile=test，库 cloudpivot_test）：
#        set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a
#        NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18081 ./target/debug/cloudpivot &
#   2. 已注入 NIK_ADMIN_PASSWORD（登录用）；直接查库时需要 NIK_DATABASE_*。
#
# 用法：
#   bash verify_impexp.sh              # 验收 1/2 + 确认入库 + 同步导出
#   bash verify_impexp.sh perf         # 追加验收 4（10 万行耗时 / 峰值 RSS）
#
# 验收 3（外键批量校验 SQL 次数 = 外键列数）的证据在两处：
#   - 本脚本输出的 report.fk_sql_statements（应用侧计数器）
#   - 单元测试 business::impexp::validate::tests::fk_validation_uses_one_statement_per_column
#   - 需要 PG 日志口径时：ALTER DATABASE cloudpivot_test SET log_statement='all'; 重启实例后
#     跑一次上传，`grep -c jsonb_array_elements_text <pg 日志>` 应为外键列数（验证后 RESET）。

set -uo pipefail
PORT="${PORT:-18081}"
BASE="http://127.0.0.1:${PORT}/api"
DIR="$(cd "$(dirname "$0")" && pwd)"
WORK=/tmp/impexp-verify
mkdir -p "$WORK"
PASS=0
FAIL=0
ok() { PASS=$((PASS + 1)); echo "PASS $1"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL $1"; }

set -a; . /etc/cloudpivot/cloudpivot-test.env 2>/dev/null; set +a

echo "===== 0. 登录 ====="
TOKEN=$(curl -s -m 15 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  --data-binary @<(printf '{"username":"admin","password":"%s"}' "${NIK_ADMIN_PASSWORD:-}") \
  | jq -r '.data.access_token // empty')
[ -n "$TOKEN" ] || { echo "登录失败（NIK_ADMIN_PASSWORD 未注入？）"; exit 1; }
AUTH="Authorization: Bearer $TOKEN"
echo "ok"

echo
echo "===== 1. 模板三要素（验收 1） ====="
curl -s -m 30 -H "$AUTH" "$BASE/business/impexp/import/template?biz_type=system.dict_item" -o "$WORK/template.xlsx"
python3 "$DIR/dump_xlsx.py" "$WORK/template.xlsx" 6 > "$WORK/template.txt"
grep -q "工作表: \['数据', '字段说明', '填写须知'\]" "$WORK/template.txt" && ok "三个 sheet 齐（数据/字段说明/填写须知）" || bad "sheet 不全"
grep -q "A1='字典编码\*'" "$WORK/template.txt" && ok "表头首列 + 必填星标" || bad "表头异常"
grep -q "A2='user_status'" "$WORK/template.txt" && ok "第 2 行示例" || bad "示例行缺失"
grep -q "A2='字典编码\*' | B2='是'" "$WORK/template.txt" && ok "字段说明（必填/类型/长度）" || bad "字段说明缺失"
head -12 "$WORK/template.txt"

echo
echo "===== 2. 5000 行预检（验收 2） ====="
python3 "$DIR/gen_xlsx.py" bad "$WORK/bad_5000.xlsx" 5000
curl -s -m 300 -H "$AUTH" -F "file=@$WORK/bad_5000.xlsx" \
  "$BASE/business/impexp/import/upload?biz_type=system.dict_item&mode=STRICT" -o "$WORK/precheck.json"
REPORT=$(jq -c '.data.report | {total, valid, error, fk_sql_statements, elapsed_ms}' "$WORK/precheck.json")
echo "报告: $REPORT"
[ "$(jq -r '.data.report.total' "$WORK/precheck.json")" = "5000" ] && ok "total=5000" || bad "total 不符"
[ "$(jq -r '.data.report.valid' "$WORK/precheck.json")" = "4995" ] && ok "valid=4995" || bad "valid 不符"
[ "$(jq -r '.data.report.error' "$WORK/precheck.json")" = "5" ] && ok "error=5" || bad "error 不符"
[ "$(jq -r '.data.report.fk_sql_statements' "$WORK/precheck.json")" = "1" ] && ok "外键 SQL 次数=1（外键列数 1）" || bad "外键 SQL 次数不符"
jq -c '.data.report.errors[] | {row_no, column_header, error_code: .kind, message}' "$WORK/precheck.json"
TASK_ID=$(jq -r '.data.task_id' "$WORK/precheck.json")

echo
echo "===== 3. 错误明细落库 + 错误行下载 ====="
curl -s -m 20 -H "$AUTH" "$BASE/business/impexp/import/$TASK_ID/errors?page=1&page_size=10" \
  | jq -c '{total: .data.total, first: .data.list[0]}'
[ "$(curl -s -m 20 -H "$AUTH" "$BASE/business/impexp/import/$TASK_ID/errors?page=1&page_size=10" | jq -r '.data.total')" = "5" ] \
  && ok "错误明细 5 条已落库" || bad "错误明细条数不符"
curl -s -m 30 -H "$AUTH" "$BASE/business/impexp/import/$TASK_ID/errors/file" -o "$WORK/errors.xlsx" \
  && python3 "$DIR/dump_xlsx.py" "$WORK/errors.xlsx" 3 | head -5 && ok "错误行 xlsx 可下载" || bad "错误行下载失败"

echo
echo "===== 4. STRICT 模式拒绝入库（有失败行） ====="
CODE=$(curl -s -m 60 -X POST -H "$AUTH" "$BASE/business/impexp/import/$TASK_ID/confirm" | jq -r '.code')
[ "$CODE" = "60014" ] && ok "有失败行时拒绝入库（60014）" || bad "STRICT 拒绝码不符: $CODE"

echo
echo "===== 5. 干净文件端到端入库 + 幂等重导 ====="
python3 "$DIR/gen_xlsx.py" perf "$WORK/clean_100.xlsx" 100 > /dev/null
import_once() {
  local up task
  up=$(curl -s -m 60 -H "$AUTH" -F "file=@$WORK/clean_100.xlsx" \
    "$BASE/business/impexp/import/upload?biz_type=system.dict_item&mode=STRICT")
  task=$(echo "$up" | jq -r '.data.task_id')
  curl -s -m 120 -X POST -H "$AUTH" "$BASE/business/impexp/import/$task/confirm" | jq -r '.data.status'
}
S1=$(import_once); S2=$(import_once)
[ "$S1" = "SUCCESS" ] && ok "首次导入 SUCCESS" || bad "导入失败: $S1"
[ "$S2" = "SUCCESS" ] && ok "重导 SUCCESS（ON CONFLICT DO UPDATE 幂等）" || bad "重导失败: $S2"

echo
echo "===== 6. 同步导出 ====="
curl -s -m 60 -H "$AUTH" "$BASE/business/impexp/export/sync?biz_type=system.dict_item&keyword=user_status" \
  -o "$WORK/export.xlsx"
python3 "$DIR/dump_xlsx.py" "$WORK/export.xlsx" 2 | tail -3
grep -q "G2='启用'" "$WORK/export.xlsx" 2>/dev/null || python3 "$DIR/dump_xlsx.py" "$WORK/export.xlsx" 2 | grep -q "G2='启用'" \
  && ok "导出把状态码翻译成中文（G2=启用）" || bad "导出翻译异常"

if [ "${1:-}" = "perf" ]; then
  echo
  echo "===== 7. 10 万行预检（验收 4） ====="
  python3 "$DIR/gen_xlsx.py" perf "$WORK/perf_100k.xlsx" 100000
  PID=$(pgrep -f "cloudpivot" | head -1)
  curl -s -m 900 -o "$WORK/perf.json" -w "HTTP %{http_code} 墙钟 %{time_total}s\n" -H "$AUTH" \
    -F "file=@$WORK/perf_100k.xlsx" "$BASE/business/impexp/import/upload?biz_type=system.dict_item&mode=STRICT"
  jq -c '.data.report | {total, valid, error, fk_sql_statements, elapsed_ms}' "$WORK/perf.json"
  [ -n "${PID:-}" ] && grep -E "VmHWM" /proc/"$PID"/status
fi

echo
echo "===== 汇总：PASS=$PASS FAIL=$FAIL ====="
[ "$FAIL" -eq 0 ]
