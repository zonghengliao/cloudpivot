# P2-8：项目 → ERP 的口径与第一步落地（采购单挂项目）

> 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §5 **DC-16**（P2）——
> 「30 张 ERP 业务表 0 张有 `project_id` ⇒ 项目成本核算这条业务价值完全没接上」。
>
> 本卡（P2-8）只做**第一步**：`biz_purchase_order.project_id`（迁移 `V0237`）+ 后端全链路 + 口径文档。
> 其余单据的证据与待办见 §5。

---

## 1. 先论证：哪些单据该挂项目

判据（三条同时成立才该挂）：

1. **有金额**（能进成本的才算归集对象，数量/主数据不算）；
2. **是「发生」而不是「配置」**（配置类改一次影响所有历史，挂项目反而制造歧义）；
3. **能说清「归集到项目的哪一刻」**（发货即成本 / 入库即成本 / 确认应付即成本 / 付款即现金流出）。

| 单据 / 表 | 该挂？ | 理由 | 归集时点语义 |
|---|---|---|---|
| `biz_purchase_order` 采购单 | ✅ **本卡已挂** | 项目采购的起点，金额在单据上（`total_with_tax`），且是「这笔钱为什么花」的唯一凭据 | 承诺（订单金额），不是成本确认 |
| `biz_stock_in` 收货单 | ✅ 该挂（待办） | 货真的到了项目现场；入库金额是**成本确认**的候选口径 | 入库即成本（与移动加权一致） |
| `biz_payable` 应付单 | ✅ 该挂（待办） | 会计上「应付确认」通常就是费用/成本确认时点 | 确认应付即成本（权责发生制） |
| `biz_payment` 付款单 | ✅ 该挂（待办） | 现金流视角的项目支出（与成本口径不同，不能混用） | 现金流出 |
| `biz_voucher_entry` 凭证分录 | ✅ 该挂（第二步，走 `aux_extra` 或新增 `aux_project_id`） | 报表侧真正按项目出数的表；DC-11 的辅助核算同一条路 | 继承来源单据 |
| `biz_material` / `biz_supplier` / `biz_warehouse` / `biz_account` / `biz_unit` | ❌ 不挂 | 主数据是**跨项目共享**的字典；给物料挂项目等于「这个物料只属于一个项目」，语义错误 | — |
| `biz_opening_balance` / `biz_opening_inventory` 期初 | ❌ 不挂 | 期初是**时点结转**，没有业务发生时点；要按项目拆期初必须先有项目级往来/存货明细账（本版本没有） | — |
| `biz_inventory` / `biz_inventory_txn` 库存台账 | ❌ 不挂（用派生） | 库存按「物料 × 仓库」建账，是维度交叉表；项目维度应由收发货单据**派生**，不再存一份真相（R1 单一数据源） | — |
| `biz_contract` 合同 | ⚠️ 不改结构 | 合同→项目是多对多（一个框架合同可能服务多个项目），真要挂需要中间表，本版本不做 | — |

**一句话**：只有「有金额、有发生时点」的业务单据该挂项目；主数据与台账不挂，靠单据派生。

---

## 2. 本卡做了什么

| 层 | 改动 | 位置 |
|---|---|---|
| 库 | `biz_purchase_order.project_id uuid`（**可空、无外键**）+ 部分索引 `idx_biz_po_project (project_id, order_date DESC) WHERE project_id IS NOT NULL` | `backend/migrations/business/V0237__erp_po_project_link.up/down.sql` |
| 模型 | `PurchaseOrderRow.project_id`、`PURCHASE_ORDER_COLUMNS` 补列、`NewPurchaseOrder.project_id`（建单 / 改单 SQL 落库）、`project_ref` / `project_names_of` | `erp/model_order.rs`、`erp/purchase_model.rs` |
| DTO | `CreatePurchaseOrderReq.project_id`（建单/改单共用）、`PreparedOrder.project_id_raw`、`PurchaseOrderRes.project_id` + `project_name`、`PurchaseOrderFilter.project_id`（列表可按项目筛） | `erp/purchase_dto.rs`、`erp/dto.rs` |
| 校验 | 事务外 `select` 校验「项目存在且未软删」→ 不通过 `400 + 66002`；非法 UUID `400 + 66006` | `erp/purchase_handler.rs::resolve_project` |
| 下发 | 列表（`/business/erp/purchase-order/page`）与详情（`/{id}`）都回填 `project_name`（**关联查现值**） | `erp/handler.rs`、`erp/purchase_handler.rs::fill_project_names` |
| 测试 | 10 条真库用例（建单带/不带项目、项目不存在、项目已软删、非法 UUID、归档项目、改单换/清空、列表按项目筛+回填、项目软删后只丢名字、筛参校验） | `erp/purchase_handler.rs::tests`（`p28_*`） |

### 2.1 口径（后续单据照抄）

