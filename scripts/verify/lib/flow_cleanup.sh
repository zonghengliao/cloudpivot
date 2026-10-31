#!/usr/bin/env bash
# ============================================================
# 调拨/库存类 e2e 的「用完即删」收尾公共函数（被 w3b / w3c / p5 复用）
#
# 为什么需要：
#   调拨单提交审批会发起 `biz_flow_instance`（biz_type=STOCK_TRANSFER），并按审批节点
#   展开审批人 —— 种子流程 `stock_transfer` 的审批节点是 `assignee_type=ADMIN`，运行时
#   展开成「所有持 SUPER_ADMIN 的用户」（`business/flow/assignee.rs`），于是 role/user
#   用例造的 operator 夹具（`tc_r_op_*` / `tc_u_op_*`，必持超管）会被挂成**待办**。
#   `biz_flow_task.assignee_id` 是硬外键 → 夹具用户既删不掉也硬删不了（外键保留豁免），
#   每跑一遍 e2e 就多钉死一批（t_15be8cc3 的实测：3 张单 = 3 实例 + 12 条 PENDING 待办）。
#
# 收尾口径（与 P11 / T12 的「取消即撤审批」同形）：
#   ① 终止 PENDING 待办：status=CANCELLED 且 completed_at=now()
#      （约束 ck_biz_flow_task_done：PENDING ⇔ completed_at IS NULL，必须成对写）
#   ② 终止活动节点：status IN ('PENDING','ACTIVE') → CANCELLED + ended_at
#   ③ 终止 PENDING 实例：status=CANCELLED + ended_at=now()
#      （约束 ck_biz_flow_instance_end：PENDING ⇔ ended_at IS NULL）
#   ④ 清单据弱引用 approval_instance_id，再删实例（biz_flow_task / _node / _cc 是 ON DELETE CASCADE）
#   ⑤ 删明细 → 删主单
#   默认「用完即删」；每个脚本都支持 <SCRIPT>_KEEP_DATA=1 保留数据供人工看页面，
#   但**保留时也必须先把在跑审批终止掉**（否则夹具用户又被钉住）。
#
# 硬红线：调用方必须传「按夹具/前缀收敛」的 scope 子查询（选 biz_stock_transfer.id），
#         不许传全表条件。scope 为空时不做任何 DML（防止手滑清库）。
#
# 用法（在调用脚本里）：
#   . "$(dirname "${BASH_SOURCE[0]}")/lib/flow_cleanup.sh"
#   SCOPE="SELECT id FROM biz_stock_transfer WHERE from_warehouse_id IN ('$WH_FROM','$WH_TO')"
#   e2e_cleanup_transfer_flow "$SCOPE"          # 本地 psql_x / psql_q 由脚本提供
# ============================================================

# 内部：取一次标量
_e2e_q() { "$1" "$2"; }

# e2e_terminate_transfer_flow <scope_sql>
#   只终止（不删）：待办 / 节点 / 实例。返回 0 = 终止执行完（不判断是否本来就没有）。
e2e_terminate_transfer_flow() {
    local scope="$1"
    local px="${E2E_CLEANUP_PSQL_X:-psql_x}"
    local pq="${E2E_CLEANUP_PSQL_Q:-psql_q}"
    local inst="SELECT id FROM biz_flow_instance WHERE biz_type = 'STOCK_TRANSFER' AND biz_id IN ($scope)"
    local n_task n_node n_inst

    if [ -z "$scope" ]; then
        echo '[cleanup] 拒绝执行：scope 为空（不许全表清）' >&2
        return 2
    fi

    n_task=$(_e2e_q "$pq" "SELECT count(*) FROM biz_flow_task WHERE status='PENDING' AND instance_id IN ($inst)")
    "$px" "UPDATE biz_flow_task
              SET status = 'CANCELLED',
                  comment = COALESCE(comment, 'e2e 收尾终止'),
                  completed_at = now()
            WHERE status = 'PENDING'
              AND instance_id IN ($inst)" >/dev/null

    n_node=$(_e2e_q "$pq" "SELECT count(*) FROM biz_flow_instance_node WHERE status IN ('PENDING','ACTIVE') AND instance_id IN ($inst)")
    "$px" "UPDATE biz_flow_instance_node
              SET status = 'CANCELLED',
                  ended_at = now(),
                  duration_ms = (EXTRACT(EPOCH FROM (now() - COALESCE(activated_at, now()))) * 1000)::bigint,
                  updated_at = now()
            WHERE status IN ('PENDING','ACTIVE')
              AND instance_id IN ($inst)" >/dev/null

    n_inst=$(_e2e_q "$pq" "SELECT count(*) FROM biz_flow_instance WHERE status='PENDING' AND biz_type='STOCK_TRANSFER' AND biz_id IN ($scope)")
    "$px" "UPDATE biz_flow_instance
              SET status = 'CANCELLED',
                  ended_at = now(),
                  result_comment = COALESCE(result_comment, 'e2e 收尾终止'),
                  revision = revision + 1,
                  updated_at = now()
            WHERE status = 'PENDING'
              AND biz_type = 'STOCK_TRANSFER'
              AND biz_id IN ($scope)" >/dev/null

    printf '[cleanup] 终止：PENDING 待办 %s 条（CANCELLED+completed_at）/ 活动节点 %s 条 / PENDING 实例 %s 条（CANCELLED+ended_at）\n' \
        "${n_task:-?}" "${n_node:-?}" "${n_inst:-?}"
    return 0
}

