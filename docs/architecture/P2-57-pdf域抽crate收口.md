# P2-57 PDF 域抽 crate 收口 —— 作废（`pdf` 并入 C5 `cloudpivot-pdf`）

> 卡：`t_a6fada9f`（P2-57【P2-C】抽 `pdf` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `74dde8b4`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41`（`11f490f2`）、`P2-42`（`e9a31e96`）、`P2-43`（`939ec719`）、`P2-44`（`810ce2c7`）、
> `P2-45`（`5a2bc55a`）、`P2-46`（`ca11c5dc`）、`P2-47`（`4648b330`）、`P2-48`（`6a473699`）、
> `P2-49`（`c1f7aa76`）、`P2-50`（`4b95fad3`）、`P2-51`（`0cc8554c`）、`P2-52`（`088c0586`）、
> `P2-53`（`17c41945`）、`P2-54`（`3e22d3fe`）、`P2-55`（`632ad268`）、`P2-56`（`74dde8b4`）
> 数据库：**本卡不需要库**（`pdf` 零零 DB 入口，见 §7）· 演练库 0 个

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-pdf/`，不搬 5 个 `.rs` / 6 243 行 + 11 个 `.tera` / 3 236 行，
不改 2 行 `crate::shared` 出边与 28 行入边 import（§3 / §4），不动 `backend/app/Cargo.toml`、
不动 `backend/Cargo.toml`、不动 `backend/crates/`（`ls backend/crates/` 仍只有 `cloudpivot-doc`）。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；卡面验收命令因此按预期
失败（不是回归）：

```bash
$ cd backend && cargo test -p cloudpivot-pdf --offline -- --test-threads=1
error: package ID specification `cloudpivot-pdf` did not match any packages

help: a package with a similar name exists: `cloudpivot-doc`
STUB_EXIT=101
```

```bash
$ cd backend && cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.11s
BUILD_EXIT=0
```

权威依据已入仓：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 288 行：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同文件 §P2-C.3 第 218 行的 **C5** 行：`cloudpivot-notice`、`-pdf`、`-docpdf`、`-monitor`｜节点
  `monitor`、`notice`、`pdf`、`docpdf`｜前置 **C1、C3**｜回边 **0 / 74 / 28 / 14**｜
  **本步要切 3 行：task 3（卡内先后：docpdf→pdf 3 行）**｜机械量 `24 / 17 825 · 22 · 0 · 2 · 0`；
- `P2-42` §4 台账（第 95 行）P2-57 行 = `5 / 6 243`｜SCC 24｜入边 `28（4）`｜出边目标 1（→内核 `shared`）｜
  `0 行`｜**`退 0`**｜落点 **`C5 cloudpivot-pdf`**；
- 本卡 2026-11-15 11:09 的改派留言（并入 C5 `cloudpivot-pdf`，与 `notice`/`docpdf`/`monitor` 同卡）。

### 1.1 与前 16 张作废卡的不同：本卡是 C5 里**唯一** `--layer-check` 退 0 的一步

`P2-42` §4 的要点 1 写得很直白：「17 张里 16 张 `--layer-check` 退 1，`pdf` 是唯一退 0 的」。
本卡复核**一致**（§2.1，`exit=0`）——`pdf` 的出边只剩内核 `shared` 2 行。

但**「可抽」不等于「现在抽」**，作废的理由有两层，两层都不是可行性：

1. **时序**：C5 前置 C1（`cloudpivot-kernel`，含 `shared`）、C3（`cloudpivot-support`）。二者未落地时
   把 `pdf` 抽成 crate，它那 2 行 `crate::shared::error_code` 只能改成对单体 `cloudpivot` 的 path 依赖，
   而单体又要依赖 `cloudpivot-pdf` 才能链接路由 ⇒ **反向依赖的空壳包**，正是 §P2-C.5 明确拒绝的假拆分。
2. **批量**：C5 是「四包一张卡」，一次改 bin 依赖、一次核对路由条数（22）比开四次工省。
   而且 `pdf` 在卡内**必须第一个搬**——`docpdf` 有 3 行指向 `pdf`（§8）。

⇒ 本卡与「C5 的 pdf 半步」是同一件事，作废的实质是**并入 C5 的卡序**，不是目标不成立。

