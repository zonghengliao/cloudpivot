# SO-14 遗留：销售出库单作废 → 应收冲销（t_b33adc45）

> **一句话**：出库单作废（`POSTED → CANCELLED`）现在会在**同一个事务**里把那张自动挂上的应收
> 从 `OPEN` 冲销为**新终态 `CANCELLED`**；已收过款 / 已核销的应收一律 409 拦住作废（整笔回滚）。
> 账龄自动排除作废单，坏账报表仍只认 `WRITTEN_OFF`，两者语义不再混用。

- 工作区：`/opt/cloudpivot`（99 工作仓），分支 main
- 迁移：`V0290__sales_receivable_cancelled`（versioned，已执行，见 §四）
- 证据脚本：`docs/evidence/20261106-so14-receivable-cancel/curl-e2e.sh`（52 断言，连跑两轮全绿）

---

## 一、口径决策（卡面四问的结论）

| # | 问题 | 结论 | 理由 |
|---|------|------|------|
| 1 | 出库作废后应收怎么处理 | **新增 `CANCELLED` 终态**（不是软删、不是 `WRITTEN_OFF`） | 软删会让这条应收从台账里消失、单号还能复用，审计追不回；`WRITTEN_OFF` 是坏账（借 6702 / 贷 1122 的减值），拿去当「作废」会污染坏账与减值口径 |
| 2 | 已收过款怎么办 | **409 拦作废 + 整笔回滚**（先冲收款 / 收款退款单） | 钱已进账却把应收作废，等于凭空多一笔「客户多付」；必须有收款退款单承接，而收款模块尚未落地（SO-13 只做了手工挂账 + 坏账核销），不硬造 |
| 3 | 负金额红字应收 | **不做**，另开卡 | `ck_biz_ar_amount` 是 `amount > 0`，放宽影响账龄除零、核销恒等式、报表取数；红字应收 / 收款退款单是独立工程量 |
| 4 | 与 SO-22 的关系 | 本卡**不生成**任何凭证（应收冲销凭证 = 借 1122 贷 6001 的红字，属收入/减值凭证组） | 与 SO-13 坏账核销同口径：`voucher_id` 保持原值不动，凭证由收入凭证组的卡统一做，避免两套分录 |

状态语义对照（写进 `receivable.rs` 表头与 V0290 注释，避免下游再各写一套）：

```text
OPEN          一分没收                （有余额，进账龄）
PARTIAL       部分已收                （有余额，进账龄）
SETTLED       收清                    （终态）
WRITTEN_OFF   坏账核销（收不回来）      （终态，进坏账报表，不进账龄）
CANCELLED     出库作废冲销（销售没发生）（终态，不进坏账报表，不进账龄）← 本卡新增
```

边表（`state_machine::RECEIVABLE_EDGES`，5 → 6 条）：

```text
OPEN    → PARTIAL       部分收款
OPEN    → SETTLED       一次收清
PARTIAL → SETTLED       收清尾款
OPEN    → WRITTEN_OFF   坏账核销（必给原因）
PARTIAL → WRITTEN_OFF   坏账核销（必给原因）
OPEN    → CANCELLED     出库作废冲销（必给原因）← 本卡新增
```

**故意没有** `PARTIAL → CANCELLED`（收过钱要先冲收款）、`SETTLED/WRITTEN_OFF/CANCELLED → *`（终态）。
库层再加一道 `ck_biz_ar_cancelled`：`status <> 'CANCELLED' OR received_amount = 0` —— 绕过 handler 也改不出「收过钱的已作废应收」。

---

## 二、改动清单

