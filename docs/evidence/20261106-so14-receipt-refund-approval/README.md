# 【SO-14 遗留 · D10 补】收款退款单接入审批流（与 PAYABLE_PAY 同级，过账前置）交付证据

- 卡：`t_b9840c8d`（assignee backend-developer），父卡 `t_573bfe5b`（SO-14 遗留：收款退款单，路线 A）
- 日期：2026-11-06
- 结论：**退款审批流已接入并端到端实测通过**。审批是 `CONFIRMED → POSTED` 的**前置条件**，
  不是退款单的一个状态；审批通过后**回调直接过账**（不需要人再点一次）。
- 本目录即复跑入口：`README.md`（口径 + 验收）、`curl-e2e.sh`（31 断言）、
  `curl-round1.txt` / `curl-round2.txt`、`unit-tests.log`（真库单测 19/19）。

---

## 一、口径决定（先定再写）

| # | 问题 | 决定 | 理由（可核对） |
|---|---|---|---|
| D10-1 | 审批点放哪 | **过账前**（`receipt_refund::change_status` 的 `CONFIRMED → POSTED`） | 退款是**真出钱**（借 1122 应收 / 贷 1002 银行）；钱出去那一刻才是风险点，确认（`CONFIRMED`）只是财务认了这笔退款。 |
| D10-2 | 路线 ①（新增 `PENDING_APPROVE` 状态）还是路线 ②（不改状态域） | **路线 ②** | ① 要 `DROP/ADD ck_biz_receipt_refund_status` 并重画状态机；更关键的是语义：资金单据的状态域回答「钱有没有动」（草稿 / 已确认 / 已过账 / 已作废），审批回答「这笔钱能不能动」。混在一个字段里，「已确认但没批」与「批了没记账」会挤成同一个值，状态日志读不出差别，报表还得再拆一次。② 与既有**凭证红冲审批**（`VOUCHER_REVERSE`：被审对象状态一个都不变，批准才执行不可逆动作）同思路。 |
| D10-3 | 判据是什么 | `biz_flow_instance` 存在 `biz_type='RECEIPT_REFUND' AND biz_id=退款单 id AND status='APPROVED'` | 只认「有过已通过的实例」，**不认最近一条实例的状态**：驳回后重提的场景里最近一条可能是 `PENDING`，而更早那条是 `APPROVED`（钱已经被批过）。宁可放过重复提交，也不要让已经批过的款过不了账。 |
| D10-4 | 审批通过后谁过账 | **回调那一步直接过账**（`receipt_refund_bridge::on_refund_settled` → `post_after_approval` → 与人工过账**同一条代码路径** `change_status`） | 让「审批过了还要人再点一次过账」= 没人点就「审批过了但没记账」，单据显示已确认、账上没有凭证，典型静默漏账。与 `payable_bridge::settle`（通过即记账）同构。 |
| D10-5 | 回调失败（如期间已结账）怎么办 | 退款单留在 `CONFIRMED`，实例是 `APPROVED`；修好阻塞原因后**人工 `PATCH status=POSTED` 即可补记**（过账闸对 APPROVED 实例放行） | 回调不回滚审批单（与全仓口径一致）；但必须留一条**可恢复**的人工补记路径，否则出现「批过了永远不能记账」的死局。e2e 的 ⑥ 就是这个场景的闸门验证。 |
| D10-6 | 没配流程怎么办 | 409 + `66033`（`APPROVAL_FLOW_NOT_CONFIGURED`，fail-closed）；持 `business:erp:skip-approval` + 理由 ≥10 字可**显式跳过并直接过账**（理由写 `biz_status_log`） | 与 SO-06 / P17 同一套 `approval_gate` 口径。跳过是**内部参数**（`change_status_with(..., approval_override)`），HTTP 入口永远传 `None` —— 换个 URL 绕不过过账闸。 |
| D10-7 | 内控（制单人不能批自己） | 两处都挂：`flow::internal_control`（**生成待办前**排除制单人，落 `SKIPPED`）+ `approve_task` 的 `guard_self_approval`（403 + 65019） | 与采购 / 收货 / 付款 / 凭证红冲 / 销售订单一致，共用开关 `erp.separation_of_duties.approve`（默认开）。只挂 403 会让「制单人看到自己的待办」变成日常骚扰；只挂排除会被转办绕过。 |
| D10-8 | 会签口径 | 单节点「财务经理审批」`approval_mode='ANY'` | 财务经理岗位可能多人 / 兜底超管可能多个，**任一人复核即可**；写 `ALL` 会让「多一个超管兜底」变成「所有人必须都批」，单节点流程永远等不齐（实测：共用库两个超管 → 一票之后实例仍是 `PENDING`）。 |
| D10-9 | 提交审批要不要新权限码 | **不新开**，复用 `business:erp:receipt-refund:status` | 与 SO-06 的 `POST /sales-order/{id}/submit-approval` 复用 `:sale-order:status` 同一口径：提交审批是「推进单据状态」的动作。复用同时让 `perm_menu_gate`（闸⑥）与 `orphan_button_gate`（闸⑦）无需新增菜单行。 |