## 2. `pdf` 的实测（只读、无库依赖，2026-11-15 于 99）

### 2.1 工具原始输出（全文 —— `pdf` 只有 5 个文件，输出不长，不摘录）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain pdf --layer-check
域：pdf（scope=top）
  规模：5 个文件 / 6243 行
  SCC：24 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（23）：aiagent, announcement, attachment, business/shared, business/task,
    changelog, docpdf, erp, flow, help, hr, impexp, masking, middleware, mutex, notice,
    permission, shared, storage, support, system, task, utils
  回边（留在应用 crate 的代码 → 本域）：28 处代码级
    business/task          code=  2 all=  2  business/task/task_pdf.rs:64, business/task/task_pdf.rs:65
    docpdf                 code=  3 all=  5  business/docpdf/doc_pdf_render.rs:20, …:21, …:64
    erp                    code= 21 all= 24  business/erp/contract/contract_pdf.rs:44, …:48, …:337, …:397, …:567, business/erp/order_pdf.rs:52
    flow                   code=  2 all=  2  business/flow/instance_pdf.rs:59, business/flow/instance_pdf.rs:60
  出边（本域 → 其它）：1 个目标
    shared                 code=  2 all=  2
  层次判据：可抽（出边全部落在更下层内核 shared, utils, business/shared）
  结论：不可抽（28 处代码级回边，4 个来源）
exit=0
```

```bash
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
C1     cloudpivot-kernel                            10    10   OK
C3     cloudpivot-support                            6     6   OK
C5     cloudpivot-notice, cloudpivot-pdf, cloudpivot-docpdf        3     3   OK
                                              切向：task(3)
                                              卡内跨包先后：docpdf→pdf(3)
…（C6 ~ C15 全 OK）
全序累计要切：66 行
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

**三处口径互相印证**：`--layer-check` 退 0（出边全在 L1 内核）、`--plan-order` 的 C5 行 3 行
全部记在 `task`（`pdf` 自己 **0 行**）、`P2-42` §4 台账 P2-57 行「出边目标 1（→内核 `shared`）/ 0 行 / 退 0」。

```bash
# 顺带复核 C5 另外两个包，确认 C5 行的 0/74/28/14 与节点顺序一一对应（pdf = 28）
$ python3 scripts/verify/crate_split_readiness.py --domain docpdf --layer-check   # 14 处回边 / 3 源；要切 pdf(3) → 退 1
$ python3 scripts/verify/crate_split_readiness.py --domain monitor --layer-check  # 0 处回边 / SCC=1；要切 support(2) → 退 1
```

### 2.2 工具的「回边」为什么只有 4 个来源 —— 内核那 1 行注释被静默吃掉

`src/shared/error_code.rs:1885` 有一行 rustdoc 链接指向本域：

```
/// [`crate::business::pdf::dto::MAX_RENDER_HTML_BYTES`]（4MiB）是硬上限：
```

它匹配工具的 `crate::business::pdf\b` 正则，但工具把「本行 `lstrip().startswith("//")`」判为注释，
**只进 `edges`（all）、不进 `code`**，而回边清单 `inbound()` 是从 `code` 里生成的 ⇒
`shared` 这个来源**在输出里根本不出现**（`shared` 的 code 计数为 0，故不入列）。

本卡用独立脚本逐行复核（不依赖上面的输出）：命中 `crate::business::pdf` / `business/pdf` 的
**全部 36 行**里，代码 28 行（与工具一致）、注释 8 行（工具只报 5 行，差 3 行是下面两类）。

> **结论**：`shared → pdf` **代码级 0 行**（不成环，§3 的 2 行是反向的 `pdf → shared`），
> 但那 1 行链接搬包后会指向不存在的模块 ⇒ 随 C5 改成 `cloudpivot_pdf::dto::MAX_RENDER_HTML_BYTES`。

## 3. 出边：2 行代码，**全部落内核**，本步要切 **0 行**

| # | 文件:行 | 行内容 | 抽出后指向 |
|--:|:--|:--|:--|
| 1 | `business/pdf/dto.rs:21` | `use crate::shared::error_code;` | C1 `cloudpivot-kernel` |
| 2 | `business/pdf/handler.rs:35` | `use crate::shared::error_code;` | C1 `cloudpivot-kernel` |

