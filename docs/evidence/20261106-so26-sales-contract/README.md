# SO-26 销售合同：放开 SALES + 客户关联（交付说明）

> 卡：`t_524de96e`【SO-26】｜依据：`docs/architecture/ARCH-7-销售模块设计.md` §15.1
> 结论：**SALES 已放开**，`party_b_type=CUSTOMER` 时 `party_b_id → biz_customer.id`，
> `SERVICE` / `LEASE` 仍拒；无表结构改动（启动日志「应用 0 个迁移」）；真机 curl 两轮 PASS=39 / FAIL=0。

## 1. 改了什么（三处代码 + 两个验收脚本）

| 文件 | 位置 | 改动 |
|---|---|---|
| `backend/src/business/erp/contract_dto.rs` | `SUPPORTED_TYPES` | `[&str; 1] = ["PURCHASE"]` → `[&str; 2] = ["PURCHASE", "SALES"]` |
| 同上 | 新增 `ensure_supported_type()` | **类型闸从 handler 提到 dto 的纯函数**，`CreateContractReq::validate()` 调用 ⇒ 400 文案「只支持 PURCHASE / SALES」只有一处出口，单测直接覆盖 `SERVICE` / `LEASE` |
| 同上 | `resolve_party_b_type` 文档 | `SALES → CUSTOMER`（`party_b_id` 指向 `biz_customer.id`），不再是「转 66020」 |
| `backend/src/business/erp/contract_model.rs` | `party_b_lookup()`（新，私有） | `party_b_ref` / `party_b_ref_for_new` 的**单一实现**：`SUPPLIER` 查 `biz_supplier.supplier_name`（新签 `status = 1`）、`CUSTOMER` 查 `biz_customer.name`（新签 `status = 'ACTIVE'`）；⚠️ 客户状态是字符串域值，写成 `status = 1` 会恒判 0 行 |
| 同上 | `CONTRACT_LIST_COLUMNS` / `CONTRACT_JOIN` | 乙方现值改 `COALESCE(s.supplier_name, cu.name)`，两个 `LEFT JOIN` 各带 `party_b_type` 条件 ⇒ 列表 / 详情对 SALES 合同也能带出客户现值 |
| `backend/src/business/erp/contract_handler.rs` | `contract_create` | 删掉两处 `66020 customer_unsupported` 硬拒；类型闸交给 `validate()`；乙方不可用文案按类型分叉（`party_label` / `party_missing_for_new`：供应商「已停用」/ 客户「已冻结」，仍 400 + 66021） |
| 同上 | `contract_update` | 换乙方时的 66021 文案同样按类型分叉；`Uuid` 导入补全 |

**行为契约（对外）**

| 请求 | 结果 |
|---|---|
| `POST /business/erp/contract`，`contract_type=SALES`，`party_b_id`=ACTIVE 客户 | 200；`party_b_type=CUSTOMER`、`party_b_name`=客户主档名（服务端快照）、`party_b_current_name`=客户现值 |
| `contract_type=SERVICE` / `LEASE` | 400 + `10001`，文案 `contract_type \`SERVICE\` 本批次未开放：只支持 PURCHASE / SALES` |
| `contract_type=SALES` + `party_b_id` 不存在 / 已软删 / 已冻结（FROZEN） | 400 + `66021`，文案点名「乙方客户 … 已冻结（V-11：冻结客户不得新签合同）」 |
| `contract_type=SALES` + 显式 `party_b_type=SUPPLIER` | 400 + `10001`（推导与显式冲突） |
| `contract_type=PURCHASE`（回归） | 200；`party_b_type=SUPPLIER`、快照/现值仍走 `biz_supplier` |

66020 `ERP_CONTRACT_CUSTOMER_UNSUPPORTED` 码值**保留不删**（历史日志 / 前端映射不报未知码），handler 不再产生。

## 2. 迁移

**无**（卡面第 4 条）。`biz_contract` 已是 `party_a / party_b` 泛化设计，`party_b_id` 是弱引用（DEC-02），
客户分支只改读哪张主档表，不动 DDL。

证据：`raw/migration_apply.txt` = `数据库迁移完成: 应用 0 个迁移`；`raw/db_evidence.txt` =
`migrations=227`、`new_sales_contract_migrations=0`、`biz_contract_cols=24`（与改前一致）。

## 3. 测试证据

### 3.1 单测（真库，`ERP_DB_URL=…/cloudpivot_so26`）

```
cargo test --lib contract                      → 93 passed; 0 failed（raw/cargo_test_contract.txt）
cargo test --lib business::erp::contract_model::tests::party_b_ref
                                               → 7 passed; 0 failed（真库，无 SKIP 行）
```

新增 / 改写的用例：

| 用例 | 钉住什么 |
|---|---|
| `contract_dto::tests::sales_is_supported_but_service_and_lease_are_not` | `SUPPORTED_TYPES == ["PURCHASE","SALES"]`；SALES 过校验；SERVICE/LEASE 400 + 文案点名放行集（`validate` 与 `ensure_supported_type` 双出口） |
| `contract_dto::tests::sales_with_supplier_party_type_is_rejected` | SALES 显式传 SUPPLIER → 400 |
| `contract_dto::tests::sales_contract_derives_customer_party_type`（改写） | SALES 推导 CUSTOMER |
| `contract_model::tests::party_b_ref_customer_branch_reads_customer_name`（原 `…_branch_is_closed` 反转） | 读到 `biz_customer.name`；FROZEN 在通用解析器里仍可取名字（只拦新签）；未知类型 None |
| `contract_model::tests::party_b_ref_customer_missing_or_deleted_is_none` | 不存在 / 已软删 → None（handler 据此 66021） |
| `contract_model::tests::party_b_ref_for_new_only_accepts_active_customer` | 新签只认 `status='ACTIVE'`；FROZEN 拒；客户 id 不能当供应商 id 用 |