### 与父卡（D10 「本卡不接审批引擎」）的关系

父卡的 D10 是**范围决定**（一张卡别同时做资金域与流程域），本卡是它的**补丁**：
不动退款单结构、不动核销算法、不动 `POSTED` 终态语义、不动 `CANCELLED ⇔ 一分没收`。

---

## 二、改动清单

### 迁移（`backend/migrations/business/`）

| 文件 | 内容 |
|---|---|
| `V0297__sales_receipt_refund_approval.up.sql` | 种流程 `receipt_refund`（`biz_type='RECEIPT_REFUND'`）：`biz_flow_def`（PUBLISHED / is_default / is_system）+ `biz_flow_def_version`（`graph_hash=6a592107a9e7e6bc34a47bc4098926f648eb15e4fb3a5b45c1f93354dfc2d1b2`）+ 3 节点（START / 财务经理审批 ROLE·ANY·FALLBACK_ADMIN / END OR 汇聚）+ 2 边 + 1 条审批人（ROLE → `FINANCE_MANAGER`）；末尾 DO 块自检条数 / hash / 会签口径 / 兜底策略 / END 汇聚 / START 直连；三层 `WHERE NOT EXISTS` 守卫（已有同名流程 / 同 biz_type 默认流程 / 角色不存在 → 整条跳过） |
| `V0297__sales_receipt_refund_approval.down.sql` | 有**在跑/已结束实例**引用该定义 → 主动拒绝（`biz_flow_instance.def_id` 是 RESTRICT，报错文案难懂）；否则按 子表→定义 顺序删固定 uuid |
| `V0296__sales_receipt_menu_page.up/down.sql` | **只改号（V0294 → V0296）与头注释，内容一字未动**，原因见下节 |

**号段（开工实测 + 撞号修正）**

开工时 `ls backend/migrations/business | sort -V | tail`：`V0294__sales_receipt_refund` /
`V0295__sales_receipt_refund_permissions`（父卡，**先一步**推到 main `c2247e9`）/
`V0296__sales_receipt_menu_page`（SO-18 卡，本地 main）。本卡顺延取 **V0297**。

> ⚠ 合并时发现：SO-18 的 `V0294__sales_receipt_menu_page` 与父卡 `V0294__sales_receipt_refund`
> **同号**（两条 V0294 同时存在会让引擎**启动即失败**：`alun-db` 的 `check_identity_conflicts`
> 对「同一 kind 重复 version」直接报错，不是告警）。已推到 main 的那条不动，
> 未推送的 SO-18 那条改号为 **V0296**（`git mv` + 文件内 `V0294` 字样同步，SQL 一行未改）。
>
> ⚠ 另：SO-19 在工作区里有一个**未提交**的 `V0295__sales_invoice`（与本行 295
> `sales_receipt_refund_permissions` 同号），本卡未触碰该文件，已在其卡评论区告警 ——
> 它提交前必须改号（建议 V0298）。

