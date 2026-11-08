# CTR-02-F1 证据：开放 SERVICE / LEASE 合同类型 + 乙方固定供应商 + `commitment_scope`

- 收口日期：2026-11-09（北京时间）
- 卡：`t_07316875`（后端）；口径来源：`docs/合同类型开放口径-决策.md`（卡 `t_be837864`）
- 环境：本机（172.18.10.99）**隔离空库 `cloudpivot_ctr02`**（首启自动跑完 **285** 个迁移）+
  独立实例 `127.0.0.1:18228`（`profile=test`）。**未碰 :8023 / 145 测试实例 / 144 生产**。
- 二进制：`sha256=0891c381e06e1281e77fb4f70aa7bf8ad006629bce61a266e3c7bee2c026dffa`
  （隔离 worktree `/root/ctr02f1-wt`，`HEAD=356c57d` + 本卡 3 个 `.rs`；`CARGO_TARGET_DIR=/root/ctr02f1-target`）
- **实测：68 条断言全绿（PASS=68 / FAIL=0）**，原始输出 `drill_out.txt`，逐请求响应 `raw/`，
  断言计数 `asserts.txt`
- **零迁移**：`backend/migrations/business` 最大仍是 `V0352`，本卡未新增任何迁移文件
  （`git status` 只有 3 个 `.rs` + 3 个文档 + 本证据目录）

> 一句话结论：**四类合同全开**（`PURCHASE` / `SALES` / `SERVICE` / `LEASE`），
> 服务 / 租赁的乙方**只能**从供应商主档选（传 `CUSTOMER` → 400，建单与改单都拦），
> 审批仍走 `contract_approve`、10 万阈值不变（实测服务 5 万单级 `n_pm`、租赁 12 万两级 `n_pm_hi → n_gm`），
> 详情新增 `commitment_scope`：采购/销售 `ORDER_LINKED`、服务/租赁 `NOT_APPLICABLE`
> （`committed_amount=0` 是**正确值**，不是「未执行」）。订单侧类型闸**未动**。

---

## 1. 代码改动（3 个文件，行为变更点）

| # | 文件 | 改动 |
|---|---|---|
| 1 | `backend/src/business/erp/contract_dto.rs` | `SUPPORTED_TYPES` 2 值 → **4 值**（= `CONTRACT_TYPES`）；`ensure_supported_type` 保留，文案改指 `ck_biz_contract_type` 白名单；新增 `ensure_party_b_type`（乙方类型 ↔ 合同类型配对，POST/PUT 共用）；`ContractDetailRes` 新增 `commitment_scope`（`From<&ContractRow>` 用纯函数填） |
| 2 | `backend/src/business/erp/contract_model.rs` | 新增 `required_party_b_type`（`SALES`→`CUSTOMER`，其余→`SUPPLIER`）；新增 `COMMITMENT_SCOPE_ORDER_LINKED` / `COMMITMENT_SCOPE_NOT_APPLICABLE` + 纯函数 `commitment_scope_of` |
| 3 | `backend/src/business/erp/contract_handler.rs` | POST 沿用 `resolve_party_b_type`（已收窄到单向）；**PUT 新增**：客户端传 `party_b_type` 时按**库里的** `contract_type` 过 `ensure_party_b_type`（草稿也不许把服务合同的乙方类型说成客户） |

未改：`purchase_handler.rs:219` / `sales_order_handler.rs:631`（订单侧类型闸保持原样）、
`biz_supplier` 无新列、审批流程 / 角色 / 条件边零改动、DDL 零改动。

### 1.1 口径与实现的一处细节（写回 ARCH-2 §3.2.3）

决策文档说「应用层强制」，落到代码是**两个调用点**：POST 的 `resolve_party_b_type`（派生 + 冲突判定）
与 PUT 的 `contract_update`（用库里的冻结 `contract_type` 判定）。已在
`docs/architecture/ARCH-2-合同表数据模型与校验规则.md` §3.2.3 的「2026-11-09 口径更新」注里
补了「实现落点」表格（文件 / 函数 / 错误码），评审可以直接从文档跳到代码。

