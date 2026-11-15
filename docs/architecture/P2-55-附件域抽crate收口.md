# P2-55 附件域抽 crate 收口 —— 作废（`attachment` 并入 C4 `cloudpivot-attachment`）

> 卡：`t_252a09ad`（P2-55【P2-C】抽 `attachment` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `3e22d3fe`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41`（`11f490f2`）、`P2-42`（`e9a31e96`）、`P2-43`（`939ec719`）、`P2-44`（`810ce2c7`）、
> `P2-45`（`5a2bc55a`）、`P2-46`（`ca11c5dc`）、`P2-47`（`4648b330`）、`P2-48`（`6a473699`）、
> `P2-49`（`c1f7aa76`）、`P2-50`（`4b95fad3`）、`P2-51`（`0cc8554c`）、`P2-52`（`088c0586`）、
> `P2-53`（`17c41945`）、`P2-54`（`3e22d3fe`）
> 数据库：145（`172.18.10.145:5432`，用户 `cloudpivot`）· 演练库 `cp_p255_attachment_drill`（**已 DROP**）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-attachment/`，不搬 11 个文件 / 9 837 行，不改 23 处
`crate::business::attachment` 引用（**17 行生产 / 6 行测试**，见 §4），不动
`backend/app/Cargo.toml`，不动 `backend/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；卡面验收命令因此按预期
失败（不是回归）：

```bash
$ cd backend && cargo test -p cloudpivot-attachment --offline -- --test-threads=1
error: package ID specification `cloudpivot-attachment` did not match any packages
STUB_EXIT=101
```

```bash
$ cd backend && cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s
exit=0
```

权威依据已入仓：

- `docs/planning/20261113-新卡清单.md` §P2-C.6：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同文件 §P2-C.3 的 **C4** 行：`cloudpivot-storage`、`cloudpivot-attachment`｜节点
  `storage`、`attachment`｜前置 **C1、C3**｜回边 **10 / 23**｜**本步要切 1 行：task 1**｜
  机械量 `20 / 18 434 · 28 · 0 · 2 · 0`（= storage 9/8 597 + attachment 11/9 837，本卡实测与之一致）；
- `P2-42` §4 台账 P2-55 行 = `11 / 9 837`｜SCC 24｜入边 `23（1）`｜出边目标 2｜`5 行`｜`退 1`｜
  落点 **`C4 cloudpivot-attachment`**；
- 本卡 2026-11-15 11:09 的改派留言（并入 C4）。

### 1.1 与前面 13 张作废卡的区别：本卡是 C4 的**一半**，而且解环账是 **0 行**

前面 P2-43 ~ P2-54 大多是「子域并进一个更大的包」或「独立包但带 2~43 行解环账」。本卡的形态不同：

1. **本卡范围 = C4 的一半，不是被吸收**：C4 规定 `cloudpivot-storage` 与
   `cloudpivot-attachment` 是**同一张卡里的两个独立包**（卡内先后 storage → attachment）。
   P2-55 与「C4 的 attachment 半步」是同一件事，作废的实质是**并入 C4 的卡序**。
2. **attachment 自己的解环账 = 0 行**（§3）。`--layer-check` 报的 5 行是
   `C1/C3 已抽出` 假设下的 support(3)+storage(2)：C3 在 C4 之前 ⇒ 合法向下箭头；
   storage 是 C4 卡内前置包 ⇒ 也是合法向下箭头。**C4 那 1 行 `task` 属于 storage**，不是 attachment。
3. **唯一不成立的是时序**：C4 前置 C1（内核）、C3（support）。实测当前 `backend/crates/`
   只有 `cloudpivot-doc`（`# P2-40 已抽出`），**C1、C3 都没落地** ⇒ 现在开工只能得到
   `cloudpivot-attachment → cloudpivot(单体)` 的反向依赖（§P2-C.5 明确拒绝的假拆分）。

⇒ 下一轮要重开，请**直接开 C4**（卡序第 4 位），别再开 P2-55。