### 后端（`backend/src/business/`）

| 文件 | 内容 |
|---|---|
| `erp/receipt_refund_bridge.rs`（**新增**，~560 行） | 审批桥：`BIZ_TYPE`/`FLOW_CODE`、`published_refund_flow()`、`latest_approval_of()`（详情页审批段）、`approved_instance_of()`（**过账闸判据**）、`form_data()`（`amount` 是 JSON 数字）、`RefundVerdict` + `on_refund_settled()`、`internal_control_creator()`、`guard_self_approval()`；6 条单测（纯函数） |
| `erp/receipt_refund.rs` | ① `RefundRow` 加 `create_by`（内控与表单快照要用）；② `RefundDetailRes` 加 `approval: Option<RefundApprovalRes>`（实例号 / 状态 / 待办数）；③ `change_status` 拆出 `change_status_with(approval_override)`：`CONFIRMED → POSTED` 前查 `approved_instance_of`，无 → **409 + 66044**（报文给 `submit-approval` 入口）；④ `post_after_approval()`（回调过账执行体，`PostOutcome::{Posted, AlreadyPosted, NotPostable}`）；⑤ `submit_approval()` + `start_approval()`（前置：`CONFIRMED` + 核销满 + 无在跑实例；有流程走 `engine::start`，没流程走 skip）；⑥ 新 handler `receipt_refund_submit_approval`；⑦ 单测 +7（3 条真库/审批链） |
| `shared/doc_kind.rs` | `FlowBizType::ReceiptRefund`（`ALL` 15→16、`as_str`/`flow_code`/`label`/`doc_kind` 四挂）+ `FLOW_BIZ_TYPE_VALUES` 15→16。`HEALTH_CHECKED` / `FLOW_TYPES` **故意不动**（绑着已应用的 V0202 自检块，改历史迁移会 checksum 失败） |
| `flow/handler.rs` | 撤回 / 同意 / 驳回三处挂 `receipt_refund_bridge::on_refund_settled`；`approve_task` 挂 `guard_self_approval` |
| `flow/internal_control.rs` | `biz_type == RECEIPT_REFUND` 分支：按退款单 `create_by` 排除制单人 |
| `flow/linkage/spec.rs` | `TRIGGER_EXEMPT` 补 `ENTITY_RECEIPT`（SO-17 遗留）与 `ENTITY_RECEIPT_REFUND`：两张资金单据的状态流转**不调联动引擎**（退款凭证在过账那一步生成、应收冲减在 `allocate` 落账），不登记就是 `flow::linkage::tests::triggers_cover_state_machine_entities` 红（实测合并树上这条本来就是红的，SO-17 的 `RECEIPT` 也漏了） |
| `shared/error_code.rs` | 新增 `ERP_RECEIPT_REFUND_APPROVAL_REQUIRED = 66044`（+ 66xxx 段码值稳定性清单） |
| `erp/receipt.rs` | **顺手修掉 SO-17 遗留的 11 条 `collapsible_if`**（`cargo clippy --fix` 机械建议，纯 let-chain 收敛、无行为变化）—— 否则全仓 `clippy -D warnings` 门禁是红的（父卡已在其证据里登记此遗留） |
| `docs/architecture/snapshot-drift/10_checks.sql` | `FLOW_BIZ` 登记一行：`RECEIPT_REFUND → biz_receipt_refund`（DI-19 的「流程实例业务单据悬空」从此覆盖退款） |
| `frontend/src/api/business/erp/doc-kinds.ts` | `FLOW_BIZ_TYPES` / `FLOW_BIZ_TYPE_LABELS` / `FLOW_BIZ_TYPE_FLOW_CODES` 三处同步（`doc_kind_tests` 的前端镜像用例逐字对拍） |

### 接口（权限码与 V0295 的 `sys_menu.auth_code` 逐字一致）

