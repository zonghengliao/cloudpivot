# P1-3 盘点单（ARCH-3 §5 DC-14）—— 交付证据

**一句话**：全系统原来没有盘点单 —— 年底账面 100、实际 92，仓管在系统里**没有地方调平**，
只能手改 `biz_inventory`（没有凭证来源、没有审批、没有留痕），审计上说不清「为什么库存变了」。
本卡把它做成**单据**：建单（账面数服务端快照）→ 填实盘 → 提交审批（可配流程、按差异金额分档）
→ 过账（三本账按差异正负走入库/出库方向 + 盘点差异凭证）→ 作废（原路回滚 + 红冲）。

---

## 1. 交付物

| 类别 | 文件 | 说明 |
|------|------|------|
| 迁移 | `backend/migrations/business/V0211__erp_stock_take.{up,down}.sql` | 两表 + 8 索引 + 7 约束 + 6 权限码 + 1 取号规则（`ST-yyyyMMdd-`）+ 审批流种子 `stock_take_post` |
| 数据层 | `backend/src/business/erp/stocktake_model.rs` | 账面快照 / 分页 / 三本账（盘盈走 `stockin_write`、盘亏走 `stockout_model`、作废走 `stockin_cancel`/`restore_*`）/ 盘点差异凭证 |
| 契约 | `backend/src/business/erp/stocktake_dto.rs` | 建单 / 填实盘 / 状态流转 / 详情 / 分页（客户端**不能**传账面数、差异、金额） |
| 接口 | `backend/src/business/erp/stocktake_handler.rs` | 6 个接口 + 过账/作废事务编排 + 审批发起（`approval_gate` 口径） |
| 审批回调 | `backend/src/business/erp/stocktake_bridge.rs` | 驳回/撤回 → 回草稿；内控（盘的人不能批自己）；孤儿实例终止 |
| 状态机 | `backend/src/business/shared/state_machine.rs` | `Entity::StockTake`（10 个实体）+ 6 条边 + 1 条必填原因边 |
| 流程接线 | `backend/src/business/flow/{handler,internal_control}.rs` | 审批结论回调 / 自审批闸 / 制单人排除 |
| 巡检 | `scripts/verify/erp_invariants.py` | 新增 I1/I2/I3（盘点组），原 37 条不变 |
| 真机脚本 | `scripts/verify/p13_{instance.sh,migration_roundtrip.sh,stock_take_e2e.py}` | 隔离实例 / 迁移三轮 / 37 条断言全链路 |

---

## 2. 真机全链路（每步的三本账数字）

来源：`raw/02-real-machine.txt`（隔离实例 127.0.0.1:18099 / 库 `cp_p13_e2e`，全部走真实 HTTP）

| 步骤 | 成本账（`biz_inventory`） | 库位账（`biz_inventory_location`） | 流水（`biz_inventory_txn`） | 凭证 |
|------|---------------------------|-------------------------------------|------------------------------|------|
| ① 基线（期初 100 件 @ 8） | **100 / 800.00** | 100 | 净额 100 | 借 1405 800 / 贷 1901 800 |
| ② 建盘点单（全盘） | 100 / 800.00 | 100 | 净额 100 | —（账面数快照 100，实盘预填 100） |
| ③ 填实盘 92（差异 −8） | 100 / 800.00 | 100 | 净额 100 | —（账一分没动） |
| ④ 提交审批 → 审批通过 | 100 / 800.00 | 100 | 净额 100 | —（状态 `PENDING_POST`） |
| ⑤ 过账 | **92 / 736.00** | **92** | 盘点流水 **−8** | 新增 `PZ-20261103-00000x`：**借 1901 64 / 贷 1405 64** |
| ⑥ 作废 | **100 / 800.00** | **100** | 盘点流水净额 **0** | 新增 `PZR-20261103-00000x`（`is_red=1`）：**借 1405 64 / 贷 1901 64** |

关键断言（37 条全过）：

- 建单后明细账面数 = 100（服务端从 `biz_inventory` 快照，客户端传不了）；
- 未审批直接过账 → **409**「审批尚未通过」（没批过不许动账）；
- 过账后成本账 92 / 736（盘亏 8×8=64，金额取**账上真实差额**，不重算 `qty × avg`）；
- 作废后三本账回基线、盘点流水净额 0（正向 + 冲销）、原凭证被红冲（`reversed_by` → 红字）。

---

## 3. 与验收标准的对应

