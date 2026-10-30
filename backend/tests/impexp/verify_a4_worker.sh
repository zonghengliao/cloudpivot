#!/usr/bin/env bash
# A4「异步任务与 worker」验收实测脚本 —— 验收 1（10 万行端到端 + 进度单调）与验收 4（过期 410）。
#
# 前置：
#   1. 隔离构建出来的 debug 实例已启动在 127.0.0.1:${PORT:-18081}（profile=test，库 cloudpivot_test），
#      且**产物 TTL 被设成 1 分钟**（CLOUDPIVOT_IMPEXP_EXPORT_TTL_SECONDS=60）：
#        bash /root/a4-build-start.sh        # 见脚本头（内含 TTL_SECONDS=60）
#   2. 10 万行数据已灌入：su - postgres -c "psql -d cloudpivot_test -f /tmp/a4_seed_100k.sql"
#
# 用法：bash verify_a4_worker.sh
#
# 验收 2（并发认领）见 verify_a4_claim.sh；验收 3（僵尸回收）见
# `impexp_worker` 集成测试的 zombie_* 用例。

set -uo pipefail
PORT="${PORT:-18081}"
BASE="http://127.0.0.1:${PORT}/api"
DIR="$(cd "$(dirname "$0")" && pwd)"
WORK=/tmp/a4-verify
mkdir -p "$WORK"
OK=0
NG=0
ok() { OK=$((OK + 1)); echo "PASS $1"; }
bad() { NG=$((NG + 1)); echo "FAIL $1"; }

set -a
. /etc/cloudpivot/cloudpivot-test.env 2>/dev/null
set +a

# SQL 走文件执行：避免 bash → ssh → su → psql 四层引号把 JSON / 单引号吃掉
sqlq() {
  printf '%s\n' "$1" > /tmp/a4-verify/q.sql
  chmod 644 /tmp/a4-verify/q.sql
  su - postgres -c "psql -d cloudpivot_test -tA -f /tmp/a4-verify/q.sql" 2>&1
}

echo "===== 0. 登录 ====="
TK=$(curl -s -m 15 -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  --data-binary "$(printf '{"username":"admin","password":"%s"}' "${NIK_ADMIN_PASSWORD:-}")" \
  | jq -r '.data.access_token // empty')
[ -n "$TK" ] || { echo "登录失败（NIK_ADMIN_PASSWORD 未注入？）"; exit 1; }
AUTH="Authorization: Bearer $TK"
echo "ok"

echo
echo "===== 1. 建异步导出任务（10 万行，XLSX）====="
ROWCNT=$(sqlq "SELECT count(*) FROM sys_dict_item i JOIN sys_dict_type t ON t.id=i.dict_type_id WHERE t.dict_code='a4_bench' AND i.deleted_at IS NULL")
echo "库内可导出行数: $ROWCNT"
CREATE=$(curl -s -m 30 -X POST "$BASE/business/impexp/export" -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"biz_type":"system.dict_item","keyword":"a4_bench","format":"XLSX","export_name":"A4十万行验收"}')
echo "$CREATE" | jq -c '{code, data: {id: .data.id, task_no: .data.task_no, status: .data.status, progress: .data.progress, total_rows: .data.total_rows, format: .data.format}}'
ID=$(echo "$CREATE" | jq -r '.data.id // empty')
[ -n "$ID" ] || { echo "建任务失败：$CREATE"; exit 1; }

echo
echo "===== 2. 轮询进度（只打印变化点，完整序列写给单调性断言）====="
: > "$WORK/series.txt"
LAST=""
STATUS=""
DEADLINE=$((SECONDS + 900))
STARTED=$SECONDS
while [ "$SECONDS" -lt "$DEADLINE" ]; do
  R=$(curl -s -m 10 -H "$AUTH" "$BASE/business/impexp/export/$ID")
  P=$(echo "$R" | jq -r '.data.progress')
  STATUS=$(echo "$R" | jq -r '.data.status')
  E=$(echo "$R" | jq -r '.data.exported_rows')
  echo "$P" >> "$WORK/series.txt"
  if [ "$P" != "$LAST" ]; then
    printf '[t+%ss] %s%% (%s 行) %s\n' "$((SECONDS - STARTED))" "$P" "$E" "$STATUS"
    LAST="$P"
  fi
  case "$STATUS" in SUCCESS | FAILED | CANCELED | EXPIRED) break ;; esac
  sleep 0.1
done
echo "耗时: $((SECONDS - STARTED))s，采样点数: $(wc -l < "$WORK/series.txt")"
echo "终态: $STATUS"
[ "$STATUS" = "SUCCESS" ] && ok "任务终态 SUCCESS" || bad "任务终态 $STATUS"

