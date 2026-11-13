# P1-04 单据明细超 200 行 → 引导用导入通道（证据）

- 卡：`t_a7833f95`（P1-04【体验】；规划清单里叫 P1-07）
- 决策（2026-11-13 老大拍板 · 方案 B）：**不做**万行明细，**做**「明确拒绝 + 可操作提示」
- 落点：后端 7 个单据的 `MAX_LINES` 拦截文案 + 前端 7 个单据页的明细表上方黄字提示（含导入向导入口）

## 一、改了什么（逐文件）

### 后端：文案收成一份

| 文件 | 改动 |
|:--|:--|
| `backend/src/business/shared/doc_lines.rs` | **新增**。`DEFAULT_MAX_LINES=200` / `IMPORT_WIZARD_HINT` / `too_many_lines(limit, actual)` —— 超限文案的唯一出处 + 3 条纯逻辑单测 |
| `backend/src/business/shared/mod.rs` | 挂 `pub mod doc_lines;` + 模块分工表补一行 |
| `backend/src/business/erp/stockin_dto.rs` | `CreateStockInReq::prepare()` 行数闸改用 `doc_lines::too_many_lines`；补 2 条单测（新 `mod tests`） |
| `backend/src/business/erp/purchase_dto.rs` | `CreatePurchaseOrderReq::prepare()` 同上；补 2 条单测 |
| `backend/src/business/erp/transfer_dto.rs` | `CreateTransferReq::validate()` 同上；补 2 条单测 |
| `backend/src/business/erp/stockout_dto.rs` | `CreateStockOutReq::check_items()` 同上；补 2 条单测 |
| `backend/src/business/erp/price_inquiry_dto.rs` | **新增** `check_inquiry_items()` / `check_quote_items()`（行数闸进 DTO，原来写在 handler 里、不可单测）；补 4 条单测 |
| `backend/src/business/erp/price_inquiry_handler.rs` | 两处（建询价 / 录报价）改为 `check_*_items(&req.items)?`；不再自己 `format!` |
| `backend/src/business/erp/purchase_requisition_dto.rs` | **新增** `check_items(&[RequisitionItemReq])`；补 3 条单测 |
| `backend/src/business/erp/purchase_requisition_handler.rs` | 建单前的行数闸改为 `check_items(&req.items)?` |
| `backend/src/business/erp/purchase_return_dto.rs` | **新增** `check_items(&[CreateReturnItemReq])`；补 3 条单测 |
| `backend/src/business/erp/purchase_return_handler.rs` | 建单前的行数闸改为 `dto::check_items(&req.items)?` |

文案（后端 400 的 `message`，7 个单据逐字一致）：

```text
明细最多 200 行（当前 301 行）。大批量请用「导入向导」：业务 → 通用底座 → 导入向导（导入通道支持 2 万行）
```

- **错误码没动**：仍是各文件原来的 `ApiError::new(400, PARAM_INVALID, …)`，只换文案。
- **上限值没动**：各文件自己的 `MAX_LINES` 仍在上限处，`doc_lines` 只产文案（改上限改各自的常量）。
- 顺带收益：询价 / 采购申请 / 采购退货这 3 个原来把行数闸写在 **handler** 里（必须有库才能测），
  现在闸在 DTO 的纯函数上，201 行 → 400 可以直接单测（见 `01_backend_unit_tests.out`）。

### 前端：同一句话 + 可点入口（7 个页面）

