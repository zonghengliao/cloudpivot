# E2E01-FIX-3 审批实例人工终止 —— 交付证据

- 卡面：`t_96198b53`「审批实例终止入口（现在只能改库才能终止会签卡住的流程）」
- 代码提交：`fc81cc6e`（feat）+ `becd04ec` / `76c3af4f` / `d95232ac`（验收脚本三处修正）
- 验证环境：145（`cloudpivot-dev.env` / 库 `cloudpivot_dev`）
  - **主证据**：`raw-145/verify_run_8033.log` —— 打 **正式测试服务 `:8033`**（新二进制已重启生效）
  - 旁证：`raw-145/verify_run.log` —— 打同库独立实例 `:8034`（重启前先验的同一份二进制）
  - 两份都是 `PASS=52 FAIL=0`

## 1. 问题 → 修法

| 环节 | 卡面描述 | 本卡落地 |
|---|---|---|
| 症状 | 会签节点缺人 → 实例永远 `PENDING`；`withdraw` 被 `409/65023` 拦下（文案还写着「请联系流程管理员终止」） | 真机复现：见 `raw-145/verify_run.log` 的 `4.1`（`HTTP 409 code=65023 msg=已有人审批（1 票），不能撤回；请联系流程管理员终止`） |
| 缺口 | `engine::terminate_instance` 只在进程内可用，没有 HTTP | 新增 `POST /api/business/flow/instance/{id}/terminate` |
| 权限 | 新增 `flow:instance:terminate` 或复用管理码（评估后定） | **复用 `business:flow:admin`**：V0124 里它的原始注释就是「流程管理员（退回任意节点 / **终止**）」，且 `reject` 的「驳回并终止」分支已经用它做行级判定。新开码要多一条迁移 + 一套角色授权决策，实际可达范围完全一样（默认都只授 `SUPER_ADMIN`） |
| 业务单据 | 必须跟着退，不许停在中间态（ARCH-8 约定 11） | 事务提交后逐条跑**全部业务桥**（`CANCELLED` 分支 = 单据退回），另含子流程回调父流程 |

## 2. 代码改动（11 文件 / +1464 行）

| 文件 | 改动 |
|---|---|
| `backend/src/business/flow/dto.rs` | `TerminateReq{reason}` + `normalize_terminate_reason`（必填、去空白、≤1000 字） |
| `backend/src/business/flow/engine.rs` | `terminate_by_admin`：复用 `terminate_instance(…, "CANCELLED", reason)`，把**已审批过的人**并入通知（原实现只通知发起人），加结构化日志 |
| `backend/src/business/flow/flow_model_progress.rs` | `approved_approvers(ex, instance_id)`：`status='APPROVED'` 的去重审批人 |
| `backend/src/business/flow/handler.rs` | 新 handler（权限注解 + 400/404 前置判定 + 事务 + 提交后 17 条业务补偿） |
| `backend/src/business/flow/terminate_db_tests.rs` | 4 条真库用例（主路径 / 终态拒绝 / 404 / 原因必填） |
| `backend/src/business/flow/mod.rs` | 挂载测试模块 |
| `backend/src/middleware/operation_log.rs` | `/terminate` → 操作名「终止审批实例」、对象 `FlowInstance` |
| `frontend/src/api/business/flow-api.ts` | `terminateFlowInstance(id, reason)` |
| `frontend/src/views/business/flow/instance/detail.vue` | 「终止审批」按钮 + 原因抽屉（有 `business:flow:admin` 且实例在跑才显示） |
| `docs/ERP-审批与审计口径.md` | §6「审批实例卡住时的标准处置流程」（卡面要求的那句话） |
| `scripts/verify/e2e01fix3_terminate.py` | 145 真机验收脚本（52 条断言 + 只读卡住实例扫描 + 零残留回查） |

## 3. 为什么实例落 `CANCELLED` 而不是 `TERMINATED`

卡面验收②写的是「实例状态 TERMINATED」，但**库与全链路都不存在这个字面量**：

- `biz_flow_instance.status` 的 CHECK 只有 `PENDING / APPROVED / REJECTED / CANCELLED / WITHDRAWN`（V0122）；
- `engine::terminate_instance` 的既有契约就是「只接受 REJECTED / CANCELLED」，`CANCELLED` 在本仓的定义就是**行政终止**；
- **15 个业务桥**（采购 / 收货 / 应付 / 合同 / 预算 / 报销 / 盘点 / 凭证 / 销售 / 调拨 …）的回调判据都是 `REJECTED | WITHDRAWN | CANCELLED → 单据退回`。新增 `TERMINATED` 要同时改 DDL + 15 个桥 + `instance_linkage` 语义 + 前端状态表，收益为零、回归面极大。

所以本卡按「实例落 `CANCELLED`（= 已终止）」实现，并在 `docs/ERP-审批与审计口径.md` §6 与 handler 头注释里写明理由。前端 `flow-instance` 域把它显示成「已取消」，详情页底部「结束说明」直接显示终止原因。

## 4. 145 真机验收（PASS=52 FAIL=0）

原始日志：`raw-145/verify_run_8033.log`（正式测试服务 `:8033`）、`raw-145/verify_run.log`（独立实例 `:8034`）。
关键断言逐条摘录（两份日志内容一致）：