```text
POST /business/erp/receipt-refund/{id}/submit-approval   business:erp:receipt-refund:status
     CONFIRMED + 核销满 → 发起 receipt_refund 实例（退款单状态不变）
     有流程 → 201/200 + approval{PENDING, pending_tasks}；没流程 → 409 66033（或 skip 直付）
PATCH /business/erp/receipt-refund/{id}/status           business:erp:receipt-refund:status
     CONFIRMED → POSTED：必须先有 APPROVED 实例，否则 409 66044
GET  /business/erp/receipt-refund/{id}                   business:erp:receipt-refund:detail
     新增 approval 段（最近一条实例：instance_no / status / pending_tasks）
```

审批动作本身走既有流程引擎，**零新增接口**：`GET /business/flow/instance/{id}`（待办）、
`POST /business/flow/task/{id}/approve`、`/reject`、`POST /business/flow/instance/{id}/withdraw`。

---

## 三、端到端链路（curl 实测覆盖）

```text
① 未审批直接过账            → 409 + 66044（报文含 submit-approval 入口）
② 提交审批                  → 200；退款单仍 CONFIRMED；实例 PENDING；1 条待办
③ 审批中重复提交            → 409（10003）
④ 审批人（非制单人）看到待办；制单人拿不到待办（内控在生成待办时就排除）
⑤ 同意                      → 实例 APPROVED → 回调**直接过账**：
                              退款单 POSTED + 凭证 借 1122 / 贷 1002 各 100 + 状态日志「审批通过（FLW-…）自动过账」
                              应收回到 OPEN / 已收 0
⑥ 已过账再作废              → 409（终态语义没动）
⑦ 第二张单驳回              → 仍是 CONFIRMED、0 张凭证、可重新提交（新实例 PENDING）
```

---

## 四、门禁

| 门禁 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt -- --check` | **rc=0** |
| 静态 | `cargo clippy --all-targets --features test-hooks -- -D warnings` | **0 告警**（含顺手修掉的 SO-17 `receipt.rs` 11 条 `collapsible_if`） |
| 编译 | `cargo check --all-targets --features test-hooks` | rc=0 |
| 单测（真库） | `ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_rr_ap cargo test --lib business::erp::receipt_refund --features test-hooks -- --nocapture --test-threads=1` | **19 passed / 0 failed**（无 SKIP；含 4 条真库用例） |
| 全量 lib 回归（**部分**，见下） | `ERP_DB_URL=… cargo test --lib --features test-hooks -- --test-threads=1` | 跑到 359 条时 0 failed（DB 用例慢，为赶交付窗口主动中止；`full-suite-partial.log`）。**受影响模块单独跑满**：`business::flow::` **389 passed / 0 failed**、`business::erp::receipt::` **11 passed**、`business::erp::receipt_refund::` **19 passed**、`business::shared::doc_kind::doc_kind_tests` **21 passed** |
| 迁移（全新库） | 空库 `cloudpivot_rr_ap` + 引擎启动 | 一次启动应用 **225 个迁移**，V0294/295/296/297 `success=t`，V0297 自检 NOTICE 通过 |
| 图 hash ↔ 行一致 | `receipt_refund::tests::seeded_flow_graph_hash_matches_its_rows` | `load_graph(version).graph_hash()` == 库里 `graph_hash`（`flow::maintain` 每日自检不会报漂移） |
| 前端词表镜像 | `business::shared::doc_kind::doc_kind_tests` | **21 passed**（含 `frontend_ts_mirror_matches_rust` / `frontend_ts_labels_match_rust`） |
| 真实 token curl | `bash docs/evidence/20261106-so14-receipt-refund-approval/curl-e2e.sh` | 连跑两轮 **31/31 断言 PASS**（`curl-round1.txt` / `curl-round2.txt`） |
| 死权限码 | `python3 scripts/verify/perm_menu_gate.py`（指向本卡隔离库） | **闸：PASS**（复用 `:status`，无新增码） |
| 闸②（数据权限） | `python3 scripts/lint/scope_gate.py --root .` | 本卡**零新增违规**（`receipt_refund_bridge.rs` 每条 SQL 都有 `SCOPE-EXEMPT` 理由）；仅剩既有 `shipment_page_fill_tests.rs` SKIP_MANIFEST 一条 FAIL（父卡已记，与本卡无关） |

真库单测里的审批相关用例（非 SKIP）：

- `refund_approval_chain_posts_on_approve_and_stays_confirmed_on_reject`
  —— 真引擎全链：登记 → 核销 → 确认 → 提交审批 → 待办（**制单人被内控排除**）→ 同意 →
  回调**自动过账**（状态 / 凭证 / 状态日志 / 去重回调幂等）→ 第二张单驳回 → 仍 `CONFIRMED`、
  0 张凭证 → 驳回后可重新提交。
- `refund_restores_receivable_to_open_and_posts_voucher`
  —— 增加「未审批过账 409 + 66044」与「造 APPROVED 实例后人工过账（状态日志带审批单号）」。
- `seeded_flow_graph_hash_matches_its_rows` —— V0297 种子的快照 / hash 与节点边行一致。

---

## 五、复跑环境

实例：`/var/tmp/t_b9840c8d-run/cloudpivot` @ `127.0.0.1:18131`，库 `cloudpivot_rr_ap`
（全新库，引擎迁到 V0297）。工作树 `/var/tmp/t_b9840c8d-wt`（分支 `fix/t_b9840c8d`）。

```bash
# 库
su postgres -c "createdb -O alun cloudpivot_rr_ap"