| 文件 | 改动 |
|:--|:--|
| `frontend/src/views/business/erp/shared/detail-line-limit.ts` | **新增**。`DETAIL_MAX_LINES` / `IMPORT_WIZARD_ROUTE_NAME` / `importWizardHint()` / `overDetailLineLimit()` / `detailLineLimitMessage()` —— 与后端 `doc_lines.rs` 同口径的前端版本 |
| `frontend/src/views/business/erp/shared/DetailLineLimitAlert.vue` | **新增**。`<t-alert theme="warning">`，`count > limit` 才渲染；文案 + 「打开导入向导」（`router-link` 指向路由名 `BusinessImpexpImport`） |
| `frontend/src/views/business/erp/stockin/index.vue` | 明细表上方挂 `<DetailLineLimitAlert :count="form.items.length" :limit="STOCKIN_MAX_LINES" testid="stockin-line-limit" />` |
| `frontend/src/views/business/erp/purchase/index.vue` | 同上，`:limit="MAX_LINES"`，`testid="purchase-line-limit"` |
| `frontend/src/views/business/erp/transfer/index.vue` | 同上（该页无上限常量 → 用默认 200），`testid="transfer-line-limit"` |
| `frontend/src/views/business/erp/stock-out/index.vue` | 同上，`:limit="STOCK_OUT_MAX_LINES"`，`testid="stockout-line-limit"` |
| `frontend/src/views/business/erp/price-inquiry/index.vue` | 同上，`:count="createForm.items.length"`，`:limit="MAX_LINES"`，`testid="piq-line-limit"` |
| `frontend/src/views/business/erp/purchase-requisition/index.vue` | 同上，`:count="createForm.lines.length"`，`testid="prq-line-limit"` |
| `frontend/src/views/business/erp/purchase-return/index.vue` | 同上，`:count="source.items.length"`（退货明细来自来源入库单），`testid="prt-line-limit"` |
| `frontend/tests/unit/detail-line-limit.test.ts` | **新增**。16 条单测：文案三段 / 边界（200 合法、201 提示）/ 7 个页面逐个有组件且绑定到明细数组 / 盘点单**不许**挂 200 提示 / 组件不阻断编辑 |

- **不阻断编辑**：组件只读 `count` / `limit`，没有 emit、不 disable 任何按钮；超限只在保存时由后端 400 拦。
- **盘点单不适用**：`stock-take` 的上限是 2000（卡面明确「保持不动」），前端单测专门断言它**没有**挂这个 200 行提示。

## 二、验证（原始输出见同目录）

| # | 命令 | 结果 | 原始输出 |
|:--:|:--|:--|:--|
| 1 | `cargo test --lib p104_line_limit_tests`（7 个单据的 201/200 边界） | **16 passed / 0 failed** | `01_backend_unit_tests.out` |
| 2 | `cargo test --lib stockin_dto::`（入库单 2 条） | **2 passed / 0 failed** | `01_backend_unit_tests.out` |
| 3 | `cargo test --lib shared::doc_lines`（文案内核 3 条） | **3 passed / 0 failed** | `01_backend_unit_tests.out` |
| 4 | `cargo fmt -p cloudpivot -- --check` | exit 0 | `02_backend_fmt_clippy.out` |
| 5 | `cargo clippy --lib --tests -- -D warnings` | exit 0（零 warning） | `02_backend_fmt_clippy.out` |
| 6 | `pnpm test:unit`（全量） | **1761 passed / 0 failed**（含本卡 16 条） | `03_frontend_unit.out` |
| 7 | `pnpm typecheck` | exit 0 | `04_frontend_typecheck.out` |
| 8 | `pnpm typecheck:unit`（含 `tests/unit`） | exit 0 | `04_frontend_typecheck.out` |
| 9 | `pnpm build`（vite build） | **✓ built in 44.28s** | `05_frontend_build.out` |

单测口径（卡面验收「201 行 → 400 且 message 含「导入向导」；200 行 → 通过」）：

- 入库 / 采购 / 调拨 / 出库：直接走各自的真实校验入口（`prepare()` / `validate()` / `check_items()`），
  真构造 200 / 201 行请求体；
- 询价 / 采购申请 / 采购退货：走新提到 DTO 的行数闸（`check_*`），是建单 handler 调的就是这一句；
- 每条 201 行用例都断言三件事：`status == 400`、`msg` 含「导入向导」、`msg` 含「当前 201 行」。

## 三、这轮没做 / 需要下一手的事

