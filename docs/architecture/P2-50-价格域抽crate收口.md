# P2-50 价格域抽 crate 收口 —— 作废（`price` 随 C13 `cloudpivot-erp` 整包搬迁）

> 卡：`t_9ddee8b3`（P2-50【P2-C】抽 `erp-price` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `c1f7aa76`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41`（`11f490f2`）、`P2-42`（`e9a31e96`，§4 已列本卡行）、`P2-43`（`939ec719`）、
> `P2-44`（`810ce2c7`）、`P2-45`（`5a2bc55a`）、`P2-46`（`ca11c5dc`）、`P2-47`（`4648b330`）、
> `P2-48`（`6a473699`）、`P2-49`（`c1f7aa76`）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-erp-price/`，不搬 9 个文件 / 6 970 行，不改任何
`crate::business::erp::price` import，不动 `app/Cargo.toml`，不动 `backend/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；
卡面验收命令 `cargo test -p cloudpivot-erp-price` 因此按预期失败（不是回归，见 §4）。

权威依据已入仓：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 **288** 行：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同表第 **226** 行 C13 = `cloudpivot-erp`｜`erp`（**一个包，不拆子域**）｜切向仅 `task 8` + `business/task 4` = 12 行；
- 同节第 **292–295** 行：`erp` 内部 51 个节点互相咬住 ⇒ 一个包，包内继续用目录分层；
- `P2-42` §4 台账第 **89** 行（P2-50）= `9 / 6 970`｜SCC 51｜入边 `3（2）`｜出边 `7`｜`15 行`｜`退 1`｜落点 `C13 包内`。

**本卡与前 8 张有一处口径差异，先写在最前面**（细节见 §2.5、§3）：

> `price` 的入边 3 行**全部是测试代码**（`erp/handler.rs` 的 `#[tokio::test]` 2 行 +
> `#[cfg(test)] mod doc_shape_tests` 1 行），**生产代码入边 = 0**；
> 剔除测试边后 `price` 在生产口径里是**单点 SCC（叶子）**，不在 29 节点的生产环里。
> 所以「51 节点 SCC ⇒ 不可能拆」这句话对 `price` **不成立**（51 是含测试边的数字）。
> 本卡作废的依据是 §3 的另外四条（包级环 / 收益 2.35% / 夹具跨包不可见 / 与 C13 决策冲突），
> **不是**「双向咬合」。下一轮若要重开本卡，请从 §3 第 2 条开始反驳，别拿 SCC 当理由。

## 2. `price` 的实测（只读、无库依赖，2026-11-15 于 99）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain price --scope erp-sub
域：price（scope=erp-sub）
  规模：9 个文件 / 6970 行
  SCC：51 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（50）：asset, budget, contract, cost, customer, erp#root:approval_gate,
    erp#root:cash_flow, erp#root:code_policy, erp#root:doc_reader_impl, erp#root:dto,
    erp#root:fx_revaluation, erp#root:handler, erp#root:income_statement,
    erp#root:inventory_audit, erp#root:invoice, erp#root:ledger, erp#root:model_master,
    erp#root:opening, erp#root:order_pdf, erp#root:order_pdf_model, erp#root:reminder,
    erp#root:reminder_contract, erp#root:reminder_payable, erp#root:reminder_purchase,
    erp#root:reminder_stock, erp#root:requisition_bridge, erp#root:serial_dto,
    erp#root:serial_handler, erp#root:serial_model, erp#root:unit_guard, expense,
    ext:changelog, ext:flow, ext:hr, ext:impexp, ext:task, linkage, payable, period,
    purchase, receivable, report, sales, shipment, stockin, stockout, stocktake, transfer,
    voucher, year
  回边（留在应用 crate 的代码 → 本域）：3 处代码级
    erp#root:doc_shape_tests code=  1 all=  1  business/erp/doc_shape_tests.rs:21
    erp#root:handler       code=  2 all=  2  business/erp/handler.rs:3952, business/erp/handler.rs:4013
  出边（本域 → 其它）：7 个目标
    voucher                code=  4 all=  4
    purchase               code=  3 all=  4
    erp#root:model_base    code=  2 all=  2
    erp#root:doc_shape     code=  2 all=  2
    erp#root:tax_rate      code=  2 all=  3
    erp#root:model         code=  1 all=  1
    erp#root:unit_guard    code=  1 all=  1
  层次判据：本步要切 7 条 / 15 行 —— 出边指向更下层以外：voucher(4), purchase(3),
            erp#root:model_base(2), erp#root:doc_shape(2), erp#root:tax_rate(2),
            erp#root:model(1), erp#root:unit_guard(1)
  结论：不可抽（3 处代码级回边，2 个来源）

