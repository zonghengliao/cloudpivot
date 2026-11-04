# P2-2 摆设字段处置 —— 验收证据（2026-11-04）

> 卡：`t_dfdbfcaf`【P2-2】摆设字段处置（填了以为生效，系统从不理它）
> 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §5 **DC-12**（P2）
> 口径文档：`docs/ERP-摆设字段口径.md`
> 提交：见卡内 `metadata.commit`

## 0. 结论速览（对卡面验收项逐条）

| 卡面验收项 | 结果 | 证据 |
|---|---|---|
| 单测：供应商额度 1 万、下单 2 万 → 单据带 `over_credit_limit=true` | ✅ | `unit-tests.txt`（`p2_2_credit_over_limit_marks_order_and_goes_to_approval`） |
| 单测：额度为空 → 不校验 | ✅ | 同上（`p2_2_credit_zero_limit_is_unlimited` + 纯函数 7 条） |
| 单测：已取消的采购单不计入累计 | ✅ | `p2_2_credit_cancelled_order_not_counted` |
| 真机：造一张超额度采购单 → 提交 → 看到超限标记 | ✅ **29/29 断言全过**（隔离实例 127.0.0.1:18099 + 副本库，真 HTTP：建供应商 → 建单 → 提交 → 读单据 + 读审批单 form_data） | `e2e-run.txt` |
| safety_stock：口径写进代码注释；空值不预警有单测 | ✅ 口径表在 `reminder_stock.rs` 文件头 + V0215 列注释 + `docs/ERP-摆设字段口径.md`；单测 4 条（0 不预警 / 恰好等于不报 / 负库存要报 / 在途仓不报） | `unit-tests.txt` |
| locked_qty：前端已隐藏/标注；后端注释说明预留 | ✅ 前端两个 Tab 的「锁定」列与页脚合计已撤（源码契约单测钉住）；后端 6 处注释 + V0215 列注释 | `unit-tests.txt`（`p2-2-stub-fields.test.ts`）、`db-evidence-test.txt` ③ |
| git commit（`fix(erp): P2-2 摆设字段处置`） | ✅ | 卡内 `metadata.commit` |
| 卡里逐条回答「填了会不会生效」 | ✅ 见下方 §1 | — |

## 1. 三个字段现在「填了会不会生效」

| 字段 | 填了会不会生效 | 生效方式（一句话） | 读它的代码 |
|---|---|---|---|
| `biz_supplier.credit_limit` | **会** | 采购单**提交**时现算「累计未结采购额（含本单）」比较额度：超限 → 单据打 `over_credit_limit=true` 并**强制走审批**（`skip-approval` 对超限单无效）；`0` = 不限 | `purchase_credit::verdict_of` ← `purchase_handler::submit_approval` |
| `biz_material.safety_stock` | **会** | 低库存提醒每天扫「物料 × 仓库的已过账结存 < 安全库存」→ 通知仓管主管；`0`/留空 = **不提醒** | `reminder_stock::scan_candidates` |
| `biz_inventory.locked_qty` | **不会**（刻意） | 预留列：无占用流程写入，恒 0；不参与可用量、不参与低库存；**前端已撤列**，只在列注释里标明「预留」 | 无 |

## 2. 改了什么（按文件）

| 文件 | 改动 |
|---|---|
| `backend/migrations/business/V0215__erp_p2_2_stub_fields.{up,down}.sql`（新） | 采购单加 3 列（`over_credit_limit` / `credit_exposure` / `credit_limit_snapshot`）+ 7 条列注释（信用额度 / 安全库存 / 两处锁定量）+ 幂等自检；down 精确回滚（列 DROP + 注释置 NULL） |
| `backend/src/business/erp/purchase_credit.rs`（新） | 信用额度**现算**守卫：`CreditVerdict::evaluate`（纯函数）、`committed_exposure`（SUM 排除 CANCELLED / 软删 / 本单）、`limit_of`、`verdict_of`、`write_verdict`、`assess_summary`；7 条纯函数单测 |
| `backend/src/business/erp/purchase_handler.rs` | `submit_approval` 提交前现算信用判定；超限 + 无已发布流程 → **409 + 66036**（skip-approval 不开放）；`submit_po_tx` 同事务写快照；`purchase_form_data` 增 `over_credit_limit`（JSON 布尔）+ `credit_exposure`/`credit_limit`（JSON 数字，抽出 `json_number`）；新增 6 条真库用例（含 skip 对照组） |
| `backend/src/business/erp/model_order.rs` / `dto.rs` | `PurchaseOrderRow` / `PurchaseOrderRes` 增三列（含「`false` ≠ 查过没问题」的注释） |
| `backend/src/business/erp/reminder_stock.rs` | 口径表写进文件头（当前库存 = `biz_inventory.qty`，不含在途/未过账/锁定量/库位）；SQL 改 `COALESCE(safety_stock,0) > 0` + 排除在途仓（`IS DISTINCT FROM`，不误伤已删仓库） |
| `backend/src/business/erp/reminder_tests.rs` | 新增 4 条真库用例（0 不预警 + 对照组 / 恰好等于 / 负库存 / 在途仓），把 `seed_low_stock` 抽成可传安全库存与数量的 `seed_stock_in` |
| `backend/src/business/erp/{model_inventory,dto,stockin_write,model_master}.rs` | 锁定量「预留」注释 6 处；`UpdateInventoryReq` 标注「无调用方，别接到路由上」；安全库存/信用额度字段注释指向口径文档 |
| `backend/src/shared/error_code.rs` | 新增 `ERP_CREDIT_LIMIT_EXCEEDED = 66036`（含为什么不复用 66019 的理由）+ 码表稳定性测试 |
| `frontend/src/views/business/erp/{inventory/index.vue,inventory/inventory-logic.ts}` | 撤掉 Tab1/Tab2 的「锁定」列与页脚「锁定」合计；顶部提示改成「数量 = 已过账结存（锁定量为预留字段，故不显示）」；`availableQty` 注释说明「现在恒等于数量」 |
| `frontend/src/views/business/erp/material/*` | 物料弹窗新增「安全库存」输入（`data-testid=mat-safety-stock`）+ 常驻提示；`MaterialFormState.safety_stock`、`safetyStockOf`（留空 → 0）、`quantityText`（去尾零）、校验（负数/非数字报错）、新建/编辑请求体显式带值 |
| `frontend/src/views/business/erp/purchase/*` | 列表「超信用额度」红色标签（tooltip 给累计/额度）；详情头部信用结论徽标（未评估 / 额度内 / 额度不限 / 超信用额度）；`creditVerdictOf` 三态纯函数 |
| `frontend/src/api/business/erp/index.ts` | `PurchaseOrderRow` 增三字段（含 `null` = 未评估的语义）；`InventoryRow.locked_qty` 标注预留 |
| `frontend/tests/unit/p2-2-stub-fields.test.ts`（新）+ `erp-material-list.test.ts` | 前端 11 条用例（三态显示 / 安全库存口径 / 锁定列已撤 / 采购页标签），并同步既有 payload 断言 |
| `scripts/verify/p22_stub_e2e.py`（新） | 真机验收 29 条断言（真 HTTP + 真库），自带前缀清场 |
| `docs/ERP-摆设字段口径.md`（新）+ `docs/README.md` | 三字段口径/结论/审批流条件分支配法/存量影响/单测索引；文档集总入口加一行 |

