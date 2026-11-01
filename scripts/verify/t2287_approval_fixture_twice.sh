#!/usr/bin/env bash
# t_2287c331 验收脚本：**同一个干净库连跑两遍**全量 lib 套件 + 每遍后查审批实例残留。
#
# 用法：
#   ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_t2287' \
#     bash scripts/verify/t2287_approval_fixture_twice.sh
#
# 前置：目标库必须是干净库（跑过一次 `target/debug/cloudpivot` 让迁移 + bootstrap 落地）。
#
# 判定（三条全满足才算 PASS）：
#   ① 两遍都 `test result: ok. … 0 failed`；
#   ② 每遍跑完 `biz_flow_instance` 里**没有** PO_APPROVE / STOCK_IN_POST 的 PENDING 行
#      （卡面验收：这两个 biz_type 不再累积 PENDING）；
#   ③ 每遍跑完 `biz_flow_task` 为 0 行（待办是残留的先头指标）。
#
# 为什么必须两遍：单跑一遍只能暴露模块内残留，跨模块累积只在第二遍显形
# （t_06c528fc 的教训，见 references/pre-submit-traps.md §9）。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT/backend" || exit 1
export PATH=/root/.cargo/bin:$PATH

if [ -z "${ERP_DB_URL:-}" ]; then
    echo "错误：必须先 export ERP_DB_URL（指向**干净**测试库）" >&2
    exit 2
fi

LOG_DIR="${LOG_DIR:-/tmp/t2287_twice}"
mkdir -p "$LOG_DIR"

# 从 ERP_DB_URL 里解析 psql 连接参数：postgres://user:pass@host:port/db
rest="${ERP_DB_URL#postgres://}"
cred="${rest%%@*}"; hostpart="${rest#*@}"
DB_USER="${cred%%:*}"; DB_PASS="${cred#*:}"
hostport="${hostpart%%/*}"; DB_NAME="${hostpart#*/}"
DB_HOST="${hostport%%:*}"; DB_PORT="${hostport##*:}"

psql_q() {
    PGPASSWORD="$DB_PASS" psql -h "$DB_HOST" -p "$DB_PORT" -U "$DB_USER" -d "$DB_NAME" -Atc "$1" 2>&1
}

fail=0
for pass in 1 2; do
    log="$LOG_DIR/pass${pass}.log"
    echo "==> 第 ${pass} 遍：cargo test --lib -- --test-threads=1（日志 $log）"
    ERP_DB_URL="$ERP_DB_URL" cargo test --lib -j 2 -- --test-threads=1 >"$log" 2>&1
    line="$(grep -E '^test result:' "$log" | tail -1)"
    echo "    $line"
    if ! grep -qE '^test result: ok\. .*0 failed' <<<"$line"; then
        echo "    ✗ 第 ${pass} 遍没有全绿" >&2
        grep -E '^failures:$' -A20 "$log" | head -30 >&2
        fail=1
    fi

    echo "    --- biz_flow_instance（biz_type, status, count）"
    psql_q "select biz_type, status, count(*) from biz_flow_instance group by 1,2 order by 1,2" \
        | sed 's/^/    /'

    pending="$(psql_q "select count(*) from biz_flow_instance where biz_type in ('PO_APPROVE','STOCK_IN_POST') and status = 'PENDING'")"
    tasks="$(psql_q "select count(*) from biz_flow_task")"
    echo "    审批残留：PO/STOCK_IN 的 PENDING 实例 = ${pending}，biz_flow_task = ${tasks}"
    if [ "${pending:-x}" != "0" ]; then
        echo "    ✗ 第 ${pass} 遍后仍有 PO_APPROVE / STOCK_IN_POST 的 PENDING 实例：${pending}" >&2
        fail=1
    fi
    if [ "${tasks:-x}" != "0" ]; then
        echo "    ✗ 第 ${pass} 遍后 biz_flow_task 仍有 ${tasks} 行（审批夹具没清干净）" >&2
        fail=1
    fi
done

if [ "$fail" -eq 0 ]; then
    echo "==> PASS：同一库连跑两遍全量都全绿，且 PO_APPROVE / STOCK_IN_POST 不再累积 PENDING"
else
    echo "==> FAIL：见上面日志" >&2
fi
exit "$fail"
