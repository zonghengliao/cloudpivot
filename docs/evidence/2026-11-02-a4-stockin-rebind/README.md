# A4 入库单改绑采购单 —— 交付证据（2026-11-02）

卡：`t_a8aa8c71`。代码：`feat(erp): A4 入库单改绑采购单`。

## 0. 一句话结论

入库单的供应商**不再由用户填写**：有来源采购单时从 `biz_purchase_order.supplier_id`
派生（填错即 400 `66028`）；无来源单（期初 / 复制 / 其他）必须**显式**打
`is_sourceless = true` 并写明原因，否则 400 `66027`。无源标记是**真实列 + 库约束钉死**
（`ck_biz_si_sourceless_pair`），可枚举、可巡检。过账前再复校一次供应商，兜「建单后被直接改库」。

## 1. 环境与边界（诚实声明）

| 项 | 值 |
|---|---|
| 代码机 | `172.18.10.99`（Hermes 所在机），仓库 `/opt/cloudpivot`，分支 `p3b` |
| 单测库 | 本机 `cloudpivot_test`（`ERP_DB_URL` 注入；V0199 已应用） |
| 真机全链路库 | 本机独立库 `cloudpivot_a4_e2e`（**从零建库 → 迁移器自动应用 143 个迁移含 V0199**） |
| 真机实例 | `./target/debug/cloudpivot`，`NIK_PROFILE=test`，`127.0.0.1:18091`，与 prod 完全隔离 |
| 不变量干净库 | 本机 `cloudpivot_a4_inv`（从零建库 → 143 个迁移 → 零业务数据） |
| 144（生产机） | **只做只读体检**（第 3 节），**未执行任何 DDL、未发布、未改数据** |

为什么不在 144 走全链路：144 上仓库停在 V0192（A3 的 V0198 未上线），
把 A4 的 DDL 打到生产库 = 发布生产，卡面明令禁止；而「生成收货单 → 过账 → 作废」需要
A3 的合同列与 V0199 的标记列同时在位。故用**同版本二进制 + 独立库 + 真实 HTTP** 走完整链路
（A1/A2/A3 同一打法），生产侧只读核对存量口径。

## 2. 迁移 V0199（`V0199__erp_stock_in_sourceless.up/down.sql`）

- `S1` 加列 `is_sourceless boolean NOT NULL DEFAULT false`
- `S2` 存量体检（只读，播报无源张数 / 无备注张数 / 供应商漂移）
- `S2'` 回填：`source_order_id IS NULL ⇒ is_sourceless = true`（只有这一个方向）
- `S3` 约束 `ck_biz_si_sourceless_pair CHECK ((is_sourceless = (source_order_id IS NULL)) IS TRUE)`；
  同时**复核** A3 的 `ck_biz_si_supplier_scope` 仍在（不在就中止迁移）
- `S4` 部分索引 `idx_biz_si_sourceless`（巡检 / 枚举走索引）
- `S5` 列注释：把「快照」语义与「不得用于跨环判定」写进库
- `S6` 迁移内自检：1 列 / 2 约束 / 1 索引 / 零违反 + 存量口径播报

> **为什么不加 `contract_id` / 合同乙方**：设计 §6.4。入库单要合同号只做「一跳带出」
> （读 `biz_purchase_order.contract_no_snapshot`），不落库、不 JOIN `biz_contract`。

### 2.1 幂等与回滚（本机 `cloudpivot_test` 实跑）

```
up    → NOTICE: S2 存量体检：无源入库单=1 张（其中无备注=1 张，只报告）；采购入库缺供应商=0 张；…漂移=0 张
       → NOTICE: S6 自检通过：1 列 / 2 约束 / 1 部分索引；钉死违反=0；存量入库单 1 张（有源=0 / 无源标记=1）
up×2  → 全部 already exists / UPDATE 0（幂等）
down  → NOTICE: 回滚完成（V0199）：列/约束/索引已删，业务数据一行未动；A3 的供应商方向约束保持原样
up    → 再次 NOTICE: S6 自检通过 …（可反复）
```

### 2.2 DB 反例演练（4/4 被拦，`cloudpivot_test` 实跑）

| # | 反例 | 结果 |
|---|---|---|
| ① | 无源（`source_order_id IS NULL`）但 `is_sourceless = false` | 被 `ck_biz_si_sourceless_pair` 拒 |
| ② | 有源（`source_order_id` 非空）却 `is_sourceless = true` | 被 `ck_biz_si_sourceless_pair` 拒 |
| ③ | `in_type='OTHER'` 却带供应商 | 被 `ck_biz_si_supplier_scope` 拒（A3 落的约束，本卡复核） |
| ④ | `in_type='PURCHASE'` 却没有供应商 | 被 `ck_biz_si_supplier` 拒（V0145 既有） |
| 正例 | 无源 + `is_sourceless=true` + 供应商 | 写入通过 |

## 3. 144 生产库只读体检（未改任何数据）

