#!/bin/bash
# SO-43 验收入口：重建夹具 → 跑真机 e2e（真实 token + 真实 HTTP）
#
# 前置：隔离实例在 127.0.0.1:18043 上跑，库 cloudpivot_so43（迁移含 V0317）。
#   起实例：bash /root/so43_start.sh（见 docs/evidence/20261107-so43-statement/README.md）
set -u
DB=cloudpivot_so43
PSQL="env PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d $DB -q -v ON_ERROR_STOP=1"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

echo "===== 1) 重放夹具（可重复执行，先删自己的行） ====="
$PSQL -f "$ROOT/scripts/verify/so43_seed.sql" || exit 1
$PSQL -Atc "select ar_no||' '||amount||' '||received_amount||' '||unsettled_amount||' '||status \
            from biz_receivable where ar_no like 'AR-SO43%' order by ar_no"

echo "===== 2) 真实 token + 真实 HTTP 断言 ====="
python3 "$ROOT/scripts/verify/so43_statement_e2e.py"
RC=$?

echo "===== 3) 台账落库核对（签发快照 vs 实时算） ====="
$PSQL -Atc "select period_from||'~'||period_to||' '||currency_code||' '||status||' '||
                   total_amount||' open='||opening_balance||' close='||closing_balance||
                   ' inv='||total_invoiced||' rcpt='||total_received||' ret='||total_returned||
                   ' items='||item_count
              from biz_statement_confirm order by period_from"
exit $RC
