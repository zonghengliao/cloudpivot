#!/usr/bin/env bash
# t_7f47221c 验收脚本：**同一个干净库连跑两遍**全量 lib 套件 + 每遍后查 `biz_status_log` 残留。
#
# 用法：
#   ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_t7f4' \
#     bash scripts/verify/t7f4_status_log_twice.sh
#
# 前置：目标库必须是干净库（跑过一次 `target/debug/cloudpivot` 让迁移 + bootstrap 落地）。
#
# 判定（四条全满足才算 PASS）：
#   ① 两遍都 `test result: ok. … 0 failed`；
#   ② 每遍跑完 `PAYABLE` / `VOUCHER` 的**孤儿行**（`entity_id` 指向已不存在的单据）= 0；
#   ③ 第二遍跑完 `select entity_type, count(*) … group by 1` 与**第一遍结束时逐行相等**
#      （= 每轮不再新增；这是卡面「同一库两遍 79 → 210」的直接反面）；
#   ④ 整库 `biz_status_log` 的孤儿行（六类实体合计）也为 0。
#
# 为什么必须两遍：单跑一遍只能暴露模块内残留，跨模块累积只在第二遍显形
# （t_06c528fc / t_2287c331 的教训）。
#
# 判据为什么不按号段前缀：`AP-YYYYMMDD-*` / `PZ-YYYYMMDD-*` 是**生产号段**，真实业务
# 入口（收货过账 / 付款 / 红冲）造出来的单就是这些号；按前缀清会误删真数据（技能 §6.40）。
# 孤儿行按定义就是垃圾，所以判据只能是「引用已失效」。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT/backend" || exit 1
export PATH=/root/.cargo/bin:$PATH

if [ -z "${ERP_DB_URL:-}" ]; then
    echo "错误：必须先 export ERP_DB_URL（指向**干净**测试库）" >&2
    exit 2
fi

LOG_DIR="${LOG_DIR:-/tmp/t7f4_twice}"
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

# 按 entity_type 计数（`-At` 的默认分隔符就是 `|`，稳定排序）
status_counts() {
    psql_q "select entity_type, count(*) from biz_status_log group by entity_type order by entity_type"
}

# 孤儿行总数：六类实体里「entity_id 指向已不存在单据」的行
orphans_all() {
    psql_q "
      select count(*) from biz_status_log s where
        (s.entity_type = 'STOCK_IN'           and s.entity_id not in (select id from biz_stock_in))
     or (s.entity_type = 'PURCHASE_ORDER'     and s.entity_id not in (select id from biz_purchase_order))
     or (s.entity_type = 'TRANSFER'           and s.entity_id not in (select id from biz_stock_transfer))
     or (s.entity_type = 'PAYABLE'            and s.entity_id not in (select id from biz_payable))
     or (s.entity_type = 'VOUCHER'            and s.entity_id not in (select id from biz_voucher))
     or (s.entity_type = 'ACCOUNTING_PERIOD'  and s.entity_id not in (select id from biz_accounting_period))"
}

finance_orphans() {
    psql_q "
      select
        (select count(*) from biz_status_log s
          where s.entity_type = 'PAYABLE' and s.entity_id not in (select id from biz_payable))
      + (select count(*) from biz_status_log s
          where s.entity_type = 'VOUCHER' and s.entity_id not in (select id from biz_voucher))"
}

fail=0
prev_counts=""
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

    counts="$(status_counts)"
    if grep -qi 'ERROR' <<<"$counts"; then
        echo "    ✗ 按类型计数失败（psql 原文）：$counts" >&2
        fail=1
    fi
    echo "    --- biz_status_log（entity_type|count）"
    sed 's/^/    /' <<<"$counts"
    fin="$(finance_orphans)"
    all="$(orphans_all)"
    echo "    PAYABLE/VOUCHER 孤儿 = ${fin}，全库孤儿（六类）= ${all}"
    if [ "${fin:-x}" != "0" ]; then
        echo "    ✗ 第 ${pass} 遍后仍有 PAYABLE / VOUCHER 孤儿状态日志：${fin}" >&2
        fail=1
    fi
    if [ "${all:-x}" != "0" ]; then
        echo "    ✗ 第 ${pass} 遍后全库仍有孤儿状态日志：${all}" >&2
        fail=1
    fi

    if [ "$pass" -eq 1 ]; then
        prev_counts="$counts"
    else
        if [ "$counts" != "$prev_counts" ]; then
            echo "    ✗ 第二遍的按类型计数与第一遍结束时不一致（每轮仍在增长）" >&2
            echo "      第一遍：$(tr '\n' ' ' <<<"$prev_counts")" >&2
            echo "      第二遍：$(tr '\n' ' ' <<<"$counts")" >&2
            fail=1
        else
            echo "    ✓ 两遍结束时按类型计数逐行相等（不再增长）"
        fi
    fi
done

if [ "$fail" -eq 0 ]; then
    echo "==> PASS：同一库连跑两遍全量都全绿；PAYABLE/VOUCHER 孤儿 = 0；按类型计数不再增长"
else
    echo "==> FAIL：见上面日志" >&2
fi
exit "$fail"