| 文件 | 函数 / 段落 | 做了什么 |
|------|-------------|----------|
| `backend/migrations/business/V0290__sales_receivable_cancelled.up.sql` | ① 重建 `ck_biz_ar_status`（5 值）② 新增 `ck_biz_ar_cancelled` ③ 列注释 ④ DO 自检（域值 / 行为探针 / 探针清场） | 状态域从 4 值扩到 5 值，并钉死「作废 ⇔ 一分没收」 |
| `.../V0290__...down.sql` | 回滚前先数 `CANCELLED` 行；有则 RAISE 拒绝（不闷声炸约束） | 回滚可读、可操作 |
| `backend/src/business/shared/state_machine.rs` | `RECEIVABLE_EDGES` 5→6、`RECEIVABLE_BASE` 5→6、`RECEIVABLE_REASON_EDGES` 2→3、`RECEIVABLE_LOCKED` 2→3 | 加边 `OPEN → CANCELLED`（mandatory + requires_reason），`CANCELLED` 入账态 |
| `backend/src/business/shared/doc_kind.rs` | `DocKind::Receivable` 文档 | 状态域 5 值 |
| `backend/src/business/erp/receivable.rs` | 新增 `STATUS_CANCELLED`、`has_balance()`、`CANCEL_REASON_PREFIX`、**`cancel_for_stock_out()`**；`STATUSES` 4→5、`LEGAL_EDGES` 5→6、`LOCKED` 2→3、`status_label` 加「已作废」 | 写侧唯一入口：`OPEN`（且 received=0）→ `CANCELLED`，状态机 + `WHERE status='OPEN' AND received_amount=0` 双 CAS，`settled_at` 落封账时间，`remark` append `[出库作废冲销] 原因` |
| `backend/src/business/erp/receivable_bridge.rs` | 新增 `CancelOutcome` + **`cancel_in_tx()`**；模块文档从「未覆盖」改成「作废 → 冲销」 | 与 `post_in_tx` 对称：判据同为 `source_type='SALES'` + `find_by_source('SALES_OUT', out_id)`；已 `CANCELLED` 走幂等分支；已收款/已核销 → `Err(409)` |
| `backend/src/business/erp/stockout_handler.rs` | `cancel_out_tx()`：红字凭证之后、返回之前调 `receivable_bridge::cancel_in_tx` | 与三本账回滚 / 红字凭证同事务；任何 Err 整笔回滚 |
| `backend/src/business/erp/receivable_query.rs` | `STATUSES` 4→5、`is_overdue()` 改用 `has_balance()` | 作废单不算逾期；`page?status=CANCELLED` 可筛，payable 的 `PARTIAL_PAID`/`PENDING_CANCEL` 仍 400 |
| `backend/src/business/erp/receivable_aging.rs` | `AGING_STATUSES` 文档 + 单测（断言 SQL 里没有 `CANCELLED`、白名单恰为 OPEN/PARTIAL） | 账龄**不需要改 SQL**（本来就按 `status IN ('OPEN','PARTIAL')` 取数），只把不变量钉死 |

前端：**无改动**。前端 `frontend/src/api/business/erp/index.ts` 的 `ReceivableStatus` 与
`views/business/erp/receivable/receivable-logic.ts` 的标签/配色/下拉**已经包含 `CANCELLED`（已作废，灰）**，
与后端 `receivable::status_label` 逐字一致（已 grep 核对，未改一行）。

---

## 三、事务与幂等（为什么这样放）

```text
过账：三本账负向 → 成本结转凭证 → 挂应收        （post_out_tx）
作废：回滚三本账 → 出库红字凭证 → 冲销应收      （cancel_out_tx，本卡补的第 ③ 步）
```

- 三步在**一个事务**里：应收侧任何 Err（已收款 / 已核销 / 并发改状态）都会让三本账与红字凭证一起回滚 ——
  证据：curl §9（已收款 → 409 后出库单仍 `POSTED`、库存仍 15、红字凭证 0 张、应收仍 `PARTIAL|100`）。
- 幂等三道：
  1. 重复作废被**出库单**状态机 CAS 挡在 409（`CANCELLED` 是终态），根本进不到桥接层；
  2. 万一重入，`cancel_in_tx` 见到应收已是 `CANCELLED` → `cancelled=false` 直接返回，不写第二次；
  3. 应收侧 UPDATE 带 `status='OPEN' AND received_amount=0`，并发收款把它推到 `PARTIAL` → 写 0 行 → 409 → 整笔回滚。
- 单测 `repeated_cancel_keeps_receivable_cancelled_once` 断言「作废轨迹只有 1 条、库存只回滚一次」。

---

## 四、迁移与回滚（实测）

**全新库整链**：`cloudpivot_so14c`（空库）由引擎从 `V0000` 迁到 `V0290`：

