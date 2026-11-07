# P2-03 采购退货闭环 —— 证据包

卡片：`t_f5470033`（P2-03 采购退货：退货单→审批→红字应付→库存回冲→凭证）

## 1. 交付物

| 类别 | 路径 |
|------|------|
| 迁移 | `backend/migrations/business/V0332__purchase_return.{up,down}.sql` |
| 契约 | `backend/src/business/erp/purchase_return_dto.rs` |
| 数据层 | `backend/src/business/erp/purchase_return_model.rs` |
| 过账 | `backend/src/business/erp/purchase_return_posting.rs` |
| 审批/内控桥 | `backend/src/business/erp/purchase_return_bridge.rs` |
| 接口层 | `backend/src/business/erp/purchase_return_handler.rs` |
| 词表 | `shared/doc_kind.rs`（`DocKind::PurchaseReturn`）、`shared/state_machine.rs`（`Entity::PurchaseReturn`）、`flow/linkage/spec.rs`（触发点 + 排除清单） |
| 联动接线 | `flow/handler.rs`（approve/reject/withdraw 回调 + 自审批闸）、`flow/internal_control.rs`（制单人排除） |
| 巡检登记 | `docs/architecture/snapshot-drift/10_checks.sql`（STATUS_ENTITY 增 `PURCHASE_RETURN`） |
| 前端 | `frontend/src/api/business/erp/purchase-return.ts`、`frontend/src/views/business/erp/purchase-return/index.vue`、`router/index.ts`、`doc-kinds.ts` |
| 验收脚本 | `docs/evidence/20261108-p203-purchase-return/e2e.py` |

## 2. 接口（真实前缀 `/api`）

| 方法 | 路径 | 权限码 |
|------|------|--------|
| POST | `/api/business/erp/purchase-return` | `business:erp:purchase-return:create` |
| GET | `/api/business/erp/purchase-return/page` | `…:list` |
| GET | `/api/business/erp/purchase-return/{id}` | `…:list` |
| GET | `/api/business/erp/purchase-return/source/{stockInId}` | `…:create` |
| POST | `/api/business/erp/purchase-return/{id}/submit` | `…:submit` |
| POST | `/api/business/erp/purchase-return/{id}/confirm` | `…:confirm` |
| POST | `/api/business/erp/purchase-return/{id}/cancel` | `…:confirm` |

## 3. 关键口径（每条都有实现 + 断言）

1. **防超退**：可退 = 入库量 − 未作废退货单累计（`exclude` 本单）。建单 / 提交 / 确认**三处**重跑。
2. **价格锁死**：单价 / 含税价 / 税率全部取原入库明细快照；请求里带 `unit_price` 只用于校验（不一致 400 + 66053）。
3. **金额倒挤**：`amount = round(qty×price,4)`、`with_tax = round(qty×price_incl,4)`、`tax = with_tax − amount` ⇒ 库层 `ck_biz_prt_tax` 精确成立。
4. **应付只冲未付**：`unpaid_part = min(退货含税, 应付未付)`；应付 `amount -= unpaid_part`（`paid_amount` 不动，库层 `paid_amount ≤ amount` 是物理下限）；已付部分转 `biz_supplier_receivable`（供应商应退款）。
5. **凭证**：借 2202 应付账款（价税合计）/ 贷 1405 库存商品（三本账真实出库成本）/ 贷 2221.01 进项税转出；成本与不含税金额的差额进 6401（计价差异），借贷精确平衡由构造保证 + `post_voucher` 再校验一次。
6. **出库单**：`out_type='OTHER'` + `source_type='PURCHASE_RETURN'`（四值域里没有「退货」，来源由 source_type 表达，与销售出库同决策）；`uk_biz_so_source` 保证一单一张。
7. **已收数量**：`biz_purchase_order_item.received_qty` 在 P2-3 已是派生值，本卡把 `POSTED_RECEIVED_SQL` 扩展为「已过账入库 − 已确认退货」——不造第二个真相源，退货作废后自动恢复。
8. **联动**：触发点 `PURCHASE_RETURN/CONFIRMED`（事务内 `run_excluding` + 提交后 `run_deferred`）；排除 `WRITE_INVENTORY` / `WRITE_INVENTORY_TXN` / `UPDATE_COST` / `UPDATE_PAYABLE_BALANCE` / **`GENERATE_VOUCHER`**（防凭证生凭证）。
9. **审批**：种子 `prt_approve`（4 节点 3 边：开始 → 采购经理 → 财务经理 → 结束，`graph_hash=9debbf67…507c`）；无已发布流程时默认 409，持 `business:erp:skip-approval` + ≥10 字理由才放行；制单人不能批自己那一票（403 + 65019）。

## 4. 验收结果

见 `e2e_output.txt`（隔离实例 `127.0.0.1:18090`，库 `cloudpivot_prt03`，由 P2-02 证据库升级到 V0332；**35 条断言 0 失败**）：

| 验收项 | 结果 |
|--------|------|
| ① 采购入库 100 个（夹具：POSTED 入库单 + 应付 1130 + 库存三本账） | 可退明细 100 / 单价 10 / 含税 11.3 |
| ② 退 30 → 库存 70 / 应付 791 / 凭证借贷平衡 339 | 见 e2e 断言 |
| ③ 再退 80 → 400 + 66051（可退只剩 70） | 见 e2e 断言 |
| ④ 退满 70 → 应付 0.0000 / SETTLED，库存 0 | 见 e2e 断言 |
| ⑤ 已付款 1130 后 退 3 → 应付不动（1130/1130/0 SETTLED）、供应商应退款 339、凭证平衡 | 见 e2e 断言 |
| ⑥ 单价篡改（9 元）→ 400 + 66053；重复确认 → 409 + 66052 | 见 e2e 断言 |

## 5. 已知限制

* 上游「采购入库」在 e2e 里用**夹具**（POSTED 入库单 + 应付 + 库存）构造，而不是走完
  「建 PO → 审批 → 收货 → 过账」全链（那是 P0-02/P2-B1 已验过的链路，本卡不重复跑）。
* 退货单**没有改单接口**：要改就作废重建（少一条「账没动但状态乱了」的窗口）。
* `APPROVED → CANCELLED` 需要原因；`SUBMITTED` 不能直接作废（先撤回）。
* `PRT_APPROVE` 未登记进 `FlowBizType` 词表 —— 与 P2-02 的 `PR_APPROVE` 同款现状（沿用既有
  先例，避免一次改动牵扯 `FlowBizType::ALL` / 前端镜像 / 健康检查清单；如需统一登记应单独开卡）。
