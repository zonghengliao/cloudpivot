# SO-24 端到端链路测试（ARCH-7 §12.2 十步）· 证据

> 卡号：`t_1a97098e`【SO-24】端到端链路测试
> 提交：`c6feedb`（证据与脚本；被验的代码版本是提交前的 `bb67777`）
> 结论：**10 步全部通过**（47/47 断言绿）+ **销售不变量 8/8 全绿（非空覆盖）** + **全量巡检零新增红**（与未跑链路的对照库同为 73/76）+ **连跑两轮逐行一致**。
> 产品代码**零改动**、**未新增迁移**（本卡只交证据与验证脚本）。

---

## 1. 环境与方法（为什么这份证据可信）

| 项 | 值 |
|---|---|
| 代码版本 | `bb67777`（本卡提交前 HEAD） |
| 后端二进制 | `backend/target/debug/cloudpivot`，构建于 `2026-11-06 23:31:42`，sha256 前 16 位 `5a608eaa8a7333b8`（两轮跑的是同一个二进制） |
| 数据库 | `cloudpivot_so24` —— **从 `cloudpivot_so23fresh`（空库 + 225 条迁移，零业务夹具）复制**，本卡每条链路都从这份干净模板重建 |
| 迁移口径 | `schema_migrations` 225 条，`applied_via not in ('executed','imported')` = **0**（`raw/migration_check.txt`） |
| 后端端口 | `127.0.0.1:18224`（隔离段 18000–18999，与 SO-23 的 18223 不撞） |
| 认证 | **真实 token**：`POST /api/system/auth/login` → Bearer JWT；每一步都是真 curl（响应体逐条落 `raw/*.json`） |
| 单据写入路径 | 全部走 HTTP API：凭证 `biz_voucher`、库存 `inventory_writer`（经出库过账内核 / 期初模块）、流程引擎、`LEGAL_EDGES` 状态机 —— **没有一条直写三本账的 SQL** |
| 脚本 | `scripts/verify/so24_e2e.sh`（`up` 建库起实例 / `chain` 跑 10 步 / `down` 停实例）、`scripts/verify/so24_control.sh`（对照库全量巡检）、`scripts/verify/so24_round_diff.sh`（两轮 diff） |

**基线数据只有迁移种子**（5 物料 / 3 仓库 / 9 角色 / admin），因此有两条**前置**（不属于 §12.2 十步，但链路跑不起来）：

1. **种子管理员首登改密**：`must_change_password=TRUE` 时除改密/登出全 403（`middleware/session_guard.rs:112`）。走真实接口 `PUT /api/system/auth/password` 改密（`Admin@123456` → `Admin@So24!2026`），再重新登录。
2. **库存期初**：干净库 0 库存，无法出货。走真实接口 `POST /api/business/erp/opening-balance/inventory`（2026-11，WH01：M-W1A-001 ×200 @300、M-W1A-002 ×100 @400），三本账由 opening 模块经 `inventory_writer` 落库，生成 `PZ-20261106-000001|OPENING|100000/100000`（`raw/pre_inventory.txt`）。
3. **审批人账号**：`so_approve` 流程要求「制单人 ≠ 审批人」（`sales_approval_bridge::guard_self_approval`）。建 `so24_sales_mgr`（角色：SALES_SUPERVISOR + FINANCE_MANAGER + GENERAL_MANAGER + SUPER_ADMIN）作为审批人，销售单由 `admin` 制单。

---

## 2. 十步结果（每步：断言 + 数据库快照）

