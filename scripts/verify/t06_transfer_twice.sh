#!/usr/bin/env bash
# t_06c528fc 验收脚本：**同一个库连跑两遍**全量 lib 套件，两遍都必须全绿。
#
# 用法：
#   ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_t06b' \
#     bash scripts/verify/t06_transfer_twice.sh
#
# 前置：目标库必须是干净库（跑过一次 `target/debug/cloudpivot` 让迁移落地）。
# 判定：两遍都出现 `test result: ok. … 0 failed` 才算过；任一遍有 failed 就非 0 退出。
#
# 为什么这条口径才是本卡的真口径：卡面现象是「同一个库第二次进入 transfer 用例就红」，
# 单跑一遍永远测不出来。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT/backend" || exit 1
export PATH=/root/.cargo/bin:$PATH

if [ -z "${ERP_DB_URL:-}" ]; then
    echo "错误：必须先 export ERP_DB_URL（指向**干净**测试库）" >&2
    exit 2
fi

LOG_DIR="${LOG_DIR:-/tmp/t06_transfer_twice}"
mkdir -p "$LOG_DIR"

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
done

if [ "$fail" -eq 0 ]; then
    echo "==> PASS：同一个库连跑两遍全量都全绿"
else
    echo "==> FAIL：见上面日志" >&2
fi
exit "$fail"
