# -*- coding: utf-8 -*-
"""状态机全组合用例数据。

**边表来源（唯一真相，逐条抄自代码，不是我编的）**

| 实体 | 来源 |
|---|---|
| `biz_task` | `backend/src/business/task/state.rs::TRANSITIONS`（10 条） |
| `biz_stock_in` | `backend/src/business/shared/state_machine.rs::STOCK_IN_EDGES`（3 条） |
| `biz_transfer` | 同上 `TRANSFER_EDGES`（6 条） |
| `biz_purchase_order` | 同上 `PURCHASE_ORDER_EDGES`（10 条） |
| `biz_payable` | 同上 `PAYABLE_EDGES`（4 条） |
| `biz_material_serial` | `backend/src/business/erp/serial_model.rs::transition_rejected_message`（3 条） |

**生成规则**：`N 个状态 × N 个状态` 全排列，一条不落。合法边断言"通过 + 副作用 + 状态日志"，
非法边（含自环）断言 `409 + 错误码 + 文案点名当前态可选去向`。

**优先级口径**：动账/动流程的边 = P0；纯状态位 = P1；从终态或入账态出发的非法边 = P0
（这类边放行过一次，账就再也对不上了）。
"""

# ==================== 自动化状态词表 ====================
# 只允许这三个取值：不许把待写的标成已完成（卡面硬红线 6）。
AUTO_DONE = "✅ 已自动"
AUTO_PART = "🟡 部分（仅单测）"
AUTO_TODO = "⬜ 待写"


def _task_edges():
    """任务：10 条合法边 → (前置, 副作用)。"""
    return {
        ("TODO", "DOING"): (
            "任务 `need_review=true`（默认）、前置依赖（biz_task_dependency）已全部完成",
            "`progress` 被抬到 ≥10（`ProgressEffect::AtLeast(10)`）；`biz_status_log` 新增 1 行 TODO→DOING",
        ),
        ("DOING", "REVIEW"): (
            "任务 `need_review=true`，操作者 = 负责人",
            "`progress` 强制 100；`biz_status_log` 新增 1 行；`done_at` 仍为空",
        ),
        ("DOING", "DONE"): (
            "任务 `need_review=false`（免验收），操作者 = 负责人/创建人",
            "`progress`=100、`done_at=now()`；`biz_status_log` 新增 1 行",
        ),
        ("REVIEW", "DONE"): (
            "操作者是验收人（创建人/分派人/超管），且任务上没有挂起的审批实例",
            "`progress`=100、`done_at=now()`；父任务据此重算进度",
        ),
        ("REVIEW", "DOING"): (
            "请求体带 `reject_reason`（≥5 字），操作者是验收人",
            "`progress` 回退到 10、`reject_reason` 落库、`done_at` 仍空",
        ),
        ("DONE", "DOING"): (
            "操作者 = 创建人或超管（重开）",
            "`progress` 归 10、`done_at` 清空；日报/燃尽图按重开日重新计",
        ),
        ("TODO", "CANCELLED"): (
            "操作者 = 负责人/创建人/分派人/超管",
            "同一事务内全部非终态子孙（TODO/DOING/REVIEW）级联为 CANCELLED；`progress` 冻结",
        ),
        ("DOING", "CANCELLED"): (
            "同上（且该任务已挂了待办时，待办须同步撤销 —— 见 TC-CONS-26）",
            "子孙级联 CANCELLED；`done_at` 不变；挂起的审批实例被撤销",
        ),
        ("REVIEW", "CANCELLED"): (
            "同上",
            "子孙级联 CANCELLED；挂起的审批实例被撤销，审批人待办里不再出现",
        ),
        ("CANCELLED", "TODO"): (
            "操作者 = 创建人或超管（恢复）",
            "`progress` 归 0；`biz_status_log` 新增 1 行 CANCELLED→TODO",
        ),
    }


