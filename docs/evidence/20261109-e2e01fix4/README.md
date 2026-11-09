# E2E01-FIX-4 证据：ERP 出参三段式口径（`header` / `items` / `meta`）+ 兼容层

卡 t_84ddfbad · 2026-11-09 · 分支 `main` · 代码提交 `bf1a4340`

## 一句话

三个高频接口在**同一份响应里**同时给出新字段（`header` / `items` / `meta`）与旧字段
（`purchase_order`+`lines` / `order` / `head`），新旧值由**同一个构造器 clone** 出来，
`cargo` 单测与 145 真机深度比对都证明「旧字段值 = 新字段值」；旧字段一个没删。

## 三个接口改成了什么

| 接口 | 旧字段 | 新增 | `meta` 内容 |
|------|--------|------|-------------|
| `POST /api/business/erp/price-inquiry` | `head`、`items` | `header`、`meta` | `quote_count` / `comparison_count` / `can_to_po` |
| `POST /api/business/erp/purchase-requisition/{id}/to-po` | `purchase_order`、`lines` | `header`、`items`、`meta` | `merged_count` / `requisition_nos` |
| `POST /api/business/erp/purchase-order/{id}/submit-approval` | `order`、`items` | `header`、`meta` | `instance_id` / `instance_no` / `flow_status` |

> 采购订单与询价单的详情结构是「一个 DTO 喂多个接口」：建单 / 改单 / 状态流转 / GET 详情
> 全部走同一个 `detail_of`，所以这些接口**一起**拿到了新字段（同一份契约，不是只改一个入口）。

## 验收对照

| 卡面验收 | 结论 | 证据 |
|---|---|---|
| ① 三个接口同时返回 header/items 和旧字段 | PASS | `raw-145/e2e01fix4_run_8033.log` 的 ①.2 / ②.4+②.5 / ③.2+③.3（键名列表实测打印） |
| ② 旧字段值 = 新字段值（不是两套逻辑） | PASS | 同日志 ②.1（header==head）、②.6（header==purchase_order）、②.7（items==lines）、③.4（header==order），全部**深度相等**；单测 `doc_shape_tests.rs` 8 条同值断言 |
| ③ 前端三页面改用 header/items 后功能正常 | PASS（构建产物级） | `raw-145/deploy-and-dist.txt` §3：三个页面 chunk 里出现 `header.inquiry_no` / `header.po_no` / `header.order_no`；§4：`head.inquiry_no`、`purchase_order.po_no` 已无命中；源码 `grep detail\.order` = 0 处。`pnpm typecheck` + `vite build` 均通过 |
| ④ docs 有「出参口径」一节 + 待迁移清单 | PASS | `docs/编码规范.md` §1.12（脚本 ④.1–④.3 断言标题/三段式/清单存在） |
| ⑤ cargo test + 前端 build | PASS | `cargo test --lib` **3949 passed / 0 failed**；`pnpm typecheck` + `pnpm build` 通过（见下「测试」） |

## 145 真机验收

```
python3 scripts/verify/e2e01fix4_doc_shape.py --base http://127.0.0.1:8033/api
→ PASS=32 FAIL=0（连跑两次，均 0 失败；夹具零残留）
```

覆盖：真 token 登录 → 取现存主数据 → 建询价单 → 建采购申请 → 转采购订单 → 提交采购订单审批
→ 每条响应做新旧字段**深度比对** → 收尾按外键顺序删夹具并回查 count=0。

脚本对采购申请用的是「自建夹具自备状态」（提交后直接把这张**自己建的单**推成 `APPROVED`），
因为本卡验的是**出参形状**不是审批链；采购订单的 `submit-approval` 走**真接口**，
拿到真审批实例（`FLW-20261109-…`，`flow_status=PENDING`）。

## 测试与构建

| 项 | 结果 |
|---|---|
| `cargo test --lib`（干净 HEAD `bf1a4340`，独立 `CARGO_TARGET_DIR`） | **3949 passed / 0 failed**（原 3936 + 新增 13：`doc_shape` 5 条 + `doc_shape_tests` 8 条） |
| `rustfmt --edition 2024 --check` 全部改动文件 | 干净 |
| `cargo clippy --lib -- -D warnings` | **我的 9 个后端文件零告警**；整仓当前红在别人的代码上（见下「已知遗留」） |
| `pnpm typecheck`（vue-tsc --noEmit） | 通过（无输出即无错） |
| `pnpm build` | 通过（产物落到 145 `frontend/dist`，入口 `assets/index-CAu74by0.js`，nginx :80 实测一致） |

## 已知遗留（**不是本卡引入**）

`cargo clippy --lib -- -D warnings` 在 99 当前工作区报 4 条，全部落在**本卡没碰过的文件**上
（`git show --stat bf1a4340` 只列出本卡 19 个文件）：

```
src/business/erp/contract_bridge.rs:52:29   unused import: `engine`
src/business/attachment/model.rs:879:1      this function has too many arguments (8/7)
src/business/erp/invoice.rs:1912:26         explicit lifetimes could be elided: 'a
src/business/erp/receivable.rs:782:5        doc list item without indentation
src/business/erp/sales_order_model.rs:661:1 this function has too many arguments (8/7)
```

其中 `attachment/model.rs` 来自同日的 V0362 附件卡、`contract_bridge.rs` 的未用 import 来自流程卡的
改动；这几处由各自卡的主人收口，本卡不动别人的代码（避免和别人的在途改动抢同一文件）。

## 文件清单

后端（9）
- `backend/src/business/erp/doc_shape.rs`（新，`DocMeta` 唯一定义处 + 5 条单测）
- `backend/src/business/erp/doc_shape_tests.rs`（新，8 条新旧同值断言）
- `backend/src/business/erp/mod.rs`
- `backend/src/business/erp/purchase_requisition_dto.rs` / `purchase_requisition_handler.rs`
- `backend/src/business/erp/purchase_dto.rs` / `purchase_handler.rs`
- `backend/src/business/erp/price_inquiry_dto.rs` / `price_inquiry_handler.rs`

前端（8）
- `frontend/src/api/business/erp/doc-meta.ts`（新）
- `frontend/src/api/business/erp/index.ts` / `price-inquiry.ts` / `purchase-requisition.ts`
- `frontend/src/views/business/erp/price-inquiry/index.vue` / `price-inquiry-logic.ts`
- `frontend/src/views/business/erp/purchase/index.vue` / `purchase-requisition/index.vue`

文档与脚本（2）
- `docs/编码规范.md` §1.12（三段式 + 兼容期规则 + 已统一表 + 待迁移清单 14 项）
- `scripts/verify/e2e01fix4_doc_shape.py`（真机验收，32 项断言）

## 原始产物

- `raw-145/e2e01fix4_run_8033.log` — 145 上对 :8033 正式服务的完整验收输出（PASS=32 FAIL=0）
- `raw-145/deploy-and-dist.txt` — 145 的 HEAD / 二进制 mtime / health / dist 切换 / 构建产物断言
