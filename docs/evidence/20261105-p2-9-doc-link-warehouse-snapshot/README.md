# P2-9 证据：单据元数据补齐（任务挂单据 + 采购单仓库快照）

> 卡：`t_bb2e4702`（P2-9）· 依据 `docs/architecture/ARCH-3-模块勾稽关系审计.md` §5 DC-19 + DC-23
> 口径：`docs/architecture/P2-9-单据元数据口径.md`
> 迁移：`backend/migrations/business/V0238__task_doc_link_and_po_warehouse_snapshot.{up,down}.sql`

## 1. 交付内容

| 层 | 内容 |
|---|---|
| 迁移 | `V0238`：`biz_task.doc_type` + `doc_id`（成对，`ck_biz_task_doc_pair`，部分索引 `idx_biz_task_doc`）+ `biz_purchase_order.warehouse_name_snapshot`。**不写值域 CHECK**（词表归 `DocKind`），末尾 DO 块 fail-closed 自检 |
| 后端 | `task/doc_ref.rs`（白名单 = `DocKind` ∩ `DOC_TARGETS`、存在性校验、单号现查）；`POST/PUT /business/task` 支持 `doc_type/doc_id`；`GET /business/task/by-doc`（走任务可见性）；任务卡/详情下发 `doc_type/doc_id/doc_no`；采购单建单/改单写仓库名快照；列表读快照（老单批量现查兜底）、详情读现值（软删回落快照）；错误码 `63022` / `63023` |
| 前端 | 采购单列表「收货仓库」列（读后端 `warehouse_name`）；采购单详情「收货仓库（现名）」+「下单时仓库快照」两行；详情底部 `DocTaskPanel`（列出关联任务 + 直接新建）；任务详情「关联单据 = 采购订单 PO-xxx」可点回 |
| 文档 | 新增口径文档；ARCH-3 DC-19 / DC-23 状态改写 |

## 2. 证据文件（本次全部为**最终提交版源码**产出）

| 文件 | 是什么 | 关键数字 |
|---|---|---|
| `migration.txt` | V0238 手工回放：up → 结构断言 → 半对反例 → down → down 幂等 → 再 up | 列 uuud/varchar 可空、部分索引 `idx_biz_task_doc` 在、`ck_biz_task_doc_pair` 在、`biz_task` 列数 39 → 41 → 39 → 41 |
| `tests-task.txt` | `cargo test --lib business::task`（真库 `cloudpivot_p29f`） | **226 passed / 0 failed**（含 P2-9 新增 9 条 `doc_link_tests`） |
| `tests-purchase.txt` | `cargo test --lib business::erp::purchase_handler`（真库） | **65 passed / 0 failed**（含 P2-9 新增 5 条 `p9_*`） |
| `tests-doc-ref.txt` | `cargo test --lib business::task::doc_ref::tests`（纯逻辑） | **6 passed / 0 failed**（白名单 / 严格匹配 / 成对 / 类型取自 DocKind） |
| `e2e-http.txt` | 真 HTTP 端到端（隔离实例 127.0.0.1:18149 + 全新空库，13 步） | **ALL PASS**：建单 → 仓库改名 → **列表显示旧名（快照）/ 详情显示新名（现值）** → 建挂单任务 → `by-doc` 反查 → 任务列表/详情带单据 → 三条负例（63022 / 63023 / 63004） |
| `ui-check.txt` + `1-purchase-detail.png` / `2-related-tasks.png` / `3-task-detail-doc.png` | 真机页面（vite dev :5183 → 后端 :18149，Playwright） | **ALL PASS**：详情两行仓库（现值 ≠ 快照，见下）、列表列显示快照、**在详情里新建跟进任务 → 出现在「关联任务」区**、任务详情显示「采购订单 PO-…」可点 |
| `invariants.txt` | `scripts/verify/erp_invariants.py` 打在同一个库 | **52/52 通过，BLOCKER 0 / WARN 0** |
| `lint-fmt.txt` / `lint-clippy.txt` / `lint-scope-gate.txt` | 静态检查（隔离树） | 见 §5 |