TASK = dict(
    prefix="TC-TASK",
    module="任务管理",
    table="biz_task",
    api="PATCH /api/business/task/{id}/status",
    body='{"status":"{to}"}',
    log_check="SELECT from_status,to_status FROM biz_status_log WHERE entity_type='TASK' AND entity_id={id} ORDER BY created_at",
    statuses=[
        ("TODO", "待开始"),
        ("DOING", "进行中"),
        ("REVIEW", "待验收"),
        ("DONE", "已完成"),
        ("CANCELLED", "已取消"),
    ],
    legal=_task_edges(),
    illegal_code="409 + `63010`（TASK_STATUS_TRANSITION_INVALID）",
    illegal_msg=(
        "文案点名当前态可选去向（TRANSITIONS 共 10 条），且 `status`/`progress` 不变、"
        "`biz_status_log` 无新增"
    ),
    self_msg="自环（from == to）不在白名单 → 409 + `63010`；不得写日志、不得改 `updated_at`",
    pre_tpl="admin 建一条 {from} 态任务（标题 `TC-TASK-{i}`，need_review 按用例前置）",
    prio_legal_default="P1",
    prio_legal={
        ("DONE", "DOING"): "P0",
        ("REVIEW", "DONE"): "P0",
        ("TODO", "CANCELLED"): "P0",
        ("DOING", "CANCELLED"): "P0",
        ("REVIEW", "CANCELLED"): "P0",
    },
    auto_default=AUTO_PART,
    auto_note=(
        "`backend/src/business/task/state.rs::whitelist_covers_exactly_the_ten_allowed_pairs` + "
        "`illegal_pairs_are_rejected_with_illegal_pair`（25 组合表驱动，纯逻辑无库）；"
        "接口层 200/409 未覆盖"
    ),
)


STOCK_IN = dict(
    prefix="TC-STKIN",
    module="收货入库单",
    table="biz_stock_in",
    api="PATCH /api/business/erp/stock-in/{id}/status",
    body='{"status":"{to}"}',
    log_check="SELECT from_status,to_status FROM biz_status_log WHERE entity_type='STOCK_IN' AND entity_id={id}",
    statuses=[
        ("DRAFT", "草稿"),
        ("POSTED", "已过账"),
        ("CANCELLED", "已作废"),
    ],
    legal={
        ("DRAFT", "POSTED"): (
            "单据有 ≥1 明细、仓库/库位/供应商齐全，操作者持 `business:erp:stock-in:status`",
            "写 `biz_inventory`（移动加权）+ `biz_inventory_location` + `biz_inventory_txn`；"
            "生成自动凭证（借 1405 存货 / 借 2221 进项税 / 贷 2202 应付）；建应付单 OPEN；"
            "写 `biz_status_log`（DRAFT→POSTED）",
        ),
        ("DRAFT", "CANCELLED"): (
            "请求体带 `reason`（作废原因）",
            "无任何账务副作用（库存/凭证/应付均不动）；写 `biz_status_log`（DRAFT→CANCELLED）",
        ),
        ("POSTED", "CANCELLED"): (
            "请求体带 `reason`，且单据未发生过出库（已发货 → 409，见 cancel_with_shipped_goods_is_409）",
            "红冲凭证（生成红字凭证、`reversed_by`/`reverse_of` 双向写回）+ 回滚库存/库位账/流水（P9）"
            "+ 应付作废；写 `biz_status_log`",
        ),
    },
    illegal_code="409 + `10003`（RESOURCE_CONFLICT）",
    illegal_msg=(
        "文案必须逐字包含「收货入库单当前是「{FROMCN}」（{FROM}），不能直接流转到「{TOCN}」（{TO}）：允许的去向是 {OUT}」"
        "（源码原串 `backend/src/business/erp/stockin_model.rs:224`），白名单必须等于当前态**真实出边**；"
        "库存/凭证/应付三处均无新增行"
    ),
    self_msg="自环 → 409 + `10003`；库存与凭证不得变动（防「重复过账」）",
    pre_tpl="用 admin 建一张 {from} 态收货单（单号 `TC-STKIN-{i}`，1 明细 × 仓库/库位/供应商）",
    prio_legal_default="P0",
    prio_legal={},
    auto_default=AUTO_TODO,
    auto_note="",
    auto_overrides={
        ("DRAFT", "POSTED"): (
            AUTO_PART,
            "`voucher.rs::post_creates_posted_auto_voucher` / `post_creates_three_entries_debit_inventory_debit_tax_credit_payable`",
        ),
        ("POSTED", "CANCELLED"): (
            AUTO_PART,
            "`stockin_handler.rs::cancel_rolls_back_cost_inventory` / `cancel_still_generates_red_voucher` / `cancel_uses_posted_amount_not_current_avg`",
        ),
        ("DRAFT", "CANCELLED"): (AUTO_PART, "`stockin_handler.rs::cancel_paths`"),
        ("CANCELLED", "CANCELLED"): (AUTO_PART, "`stockin_handler.rs::cancel_twice_is_409`（作废后再作废 409）"),
    },
)