## 2. `attachment` 的实测（只读、无库依赖，2026-11-15 于 99）

### 2.1 工具原始输出（逐字）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain attachment --layer-check
域：attachment（scope=top）
  规模：11 个文件 / 9837 行
  SCC：24 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（23）：aiagent, announcement, business/shared, business/task, changelog, docpdf, erp,
    flow, help, hr, impexp, masking, middleware, mutex, notice, pdf, permission, shared, storage, support,
    system, task, utils
  回边（留在应用 crate 的代码 → 本域）：23 处代码级
    erp                    code= 23 all= 23  business/erp/asset/asset_handler.rs:44, …（23 处 all，逐行列于 §4）
  出边（本域 → 其它）：5 个目标
    shared                 code= 12 all= 13
    utils                  code=  4 all=  4
    support                code=  3 all=  3
    storage                code=  2 all=  6
    business/shared        code=  1 all=  3
  层次判据：本步要切 2 条 / 5 行 —— 出边指向更下层以外：support(3), storage(2)
            （假设 L1 内核 shared, utils, business/shared 已抽出、装配层 bin 已独立成包；完整卡序见 --plan-order）
  结论：不可抽（23 处代码级回边，1 个来源）
exit=1
```

```bash
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
卡号     crate                                      本步要切    承诺   判定  说明
C4     cloudpivot-storage, cloudpivot-attachment        1     1   OK
…
全序累计要切：66 行
卡序覆盖 28 个节点；未进卡序（留在单体）2 个：app#root, business#root
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

**5 行 vs 0 行不矛盾**：`--layer-check` 只假设「L1 内核 + bin 已抽出」，所以把 support(3)、
storage(2) 也算成要切；按卡序，C3（support）在 C4 之前完成、storage 是 C4 卡内前置包，
这 5 行就都变成**合法的向下箭头**（`attachment → cloudpivot-support`、`→ cloudpivot-storage`）。

### 2.2 SCC：与 23 个节点同环，但本域的环可以「按卡序单向化」

`attachment` 在顶层 24 节点 SCC 里（环内还有 `storage`、`support`、`erp`、`task` 等）。
拆包不要求先出环，只要求**每一步的出边都指向已抽出的包**：

- `attachment → storage`（2 行）：storage 是 C4 卡内前置包 ⇒ 合法；
- `storage → attachment`：**0 行**（`--domain storage` 的出边表里没有 attachment）⇒ 二者无环，可先后抽出；
- `attachment → support`（3 行）：C3 在前 ⇒ 合法；
- `attachment → shared/utils/business::shared`（17 行）：C1 内核 ⇒ 合法。

⇒ **attachment 是本仓少见的「前置齐备后 0 行解环」域**，与 `pdf`（P2-57，0 行）同类。

## 3. 出边：22 行代码（prod 17 / test 5），按前置卡归类后 attachments 本步要切 **0 行**

`rg 'crate::' business/attachment`（排除自身路径）实测 **29 条命中 = 代码 22 + 注释 7**，
与工具 `code=12+4+3+2+1=22` 完全一致。