UI 证据里两个仓库名**刻意不同**（这是 DC-23 的验收点）：

```
收货仓库（现名）    WH02 改名后仓（P2-9-1793822381）   ← 详情：现值
下单时仓库快照      WH02 改名后仓（P2-9）              ← 下单那一刻的名字
列表「收货仓库」列   WH02 改名后仓（P2-9）              ← 列表：快照
```

## 3. 卡面验收逐条对照

| 验收项 | 结论 | 证据 |
|---|---|---|
| 迁移 up/down 通过 | ✅ | `migration.txt`（up → down → down 幂等 → up；列数 39/41 往返）+ 真引擎全新空库应用（`应用 179 个迁移`，`schema_migrations` 记 `238 \| task_doc_link_and_po_warehouse_snapshot \| executed \| success=true`） |
| 单测 ≥10 条（白名单 / doc 不存在 / 反向查询 / 快照写入 / 老单 fallback） | ✅ **20 条**（9 真库 doc_link + 5 真库 p9 + 6 纯逻辑 doc_ref） | `tests-task.txt` / `tests-purchase.txt` / `tests-doc-ref.txt` |
| 真机：采购单详情能给这张单新建跟进任务 → 任务列表能看到 → 任务详情能看到关联单据 | ✅ | `ui-check.txt`（Playwright 点击）+ `e2e-http.txt`（真 HTTP 同一链路） |
| 真机：仓库改名后老单列表显示旧名（快照），详情显示现名（关联） | ✅ | `ui-check.txt` 两行断言 + `2-related-tasks.png` |
| 不变量全绿 | ✅ | `invariants.txt`：52/52，BLOCKER 0 / WARN 0 |
| git commit（`feat(erp): P2-9 …`） | ✅ | 见卡内 completion 的 commit sha |

## 4. 单测清单（P2-9 新增 20 条）

**真库 · `business::task::model::doc_link_tests`（9 条）**

1. `doc_link_rejects_unknown_type` —— 白名单外类型 → `400 + 63022`（报文列出允许值）
2. `doc_link_rejects_missing_doc` —— 单据不存在 → `400 + 63023`
3. `doc_link_rejects_soft_deleted_doc` —— 单据已软删 → `400 + 63023`
4. `doc_link_create_persists_and_detail_reports_no` —— 两列真落库 + 详情下发 `doc_type/doc_id/doc_no`
5. `doc_link_absent_keeps_columns_null` —— 不传 = 自由任务，两列 NULL、无单号
6. `doc_link_by_doc_lists_only_that_doc` —— 反查只返回这张单的任务（别的单 / 自由任务都不混入）
7. `doc_link_by_doc_respects_visibility` —— **可见性**：别人挂在同一张单上的任务，`SelfOnly` 范围看不到
8. `doc_link_update_clears_and_swaps` —— 改任务：换单 → 真落库；清单 → 两列 NULL；清单后不再出现在反查里
9. `doc_link_db_rejects_half_pair` —— 绕过 handler 直写半对 → 被 `ck_biz_task_doc_pair` 拒绝

**真库 · `business::erp::purchase_handler::tests::p9_*`（5 条）**

1. `p9_create_writes_warehouse_name_snapshot` —— 建单写快照（真落库）
2. `p9_update_refreshes_snapshot` —— 改名后改单，快照刷新成新名
3. `p9_list_keeps_snapshot_but_detail_shows_live_name` —— **卡面验收**：列表旧名 / 详情现名 / 快照并列
4. `p9_legacy_null_snapshot_falls_back_without_backfill` —— 老单（NULL）列表兜底现查，且**不写库**
5. `p9_deleted_warehouse_detail_falls_back_to_snapshot` —— 仓库软删 → 详情回落快照（不空白）

**纯逻辑 · `business::task::doc_ref::tests`（6 条）**