| 验收项 | 结果 | 证据 |
|--------|------|------|
| 迁移 up/down/up 三轮通过 | ✅ 3 次 up / 2 次 down，down 后零残留、up 后结构齐全 | `raw/04-migration-up-down-up.txt` |
| 单测 ≥ 12 条（盘盈/盘亏/无差异/作废回滚/审批流/负库存拦截） | ✅ **18 条**（4 条纯逻辑 + 12 条真库 + 2 条审批回调纯逻辑） | `raw/05-stocktake-unit-tests.txt` |
| 真机全链路（建单→实盘 92→审批→过账→库存 92→盘亏凭证→作废→回 100→红冲） | ✅ 37/37 | `raw/02-real-machine.txt` |
| 不变量原 32 条 + 新增全绿 | ✅ **40/40**（原 37 + 新增 3 条盘点） | `raw/03-invariants.txt` |
| git commit（`feat(erp): P1-3 盘点单`） | ✅ | `git log -1` |
| 卡里贴三本账数字 | ✅ | 本文第 2 节 + 看板评论 |

---

## 4. 设计要点（为什么这么做）

### 4.1 三本账**复用**，不重写

| 差异 | 库存方向 | 用的函数（与出库单 / 入库单同一套实现） |
|------|----------|------------------------------------------|
| `diff > 0` 盘盈 | 增加 | `stockin_write::{apply_cost, apply_location_qty}`（收货过账同一个移动加权） |
| `diff < 0` 盘亏 | 减少 | `stockout_model::{lock_bucket, deduct_cost, lock_location_row, deduct_location_qty}`（出库过账同一个负库存闸） |
| 作废·盘盈行 | 扣回 | `stockin_cancel::{reverse_cost, reverse_location_qty}`（按账上真实金额冲减） |
| 作废·盘亏行 | 加回 | `stockout_model::{restore_cost, restore_location_qty}`（原路退回） |

**没有**新写「成本账怎么算 / 库位账怎么减 / 流水怎么写」的任何一行公式；盘点只负责
「差异是多少、往哪个方向走、凭证挂哪个科目」。

### 4.2 为什么盘盈不建「入库单」、盘亏不建「出库单」

- 盘亏**本来就可以**走出库单（`out_type=LOSS`，凭 借 1901 / 贷 1405）——但盘盈没有对应的入库单类型
  （`biz_stock_in.in_type` 只有 `PURCHASE/RETURN/OTHER`，且 `PURCHASE_IN` 的凭证规则贷方是**应付账款**，
  盘盈贷 1901 会走错科目）。为了**一张盘点单对应一套逻辑**（而不是两套半），过账直接在盘点单内完成，
  并由**盘点单自己**出「盘点差异凭证」（`source_type='STOCK_TAKE'`，盘盈/盘亏分录成对出现）。
- 代价：盘盈盘亏不在出库单列表里出现。补偿：盘点单详情能看到每行差异与差异金额，
  `biz_inventory_txn.doc_type='STOCK_TAKE'` 让库存流水可以按盘点追溯（含 `doc_id` 指回盘点单）。

### 4.3 审批「通过」不等于「过账」

```
DRAFT ──提交审批──▶ PENDING_POST ──审批通过（实例 APPROVED）──▶ PENDING_POST（等过账）
                        │                                          │
                        └──驳回/撤回/终止──▶ DRAFT（清实例）        └──过账──▶ POSTED
```

审批只回答「这份差异能不能调平」，过账回答「现在调」。两者分开才有 `posted_by` 这个可追责的人，
也才让「制单 / 审批 / 过账」三人可分（`erp.separation_of_duties.post` 开关）。
过账入口硬校验实例状态必须 `APPROVED` —— 审批结论是**证据**，不是**触发**。

### 4.4 审批流可配（不是硬编码）

`stock_take_post`（`biz_type=STOCK_TAKE_POST`）种在迁移里，6 节点 6 边：

```
s → c_amt（条件：form.total_diff_amount < 10000）
      ├── 是 → n_wh（仓管主管，单级）→ e
      └── 否 → n_fin（仓管主管）→ n_gm（总经理）→ e   ← 串行两级，不是会签
```

阈值、审批人、超时全在 `biz_flow_edge.condition_expr` / `biz_flow_node` 里，
客户在「流程定义」里另存版本即可改（`graph_hash = cfc6101455d799fb4377353ad57c85dfae8f30956a94ae642c88e1644a6bfb4a`）。

---

## 5. 已知边界 / 未做的事

1. **前端页面未做**（本卡验收标准不含前端）：接口与权限码已就绪（`business:erp:stock-take:*`），
   前端 `views/business/erp/stock-take/` 属独立一张卡。
2. **一物一码（`serial_required`）物料的盘点**未特殊处理：按数量调整库存，不碰 `biz_material_serial`；
   若这类物料要盘，需要按序列号逐件核对（另开卡）。
3. **只在「未分配库位桶」上验证**了三本账；指定库位（`location_id`）的路径走的是同一批函数
   （`apply_location_qty` / `lock_location_row` 的分支 A），单测覆盖了参数传递，真机只跑了未分配桶。
4. `down` 迁移在「功能已被使用」时会**主动拒绝**（有已过账盘点单 / 有审批实例留档都一样），
   这是有意的：不让一次回滚把审计痕迹悄悄抹掉。