| # | 文件:行 | 目标 | 形态 | 归属前置 | 行内容 |
|--:|:--|:--|:--:|:--:|:--|
| 1 | `handler.rs:43` | `business::shared` | 码 | C1 内核 | `use crate::business::shared::user_name;` |
| 2 | `rule_admin/handler.rs:41` | `support` | 码 | C3 | `use crate::support::log;` |
| 3 | `handler.rs:49` | `support` | 码 | C3 | `use crate::support::log;` |
| 4 | `handler.rs:50` | `support` | 码 | C3 | `use crate::support::log::model::OperationLogModel;` |
| 5 | `handler.rs:44` | `business::storage` | 码 | C4 卡内（storage 先） | `use crate::business::storage;` |
| 6 | `handler.rs:45` | `business::storage` | 码 | C4 卡内（storage 先） | `use crate::business::storage::handler::{…}` |
| 7 | `dto.rs:26` | `shared` | 码 | C1 | `use crate::shared::error_code;` |
| 8 | `handler.rs:48` | `shared` | 码 | C1 | `use crate::shared::error_code;` |
| 9 | `rule.rs:62` | `shared` | 码 | C1 | `use crate::shared::error_code;` |
| 10 | `rule_admin/dto.rs:30` | `shared` | 码 | C1 | `use crate::shared::constants::SUPER_ADMIN_ROLE_CODE;` |
| 11 | `rule_admin/dto.rs:31` | `shared` | 码 | C1 | `use crate::shared::error_code;` |
| 12 | `rule_admin/handler.rs:40` | `shared` | 码 | C1 | `use crate::shared::error_code;` |
| 13 | `rule_admin/model.rs:26` | `shared` | 码 | C1 | `use crate::shared::error_code;` |
| 14 | `rule_admin/tests.rs:36` *(test)* | `shared` | 码 | C1 | `use crate::shared::constants::SUPER_ADMIN_ROLE_CODE;` |
| 15 | `rule_admin/tests.rs:37` *(test)* | `shared` | 码 | C1 | `use crate::shared::error_code;` |
| 16 | `tests.rs:34` *(test)* | `shared` | 码 | C1 | `use crate::shared::error_code;` |
| 17 | `tests.rs:762` *(test)* | `shared` | 码 | C1 | `assert_eq!(error.code, crate::shared::error_code::PERMISSION_DENIED);` |
| 18 | `tests.rs:1917` *(test)* | `shared` | 码 | C1 | `assert_eq!(error.code, crate::shared::error_code::PARAM_INVALID);` |
| 19 | `dto.rs:738` | `utils` | 码 | C1 | `match (uploaded_by, crate::utils::current_user_uuid(claims))` |
| 20 | `handler.rs:51` | `utils` | 码 | C1 | `use crate::utils;` |
| 21 | `rule_admin/handler.rs:42` | `utils` | 码 | C1 | `use crate::utils;` |
| 22 | `rule_admin/model.rs:27` | `utils` | 码 | C1 | `use crate::utils;` |

注释 7 条（工具不计入 `code`，搬包后是 rustdoc 链接失效，需顺手改成 `cloudpivot_*::…`）：
`handler.rs:19,21`（storage）、`mod.rs:5` / `model.rs:4`（storage）、`handler.rs:1070`（shared）、
`model.rs:953,965`（business::shared）。

**归类结论**

| 目标 | 行数 | 抽出后指向 | 本步要切 |
|:--|--:|:--|:--:|
| `shared` / `utils` / `business::shared` | 12 + 4 + 1 = **17** | C1 `cloudpivot-kernel` | 0 |
| `support` | **3** | C3 `cloudpivot-support`（C4 前置） | 0 |
| `business::storage` | **2** | C4 卡内 `cloudpivot-storage`（先抽） | 0 |
| 合计 | **22** | — | **0 行** |

**C4 那 1 行 `task` 属于 storage，不属于 attachment**（核到行）：

```
business/storage/cleanup.rs:25   use crate::task::Scheduler;
```

反向的 `task/mod.rs:238  crate::business::storage::cleanup::register_builtin(self)` 是
「单体 → storage crate」的合法方向，不用切（C14 之后变 `→ cloudpivot-storage`）。

## 4. 入边：23 行代码（prod **17** / test **6**），全部来自 `erp` 一个源

`rg 'business::attachment'`（排除本模块）实测 25 条 = **代码 23 + 注释 2**，与工具 `code=23 / 1 源` 一致。