| 步 | 动作 | 实测结果 | 证据 |
|---|---|---|---|
| ① | 建客户「测试客户A」，信用额度 10 万 | `C-SO24-001 / 测试客户A / credit_limit=100000.0000 / credit_days=30` | `raw/step01.txt`、`raw/step01_customer.json` |
| ② | 销售订单 5 万（不含税）+2 物料，提交→审批通过 | `SO-20261106-000001`：不含税 `50000.0000`、税 `6500.0000`、价税合计 `56500.0000`；明细 2 行（M-W1A-001 100×300、M-W1A-002 50×400）；`submit-approval` → `SUBMITTED`；审批走 **n_sales → n_finance 两个节点**（金额 56500 落在 `10000 ≤ x < 100000` 档）→ 实例 `APPROVED`、单据 `APPROVED` | `raw/step02_so.txt`、`raw/step02_submitted.txt`、`raw/step02_approved.txt`、`raw/step02_approve_r1.json`、`raw/step02_approve_r2.json` |
| ③ | 发货单只发 1 个物料（部分发货） | `SH-20261106-000001` `DRAFT`，仅 line1 全量 100（line2 未发） | `raw/step03.txt` |
| ④ | 提交 + 过账 | `DRAFT → PENDING_POST → POSTED`；**出库单** `OUT-20261106-000001`；**库存** M1 200→100；**三账一致** M1 `100/100/100`；**成本凭证** `PZ-…000002` 借 6401 `30000` / 贷 1405 `30000`；**应收** `AR-20261106-000001` `33900.0000`；**订单** `PARTIAL_OUT`，line1 `delivered_qty=100.0000`、line2 `0.0000` | `raw/step04_after_post.txt`、`raw/step04_submit.json`、`raw/step04_post.json` |
| ⑤ | 第二张发货单发剩余 | `SH-20261106-000002` 过账 → **订单 `OUTBOUND`**；M2 100→50；`Σ应收 = 56500.0000`（33900 + 22600） | `raw/step05.txt`、`raw/step05_shipment_create.json`、`raw/step05_post.json` |
| ⑥ | 开票 5 万（不含税），税率 13% | 按 2 张应收分别开：`IV-…000001` 不含税 `30000` 税 `3900` 合计 `33900`；`IV-…000002` 不含税 `20000` 税 `2600` 合计 `22600`；两张均 `DRAFT → ISSUED` | `raw/step06_invoice1.json`、`raw/step06_invoice2.json`、`raw/step06_issue1.json`、`raw/step06_issue2.json` |
| ⑦ | 收入凭证 | `PZ-…000004` 借 1122 `33900` / 贷 5001 `30000` + 2221.02 `3900`；`PZ-…000005` 借 1122 `22600` / 贷 5001 `20000` + 2221.02 `2600`；**合计 借 1122 `56500.0000` / 贷 5001 `50000.0000` + 2221.02 `6500.0000`** | `raw/step07_revenue_voucher.txt` |
| ⑧ | 收款 5.65 万并核销 | `RC-20261106-000001` `56500.0000`；核销 2 张应收（33900 + 22600 = 56500）；`DRAFT → CONFIRMED → POSTED`；`PZ-…000006` 借 1002 `56500` / 贷 1122 `56500` | `raw/step08_*.json`、`raw/step09_final.txt` |
| ⑨ | AR SETTLED / 信用占用归零 | 2 张应收全部 `SETTLED`，`unsettled=0.0000`；客户 `C-SO24-001` 未结应收 `open_ar=0.0000`（额度 100000） | `raw/step09_final.txt` |
| ⑩ | `erp_invariants.py` | **销售组 8/8 通过，BLOCKER 0，WARN 0**（非空覆盖见 §5） | `raw/step10_invariants.txt` |

链路共产生 **1 订单 / 2 发货单 / 2 销售出库单 / 2 应收 / 2 发票 / 1 收款单 / 2 核销行 / 6 张凭证**（`raw/round1_console.txt` 末尾「单据汇总」）。

---

## 3. 与 ARCH-7 §12.2 的实测订正（文档 vs 实现，5 处）

链路全通，但按稿面字面复核有 5 处**口径差异**。以下以实测为准，建议 ARCH-7 同步修订（本卡不改文档，只登记）：

| # | §12.2 / 相关章节写法 | 实测（本卡证据） | 说明 |
|---|---|---|---|
| 1 | ④「生成销售出库单（`out_type=SALES`）」 | `biz_stock_out.out_type=OTHER`、`source_type=SALES` | 销售出库走 `source_type` 轴，与 SO-23 不变量 S6 的判据一致（只认 `out_type` 会 0 命中）。ARCH-7 §13 风险 #2 的原计划（扩 `out_type` 值域）未采用 |
| 2 | ⑦「贷 6001 5 万」 | 贷 **5001** 主营业务收入 | 科目表里收入科目是 5001；6001 不存在于本库科目体系（SO-23 已登记同口径） |
| 3 | ④/⑤「订单 PARTIAL_SHIPPED / SHIPPED」 | **`PARTIAL_OUT` / `OUTBOUND`**（`state_machine::SALES_ORDER_EDGES` 11 条边） | 状态库里没有 `PARTIAL_SHIPPED/SHIPPED` 两个值 |
| 4 | ⑨「客户 `credit_used` 归零」 | `biz_customer` **无 `credit_used` 列**；信用占用 = `Σ biz_receivable.unsettled_amount` | 实测该口径 `0.0000`（已归零）；额度校验在提交/出库时按未结应收 + 本单金额实时算 |
| 5 | ⑥「开票 IN-xxx（5 万）」单张 | 发票**按应收**开，2 张应收 → **2 张发票**（30000 + 20000 = 不含税 50000，税 6500） | 单张发票不得超过对应应收金额（`invoice` 模块约束），合计口径与稿面一致 |