# 实例（run 目录自带 config/：config.toml + config-test.toml；test profile 必须开认证层，
# 否则 #[permission] 拿不到 claims → 业务接口 403 / 改密接口 500）
bash /var/tmp/t_b9840c8d-run/start.sh
curl -s http://127.0.0.1:18131/health

# 真库单测
cd /var/tmp/t_b9840c8d-wt/backend
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_rr_ap' \
  cargo test --lib business::erp::receipt_refund --features test-hooks -- --test-threads=1

# 两轮 curl（脚本自带清场与审批人夹具，可重复跑）
cd /var/tmp/t_b9840c8d-wt
BASE=http://127.0.0.1:18131/api DB=cloudpivot_rr_ap \
  bash docs/evidence/20261106-so14-receipt-refund-approval/curl-e2e.sh
```

curl 脚本预置的夹具（只在隔离库里操作）：

- 客户 `RR14AP-CUST` + 应收 `AR-RR14AP-000001`（PARTIAL / 已收 100）
- 审批人 `rr_approver`（超管，口令与 admin 同步）—— 与制单人 admin 不同人，
  所以内控「制单人不能批自己」是**真跑**过的，不是绕过的。

---

## 六、已知边界（留档，不是缺陷）

1. **前端退款页面未落地**（父卡口径不变）：`DocKind::ReceiptRefund.route()` 仍返回 `None`、
   `sys_menu` 页面节点仍 `is_visible=FALSE`。前端卡落地时，提交审批按钮走
   `POST …/submit-approval`，并用详情的 `approval` 段渲染「审批中 / 被驳回 / 已通过」
   （**不要只看 `status`**：审批不是状态）。
2. **`skip-approval` 直付分支未被 e2e 覆盖**：它要求 `business:erp:skip-approval` 权限码 +
   理由 ≥10 字，而本卡隔离库里 admin 是超管（有码），但 e2e 故意**不**走这条
   （要验证的是「默认拒绝 + 审批通过自动过账」主链）。代码路径与 P17 / SO-06 共用同一套
   `approval_gate`，且 `change_status_with(approval_override)` 的绕过能力只在内部调用点出现。
3. **`HEALTH_CHECKED` / `FLOW_TYPES` 未扩**：`GET /business/erp/approval-flow/health` 仍只体检
   8 个业务类型。扩清单要重写已应用的 `V0202` 自检块（checksum 不可改），属另一张卡的范围。
4. **`biz_status_log` 不记「驳回」**：驳回时退款单状态不变（`CONFIRMED` 不变），
   没有合法边可写，因此不硬造一条 `CONFIRMED → CONFIRMED` 日志；审批意见在审批中心实例详情里，
   发起人也会收到流程引擎的通知。