| # | 文件:行 | prod/test | 行内容 |
|--:|:--|:--:|:--|
| 1 | `erp/asset/asset_handler.rs:44` | prod | `use crate::business::attachment::model::DocKind as AttachDocKind;` |
| 2 | `erp/asset/asset_handler.rs:45` | prod | `use crate::business::attachment::rule::{self as attach_rule, RequiredAt as AttachAt};` |
| 3 | `erp/asset/asset_linkage_tests.rs:768` | **test** | `crate::business::attachment::test_support::attach_pdf(` |
| 4 | `erp/asset/asset_linkage_tests.rs:769` | **test** | `crate::business::attachment::model::DocKind::FixedAsset,` |
| 5 | `erp/contract/contract_pdf.rs:40` | prod | `use crate::business::attachment::model::{self as attachment_model, DocKind, ListOrder};` |
| 6 | `erp/expense/expense_handler.rs:40` | prod | `…::model::DocKind as AttachDocKind;` |
| 7 | `erp/expense/expense_handler.rs:41` | prod | `…::rule::{self as attach_rule, RequiredAt as AttachAt};` |
| 8 | `erp/expense/expense_linkage_tests.rs:412` | **test** | `crate::business::attachment::test_support::attach_pdf(` |
| 9 | `erp/expense/expense_linkage_tests.rs:413` | **test** | `…::model::DocKind::ExpenseReport,` |
| 10 | `erp/invoice.rs:107` | prod | `…::model::DocKind as AttachDocKind;` |
| 11 | `erp/invoice.rs:108` | prod | `…::rule::{self as attach_rule, RequiredAt as AttachAt};` |
| 12 | `erp/payable/payable_payment.rs:98` | prod | `…::model::DocKind as AttachDocKind;` |
| 13 | `erp/payable/payable_payment.rs:99` | prod | `…::rule::{self as attach_rule, RequiredAt as AttachAt};` |
| 14 | `erp/purchase/purchase_return_handler.rs:52` | prod | `…::model::DocKind as AttachDocKind;` |
| 15 | `erp/purchase/purchase_return_handler.rs:53` | prod | `…::rule::{self as attach_rule, RequiredAt as AttachAt};` |
| 16 | `erp/receipt.rs:76` | prod | `…::model::DocKind as AttachDocKind;` |
| 17 | `erp/receipt.rs:77` | prod | `…::rule::{self as attach_rule, RequiredAt as AttachAt};` |
| 18 | `erp/sales/sales_return_handler.rs:59` | prod | `…::model::DocKind as AttachDocKind;` |
| 19 | `erp/sales/sales_return_handler.rs:60` | prod | `…::rule::{self as attach_rule, RequiredAt as AttachAt};` |
| 20 | `erp/stocktake/stocktake_handler.rs:35` | prod | `…::model::DocKind as AttachDocKind;` |
| 21 | `erp/stocktake/stocktake_handler.rs:36` | prod | `…::rule::{self as attach_rule, RequiredAt as AttachAt};` |
| 22 | `erp/stocktake/stocktake_tests.rs:347` | **test** | `crate::business::attachment::test_support::attach_pdf(` |
| 23 | `erp/stocktake/stocktake_tests.rs:348` | **test** | `…::model::DocKind::StockTake,` |

注释 2 条（工具不计入，搬包后是 rustdoc 失效）：`hr/mod.rs:90`、`monitor/diagnostic.rs:18`。

**入边的形态高度收敛**，对改 import 有利：prod 17 行只有 4 种模式 ——
`model::DocKind as AttachDocKind`（9 处）、`rule::{self as attach_rule, RequiredAt as AttachAt}`（8 处，
与前者成对）、`contract_pdf.rs:40` 的 `{model, DocKind, ListOrder}`（1 处）。改法唯一：
`crate::business::attachment::` → `cloudpivot_attachment::`。

### 4.1 6 行测试入边会**在跨 crate 后直接编译失败**（不是改 import 能解决的）

`test_support` 是 `business/attachment/mod.rs:89` 的 `#[cfg(test)] pub mod test_support`。
`#[cfg(test)]` 是**按 crate 生效**的：`cloudpivot-attachment` 作为依赖被 `cloudpivot` 的 test
构建编译时**不带** `cfg(test)` ⇒ `test_support` 整个模块消失，三个 `*_tests.rs`
（`asset_linkage_tests` / `expense_linkage_tests` / `stocktake_tests`，均 `#[cfg(test)] mod`）的 6 行全部编译失败。