附带两条实现细节（写脚本时踩到，供后续复现参考）：`biz_shipment_item` **没有 `line_no`**（按 `so_item_id` 关联订单行）；`POST /business/erp/invoice` 的 `tax_rate` 取值域是 **0~1**（`0.13`），不是百分数 `13`。

---

## 4. 数据库状态快照（逐步）

| 文件 | 内容 |
|---|---|
| `raw/step01.txt` | 客户档案 + 该步单据计数 |
| `raw/pre_inventory.txt` | 期初三本账（`biz_inventory` / `biz_inventory_location` / `biz_inventory_txn`）+ OPENING 凭证 |
| `raw/step02_so.txt` | 订单头 + 明细（amount/tax_rate/tax_amount/delivered_qty） |
| `raw/step02_submitted.txt` | 订单 `SUBMITTED` + 审批实例 + 待办（节点/指派人） |
| `raw/step02_approved.txt` | 订单 `APPROVED` + 实例 `APPROVED` + 两条审批留痕（n_sales / n_finance） |
| `raw/step03.txt` | 发货单 `DRAFT` + 发货明细 + 订单未变（`delivered=0`）+ 库存未动（200/100） |
| `raw/step04_after_post.txt` | **出库单 / 三本账 / 库存流水 / 凭证 + 分录 / 应收 / 订单状态**（本卡最关键快照） |
| `raw/step05.txt` | 订单 `OUTBOUND` + 2 张发货单 + 2 张应收 + 剩余库存 |
| `raw/step07_revenue_voucher.txt` | 收入凭证头 + 分录 + 发票（含关联应收单号） |
| `raw/step09_final.txt` | 应收 `SETTLED` + 核销明细 + 收款单 + 客户信用占用 + 全部 6 张凭证 |
| `raw/step10_invariants.txt` | 销售组巡检全文（8 条 + 逐条覆盖数） |
| `raw/step10_invariants_full.txt` | 全量巡检全文（76 条） |
| `raw/control_pristine_full.txt` | **对照库**（干净模板，未跑链路）全量巡检 |
| `raw/migration_check.txt` | 迁移口径自查（225 条，`applied_via` 全部 executed/imported） |
| `raw/round1_console.txt` / `raw/round2_console.txt` | 两轮 console 全文 |
| `raw/round_diff.txt` | 两轮 diff 结论（UUID/单号归一化后 **DIFF-EMPTY**） |

`raw/*.json` 为每一步的真实 HTTP 响应体（`access_token`/`refresh_token` 已脱敏为 `<redacted>`）。

---

## 5. 不变量结果（含「绿」与「没查」的区分）

销售组（`--group 销售`）—— **8/8 通过，BLOCKER 0，WARN 0，且每一组都非空覆盖**：

```
· 覆盖：销售订单 1 张 / 明细 2 行        ✅ S1 价税合计 = 不含税 + 税额（且明细 Σ = 头）
· 覆盖：订单明细 2 行，其中已发货 2 行    ✅ S2 0 ≤ delivered_qty ≤ qty
· 覆盖：发票 2 张（红字 0 张）            ✅ S3 发票 价税合计 = 不含税 + 税额
· 覆盖：应收 2 张，其中已收款 2 张        ✅ S4 0 ≤ received_amount ≤ amount
· 覆盖：收款 1 张 / 核销明细 2 行         ✅ S5 allocated_amount ≤ amount 且 Σ核销 = 已核销
· 覆盖：已过账销售出库 2 张 / SALES_OUT 应收 2 张  ✅ S6 已过账销售出库必有应收（勾稽）
· 覆盖：全账凭证 6 张，其中销售相关 5 张  ✅ S7 销售相关凭证 Σ借 = Σ贷（头 = 分录取和）
· 覆盖：物料×仓库 2 组 / 已过账出库单 2 张 ✅ S8 三账一致（库存 = 库位账 = 流水）
```

全量巡检（76 条）：

| 库 | 结果 | 说明 |
|---|---|---|
| `cloudpivot_so24`（跑完 10 步链条） | **73/76 通过，BLOCKER 1，WARN 2** | `raw/step10_invariants_full.txt` |
| `cloudpivot_so24ctl`（同一模板，**未跑链路**） | **73/76 通过，BLOCKER 1，WARN 2** | `raw/control_pristine_full.txt` |

