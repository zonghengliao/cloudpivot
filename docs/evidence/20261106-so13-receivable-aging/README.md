# SO-13 应收后端 API + 账龄 —— 证据

卡：`t_b0fb1724`【SO-13】应收后端 API + 账龄
父卡：`t_d1f27e36`（SO-12 表 `biz_receivable` + `V0287`）
本轮工作区：`/opt/cloudpivot`（99 工作仓），隔离库 `cloudpivot_so13`（引擎迁到 288）

---

## 一、交付物

| 文件 | 内容 | 主要函数 |
|------|------|----------|
| `backend/src/business/erp/receivable.rs`（新增，~1290 行） | 应收写侧：手工挂账 + 坏账核销（镜像 `payable.rs`，但状态域是应收的 4 值） | `create_manual` / `create_manual_in_tx` / `insert` / `write_off` / `write_off_api` / `find_by_id` / `find_by_source` / `next_receivable_no` / `unsettled_of` / `due_date_of` / `validate_amount` / `validate_credit_days` / `validate_reason` / `validate_period` / `status_label` + 两个 HTTP 入口 `receivable_create` / `receivable_write_off` |
| `backend/src/business/erp/receivable_query.rs`（新增，~1010 行） | 应收读侧：分页 + 详情（含客户名 / 凭证号），镜像 `payable_query.rs` | `receivable_page` / `find_receivable` / `receivable_detail` / `voucher_no_of` / `is_overdue` / `ReceivableRes::from_row` + 两个 HTTP 入口 |
| `backend/src/business/erp/receivable_aging.rs`（新增，~1400 行） | 应收**账龄分析**：分档/逾期天数/基准日/金额归一律**直接复用** `payable_aging` | `pivot_customers` / `summarise_customers` / `load_cells` / `load_totals_and_details` / `build_snapshot` / `assemble` / `base_cte` + HTTP 入口 `receivable_aging_handler` |
| `backend/migrations/business/V0288__sales_receivable_permissions.{up,down}.sql`（新增） | 5 个权限码（1 页面节点 + 4 按钮），path/component 留空等 SO-15，末尾 DO 块自检 | — |
| `backend/src/business/shared/state_machine.rs`（改） | 登记 `Entity::Receivable`（ALL 16 → 17）+ `ENTITY_RECEIVABLE` + `RECEIVABLE_EDGES(5)` / `RECEIVABLE_BASE` / `RECEIVABLE_REASON_EDGES` / `RECEIVABLE_LOCKED` | — |
| `backend/src/business/shared/doc_kind.rs`（改） | 登记 `DocKind::Receivable`（ALL 20 → 21 / `DOC_KIND_VALUES` 21 / `as_str` / `label` / `table` / `is_document` / `route`） | — |
| `backend/src/business/erp/mod.rs`（改） | 三个新模块声明 + 模块文档表三行 | — |
| `backend/src/business/erp/payable_aging.rs`（改） | 改掉过期注释「全库没有 `biz_receivable`」，改成「应收账龄在 `receivable_aging`，分档只有一份」 | — |
| `frontend/src/api/business/erp/doc-kinds.ts`（改） | 前端词表镜像加 `RECEIVABLE`（`DOC_KINDS` / `DOC_KIND_LABELS` / `DOC_KIND_TABLES`），`doc_kind_tests::frontend_ts_mirror_matches_rust` 需要 | — |
| `docs/architecture/snapshot-drift/10_checks.sql`（改） | 巡检登记 `('STATUS_ENTITY','RECEIVABLE','biz_receivable','deleted_at',…)` | — |

**边界（没做的，别当成漏了）**：本卡**不生成**坏账核销凭证（ARCH-7 §7.2 ④ 的 6702/1122 分录要等「收入/减值凭证」那组卡先把科目映射与 `biz_voucher_rule` 种子落地；手搓分录过不了 `voucher::post_voucher`）。`voucher_id` 原样保留不动，核销只改状态 + `settled_at` + `remark`。出库 → 应收挂账联动属 SO-14（`receivable_bridge.rs`）。

---

## 二、账龄「与应付同口径」是怎么保证的（不是抄一份）

`receivable_aging.rs` **没有**自己的档位表：它 `use payable_aging::{BUCKETS, bucket_of, bucket_case_sql, money, overdue_days, resolve_as_of, parse_date, AsOf, AgingBucketDef, AgingBucketAmountRes, DETAIL_LIMIT, load_period_status}`。

三条独立证据：

