# M3-11 客户信用：超信用拦截（按配置策略执行）

- 卡：`t_8a08ce48`（M3-11【M3·销售】客户信用：超信用拦截）
- 上游：`t_464b7265`（M3-09 策略三态 `BLOCK/WARN/PASS` + 提交守卫）、`t_8729b656`（M3-10 实时快照）
- 日期：2026-11-17

## 一句话

信用额度的**唯一占用时刻是 `APPROVED`**。M3-09 只把闸装在「提交」那一脚，本卡把它补在
**「审批通过」**那一脚：审批点「同意」之前按客户策略拦（`BLOCK` → 409，待办与单据都不动），
审批结论回写单据时再用同一份判定兜一次（`BLOCK` → 不落 `APPROVED`，**打回草稿**并写清原因，
不留死单）。判定仍然只有一份：`credit::decide`。

## 为什么必须补这一脚（不是重复劳动）

| 提交后 → 审批前发生了什么 | 提交守卫（M3-09）当时看到 | 审批通过时真实情况 |
|---|---|---|
| 同客户另一张单被审批通过（`credit_used` 涨了） | 不含它的占用 | 已占额度更大 |
| 财务把该客户额度调低 | 旧额度 | 新额度 |
| 管理员把策略从 `WARN` 改成 `BLOCK` | WARN 已放行 | 该拦 |

审批通常隔几小时到几天，这三件事在这段时间里都可能发生。

## 三个执行点（同一份 `credit::decide`）

| # | 执行点 | 入口 | `BLOCK` | `WARN` | `PASS` |
|---|---|---|---|---|---|
| ① | 提交（进 `SUBMITTED` / 免审直过） | `sales_order_handler::ensure_submittable`（M3-09） | 409，单据留 `DRAFT` | 放行 + 响应文案 + 台账 | 静默放行 |
| ② | 审批点「同意」之前（**事务之前**） | `sales_approval_bridge::guard_credit_on_approve`（本卡） | 409 + `10003`，**待办与单据都不动** | 放行（不写台账） | 放行 |
| ③ | 审批结论回写单据（占用真正发生） | `sales_approval_bridge::settle`（本卡） | 不落 `APPROVED`，**`SUBMITTED → DRAFT`** + 原因写 `remark` | 落 `APPROVED` + 台账 + WARN 日志 | 落 `APPROVED` 不留痕 |

- ② 的意义：给审批人一句能照做的拒绝（五个数 + 三条出路），且**什么都没生效** —— 调额 / 收款 /
  改策略后再点同意，或直接驳回改单。
- ③ 的意义：② 到 ③ 之间仍有毫秒级窗口（并发审批 / 回调重放 / 别的入口直接调回调），「占钱」这件事
  必须由**写状态的那一次**自己兜住。
- ② 不在审批点写台账：审批可能紧接着被驳回 / 终止，写了就是「已放行」的假记录。台账一律在 ③
  真落 `APPROVED` 之后写，且 `note_over_limit` 幂等（提交时已写过同一张单 → 0 行）。

## 为什么 ③ 打回草稿而不是停在 `SUBMITTED`（约定 11）

审批单已经 `APPROVED` 且不可回滚。若单据停在 `SUBMITTED`：没有待办、没有接口能推它 →
**只能改库**，这正是 AGENTS.md 约定 11 明令禁止的形态。打回 `DRAFT` 后用户可继续：
收款 / 调额 / 改策略后**重新提交审批**（`approval_instance_id` 被清空，不会撞
`uk_biz_flow_instance_biz_active`；drill 里有一条断言专门验这一步）。

## 为什么经 `doc_reader` 注册表（而不是 handler 里直调 ERP 桥）

架构门禁 `business::flow::doc_reader_tests::flow_erp_reference_budget_is_not_exceeded`
钉死 **`flow` → `erp` 直连引用 = 0**。给 `DocReader` 加了 `guard_approve(task_id)`（**默认
`Ok(())`**：绝大多数审批线没有「通过即占硬额度」这门生意），`ErpDoc` 的 18 个变体里**只有
`SoApprove`** 接额度闸；新增变体时 `match` 不全会编译失败，逼着人想一遍。

## 改动文件

