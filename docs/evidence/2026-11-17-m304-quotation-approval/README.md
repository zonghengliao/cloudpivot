# M3-04 报价单：审批流接线（走 flow，不硬编码）· 交付证据

- **卡**：`t_6bede75a`（M3-04【M3·销售】报价单：审批流接线）
- **上游**：M3-01（V0432 建表）/ M3-02（V0433 五个权限码 + 状态机）/ M3-03（V0434 转单）
- **日期**：2026-11-17
- **结论**：报价单「发出」前必须走 `quotation_approve` 流程（审批不是状态），
  审批通过回调**自动发出**；金额分级写在**流程定义的条件边**上，代码里没有一个金额 if/else。

## 一、口径（先看这段，再看代码）

```text
DRAFT ──提交审批──▶ [biz_flow_instance: QUOTATION_APPROVE / PENDING]
  │                        ├── 审批通过 ──▶ SENT（回调**自动发出**，写状态日志）
  │                        └── 驳回（退发起人）──▶ 仍是 DRAFT，发起人拿到 RESUBMIT 待办
  │                                            改完点「重新提交」继续同一轮
  │                        └── 终态驳回 / 撤回 / 终止 ──▶ 仍是 DRAFT（可重提，新实例）
  └── 直接 PATCH status=SENT ──▶ 409 + 66088（必须先有 APPROVED 实例）
```

### 1. 为什么「审批不是一个状态」（不动 `ck_biz_quotation_status`）

`biz_quotation.status` 的五值（`DRAFT` / `SENT` / `ACCEPTED` / `REJECTED` / `CANCELLED`）
是**客户那边进行到哪一步**的事实；审批是「这张价能不能对外报」的**前置条件**。混在一个字段里：

- `SENT` 会同时表示「客户收到了」与「内部批过了」，报表拆不出来；
- 要改 `ck_biz_quotation_status`（DROP/ADD CHECK），风险面大于收益；
- 与收款退款（`RECEIPT_REFUND`，t_b9840c8d 路线 ②）、凭证红冲（`VOUCHER_REVERSE`）的口径分叉。

**代价如实写明**：审批在跑时 `status` 仍是 `DRAFT`。前端不能只看 `status` 判断「已提交待批」，
要读详情 `approval` 段（`instance_no` / `status` / `pending_tasks`）或列表的 `approval_instance_id`。

### 2. 为什么通过后**自动发出**，而不是留给人再点一次

「批了但没人点发出」= 内部批过、客户从没收到：单据看着是草稿、销售以为报出去了。
本仓口径是回调直接执行那个不可逆动作（与 `payable_bridge` 通过即记账、
`receipt_refund_bridge` 通过即过账同构）。发出本体与人工 `PATCH /status` 是**同一条代码路径**
（`change_status_with` → 三道守卫 → 审批闸 → 状态机留痕 → CAS），不另写一套。

### 3. 金额分级在流程定义里，不在代码里

`V0435` 种的标准流程 `quotation_approve`：

```text
START ─▶ 销售主管审批（ROLE SALES_SUPERVISOR，ANY）
            └─▶ 条件 c_amt
                  ├── form.amount < 100000  ──▶ END
                  └── 兜底（is_default）    ──▶ 财务经理复核（ROLE FINANCE_MANAGER）──▶ END
```

`form.amount` = **价税合计**（客户看到的那个数，也是对外的承诺额），由
`quotation_approval_bridge::form_data` 冻结进 `biz_flow_instance.form_data`，且必须是
**JSON 数字**（字符串会让 `cmp` 判 `TypeMismatch` 并静默掉进兜底边 —— 与收款退款同一个坑）。

客户要改阈值 / 换审批岗位：在流程定义里另存版本即可，**不改代码、不发版**。

## 二、改动清单

