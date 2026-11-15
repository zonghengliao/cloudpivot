#!/usr/bin/env bash
# 真库跑 M1-04 的外币用例（克隆模板库 → 补 V0406/V0407/V0408/V0409 → 跑 → 删库）
#
# 用法：bash scripts/verify/m104_model_tests.sh [HOST]
#   HOST 默认 172.18.10.145（在 99 上跑）；在 145 上跑传 127.0.0.1。
#
# 三条硬口径（这一步的坑，写在这里免下次再踩）：
#   ① **过滤名要写全路径** `business::erp::voucher::voucher_write::tests::m104`
#      —— 用例挂在 `voucher_write.rs` 的 `mod tests` 里，漏一段就命中 0 条；
#      「跑过了」其实是「一条没跑」。
#   ② **口令不许写 `***`**：连不上时用例只打印 `SKIP: ERP_DB_URL 连接失败` 然后 return，
#      脚本照样退 0 ⇒ 假绿。出现 SKIP 或 0 命中一律判红。
#   ③ **模板库停版在 V0404**：V0406/V0409 得自己补，否则外币列不存在（用例全红但不是代码问题）。
set -uo pipefail
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
HOST="${1:-172.18.10.145}"
DB=cp_m104_models
URL="postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@$HOST:5432/$DB"
MIG=/opt/cloudpivot/backend/app/migrations/business
dropdb -h "$HOST" -U cloudpivot --if-exists "$DB" >/dev/null 2>&1
createdb -h "$HOST" -U cloudpivot -T cloudpivot_test "$DB" >/dev/null || { echo "create-fail"; exit 2; }
for f in V0406__account_voucher_currency V0407__exchange_rate_effective_range \
         V0408__account_foreign_currency V0409__voucher_entry_foreign_amounts; do
    psql -h "$HOST" -U cloudpivot -d "$DB" -q -v ON_ERROR_STOP=1 -1 -f "$MIG/$f.up.sql" >/dev/null 2>&1 \
      || { echo "$f apply failed"; dropdb -h "$HOST" -U cloudpivot --if-exists "$DB" >/dev/null 2>&1; exit 2; }
done
echo "== 演练库 $DB 就绪（模板 + V0406..V0409），开始跑 M1-04 外币用例 =="
cd /opt/cloudpivot/backend || exit 2
OUT=$(ERP_DB_URL="$URL" cargo test -p cloudpivot --lib \
        business::erp::voucher::voucher_write::tests::m104 -- --test-threads=1 2>&1)
RC=$?
echo "$OUT" | tail -40
if echo "$OUT" | grep -q "SKIP: "; then
  echo "!! 出现 SKIP（数据库没连上），本次结果不算通过"; RC=3
fi
if ! echo "$OUT" | grep -qE "^test result: ok\. [1-9][0-9]* passed"; then
  echo "!! 0 个用例命中（过滤名写错？），本次结果不算通过"; RC=4
fi
dropdb -h "$HOST" -U cloudpivot --if-exists "$DB" >/dev/null 2>&1 && echo "== 演练库已删 =="
exit $RC