合计：`shared` **2 行** ⇒ 出边目标 1 个、**0 行需要在本步切**（C5 的 3 行账全部属于 `notice → task`，
见 P2-56 §3，与本卡无关）。

`pdf` 与内核的唯一耦合就是错误码常量，且**错误码本体在 `shared/error_code.rs`（内核侧）**：

| 常量 | 值 | 定义处 |
|:--|--:|:--|
| `PDF_RENDER_UNAVAILABLE` | 67001 | `shared/error_code.rs:1863` |
| `PDF_RENDER_TIMEOUT` | 67002 | `:1869` |
| `PDF_TEMPLATE_INVALID` | 67003 | `:1875` |
| `PDF_OPTIONS_INVALID` | 67004 | `:1881` |
| `PDF_RENDER_TOO_LARGE` | 67005 | `:1887` |
| （另有 `INTERNAL_ERROR` 复用） | — | 同上 |

即：**错误码段属于内核，`pdf` 只是使用方** ⇒ 搬包后是合法的向下箭头，不需要「错误码上提」。

### 3.1 本域没有 `crate::chrono` / `crate::run` 这类编译级改造面

`grep 'crate::'` 在 `business/pdf/**` 里除去 `crate::business::pdf` 之外**只有上表 2 行**（含注释 0 行）。
对比 notice（5 行 `crate::chrono` + 1 行 `crate::run` 文档链接），`pdf` 搬包**不需要**改
`nik::prelude::chrono` 之类的路径；`mod.rs` 的模块文档里也没有跨包 intra-doc 链接。

## 4. 入边：28 行代码（prod **22** / test **6**）+ 注释 8 行

工具口径 33 行（28 代码 + 5 注释），本卡逐行复核 36 行（多出 3 行：1 行内核 rustdoc 链接 +
2 行反引号散文 `` `business/pdf` ``，见下表「注释」列打 ✎ 的两行）。

### 4.1 代码 28 行（逐行）

| # | 文件:行 | prod/test | 行内容（摘） |
|--:|:--|:--:|:--|
| 1 | `business/task/task_pdf.rs:64` | prod | `use crate::business::pdf::handler as pdf_handler;` |
| 2 | `business/task/task_pdf.rs:65` | prod | `use crate::business::pdf::template::{…};` |
| 3 | `business/docpdf/doc_pdf_render.rs:20` | prod | `use crate::business::pdf::PdfOpts;` |
| 4 | `business/docpdf/doc_pdf_render.rs:21` | prod | `use crate::business::pdf::engine;` |
| 5 | `business/docpdf/doc_pdf_render.rs:64` | prod | `) -> std::result::Result<RenderedDoc, crate::business::pdf::PdfError> {` |
| 6 | `business/flow/instance_pdf.rs:59` | prod | `use crate::business::pdf::handler as pdf_handler;` |
| 7 | `business/flow/instance_pdf.rs:60` | prod | `use crate::business::pdf::template::{…};` |
| 8 | `business/erp/contract/contract_pdf.rs:44` | prod | `use crate::business::pdf::template::{…};` |
| 9 | `business/erp/contract/contract_pdf.rs:48` | prod | `use crate::business::pdf::{engine, handler as pdf_handler};` |
| 10 | `business/erp/contract/contract_pdf.rs:337` | **test** | `use crate::business::pdf::template::sample_contract;` |
| 11 | `business/erp/contract/contract_pdf.rs:397` | **test** | `use crate::business::pdf::template::PAYMENT_TERMS_EMPTY;` |
| 12 | `business/erp/contract/contract_pdf.rs:567` | **test** | `… HashMap<&str, &crate::business::pdf::template::ContractAttachment> = ctx` |
| 13 | `business/erp/order_pdf.rs:52` | prod | `use crate::business::pdf::template::{…};` |
| 14 | `business/erp/order_pdf.rs:56` | prod | `use crate::business::pdf::{engine, handler as pdf_handler};` |
| 15 | `business/erp/order_pdf_model.rs:28` | prod | `use crate::business::pdf::template::OrderDocKind;` |
| 16 | `business/erp/report/report_pdf.rs:77` | prod | `use crate::business::pdf::handler as pdf_handler;` |
| 17 | `business/erp/report/report_pdf.rs:78` | prod | `use crate::business::pdf::template::{…};` |
| 18 | `business/erp/return_pdf.rs:53` | prod | `use crate::business::pdf::handler as pdf_handler;` |
| 19 | `business/erp/return_pdf.rs:54` | prod | `use crate::business::pdf::template::{…};` |
| 20 | `business/erp/stock/stock_pdf.rs:50` | prod | `use crate::business::pdf::template::{…};` |
| 21 | `business/erp/stock/stock_pdf.rs:54` | prod | `use crate::business::pdf::{engine, handler as pdf_handler};` |
| 22 | `business/erp/stock/stock_pdf.rs:686` | **test** | `use crate::business::pdf::template::sample_stock;` |
| 23 | `business/erp/stock/stock_pdf.rs:1366` | **test** | `export_operation(crate::business::pdf::template::StockDocKind::StockIn),` |
| 24 | `business/erp/stock/stock_pdf.rs:1370` | **test** | `export_operation(crate::business::pdf::template::StockDocKind::StockOut),` |
| 25 | `business/erp/stock/stock_take_pdf.rs:47` | prod | `use crate::business::pdf::handler as pdf_handler;` |
| 26 | `business/erp/stock/stock_take_pdf.rs:48` | prod | `use crate::business::pdf::template::{…};` |
| 27 | `business/erp/transfer/transfer_pdf.rs:47` | prod | `use crate::business::pdf::handler as pdf_handler;` |
| 28 | `business/erp/transfer/transfer_pdf.rs:48` | prod | `use crate::business::pdf::template::{…};` |