# e2e_cleanup_transfer_flow <scope_sql>
#   终止 → 删实例（级联待办/节点/抄送）→ 删明细 → 删主单；最后按 scope 核对，残留非 0 返回 1。
e2e_cleanup_transfer_flow() {
    local scope="$1"
    local px="${E2E_CLEANUP_PSQL_X:-psql_x}"
    local pq="${E2E_CLEANUP_PSQL_Q:-psql_q}"
    local inst="SELECT id FROM biz_flow_instance WHERE biz_type = 'STOCK_TRANSFER' AND biz_id IN ($scope)"
    local left_t left_i left_d left_s

    if [ -z "$scope" ]; then
        echo '[cleanup] 拒绝执行：scope 为空（不许全表清）' >&2
        return 2
    fi

    e2e_terminate_transfer_flow "$scope" || return 2

    # 单据的 approval_instance_id 是**弱引用**（无外键），先置空再删实例，避免留悬空指针
    "$px" "UPDATE biz_stock_transfer SET approval_instance_id = NULL, updated_at = now()
            WHERE id IN ($scope)" >/dev/null
    # 实例自引用 prev_instance_id 是 NO ACTION，链式退回的行要先断链
    "$px" "UPDATE biz_flow_instance SET prev_instance_id = NULL
            WHERE id IN ($inst)" >/dev/null
    "$px" "DELETE FROM biz_flow_instance WHERE id IN ($inst)" >/dev/null

    "$px" "DELETE FROM biz_stock_transfer_item_serial
            WHERE item_id IN (SELECT i.id FROM biz_stock_transfer_item i WHERE i.transfer_id IN ($scope))" >/dev/null
    "$px" "DELETE FROM biz_stock_transfer_item WHERE transfer_id IN ($scope)" >/dev/null
    "$px" "DELETE FROM biz_stock_transfer WHERE id IN ($scope)" >/dev/null

    left_t=$(_e2e_q "$pq" "SELECT count(*) FROM biz_stock_transfer WHERE id IN ($scope)")
    left_i=$(_e2e_q "$pq" "SELECT count(*) FROM biz_flow_instance WHERE biz_type='STOCK_TRANSFER' AND biz_id IN ($scope)")
    left_d=$(_e2e_q "$pq" "SELECT count(*) FROM biz_stock_transfer_item WHERE transfer_id IN ($scope)")
    left_s=$(_e2e_q "$pq" "SELECT count(*) FROM biz_stock_transfer_item_serial
                             WHERE item_id IN (SELECT i.id FROM biz_stock_transfer_item i WHERE i.transfer_id IN ($scope))")
    printf '[cleanup] 删除：实例（级联待办/节点/抄送）+ 明细 + 主单后残留 = 单据 %s / 实例 %s / 明细 %s / 序列号 %s\n' \
        "${left_t:-?}" "${left_i:-?}" "${left_d:-?}" "${left_s:-?}"
    [ "${left_t:-1}" = "0" ] && [ "${left_i:-1}" = "0" ] && [ "${left_d:-1}" = "0" ] && [ "${left_s:-1}" = "0" ]
}

# e2e_assert_scope_clean <scope_sql> <ok_fn> <bad_fn>
#   给调用脚本用的断言：残留必须为 0，并把「有没有 tc_*_op_ 夹具被挂成待办」一起报出来。
e2e_assert_scope_clean() {
    local scope="$1" okf="${2:-ok}" badf="${3:-bad}"
    local pq="${E2E_CLEANUP_PSQL_Q:-psql_q}"
    local n_tr n_inst n_pend

    n_tr=$(_e2e_q "$pq" "SELECT count(*) FROM biz_stock_transfer WHERE id IN ($scope)")
    n_inst=$(_e2e_q "$pq" "SELECT count(*) FROM biz_flow_instance WHERE biz_type='STOCK_TRANSFER' AND biz_id IN ($scope)")
    n_pend=$(_e2e_q "$pq" "SELECT count(*) FROM biz_flow_task t
                             WHERE t.status='PENDING'
                               AND t.instance_id IN (SELECT id FROM biz_flow_instance
                                                      WHERE biz_type='STOCK_TRANSFER' AND biz_id IN ($scope))")

    if [ "${n_tr:-1}" = "0" ] && [ "${n_inst:-1}" = "0" ] && [ "${n_pend:-1}" = "0" ]; then
        "$okf" "收尾核对：夹具单据 0 / 审批实例 0 / PENDING 待办 0（不再钉死任何夹具用户）"
    else
        "$badf" "收尾核对未通过：单据 $n_tr / 实例 $n_inst / PENDING 待办 $n_pend（应全 0）"
    fi
}

# e2e_report_pinned_fixtures
#   可观测性（不判定）：当前库里有多少条 PENDING 待办挂在 tc_[ru]_op_ 夹具用户上。
#   正常应为 0；非 0 说明**上一轮**某次 e2e 没走收尾（或被 kill -9）。
e2e_report_pinned_fixtures() {
    local pq="${E2E_CLEANUP_PSQL_Q:-psql_q}"
    local n
    n=$(_e2e_q "$pq" "SELECT count(*) FROM biz_flow_task t JOIN sys_user u ON u.id = t.assignee_id
                       WHERE t.status='PENDING' AND u.username ~ '^tc_[ru]_op_'")
    printf '[cleanup] 全库 KPI：PENDING 待办钉住的 tc_[ru]_op_ 夹具用户 = %s 条（应为 0）\n' "${n:-?}"
}
