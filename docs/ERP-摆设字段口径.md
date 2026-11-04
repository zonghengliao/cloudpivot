# ERP 摆设字段口径（ARCH-3 §5 DC-12 · P2-2）

> 卡：`t_dfdbfcaf`【P2-2】摆设字段处置
> 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §5 **DC-12**
> 迁移：`backend/migrations/business/V0215__erp_p2_2_stub_fields.{up,down}.sql`
> 代码：`backend/src/business/erp/purchase_credit.rs`（①）、`reminder_stock.rs`（②）、
> `dto.rs` / `model_inventory.rs` / `stockin_write.rs`（③）

## 0. 一句话结论（卡面要求逐条回答）

| 字段 | 填了会不会生效 | 生效方式 | 消费它的代码 |
|---|---|---|---|
| `biz_supplier.credit_limit` 信用额度 | **会**（P2-2 起） | 采购单**提交**时现算「该供应商累计未结采购额（含本单）」，超限则单据打 `over_credit_limit = true` 并**强制走审批**（`skip-approval` 对超限单不开放）；额度填 `0` = 不限 | `purchase_credit::verdict_of` → `purchase_handler::submit_approval` |
| `biz_material.safety_stock` 安全库存 | **会**（V0210 起） | 仓库作业提醒任务（`erp_low_stock_scan`）每天扫一次：某物料在某仓库的**已过账结存**低于它 → 通知仓管主管；填 `0`/留空 = **不提醒** | `reminder_stock::scan_candidates` |
| `biz_inventory.locked_qty` 锁定量 | **不会**（本版本刻意不生效） | 预留列：全仓无任何占用/拣货流程写它，恒为 `0`；不参与可用量、不参与低库存判定；**前端已撤掉「锁定」列** | 无（只有建库存行时显式写 0） |

---

## 1. ① 信用额度：下单时校验（策略 B：允许 + 强制审批）

### 1.1 口径（一个公式，三处引用）

```text
累计未结采购额 = SUM(biz_purchase_order.total_with_tax)
                  WHERE supplier_id = <本单供应商>
                    AND status <> 'CANCELLED'      -- 作废 = 撤回承诺，不算累赘
                    AND deleted_at IS NULL         -- 软删单不算
                    AND id <> <本单 id>            -- 本单单独加一次，不重复算

超限 = credit_limit > 0 AND (累计未结采购额 + 本单价税合计) > credit_limit
```

四个容易做错的地方，逐条钉死（每条都有单测）：

| 点 | 口径 | 错法 |
|---|---|---|
| 排除作废单 | `status <> 'CANCELLED'` | 把作废单算进累计 → 用户明明撤了单却被判超限 |
| 本单只算一次 | 聚合时 `id <> 本单`，再加本单金额 | 聚合含本单 + 再加一次 → 额度 1 万、单 6 千的正当单被判超限 |
| `0` = 不限 | `credit_limit <= 0` → 不校验 | 把 `0` 当阈值 → 任何单都超限（客户填 0 意为「不设限」） |
| 边界 | `>` 而不是 `>=` | 累计恰好等于额度被判超，用户被迫走无谓审批 |

### 1.2 为什么**现算**、不加「已用额度」缓存列（R1）

缓存列（`used_amount` / `available_credit`）一定会漂移：作废、改单、部分收货、软删、
手工改库，任一动作忘了维护它，结果就是**要么把用户挡在门外，要么把超限单放过去**。
代价是每次提交多一条聚合查询（单供应商几十张单、走 `idx_biz_po_supplier`，毫秒级），
提交不是高频动作 —— 这笔账划算。

**但「提交那一刻查到什么」必须冻结**（审计事实）。所以落三列快照：

| 列 | 含义 | `NULL` 的语义 |
|---|---|---|
| `over_credit_limit BOOLEAN NOT NULL DEFAULT FALSE` | 提交时点是否超限 | —— |
| `credit_exposure NUMERIC(18,4)` | 提交时点「累计未结采购额（含本单）」 | **从未提交过 / 未评估** |
| `credit_limit_snapshot NUMERIC(18,4)` | 提交时点供应商额度 | 同上 |

⚠️ `over_credit_limit = false` **不等于**「查过、没超」：草稿也是 `false`。
判断「评估过没有」只看 `credit_exposure IS NULL`（前端 `creditVerdictOf` 就是按这个分三态）。

供应商额度之后被调高/调低**不回写历史单据** —— 那是「当时的评审结论」，改了就是造假。