### 4.2 注释 8 行（工具报 5 行，本卡复核 8 行）

| # | 文件:行 | 工具计数 | 说明 |
|--:|:--|:--:|:--|
| 1 | `shared/error_code.rs:1885` | ✗（`all` 计 1，但 `code`=0 故不入回边清单） | 内核侧 rustdoc 链接，§2.2 |
| 2 | `business/docpdf/doc_pdf_model.rs:7` | ✎ 正则不匹配（散文 `` `business/pdf` ``） | 描述性提及 |
| 3 | `business/docpdf/doc_pdf_render.rs:56` | ✓ | `/// 原样透传 [`crate::business::pdf::dto::PdfError`]…` |
| 4 | `business/docpdf/doc_pdf_render.rs:57` | ✓ | `/// 由调用方 [`crate::business::pdf::dto::PdfError::into_api`] 转 HTTP。` |
| 5 | `business/erp/order_pdf.rs:20` | ✓ | `//! （中文走 RFC 5987，见 [`crate::business::pdf::handler::content_disposition`]）；` |
| 6 | `business/erp/order_pdf.rs:36` | ✓ | `//! 全部在 [`crate::business::pdf::template`] 里…` |
| 7 | `business/erp/order_pdf_model.rs:8` | ✎ 正则不匹配（散文 `` `business/pdf` ``） | 描述性提及 |
| 8 | `business/erp/stock/stock_pdf.rs:29` | ✓ | `//! [`crate::business::pdf::template`]，与对账单 / 订单 PDF 同一套实现。` |

**改法唯一**：`crate::business::pdf::` → `cloudpivot_pdf::`（28 行代码 + 6 行 rustdoc 链接）；
2 行反引号散文改成 `` `cloudpivot_pdf` `` 即可（不改也能编译）。

### 4.3 四个来源的归属：谁都不用付账

| 来源 | 代码行 | 归属卡 | 与 C5 的相对位置 | 需不需要付账 |
|:--|--:|:--:|:--|:--|
| `erp` | 21 | C13 `cloudpivot-erp` | C5 **之后** | 不需要（合法的向下箭头） |
| `business/task` | 2 | C14 `cloudpivot-task` | C5 **之后** | 不需要 |
| `flow` | 2 | C12 `cloudpivot-flow` | C5 **之后** | 不需要 |
| `docpdf` | 3 | **C5 同卡** | 同卡内 | 卡内改指向（`cloudpivot_pdf::`），零额外账 |
| **合计** | **28** | — | — | **0 行外部欠账** |

### 4.4 测试入边只需公开 API，不需要 `test-support` feature