```
sales_customer_credit_days | 289 | success=t | executed | 3ms
sales_receivable_cancelled | 290 | success=t | executed | 2ms   ← 本卡
约束：ck_biz_ar_status / ck_biz_ar_cancelled / ck_biz_ar_amount / ck_biz_ar_unsettled / ck_biz_ar_credit / ck_biz_ar_settled（6 条）
NOTICE V0290 自检通过：ck_biz_ar_status 5 值（含 CANCELLED）/ ck_biz_ar_cancelled 拦住已收款作废 / 原有 4 值不受影响
```

**已有库（`cloudpivot_so14`，引擎 289）手跑 up**：`ALTER TABLE … DO …`，自检 NOTICE 通过。

**回滚往返**：

```
down（无 CANCELLED 行）→ 成功，ck_biz_ar_status 恢复 4 值，ck_biz_ar_cancelled 删除
up  再跑           → 成功，5 值恢复（幂等：DROP+ADD 约束，探针插完即删）
down（有 1 张 CANCELLED 行）→ 拒绝：
  ERROR: V0290 回滚被拒：库里还有 1 张 status=CANCELLED 的应收单。请先决定这批单据的去向（软删 / 改回 OPEN），再回滚本迁移
  且 DDL 一行没执行（约束仍 5 值）
```

---

## 五、单测（真库）

```bash
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so14' \
  cargo test --lib business::erp::receivable_bridge::tests --features test-hooks \
  -- --nocapture --test-threads=1
# test result: ok. 13 passed; 0 failed; 2806 filtered out; finished in 14.67s
```

新增 3 条（全部真库、非 SKIP）：

| 用例 | 断言 |
|------|------|
| `cancel_stock_out_cancels_receivable_and_restores_stock` | 过账 → 应收 `OPEN 1130` 且**在账龄里** → 作废 → 应收 `CANCELLED`（同一行）、`received=0`、`unsettled=1130`、`settled_at` 落值、`remark` 带前缀与原因、无「坏账」字样、状态日志 `OPEN>CANCELLED|原因`、**账龄已排除**、库存 10→20、出库单 `CANCELLED` |
| `cancel_blocked_when_receivable_has_receipts` | 应收造 `PARTIAL`（收 40）→ 作废 **409**、文案点名已收款 → 出库单仍 `POSTED`、库存仍 10、应收仍 `PARTIAL|40`、**红字凭证 0 张** |
| `repeated_cancel_keeps_receivable_cancelled_once` | 第一次作废成功；第二次 409；直接重入桥接层 `cancelled=false`；`CANCELLED` 轨迹只有 1 条；库存只回滚一次 |

同时更新的既有单测（域值/边表/白名单/逾期/账龄）：
`receivable::status_domain_matches_check_constraint`、`legal_edges_match_arch7_state_graph`、
`receivable_query::status_filter_rejects_payable_domain_values`、`overdue_boundary_is_strict_less_than`、
`receivable_aging::sql_bucket_case_comes_from_payable`。

原始输出：`unit-tests.log`。

---

## 六、真实 token 的端到端 curl（连跑两轮）

实例：`backend/target/debug/cloudpivot` @ `127.0.0.1:18118`，库 `cloudpivot_so14c`（全新迁到 290），admin 真 token。

```bash
BASE=http://127.0.0.1:18118/api DB=cloudpivot_so14c \
  bash docs/evidence/20261106-so14-receivable-cancel/curl-e2e.sh
```

```
第一轮：==> 汇总：PASS=52 FAIL=0   （rc=0）
第二轮：==> 汇总：PASS=52 FAIL=0   （rc=0）
```

链路与逐段实测（摘录第一轮，原始输出见 `curl-round1.txt` / `curl-round2.txt`）：