| 文件 | 改了什么 |
|---|---|
| `backend/app/migrations/business/V0435__quotation_approval_flow.up.sql`（新） | ① `biz_quotation` 加 `approval_instance_id` + 部分索引（`IF NOT EXISTS`）；② 种 `quotation_approve` 流程（定义 / 版本 / 5 节点 / 5 边 / 2 审批人）；③ 7 段自检（含 hash、条件边阈值、兜底边、END OR 汇聚、START 直连） |
| `.../V0435__quotation_approval_flow.down.sql`（新） | 反序清理（实例 / 待办 → 图 → 版本 → 定义 → 列）；**不动 `status`**（发出是既成事实，回滚迁移不编造历史） |
| `backend/app/src/business/erp/sales/quotation_approval_bridge.rs`（新） | 桥：`published_quotation_flow` / `approved_instance_of`（发出闸）/ `latest_approval_of` / `held_by_approver` / `pending_resubmit_of` / `cancel_approval_of` / `on_quotation_settled`（回调）/ `form_data` / `guard_self_approval` / `internal_control_creator` + 5 条纯逻辑单测 |
| `.../sales/quotation_handler.rs` | 新端点 `POST /{id}/submit-approval`（复用 `quotation:status` 码）；发出闸（`DRAFT→SENT` 需 APPROVED 实例）；审批中锁（改 / 删 / 换状态）；作废同事务撤审批；`issue_after_approval`（回调执行体）；详情出参加 `approval` 段与 `approval_instance_id` |
| `.../sales/quotation_model.rs` | `QuotationRow.approval_instance_id` + `COLUMNS` + `apply_approval_instance`（回写指针） |
| `.../sales/mod.rs` | 声明新模块 |
| `backend/app/src/business/erp/doc_reader_impl.rs` | 注册第 18 个 `DocReader`（`QUOTATION_APPROVE` → `biz_quotation` / `quote_no` / `status` / `create_by`），settle / guard / creator 三条委派 |
| `backend/app/src/business/erp/doc_reader_impl_tests.rs` | 受控登记表 16→17、实现数 17→18（`bridge_codes` 加报价单那条） |
| `backend/app/src/business/erp/flow_biz_type_contract_tests.rs` | `quotation_approval_bridge::BIZ_TYPE` 进「必须登记在词表」清单 |
| `backend/app/src/business/flow/internal_control.rs` + `flow/doc_reader_tests.rs` | `GOVERNED_CODES` 16→17（制单人不可自审自己报的价） |
| `backend/crates/cloudpivot-doc/src/lib.rs` | `FlowBizType::QuotationApprove`（`ALL` 24→25 / `as_str` / `flow_code` / `label` / `doc_kind`）；`DocKind::Quotation` 挪进 `is_document()`（M3-02 留的收口项）；`FLOW_BIZ_TYPE_VALUES` 24→25 |
| `frontend/src/api/business/erp/doc-kinds.ts` | `FLOW_BIZ_TYPES` / `FLOW_BIZ_TYPE_LABELS` / `FLOW_BIZ_TYPE_FLOW_CODES` 三个镜像同步（Rust `contract_tests` 逐字比对） |
| `backend/crates/cloudpivot-kernel/src/shared/error_code.rs` | 新错误码 `ERP_QUOTATION_APPROVAL_REQUIRED = 66088`（+ 码表自检一行） |
| `scripts/verify/m304_quotation_approval_e2e.sh` / `.py`（新） | 一条链路的验收（见下） |
| `scripts/verify/m304_rows_hash.py`（新） | `p22b_rows_hash.py` 的「读环境变量」版本（原脚本把 `-h 127.0.0.1 -U alun` 写死，99 上跑不了） |

**没有新增权限码、没有新增菜单行**：提交审批复用 `business:erp:quotation:status`
（与收款退款 V0297、销售订单 SO-06 同一口径 —— 提交审批是「推进这张单」的动作，
另开码要多一行菜单却没有独立授权语义）。所以 `perm_menu_gate` / `menu_invariants` 不受影响。

## 三、验收（全部实跑，输出见下方命令）

```bash
# ① 一条链路的 E2E（空库首启，真接口 + 真库 + 真角色）
SKIP_BUILD=1 bash scripts/verify/m304_quotation_approval_e2e.sh
#    → PASS=29 FAIL=0（73 条真接口断言 + V0435 形态 9 条 + 落库形态 8 条 + menu_invariants 8 条）
```

E2E 覆盖的 10 个场景（每个都是「真角色拿真待办」，不是超管短路）：

| # | 场景 | 关键判据 |
|:--:|---|---|
| ① | 小额单提交审批 | 单据仍 `DRAFT`；`approval.status=PENDING`；`instance_no=FLW-…`；`pending_tasks=1`；列表行带 `approval_instance_id`（详情才有摘要，防 N+1） |
| ② | 审批中的四道闸 | `PATCH status=SENT` / `PUT` / `DELETE` / 重复提交 **全部 409**，报文点名审批单号 |
| ③ | 内控（制单人不可自审） | 制单员**也在**销售主管角色里，但他的待办为 0、库里落 `SKIPPED` + 原因；销售主管待办恰好 1 条 |
| ④ | 通过 → 自动发出 | 单据 `SENT`；`biz_status_log.reason` = 「审批通过（FLW-…）自动发出」；待办清零 |
| ⑤ | 大额两档（≥10 万） | 销售主管批完**仍是 `DRAFT`**（条件边送进财务）；财务批完才 `SENT` |
| ⑥ | 驳回「退发起人」 | 单据 `DRAFT` 且**可改**（不被锁死）；发起人拿 `RESUBMIT` 待办；此时走「提交审批」409 并指向重新提交；重新提交后继续同一轮，批完 `SENT` |
| ⑥' | 终态驳回 | 实例 `REJECTED`；单据 `DRAFT` 可改可重提（**新实例**） |
| ⑦ | 作废撤审批 | `DRAFT→CANCELLED` 200；在跑实例被**同一事务**终止；无孤儿待办 |
| ⑧ | 没配流程 | 默认 409 + `66033`；带 skip 但理由太短 400；持 `business:erp:skip-approval` + 理由 ≥10 字 → **直接发出**且理由进状态日志；流程恢复后带 skip 一律 400 |
| ⑨⑩ | 入口守卫 | 已发出的再提交 409；非法 uuid 400；不存在 404 |