python3 - "$WORK/series.txt" <<'PY'
import sys
values = [int(line) for line in open(sys.argv[1]) if line.strip()]
assert values, "没有采到进度"
bad_steps = [(i, values[i - 1], values[i]) for i in range(1, len(values)) if values[i] < values[i - 1]]
distinct = sorted(set(values))
print(f"采样点数={len(values)} 最小={min(values)} 最大={max(values)} 回退次数={len(bad_steps)}")
print(f"出现的不同进度值（{len(distinct)} 个）: {distinct}")
if bad_steps:
    print("回退位置:", bad_steps[:5])
    sys.exit(1)
if values[-1] != 100:
    print("终值不是 100")
    sys.exit(1)
if len(distinct) < 3:
    print("中间进度值太少（无法证明按块更新）")
    sys.exit(1)
PY
[ $? -eq 0 ] && ok "进度 0→100 单调递增（无回退，中间值≥2 个）" || bad "进度不单调"

echo
echo "===== 3. 产物下载 + sha256 校验 + 行数核对 ====="
HTTP=$(curl -s -m 180 -o "$WORK/a4_export.xlsx" -w '%{http_code}' -H "$AUTH" \
  "$BASE/business/impexp/export/$ID/download")
echo "下载 HTTP 状态: $HTTP  大小: $(stat -c '%s' "$WORK/a4_export.xlsx") 字节"
[ "$HTTP" = "200" ] && ok "产物可下载（200）" || bad "下载状态 $HTTP"

DB_SHA=$(sqlq "SELECT COALESCE(file_sha256,'') FROM sys_export_task WHERE id='$ID'")
DB_PATH=$(sqlq "SELECT COALESCE(file_path,'') FROM sys_export_task WHERE id='$ID'")
DB_SIZE=$(sqlq "SELECT COALESCE(file_size,0) FROM sys_export_task WHERE id='$ID'")
DB_TOTAL=$(sqlq "SELECT total_rows FROM sys_export_task WHERE id='$ID'")
DB_EXPORTED=$(sqlq "SELECT exported_rows FROM sys_export_task WHERE id='$ID'")
DB_PROGRESS=$(sqlq "SELECT progress FROM sys_export_task WHERE id='$ID'")
FILE_SHA=$(sha256sum "$WORK/a4_export.xlsx" | awk '{print $1}')
DISK_SHA=$(sha256sum "$DB_PATH" | awk '{print $1}')
LOCAL_SIZE=$(stat -c '%s' "$WORK/a4_export.xlsx")
echo "DB  file_sha256 = $DB_SHA"
echo "下载文件 sha256 = $FILE_SHA"
echo "落盘文件 sha256 = $DISK_SHA"
echo "DB file_size=$DB_SIZE 下载字节=$LOCAL_SIZE total_rows=$DB_TOTAL exported_rows=$DB_EXPORTED progress=$DB_PROGRESS"
echo "落盘路径: $DB_PATH"
[ "$FILE_SHA" = "$DB_SHA" ] && ok "下载文件 sha256 == DB file_sha256" || bad "sha256 不一致"
[ "$FILE_SHA" = "$DISK_SHA" ] && ok "下载文件 sha256 == 落盘产物 sha256" || bad "落盘 sha256 不一致"
[ "$DB_SIZE" = "$LOCAL_SIZE" ] && ok "字节数一致（$DB_SIZE）" || bad "字节数不一致"
[ "$DB_EXPORTED" = "$DB_TOTAL" ] && ok "exported_rows == total_rows（$DB_TOTAL）" || bad "导出行数不符"

python3 "$DIR/count_xlsx.py" "$WORK/a4_export.xlsx" > "$WORK/xlsx.json"
jq -c '{rows, header: [.header[].value], first_data: [.first_data[].value]}' "$WORK/xlsx.json"
XROWS=$(jq -r '.rows' "$WORK/xlsx.json")
[ "$XROWS" = "$((DB_TOTAL + 1))" ] && ok "xlsx 行数 = 表头 1 + 数据 $DB_TOTAL" || bad "xlsx 行数 $XROWS != $((DB_TOTAL + 1))"

echo
echo "===== 4. 过期下载 410（TTL=60s，等过期后再下）====="
EXPIRE_AT=$(sqlq "SELECT expire_at::text FROM sys_export_task WHERE id='$ID'")
echo "expire_at = $EXPIRE_AT（TTL=60 秒）"
WAIT=$(sqlq "SELECT GREATEST(CEIL(EXTRACT(EPOCH FROM (expire_at - CURRENT_TIMESTAMP))), 0)::int + 2 FROM sys_export_task WHERE id='$ID'")
echo "等待 ${WAIT}s 直到产物过期..."
sleep "$WAIT"
HTTP2=$(curl -s -m 30 -o "$WORK/gone.json" -w '%{http_code}' -H "$AUTH" \
  "$BASE/business/impexp/export/$ID/download")