- 6 行测试引用（#10/#11/#12/#22/#23/#24）全是普通 `use` + 调用公开项
  （`sample_contract` / `PAYMENT_TERMS_EMPTY` / `ContractAttachment` / `sample_stock` / `StockDocKind`），
  **都是 `pub`**（`template.rs` 对外暴露的样本构造器）⇒ 不需要 `#[cfg(test)]` 跨包可见性技巧。
- `grep 'test_support\|test_helpers\|fixture_hygiene' business/pdf` = **0**；`pdf` **零夹具引用**，
  与 notice/attachment 同口径。
- `pdf` 自己也没有跨包夹具需求：4 个 `#[cfg(test)]` 模块全在本目录内，86 个用例自给自足（§7）。

### 4.5 前端 0 引用 —— 3 个接口是运维/联调口，不是页面 API

```bash
$ grep -rn 'business/pdf' frontend/src
（0 行）
```

业务侧的「导出 PDF」按钮走的是**各自单据的导出接口**（`erp/order_pdf.rs` 等，服务端调
`pdf_handler` / `engine`），不直连 `/business/pdf/*`。所以 `pdf` 抽包**不影响前端**；
`scripts/` 里两条命中都是 URL（`e2e12_a.py` 探活、`install-pdf-infra.sh` 提示），不是源码路径
（§6 #5）。

## 5. 机械量（与 C5 行 `24 / 17 825 · 22 · 0 · 2 · 0` 对账）

| 项 | `pdf` 实测 | C5 合计（notice+pdf+docpdf+monitor） | 说明 |
|:--|:--|:--|:--|
| `.rs` 文件 / 行 | **5 / 6 243** | 24 / 17 825 ✔（notice 11/8 224 + **pdf 5/6 243** + docpdf 3/631 + monitor 5/2 727） | `dto 665 / engine 555 / handler 447 / mod 46 / template 4 530`（全平铺，无子目录） |
| `.tera` 模板文件 / 行 | **11 / 3 236** | **计划表未计**（见 §5.1 口径差 ①） | `pdf/templates/*.html.tera`：statement / order / contract / stock / transfer / stock_take / return_doc / approval / task / report_balance / report_amount |
| HTTP 端点 | **3** | 22 = notice 17 + **pdf 3** + docpdf 0 + monitor 2 | `GET /business/pdf/health`、`POST /business/pdf/sample`、`POST /business/pdf/render`（`handler.rs:53/93/123`） |
| 权限码 | **1** 个（3 处 `#[nik::permission]`） | — | `business:pdf:render:export` ×3（**三条路由全挂码，0 条裸路由**；连 `health` 探活也要本码） |
| 免认证 | **0**（`auth_only` 0 / `no_auth` 0） | — | 与 notice 的 `/internal/alert` 不同，`pdf` 无豁免路由、无手写路由 |
| 菜单行 | **1** | — | `V0343` 1 行：`menu_type=3` 按钮型，`id …f001`，`title=PDF 导出`，`parent=10000000-…-0050`（**工作台**页面），`sort=60`，`is_visible=FALSE`，`module_code=business` |
| 表 | **0** | — | `pdf` 不读库不写库：`grep 'db()\|ensure_db\|Database::connect' business/pdf` = 0 |
| 迁移 | **1 个版本 / 2 文件** | — | `business/V0343__pdf_export_permission.{up,down}.sql`（只有权限点 + 角色授权，无 DDL） |
| `#[cfg(test)]` | **4** | — | `dto:466` / `engine:466` / `handler:276` / `template:3224` |
| 用例 | **86** | — | dto 11 + engine 5 + handler 12 + template 58（`grep -c '#\[test\]\|#\[tokio::test\]'` 合计 86，与真跑一致） |
| 真库用例 | **0** | 0 ✔ | 无 `*_db_tests.rs`，也**无行内真库用例**（0 处 `ensure_db`）⇒ C5 卡面「真库用例 0」对 `pdf` 成立 |
| `include_str!` | **11**（全是 `templates/x.tera`，**同目录内**） | — | 搬包只要 `templates/` 跟过去，路径深度不变，**一行不用改** |
| 出目录引用（`../`） | **0** | 2（**属 monitor**，见 §5.1 口径差 ②） | `business/monitor/handler.rs:175/187` 的 `include_str!("../../../migrations/seed/S__*_data.sql")` |
| 夹具引用 | **0** | 0 ✔ | 无 `test_support` / `test_helpers` / `fixture_hygiene` / `SEED_ROLE_LOCK` |