TRANSFER = dict(
    prefix="TC-XFER",
    module="库存调拨单",
    table="biz_stock_transfer",
    api="PATCH /api/business/erp/transfer/{id}/status",
    body='{"status":"{to}"}',
    log_check="SELECT from_status,to_status FROM biz_status_log WHERE entity_type='STOCK_TRANSFER' AND entity_id={id}",
    statuses=[
        ("DRAFT", "草稿"),
        ("PENDING", "待审批"),
        ("APPROVED", "已通过"),
        ("IN_TRANSIT", "在途"),
        ("DONE", "已完成"),
        ("CANCELLED", "已作废"),
    ],
    legal={
        ("DRAFT", "PENDING"): (
            "调拨单有 ≥1 明细、源仓/目标仓不同、操作者持 `business:erp:transfer:status`",
            "创建审批实例（`biz_flow_instance` PENDING + 首个节点待办）；`uk_biz_flow_instance_biz_active` 保证同时只有一个在跑实例",
        ),
        ("DRAFT", "CANCELLED"): (
            "请求体带 `reason`",
            "无账务副作用；写 `biz_status_log`（DRAFT→CANCELLED）",
        ),
        ("PENDING", "APPROVED"): (
            "审批实例已审批通过（回调推进）或超管直改",
            "实例置 APPROVED；单据置 APPROVED，等待发货",
        ),
        ("PENDING", "CANCELLED"): (
            "请求体带 `reason`（`pending_to_cancelled_requires_reason`）",
            "**撤掉在跑审批实例**（实例 CANCELLED、审批人待办消失、`ended_at` 同时写）；库存不动",
        ),
        ("APPROVED", "IN_TRANSIT"): (
            "源仓有足够库存（无序列号管控或序列号已选出）",
            "源仓 `biz_inventory` 扣减 + `biz_inventory_location` 扣减 + `TRANSFER_OUT` 流水；写 `biz_status_log`",
        ),
        ("IN_TRANSIT", "DONE"): (
            "在途仓/目标仓已建、库位可用",
            "目标仓入库 + `TRANSFER_IN` 流水 + 库位账；源价带成本转移（金额不得为 0，见 C3/C4）；`biz_status_log`",
        ),
    },
    illegal_code="409 + `10003`（RESOURCE_CONFLICT）",
    illegal_msg=(
        "文案必须逐字包含「非法状态流转：{FROMCN}（{FROM}）→ {TOCN}（{TO}）」（括号里跟一句拒绝原因，如「库存不足」「无权操作」）+「{FROMCN}（{FROM}）可以流转到：{OUT}」，"
        "第二句允许的去向必须等于当前态**真实出边**；终态时第二句是「{FROMCN}（{FROM}）是终态，不能再流转」；"
        "源码 `backend/src/business/erp/transfer_model.rs:176-196`"
    ),
    self_msg="自环 → 409 + `10003`；不得重复扣减/重复入库",
    pre_tpl="admin 建一张 {from} 态调拨单（单号 `TC-XFER-{i}`，源仓 A → 目标仓 B，1 明细）",
    prio_legal_default="P0",
    prio_legal={("DRAFT", "PENDING"): "P1"},
    auto_default=AUTO_TODO,
    auto_note="",
    auto_overrides={
        ("PENDING", "CANCELLED"): (AUTO_PART, "`transfer_handler.rs::pending_to_cancelled_requires_reason`"),
        ("DONE", "DRAFT"): (AUTO_PART, "`transfer_handler.rs::done_rejects_every_transition`"),
        ("DONE", "PENDING"): (AUTO_PART, "`transfer_handler.rs::done_rejects_every_transition`"),
        ("DONE", "APPROVED"): (AUTO_PART, "`transfer_handler.rs::done_rejects_every_transition`"),
        ("DONE", "IN_TRANSIT"): (AUTO_PART, "`transfer_handler.rs::done_rejects_every_transition`"),
        ("DONE", "DONE"): (AUTO_PART, "`transfer_handler.rs::done_rejects_every_transition`"),
        ("DONE", "CANCELLED"): (AUTO_PART, "`transfer_handler.rs::done_rejects_every_transition`"),
    },
)