$ python3 scripts/verify/crate_split_readiness.py --domain price --scope erp-sub --check        # 旧口径
exit=1
$ python3 scripts/verify/crate_split_readiness.py --domain price --scope erp-sub --layer-check  # 层次口径
exit=1
```

### 2.1 规模（与台账逐项对得上）

```bash
$ cd /opt/cloudpivot/backend && wc -l app/src/business/erp/price/*.rs
   21  mod.rs
  194  price_handler.rs
  351  price_dto.rs
  819  price_admin_model.rs
  839  price_inquiry_model.rs
  856  price_inquiry_handler.rs
  962  price_model.rs
 1094  price_inquiry_dto.rs
 1834  price_admin_handler.rs
 6970  total
$ find app/src/business/erp/price -name '*.rs' | wc -l        # 9（无嵌套子目录）
$ find app/src/business/erp -name '*.rs' | wc -l              # 353
$ find app/src/business/erp -name '*.rs' -exec cat {} + | wc -l   # 296 388
```

占比：**9 / 353 文件 = 2.55%**、**6 970 / 296 388 行 = 2.35%**（`erp` 占全仓约 50%）。

### 2.2 入边：生产 0 行 / 测试 3 行（2 源）/ 文档注释 1 行

**全仓严格 grep（`erp::price` 带词界，排除本域目录）共 4 行**：

```
business/erp/purchase/purchase_requisition_dto.rs:3   //! 与 [`crate::business::erp::price::price_inquiry_dto`] / …   ← 文档注释
business/erp/handler.rs:3952        use crate::business::erp::price::price_dto::PriceResolveQuery;                      ← 测试
business/erp/handler.rs:4013        crate::business::erp::price::price_handler::resolve_price(PriceResolveQuery {         ← 测试
business/erp/doc_shape_tests.rs:21  use crate::business::erp::price::price_inquiry_dto::{PriceInquiryDetailRes, …};       ← 测试
```

三条「测试」的判定依据（逐行看过，不是猜）：

- `handler.rs:2502` 是 `mod tests {`，3952 / 4013 都落在其内，且所在用例是
  `#[tokio::test] async fn material_standard_price_feeds_price_resolve`（E2E-FIX-4 标准售价闭环）；
- `erp/mod.rs:142-143` 是 `#[cfg(test)] mod doc_shape_tests;` —— `doc_shape_tests.rs` 整个文件都是测试模块。

**另有 3 行相对引用在 `erp/mod.rs` 里**（`price::x` 无 `erp::` 前缀，不在上面的 grep 口径内，属包内文档表）：

```
business/erp/mod.rs:89  //! | [`price::price_inquiry_dto`] | 询比价契约（P2-01）… |
business/erp/mod.rs:90  //! | [`price::price_inquiry_model`] | 询比价数据层（V0321）… |
business/erp/mod.rs:91  //! | [`price::price_inquiry_handler`] | 询比价 6 个接口… |
business/erp/mod.rs:173 pub mod price;      ← 模块声明（C13 整包搬迁时随 mod.rs 一起走）
```

⇒ **入边账目：生产 0 行 / 测试 3 行 / 注释 4 处（1 行 `erp::price` 注释 + 3 行相对文档表）**。
脚本报「3 处代码级 / 2 个来源」是**含测试代码**的口径（脚本只跳过 `//` 开头的行，不区分 `#[cfg(test)]`）。

### 2.3 出边：脚本口径 15 行 / 7 目标；其中生产 12 行、测试 3 行

**全部 27 行 `crate::` 引用逐行复核**（`price/` 下 `crate::business::` 共 20 行，剔自引用后 = 出边 17 行 + 内核 `business::shared` 3 行；
另有 `crate::shared::` / `crate::utils` 9 行，合计 29 行 = 17 + 12 内核）：

| # | 位置 | 目标 | 生产/测试 |
|:--:|:--|:--|:--:|
| 1 | `price_admin_handler.rs:145` `crate::business::erp::voucher::today_cst(Utc::now())` | voucher | 生产 |
| 2 | `price_admin_handler.rs:1573` `use …::voucher::voucher_query::tests::{DB_LOCK, ensure_db};` | voucher | **测试** |
| 3 | `price_dto.rs:131` `None => crate::business::erp::voucher::today_cst(…)` | voucher | 生产 |
| 4 | `price_dto.rs:291` `assert_eq!(req.date, …::voucher::today_cst(…))` | voucher | **测试** |
| 5 | `price_admin_model.rs:32` `use …::model_base::{find_by_id, page_of};` | model_base | 生产 |
| 6 | `price_admin_model.rs:37` `pub use …::model_base::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE, normalize_page};` | model_base | 生产 |
| 7 | `price_inquiry_dto.rs:3` `//! 与 [`…::purchase::purchase_dto`] 同一套约定` | purchase | **注释** |
| 8 | `price_inquiry_dto.rs:656` `pub meta: crate::business::erp::doc_shape::DocMeta,` | doc_shape | 生产 |
| 9 | `price_inquiry_dto.rs:681` `let meta = …::doc_shape::DocMeta::empty()` | doc_shape | 生产 |
| 10 | `price_inquiry_handler.rs:56` `use …::model::normalize_page;` | model | 生产 |
| 11 | `price_inquiry_handler.rs:69` `use …::purchase::purchase_dto::{CreatePurchaseOrderReq, …};` | purchase | 生产 |
| 12 | `price_inquiry_handler.rs:70` `use …::purchase::purchase_handler;` | purchase | 生产 |
| 13 | `price_inquiry_handler.rs:71` `use …::purchase::purchase_model::{self, PRICE_MODE_EXCL};` | purchase | 生产 |
| 14 | `price_inquiry_handler.rs:72` `use …::unit_guard;` | unit_guard | 生产 |
| 15 | `price_model.rs:320` `/// 折算复用 [`…::tax_rate::split`]：…` | tax_rate | **注释** |
| 16 | `price_model.rs:349` `let (price_excl, _tax) = …::tax_rate::split(price, material_tax_rate);` | tax_rate | 生产 |
| 17 | `price_model.rs:609` `assert_eq!(…::tax_rate::tax_of(excl, dec("13")), tax);` | tax_rate | **测试** |

⇒ **出边 = 15 行代码 / 7 目标（脚本口径，= 17 行减 2 行文档注释）**，细分：

```
生产 12 行：voucher 2、purchase 3、model_base 2、doc_shape 2、tax_rate 1、model 1、unit_guard 1
测试  3 行：voucher 2（admin_handler:1573、dto:291）、tax_rate 1（price_model:609）
文档  2 行：purchase 1（inquiry_dto:3）、tax_rate 1（price_model:320）
```

**没有一条指向 L1 内核之外的其他域**：无 `flow`/`hr`/`impexp`/`attachment`/`notice` 等跨域接缝（与
`expense`(10 行)、`contract`(27 行) 不同），全部落在 `erp` 包内兄弟模块。这决定了 C13 后的抽包代价比前几张卡小，
但仍不足以单独立包（§3）。

### 2.4 内核引用 12 行（L1，合法下层，不计切边）

```
crate::shared::error_code      5 行：admin_handler:51、dto:50、handler:47、inquiry_dto:38、inquiry_handler:73
crate::utils                   4 行：admin_handler:52、admin_model:34、inquiry_handler:74、inquiry_model:38
crate::business::shared::*     3 行：admin_model:33 scope、inquiry_dto:37 doc_lines、inquiry_model:37 scope
```

合计 **12 行**（`grep -rn "crate::business::" price | grep -v price 自引用` = 20 行 = 出边 17 + 内核 3；
再加 `crate::shared::`/`crate::utils` 9 行 ⇒ 全目录 29 行 `crate::` 引用，逐项对得上）。

### 2.5 口径修正：SCC「51」是含测试边的数字；生产口径是 29 节点，`price` 是**单点（叶子）**

脚本把 `#[cfg(test)]` 块内的边也算进 `code`（只排除 `//` 注释行），所以它报的 SCC（51）与本卡关心的
「生产中能不能成包」是两个问题。本卡用更严的口径重算了一遍（剔除 ① `#[cfg(test)]` 门控块内的代码，
② 整文件测试模块 = `*_tests.rs` / `*_test_support.rs` 以及父 `mod.rs` 里 `#[cfg(test)] mod x;` 声明的 121 个文件）：

```bash
# 复现（一次性分析脚本，未入仓 —— 逻辑 30 行，见本节末说明）
python3 <scratch>/price_prod_edges2.py
整文件测试模块数 = 121 （erp 共 594 个 .rs）

=== price 生产出边 ===
  -> erp#root:doc_shape: 2 行   price_inquiry_dto.rs:656, :681
  -> erp#root:model:     1 行   price_inquiry_handler.rs:56
  -> erp#root:model_base:2 行   price_admin_model.rs:32, :37
  -> erp#root:tax_rate:  1 行   price_model.rs:349
  -> erp#root:unit_guard:1 行   price_inquiry_handler.rs:72
  -> purchase:           3 行   price_inquiry_handler.rs:69, :70, :71
  -> voucher:            2 行   price_admin_handler.rs:145, price_dto.rs:131
  合计 12 行 / 7 目标
=== price 测试出边 ===
  -> erp#root:tax_rate: 1 行 / -> voucher: 2 行

=== 生产口径 SCC（剔除全部测试边）===
  size=29: budget, contract, cost, customer, erp#root:dto, erp#root:fx_revaluation,
           erp#root:handler, erp#root:income_statement, erp#root:invoice, erp#root:ledger,
           erp#root:model_master, erp#root:opening, erp#root:reminder,
           erp#root:requisition_bridge, erp#root:serial_dto, erp#root:serial_model,
           ext:task, linkage, payable, period, purchase, receivable, report, sales,
           shipment, stockin, stockout, voucher, year
  size=4:  ext:changelog, ext:flow, ext:hr, ext:impexp
  price 所在 SCC: ['price']          ← 单点
=== 生产口径：谁引用 price ===  无（0 行）
=== 测试口径：谁引用 price ===  doc_shape_tests.rs:21、handler.rs:3952、handler.rs:4013
```

结论三条（都影响判断，必须分开说）：

1. **生产上 `price` 是纯下游叶子**：12 行出边 / 0 行入边，既不被 `purchase` 也不被 `handler` 的生产代码引用。
2. **它的 SCC 成员资格完全由测试边造成**：`handler.rs` 的标准售价闭环用例 + `doc_shape_tests.rs` 的序列化契约用例。
3. **前 8 张卡用的「SCC 51 ⇒ 双向咬合 ⇒ 不可能拆」对 `price` 是错的论证**。`budget`/`contract`/`asset`/`expense`
   的生产入边确实存在（20 / 7 / 17 / 2 行），`price` 是 **0**。所以本卡改换 §3 的论证，并在 §6 把这条口径差异交给工具维护者。

> 复现口径（不依赖上面的脚本）：`crate_split_readiness.py --json` 给的是含测试边的图；要剔除测试边，
> 判据是「该行是否落在 `#[cfg(test)]` 门控的 `mod`/`fn` 里，或该文件是否 `*_tests.rs` / `#[cfg(test)] mod x;` 声明的模块」。
> 本卡一次性脚本 `price_prod_edges2.py`（130 行，含括号深度跟踪）留在工作区、未入仓 ——
> 若后续 10 张卡都要这个口径，应由工具侧加 `--exclude-tests` 开关统一实现（§6 建议 2）。

### 2.6 进链接单元靠什么（15 条路由 / 14 个权限码）

`app/src/business/erp/mod.rs:173 pub mod price;` + 三个 handler 里 **15 条路由**
（**30 条 `#[nik::*]` 属性 = 15 `#[nik::permission]` + 15 路由宏**，`grep -c "#\[nik::"` 得 30 是双计数），
路由自注册、无手写 `routes()` 汇编。逐条抽出并校验了 `#[nik::permission]` 里的 `path` / `method` 与路由宏**全部一致**（15/15）：

| 方法 | 路径（省略 `/api` 前缀） | 权限码 | 位置 |
|:--|:--|:--|:--|
| GET | `/business/erp/price/agreed/page` | `business:erp:cust-price:list` | price_admin_handler.rs:1075 |
| POST | `/business/erp/price/agreed` | `business:erp:cust-price:create` | :1086 |
| PUT | `/business/erp/price/agreed/{id}` | `business:erp:cust-price:update` | :1101 |
| DELETE | `/business/erp/price/agreed/{id}` | `business:erp:cust-price:delete` | :1117 |
| GET | `/business/erp/price/tier/page` | `business:erp:price-tier:list` | :1133 |
| POST | `/business/erp/price/tier` | `business:erp:price-tier:create` | :1144 |
| PUT | `/business/erp/price/tier/{id}` | `business:erp:price-tier:update` | :1159 |
| DELETE | `/business/erp/price/tier/{id}` | `business:erp:price-tier:delete` | :1175 |
| GET | `/business/erp/price/resolve` | `business:erp:price:resolve` | price_handler.rs:150 |
| POST | `/business/erp/price-inquiry` | `business:erp:price-inquiry:create` | price_inquiry_handler.rs:199 |
| GET | `/business/erp/price-inquiry/page` | `business:erp:price-inquiry:list` | :236 |
| GET | `/business/erp/price-inquiry/{id}` | `business:erp:price-inquiry:list` | :356 |
| POST | `/business/erp/price-inquiry/{id}/quote` | `business:erp:price-inquiry:quote` | :452 |
| POST | `/business/erp/price-inquiry/{id}/award` | `business:erp:price-inquiry:award` | :586 |
| POST | `/business/erp/price-inquiry/{id}/to-po` | `business:erp:price-inquiry:to-po` | :790 |

= **15 条路由 / 14 个权限码**（`price-inquiry:list` 承载 2 条）。权限码种子在迁移里（与 crate 形态无关）：

```
V0307__price_resolve_permission   → business:erp:price:resolve
V0308__customer_price_menu        → business:erp:cust-price:{list,create,update,delete}
                                    business:erp:price-tier:{list,create,update,delete}
V0321__price_inquiry              → business:erp:price-inquiry:{list,create,quote,award,to-po}
V0368__receipt_refund_page_and_price_page → 页面行 / page 码
```

前端 7 个文件引用这批 **HTTP 路径**（`api/business/erp/{price,customer-price,price-inquiry}.ts`、
`router/index.ts`、`views/business/erp/{price,price-inquiry}/*`）——抽 crate 不改路径，所以前端零改动。

`price/` 下**没有** `include_str!` / `include_bytes!` / `#[path = …]`（与 `expense`、`contract` 不同，本域 0 处），
也**没有** `super::super::` 相对路径（8 处 `super::` 全部是文件内 `mod tests` 的 `use super::*;`，抽包后仍成立）。

## 3. 为什么仍然不作废成包

前提已换：`price` 不是双向咬合，是**生产叶子**（§2.5）。所以下面四条是新的、且逐条可复核：

1. **C13 之前抽 = 包级环（与叶子身份无关）**。12 行生产出边指向 `voucher` / `purchase` / `model_base` /
   `doc_shape` / `model` / `tax_rate` / `unit_guard` —— 这 7 个目标在 C13 之前**没有 crate 形态**，
   只能 `use cloudpivot`（应用单体）。同时 15 条路由靠 `#[nik::get]` 在**最终链接单元**收集（AGENTS/§P2-C.4 第 6 条），
   装配层必须依赖 `cloudpivot-erp-price` 才能让路由不静默 404。两边同时成立 ⇒
   `cloudpivot → cloudpivot-erp-price → cloudpivot`，`cargo check` 直接
   `error: cyclic package dependency`。这正是 §P2-C.5 已论证的「假拆分」，也是 C1（内核）+ C2（bin）排在所有域卡之前的唯一理由：
   **叶子也需要先有内核和装配层**。
2. **C13 之后抽 = 合法但只切 2.35%**。C13 落地后 12 行出边变成 `cloudpivot-erp-price → cloudpivot-erp` 的向下箭头，能编过。
   但包边界对增量编译的收益来自「内核 / 装配层 / `erp`」这一级（`erp` 占全仓约 50%），
   把 `erp` 再切成 18 块每块 2~3% 并不改变「改任何一个 erp 子域都要重编 `erp`」的事实；
   且与 §P2-C.6 第 292–295 行的决定（**`erp` 一个包，包内继续用目录分层**）直接冲突。
3. **三条具体代价（都比前几张卡轻，但都不是 0）**：
   - **`#[cfg(test)] pub(crate)` 夹具跨包不可见 ⇒ 5 个真库用例退化成静默 SKIP（假绿）**。
     本域的 5 个真库用例（`tier_overlap_rejected_and_adjacent_allowed`、`tier_overlap_scoped_to_material_customer_type_period`、
     `tier_update_cannot_create_overlap_and_excludes_self`、`inactive_tier_may_overlap_but_reenable_is_blocked`、
     `adjacent_periods_share_boundary_day`，全在 `price_admin_handler.rs`）都通过
     `use crate::business::erp::voucher::voucher_query::tests::{DB_LOCK, ensure_db};` 拿夹具，
     而那是 `voucher/voucher_query.rs:890-891` 的 `#[cfg(test)] pub(crate) mod tests` ——
     **换 crate 后该符号根本不存在**（不是可见性问题，是 `cfg(test)` 不参与依赖包编译），
     5 个用例会编译失败；若图省事改成「拿不到就 return」，即变成 §P2-C.5 点 3 描述的假绿。
     必须走 C1 的 `test-support` feature 方案，或把 `DB_LOCK/ensure_db` 提到 `business/shared` 的测试支持里。
   - **反向 dev-dependency**：`erp` 自己的 3 行测试引用（`handler.rs:3952`、`handler.rs:4013`、`doc_shape_tests.rs:21`）
     会要求 `cloudpivot-erp` 的 `[dev-dependencies]` 依赖 `cloudpivot-erp-price`（Cargo 允许 dev-dep 成环，
     但那是本仓第一次出现「子包 ↔ 父包」的 dev 环，会给后续 17 张同类卡开一个新范式）。
   - **收口面**：`erp/mod.rs:173 pub mod price;`、3 行模块文档表（89–91）、路由条数门禁与
     `erp::{fixture_hygiene, …}` 一类的花括号导入都要重接线；前端与迁移不用动（路径不变）。
4. **C13 整包搬迁时 `price` 的代价 = 0**（这是「作废」的正面理由，不只是「做不了」）：
   12 行生产出边 + 3 行测试入边 + 2 行注释 + 3 行相对文档表**全在包内，路径一行不改**；
   12 行内核引用不变；15 条路由随三个 handler 文件平移即自注册；5 个真库用例的
   `#[cfg(test)] pub(crate)` 夹具仍在同一 crate 内，**不需要 feature 化**。
   C13 是**唯一**允许动 `erp` 编码结构的一步（§P2-C.4 施工单第 2~4 条），届时 `price` 目录保持 `business/erp/price/` 分层即可。

## 4. 两条卡面命令的真实返回（说明「失败」是刻意的）

```bash
$ cd /opt/cloudpivot/backend && cargo build --workspace --offline ; echo build_exit=$?
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.14s
build_exit=0

$ cd /opt/cloudpivot/backend && cargo test -p cloudpivot-erp-price --no-run ; echo stub_exit=$?
error: package ID specification `cloudpivot-erp-price` did not match any packages
stub_exit=101
```

第二条失败 = **包里没有这个 crate**（本卡就是要不建它），不是构建回归；工作区仍是 P2-40 落地时的 2 个包：

```bash
$ cd /opt/cloudpivot/backend && cargo metadata --no-deps --format-version 1 | grep -o '"workspace_members":\[[^]]*\]'
"workspace_members":["path+file:///opt/cloudpivot/backend/app#cloudpivot@0.1.0",
                     "path+file:///opt/cloudpivot/backend/crates/cloudpivot-doc#0.1.0"]
$ ls crates/            # cloudpivot-doc（唯一已抽出的域 crate）
```

卡序仿真未受影响（`--plan-order` 里**没有 `price` 节点**：`erp` 的子域不单独成步）：

```bash
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
C13    cloudpivot-erp                               12    12   OK        切向：task(8), business/task(4)
C14    cloudpivot-task                               0     0   OK
C15    收尾小域（project/taskflow/dashboard）            0     0   OK
全序累计要切：66 行
卡序覆盖 28 个节点；未进卡序（留在单体）2 个：app#root, business#root
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

`cargo fmt --check` 为 0（零 Rust 变更 ⇒ 无格式面）。

### 4.1 基线复核：`price` 现有 96 个单测在真库上全绿（「不搬」不是把坏账留在原地）

99 上跑、PG 连 `172.18.10.145:5432`（99 本机 PG 已卸），库 `cp_p250_price` 由模板 `cloudpivot_test` 克隆（182 表），**用完即 DROP**：

```bash
$ PGPASSWORD=*** createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_p250_price
createdb exit=0
$ cd /opt/cloudpivot/backend && ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p250_price" \
    cargo test -p cloudpivot --lib business::erp::price -- --test-threads=1
running 96 tests
...
test result: ok. 96 passed; 0 failed; 0 ignored; 0 measured; 5585 filtered out; finished in 7.56s
```

**「真库真的连上了」的对照实验**（不做这一步，5 个真库用例静默 `return` 也会报绿 —— 本仓明确反对这种假绿）：

```bash
# ① 设了 ERP_DB_URL：无 SKIP 行，7 个 tier 用例全绿（耗时 5.26s，含建表/清场）
$ ERP_DB_URL=… cp_p250_price cargo test -p cloudpivot --lib …::price_admin_handler::tests::tier_ \
    -- --test-threads=1 --nocapture
running 7 tests ... test result: ok. 7 passed; 0 failed; …; finished in 5.26s
#                                                   ↑ 输出里没有任何 "SKIP:"

# ② 不设 ERP_DB_URL（env -u）：同一个用例直接打印跳过原因 —— 证明开关有效、① 确实连了库
$ env -u ERP_DB_URL cargo test -p cloudpivot --lib …::tests::tier_overlap_rejected -- --nocapture
test … tier_overlap_rejected_and_adjacent_allowed ... SKIP: 未设置 ERP_DB_URL（不连真实数据库）
test result: ok. 1 passed; 0 failed; …; finished in 0.00s
```

夹具落库后由用例自身 `tier_cleanup()` 清场（`price_admin_handler.rs:1692/1734/…`），所以跑完查
`biz_material/biz_customer/biz_price_tier` 的 `E2E08T-%` 行数为 0 属正常，不是「没跑库」。

## 5. 复核命令（只读，可对任意机器跑）

```bash
cd /opt/cloudpivot
python3 scripts/verify/crate_split_readiness.py --domain price --scope erp-sub                # 9 文件/6 970 行，SCC 51，入边 3/出边 15（含测试口径）
python3 scripts/verify/crate_split_readiness.py --domain price --scope erp-sub --layer-check  # 退 1：本步要切 7 条/15 行
python3 scripts/verify/crate_split_readiness.py --domain price --scope erp-sub --check        # 退 1（旧口径，只看入边）
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md  # 退 0：15 步 66 行，绿

grep -rn "erp::price\b" --include=*.rs backend/app/src | grep -v "business/erp/price/"          # 4 行：3 测试 + 1 注释
grep -rn "erp::price\b" --include=*.rs backend/app/src/business/erp/handler.rs                 # 3952/4013（都在 mod tests 内）
grep -n -B2 "mod doc_shape_tests" backend/app/src/business/erp/mod.rs                          # 142-143 = #[cfg(test)] mod
grep -rn "crate::business::" backend/app/src/business/erp/price | grep -v "crate::business::erp::price" | wc -l  # 20 = 出边 17 + 内核 business::shared 3
grep -rn "crate::shared::\|crate::utils" backend/app/src/business/erp/price | wc -l            # 9（内核余下：error_code 5 + utils 4）
grep -rn "include_str!\|include_bytes!\|#\[path" backend/app/src/business/erp/price            # 0 行（本域无源码路径硬耦合）
grep -rn "super::super::" backend/app/src/business/erp/price                                   # 0 行
grep -rnE "#\[nik::(get|post|put|delete|patch)" backend/app/src/business/erp/price | wc -l     # 15（路由宏）
grep -rc "#\[nik::permission" backend/app/src/business/erp/price/*.rs | grep -v ':0'            # 15（权限属性；与路由 1:1）
grep -rn "voucher::voucher_query::tests::{DB_LOCK, ensure_db}" backend/app/src/business/erp/price # 1 行：跨模块 cfg(test) 夹具
cd backend && cargo build --workspace --offline ; echo $?                                        # exit 0
cd backend && cargo test -p cloudpivot-erp-price --no-run ; echo $?                              # exit 101（package 不存在 = 预期）
# 基线绿（可选，需真库）
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_p250_price
cd backend && ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p250_price" \
  cargo test -p cloudpivot --lib business::erp::price -- --test-threads=1                        # 96 passed / 0 failed
dropdb -h 172.18.10.145 -U cloudpivot cp_p250_price
```

## 6. 保留物 / 后继

- **保留**：`scripts/verify/crate_split_readiness.py`（`--domain` / `--layer-check` / `--plan-order`）、
  `docs/planning/20261113-新卡清单.md` §P2-C、本文（单卡实测，再派发时不必重扫）。
- **后继**：`price`（15 条路由 / 14 个权限码 / 96 个单测，其中 5 个真库用例）随 **C13 `cloudpivot-erp`** 整包搬迁，
  包内保持 `business/erp/price/` 目录分层；17 行出边（含 2 行注释）、3 行测试入边、3 行相对文档表、
  12 行内核引用、`mod.rs:173` 的模块声明、15 条路由、5 个真库用例夹具**一行都不用改**。
- **给 C13 施工单的建议（本域特有，前几张卡没有）**：
  `price` 的 5 个真库用例是**唯一**把「另一个模块的 `#[cfg(test)]` 夹具」当依赖用的地方
  （`voucher::voucher_query::tests::{DB_LOCK, ensure_db}`，指向 `voucher/voucher_query.rs:890-891`）。
  C13 整包搬迁时它天然成立；但**若**将来把 `voucher` 或 `price` 再拆出去，这 1 行必须先改成
  `business/shared` 的测试支持（与 C1 的 `test-support` feature 合并），否则 5 个用例编译失败或退化为静默 SKIP。
  建议 C13 落地时在 `docs/` 里一次性点名「跨模块 `#[cfg(test)]` 夹具」完整清单，避免逐卡重复发现。
- **给工具的口径建议（本卡实测，影响前 8 张卡的论证措辞，不改结论）**：
  1. `crate_split_readiness.py` 把 `#[cfg(test)]` 块内的引用算作 `code`（只排除 `//` 注释行），
     于是「SCC 51」与「生产 SCC 29」是两个数。本卡实测：**`price` 的 SCC 成员资格完全由 3 行测试入边造成**，
     按 51 结论会得出「双向咬合」这一**错误**理由。建议加 `--exclude-tests`（判据见 §2.5 引述），
     并把 `--domain` 输出拆成「生产 / 测试」两列；`P2-42` §4 台账的 SCC 列应标注是哪个口径。
  2. 同一处还应排除「整文件测试模块」（`*_tests.rs` / `*_test_support.rs` / `#[cfg(test)] mod x;` 声明的文件）——
     `erp` 下有 **121 个**，它们的边现在全部计入生产边图。
  3. 本域**未命中** P2-48/P2-49 报的两条既有缺陷（无 `erp::{…}` 花括号导入、无 `attachment/notice` 类跨域域），
     所以 `price` 的脚本口径数字（15 行 / 7 目标）与逐行复核一致，**不需要修正**。
- **本卡零生产代码改动**：无 Rust 文件变更 ⇒ `cargo fmt` 无格式面（`--check` exit 0）；
  `git status` 下唯一变化是本文（`.md`）。`price` 现有 96 个单测（含 5 个真库用例）全绿（§4.1），
  即「作废」不是把坏账留在原地。
