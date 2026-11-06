# SO-05（t_9832ecaf）销售订单后端 API + 状态机 —— 证据包

> 卡面：实现销售订单 CRUD + 状态机，非法迁移必须被拒，`cargo test` 全绿 + curl 实测。
> 上游交接：t_f5f501a7（SO-04，提交 `c1bddb3`，V0264 建表 + `sales_model.rs` 取号）。

## 1. 交付清单（改了哪些文件 / 函数）

| 文件 | 类型 | 内容 |
|---|---|---|
| `backend/src/business/erp/sales_order_model.rs` | 新增 1.1k 行 | 行结构 `SalesOrderRow` / `SalesOrderItemRow`、`NewItem` / `NewSalesOrder` / `Totals`、`line_amounts`、`LEGAL_EDGES`、`can_transition` / `allowed_targets`、`create_order` / `update_order` / `apply_status`、`find_order` / `items_of` / `order_page` / `customer_labels` / `customer_open_credit`、13 条单测（含 5 条真库） |
| `backend/src/business/erp/sales_order_handler.rs` | 新增 1.3k 行 | 5 个接口 + 4 个权限码 + 校验/守卫 `build_item` / `build_order` / `require_active_customer` / `ensure_submittable`、7 条单测（含 4 条真库） |
| `backend/src/business/shared/state_machine.rs` | 改 | 新增 `ENTITY_SALES_ORDER`、`SALES_ORDER_EDGES`（10 条）、`SALES_ORDER_BASE`、`SALES_ORDER_REASON_EDGES`（4 条）、`SALES_ORDER_LOCKED`；`Entity::SaleOrder` 变体 + `ALL`（14→15）+ `as_str`/`label`/`edges`/`base_edges`/`reason_edges`/`locked_statuses` 六处 match |
| `backend/src/business/shared/doc_kind.rs` | 改 | `DocKind::SaleOrder`（`SALE_ORDER` / 销售订单 / `biz_sales_order` / `sales-order`）+ `ALL`（18→19）+ 巡检字面量表 `DOC_KIND_VALUES`（18→19） |
| `backend/src/business/flow/linkage/spec.rs` | 改 | `TRIGGER_EXEMPT` 加 `SALE_ORDER`（本卡不接联动引擎，见 §4 决策 3） |
| `frontend/src/api/business/erp/doc-kinds.ts` | 改 | 词表镜像补 `SALE_ORDER`（`DOC_KINDS` / `DOC_KIND_LABELS` / `DOC_KIND_TABLES`）——后端 `doc_kind_tests` 逐字比对 |
| `docs/architecture/snapshot-drift/10_checks.sql` | 改 | `STATUS_ENTITY` 登记 `SALE_ORDER → biz_sales_order`（DI 巡检 + `doc_kind_tests` 双守卫） |
| `backend/migrations/business/V0265__sales_order_permissions.{up,down}.sql` | 新增 | 4 个权限码（页面节点 `…2651` + 三个按钮 `…2652/2653/2654`），`path/component` 留 NULL 等前端卡 |
| `backend/src/business/erp/mod.rs` | 改 | 注册两个新模块 |

### 接口（前缀 `/api` 由 `[router].prefix` 注入）

| 方法 | 路径 | 权限码 |
|---|---|---|
| POST | `/business/erp/sales-order` | `business:erp:sale-order:create` |
| PUT | `/business/erp/sales-order/{id}` | `business:erp:sale-order:update` |
| GET | `/business/erp/sales-order/{id}` | `business:erp:sale-order:list` |
| GET | `/business/erp/sales-order/page` | `business:erp:sale-order:list` |
| PATCH | `/business/erp/sales-order/{id}/status` | `business:erp:sale-order:status` |

### 状态机（`state_machine::SALES_ORDER_EDGES`，10 条）

```
DRAFT → SUBMITTED → APPROVED → PARTIAL_OUT → OUTBOUND
                       ├→ OUTBOUND（一次发完）   └→ CLOSED（剩余量作废）
                       └→ CLOSED（未发货直接关闭）
DRAFT / SUBMITTED / APPROVED → CANCELLED
```

- 状态域取自 PRD §4.13.3 的七值（`PARTIAL_OUT` / `OUTBOUND`，**不是**技术架构旧稿的 `SHIPPED` 系）；
  与 V0264 的 `ck_sales_order_status` 逐字一致（`sales_model::STATUSES` 单测已钉住）。
- 要原因的四条边：`SUBMITTED→CANCELLED`、`APPROVED→CANCELLED`、`APPROVED→CLOSED`、`PARTIAL_OUT→CLOSED`。
- 三个终态（`OUTBOUND` / `CLOSED` / `CANCELLED`）没有出边。

## 2. 验收证据（本目录）