echo "过期后下载 HTTP 状态: $HTTP2"
cat "$WORK/gone.json"; echo
[ "$HTTP2" = "410" ] && ok "过期下载返回 410 Gone" || bad "过期下载状态 $HTTP2"
GCODE=$(jq -r '.code' "$WORK/gone.json" 2>/dev/null)
[ "$GCODE" = "60016" ] && ok "业务码 60016（EXPORT_EXPIRED）" || bad "业务码 $GCODE"
AFTER=$(sqlq "SELECT status FROM sys_export_task WHERE id='$ID'")
echo "下载命中过期后的任务状态: $AFTER"
[ "$AFTER" = "EXPIRED" ] && ok "顺手把任务置 EXPIRED（懒过期）" || bad "状态 $AFTER"
FILE_AFTER=$(sqlq "SELECT COALESCE(file_path,'(已清空)') FROM sys_export_task WHERE id='$ID'")
echo "过期后的 file_path: $FILE_AFTER"
[ ! -f "$DB_PATH" ] && ok "过期后物理文件已删除" || bad "物理文件仍在: $DB_PATH"

echo
echo "===== 5. RUNNING 期间取消（批次边界中止）====="
CREATE2=$(curl -s -m 30 -X POST "$BASE/business/impexp/export" -H "$AUTH" -H 'Content-Type: application/json' \
  --data-binary '{"biz_type":"system.dict_item","keyword":"a4_bench","format":"CSV","export_name":"A4取消验收"}')
ID2=$(echo "$CREATE2" | jq -r '.data.id // empty')
[ -n "$ID2" ] || { echo "建任务失败：$CREATE2"; exit 1; }
# 等它跑到 RUNNING 且进度 > 0（证明是「执行中」而不是「排队中」被取消）
CANCEL_HTTP=""
for _ in $(seq 1 600); do
  R2=$(curl -s -m 10 -H "$AUTH" "$BASE/business/impexp/export/$ID2")
  S2=$(echo "$R2" | jq -r '.data.status')
  P2=$(echo "$R2" | jq -r '.data.progress')
  case "$S2" in
    SUCCESS | FAILED | CANCELED | EXPIRED) break ;;
    RUNNING)
      if [ "$P2" != "0" ]; then
        echo "取消时状态=$S2 进度=${P2}%"
        CANCEL_HTTP=$(curl -s -m 20 -o "$WORK/cancel.json" -w '%{http_code}' -X POST -H "$AUTH" \
          "$BASE/business/impexp/export/$ID2/cancel")
        break
      fi
      ;;
  esac
  sleep 0.1
done
echo "取消接口 HTTP: $CANCEL_HTTP"
cat "$WORK/cancel.json" 2>/dev/null | jq -c '{code, data}' 2>/dev/null || true
[ "$CANCEL_HTTP" = "200" ] && ok "RUNNING 任务取消接口返回 200" || bad "取消接口 $CANCEL_HTTP"
# 等 worker 在批次边界收尾
FINAL2=""
for _ in $(seq 1 300); do
  FINAL2=$(sqlq "SELECT status FROM sys_export_task WHERE id='$ID2'")
  [ "$FINAL2" = "CANCELED" ] && break
  sleep 0.1
done
echo "取消后终态: $FINAL2"
[ "$FINAL2" = "CANCELED" ] && ok "worker 在批次边界中止并置 CANCELED" || bad "取消后状态 $FINAL2"
CANCEL_MSG=$(sqlq "SELECT COALESCE(message,'') FROM sys_export_task WHERE id='$ID2'")
echo "取消原因: $CANCEL_MSG"
CANCEL_PROGRESS=$(sqlq "SELECT progress FROM sys_export_task WHERE id='$ID2'")
echo "取消时进度: ${CANCEL_PROGRESS}%（不是 100，说明真的中止了）"
[ "$CANCEL_PROGRESS" != "100" ] && ok "取消任务进度未被写成 100" || bad "取消任务进度被写成 100"
CANCEL_PATH=$(sqlq "SELECT COALESCE(file_path,'(无产物)') FROM sys_export_task WHERE id='$ID2'")
echo "取消任务的产物路径: $CANCEL_PATH"
[ "$CANCEL_PATH" = "(无产物)" ] && ok "取消的任务没有留下产物" || bad "取消任务留下了产物: $CANCEL_PATH"

echo
echo "===== 汇总 ====="
echo "PASS=$OK FAIL=$NG"
[ "$NG" -eq 0 ] || exit 1