1. **单测 `bucket_boundaries_are_identical_to_payable`**：−400..=400 逐天比对 + 11 个边界值钉住 + 档位键顺序钉住。
2. **单测 `db_bucket_boundaries_and_backet_source_agree`**：真库里 SQL 侧 `CASE` 聚合出来的各档金额 与 Rust 侧 `bucket_of(due_date)` 现算的金额**逐档相等**（分档漂移立刻红）。
3. **curl 实测第 2 步**：同一批到期日（逾期 0/1/30/31/60/61/90/91 天）各造 8 张应收 + 8 张应付，两个接口出参逐档比对相同：

```
应收各档：[{"key":"NOT_DUE","unpaid":"100.0000"},{"key":"D0_30","unpaid":"200.0000"},
          {"key":"D31_60","unpaid":"200.0000"},{"key":"D61_90","unpaid":"200.0000"},
          {"key":"OVER_90","unpaid":"100.0000"}]
应付各档：[{"key":"NOT_DUE","unpaid":"100.0000"},{"key":"D0_30","unpaid":"200.0000"},
          {"key":"D31_60","unpaid":"200.0000"},{"key":"D61_90","unpaid":"200.0000"},
          {"key":"OVER_90","unpaid":"100.0000"}]
PASS  各档金额逐个相同
PASS  明细档位聚合 == 汇总档位（Rust/SQL 同源）
```

业务口径上的**必然差异**（不是不一致）：应收只纳入 `OPEN / PARTIAL`（`SETTLED` 无余额、`WRITTEN_OFF` 已核销不再算资产；对应付排除 `CANCELLED`）；金额列 `unsettled_amount` ← `unpaid_amount`；对手方 `biz_customer.name` ← `biz_supplier.supplier_name`。

---

## 三、门禁与实测

### 1) 编译 / 静态检查

```
cargo fmt                                                        → rc=0
cargo clippy --all-targets --features test-hooks -- -D warnings   → rc=0（0 告警）
cargo check --all-targets                                        → rc=0（0 告警）
```

### 2) 单测（真库：`ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_so13`）

```
cargo test --lib business::erp::receivable -- --test-threads=1
test result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 2771 filtered out; finished in 6.75s
```

31 条 = `receivable` 15 条 + `receivable_aging` 9 条 + `receivable_query` 7 条，覆盖卡面点名的三类：

| 卡面要求 | 用例 |
|----------|------|
| 正常分页 | `db_page_returns_fixtures_with_total`（总数/排序/客户名 JOIN/状态筛选/客户筛选/到期日区间）、`db_page_pagination_is_stable`（第 3 页 1 条、越界页空列表不报错） |
| 账龄分桶边界 | `bucket_boundaries_are_identical_to_payable`、`db_bucket_boundaries_and_backet_source_agree`（0/1/30/31/60/61/90/91 各一档）、`db_period_cutoff_excludes_later_documents`、`assemble_computes_ratio_and_detail_buckets` |
| 坏账核销 | `db_write_off_sets_terminal_state_and_reason`（WRITTEN_OFF + settled_at + 日志 + 二次核销 409）、`db_write_off_respects_state_domain`（SETTLED 409 / PARTIAL 可核销）、`write_off_reason_validation`、`db_customer_gate_blocks_frozen_and_missing` |

另外 3 组防线：`status_domain_matches_check_constraint`（`PARTIAL_PAID`/`CANCELLED` 不许进来）、`legal_edges_match_arch7_state_graph`（5 条边 + 终态 + requires_reason）、`db_param_validation_happens_before_any_write`（参数错不落任何行）。

### 3) 5 接口真实 token curl（连跑两轮，各 41 条断言）
脚本：`docs/evidence/20261106-so13-receivable-aging/curl-5-endpoints.sh`
实例：`./target/debug/cloudpivot` @ `127.0.0.1:18114`，库 `cloudpivot_so13`，admin 真 token。

```
第一轮：==> 汇总：PASS=41 FAIL=0    （exit 0）
第二轮：==> 汇总：PASS=41 FAIL=0    （exit 0）
```

逐接口结论（摘录）：

```
POST   /api/business/erp/receivable            → 200 / code 0 / OPEN / 单号 AR-20261106-000008 / 未收=1234.56
GET    /api/business/erp/receivable/{id}       → 客户名「应收演示客户」/ 标签「未收」/ voucher_no=null
GET    /api/business/erp/receivable/page       → total=9 / page_size=5 生效 / 全 OPEN / status=PARTIAL_PAID → 10001
GET    /api/business/erp/receivable/aging      → RECEIVABLE_AGING / 5 档 / 未到期档 1334.5600
PATCH  /api/business/erp/receivable/{id}/write-off → WRITTEN_OFF / settled_at 落值 / 原因进 remark
反向：终态再核销 10003 / 缺原因 10001 / 非法 UUID 66006 / 不存在 10002 / amount=0 10001
库层：biz_status_log(entity_type=RECEIVABLE) = 2 行
      NULL->OPEN | 手工挂账
      OPEN->WRITTEN_OFF | 客户已注销，经理批准核销（SO13 curl 演示）
```

