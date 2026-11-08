#!/usr/bin/env bash
# ============================================================================
# P3-09 · 一条命令起整场演练（可重复执行）
#
#   bash docs/evidence/20261108-p309-wf-drill/scripts/00_run.sh
#
# 做的事（每一步都可重复）：
#   1) 停掉本卡遗留实例（18091），drop/create **全新空库**（不复用别人的证据库）；
#   2) 用二进制快照起隔离实例 :18091，等 /health；
#   3) 跑 scripts/e2e.py（A 8 条 + B 3 条 + C 4 条，输出 PASS/FAIL 计数）；
#   4) 停实例 → 复核 18091 端口已释放（结果里写「18091/health=000」）。
#
# 环境：本机 PG（127.0.0.1:5432, alun/alun）；144 生产库零读写。
# ============================================================================
set -uo pipefail
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

OUT="$RAW/${OUT_NAME:-00_run.txt}"
: >"$OUT"
exec > >(tee -a "$OUT") 2>&1

say "================ P3-09 工作流全链路回归演练（$DB / :$PORT）================"
say "二进制：$BIN（$(md5sum "$BIN" | cut -d' ' -f1)）"
say "环境文件：$ENVF（$(stat -c '%a' "$ENVF")）"

stop_instance
say "--- 0) 全新空库（不复用任何人的证据库）"
su postgres -s /bin/sh -c "dropdb --if-exists --force $DB" >/dev/null 2>&1
su postgres -s /bin/sh -c "createdb -O alun $DB" || { say "❌ 建库失败"; exit 1; }
say "createdb $DB → OK"

say "--- 1) 起隔离实例（跑全量迁移）"
start_instance || { say "❌ 实例没起来"; tail -40 "$ILOG"; exit 1; }
wait_http || { say "❌ 240s 内 /health 未通"; tail -40 "$ILOG"; exit 1; }
say "health → 200（pid $(cat "$IPID")）"
grep -oE "应用 [0-9]+ 个迁移" "$ILOG" | tail -1 | sed 's/^/迁移日志：/'
psql -h 127.0.0.1 -U alun -d "$DB" -Atc \
  "select 'schema_migrations versioned=' || count(*) || ' max=' || max(version)
     from schema_migrations where kind='versioned'" 2>/dev/null

say "--- 2) 跑演练脚本"
BASE="http://127.0.0.1:$PORT" DB="$DB" RAW="$RAW" python3 "$EV/scripts/e2e.py"
RC=$?
say "e2e.py exit=$RC"

say "--- 3) 停实例 + 端口复核"
stop_instance
sleep 2
LEFT=$(ss -lntp 2>/dev/null | grep -c ":$PORT " || true)
HEALTH=$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 "http://127.0.0.1:$PORT/health" || true)
say "18091 监听行数=$LEFT  health=$HEALTH（期望 0 / 000）"
say "库 $DB 保留待复核（本卡口径：跑完停实例、留库）"
exit $RC