⇒ C4 的施工单必须多一条**设计决定**（本卡只报不动）：把夹具导出成
`#[cfg(any(test, feature = "test-support"))] pub mod test_support;`，并在
`app/Cargo.toml` 的 `[dev-dependencies]` 里 `cloudpivot-attachment = { path = "…", features = ["test-support"] }`。
用 `cfg(any(test, …))` 而不是只加 feature，是为了让 crate 自己的 96 条用例仍能在
`cargo test -p cloudpivot-attachment` 里直接用它（卡面验收就是这条命令）。

## 5. 机械量（与 C4 行 `20 / 18 434 · 28 · 0 · 2 · 0` 对得上）

| 项 | attachment 实测 | C4 合计（含 storage 9 / 8 597） | 说明 |
|:--|:--|:--|:--|
| 文件 / 行 | **11 / 9 837** | 20 / 18 434 ✔ | 顶层 5（`mod/handler/dto/model/rule`）+ `rule_admin/` 4 + `tests.rs` + `rule_admin/tests.rs` |
| 路由 | **12** | 28 ✔ | storage 16 + attachment 12；attachment = `handler.rs` 6（upload/list/check/sort/delete/download）+ `rule_admin/handler.rs` 6（list/options/create/update/enabled/delete） |
| 权限码 | **7**（去重） | — | 12 处 `#[nik::permission]` = `business:attachment:upload×3` + `business:attachment:list×3` + `support:attachment-rule:{list×2,create×1,update×2,delete×1}`。第 7 个码 `business:attachment:delete` **不挂路由**，是行级升权码（`dto.rs:35 PERM_DELETE`，`dto::can_delete` 判） |
| 菜单行 | **7** | — | `V0212` 3 行（纯按钮行，`menu_type=3`）+ `V0349` 4 行（1 个页面行 `/support/attachment-rule` + 3 个按钮行） = 7 行 / 7 码 |
| 真库用例文件（`*_db_tests.rs`） | **0** | 0 ✔ | 但**行内**真库用例 28 个（`db_` 前缀），见 §7 假绿对照 |
| 出目录 `include_str!` | **2** | 2 ✔ | `tests.rs:502,515` → `../../../migrations/business/V0362__doc_attachment_file_name.{up,down}.sql` |
| 夹具引用 | **0** | 0 ✔ | 无 `test_helpers` / `fixture_hygiene` / `SEED_ROLE_LOCK` / `ensure_any_*` |
| `#[cfg(test)]` | 6 | — | `mod.rs` 2 + `model.rs` 2 + `rule.rs` 1 + `rule_admin/mod.rs` 1 |
| `#[test]` / `#[tokio::test]` | 96 | — | `tests.rs` 65 + `rule_admin/tests.rs` 14 + `rule.rs` 17 |
| 表 | 3 | — | `biz_doc_attachment`（桥接表）、`biz_attachment_rule`（必传规则）、`sys_file`（复用 storage，只读/写引用） |
| 迁移 | 9 | — | `V0212`（建表+3 码+菜单）/`V0344`（sort_order）/`V0345`（file_roles）/`V0347`（必传规则）/`V0349`（规则管理菜单 4 码）/`V0350`（advice 单据类型）/`V0352`（uploader_name 快照）/`V0362`（file_name）/`V0389`（HR 单据类型） |

**搬包时另有一处会断**（不在 C4 行的 5 个机械量里）：上表 2 处 `include_str!` 的相对深度会变。
现在 `src/business/attachment/tests.rs` + `../../../` = `backend/app/migrations/`；搬到
`crates/cloudpivot-attachment/src/tests.rs` 后 `../../../` = `backend/` ⇒ 需改写为
`../../app/migrations/…`（且 crate 不持有迁移目录，只能反向引用 app 包 —— 与 P2-54
「包内自身路径改写」同类，属 C4 施工单的第 2 条设计决定）。

## 6. 搬包会失效的 5 处门禁（按源码路径硬编码，本卡只报不动）