| 文件 | 内容 |
|---|---|
| `round-1-unit-db.txt` / `round-2-unit-db.txt` | 同一个库（`cloudpivot_so05`）连跑两轮 `business::erp::sales_order` → 各 **20 passed / 0 failed**（9.76s / 9.88s，耗时本身证明真库用例没被 SKIP） |
| `curl-e2e.sh` / `curl-e2e.txt` | 真实 admin token 的 22 条 curl 断言（`=== 全部断言通过`），含 409 非法迁移 / 400 域外值 / 400 缺原因 / 409 守卫 |
| `regression-shared.txt` | `business::shared::`（词表 + 状态机）→ **113 passed** |
| `regression-flow.txt` | `business::flow::`（联动 + 边配置）→ **389 passed** |
| `regression-customer.txt` | `business::erp::customer_`（SO-02 回归）→ **11 passed** |
| `full-suite.txt` | 干净库（`cloudpivot_so05b`）全量 lib 套件（`--test-threads=1`）：**2722 passed / 1 failed / 1521s**。唯一失败是 `flow::linkage::tests_wiring::wired_triggers_match_call_sites`，**与本卡无关**（见 §5） |
| `head-consistency.txt` | 对本次提交 `d573d0f` 的静态核对（联动调用点 / wired 登记 / `transfer_handler*` 是否在本卡 diff 里） |
| `invariants-clean.txt` | 干净库 `erp_invariants.py` → **67/68，BLOCKER 0，WARN 1**（唯一 WARN = 新库 admin 无部门，bootstrap 种子现状，与 SO-04 同一基线） |
| `snapshot-audit-clean.txt` | 干净库 `snapshot_audit.py` → **26/26 通过，BLOCKER 0** |
| `invariants.txt` | ⚠ 这是在**跑过 flow 回归的脏库**（`cloudpivot_so05`）上的输出：`67/68` 之外的 `D1 已过账收货单必有凭证` BLOCKER 来自 `D2L-SI-2`（联动测试夹具 `business::flow::linkage`，10:18:40 由回归跑出来，非本卡代码）；干净库基线见 `invariants-clean.txt` |
| `clippy.txt` | `cargo clippy --all-targets --features test-hooks -- -D warnings` → 零告警 |
| `frontend-typecheck.txt` | `pnpm typecheck`（vue-tsc --noEmit）→ rc=0（词表镜像改动） |
| `frontend-build.txt` | `NODE_OPTIONS=--max-old-space-size=6144 pnpm build` → rc=0。**首轮 OOM**：`vite build` 默认 1.5G 堆，当时全量 `cargo test`（2710 条真库）正在同机跑，机器 15G 内存不够；套件跑完重跑即过。与本卡改动无关（前端只加了常量数组） |
| `diff/` | 本卡全部改动相对 `c1bddb3` 的 unified diff |

### curl 实测关键输出（原文见 `curl-e2e.txt`）

- 建单：`SO-20261106-000045`，`total_amount=150.0000 / total_tax=19.5000 / total_with_tax=169.5000`（服务端按行算，价税恒等）
- 非法迁移（跳步）：
  `HTTP 409 {"code":10003,"msg":"非法状态迁移：草稿（DRAFT）→ 已审批（APPROVED）不在合法边表里；当前可流转到：SUBMITTED（已提交） / CANCELLED（已取消）"}`
- 域外值 `SHIPPED`：`HTTP 400`（`target_status 必须是 DRAFT / SUBMITTED / APPROVED / PARTIAL_OUT / OUTBOUND / CLOSED / CANCELLED 之一`）
- 提交后作废缺原因：`HTTP 400`；带原因：`HTTP 200`
- 终态出边：`HTTP 409 …已是终态 已取消（CANCELLED），不能再流转`
- 信用额度：`HTTP 409 …额度 50.0000，在手订单已占 0，本单 SO-20261106-000030 价税合计 113.0000，合计 113.0000 超出 63.0000`
- 提交守卫：0 行明细 → `400`；客户冻结（建单时 ACTIVE，草稿期间被冻结）→ `409`；冻结客户建新单 → `409`

## 3. 复现步骤

```bash
# ① 干净库（会跑迁移 + bootstrap）
ERP_DB=cloudpivot_so05 PORT=18097 RECREATE=1 bash scripts/verify/erp_clean_db.sh
# ② 单测（两轮）
cd backend && ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_so05' \
  cargo test --lib --features test-hooks -- business::erp::sales_order --test-threads=1
# ③ 起隔离实例 + curl 实测
bash /root/.hermes/profiles/backend-developer/cache/scratch/so05_instance.sh &   # 127.0.0.1:18099
bash docs/evidence/20261106-so05-sales-order-api/curl-e2e.sh
```

## 4. 设计与踩坑（写给下一个改这条链的人）