白名单恰好 5 种 / 每种有实体表且单号列符合形态 / 每个 code 都能被 `DocKind::parse` 回解（不新开词表）/
`PAYMENT`·`STOCK_OUT`·`BUDGET` 不在白名单（防无意放宽）/ 严格全等匹配（不做大小写归一）/ 成对与非法 UUID 分码

## 5. 检查链状态（隔离树 `/root/p28-tree`，只含本卡改动）

| 检查 | 结果 |
|---|---|
| `rustfmt +1.94.1 --edition 2024 --check`（本卡 20 个文件） | ✅ 0 diff |
| `cargo clippy -- -D warnings` | ✅ EXIT=0（无告警） |
| `cargo build` | ✅ 0 error |
| `cargo fmt -p cloudpivot -- --check` | ⚠️ 整仓 2 处漂移：`src/bootstrap.rs`、`src/shared/startup_guard.rs` —— **HEAD 原样**（`git status --porcelain` 为空），与本卡无关；本卡文件已单独 fmt 干净 |
| 闸② `scope_gate.py` | ⚠️ 6 条 FAIL **全部来自 budget/asset 批次的既有文件**（`asset_handler.rs` / `budget_handler.rs` 缺 SCOPE-EXEMPT；`asset_model_tests.rs` / `budget_dto_tests.rs` / `budget_guard_tests.rs` / `budget_model_tests.rs` 未登记 SKIP_MANIFEST）——这些文件本卡一行未动；**本卡路径（`business/task/*`、`business/erp/{dto,handler,model_order,purchase_*}*`）0 条 FAIL**，新增测试文件 `task_doc_link_tests.rs` 已登记进 `SKIP_MANIFEST` |

## 6. 复现步骤

```bash
# 1) 造一个全新空库并起隔离实例（真引擎跑迁移 + 种子）
runuser -u postgres -- createdb -O alun cloudpivot_p29f
bash /root/p29b-instance.sh cloudpivot_p29f 18149        # 见 skill：setsid + NIK_* 环境
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:18149/health   # 200

# 2) 真库单测（TASK_DB_URL / ERP_DB_URL 指同一个库）
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_p29f \
TASK_DB_URL=$ERP_DB_URL NIK_PROFILE=test \
  cargo test --lib business::task -- --test-threads=1
ERP_DB_URL=... cargo test --lib business::erp::purchase_handler -- --test-threads=1

# 3) 真 HTTP e2e
P29_DB=cloudpivot_p29f P29_BASE=http://127.0.0.1:18149 python3 /root/p29_e2e.py

# 4) 真机页面（vite dev 指向同一后端）
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18149 ./node_modules/.bin/vite --port 5183
P29_BASE=http://127.0.0.1:5183 P29_PW='P29Admin@123456' \
 P29_ORDER_ID=<订单id> P29_ORDER_NO=<订单号> P29_OLD_WH=<快照名> P29_NEW_WH=<现名> \
 node /root/p29_ui_check.mjs

# 5) 不变量
NIK_DATABASE_NAME=cloudpivot_p29f NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  python3 scripts/verify/erp_invariants.py
```

## 7. 已知边界（本卡明确不做）

1. **不合并两套任务表**：`biz_task`（工作任务）/ `biz_flow_task`（审批待办）各司其职，只加一条边。
2. **只给采购单加仓库快照**：收货单 / 调拨 / 盘点的「仓」语义不同（实际入库仓 ≠ 计划收货仓），不做批量复制。
3. **不回填老单快照**：显示层兜底（只读不写），见口径文档 §2.3。
4. **DC-23 的另一半（提交后改仓库）没做**：属 `R-P2-5 ③`，要动状态机 + 收货侧校验 + 留痕。
5. **前端深链只覆盖采购单**：收货单 / 合同 / 应付 / 凭证详情页还没有 `?id=` 深链，任务详情里只显示「类型 + 单号」（不造点不动的链接）。
6. **子任务 / 副本不继承单据关联**（挂单是显式声明）。
7. 隔离实例未开 `sse` feature，页面里 `/api/business/notice/stream` 会 401（与本卡无关，已在 `ui-check.txt` 标注为 INFO）。
