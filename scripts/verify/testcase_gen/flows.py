# -*- coding: utf-8 -*-
"""审批流全场景用例（A1–A15 × 4 个角色视角 = 60 条）。

**接口与错误码来源（逐条核对过）**

| 场景 | 真实接口 / 常量 |
|---|---|
| 提交 | `POST /api/business/flow/instance`（`business:flow:instance:create`）|
| 批准 / 驳回 / 转交 / 加签 | `POST /api/business/flow/task/{id}/{approve,reject,transfer,add-sign}`（`business:flow:approve`）|
| 撤回 | `POST /api/business/flow/instance/{id}/withdraw`（`business:flow:instance:withdraw`，`65023` 不可撤回）|
| 超时 | 内置任务 `flow_timeout_scan`，cron `0 */10 * * * *`；`AUTO_APPROVE` 需 `flow_def.allow_auto_approve=true` |
| 驳回意见 | `MIN_REJECT_COMMENT_CHARS = 5`（`flow/dto.rs`）|
| 会签 / 或签 | `engine.rs::countersign_all_waits_for_everyone` / `countersign_any_passes_on_first_vote` / `countersign_ratio_passes_early` |
| 回调 / 取消 / 删除联动 | `flow/task_bridge.rs::cancel_on_task_cancelled` / `cancel_on_task_deleted` / `on_transfer_settled` |
| 关键错误码 | `65004` 无权限、`65020` 待办已处理、`65023` 不可撤回、`65024` 实例非 PENDING、`65026` 转交非法、`65027` 加签非法、`65028` 不是我的待办、`65021` 定义在用不可删、`65022` 版本不可编辑、`65014` join 模式非法 |

**角色口径（四个视角，每场景都测）**

| 视角 | 口径 |
|---|---|
| 提交人 | 发起人本人：能撤回、能看自己实例、**不能**替审批人批准 |
| 审批人 | 该节点 assignee：能批准/驳回/转交/加签，处理完待办消失 |
| 超管 | `SUPER_ADMIN` 短路权限码（不 403），但「是不是我的待办」是数据判定 → 多半 `65028` |
| 无关人 | 无 `business:flow:*` → `403 + 30001`；看不到别人的实例 → `404 + 65002` |
"""

from .state_machines import AUTO_DONE, AUTO_PART, AUTO_TODO

OUTSIDER_DEFAULT = (
    "换一个无 `business:flow:*` 权限的账号（emp）调用同一接口",
    "403 + `30001`（PERMISSION_DENIED）；SQL 前后对比 `biz_flow_instance` / `biz_flow_task` 零变化",
)
ADMIN_DEFAULT = (
    "换 SUPER_ADMIN（admin）调用同一接口",
    "权限码短路（不 403），但「是否本人的待办」照旧判定 → 409/403 + `65028`（不是我的待办）；实例零变化",
)


def _s(key, title, pre, steps, views, prio, auto_map, note=""):
    return dict(
        key=key, title=title, pre=pre, steps=steps, views=views,
        prio=prio, auto_map=auto_map, note=note,
    )