### 5.1 两处口径差（C5 施工单必须带）

1. **C5 的 `24 / 17 825` 只数 `.rs`，漏了 `pdf` 的 11 个模板**。`pdf` 实际搬 **16 文件 / 9 479 行**；
   C5 四包合计应为 **35 文件 / 21 061 行**（+11 / +3 236）。`include_str!` 在编译期就吃文件，
   **漏搬 `templates/` = 编译直接失败**（11 个 `include_str!` 全是硬错误，不是运行期 404），
   所以这条不是口径洁癖：搬包清单里必须显式写 `crates/cloudpivot-pdf/templates/*.tera`。
2. **C5 的「出目录引用 = 2」不属于 `pdf`/`docpdf`，属于 `monitor`**。`P2-56` §5 写「2（属 pdf/docpdf，见 P2-57）」
   是误归因；本卡实测：`pdf` 的 11 处 `include_str!` **全部同目录**，`docpdf` **0 处**，那 2 处在
   `business/monitor/handler.rs:175` / `:187`（`../../../migrations/seed/`）—— `monitor` 随 C5 搬包时
   这两行的相对深度会变（`crates/cloudpivot-monitor/src/handler.rs` → 要到 `app/migrations/seed/`
   得写 `../../../app/migrations/seed/…`，或改成 `CARGO_MANIFEST_DIR` 拼接）。**这是 C5 里唯一需要
   改相对路径的一处**，容易被漏。

## 6. 搬包会失效的门禁（按源码路径硬编码，本卡只报不动）

| # | 位置 | 现状 | 搬迁后 | 严重度 |
|--:|:--|:--|:--|:--|
| 1 | `system/menu/permission_audit_tests.rs:747` `src_root() = CARGO_MANIFEST_DIR/src` | 只扫 `backend/app/src` | **N14 直接变红**：`business:pdf:render:export` 在 `app/src` 内**外部引用 = 0**（见下），`V0343` 那行菜单判成死码 | 🔴 红 |
| 2 | 同上（`scan_routes`） | 3 条 pdf 宏路由在扫描面内 | 3 条路由**静默逃出** N16（裸路由）/N21（静态路由被模板遮蔽） | 🟠 静默 |
| 3 | `scripts/lint/scope_gate.py` `SKIP_MANIFEST` | **没有** `business/pdf/**` 条目（`grep -n 'business/pdf' scope_gate.py` = 0 行） | 搬走后扫描面少一目录，清单无需改 ⇒ **不响**（静默） | 🟠 静默 |
| 4 | `business/shared/layering_tests.rs:44` `MIN_DOMAINS = 18` | `src/business` 下 **22** 个目录 | 21 ≥ 18 ⇒ **静默变绿**，`pdf` 的跨域引用从此无人扫 | 🟠 静默 |
| 5 | `scripts/lint/path_refs_gate.py`（`$SRC` → `backend/app/src`） | 899 个脚本 0 失效 | 今天无脚本写 `$SRC/business/pdf/…`（两条命中都是 URL） ⇒ **预防性** | 🟡 预防 |
| 6 | `src/shared/error_code.rs:1885` rustdoc 链接 | 内核文档链到 `crate::business::pdf::dto::MAX_RENDER_HTML_BYTES` | 链接失效（`cargo build` 不报，`cargo doc` 告警）⇒ 改 `cloudpivot_pdf::…` | 🟡 文档 |

**唯一那个权限码的「外部引用」实测**（`backend/app/src`，排除 `business/pdf/**`）：

```bash
$ grep -rn 'business:pdf' backend/app/src --include=*.rs | grep -v 'src/business/pdf/'
（0 行）
```

即：`business:pdf:render:export` 的唯一落地就是 `business/pdf/handler.rs` 里那 3 处
`#[nik::permission]`。搬出 `app/src` ⇒ N14 看不到 ⇒ `V0343` 的菜单行判死码。

