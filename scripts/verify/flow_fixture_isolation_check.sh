#!/usr/bin/env bash
# 状态机边配置审批（flow::edge_admin_tests）的**夹具隔离**回归：卡 t_977c5189
#
# 复现的真实场景：全量真库跑法里，`payable_bridge`（P17）模块先跑、`flow::edge_admin_tests`
# 后跑，两者在**同一个库、同一个进程**里。P17 的夹具账号若在模块跑完后仍「在职 + 挂着
# SUPER_ADMIN」，`FALLBACK_ADMIN` 的会签人数就从 1 变 3 ⇒ 批量审批用例只批掉 1/3，
# 实例停在 PENDING、落边 0 行（3 条假红）。
#
# 本脚本把这条链路钉死，判据四条：
#   ① 两个模块同进程串跑必须全绿（`--test-threads=1`）；
#   ② P17 模块跑完后**不许**留下在职的 `p17_*` 账号（teardown 生效）；
#   ③ `FLOW_CONFIG_APPROVER`（复核岗）不许留绑定（本模块夹具已还原）；
#   ④ 不许留 `FLOW_EDGE_CFG` 的实例 / 配置行 / 审计行。
#
# 用法（库必须是隔离库：工具会往库里写夹具并清场）：
#   ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_iso977' \
#     bash scripts/verify/flow_fixture_isolation_check.sh
#
# 没有现成隔离库时先建：`bash scripts/verify/erp_clean_db.sh`
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
URL="${ERP_DB_URL:-${SYS_DB_URL:-}}"
if [ -z "$URL" ]; then
    echo "✗ 需要 ERP_DB_URL（指向隔离库）。先 bash scripts/verify/erp_clean_db.sh" >&2
    exit 2
fi

# 库名白名单：本工具会写/删夹具，绝不碰生产库
DB_NAME="$(printf '%s' "$URL" | sed -E 's#.*/([^/?]+)(\?.*)?$#\1#')"
case "$DB_NAME" in
    cloudpivot|postgres|template0|template1|*prod*)
        echo "✗ 拒绝在库 \`$DB_NAME\` 上跑（这不是隔离库）" >&2
        exit 2
        ;;
    cloudpivot_*) ;;
    *)
        echo "✗ 库名 \`$DB_NAME\` 不在白名单（必须 cloudpivot_*）" >&2
        exit 2
        ;;
esac

echo "==> 库：$DB_NAME"
export ERP_DB_URL="$URL"
export SYS_DB_URL="${SYS_DB_URL:-$URL}"
export TASK_DB_URL="${TASK_DB_URL:-$URL}"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0

PSQL=(psql "$URL" -tAc)
q() { "${PSQL[@]}" "$1"; }

LOG="${TMPDIR:-/tmp}/t977_flow_isolation.$$.log"
echo "==> ① 同进程串跑：payable_bridge（P17）→ flow::edge_admin_tests"
( cd "$ROOT/backend" && cargo test --lib --features test-hooks -- \
      --test-threads=1 business::erp::payable_bridge::tests business::flow::edge_admin_tests ) \
    > "$LOG" 2>&1
rc=$?
grep -E '^test result:' "$LOG" | sed 's/^/    /'
if [ "$rc" -ne 0 ]; then
    echo "✗ 用例有失败（退出码 $rc）：" >&2
    grep -E '^test .* FAILED' "$LOG" | sed 's/^/    /' >&2
    echo "  完整日志：$LOG" >&2
    exit 1
fi

failed=0
check() { # $1=说明 $2=实际值 $3=期望值
    if [ "$2" != "$3" ]; then
        echo "✗ $1：实际 \`$2\`，期望 \`$3\`" >&2
        failed=$((failed + 1))
    else
        echo "    ✓ $1 = $3"
    fi
}

echo "==> ② P17 夹具账号是否残留（在职 = deleted_at IS NULL）"
check "在职 p17_* 账号数" \
      "$(q "select count(*) from sys_user where username like 'p17%' and deleted_at is null")" "0"

echo "==> ③ 复核岗 FLOW_CONFIG_APPROVER 的绑定是否还原"
check "复核岗在职成员数（本模块夹具应已解绑）" \
      "$(q "select count(*) from sys_user u join sys_user_role ur on ur.user_id=u.id \
              join sys_role r on r.id=ur.role_id \
             where r.role_code='FLOW_CONFIG_APPROVER' and u.deleted_at is null")" "0"

echo "==> ④ FLOW_EDGE_CFG 夹具残留"
check "实例数" "$(q "select count(*) from biz_flow_instance where biz_type='FLOW_EDGE_CFG'")" "0"
check "存活配置行数" "$(q "select count(*) from biz_flow_state_edge where deleted_at is null")" "0"
check "审计行数" "$(q "select count(*) from biz_status_log where entity_type='FLOW_EDGE_CFG'")" "0"

echo "==> ⑤ 已发布流程图仍是 V0206 基线"
check "graph_hash" \
      "$(q "select v.graph_hash from biz_flow_def d join biz_flow_def_version v on v.id=d.current_version_id \
             where d.flow_code='flow_edge_config' and d.deleted_at is null")" \
      "c0ad61937b734114f6865f9347b57ab9824f560605e522a204d22650baa30e08"

rm -f "$LOG"
if [ "$failed" -ne 0 ]; then
    echo "✗ 夹具隔离回归未通过（$failed 项）" >&2
    exit 1
fi
echo "✓ 夹具隔离回归通过：两个模块同进程全绿，且库侧 0 残留"