| # | 位置 | 现状 | 搬迁后 | 严重度 |
|--:|:--|:--|:--|:--|
| 1 | `system/menu/permission_audit_tests.rs:747-748` `src_root() = CARGO_MANIFEST_DIR/src` | 只扫 `backend/app/src` | **N14 直接变红**：7 个码在 `app/src` 内**零外部引用**（实测逐码 0，见下），`dead` 非空即断言失败 | 🔴 红 |
| 2 | 同上（`scan_routes`） | 12 条 attachment 路由在扫描面内 | 12 条路由**静默逃出** N16（裸路由）/N21（静态路由被模板遮蔽）；`routes.len() > 300` 仍过 | 🟠 静默 |
| 3 | `scripts/lint/scope_gate.py:83-84` `SKIP_MANIFEST` | 显式登记 `business/attachment/tests.rs`、`business/attachment/rule_admin/tests.rs` | 两个文件搬走后，「实测跳过集合 ≠ SKIP_MANIFEST（160 条）」⇒ **闸直接 FAIL**（这是刻意摩擦，报错会给可粘贴行） | 🔴 红（好在会响） |
| 4 | `business/shared/layering_tests.rs:43` `MIN_DOMAINS = 18` | `src/business` 下 22 个目录（含 attachment） | 21 个，仍 ≥18 ⇒ **静默变绿**，attachment 的跨域引用从此无人扫 | 🟠 静默 |
| 5 | `scripts/lint/path_refs_gate.py:51-65`（`$SRC` → `backend/app/src`） | 当前 899 个脚本 0 失效 | 任何脚本/hook 里写 `$SRC/business/attachment/…` 即红（今天实测 0 处，属**预防性**） | 🟡 预防 |

7 个码的「外部引用 = 0」实测（`backend/app/src`，排除 `business/attachment/**`）：

```
business:attachment:upload            0
business:attachment:list              0
business:attachment:delete            0
support:attachment-rule:list          0
support:attachment-rule:create        0
support:attachment-rule:update        0
support:attachment-rule:delete        0
```

即：这 7 个码的**唯一落地**就是 `business/attachment/**` 里的 `#[nik::permission]` / 常量声明。
搬出 `app/src` ⇒ N14 的扫描面看不到它们 ⇒ 7 个菜单行（`V0212` 3 条 + `V0349` 4 条）判成死码。

**建议（并入 C2 的扫描根收口，与 P2-54 同一条）**：把
`Repo::load(&src_root())` 的扫描根扩成 `app/src + crates/*/src`，
`scope_gate.py` 的 `SCAN_DIRS` / `SKIP_MANIFEST` 与 `layering_tests` 的目录枚举同源化。
这是**所有域抽包卡**的共同前置，不是 attachment 独有。

## 7. 验证与基线（真库全绿 + 假绿对照 + 三个门禁）

```bash
# 场景 A（99 上跑，连 145 的 PG）
PGPASSWORD=*** createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_p255_attachment_drill
ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p255_attachment_drill" \
HR_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p255_attachment_drill" \
  cargo test --offline -p cloudpivot --lib business::attachment -- --test-threads=1
```

```
running 96 tests
test result: ok. 96 passed; 0 failed; 0 ignored; 0 measured; 5585 filtered out; finished in 41.73s
```

- **真库真的连上了**：41.73s、`SKIP` 行 0 条（28 个 `db_` 前缀用例实跑）。
- **假绿对照**（同一条命令，`env -u ERP_DB_URL -u HR_DB_URL`）：

  ```
  test result: ok. 96 passed; 0 failed; 0 ignored; 0 measured; 5585 filtered out; finished in 0.02s
  NODB_EXIT=0
  ```

  **同样是 96 passed，但 0.02s** —— 跑数不能证明连库。attachment 的真库入口是
  `tests.rs:1149-1176` / `rule_admin/tests.rs:329-356` 的 `try_db()`（`ERP_DB_URL` 未设 → `None` →
  用例提前 `return`，`println!("SKIP: …")` 被 libtest 默认捕获，**日志里看不到**）。
  判真跑的唯一可靠依据是**耗时量级**（41.73s vs 0.02s）。