### 1.3 谁来消费这个标记

1. **审批人**：`biz_flow_instance.form_data.over_credit_limit`（JSON **布尔**）+
   `credit_exposure` / `credit_limit`（JSON **数字**），审批单详情直接可见；
2. **列表 / 详情**：`PurchaseOrderRes.over_credit_limit` 等三列，前端列表显示红色
   「超信用额度」标签、详情显示「未评估 / 额度内 / 额度不限 / 超信用额度」+ tooltip；
3. **流程管理员**（可选）：在流程设计器里给 `po_approve` 加一条条件边，把超限单
   额外送一道复核：

```json
{ "field": "form.over_credit_limit", "cmp": "eq", "value": true }
```

   条件 DSL 的 `eq` 对布尔按值比较（`flow::graph::eval`），数字比较用 `gt` / `gte`；
   **千万不要**把布尔写成字符串 `"true"`、把金额写成字符串 `"20000.0000"` ——
   TypeMismatch 会被当成「条件不成立」**静默走兜底边**（金额分级会悄悄错档）。
   单测 `p2_2_credit_over_limit_marks_order_and_goes_to_approval` 用真实 `form_data`
   跑了一遍 `eval`，钉住「这样一条边真的会命中」。

> 为什么本卡不直接把这条分支种进 `po_approve`：种子流程（V0182）已经是**发布态**
> 且带版本快照（`graph_hash`），改它等于让所有租户的审批路由在无人知晓的情况下变道。
> 「要不要多一道复核、复核给谁」是内控设计决策，应由流程管理员在流程设计器里配
> （页面已支持条件边配置，见 `V0206__flow_edge_config_admin`）。代码这一侧保证
> 「变量一定在、类型一定对、超限一定进审批」—— 这三件事缺一件，配了分支也没用。

### 1.4 超限 + 没有已发布流程 = 409（skip-approval 不开放）

策略 B 的前提是「有流程承接」。流程没配时不能让持 `business:erp:skip-approval` 的账号
点一下「跳过」把超限单放过去 —— 那等于信用额度根本没生效。所以：

```text
超限 + 有 po_approve   → 正常提交，标记落单据，进审批
超限 + 无 po_approve   → 409 + 66036 ERP_CREDIT_LIMIT_EXCEEDED（报文给两条出路：配流程 / 调额度）
未超 + 无 po_approve   → 走 DC-01 原有逻辑（409，或持 skip-approval + 理由 ≥10 字显式跳过）
```

跳过的路径也会把评估快照补上（事后回写），否则那张单永远显示「未评估」。

---

## 2. ② 安全库存：口径统一

消费者是提醒任务（P1-2，`erp_low_stock_scan`，每天一次），本卡只把口径写死。

| 问题 | 口径 | 理由 |
|---|---|---|
| 用哪一列 | **`biz_inventory.qty`**（物料 × 仓库的已过账结存） | 补货看「手上有多少」 |
| 含在途吗 | **不含** | 在途只是 `biz_stock_transfer.transit_warehouse_id` 这个文档字段；调拨过账（`DONE`）才把数量从调出仓搬到调入仓 |
| 含未过账收货吗 | **不含** | 收货过账才写 `biz_inventory`；把「还没验收」的货算进来会掩盖缺料 |
| 减锁定量吗 | **不减**（也不用减） | `locked_qty` 恒为 0；提醒是「仓里有多少」，不是「还能卖多少」 |
| 按物料合计吗 | **不，按物料 × 仓库** | 原料仓缺货，成品仓再多也调不过来 —— 一行对应一个可执行的补货动作 |
| 用库位账吗 | **不用** | 库位账是仓库账的细分，且多数行还挂在「未分配库位」桶上，按库位判定会重复提醒 |
| 负库存 | **照实数（可为负）** | 账实不符比「低一点」更该提醒，不 `GREATEST(qty,0)` 抹平 |
| 在途仓 | **排除**（`warehouse_type <> 'IN_TRANSIT'`） | 在途仓是记账用的虚拟仓，不是可补货的实体仓 |
| 停用物料 | **排除**（`status = 1`） | 不生产了，提醒也没人处理 |
| 空值 / `0` | **不预警** | `0` 的语义是「没设阈值」，不是「安全库存为 0 也要报」 |