### 1.2 列表要不要 `commitment_scope`？

**没加**。列表 DTO（`ContractRes`）本来就不算 `committed_amount`（只有详情算），
所以不存在「列表有金额、详情说不适用」的矛盾；`s4_page` 断言
`list[0] | has("committed_amount") == false` 钉住了这条，避免将来有人给列表加履约列时漏口径。

---

## 2. 验收对照（卡面 8 条）

| # | 验收项 | 结果 | 证据 |
|---|---|---|---|
| 1 | `POST /api/business/erp/contract` 建 `SERVICE`（乙方=供应商）→ 200，`contract_no` = `HT-…`；`LEASE` 同理 | ✅ | `raw/s2_srv.json`（`HT-20261109-000002`，`party_b_type=SUPPLIER`）、`raw/s2_lse.json`（`HT-20261109-000003`）、`drill_out.txt` S2 |
| 2 | `SERVICE` + `party_b_type=CUSTOMER` → 400，文案可读（无 SQLSTATE） | ✅ | `raw/s2_srv_cust.json`：`400 / 10001`，「服务（SERVICE）合同的乙方类型必须是 SUPPLIER（服务商在系统中登记为供应商），与 party_b_type=`CUSTOMER` 冲突」；断言同时检查文案不含 `SQLSTATE/23514`。`LEASE + CUSTOMER` 同样 400（`raw/s2_lse_cust.json`） |
| 3 | `SERVICE` + 供应商已停用/软删 → 仍 400（`party_b_ref_for_new` 口径不回退） | ✅ | `raw/s2_srv_off.json`：`400 / 66021`「乙方供应商 … 不存在、已删除或已停用（V-11：停用供应商不得新签合同）」；停用确经 API 写库（`raw/s1_sup_disable.json`，`status` 0→库内为 0） |
| 4 | `GET /contract/{id}`：`PURCHASE` → `ORDER_LINKED`；`SERVICE`/`LEASE` → `NOT_APPLICABLE` 且 `committed_amount=0` | ✅ | `raw/s4_pur.json` / `raw/s4_srv.json` / `raw/s4_lse.json`（后者还有 `remaining_amount=50000.0000`、`open_order_count=0`、`open_order_nos=[]`） |
| 5 | 审批走 `contract_approve`：`<10 万` 落 `n_pm`（单级）；`≥10 万` 落 `n_pm_hi → n_gm`（两级），两类合同各一条 | ✅ | 服务 5 万：`raw/s5_sub_srv.json` + 库断言 `首节点=n_pm`、待办数 1、轮次 1（`raw/s5_flow.txt`）；租赁 12 万：首节点 `n_pm_hi`、节点顺序 `n_pm_hi,n_gm,`、轮次 2（`raw/s5_flow.txt`、`raw/s5_lse_approve_r{1,2}.json`）。制单人自批仍 `403`（`raw/s5_self.json`） |
| 6 | `LEASE` 合同 `end_date=today+7` → `erp_contract_expire_scan` 命中 | ✅ | `raw/s6_scan.json`：`scanned=1 sent=1 skipped_reminded=0`；库：`biz_reminder_log` 1 行、`stage=D-7`、`remind_kind=CONTRACT_EXPIRE`；二次扫描 `sent=0` 且台账仍 1 行（幂等）。**未改** `reminder_contract.rs` 的角色 |
| 7 | `cargo test --lib` 全绿；合同域真库用例全绿；`cargo fmt --check` + `cargo clippy -- -D warnings` 零新增 | ✅（含基线说明，见 §3） | `tests_cargo_full_lib.txt`：**3858 passed / 0 failed**；`tests_cargo_contract_db.txt`：`ERP_DB_URL=…/cloudpivot_ctr02` 下**81 passed / 0 failed**（49.45s，真连库）；`clippy_lib.txt` / `fmt_cargo_check.txt`：3 个 clippy 错误 + 8 个 fmt 文件**全在未触碰的他人文件**，本卡 3 文件 `rustfmt --check` 通过 |
| 8 | 证据落 `docs/evidence/<日期>-ctr02-f1/`（README + 原始响应 + 断言计数） | ✅ | 本目录；`asserts.txt` = `PASS=68 FAIL=0` |