```
[PASS] 3.2 发起后实例 PENDING            status=PENDING
[PASS] 3.3 会签应到 3 票                 task_count=3
[PASS] 3.5 会签甲同意（只签 1 票）        HTTP 200
[PASS] 3.6 实例仍 PENDING（1/3，批不完）  status=PENDING
[PASS] 3.7 停用会签人 b / c（模拟离职）    HTTP 200 ×2
[PASS] 4.1 有 1 票后撤回被 409/65023 拦下  msg=已有人审批（1 票），不能撤回；请联系流程管理员终止
[PASS] 5.1 无 business:flow:admin → 403   msg=无权限访问，需要权限: business:flow:admin
[PASS] 5.2 越权尝试没有改动实例            status=PENDING
[PASS] 5.3 空原因 → 400 + 65005
[PASS] 6.1 终止审批单                     HTTP 200
[PASS] 6.2 返回实例状态 CANCELLED
[PASS] 6.3 库里实例 = CANCELLED
[PASS] 6.4 ended_at 已写（终态约束）
[PASS] 6.5 终止原因落进 result_comment     result_comment=会签节点缺人（3 人会签只有 1 人在职），人工终止
[PASS] 6.6 未处理待办全部撤销（PENDING=0）
[PASS] 6.7 已投过的票保留（approved=1）
[PASS] 6.8 不留「处理中」的孤儿节点        active_nodes=0
[PASS] 6.9 通知已发（发起人 + 已审批过的人） receivers=3
[PASS] ④ 操作日志有记录  row=['终止审批实例','admin','{"reason": "会签节点缺人（3 人会签只有 1 人在职），人工终止"}']
[PASS] ⑥ 已结束实例再终止 → 400 + 65024
[PASS] 7.5 采购单进入审批中并生成审批实例   instance=01a1bf81-… po_status=SUBMITTED
[PASS] 7.6 终止采购审批实例                HTTP 200
[PASS] 7.8 ★ 关联采购单退回 DRAFT          po_status=DRAFT
[PASS] 7.9 采购单上没有残留实例指针         approval_instance_id 已清空
[PASS] 7.10 采购审批单的待办也被撤          pending=0
[PASS] 8.1–8.4 零残留（实例 / 账号 / 流程定义 / 采购单）
==============================================================================
PASS=52 FAIL=0
```

> 「3 人会签缺人」的构造：`COUNTERSIGN + approval_mode=ALL + 3 个 USER 审批人`，发起后 3 条待办；
> 甲签 1 票，乙/丙**停用**（`PATCH /system/user/{id}/status {status:0}`）→ 剩下两张票永远没人签。
> 节点的 `timeout_hours=48 + REMIND` 是为过 R28（`65048`）发布校验，催办不会替人签字。

## 5. 卡面第三条：现存「卡住的实例」扫描（只读）

脚本启动时对 145 `cloudpivot_dev` 做一次只读扫描，口径 = `PENDING` 且满足任一：

- 名下 `PENDING` 待办数 = 0（没人能推进）；
- 没有 `ACTIVE` 节点（引擎不会再动）；
- 已挂 7 天以上仍未结束。

```
实例总数（PENDING）= 8
可疑卡住 = 0
```

**结论：当前 145 测试库没有卡死实例**（E2E-01 演练那两张 PRQ 已经被绕过/收尾）。以后再出现，直接走
`POST /api/business/flow/instance/{id}/terminate` 收单，标准处置流程见 `docs/ERP-审批与审计口径.md` §6。

## 6. 测试与构建

| 项 | 命令 | 结果 |
|---|---|---|
| 类型检查 | `cargo check --lib`（独立 `CARGO_TARGET_DIR`） | 通过（52s） |
| 模块单测 | `cargo test --lib business::flow -- --test-threads=1`（真库 `cp_t3_term`） | **556 passed / 0 failed**（含新增 4 条） |
| 新增真库用例 | `cargo test --lib business::flow::terminate_db_tests` | 4 passed（主路径 / 终态拒绝 / 404 / 原因必填） |
| 格式 | `rustfmt +1.94.1 --edition 2024 --check`（本卡 7 个 .rs） | 干净 |
| 前端类型 | `pnpm typecheck` | 通过 |
| 前端构建 | `vite build --outDir dist-t3fix` | 通过；产物 `detail-*.js` 内含「终止审批」 |
| 145 真机验收 | `python3 scripts/verify/e2e01fix3_terminate.py --base http://127.0.0.1:8034/api` | `PASS=52 FAIL=0` |
| 145 正式服务验收 | 同上，`--base http://127.0.0.1:8033/api`（重启后） | `PASS=52 FAIL=0` |
| 145 部署 | `git merge --ff-only origin/main` → `cargo build`（2m13s）→ 重启 `cloudpivot-test` → `health=200` | 前端 `dist` 已换：入口 `assets/index-DuoE__kk.js`，`dist/assets/detail-DLqifEnT.js` 含「终止审批」（`http://127.0.0.1/` 实测返回该入口）；无 token 打 terminate → `401`（路由存在，不是 404） |

## 7. 已知限制 / 未做

1. **浏览器点击走查未做**：本环境禁止用自动化工具输入登录口令。替代证据 = 前端 build 产物含按钮文案 +
   `canTerminate` 的两条判据（权限 + 实例在跑）+ 接口真实 token 全过；145 的 `dist` 已重建。
2. **单据 `remark` 文案**：终止后采购单落库的 `remark` 仍写「审批驳回（未填写意见）」——这是 6 个业务桥
   共用的既有文案（驳回 / 撤回 / 终止同口径），本卡未改（改一处要动 6 个桥，属独立的文案收敛卡）。
3. **`FLOW_INSTANCE` 联动触发点不响 `CANCELLED`**：沿用 `instance_linkage.rs` 文件头的既有决策
   （级联终止与人工终止共用一个状态，接了会「同一张单有时响有时不响」）。终止的通知由引擎自己的
   effects 发，收件人 = 发起人 + 已审批过的人。
4. **403 用的是 `business:flow:admin`**：想看「谁能终止」就去角色管理看这个码的授权（默认只有 `SUPER_ADMIN`）。