| 文件 | 改了什么 |
|---|---|
| `backend/app/src/business/erp/credit.rs` | 新增 `OrderCredit` + `assess_order`（单据级取数 + 判定，一处可读）：`used` 读缓存列、`pending` 排除本单、`limit=0` 省两条聚合；三份文案（409 / 打回原因 / WARN 提示）；模块头补「三个执行点」表 |
| `backend/app/src/business/erp/sales/sales_approval_bridge.rs` | 新增 `guard_credit_on_approve`（审批前闸，409 + `10003`）；`settle` 在写状态前按策略再判，`BLOCK` → 打回草稿（`compensation_reason`），`WARN` → 落 `APPROVED` 后写台账 + WARN 日志；+5 条真库用例 |
| `backend/app/src/business/flow/doc_reader.rs` | trait 新增 `guard_approve`（默认放行）+ 扇出 `guard_approve_for_task` |
| `backend/app/src/business/erp/doc_reader_impl.rs` | `ErpDoc::guard_approve`：只有 `SoApprove` 接闸，其余 17 条显式放行 |
| `backend/app/src/business/flow/handler.rs` | `approve_task` 在事务之前调 `doc_reader::guard_approve_for_task`；文档补 409 口径 |
| `backend/app/src/business/flow/doc_reader_tests.rs` | +1 条扇出单测（空注册表放行 / 每个实现都被调到 / 拒绝原样透传 409） |
| `scripts/verify/m311_credit_block_approval.sh` | 29 断言：迁移补齐 + 接线静态断言 + 死权限码闸 + 4 组真库用例（SKIP=0）+ 清场 |

**不加迁移**（无 DDL，下一个可用号仍是 `V0438`）；**不造新权限码**（4 个调用点都在既有权限后面）；
**不动前端**（M3-10 的额度胶囊已按 `verdict` 分别提示「提交会被拒 / 可提交但记台账 / 按策略放行」，
审批侧 409 由审批中心统一 toast）。

## 验证（全部真实执行）

| 项 | 结果 |
|---|---|
| `cargo fmt --check` | 0 差异（只格式化 `cloudpivot` 包，未触 vendor） |
| `cargo clippy --lib -- -D warnings` | 0 warning（见 `01-backend-lint.out`） |
| `bash scripts/verify/m311_credit_block_approval.sh` | **ALL PASS，PASS=29 FAIL=0**（见 `00-drill.out`） |
| └ `business::erp::credit` | 30 条全绿，SKIP=0（新增 `scenario_assess_order_matches_submit_guard_reads`） |
| └ `business::erp::sales::sales_approval_bridge` | 14 条全绿，SKIP=0（新增 5 条：前闸 409·待办不动 / WARN 台账 / PASS 静默 / 过审兜底打回草稿+可重提 / 注册表 17 条非销售线放行） |
| └ `business::erp::sales` | 211 条全绿（提交守卫 / 报价单 / 退货 / 审批桥不被打破） |
| └ `business::flow` | 433 条全绿（**含架构门禁 flow→erp=0** 与 `guard_approve_for_task` 扇出） |
| 演练库 | 145 上 `cp_m311%` 残留 0（脚本自 DROP）；夹具在本库清尾后 0 残留 |

演练库按卡面约束建在 **145（`172.18.10.145:5432`，用户 `cloudpivot`）**，库名含 `drill`；
模板库 `cloudpivot_test` 落后 33 条业务迁移，脚本按 `V0405…V0437` 逐条幂等补齐（与 M3-10 同法）。

## 已知边界（不是漏了）

1. **客户冻结**在审批通过时不重复判：冻结是比额度更硬的闸，提交守卫（`require_active_customer`）
   已经拦过；本卡的评估只如实带出 `customer_status`，不重复报错（否则同一件事两处报错、文案打架）。
   冻结发生在「提交后、审批前」时，审批仍会通过 —— 属于「客户档案冻结」卡的口径，不在本卡。
2. **评估查询失败**时 `settle` 按放行处理（WARN 日志）：额度判定不该把审批回调打死（回调失败只会
   留死单）；真出问题由每日兜底重算（`erp_credit_recompute`）+ 不变量巡检兜。
3. **并发两单同时审批**不需要客户行锁：本单从 `pending`（`SUBMITTED`）搬进 `used`（`APPROVED`）时
   **金额不变**，两单各自看到的合计完全相等，不存在「都以为没超」的窗口（推导见 `credit.rs` 模块注释）。
4. `WRITTEN_OFF`（坏账）/ `PARTIAL_OUT → CLOSED` 的口径边界沿用 `credit.rs` 既有说明，本卡未改。

## 与后续卡的衔接

- **M3-12（收款核销后释放）**：直接调 `credit::sync(cause=AR_SETTLED)`，与本卡无耦合。
- **M3-13（信用单测：并发下单 / 部分收款）**：本卡已覆盖「审批期额度被占满 → 拦截 / 打回」；
  并发用例可直接复用 `assess_order` 的中间量断言。
- **M3-14（客户信用页）**：直接渲染 `GET /business/erp/credit/{customer_id}` 的
  `occupied / available / open_receivable`；页面上的「超限策略」列读 `credit_policy`。
