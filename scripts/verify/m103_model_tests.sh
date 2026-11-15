#!/usr/bin/env bash
# 真库跑 ERP model 单测（含新增的外币核算标记用例）：克隆模板库 → 应用 V0408 → 跑 → 删库
#
# 用法：bash scripts/verify/m103_model_tests.sh [HOST]
#   HOST 默认 172.18.10.145（在 99 上跑）；在 145 上跑传 127.0.0.1。
#
# 两条硬口径（这一步的坑，写在这里免下次再踩）：
#   ① **过滤名是 `business::erp::model::`**：用例挂在 `erp/model.rs` 的
#      `#[path = "model_tests.rs"] mod tests` 下面，写 `business::erp::model_tests`
#      命中 0 条 —— “跑过了”其实是“一条没跑”。
#   ② **口令不许写 `***`**：`ensure_db()` 连不上只打印 `SKIP: ERP_DB_URL 连接失败`，
#      用例直接 return，脚本照样退 0 ⇒ 假绿。这里出现 SKIP 或 0 命中一律判红。
set -uo pipefail
export PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4
HOST="${1:-172.18.10.145}"
DB=cp_m103_models
URL="postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@$HOST:5432/$DB"
dropdb -h "$HOST" -U cloudpivot --if-exists "$DB" >/dev/null 2>&1
createdb -h "$HOST" -U cloudpivot -T cloudpivot_test "$DB" >/dev/null || { echo "create-fail"; exit 2; }
psql -h "$HOST" -U cloudpivot -d "$DB" -q -v ON_ERROR_STOP=1 \
  -f /opt/cloudpivot/backend/app/migrations/business/V0408__account_foreign_currency.up.sql >/dev/null 2>&1 \
  || { echo "V0408 apply failed"; dropdb -h "$HOST" -U cloudpivot --if-exists "$DB" >/dev/null 2>&1; exit 2; }
echo "== 演练库 $DB 就绪（模板 + V0408），开始跑 erp model_tests =="
cd /opt/cloudpivot/backend || exit 2
OUT=$(ERP_DB_URL="$URL" cargo test -p cloudpivot --lib business::erp::model:: -- --test-threads=1 2>&1)
RC=$?
echo "$OUT" | tail -30
if echo "$OUT" | grep -q "SKIP: "; then
  echo "!! 出现 SKIP（数据库没连上），本次结果不算通过"; RC=3
fi
if ! echo "$OUT" | grep -qE "^test result: ok\. [1-9][0-9]* passed"; then
  echo "!! 0 个用例命中（过滤名写错？），本次结果不算通过"; RC=4
fi
dropdb -h "$HOST" -U cloudpivot --if-exists "$DB" >/dev/null 2>&1 && echo "== 演练库已删 =="
exit $RC