**建议（并入 C2 扫描根收口，与 P2-54/P2-55/P2-56 同一条）**：`Repo::load` 的扫描根扩成
`app/src + crates/*/src`，`scope_gate.py` 的 `SCAN_DIRS`、`layering_tests` 的目录枚举同源化。
这是**所有域抽包卡的共同前置**，不是 pdf 独有。

## 7. 验证与基线（无库依赖，这是 `pdf` 与 notice 最大的不同）

`pdf` 有 **0** 处 DB 入口（`grep 'db()\|ensure_db\|Database::connect\|ERP_DB_URL' business/pdf` = 0 行），
所以本卡**不建演练库**，也用「假库 URL」反证它真的不碰库：

```bash
# ① 正常（不设任何 DB 环境变量）
$ cd backend && env -u ERP_DB_URL -u HR_DB_URL \
    cargo test -p cloudpivot --lib 'business::pdf' --offline -- --test-threads=1
test result: ok. 86 passed; 0 failed; 0 ignored; 0 measured; 5595 filtered out; finished in 0.09s

# ② 对照：给一个**必然连不上**的库地址（若有用例真连库，这里必然失败）
$ ERP_DB_URL='postgres://nobody@127.0.0.1:1/none' \
    cargo test -p cloudpivot --lib 'business::pdf' --offline -- --test-threads=1
test result: ok. 86 passed; 0 failed; 0 ignored; 0 measured; 5595 filtered out; finished in 0.10s
```

**两次 86/86、耗时同量级（0.09s vs 0.10s）** ⇒ 本域用例对库完全无感知（不是「SKIP 了所以秒过」，
是**根本没有 DB 用例**）。这也说明卡面验收 `cargo test -p cloudpivot-pdf -- --test-threads=1`
搬到 C5 后是**纯离线**可过的，不像 notice 必须带库。

门禁基线（搬迁前，全绿）：

```bash
$ python3 scripts/lint/scope_gate.py
  PASS：扫描范围已覆盖 backend/app/src/{business/task,business,system,support,shared}
  PASS：跳过清单与 SKIP_MANIFEST 逐条一致（160 条；多一个少一个都不放行）
  PASS：0 处越界
  → exit=0
$ python3 scripts/lint/path_refs_gate.py
  结论：PASS（扫描 899 个脚本，白名单 10 条，0 处失效路径）            → exit=0
$ python3 scripts/lint/module_cycle_report.py                          → exit=0
$ cd backend && cargo build --workspace --offline                       → exit=0（Finished dev in 0.11s）
$ python3 scripts/verify/crate_split_readiness.py --domain pdf --layer-check → exit=0（本步要切 0 行）
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md → exit=0（C5 3/3 OK；全序 66 行）
```

`cargo fmt`：本卡**零 Rust 变更**（只新增本文档），无格式面；且 `/opt/cloudpivot` 是多 worker 共享工作区，
跑全仓 `cargo fmt` 会改到别人的 WIP ⇒ 未跑。

## 8. 下一轮怎么开（C5 卡内顺序：**pdf 第一**）

1. **前置**：C1 `cloudpivot-kernel`（`shared` 的 `error_code`）、C3 `cloudpivot-support`。二者未落地前开工
   只会得到反向依赖（当前 `backend/crates/` 只有 `cloudpivot-doc`）。
2. **卡内顺序（本卡更正 P2-56 §8 第 2 条）**：`pdf` → `docpdf` → `notice` → `monitor`。
   - `docpdf` 有 3 行指向 `pdf`（`PdfOpts` / `engine` / `PdfError`）⇒ **`pdf` 必须先成为包**，
     `--plan-order` 的「卡内跨包先后：docpdf→pdf(3)」说的是**依赖方向**，不是抽出顺序；
     `P2-56` 把它当顺序读（`docpdf` → `pdf`）会先撞上「docpdf 反向依赖单体」的环。
   - `pdf` 自己 **0 行要切**，是最省的第一步；`notice` 需切 3 行 `task`；`monitor` 的 2 行 `support`
     由 C3 前置抵付。