两库的非绿条目**是同一批既有项**，即：**跑通整条销售链没有引入任何新增违规**。

既有 3 条（非本卡引入，SO-23 卡已登记）：

- 🚨 **H6**（BLOCKER）：预置流程种子的 `INSERT INTO biz_flow_def` 未显式写 `is_system`（V0281 种子）。修法会连带 `so_approve` 快照边序不可重放，**已开卡 `t_1dadde81` 交 backend-developer 合并修**（V0301 在工作区推进中）。
- ⚠️ **H5**（WARN）：同源（自称「标准」的流程定义应 `is_system=TRUE`）。
- ⚠️ **N2**（WARN）：干净库无 `sys_dept` 行（组织树未落地），`在用 HUMAN 账号必须有部门归属`。

> 因此卡面「⑩ 跑 erp_invariants.py 全绿」在本卡的达成口径是：**销售组 8/8 全绿（这一组是 SO-24 的验收面）** + **全量零新增红**。全量要做到 BLOCKER 0 需先收口 H6（`t_1dadde81`）与 N2（组织树落地）。

---

## 6. 连跑两轮

```
round1: PASS=47 FAIL=0   销售组 8/8  全量 73/76
round2: PASS=47 FAIL=0   销售组 8/8  全量 73/76
diff（UUID/单号归一化）: DIFF-EMPTY —— 两轮逐行一致
```

两轮都是**从干净模板重建库**后再跑（不是在同一库上重复执行），因此也顺带验证了链路的幂等性与编号序列（SO/SH/OUT/AR/IV/RC 均从 `-000001` 起）。

---

## 7. 复现命令

```bash
# ① 建隔离库（从零业务夹具模板复制）+ 起后端 :18224
bash scripts/verify/so24_e2e.sh up

# ② 跑 10 步（真 token curl，逐条响应与快照落 docs/evidence/sales-e2e/raw/）
bash scripts/verify/so24_e2e.sh chain

# ③ 对照库（不跑链路）全量巡检 —— 用于证明「零新增红」
bash scripts/verify/so24_control.sh

# ④ 两轮一致性
bash scripts/verify/so24_round_diff.sh

# ⑤ 手动跑某一组
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_DATABASE_NAME=cloudpivot_so24 python3 scripts/verify/erp_invariants.py --group 销售

# ⑥ 收工
bash scripts/verify/so24_e2e.sh down
```

隔离库/端口均在脚本头部可用环境变量覆盖（`SO24_DB` / `SO24_PORT` / `SO24_SRC_DB`）。

---

## 8. 未覆盖 / 已知风险（诚实边界）

1. **反向验收（ARCH-7 §12.3）不在本卡**：订单作废、发货单作废、出库单红冲、收款作废、发票作废 5 条反向链未在本卡执行 —— 稿面把它们单列成 §12.3，建议独立成卡（`so24-reverse`）。
2. **信用额度闸只做了正向**：本卡额度 10 万 > 订单 5.65 万，未构造超额被拒的反向用例（属 §12.3 之外的补充面，建议并入反向卡）。
3. **期初库存是 e2e 前置**：干净库无库存，若走「采购入库全链」再销售，能进一步覆盖采购→库存→销售的跨链勾稽，但会显著拉长本卡（>1.5h 预算）。
4. **前端零覆盖**：本卡只验后端 API 与账务；页面级验证属各 SO-11/15/18/21/25 卡。
5. **审批人账号是夹具**：`so24_sales_mgr` 同时带 SUPER_ADMIN（为了绕过权限菜单授权面）与三个业务角色（为了进 `so_approve` 的节点指派）。生产语义下应按角色矩阵只给业务角色 + 菜单授权，这属 SO-25 的验收面。

---

## 9. 本卡改动清单

| 文件 | 类型 | 说明 |
|---|---|---|
| `scripts/verify/so24_e2e.sh` | 新增 | 建库/起实例 + 10 步链路（真 token curl）+ 逐步快照 + 销售组与全量巡检 + 证据脱敏 |
| `scripts/verify/so24_control.sh` | 新增 | 干净对照库全量巡检（证明零新增红） |
| `scripts/verify/so24_round_diff.sh` | 新增 | 两轮结果归一化 diff |
| `docs/evidence/sales-e2e/README.md` | 新增 | 本文件 |
| `docs/evidence/sales-e2e/raw/*` | 新增 | 逐步 HTTP 响应 + 数据库快照 + 巡检全文 + 两轮 console |
| 产品代码（Rust / 前端） | **零改动** | 因此无 `cargo test` / `pnpm typecheck` 新增面，无新增迁移 |