```bash
# ② 离线单测（全库）
cargo test --lib -p cloudpivot                    # 5910 passed / 1 failed（唯一失败＝存量 o13 现金流底稿，见 t_0fd37353）
cargo test --lib -p cloudpivot-doc                # 19 passed（词表 / 前端镜像 / 巡检契约）
# ③ 真库单测（库 cp_m304_ut，服务跑完 435 条迁移后手工建的演练库）
ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m304_ut \
  cargo test --lib -p cloudpivot business::erp::sales -- --test-threads=1        # 156 passed（SKIP 0）
ERP_DB_URL=… cargo test --lib -p cloudpivot business::erp::doc_reader_impl -- --test-threads=1   # 16 passed
ERP_DB_URL=… cargo test --lib -p cloudpivot business::flow:: -- --test-threads=1                 # 432 passed
ERP_DB_URL=… cargo test --lib -p cloudpivot system::menu                                       # 22 passed
# ④ 工程门禁
cd backend && cargo fmt -p cloudpivot -p cloudpivot-doc -p cloudpivot-kernel
cd backend && cargo clippy --lib -p cloudpivot -p cloudpivot-doc -p cloudpivot-kernel -- -D warnings   # clean
bash scripts/check.sh --fast                     # L0 + L1 全部通过
cd frontend && pnpm typecheck && pnpm build      # 都通过
# ⑤ 权限/菜单闸
NIK_DATABASE_*=<cp_m304_ut> python3 scripts/verify/perm_menu_gate.py    # PASS=***
NIK_DATABASE_*=<cp_m304_ut> python3 scripts/verify/orphan_button_gate.py # 1 条存量（FINANCE_DIRECTOR receivable:aging，V0418/V0425，非本卡）
bash scripts/verify/menu_invariants.sh cp_m304_ut                        # 8 条全绿
```

## 四、实测踩到的坑（写给下一个人）

1. **`biz_instance_running` 不是「能不能改」的判据**。驳回选「退发起人」后实例**仍是 `PENDING`**，
   票在发起人自己手里（`RESUBMIT` 待办）。若按「实例在跑」拦编辑，发起人既改不了也提交不了
   ——「退回发起人」直接变成死锁，只能改库。所以本卡的判据是
   `quotation_approval_bridge::held_by_approver`（有 `PENDING` 的 `APPROVAL` 待办）。
2. **内控闸必须放过 `RESUBMIT` 待办**。`guard_self_approval` 拿到的若是发起人自己的
   「重新提交」待办（`task_kind = RESUBMIT`），那不是「给自己的价投票」，放行；
   不放过 = 同样把「退回发起人」锁死。
3. **`p22b_rows_hash.py` 把 `-h 127.0.0.1 -U alun` 写死**（99 上 PG 已卸载、用户已换）。
   直接拿它在 99 上跑会静默失败（`2>/dev/null` 一裹就看不出），于是出现
   「明明 hash 对却报不一致」的假红。本卡另存 `m304_rows_hash.py`（只改连接参数）。
4. **夹具账号的口令要在 admin 改密之后再同步**：脚本用 `(SELECT password FROM sys_user WHERE
   username='admin')` 建夹具账号时，admin 还是初始口令；python 里走真接口改密后必须再 SQL
   复制一次哈希，否则夹具账号登录失败（首轮 E2E 就死在这）。
5. **`return_to` 没有 `START` 这个值**：`PREV` / `INITIATOR` / `NONE`（`FINAL`）/ 节点 key；
   写 `START` 会被当成「节点 key」→ `needs_admin()` → 403。
6. **`psql -tA` 打 `boolean` 出来是 `true/false`**（不是 `t/f`）：断言里别写 `t`。

## 五、留给下游

- **M3-05（报价单单测）/ M3-06（前端列表详情页）**：详情 `approval` 段与
  `approval_instance_id` 已就位；列表页判断「待批」请看 `approval_instance_id`，
  **不要**看 `status`（审批不是状态）。提交审批按钮挂 `business:erp:quotation:status` 码。
- **附件 / 提醒**：`DocKind::Quotation` 本轮只挪进 `is_document()`（可审批 / 可作废），
  `ATTACHMENT_DOC_KINDS` / `REMINDER_DOC_KINDS` **没动** —— 两张清单是两件事，
  要做「报价单挂附件」得单开卡（还要动 `ck_biz_doc_attachment_type`）。
- **外币报价**：报价单仍无币种列；转单按本位币（M3-03 的决定）。外币报价是另一个决定。
- **`scripts/verify/README.md` 的脚本清单行没加**：该文件此刻正被其它卡改动，
  共享工作区里逐 hunk 提交风险大于收益，合并后补一行即可（与 M3-03 同处理）。

## 六、清理

- 演练库 `cp_m304_approval_e2e_*` 由脚本自己 `dropdb`；真库单测库 `cp_m304_ut` 用完即删；
- 无残留 `cloudpivot` 进程（实例由脚本 `kill` 并等待退出）；
- `git add` 只按路径加本卡 18 个文件（未夹带其它 worker 的 `README.md` / `frontend/e2e` /
  `scripts/verify/README.md` / `menu_invariants.sh` 等改动）。