```sql
-- 144 cloudpivot（V0192 水位，A3/A4 的迁移都未上线）
PO 15 / SI 12
src_null                    6   -- 无源入库单（迁移后会被标为 is_sourceless=true）
nonpurchase_with_supplier  0   -- S2-① 干净：ck_biz_si_supplier_scope 可安全加
purchase_without_supplier  0   -- S2-② 干净
supplier_drift_vs_po       0   -- S2-③ 干净：历史没有「入库单供应商 ≠ 采购单供应商」
```

现存 5 张未删的无源单（4 张无备注），全部是 2026-10-31 的联调 / 抽验单：

```
IN-20261031-000001 DRAFT    (无备注)
IN-20261031-000005 POSTED   (无备注)
IN-20261031-000006 CANCELLED(无备注)
IN-20261031-000007 CANCELLED(无备注)
IN-20261031-000015 CANCELLED P10发布抽验-1件-验完作废
```

处置：迁移**只如实标记**，不改备注、不改状态（历史证据不许篡改）；新建单才要求写原因。

## 4. 单测（13 条，真库 `cloudpivot_test`）

`cargo test --lib a4_ -- --test-threads=1` → **13 passed / 0 failed / 0 ignored**（20.32s）

| 用例 | 对应卡面验收 |
|---|---|
| `a4_01_purchase_with_source_derives_supplier` | ①⑥⑨ 有源派生供应商 + `source_item_id` 从采购单带出 |
| `a4_02_purchase_without_source_is_400` | ② PURCHASE 无来源单 → 400 + 66027 |
| `a4_03_source_order_missing_is_400` | ③ 采购单不存在/已软删 → 400 |
| `a4_04_source_order_not_receivable_is_400` | ④ DRAFT/CANCELLED/CLOSED → 400（SUBMITTED 仍可收，见第 7 节差异） |
| `a4_05_client_supplier_mismatch_is_400` | ⑤ 供应商 ≠ 采购单 → 400 + 66028；相等幂等放行 |
| `a4_06_sourceless_requires_flag_and_reason` | ⑦⑧ 无源必须标记 + 原因；有源单的冗余标记按来源归一化 |
| `a4_07_non_purchase_with_supplier_db_rejects` | ⑦⑧ DB 层：非 PR 带供应商 / 无源不标记都被库拒 |
| `a4_08_over_receipt_is_400` | ⑩ 超收 → 400（D4 老校验仍在） |
| `a4_09_list_and_detail_expose_snapshot_columns` | ⑪ 列表 `source_order_no` + `supplier_name` + `is_sourceless`；详情一跳带出合同号 |
| `a4_10_posting_rechecks_supplier` | 设计 §7.2 过账前复校（建单后改库 → 过账 400，库存不动） |
| `a4_11_post_and_cancel_keep_purchase_chain` | 卡面 §5 全链路（过账 → 三账 + 采购回写 → 作废 → 回基线） |
| `a4_12_copy_marks_sourceless` | 复制单 → 无源标记 + 备注「复制自 IN-xxx」 |
| `a4_13_draft_update_cannot_hand_pick_supplier` | 草稿改单不许单独换供应商 |

## 5. 真机全链路（`cloudpivot_a4_e2e` + `127.0.0.1:18091`，真实 JWT + 真实 PG）

脚本：`chain.sh`（响应原文在 `responses/`）。逐步摘要：

| 步 | 动作 | 响应摘要 |
|---|---|---|
| 1 | 建采购合同（乙方 = A4 一号供应商，1000 元） | `HT-20261102-000001` / DRAFT |
| 2 | 合同签署 + 生效 | SIGNED → **ACTIVE** |
| 3 | 建 **CONTRACT** 型采购单（**不传 supplier_id**） | `PO-20261102-000001`，`supplier_name=「A4 一号供应商」`，`contract_no=HT-20261102-000001`（从合同派生） |
| 4 | 采购单提交 | APPROVED（种子审批流自动过） |
| 5 | 待收接口 | `item_id=…7c46`，`pending_qty=10.0000` |
| 6 | **生成收货单（不传 supplier_id）** | `IN-20261102-000001`，`supplier_id=a400…0002`、`supplier_name=「A4 一号供应商」`、`is_sourceless=false`、`source_order_no=PO-20261102-000001`、`contract_no=HT-20261102-000001`（一跳）、明细 `source_item_id=…7c46` |
| 7 | 基线 | 库存 `0/0/0`；凭证 0；应付 0；流水 0；采购已收 0，状态 APPROVED |
| 8 | 过账 | `{"code":0,"msg":"ok"}` |
| 9 | 过账后 | 库存 `6.0000 / 12.0000 / 2.000000`；凭证 1；应付 1；流水 1（`PURCHASE_IN`）；采购已收 `6.0000`，状态 **PARTIAL_RECEIVED**；单据 `voucher_id/payable_id` 已回填，价税合计 13.56 |
| 10 | 作废（带原因） | `{"code":0}` |
| 11 | 作废后 | 库存 `0.0000 / 0.0000 / 0.000000`；凭证 2（含 **1 张红冲**，借贷各 13.56 → 净额 0）；应付 `AP-20261102-000001` **CANCELLED**；流水 `PURCHASE_IN` + `PURCHASE_IN_CANCEL` 各 6/12；采购已收 `0.0000`，状态 `PARTIAL_RECEIVED`（RECEIVED/部分 回退的合法边） |
| 12 | 列表 | `IN-20261102-000001`：`supplier_name=「A4 一号供应商」`、`source_order_no=PO-20261102-000001`、`is_sourceless=false` |

