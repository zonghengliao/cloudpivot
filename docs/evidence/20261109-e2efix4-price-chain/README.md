# E2E-FIX-4：取价链标准售价写接口 + 含税 / 不含税口径标注

- 卡片：`t_4570b9e1`（E2E-FIX-4【P3】，来源：E2E-02 销售链演练）
- 日期：2026-11-09
- 影响面：销售取价链（`GET /api/business/erp/price/resolve`）、物料主数据写接口、销售订单页「自动带价」

## 一、修的是什么（两条 P3 缺陷）

### ① 物料标准售价没有写接口 → 取价链第 ④ 级（`STANDARD`）形同不存在

`CreateMaterialReq` / `UpdateMaterialReq` 都没有 `standard_price` 字段，第 ④ 级只能靠改库命中，
等于「5 级取价只实现了 3 级」。

**修复**：两个请求体都加 `standard_price`；`MaterialRes` 回显；前端物料页加「标准售价」输入框。

### ② 含税 / 不含税口径不标注 → 订单单价虚高一个税率

价格表里**两套口径并存**：

| 来源 | 存的值 | 口径 |
|---|---|---|
| `biz_customer_price`（协议价）/ `biz_price_tier`（阶梯价 / 促销价） | `price` | **含税** |
| `biz_material.standard_price`（标准售价） | `standard_price` | **不含税** |
| `biz_sales_order_item.price`（订单行单价） | `price` | **不含税** |

`GET /price/resolve` 原来只回 `{price, source, source_id}`，前端把**含税 120** 直接填进
**不含税**的订单行 → 单价虚高 13%（120 vs 106.1947），税额再按 13% 算一遍 → 整单金额全错。

**修复**：出参补三个口径字段，前端**只准用 `price_excl` 填单**，并在单价下方提示「含税价 120（税率 13%）」。

```json
{
  "price": "120.0000",
  "source": "AGREED",
  "source_id": "…uuid…",
  "tax_inclusive": true,
  "price_excl": "106.1947",
  "tax_rate": "13.0000"
}
```

规则（`price_model::tax_view`，纯函数 + 单测钉死）：

| `source` | `tax_inclusive` | `price_excl` | `tax_rate` |
|---|---|---|---|
| `AGREED` / `TIER_CUSTOMER` / `TIER_COMMON` | `true` | `price / (1 + rate/100)`，4 位四舍五入 | 物料税率 |
| `STANDARD` | `false` | `= price` | 物料税率 |
| `NONE` | `false` | `null` | `null` |

折算复用 `tax_rate::split`：先定不含税（4 位四舍五入）、税额 = 含税 − 不含税，
所以「不含税 + 税额 = 原含税价」恒成立（不会出现 1 分尾差）。

## 二、验收对照（全部来自真实 HTTP 实测，见 `live-99.txt`，35/35 断言绿）

| # | 验收项 | 实测结果 |
|---|---|---|
| ① | 物料建 / 改能设 `standard_price` → 第 ④ 级可命中 | 建 `11.5` → 取价 `STANDARD`；改成 `20` → 立刻给 `20.0000`；清空（`null`）→ 落 `NONE`；设 `0` → `400/10001` 且不改库 |
| ② | resolve 返回 `tax_inclusive` + `price_excl` + `tax_rate` | 出参**恰好 6 个字段**（`jq '.data\|keys\|length' == 6`） |
| ③ | 协议价 120（含税）→ `price_excl = 106.1947` | `120.0000 / 1.13 = 106.1947`（税率 13%） |
| ④ | 前端填单用 `price_excl`，订单单价 106.19 而不是 120 | 建销售订单 `SO-20261109-000001`：行单价 `106.194700`、落库 `106.1947` |
| ⑤ | 税额计算正确（106.19 × 13%） | 2 件：金额 `212.3894`、税额 `27.6106`、价税合计 `240.0000`（= 2 × 120，含税口径闭环） |
| ⑥ | cargo test + 前端 build + push | 见 `verification.txt`；前端 `typecheck` / `build` exit 0；commit 见卡内汇报 |

## 三、改动文件