## 3. 关键设计决策

1. **策略 B（允许提交 + 强制审批）而不是 409 拒绝**：ERP 现实里「超信用要人复核」比「下不进去」
   正确。为了让 B 真的成立，超限单在没有已发布 `po_approve` 时**不许 skip-approval**
   （否则额度等于没生效），报文给两条出路（配流程 / 调额度）。
2. **现算 + 快照，不做「已用额度」缓存列**（R1）：累计额每次提交 `SUM` 现算（聚合走
   `idx_biz_po_supplier`，毫秒级）；**但**「提交那一刻查到什么」冻结成三列 ——
   当前值 vs 当时值是两件事，前者现算、后者快照，改额度不回写历史。
3. **`NULL` 是「未评估」的唯一标记**：`over_credit_limit=false` + `credit_exposure IS NULL`
   读作「没查过」；前端 `creditVerdictOf` 必须分三态显示，否则草稿会被显示成「额度内」。
4. **不把「超信用额度」分支种进 `po_approve`**：种子流程已是发布态且带版本快照（`graph_hash`），
   改它会让所有租户的审批路由悄悄变道。本卡保证「变量在 + 类型对 + 超限必进审批」，
   分支（多一道复核、给谁）由流程管理员在设计器里配；单测用真实 `form_data` 跑了一遍
   `flow::graph::eval`，证明 `form.over_credit_limit eq true` 真的会命中。
5. **安全库存「空」= 0**：列是 `NUMERIC NOT NULL DEFAULT 0`（V0137），没有 NULL 状态；
   前端留空 → 0 → 不预警，三处（前端 / SQL / 文档）用同一句话。
6. **锁定量选 B（标注预留）**：不做占用流程（销售模块还没建），但也不留一列恒 0 的假象 ——
   前端撤列、后端注释、列注释写明「预留」。

## 4. 证据文件

| 文件 | 内容 |
|---|---|
| `e2e-run.txt` | 真机 e2e 全文（29 条断言 + 清场；隔离实例 :18099 / 库 `cloudpivot_p16md`） |
| `unit-tests.txt` | 后端 4 组单测 + 前端单测的结果行 |
| `check.txt` | `bash scripts/check.sh`（fmt / clippy -D warnings / check / vue-tsc / 闸① / 闸②（跳过清单一致）/ C7 G1–G6 / 调色板）EXIT=0 |
| `db-evidence-test.txt` | 测试库侧：迁移登记 + 三列类型 + 7 条列注释 + 零回填自证 + 锁定量 0 行非零 |
| `db-evidence-prod.txt` | 生产库（144，只读）：存量事实、额度全 0 不影响存量单、状态分布、迁移前三列不存在 |
| `invariants-prod.txt` | 生产库 ERP 不变量 **40/40 通过，BLOCKER 0 / WARN 0** |

## 5. 诚实边界（没做到 / 没验的）

- **生产未部署**：`V0215` 未应用到生产（prod 未重启，`db-evidence-prod.txt` ④ 证明列还不存在）。
  迁移是纯 `ADD COLUMN` + `COMMENT`、幂等且带回滚脚本，下次重启自动应用。
- **UI 没做浏览器截图**：超限标签/信用徽标/隐藏锁定列是「源码契约 + 纯函数单测」验证的
  （`p2-2-stub-fields.test.ts`），没有跑 Playwright 真浏览器；接口侧的证据是真机 e2e。
- **`po_approve` 的「超信用额度」条件分支未由本卡种入**（理由见 §3.4），只保证变量与类型可用。
- 前端 `可用` 列保留：`locked_qty` 恒 0 时它等于数量，已在页面提示里写明原因。