> 额外断言（卡面没点名但本卡必须证明的）：**PUT 草稿改单也过配对校验** ——
> `PUT SERVICE {party_b_type:CUSTOMER}` → `400/10001`，且**合同仍 DRAFT、备注未被改**
> （证明是校验拦住而不是写到一半）；`PUT {party_b_type:SUPPLIER, remark:…}` → 200。
> 见 `raw/s3_put_bad.json` / `raw/s3_put_ok.json` / `drill_out.txt` S3。

---

## 3. 检查链原始结果（含基线红点归属）

```text
# 合同域真库用例（隔离库 cloudpivot_ctr02，串行）
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_ctr02 \
  cargo test --lib business::erp::contract -- --test-threads=1
→ test result: ok. 81 passed; 0 failed; 0 ignored   （49.45s，确实是真连库：未设变量的同命令 0.10s）

# 全量 lib（未设 ERP_DB_URL，DB 用例按既有约定自跳过）
cargo test --lib -j 2
→ test result: ok. 3858 passed; 0 failed; 0 ignored

# clippy
cargo clippy --lib -- -D warnings
→ EXIT=101，3 个 error，全部在**本卡未触碰**的文件：
   src/business/erp/invoice.rs:1967        （explicit lifetimes could be elided — 既有）
   src/business/erp/receivable.rs:800      （doc list item without indentation — 既有）
   src/business/erp/sales_order_model.rs:699（too many arguments 8/7 — 既有）
# 证明「零新增」：把这三条既有 lint 临时 allow 后重跑
cargo clippy --lib -- -D warnings -A clippy::needless_lifetimes \
  -A clippy::doc_lazy_continuation -A clippy::too_many_arguments
→ EXIT=0（clippy_lib_baseline_allowed.txt）

# fmt
cargo fmt -p cloudpivot -- --check
→ EXIT=1，Diff 落在 8 个**他人文件**（HEAD 上就未格式化）：
   asset_compute.rs / asset_linkage.rs / asset_model.rs / currency_admin_model.rs /
   opening.rs / report.rs / transfer_handler_common.rs / transfer_handler_db_tests_settle.rs
# 本卡 3 个文件单独检查（scripts/check.sh 同口径）
rustfmt --edition 2024 --check contract_dto.rs contract_model.rs contract_handler.rs
→ PASS（无输出）
```

> `cargo fmt -p cloudpivot --check` 的基线红**不是本卡引入**：worktree 里 `git status` 对这 8 个文件是干净的
> （= HEAD 原样），本卡只改了 3 个 contract 文件。这也是不跑 `cargo fmt` 全仓的原因 ——
> 会把别人的文件一起重排，制造无主 diff。

---

## 4. 复跑方式

```bash
cd docs/evidence/20261109-ctr02-f1
bash tools/00_env.sh up          # 空库 cloudpivot_ctr02 + 127.0.0.1:18228（首启自动迁移）
bash tools/01_drill.sh           # 期望 PASS=68 FAIL=0（会重建一份 raw/）
bash tools/00_env.sh down        # 停实例（库保留，便于复跑）
```

`00_env.sh` 的二进制路径是隔离 worktree `/root/ctr02f1-wt` + 独立 target
`/root/ctr02f1-target`；要复跑需先 `cargo build --bin cloudpivot`（同 target）。
`raw/*.json` 里的 `access_token` / `refresh_token` 已打码为 `<redacted>`。

## 5. 遗留 / 未做（都在卡外）

- 前端（四类下拉、乙方 label 文案、`NOT_APPLICABLE` 不渲染 0.00）由 `t_98ce6eab` 做，本卡只交后端字段与口径。
- 合同付款计划成为真正履约单据 → RP-2026-005；供应商分类列 → RP-2026-006；按类型分流审批 → RP-2026-007；
  服务采购走采购单（费用化采购）→ RP-2026-008。
- 订单侧类型闸按卡面**保持原样**，未做「服务/租赁挂单」的任何放开。