**后端**（`use alun::prelude::*`；无新增 crate）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/dto.rs` | `CreateMaterialReq.standard_price: Option<Decimal>`、`UpdateMaterialReq.standard_price: Option<Option<Decimal>>`（三态：缺席=不改 / `null`=清空 / 有值=覆盖）、`MaterialRes.standard_price`；`require_standard_price`（`> 0`）+ 2 条单测 |
| `backend/src/business/erp/handler.rs` | 建物料 INSERT 带 `standard_price`；改物料按三态拼 `standard_price` 片段（`NULLIF(?::text,'')::numeric`，避免 canal 把占位符推断成 numeric）；真库用例 `material_standard_price_feeds_price_resolve` |
| `backend/src/business/erp/model_master.rs` | `MATERIAL_COLUMNS` + `MaterialRow.standard_price` |
| `backend/src/business/erp/price_model.rs` | `MaterialPrice.tax_rate`、`MATERIAL_SQL` 多取 `tax_rate`、`TaxView` + `tax_view()`（6 条单测，含 120→106.1947 闭环） |
| `backend/src/business/erp/price_dto.rs` | `PriceResolveRes` 三个新字段 + 契约文档；出参形状单测（3 → 6 字段） |
| `backend/src/business/erp/price_handler.rs` | `resolve_validated` 一并带回物料税率；组装出参口径字段 |

**前端**

| 文件 | 改动 |
|---|---|
| `frontend/src/api/business/erp/index.ts` | `MaterialRow.standard_price`、建改请求体字段 |
| `frontend/src/api/business/erp/customer-price.ts` | `PriceResolveRes` 三个新字段（带「填单用 price_excl」注释） |
| `frontend/src/views/business/erp/material/material-query.ts` | 表单字段、`standardPriceOf`、校验（`> 0`）、建改请求体、`STANDARD_PRICE_HINT` |
| `frontend/src/views/business/erp/material/index.vue` | 「标准售价」输入框 + 灰字提示 |
| `frontend/src/views/business/erp/sales-order/sales-order-query.ts` | `applyResolvedPrice` **改用 `price_excl` 填单**、税率跟随折算税率、`price_incl` / `price_tax_rate` 展示字段、`linePriceInclHint()` |
| `frontend/src/views/business/erp/sales-order/index.vue` | 单价下方「含税价 120（税率 13%）」提示；换物料时清掉口径提示；无价提示文案改「请手填不含税单价」 |

**测试 / 文档**

- `frontend/tests/unit/erp-sales-order.test.ts`：新增 6 条（`price_excl` 填单、口径提示、手动价不被覆盖、NONE 不清空、税额闭环）
- `frontend/tests/unit/erp-material-list.test.ts`：新增 1 条（标准售价三态 / 校验 / 回填）
- `frontend/tests/unit/p2-2-stub-fields.test.ts`：夹具补 `standard_price`（类型完整性）
- `docs/architecture/ARCH-7-销售模块设计.md` §15.3.3 / §15.3.4：出参口径表 + 标准售价维护入口

## 四、迁移

**本卡不需要新迁移**：`biz_material.standard_price` 由 **V0270**（`sales_price`）提供，列类型
`numeric(18,4)`、约束 `ck_biz_material_standard_price: standard_price IS NULL OR standard_price > 0`。

本卡据此把校验定为 **`> 0`**（不是 `>= 0`）：0 元不表示免费，要取消标准售价请留空（`NULL`）。
已应用迁移一行未改，`schema_migrations` 仍是 289 条（`live-99.txt` §11）。

## 五、怎么重跑

```bash
# 1) 起一个全新隔离库 + 隔离端口的实例，跑 35 条断言（不碰共享库 / 不碰 :8023）
bash docs/evidence/20261109-e2efix4-price-chain/live-99.sh   # DB / PORT / BIN 可覆盖，见脚本头

# 2) 纯逻辑 + 真库单测
cd backend
cargo test --lib business::erp::price_            # tax_view / resolve 全链 / 出参形状
cargo test --lib business::erp::dto::tests::material_standard
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/<隔离库>' \
  cargo test --lib business::erp::handler::tests::material

# 3) 前端
cd frontend && pnpm typecheck && pnpm test:unit && pnpm build
```

## 六、如实说明（未覆盖 / 既有红）

1. **全量 `cargo test`（3879 条）没跑完**：本卡只跑定向套件（`price_*` 84 条 + 物料相关 11 条，
   其中 9 条真库用例）。改动面集中在物料 DTO/INSERT/UPDATE 与取价出参，已被这些用例覆盖。
2. **前端单测有 2 条红，是 main 既有红，与本卡无关**：`erp-commission`（后端 `LEGAL_EDGES` 源码格式
   与前端解析漂移）、`erp-sales-order` 的 `create payload keeps dates…`（SO-42 加了 `currency_code`
   但该 deepEqual 没更新）。已在**干净 worktree（HEAD）**复现同样 2 条红，见 `verification.txt` §5。
3. **clippy `-D warnings` 在 main 上本就红**（lib 3 条 + test 18 条，全在 flow/pdf/invoice/receivable/
   sales_order_model 等**非本卡文件**）。本卡 6 个后端文件零诊断。
4. **145 主实例没重启**：本卡用 99 的隔离库 `cp_e2efix4` + 端口 18321 + 本机 debug 二进制实测，
   不碰任何共享实例；145/144 的部署是发布环节的事（无新迁移需要应用）。