```
POST  /customer（credit_days=45）             → 回显 45
POST  /sales-order（10×100@13%）              → 价税合计 1130.0000；夹具置 APPROVED
POST  /shipment → PATCH POSTED                → OUT-20261106-000003；应收 1 张 1130.0000 OPEN；库存 20→10
GET   /receivable/aging?as_of=今天             → 明细含该出库单（source_no=OUT-…）
PATCH /stock-out/{id}/status CANCELLED        → 出库单 CANCELLED
GET   /receivable/{id}                        → status CANCELLED / 标签「已作废」/ received 0.0000 /
                                                unsettled 1130.0000 / settled_at 非空 / overdue=false
GET   /receivable/page?status=CANCELLED       → total=1；?status=OPEN 同单 → total=0
GET   /receivable/aging?as_of=今天             → 明细已排除该单；doc_count −1；未收合计 −1130.0000
库层   biz_inventory 20（回滚）/ biz_inventory_location 20 / 流水净额 0
       biz_voucher 红字 1 张 + 原凭证 reversed_by 非空
       biz_status_log(RECEIVABLE) = OPEN>CANCELLED|客户取消订单，货退回仓库
       biz_receivable 行仍在（deleted_at IS NULL），remark 带 [出库作废冲销]
幂等   重复 PATCH CANCELLED → code=10003；应收仍 1 张、轨迹仍 1 条、库存仍 20
已收款 第二单（5×100@13% → 565）造 PARTIAL（已收 100）后再作废 → 10003 且报文要求「先冲收款」
       → 出库单仍 POSTED / 库存仍 15 / 红字凭证 0 / 应收仍 PARTIAL|100.0000
白名单 ?status=CANCELLED → 200；?status=PARTIAL_PAID → 10001
```

---

## 七、其它门禁

```
cargo fmt --check                  → rc=0
cargo clippy --all-targets --features test-hooks -- -D warnings → rc=0（0 告警）
cargo test --features test-hooks（全量，见 fold 摘要）→ 见 §八
scripts/lint/scope_gate.py         → 本卡新增 SQL 全部走 TABLE 常量 / 已有 SCOPE-EXEMPT（模块头），零新增违规
                                     （脚本仅剩一条**既有** FAIL：SO-09 的 shipment_page_fill_tests.rs 未登记 SKIP_MANIFEST）
本卡无新接口 / 无新权限码 ⇒ perm_menu_gate 无需新增条目（V0288 的 5 个码不变）
10_checks.sql                      → 无需改：登记表按 (ref_domain, ref_type) 登记**实体**，
                                     RECEIVABLE→biz_receivable 已由 SO-13 登记；状态取值域不在该表登记范围
```

---

## 八、全量回归

`cargo test --features test-hooks`（库 `cloudpivot_so14`，1212.53s）：

```
test result: FAILED. 2813 passed; 6 failed; 0 ignored; 0 measured
本卡 3 条新用例全部 passed
```

6 条失败逐条归因（明细见 `full-suite-summary.txt`）：

| 失败 | 归因 |
|------|------|
| `p16_graph_snapshot_selfcheck_is_clean`、`sales_approval_bridge::three_tiers_…` | **既有缺陷**（父卡 SO-14 报的同一对：`so_approve` 图漂移 `RowsEdited`）；单跑复现，本卡不碰 flow 定义 |
| `doc_kind_registries_match_doc_kind_table`、`frontend_ts_mirror_matches_rust` | 并行卡 **SO-17（收款单）在途未提交改动**（`10_checks.sql` / `doc-kinds.ts` 先加了 `RECEIPT`，Rust 侧当时还没跟上）；本卡提交里不含 RECEIPT 任何一行 |
| `payable_bridge::p17_skip_approval_…`、`stocktake_tests::zz_teardown_…` | **共享库串味 / 顺序相关**，单跑 2 passed（流程被别的用例发布、待办 FK 未清） |

---

## 九、边界（刻意没做，别当漏了）

1. **负金额红字应收 / 收款退款单**：`ck_biz_ar_amount` 是 `amount > 0`，放宽影响面大；且「已收过款再作废」的正确处理是收款退款，需要收款模块 —— 已另开卡。
2. **应收冲销的会计凭证**（红字：借 1122 红 / 贷 6001 红）：与 SO-13 坏账核销同口径，属收入/减值凭证组卡，本卡不动 `voucher_id`。
3. **期间闸**：出库作废这条路径本身不过 `period::ensure_period_writable`（红字凭证直接落作废当天），只在应收侧单方面加闸会让「销售来源出库单」与「领用出库」的作废行为不一致 ⇒ 本卡不加，属出库作废既有口径（要收口就一起收）。
4. **发货单自身作废**：`POSTED` 是发货单终态（`SHIPMENT_LOCKED`），现实路径就是作废那张销售来源出库单 —— 本卡覆盖的正是这条。