- 演练库已清理：`cp_p255_attachment_drill` 已 `DROP`。

门禁基线（搬迁前，全绿）：

```bash
$ python3 scripts/lint/scope_gate.py                      → exit=0（PASS：跳过清单与 SKIP_MANIFEST 逐条一致，160 条；0 处越界）
$ python3 scripts/lint/path_refs_gate.py                  → exit=0（899 个脚本，0 处失效路径）
$ python3 scripts/lint/module_cycle_report.py             → exit=0
$ cd backend && cargo build --workspace --offline         → exit=0
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md → exit=0（C4 1/1 OK，全序 66 行）
```

## 8. 下一轮怎么开（直接开 C4，卡内 storage → attachment）

1. **前置**：C1 `cloudpivot-kernel`（shared/utils/business::shared）、C3 `cloudpivot-support`。二者未落地前开工
   只会得到反向依赖。
2. **卡内顺序**：先 `cloudpivot-storage`（`attachment → storage` 2 行要求 storage 已是包），再
   `cloudpivot-attachment`。两张包在同一张卡里做，`attachment ⇄ storage` 不成环（storage→attachment 实测 0 行）。
3. **本步要切 1 行**（属 storage）：`business/storage/cleanup.rs:25 use crate::task::Scheduler;`
   —— 注册点倒置（`task/mod.rs:238` 已是「单体 → storage」的合法方向）。
4. **attachment 本步要切 0 行**，但施工单必须额外带两条设计决定（本卡发现，别漏）：
   - `test_support` 改 `#[cfg(any(test, feature = "test-support"))]` + app 侧 `[dev-dependencies]` 开 feature（§4.1）；
   - 2 处 `include_str!("../../../migrations/…")` 改深一层到 `../../app/migrations/…`（§5 末）。
5. **改 import 的面**：22 处（prod 17 / test 5 + 6 行 test_support 需随 feature 改路径）→
   `crate::business::attachment::` → `cloudpivot_attachment::`；顺手把 7 条 rustdoc 链接改掉。
6. **门禁同批改**（否则 C4 一落地就红）：第 6 节的 #1/#2/#3/#4 四处扫描根需要与 C2 同批扩到
   `crates/*/src`；在此之前不要单独搬 attachment。

## 附录 A：核对清单（本卡实测 vs 计划表）

| 项 | 计划表（§P2-C.3 C4 行） | 本卡实测 | 结论 |
|:--|:--|:--|:--|
| 文件 / 行 | 20 / 18 434（storage+attachment） | 11 / 9 837（attachment）+ 9 / 8 597（storage） | 一致 |
| 路由 | 28 | 12（attachment）+ 16（storage） | 一致 |
| 真库用例文件 | 0 | 0（但行内 28 个 `db_` 用例） | 口径是「文件数」，**行内真库用例计划表未计** → 建议 C4 卡面写明「真库用例 28 条，必须带库跑」 |
| 出目录引用 | 2 | 2 | 一致（且搬包后需改深度，计划表未提） |
| 夹具引用 | 0 | 0 | 一致 |
| 本步要切 | 1 行（task 1） | storage→task 1 行；attachment 0 行 | 一致，且**确认那 1 行不属于 attachment** |
| 入边 | 10（storage）/ 23（attachment） | 23（erp 1 源；prod 17 / test 6） | 一致；计划表未区分 prod/test，也未提 `test_support` 的跨 crate 问题 |

## 附录 B：本卡未做的事（明确边界）

- 未建 `backend/crates/cloudpivot-attachment/`，未搬任何文件，未改任何 `Cargo.toml`；
- 未改任何 import（22 处代码引用原样保留）、未改 7 条 rustdoc 链接；
- 未改门禁脚本（第 6 节 5 处只是**报告**，改动随 C2 的扫描根收口一起做）；
- 未新增迁移；`biz_doc_attachment` / `biz_attachment_rule` / 7 个权限码 / 8 个菜单行的库内状态不变。