**「空值」在本仓就是 `0`**：`biz_material.safety_stock` 是 `NUMERIC NOT NULL DEFAULT 0`，
前端留空 → 提交 `0` → 不预警。DTO 的 `Option<Decimal>` 只表示「本次请求没带这个字段 = 不改」，
不是「清空成 NULL」。SQL 仍写 `COALESCE(m.safety_stock, 0) > 0`：将来若把列放开成可空，
NULL 也不预警，且读起来是明确的「缺省 0」而不是依赖三值逻辑的巧合。

前端物料弹窗新增「安全库存」输入框，旁边常驻提示：
**「安全库存用于低库存提醒：某仓库的账面数量低于它时通知仓管主管。留空或填 0 = 不提醒。」**

---

## 3. ③ 锁定量：标注预留（不做占用流程）

选了卡面的**选项 B**：不做「库存占用」流程（销售模块还没有，造了也没人用），
而是把它明确标成预留，避免「显示一列永远是 0 的数字」让人以为占用机制在工作。

| 位置 | 处置 |
|---|---|
| 数据库 | **保留列**（不删，未来「销售订单占用 / 生产预占」会用它）；列注释写明「预留，本版本恒为 0」（V0215） |
| 后端模型/DTO | `InventoryRow` / `InventoryRes` / `InventoryLocationRow` 等字段注释标注「预留列，不参与可用量、不参与低库存判定」 |
| `UpdateInventoryReq`（改锁定量） | **无任何路由/调用方**，doc 注释写明「别顺手把它接到路由上」（手工改非 0 不会影响任何判定，只会造一列看着在工作、其实没人读的数字） |
| 收货/出库写入 | 新建库存行显式写 `locked_qty = 0`，已有行不动（`stockin_write::apply_location_qty`） |
| 前端库存页 | **撤掉「锁定」列**（Tab1 即时库存 + Tab2 库位库存）与页脚「锁定」合计；顶部提示改为「数量 = 已过账的账面结存（可用 = 数量；「锁定量」为预留字段，本版本无占用流程，故不显示）」 |

「可用」列**保留**：`可用 = qty − locked_qty` 的公式是对的，只是现在 `locked_qty` 恒为 0
所以它等于数量。等占用流程落地，只改数据、不改 UI 公式。

---

## 4. 存量影响（生产库实测，零回填）

`V0215` 只做 `ADD COLUMN` + `COMMENT`，**不回填任何数据**：

- 三列默认 `FALSE` / `NULL`，`NULL` = 「从未提交过」，回填一个假的 `FALSE`
  会把「没查过」伪装成「查过且没问题」；
- 生产库现状（2026-11-04 只读实测）：`biz_supplier` 1 行且 `credit_limit = 0`（不限）
  ⇒ **开启校验不影响任何存量单**；`biz_purchase_order` 13 行（APPROVED 2 / RECEIVED 8 /
  DRAFT 1 / CANCELLED 2）全部 `credit_exposure = NULL`（未评估）；
- `biz_material` 5 行，其中 3 行 `safety_stock > 0`（低库存提醒已有消费者）；
- `biz_inventory` 3 行，`locked_qty` 非零 **0 行**（与审计一致）。

## 5. 单测索引

| 口径 | 用例 |
|---|---|
| 判定纯函数（含边界 / 0 = 不限 / 本单只算一次 / 未评估三态） | `business::erp::purchase_credit::tests::*`（7 条） |
| 超限标记 + form_data 类型 + 条件边命中 | `purchase_handler::tests::p2_2_credit_over_limit_marks_order_and_goes_to_approval` |
| 额度内不标记 | `p2_2_credit_within_limit_is_not_marked` |
| 额度为空不校验 | `p2_2_credit_zero_limit_is_unlimited` |
| 作废单不计入累计 | `p2_2_credit_cancelled_order_not_counted` |
| 超限不许 skip-approval | `p2_2_credit_over_limit_cannot_skip_approval` |
| 跳过后仍补快照（对照组） | `p2_2_credit_within_limit_can_skip_approval_and_still_records_snapshot` |
| 安全库存 0 不预警（+ 对照组） | `reminder_tests::low_stock_zero_safety_is_silent_but_control_material_is_not` |
| 恰好等于不报 / 负库存要报 / 在途仓不报 | `reminder_tests::low_stock_{exact_boundary_is_silent,negative_qty_is_reported,ignores_in_transit_warehouse}` |
| 前端三态显示 / 安全库存口径 / 锁定列已撤 | `frontend/tests/unit/p2-2-stub-fields.test.ts`（11 条） |