### 4) 前端（改了 `doc-kinds.ts`）

```
pnpm typecheck                                   → rc=0
NODE_OPTIONS=--max-old-space-size=4096 pnpm build → rc=0
```

> 注：默认 heap 下 `vite build` 在 1.5 GB 触发 V8 OOM（与其他 agent 的 cargo 编译并发、内存紧张），
> 加 `NODE_OPTIONS=--max-old-space-size=4096` 后通过；与本卡代码无关（`vue-tsc --noEmit` 全程 0 错）。

### 4b) 全量回归

```
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so13' \
  cargo test --lib --features test-hooks -- --test-threads=1

test result: FAILED. 2801 passed; 3 failed; 0 ignored; … finished in 1313.11s
```

3 条失败**逐条归因**：

| # | 用例 | 归因 | 处置 |
|---|------|------|------|
| 1 | `business::flow::linkage::tests::triggers_cover_state_machine_entities` | **本卡引入**：登记了 `Entity::Receivable` 却没进联动触发点白名单/例外清单 | 已修：`flow/linkage/spec.rs::TRIGGER_EXEMPT` 加 `ENTITY_RECEIVABLE`（与 SALE_ORDER / SHIPMENT 同口径，本卡状态流转不调联动引擎）→ 复跑 `business::flow::linkage` **65/65 全绿** |
| 2 | `business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean` | **既有缺陷（SO-06 线）**：`flow_graph_selfcheck` 报 `so_approve`（def `…d0281`）`RowsEdited`，stored_hash `da3c2eed…` ≠ recomputed `97fe9377…`。只有 `V0281__sales_order_flow` 建过这个流程，本卡两个迁移（287/288）**不碰** `biz_flow_def*` | 单跑这条也失败（不是顺序污染）；SO-09/SO-10 证据里已记录同一现象 |
| 3 | `business::erp::sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved` | **既有缺陷（同属 so_approve 那条流程）**：`assert left ""==right "APPROVED"` 状态没推进 | 单跑也失败；同上 |

> 结论：本卡范围内的 31 条新单测 + `business::flow::linkage`(65) + `business::shared::`(113)
> 全绿；全量剩下的 2 条是 `so_approve` 流程的既有缺陷（与本卡改动无关，本卡不碰 flow 定义）。

### 5) 迁移
```
schema_migrations:
  287 | sales_receivable             | versioned | executed | success=t   （父卡 SO-12）
  288 | sales_receivable_permissions | versioned | executed | success=t   （本卡）
启动日志：NOTICE: V0288：应收 5 个权限码已种（页面节点隐藏且 path 待前端卡补），4 个按钮挂在页面下
sys_menu（5 行，页面 is_visible=f、path 留空等 SO-15）：
  20000000-…-2880  2  business:erp:receivable:list        icon=money
  20000000-…-2881  3  business:erp:receivable:detail      parent=…2880
  20000000-…-2882  3  business:erp:receivable:aging       parent=…2880
  20000000-…-2883  3  business:erp:receivable:create      parent=…2880
  20000000-…-2884  3  business:erp:receivable:write-off   parent=…2880
```

干净库：`REPO=/opt/cloudpivot ERP_DB=cloudpivot_so13 RECREATE=1 PORT=18113 bash scripts/verify/erp_clean_db.sh`
→ `schema_migrations(max|count) = 288|216`、超管 1 人（`assignee_type=ADMIN` 节点会签人数不变）。

---

## 四、复现方式

```bash
# 1) 干净库（含 V0287 表 + V0288 权限码）
REPO=/opt/cloudpivot ERP_DB=cloudpivot_so13 RECREATE=1 PORT=18113 bash scripts/verify/erp_clean_db.sh

# 2) 单测
cd /opt/cloudpivot/backend
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_so13' \
  cargo test --lib business::erp::receivable -- --test-threads=1

# 3) 起实例 + curl 闭环（脚本自带夹具与断言）
NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_so13 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:18114 NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
./target/debug/cloudpivot &
BASE=http://127.0.0.1:18114/api bash docs/evidence/20261106-so13-receivable-aging/curl-5-endpoints.sh
```

⚠ 该 curl 脚本会**清空**演示库的 `biz_receivable` / `biz_payable`（账龄是全库口径，只有表里没有别人的夹具
才能做「应收 vs 应付各档逐个相同」的严格比对）。只在一次性隔离库上跑。
