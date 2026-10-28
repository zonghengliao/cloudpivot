#!/usr/bin/env bash
# 验收 3：本地后端 1 GB 流式下载 —— 进程 RSS 增长必须 < 200 MB。
#
# 做法：
#   1. 造 1 GiB 文件放进测试后端的上传目录，直接插一条 sys_file（storage_code=NULL，
#      即「历史文件」语义 ⇒ 走本地默认目录）；
#   2. 记录后端进程 VmRSS 基线；
#   3. 下载到 /dev/null 的同时每 0.2 s 采样 /proc/<pid>/status，取峰值；
#   4. 打印基线 / 峰值 / 增量 / 下载字节数。
#
# 用法：bash /root/a2-test-run/rss_download.sh
set -uo pipefail
BASE=http://127.0.0.1:8025/api
RUN_DIR=${RUN_DIR:-/root/a2-test-run}
set -a; . /etc/cloudpivot/cloudpivot-test.env; set +a

TOKEN=$(curl -s -X POST "$BASE/system/auth/login" -H 'Content-Type: application/json' \
  -d "{\"username\":\"admin\",\"password\":\"$ALUN_ADMIN_PASSWORD\"}" | jq -r '.data.access_token')
AUTH="Authorization: Bearer $TOKEN"
PID=$(pgrep -f 'target/debug/cloudpivot' | head -1)
[ -z "$PID" ] && { echo "找不到测试后端进程"; exit 1; }
rss_kb() { awk '/VmRSS/{print $2}' "/proc/$1/status"; }

BIG="$RUN_DIR/uploads/rss-big.bin"
mkdir -p "$RUN_DIR/uploads"
if [ ! -f "$BIG" ] || [ "$(stat -c%s "$BIG")" != "1073741824" ]; then
  echo "生成 1 GiB 测试文件 ..."
  dd if=/dev/zero of="$BIG" bs=1M count=1024 status=none
fi
BIG_SIZE=$(stat -c%s "$BIG")

# 直接插一条记录（模拟历史文件：storage_code=NULL）
FID=$(python3 -c 'import uuid;print(uuid.uuid4())')
PGPASSWORD="$ALUN_DATABASE_PASSWORD" psql -q -h 127.0.0.1 -U "$ALUN_DATABASE_USER" -d cloudpivot_test -c "
INSERT INTO sys_file (id, file_name, original_name, file_key, file_size, file_hash, mime_type, extension,
                      storage_path, category, status, created_at, updated_at)
VALUES ('$FID', 'rss-big.bin', 'rss-big.bin', 'rss-big.bin', $BIG_SIZE, NULL,
        'application/octet-stream', 'bin', '$BIG', 'attachment', 1, now(), now());
" >/dev/null

BASE_RSS=$(rss_kb "$PID")
SAMPLES="$RUN_DIR/rss-samples.txt"
: > "$SAMPLES"
( while kill -0 "$PID" 2>/dev/null; do rss_kb "$PID" >> "$SAMPLES"; sleep 0.2; done ) &
SAMPLER=$!
sleep 0.5

START=$(date +%s.%N)
BYTES=$(curl -s -o /dev/null -w '%{size_download}' "$BASE/business/storage/download/$FID" -H "$AUTH")
END=$(date +%s.%N)
sleep 0.3
kill "$SAMPLER" 2>/dev/null

PEAK_RSS=$(sort -n "$SAMPLES" | tail -1)
DELTA_MB=$(( (PEAK_RSS - BASE_RSS) / 1024 ))
ELAPSED=$(python3 -c "print(f'{$END-$START:.2f}')")

echo "== 1 GB 本地流式下载 RSS 采样（pid=$PID） =="
echo "下载字节数     : $BYTES (期望 $BIG_SIZE)"
echo "下载耗时       : ${ELAPSED}s"
echo "RSS 基线       : ${BASE_RSS} KB"
echo "RSS 峰值       : ${PEAK_RSS} KB"
echo "RSS 增量       : ${DELTA_MB} MB（阈值 < 200 MB）"
echo "采样点数       : $(wc -l < "$SAMPLES")"
echo "前 5 个采样    : $(head -5 "$SAMPLES" | tr '\n' ' ')"
echo "后 5 个采样    : $(tail -5 "$SAMPLES" | tr '\n' ' ')"

# 收尾：删记录与文件（不留在测试库里）
PGPASSWORD="$ALUN_DATABASE_PASSWORD" psql -q -h 127.0.0.1 -U "$ALUN_DATABASE_USER" -d cloudpivot_test \
  -c "DELETE FROM sys_file WHERE id='$FID';" >/dev/null
rm -f "$BIG"

[ "$BYTES" = "$BIG_SIZE" ] && [ "$DELTA_MB" -lt 200 ] && echo "RESULT: PASS（流式，内存不随文件增长）" || echo "RESULT: FAIL"