PURCHASE = dict(
    prefix="TC-PO",
    module="采购订单",
    table="biz_purchase_order",
    api="PATCH /api/business/erp/purchase-order/{id}/status",
    body='{"status":"{to}"}',
    log_check="SELECT from_status,to_status FROM biz_status_log WHERE entity_type='PURCHASE_ORDER' AND entity_id={id}",
    statuses=[
        ("DRAFT", "草稿"),
        ("SUBMITTED", "已提交"),
        ("APPROVED", "已批准"),
        ("PARTIAL_RECEIVED", "部分收货"),
        ("RECEIVED", "已收货"),
        ("CLOSED", "已关闭"),
        ("CANCELLED", "已作废"),
    ],
    legal={
        ("DRAFT", "SUBMITTED"): (
            "订单有 ≥1 明细、供应商/计量单位齐全",
            "订单进入待审批；`biz_status_log`（DRAFT→SUBMITTED）",
        ),
        ("DRAFT", "CANCELLED"): ("请求体带 `reason`", "无账务副作用；写日志"),
        ("SUBMITTED", "APPROVED"): (
            "审批通过（采购单尚未接入审批流程时由超管直改 —— 缺口 K-01）",
            "订单置 APPROVED，等待收货",
        ),
        ("SUBMITTED", "CLOSED"): (
            "请求体带 `reason`（提前关闭）",
            "订单置 CLOSED；未收货部分不再收货",
        ),
        ("SUBMITTED", "CANCELLED"): (
            "请求体带 `reason`（`cancel_submitted_requires_reason_then_appends_remark`）",
            "订单置 CANCELLED，`reason` 追加进 `remark`；写日志",
        ),
        ("SUBMITTED", "PARTIAL_RECEIVED"): (
            "由收货过账链路（`stockin_model::sync_purchase_status`）写入，**不是**人工 PATCH",
            "订单状态 = 部分收货；`biz_status_log.biz_ref_no` = 收货单号（缺口 K-02：人工直改未收紧）",
        ),
        ("SUBMITTED", "RECEIVED"): (
            "同上，一次性收满",
            "订单状态 = 已收货；`biz_ref_no` = 收货单号（缺口 K-02）",
        ),
        ("APPROVED", "PARTIAL_RECEIVED"): ("收货链路写入（部分收货）", "与 SUBMITTED→PARTIAL_RECEIVED 同口径"),
        ("APPROVED", "RECEIVED"): ("收货链路写入（一次收满）", "与 SUBMITTED→RECEIVED 同口径"),
        ("PARTIAL_RECEIVED", "RECEIVED"): (
            "对同一订单再收一次、累计收满（**不是**自环：状态真的变了）",
            "订单收到 RECEIVED；`biz_status_log` 新增 1 行（不是 2 行 —— 自环必须跳过）",
        ),
    },
    illegal_code="409 + `10003`（RESOURCE_CONFLICT）",
    illegal_msg=(
        "文案必须逐字包含「采购订单当前是「{FROMCN}」（{FROM}），不能直接流转到「{TOCN}」（{TO}）：允许的去向是 {OUT}」"
        "（源码原串 `backend/src/business/erp/purchase_model.rs:155`）；"
        "从终态（RECEIVED/CLOSED/CANCELLED）出发时改成「没有可流转的下一个状态：它已是终态，或该状态由收货 / 审批流程维护（P2-B1）」（`purchase_model.rs:148`）"
    ),
    self_msg="自环 → 409 + `10003`（`PARTIAL_RECEIVED→PARTIAL_RECEIVED` 的继续收货由调用方跳过 transition，只写流水）",
    pre_tpl="admin 建一张 {from} 态采购单（单号 `TC-PO-{i}`，1 明细）",
    prio_legal_default="P1",
    prio_legal={
        ("SUBMITTED", "PARTIAL_RECEIVED"): "P0",
        ("SUBMITTED", "RECEIVED"): "P0",
        ("APPROVED", "PARTIAL_RECEIVED"): "P0",
        ("APPROVED", "RECEIVED"): "P0",
        ("PARTIAL_RECEIVED", "RECEIVED"): "P0",
        ("SUBMITTED", "CANCELLED"): "P0",
    },
    auto_default=AUTO_TODO,
    auto_note="",
    auto_overrides={
        ("SUBMITTED", "CANCELLED"): (
            AUTO_PART,
            "`purchase_handler.rs::cancel_submitted_requires_reason_then_appends_remark`",
        ),
    },
)