3. **`pdf` 新 crate 的依赖面（照 `app/Cargo.toml` 现声明逐字搬，Cargo.lock 仍一份）**：
   `chromiumoxide = "0.9"`、`tera = "1"`、`axum = { version = "0.8", features = ["multipart"] }`、
   `tokio`、`serde`、`futures-util`、`nik`（用到 `nik::prelude::*` 的 `Res/ResResult/ApiError` 与
   `nik::prelude::rust_decimal::{Decimal, RoundingStrategy}` ⇒ feature 点单必须含 `rust_decimal`；
   **不需要** `postgres`/`db`，本域不碰库）。
4. **`templates/` 必须跟着搬**：`crates/cloudpivot-pdf/templates/*.tera`（11 个），`include_str!` 路径
   原样保留（同目录）。
5. **不改运行时配置**：`PDF_CDP_URL` / `PDF_MAX_CONCURRENCY` / `PDF_RENDER_TIMEOUT_MS` 三个环境变量
   不走 `config.toml`（`engine.rs` 模块文档已写明理由：`AppConfig` 无法加自定义段）⇒ 搬包对环境面零影响。
6. **装配层**：`pdf` 在 `lib.rs` 里**没有**注册点（`grep pdf app/src/lib.rs` = 0），路由靠
   `#[nik::get/post]` + `inventory`；`business/mod.rs:43` 的 `pub mod pdf;` 改成 bin 侧
   `pub use cloudpivot_pdf;`（或直接从 bin 依赖它，让 inventory 收集到路由项 —— 漏了会**静默 404**）。
7. **门禁同批改**（否则 C5 一落地就红）：§6 的 #1（N14 扫描根）、#2（N16/N21 路由扫描面）、#4（`MIN_DOMAINS`）；
   #3 是**静默**的，必须靠 #1 同源化兜住；#6 顺手改 1 行文档链接。
8. **卡面机械量修正**：C5 按 **35 文件 / 21 061 行**记（含 11 个 `.tera`），路由 22 不变。

## 附录 A：核对清单（本卡实测 vs 计划表）

| 项 | 计划表（§P2-C.3 C5 行 / P2-42 §4） | 本卡实测 | 结论 |
|:--|:--|:--|:--|
| 文件 / 行 | 5 / 6 243 | 5 / 6 243 `.rs`（+11 模板 / 3 236 行未计） | 一致（`.rs` 口径）；模板漏计见 §5.1 ① |
| SCC | 24 | 24（同环 23 节点） | 一致 |
| 入边 | 28（4 源） | 28 行代码（prod 22 / test 6）+ 8 行注释 | 一致；计划表未区分 prod/test |
| 出边 | 1 目标（→内核 `shared`）/ 0 行 | 2 行代码（全 `shared`）/ 0 行要切 | 一致 |
| 本步要切 | 0 行 | 0 行（`--layer-check` 退 0、`--plan-order` C5 的 3 行全在 `task`） | 一致 |
| 路由 | C5 22（四包合计） | `pdf` 3 → 17+3+0+2 = 22 | 一致 |
| 权限码 | — | `pdf` 1 个 / 3 处注解 / 0 裸路由 | 补充 |
| 菜单行 | — | `pdf` 1 行（`V0343`，按钮型） | 补充 |
| 表 / 迁移 | — | 表 0；迁移 1 版本 / 2 文件 | 补充 |
| `include_str!` | C5 2（出目录） | `pdf` 11 处**同目录**，出目录 **0**；C5 那 2 处属 `monitor` | 口径差 ②（`P2-56` 误归因已更正） |
| 夹具引用 | 0 | 0 | 一致 |
| 真库用例 | 0 | 0 文件、0 行内真库用例（86 用例 0.09s，假库对照同绿） | 一致 |

## 附录 B：本卡未做的事（明确边界）

- 未建 `backend/crates/cloudpivot-pdf/`，未搬任何文件（含 11 个 `.tera`），未改任何 `Cargo.toml`；
- 未改任何 import（2 行出边 + 28 行入边原样保留）、未改 6 条 rustdoc 链接、未改 2 行散文；
- 未改门禁脚本（§6 的 6 处只是**报告**，改动随 C2 的扫描根收口 / C5 一起做）；
- 未新增迁移；`V0343` 的 1 行菜单 / 1 个权限码库内状态不变；
- 未动 `frontend/`（0 引用）与 `scripts/`（2 条命中都是 URL）。