### 5.1 真机反例（`negatives.sh`，响应原文在 `responses/20..24`）

| # | 场景 | 结果 |
|---|---|---|
| R1 | 无源单不打标记 | 400 **66027**「采购入库必须绑定来源采购单…请显式传 is_sourceless=true 并在 remark 写明来源」 |
| R2 | 无源 + 标记 + 原因 | 200，`IN-20261102-000002`，`is_sourceless=true` |
| R3 | 有源 + 供应商填错 | 400 **66028**「供应商与来源采购单 PO-… 不一致…不允许手工填写」 |
| R4 | 复制为新单 | 200，`IN-20261102-000003`，`is_sourceless=true`，`source_order_id=null`，`remark="| 复制自 IN-20261102-000001"` |
| R5 | 列表 | 3 张单：两张无源（`is_sourceless=true`，`source_order_no=null`）+ 一张有源（PO 单号在位） |

## 6. 不变量（`scripts/verify/erp_invariants.py`）

干净库 `cloudpivot_a4_inv`（从零建库 → 143 个迁移含 V0199 → 零业务数据）：

```
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。
```

## 7. 回归（本机 `cloudpivot_test`）

```bash
cargo fmt -p cloudpivot -- --check          # 通过
cargo clippy -- -D warnings                  # 通过（lib + bins，零警告）
cargo build --bin cloudpivot                 # 通过
cargo test --lib -- --test-threads=1         # 1976 passed / 0 failed / 0 ignored（726.26s）
```

基线 1963（A3 交付时）→ **+13 条 A4 用例 = 1976**，无 skip / 无 ignore。

> **串行跑的原因（诚实记录）**：并行跑时 `cash_flow::tests::r3_real_config_covers_all_accounts`
> 会偶发失败，报「分类配置漏科目 P2D1A / P2D1B」。这两个科目是 `voucher_query.rs` 的 P2-D1 夹具
> 在**并行窗口内**临时建出来的（`DELETE FROM biz_account WHERE account_code LIKE 'P2D1%'` 就是它的清理），
> 而该用例断言「`biz_account` 里每个科目都要有现金流分类行」—— 两个模块用各自的 `DB_LOCK`，跨模块不互斥。
> 与 A4 无关（A4 不碰 `biz_account` / 现金流），单独跑该用例与单跑 `cash_flow` 模块都是绿；
> 清掉上一轮残留的 P2D1 科目后串行全量 **1976/1976 全绿**。
> 这是既有的「共享测试库 + 跨模块并行」问题，留给测试基座卡收敛，不在本卡改别人的用例。

## 8. 与卡面 / 设计的口径差异（全部是「变宽不变窄」或「不改既有行为」）

| # | 卡面/设计 | 实际 | 理由 |
|---|---|---|---|
| 1 | 卡面 V0196 | **V0199** | V0193–V0198 已被占（A3 的 V0198 是最近一个） |
| 2 | 卡面「采购单状态允许收货（APPROVED / PARTIAL_RECEIVED）」 | 保留 `SUBMITTED` 也可收 | `purchase_receivable` 白名单是 D4 定稿；ERP-B2 内控开关默认关，SUBMITTED 是常态。判死会让「建单 → 收货」在默认配置下断链 |
| 3 | 卡面「非 PURCHASE 型传了 supplier_id → 400 或置 NULL」 | DB 约束拒绝（接口层根本不产生非 PURCHASE） | `stockin_write` 把 `in_type` 硬编码 `'PURCHASE'`；A4 不扩枚举（设计 §4.4） |
| 4 | 设计 §4.3「DRAFT 也禁改 supplier/source」 | DRAFT **允许**换来源单，但供应商随之重新派生；单独换供应商 → 400 | 设计内部两处口径互相矛盾（§4.3 注 vs §7.2）；选「不许单独换供应商、换单必须跟着采购单走」，既守住 R2 又保留纠错能力 |
| 5 | 无源标记形态 | 新增 `is_sourceless` 列（不用 remark 文本） | AP-5 要求可枚举 + 能巡检；`remark` 文本无法索引、无法枚举，写进卡面的是这个选择 |
| 6 | 有源单多带 `is_sourceless=true` | 归一化为 false（不拒单） | 标记等价于「没有来源单」，冗余不值 400；真正要拦的是「供应商填错」（66028）。单测 `a4_06` 的 ④ 钉住了这条归一化 |