| 编号 | 口径 | 为什么 |
|---|---|---|
| **P1** | **项目名称实时关联查，不落快照** | 成本归集看的是「同一个项目」：项目改名后旧单必须跟着显示新名，否则按项目汇总会因改名裂成两行（与 P2-7 的 S2 同一条理由）。供应商名相反（要冻结「当时是谁」），两者不要混。 |
| **P2** | **列可空、无外键** | 可空：存量单从未归集过项目（生产 18 张），强制补值等于让运维猜（假数据比 NULL 更坏）。无外键：`biz_project` 走软删，外键拦不住已软删；且历史清理时外键会卡住单据或级联删账。与 `supplier_id` / `warehouse_id` / `contract_id` 一致。 |
| **P3** | **校验「存在且未软删」，不校验状态** | 归档（`ARCHIVED`）项目上补录当期采购是合法业务（跨期入账），拒绝只会让用户把成本挂到错的项目上。是否允许新单选归档项目由前端下拉控制（下拉只列 `ACTIVE`，已归档仍可保留历史单选值）。 |
| **P4** | **改单为整单覆盖：不传 `project_id` = 清空归属** | 与 `contract_id` / `remark` 同口径（`CreatePurchaseOrderReq` 同时是改单请求）。老客户端 PUT 不带该字段会把项目清掉 —— 前端必须始终回传（表单选择器的值）。 |
| **P5** | **存量不回填** | 18 张存量单 `project_id` 保持 `NULL`；NULL 的语义是「未归集项目」，不是「项目为空」的错数据。 |

---

## 3. 迁移号段的实际选择（卡面写 V0219，实际用 V0237）

动手前复核 `backend/migrations/*/`：

- `V0219` 早已被占用（`V0219__erp_p2_5_biz_type_vocabulary`，见 `schema_migrations.version=219`）；
- 当前最大号是 `system/V0235`、`business/V0236`，故本卡取 **`V0237`**。

索引与卡面的差异：卡面写 `(project_id)`，实际建 `(project_id, order_date DESC)` ——
唯一读路径是「按项目筛 + 按下单日期倒序」（`PURCHASE_ORDER_ORDER`），复合索引让筛和排都走索引，
与 V0198 的 `idx_biz_po_contract (contract_id, order_date DESC)` 完全同形。索引名沿用卡面的 `idx_biz_po_project`。

---

## 4. 验收（实测，证据见 `docs/evidence/20261105-p2-8-po-project/`）

| 项 | 结果 |
|---|---|
| 迁移 up / down / up（同一库连做，含幂等重复执行） | ✅ 见 `migration-cycle.txt` |
| 真库单测 10 条（`p28_*`） | ✅ 见 `cargo-test.txt` |
| 真机：建采购单选项目 → 列表能看到 → 详情能看到 | ✅ 见 `e2e-api.txt`（隔离实例 + curl，非 mock） |
| 不变量套件 | ✅ 见 `invariants.txt` |
| 存量单不受影响（18 张 NULL 可读） | ✅ 见 `legacy-rows.txt` |

---

## 5. 还有哪些单据待挂项目（本卡只记录，不动结构）

| 顺序 | 单据 | 建议做法 | 前置决策 |
|---|---|---|---|
| 1 | `biz_stock_in` 收货单 | 加 `project_id`（可空、无外键），**建单时从来源采购单带出**、允许改；入库过账时按该项目的 `project_id` 写凭证辅助 | 「项目成本 = 收货金额」还是「= 应付金额」——**这个会计口径必须先拍板**（见下表） |
| 2 | `biz_payable` 应付单 | 加 `project_id`，从来源收货单带出 | 同上；若选「应付确认」，则应付才是成本表 |
| 3 | `biz_payment` 付款单 | 加 `project_id`，从来源应付单带出（现金流口径，与成本口径**分开**统计） | 无（纯现金流） |
| 4 | `biz_voucher_entry` | `aux_project_id` 新列，或 `aux_extra` jsonb；从来源单据派生 | 与 1/2 的口径一致 |
| 5 | 报表 | 「项目成本」报表（按 `project_id` 汇总收货/应付 + 凭证侧核对） | 依赖 1–4 |

### 5.1 卡住这条链的产品决策（已记录，未决）

**「项目成本按哪个时点确认」**——ARCH-3 §5 R-P2-3 已经写明建议「按收货确认（与应付口径一致）」，
但这是**会计口径决策**，本卡无权定。在它拍板前，把 `project_id` 铺到收货/应付/凭证上，
会出现「同一笔钱在三个表上各算一次」的假报表风险，所以本卡**只挂了链路最上游的采购单**。

### 5.2 与 DC-11（凭证辅助核算）的接续

DC-11 的三列（`aux_material_id` / `aux_warehouse_id` / `aux_dept_id`）**永远为空**：代码里没有任何
`aux_*_id: Some(...)` 写入点。项目维度要做成「能出报表」而不是「库里有个字段」，
必须和 DC-11 一起做（同一个凭证生成点），否则会重复 DC-11 的坑。

---

## 6. 已知限制（照实写）

1. **前端未在本卡落地**（表单选择器 + 列表「项目」列）：本卡开工时 `purchase/index.vue` 与
   `purchase-logic.ts` 正被 P2-7-F1（供应商名口径，卡 `t_f5fbd004`）占用，两个 worker 同改一个文件会互相
   覆盖。已按团队边界另开前端卡，规格见该卡；后端契约已冻结（`project_id` / `project_name` / 列表筛参 `project_id`）。
2. **项目下拉的数据源是 `/business/project/page`**，它按项目模块的可见性裁剪（我参与 / 我负责的项目）。
   采购员看不到自己没参与的项目 —— 这是**已知取舍**：ERP 单据的数据范围有意为 0（DC-11 同源理由），
   但项目清单不该绕过项目模块的权限。若业务要求「采购员可选任意项目」，需要单独开一个
   「项目选择器（只读全量）」接口，属于后续设计。
3. **没有「按项目汇总」的接口**：本卡只给了 `project_id` 筛参。真正的项目成本报表要等 §5 的链路铺完。
4. `biz_purchase_order` 之外的 29 张 ERP 表本卡**一列都没动**（卡面明令「一次只做一条链」）。