### 3.2 全量 `cargo test`（两档口径都跑了）

| 口径 | 结果 | 说明 |
|---|---|---|
| 不带 `ERP_DB_URL` | **3082 passed / 0 failed**（23 个 suite） | 真库用例按仓库约定打印 `SKIP` 后返回 ⇒ 只代表纯逻辑全绿 |
| 带 `ERP_DB_URL=cloudpivot_so26` | **2846 passed / 7 failed** | 7 条红**全部**在合同域之外，且都是「继承数据 / 跨模块并行」导致，见下 |

7 条红逐条归因（`raw/cargo_test_full_summary.txt`）：

| 用例 | 原因 | 与本卡的关系 |
|---|---|---|
| `receivable_aging::db_{bucket_boundaries,customer_pivot,period_cutoff}`（3 条） | `DELETE FROM biz_receivable` 被 `biz_invoice_receivable_id_fkey` / `biz_receipt_allocation_receivable_id_fkey` 挡住——模板库 `cloudpivot_so25` 里带着 13 条应收 / 1 条收款 / 2 张发票的旧业务数据 | 无（合同域不碰应收/发票） |
| `payable_bridge::p17_skip_approval_payment_logs_are_purged_by_id` | 「`payable_pay` 已发布：有流程时必须走审批」——库内已有已发布流程定义 | 无；**单跑该用例 = 1 passed** ⇒ 跨模块并行干扰 |
| `sales_approval_bridge::db_tests::{three_tiers_start_flow_and_approve_to_approved, reject_returns_order_to_draft}` | 取到的待办 `node_key` 为空（库内 `so_approve` 流程版本快照来自邻居卡的 V0301 重写），测试预期 `n_sales` | 无（sales order 审批链，本卡未触碰） |
| `stocktake_tests::zz_teardown_purges_stocktake_fixtures` | teardown 清理被并行跑的其他模块夹具占用 | 无；**单跑该用例 = 1 passed** |

> 根因是**测试库不干净**，不是代码：本卡实测「空库按版本顺序重放迁移」目前会在 `V0267` 直接失败
> （`V0267 自检 6.1 失败：以下销售权限码在 sys_menu 里没有承载 → business:erp:shipment:list …`，
> 因为 shipment/receivable/receipt/invoice 的菜单行由更晚的 V0287–V0296 建立，而 V0267 的号段在前）。
> 所以现阶段拿不到「干净装配库」，只能用模板库副本；这条缺陷属父卡 SO-25 的迁移号段问题，**已单独建卡**。

### 3.3 真机 curl（验收第三条）

```
bash scripts/verify/so26_instance.sh up
bash scripts/verify/so26_sales_contract_e2e.sh      → PASS=39 FAIL=0（连跑两轮，结论一致）
```

覆盖：`/health` 200 → admin 登录 → 建 ACTIVE / FROZEN 两个客户 → 建 SALES 合同（类型 / 乙方类型 /
乙方 id / 快照名 / 现值 / 状态 / 合同号前缀）→ 详情 + 分页（`contract_type=SALES`）带出客户现值 →
库内三元组 `SALES|CUSTOMER|SO-26 销售合同客户` → SERVICE / LEASE 400 + 10001 → 幽灵客户 400 + 66021 →
FROZEN 客户 400 + 66021 → SALES+SUPPLIER 冲突 400 → PURCHASE 回归 200。
逐份响应体在 `raw/*.json`（JWT 已脱敏），日志 `e2e.txt`。

留证的成交合同：`HT-20261107-000039`（第一轮）、`HT-20261107-000040`（第二轮）、
`HT-20261107-000100`（第三轮，**用 rustfmt 后的最终源码重建二进制再跑**——`target/debug/cloudpivot`
mtime 00:38:45 晚于最后一次 `.rs` 修改），见 `raw/db_sales_contract.txt` 与 `e2e.txt`（`e2e.txt` 即第三轮日志）。

## 4. 前端（不在本卡，已被子卡覆盖）

`frontend/src/views/business/erp/contract/contract-logic.ts:125` 仍是
`SUPPORTED_CONTRACT_TYPES = ['PURCHASE']`，表单乙方选择器也还是供应商。
这属 ARCH-7 §15.1.2 ⑥ 的前端部分，由子卡 **SO-27（t_ef63b009，第 4 条）** 负责，本卡不动前端。

## 5. 复现命令

```bash
# 库：从 227 条迁移的模板库复制（本卡无新迁移）
su postgres -s /bin/sh -c "psql -c 'CREATE DATABASE cloudpivot_so26 OWNER alun TEMPLATE cloudpivot_so25'"

cd /opt/cloudpivot/backend && export PATH=/root/.cargo/bin:$PATH
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_so26' cargo test --lib contract

cd /opt/cloudpivot
bash scripts/verify/so26_instance.sh up            # :18226
bash scripts/verify/so26_sales_contract_e2e.sh
bash scripts/verify/so26_instance.sh down
```