1. **状态域以 PRD 为准**：`PARTIAL_OUT` / `OUTBOUND` 两个值是本条链**唯一**合法写法；
   `sales_model::STATUSES`、`state_machine::SALES_ORDER_EDGES`、`ck_sales_order_status` 三处必须逐字一致
   （单测 `legal_edges_use_declared_status_domain` + `main_chain_edges_allowed` 钉住）。
2. **金额舍入必须是「四舍五入」**：`rust_decimal::Decimal::round_dp` 默认是**银行家舍入**，
   `3 × 17.655 × 13%` 会算成 `6.8854` 而不是 `6.8855`（单测实测红过）。
   本卡统一用 `round_dp_with_strategy(4, RoundingStrategy::MidpointAwayFromZero)`，且**逐行舍入再汇总**。
3. **联动引擎显式豁免**：`flow::linkage::tests::triggers_cover_state_machine_entities` 要求状态机的每个实体
   要么是联动触发点、要么在 `TRIGGER_EXEMPT`。本卡不接联动（出库过账卡再接），
   所以 `SALE_ORDER` 进豁免清单 —— 登记在白名单却没人调引擎就是「配了规则永远不触发」那个病。
4. **改单只允许 DRAFT**：`update_order` 的 UPDATE 带 `WHERE status = 'DRAFT'`（CAS），
   handler 先查状态给人话 409，并发下由 CAS 兜底。
5. **信用占用口径**：`SUBMITTED / APPROVED / PARTIAL_OUT` 三个状态占用额度（`CREDIT_OCCUPYING_STATUSES`），
   `limit = 0` = 不控额度；提交时用 `customer_open_credit(ex, customer, Some(本单 id))` 排除自己。
6. **客户是弱引用**：`customer_id` 无外键，「客户未冻结」必须自己查 `biz_customer.status = 'ACTIVE'`
   （SO-04 交接第 ③ 条），建单 / 提交各查一次（草稿期间客户可能被冻结）。
7. **测试夹具清场按 customer_id，不能按单号前缀**：单号是取号器生成的 `SO-yyyyMMdd-NNNNNN`，
   按 `SO5T-` 前缀删**永远删不到**（第一版实测栽在这）。
8. **curl 实测先撞 403 `首次登录必须先修改初始密码`**：bootstrap 种子的 admin 带
   `must_change_password = true`，`session_guard` 对**所有**业务接口返回 403。
   实测脚本先 `PUT /api/system/auth/password` 改密再重登（不是本卡的 bug）。
9. **V0265 的 path / component 留 NULL**：`system::menu::tests::n08`（菜单路径必须能在前端路由解析）
   在前端页落地之前会红 —— 与 V0261/V0262 的两次收尾同一口径。

## 5. 全量套件里那 1 条 FAILED 的归因（不是本卡）

```
business::flow::linkage::tests_wiring::wired_triggers_match_call_sites ... FAILED
联动引擎的调用点与 spec::TRIGGER_SPECS 不一致：
- 调了引擎但没登记（多接了触发点却没写 wired = true）：
  ["src/business/erp/transfer_handler_db_tests_linkage.rs", "src/business/erp/transfer_handler_status.rs"]
```

- 这两个文件是**另一张卡（LNK-01 调拨接线）正在共享工作树里的未提交改动**：
  `git status` → `M backend/src/business/erp/transfer_handler_status.rs`、
  `?? backend/src/business/erp/transfer_handler_db_tests_linkage.rs`、
  `M backend/src/business/flow/linkage/spec.rs`；
- 本卡提交 `d573d0f` **一行都没碰** `transfer_handler*`（`git show --name-only d573d0f | grep transfer_handler` = 0 行），
  只在自己的 `doc_kind/state_machine` 上新登记了 `SALE_ORDER`（销售订单不调联动引擎，登记在
  `TRIGGER_EXEMPT`，扫描器根本不会把它算进「调了引擎的文件集合」）；
- 本卡早前在**一致状态**的工作树上跑 `business::flow::` 回归时，这条用例是 **ok**
  （`regression-flow.txt`：389 passed / 0 failed，含 `wired_triggers_match_call_sites ... ok`）。
- 提交时还收口过一次「共享索引」问题：第一版 `f4d2335` 误把 LNK-01 在 `linkage/spec.rs` 的
  未提交 hunk（`TRANSFER` 的 `wired: true` + `TRANSFER_EXCLUDED_ACTIONS`）一起带进提交，
  已 `reset --soft` 后用 `git update-index` 只重放本卡的 6 行 hunk 重新提交为 `d573d0f`，
  对方的工作树改动原样保留（`git status` 仍是 `MM`）。因此 **HEAD 上的 spec.rs 与
  HEAD 上的 transfer_handler_status.rs 是自洽的**（HEAD 的 transfer_handler_status.rs 里
  `linkage::engine::run` 出现 0 次，见 `head-consistency.txt`）。