SCENARIOS = [
    _s(
        "A1", "提交审批",
        "流程定义 `demo-single` 已发布（1 个 APPROVAL 节点，审批人 = 财务）；待审批业务单据存在（调拨单 DRAFT）",
        "1.`POST /api/business/flow/instance`，body `{def_code:\"demo-single\", title:\"TC-FLOW-A1\", biz_type:\"TRANSFER\", biz_id:<调拨单>}` 2.查 `biz_flow_instance` / `biz_flow_task` / 业务单据状态",
        {
            "提交人": ("提交人本人发起", "201/200；实例 PENDING、`initiator_id`=提交人、首个节点产生 1 条 PENDING 待办、业务单据状态随之推进（如 DRAFT→PENDING）"),
            "审批人": ("审批人（财务）在同一流程上再发一次自己的单据", "200，生成**自己的**实例；不得混入别人的实例（`initiator_id` 必须是本人）"),
            "超管": ("超管代他人发起（`initiator_id` 指定别人）", "200 但 `initiator_id` 必须真的等于被指定人（不允许被服务端静默改写为超管本人）；`created_by` 记超管"),
            "无关人": ("无 `business:flow:instance:create` 的账号提交", "403 + `30001`；`biz_flow_instance` 无新增行"),
        },
        "P0",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_TODO, ""), "超管": (AUTO_TODO, ""), "无关人": (AUTO_PART, "`scripts/verify/v_flow.py`（权限面）")},
    ),
    _s(
        "A2", "单人节点批准",
        "A1 的单签实例，待办在审批人名下",
        "1.`GET /api/business/flow/task/todo` 2.`POST /api/business/flow/task/{id}/approve` body `{comment:\"同意\"}` 3.再查待办与实例",
        {
            "提交人": ("提交人（非审批人）用自己的 token 批准该待办", "`65028`（不是我的待办）或 `65004`；待办仍 PENDING"),
            "审批人": ("审批人本人批准", "200；待办 APPROVED、实例 APPROVED、`ended_at` 非空（`ck_biz_flow_instance_end`）、待办列表里消失"),
            "超管": ADMIN_DEFAULT,
            "无关人": OUTSIDER_DEFAULT,
        },
        "P0",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_DONE, "`frontend/e2e/05-approval.spec.ts`（待办→详情→同意→实例 APPROVED）"),
         "超管": (AUTO_TODO, ""), "无关人": (AUTO_PART, "`scripts/verify/v_flow.py`")},
    ),
    _s(
        "A3", "多人会签（全部同意才通过）",
        "流程含 COUNTERSIGN 节点，审批人 = 2 人，模式「全部同意」",
        "1.审批人甲批准 2.查实例 3.审批人乙批准 4.查实例与待办",
        {
            "提交人": ("提交人查看进度并在甲批准后尝试撤回", "甲批准后实例仍 PENDING（会签未齐）；提交人撤回 → 200，实例 WITHDRAWN、剩余待办 CANCELLED"),
            "审批人": ("甲批准 → 乙批准", "甲：200，实例仍 PENDING、乙的待办仍 PENDING；乙：200，实例 APPROVED"),
            "超管": ("超管在乙未处理前直接批准乙的待办", "`65028`（不是我的待办）—— 超管不得代替会签人"),
            "无关人": OUTSIDER_DEFAULT,
        },
        "P0",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_PART, "`engine.rs::countersign_all_waits_for_everyone`（引擎纯逻辑）+ `scripts/verify/v_flow.py` 会签"),
         "超管": (AUTO_TODO, ""), "无关人": (AUTO_PART, "`scripts/verify/v_flow.py`")},
    ),
    _s(
        "A4", "多人或签（一人同意即通过）",
        "流程含 COUNTERSIGN 节点，模式「任一同意」（`countersign_any_passes_on_first_vote`）",
        "1.审批人甲批准 2.查乙的待办与实例",
        {
            "提交人": ("查看实例状态", "实例 APPROVED；`biz_flow_instance_node` 该节点通过"),
            "审批人": ("甲批准后，乙再批准自己那条待办", "甲：200 且实例 APPROVED；乙的待办已被置 `SKIPPED`/`CANCELLED`，乙再点 → `65020`（待办已处理）"),
            "超管": ADMIN_DEFAULT,
            "无关人": OUTSIDER_DEFAULT,
        },
        "P0",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_PART, "`engine.rs::countersign_any_passes_on_first_vote`（纯逻辑）"),
         "超管": (AUTO_TODO, ""), "无关人": (AUTO_TODO, "")},
    ),
    _s(
        "A5", "驳回",
        "A1 的单签实例，待办在审批人名下",
        "1.`POST /api/business/flow/task/{id}/reject` body `{comment:\"金额不符，退回\"}` 2.查实例/业务单据",
        {
            "提交人": ("提交人用自己 token 驳回该待办", "`65028`/`65004`；待办不变"),
            "审批人": ("审批人驳回，意见 12 字", "200；实例 REJECTED + `ended_at`；业务单据回退到可编辑态（如 PENDING→DRAFT 或保持 DRAFT 且可改）；提交人收到站内通知"),
            "超管": ADMIN_DEFAULT,
            "无关人": OUTSIDER_DEFAULT,
        },
        "P0",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_PART, "`scripts/verify/v_flow.py` 驳回 + `engine.rs::reject`"),
         "超管": (AUTO_TODO, ""), "无关人": (AUTO_PART, "`scripts/verify/v_flow.py`")},
    ),
    _s(
        "A6", "撤回（发起人）",
        "A1 的实例仍 PENDING",
        "1.`POST /api/business/flow/instance/{id}/withdraw` 2.查实例 / 待办 / 业务单据",
        {
            "提交人": ("发起人撤回", "200；实例 WITHDRAWN + `ended_at`；全部 PENDING 待办 → CANCELLED；业务单据回到 DRAFT"),
            "审批人": ("审批人尝试撤回别人的实例", "`65004` 或 `65023`（不可撤回）；实例仍 PENDING"),
            "超管": ("超管撤回他人实例", "200（超管可代撤）且审计日志记 `operator`=超管；待办全部 CANCELLED"),
            "无关人": ("无权限账号撤回", "403 + `30001`"),
        },
        "P0",
        {"提交人": (AUTO_PART, "`engine.rs::withdraw`（引擎逻辑）+ 单测覆盖"), "审批人": (AUTO_TODO, ""),
         "超管": (AUTO_PART, "`flow/model.rs::pending_instance_cancelled_with_ended_at_and_comment` 相邻覆盖"), "无关人": (AUTO_PART, "`scripts/verify/v_flow.py`")},
    ),
    _s(
        "A7", "转交",
        "A1 的待办在审批人名下",
        "1.`POST /api/business/flow/task/{id}/transfer` body `{to_user_id:<另一审批人>, comment:\"出差，请代办\"}` 2.查两张票",
        {
            "提交人": ("提交人尝试转交审批人的待办", "`65028`/`65026`；票不变"),
            "审批人": ("审批人转交给乙", "200；原票 `TRANSFERRED` + 新票 PENDING 挂乙；实例仍 PENDING；乙的待办列表出现该票"),
            "超管": ADMIN_DEFAULT,
            "无关人": OUTSIDER_DEFAULT,
        },
        "P1",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_PART, "`engine.rs::transfer`（实现存在，转交非法场景有 `65026` 校验）"),
         "超管": (AUTO_TODO, ""), "无关人": (AUTO_TODO, "")},
    ),
    _s(
        "A8", "加签",
        "A1 的待办在审批人名下",
        "1.`POST /api/business/flow/task/{id}/add-sign` body `{user_ids:[<丙>], comment:\"请丙复核\"}` 2.查待办",
        {
            "提交人": ("提交人尝试加签", "`65028`/`65027`；待办数不变"),
            "审批人": ("审批人加签丙", "200；丙多出 1 条 PENDING 待办；加签票与原票同属一个节点，节点通过条件按会签口径重算"),
            "超管": ADMIN_DEFAULT,
            "无关人": OUTSIDER_DEFAULT,
        },
        "P1",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_PART, "`engine.rs::add_sign` + `65027` 校验"),
         "超管": (AUTO_TODO, ""), "无关人": (AUTO_TODO, "")},
    ),
    _s(
        "A9", "超时自动处理",
        "节点 `timeout_hours=1`、`timeout_action=AUTO_APPROVE`；`flow_def.allow_auto_approve=true`；待办已超 1 小时",
        "1.触发内置任务 `flow_timeout_scan`（或 `SELECT cron_flow_timeout()`）2.查待办/实例/通知",
        {
            "提交人": ("提交人查实例", "实例按配置自动推进；`is_auto=true`、审批意见 = 「超时自动通过」；提交人收到通知"),
            "审批人": ("审批人在超时前批准", "200；随后扫描任务看到该票已终结 → 不重复处理（幂等，`urge_idempotency_key`）"),
            "超管": ("`allow_auto_approve=false` 时再跑扫描", "**完全不自动通过**（也不发通知）；待办仍 PENDING —— 全局闸默认 false 是硬要求"),
            "无关人": ("无 `business:flow:approve` 的账号试图手工触发扫描", "403 + `30001`；扫描是内置任务，不接受普通用户触发"),
        },
        "P1",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_PART, "`flow/timeout.rs::should_auto_approve` / `urge_idempotency_key`（纯逻辑）"),
         "超管": (AUTO_PART, "`flow/timeout.rs` 全局闸判定"), "无关人": (AUTO_TODO, "")},
    ),
    _s(
        "A10", "业务单据在审批中被取消（TSK-149 那一类）",
        "调拨单 PENDING 且有在跑审批实例（A1 造好）",
        "1.`PATCH /api/business/erp/transfer/{id}/status` body `{status:\"CANCELLED\", reason:\"用例取消\"}` 2.查实例 / 待办 / 审批人待办列表",
        {
            "提交人": ("提交人取消自己的调拨单", "200；实例 CANCELLED + `ended_at` 非空；**审批人待办里不再出现该票**（待办 → CANCELLED）；`biz_status_log` 有 CANCELLED 行"),
            "审批人": ("审批人此时再去批准那张票", "`65020`/`65024`（待办已处理/实例非 PENDING）；不得把已取消的单据改回 APPROVED"),
            "超管": ("超管直接改单状态绕过撤回", "同提交人：模板必须走同一事务把实例与待办一起撤（这正是 TSK-149 的根因面）"),
            "无关人": ("无 `business:erp:transfer:status` 的账号取消", "403 + `30001`；实例仍 PENDING"),
        },
        "P0",
        {"提交人": (AUTO_PART, "`flow/model.rs::cancel_pending_of_biz` + `transfer_handler.rs::withdraw_cancels_running_approval_instance`"),
         "审批人": (AUTO_TODO, ""), "超管": (AUTO_TODO, ""), "无关人": (AUTO_TODO, "")},
        note="本场景就是 TSK-149 的回归面；一旦回退到「只改单据不改实例」，审批人待办会挂着一条死票。",
    ),
    _s(
        "A11", "业务单据在审批中被删除",
        "任务挂了审批实例且实例 PENDING",
        "1.`DELETE /api/business/task/{id}` 2.查实例 / 待办",
        {
            "提交人": ("创建人删除自己挂审批的任务", "200；实例 CANCELLED + `ended_at`；待办全部 CANCELLED；软删 `deleted_at` 非空"),
            "审批人": ("审批人再去批准该票", "`65020`/`65024`；不得给已删任务写审批结果"),
            "超管": ("超管删除他人任务", "200，审计留痕；实例同样被撤（不允许出现「实例挂在已删任务上」）"),
            "无关人": ("无关人删除", "403 + `63002`（TASK_FORBIDDEN）；实例不动"),
        },
        "P0",
        {"提交人": (AUTO_PART, "`flow/model.rs::delete_task` + `cancel_on_task_deleted` 单测（`pending_instance_cancelled_with_ended_at_and_comment`）"),
         "审批人": (AUTO_TODO, ""), "超管": (AUTO_TODO, ""), "无关人": (AUTO_TODO, "")},
    ),
    _s(
        "A12", "审批通过但业务单据已终态（回调保护）",
        "任务已在 CANCELLED/DONE，但实例仍 PENDING（历史脏数据 / 并发窗口）",
        "1.审批人批准该票 2.查任务状态与 `biz_status_log`",
        {
            "提交人": ("提交人查看任务", "任务状态**不变**（仍是 CANCELLED/DONE），不被回调改回 DOING"),
            "审批人": ("审批人批准", "200 或明确 4xx —— 但**无论哪种都不许把已终态任务改活**；`TaskStatus::FlowCallback` 只在 REVIEW 态生效（`callback_stays_inside_review`）"),
            "超管": ("超管重放回调", "同上：`callback_bypasses_gate_on_review_to_done` 只对 REVIEW→DONE 放行，其他态一律拒绝"),
            "无关人": ("无关人触发回调", "403 + `65004`/`30001`；任务状态零变化"),
        },
        "P0",
        {"提交人": (AUTO_PART, "`task/state.rs::callback_stays_inside_review` / `callback_identity_does_not_leak_into_user_path`"),
         "审批人": (AUTO_PART, "`task/state.rs::callback_bypasses_gate_on_review_to_done`"), "超管": (AUTO_PART, "同左（纯逻辑）"), "无关人": (AUTO_TODO, "")},
    ),
    _s(
        "A13", "审批中改了流程定义（版本切换）",
        "实例 A 跑在 v1；随后把定义改成 v2 并发布",
        "1.`PUT /api/business/flow/version/{v1}/graph` 改节点 2.`POST /api/business/flow/def/{id}/publish` 3.查实例 A 的节点与审批人 4.新发起实例 B",
        {
            "提交人": ("提交人改自己正在跑的流程并看实例 A", "已发布版本不可改 → `65022`（FLOW_VERSION_NOT_EDITABLE）；只允许 `POST /def/{id}/draft` 生成 v2；实例 A 仍按 v1 快照跑（`load_snapshot`）"),
            "审批人": ("审批人处理实例 A 的票", "200；审批人仍是 v1 里的那个人，不因 v2 改了审批人而变"),
            "超管": ("超管删除在用定义", "`65021`（FLOW_DEF_IN_USE）；定义与实例都留着"),
            "无关人": ("无关人发布定义", "403 + `30001`；版本号不变"),
        },
        "P0",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_TODO, ""), "超管": (AUTO_PART, "`flow/model.rs::copy_version_to_draft` / `load_snapshot` 单测"), "无关人": (AUTO_TODO, "")},
    ),
    _s(
        "A14", "条件分支（金额 > X 走不同路径）",
        "定义含 CONDITION 节点：金额 > 10000 走「财务总监」，否则走「财务专员」",
        "1.提交金额 500 的单据 2.提交金额 50000 的单据 3.查两条实例的当前节点",
        {
            "提交人": ("提交两笔不同金额", "两实例当前节点不同（500 → 财务专员；50000 → 财务总监）；`biz_flow_instance_node` 的 node_key 与分支一致"),
            "审批人": ("财务专员看自己的待办", "只看到 500 那笔；50000 那笔不在他的待办里"),
            "超管": ("两条实例都能看", "200；节点与金额匹配；不出现「两条都走同一分支」（那说明条件引擎没生效）"),
            "无关人": ("无关人提交", "403 + `30001`；实例数不增"),
        },
        "P1",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_TODO, ""), "超管": (AUTO_PART, "`flow/expression.rs` + `engine.rs::pick_condition_edge`（纯逻辑有测）"), "无关人": (AUTO_TODO, "")},
    ),
    _s(
        "A15", "并行分支",
        "定义含 PARALLEL 节点，两条并行分支各一个审批人，汇合 join = AND",
        "1.提交 2.甲批准 3.查实例 4.乙批准 5.查实例",
        {
            "提交人": ("查看进度", "甲批准后实例仍 PENDING（AND 汇聚未齐）；乙批准后实例 APPROVED"),
            "审批人": ("甲 → 乙 依次批准", "各自 200；任一方未批前实例不得前进（否则就是 OR 语义，属于 bug）"),
            "超管": ("把 join 模式改成非法值再发布", "`65014`（FLOW_GRAPH_JOIN_MODE）；发布被拒，旧版本继续可用"),
            "无关人": OUTSIDER_DEFAULT,
        },
        "P1",
        {"提交人": (AUTO_TODO, ""), "审批人": (AUTO_PART, "`engine.rs::passed_upstream_sources` / `maybe_finish`（纯逻辑）"), "超管": (AUTO_PART, "`65014` join 模式校验单测"), "无关人": (AUTO_TODO, "")},
    ),
]

ROLE_ORDER = ["提交人", "审批人", "超管", "无关人"]