PAYABLE = dict(
    prefix="TC-AP",
    module="应付单",
    table="biz_payable",
    api="POST /api/business/erp/payable/{id}/pay 或 POST /api/business/erp/payable/{id}/cancel",
    body='{"amount":"100.00","pay_method":"BANK"}' + " / " + '{"reason":"用例作废"}',
    log_check="SELECT from_status,to_status FROM biz_status_log WHERE entity_type='PAYABLE' AND entity_id={id}",
    statuses=[
        ("OPEN", "未付款"),
        ("PARTIAL_PAID", "部分付款"),
        ("SETTLED", "已结清"),
        ("CANCELLED", "已作废"),
    ],
    legal={
        ("OPEN", "PARTIAL_PAID"): (
            "应付金额 1000，付款 300（< 剩余未付），操作者持 `business:erp:payable:pay`",
            "`paid_amount`=300、`status`=PARTIAL_PAID、写一条 `biz_payable_payment`；写 `biz_status_log`",
        ),
        ("OPEN", "SETTLED"): (
            "应付金额 1000，一次付 1000",
            "`paid_amount`=1000、`settled_at`=now()、`status`=SETTLED",
        ),
        ("PARTIAL_PAID", "SETTLED"): (
            "剩余未付 700，付 700（恰好清账）",
            "`settled_at`=now()、`status`=SETTLED；未付余额 = 0",
        ),
        ("OPEN", "CANCELLED"): (
            "请求体带 `reason`，`paid_amount = 0`",
            "`status`=CANCELLED；若来源收货单已作废则在此前已作废（`cancel_twice_is_409`）",
        ),
    },
    illegal_code="409 + `10003`（RESOURCE_CONFLICT）",
    illegal_msg="文案点名当前态：已结清 → 「已结清不能再付款」；已作废 → 「应付单 … 已经作废过了，不能重复作废」；部分付款 → 「已付 100.00，不能作废」",
    self_msg="自环（from == to）不在白名单 → 409 + `10003`；不得写日志、不得改 `updated_at`",
    self_msgs={
        ("PARTIAL_PAID", "PARTIAL_PAID"): (
            "200 且 `status` 仍 PARTIAL_PAID（再付 300，剩余 400）：付款记录 +1、`paid_amount`=600；"
            "**不得写一条自环 `biz_status_log`**（`transition()` 拒绝自环，调用方要先判状态相同就跳过）"
        ),
        ("SETTLED", "SETTLED"): "409 + `10003`；文案「已结清不能再付款」；`paid_amount`/付款记录不变",
        ("CANCELLED", "CANCELLED"): "409 + `10003`；文案「应付单 … 已经作废过了，不能重复作废」；无新增付款/红冲",
        ("OPEN", "OPEN"): (
            "没有任何请求能把 OPEN 留在 OPEN —— 付款必改状态。用例改为断言：`amount=0` 调 pay → 400 + `10001`，"
            "且 `status` 仍 OPEN、无付款记录"
        ),
    },
    pre_tpl="建一张 {from} 态应付单（金额 1000，`paid_amount` 按用例设置，单号 `TC-AP-{i}`）",
    prio_legal_default="P0",
    prio_legal={},
    auto_default=AUTO_TODO,
    auto_note="",
    auto_overrides={
        ("OPEN", "SETTLED"): (AUTO_PART, "`payable_payment.rs::p7_cancel_unpaid_payable_reverses_its_voucher` 相邻覆盖（全额付款路径）"),
        ("SETTLED", "SETTLED"): (AUTO_PART, "`payable_payment.rs::p7_pay_settled_or_cancelled_is_conflict`"),
        ("SETTLED", "OPEN"): (AUTO_PART, "`payable_payment.rs::p7_cancel_paid_payable_is_conflict`"),
        ("PARTIAL_PAID", "CANCELLED"): (AUTO_PART, "`payable_payment.rs::p7_cancel_paid_payable_is_conflict`"),
    },
)