1. ~~**「导入向导」目前还不能导这 7 个单据的明细**（如实记录，需要产品决策）。~~
   → **已决策：改文案（2026-11-13，`t_35a19386`）** —— 决策文档 `docs/单据明细导入口径-决策.md`
   （commit `f495ef26`），落地卡 `t_778bf23f`（commit 见 `git log --grep t_778bf23f`）。
   7 个单据后半句改为「请拆成多张单据提交，每张不超过 200 行。」，
   黄条的「打开导入向导」入口一并删除；「单据明细 Excel 导入」进需求池 `RP-2026-009`（触发制）。原始证据：`06_copy_split_doc.out`。

   以下为当时的实测记录（保留作依据）：
   实测 `backend/src/business/impexp/builtin/mod.rs` 注册的导入定义只有 9 个：
   `system.dict_item`、`erp.supplier` / `erp.material` / `erp.unit` / `erp.account` / `erp.warehouse` / `erp.location`、
   `hr.attendance`、`hr.employee`；`erp_doc.rs` 只注册了 **3 个导出**（`erp.stock_in` / `erp.payable` / `erp.voucher`），
   注释原文：**「这些单据不走 Excel 导入」**。
   ⇒ 用户按提示点进「导入向导」，下拉里找不到「采购订单 / 出入库单」这类业务。
   「导入通道支持 2 万行」这句本身是真的（`impexp/builtin/master_data/mod.rs:79` 与 `hr.rs:94` 的 `MAX_ROWS = 20_000`），
   差的是**这 7 个单据没有导入定义**。已另建卡给产品经理决策（补导入定义 or 改文案为「拆单提交」）。
2. **盘点单（`stocktake`，上限 2000）文案未改**：卡面明确「保持不动」。它的超限文案仍是
   `抽盘明细最多 2000 行（收到 2001 行）`，同样是纯技术口吻 —— 若后续要统一口径，把
   `stocktake_dto.rs:195/271` 两处换成 `doc_lines::too_many_lines` 即可（`doc_lines` 已按下限参数化，单测里覆盖了 2000）。
3. **销售域（销售订单 / 发货 / 销售退货）只有前端 `MAX_LINES = 200` 的限制，后端没有行数闸**
   （`backend/src/business/erp/sales_order_*` / `shipment_*` 里没有 `items.len() >` 校验）。
   不是本卡范围，但如果哪天补后端口径，前端这些页也应一起挂 `DetailLineLimitAlert`。

## 四、P1-04 后续（`t_778bf23f`，2026-11-13）：文案改「拆单提交」

| 位置 | 改动 |
|:--|:--|
| `backend/.../shared/doc_lines.rs` | `IMPORT_WIZARD_HINT` → **`SPLIT_DOC_HINT`**（「请拆成多张单据提交，每张不超过 200 行。」）；新增 `too_many_lines_with(limit, actual, hint)` / `too_many_lines_labeled(label, …)`（盘点单两处入口带「抽盘 / 实盘」前缀）；单测加反向断言 `!msg.contains("导入向导")` / `!msg.contains("2 万行")` |
| `backend/.../erp/stocktake_dto.rs` | `:195` / `:271` 两处 `format!` 收口到 `doc_lines::too_many_lines_labeled(…)`，`收到 N 行` → `当前 N 行` + 尾句「请按仓库或物料分类分多次抽盘。」；上限 2000 **不动**；新增 3 条单测 |
| `frontend/.../shared/detail-line-limit.ts` | `importWizardHint()` → **`splitDocumentHint()`**；删除 `IMPORT_WIZARD_ROUTE_NAME` |
| `frontend/.../shared/DetailLineLimitAlert.vue` | 删掉 `<router-link>`「打开导入向导」入口与 `__link` 样式，只渲染 `{{ message }}` |
| `frontend/.../stock-take/stock-take-logic.ts` | 抽盘超限文案补尾句（`STOCK_TAKE_TOO_MANY_HINT`），与后端逐字一致 |
| `frontend/tests/unit/detail-line-limit.test.ts` | 三段断言换成「上限 / 当前行数 / 拆单动作」+ 反向断言；删「链接指向真实路由名」，换「组件不再带任何跳转」；新增盘点口径用例 |

验证见 `06_copy_split_doc.out`（含真 HTTP 端到端：201 行入库单 → 400，`msg` 与前端黄条**逐字节相同**；200 行放行）。