SERIAL = dict(
    prefix="TC-SERIAL",
    module="物料序列号",
    table="biz_material_serial",
    api="PATCH /api/business/erp/serial/{id}/status",
    body='{"status":"{to}","reason":"用例"}' + "",
    log_check=None,
    statuses=[
        ("IN_STOCK", "在库"),
        ("OUTBOUND", "已出库"),
        ("SCRAPPED", "已报废"),
    ],
    legal={
        ("IN_STOCK", "OUTBOUND"): (
            "序列号属于该物料/仓库、操作者持 `business:erp:serial:status`",
            "`outbound_at`=now()、写 `SERIAL_OUT` 流水（库存联动，见 B4）",
        ),
        ("IN_STOCK", "SCRAPPED"): (
            "请求体带报废原因",
            "`scrapped_at`=now()、写 `SERIAL_OUT` 流水；序列号不可逆",
        ),
        ("OUTBOUND", "SCRAPPED"): (
            "请求体带报废原因（已出库件在客户端报废）",
            "`scrapped_at`=now()，`outbound_at` **保持不变**（`transition_to_scrapped_from_outbound_keeps_time`）",
        ),
    },
    illegal_code="409 + `66008`（ERP_SERIAL_STATUS_INVALID）",
    illegal_msg="文案区分两种：`OUTBOUND→IN_STOCK` → 「已出库的序列号不能回退到在库：要重新入库请生成新的序列号」；`SCRAPPED→*` → 「已报废的序列号不能再流转（当前 … → 目标 …），状态机不可逆」",
    self_msg="自环 → 409 + `66008`；不得写流水、不得改时间戳",
    pre_tpl="造一个 {from} 态序列号（`TC-SERIAL-{i}`，挂在该物料/仓库上）",
    prio_legal_default="P0",
    prio_legal={},
    auto_default=AUTO_TODO,
    auto_note="",
    auto_overrides={
        ("IN_STOCK", "OUTBOUND"): (AUTO_PART, "`serial_handler.rs::transition_to_outbound_sets_outbound_at`"),
        ("IN_STOCK", "SCRAPPED"): (AUTO_PART, "`serial_handler.rs::transition_to_scrapped_from_stock`"),
        ("OUTBOUND", "SCRAPPED"): (AUTO_PART, "`serial_handler.rs::transition_to_scrapped_from_outbound_keeps_time`"),
        ("OUTBOUND", "IN_STOCK"): (AUTO_PART, "`serial_handler.rs::transition_back_to_stock_is_rejected`"),
    },
)


ENTITIES = [TASK, STOCK_IN, TRANSFER, PURCHASE, PAYABLE, SERIAL]

# 终态集合（判优先级用：从终态出发的非法边一律 P0）
TERMINAL = {
    "TC-TASK": {"DONE", "CANCELLED"},
    "TC-STKIN": {"CANCELLED"},
    "TC-XFER": {"DONE", "CANCELLED"},
    "TC-PO": {"RECEIVED", "CLOSED", "CANCELLED"},
    "TC-AP": {"SETTLED", "CANCELLED"},
    "TC-SERIAL": {"SCRAPPED"},
}
