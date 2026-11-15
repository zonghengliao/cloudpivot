# P2-48 合同域抽 crate 收口 —— 作废（`contract` 随 C13 `cloudpivot-erp` 整包搬迁）

> 卡：`t_d8b3c188`（P2-48【P2-C】抽 `erp-contract` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `4648b330`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41-成本域抽crate可行性分析.md` §8（`11f490f2`）、`P2-42-…旧P2-C链实测.md`（`e9a31e96`，§4 已列本卡行）、
> `P2-43-销售域抽crate收口.md`（`939ec719`）、`P2-44-库存域抽crate收口.md`（`810ce2c7`）、
> `P2-45-财务域抽crate收口.md`（`5a2bc55a`）、`P2-46-资产域抽crate收口.md`（`ca11c5dc`）、
> `P2-47-预算域抽crate收口.md`（`4648b330`）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-erp-contract/`，不搬 9 个文件 / 8 775 行，不改任何
`crate::business::erp::contract` import，不动 `app/Cargo.toml`，不动 `backend/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；
卡面验收命令 `cargo test -p cloudpivot-erp-contract` 因此按预期失败（不是回归，见 §4）。

权威依据已入仓（不是本卡临时主张）：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 **288** 行：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同表第 **226** 行 C13 = `cloudpivot-erp`｜`erp`（**一个包，不拆子域**）｜切向仅 `task 8` + `business/task 4` = 12 行；
- 同节第 **292–295** 行：`erp` 内部 51 个节点互相咬住 ⇒ 一个包，包内继续用目录分层；
- `P2-42` §4 的 17 卡台账第 **P2-48** 行已提前记入本卡实测（`9 / 8 775`｜SCC 51｜入边 7（7）｜出边 7 目标｜
  要切 17 行｜`--layer-check` 退 1｜落点 C13 包内），本卡是对该行的逐行复核与入档。

## 2. `contract` 的实测（只读、无库依赖，2026-11-15 于 99）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain contract --scope erp-sub
域：contract（scope=erp-sub）
  规模：9 个文件 / 8775 行
  SCC：51 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（50）：asset, budget, cost, customer, erp#root:approval_gate, …,
                           expense, ext:changelog, ext:flow, ext:hr, ext:impexp, ext:task,
                           linkage, payable, period, price, purchase, receivable, report,
                           sales, shipment, stockin, stockout, stocktake, transfer, voucher, year
  回边（留在应用 crate 的代码 → 本域）：7 处代码级
    erp#root:doc_reader_impl       code=  1 all=  1  business/erp/doc_reader_impl.rs:121
    erp#root:doc_reader_impl_tests code=  1 all=  1  business/erp/doc_reader_impl_tests.rs:28
    erp#root:order_pdf             code=  1 all=  1  business/erp/order_pdf.rs:371
    ext:shared                     code=  1 all=  2  shared/permission_alias.rs:111
    purchase                       code=  1 all=  1  business/erp/purchase/purchase_handler.rs:47
    sales                          code=  1 all=  1  business/erp/sales/sales_order_handler.rs:82
    stock                          code=  1 all=  1  business/erp/stock/stock_pdf.rs:1362
  出边（本域 → 其它）：7 个目标
    ext:flow                     code=  4 all=  4
    linkage                      code=  4 all=  4
    ext:changelog                code=  3 all=  3
    erp#root:fixture_hygiene     code=  2 all=  2
    erp#root:approval_gate       code=  2 all=  2
    erp#root:serial_model        code=  1 all=  1
    erp#root:order_pdf_model     code=  1 all=  1
  层次判据：本步要切 7 条 / 17 行 —— 出边指向更下层以外：ext:flow(4), linkage(4), ext:changelog(3),
            erp#root:fixture_hygiene(2), erp#root:approval_gate(2), erp#root:serial_model(1),
            erp#root:order_pdf_model(1)
  结论：不可抽（7 处代码级回边，7 个来源）

$ python3 scripts/verify/crate_split_readiness.py --domain contract --scope erp-sub --check        # 旧口径（只看入边）
exit=1
$ python3 scripts/verify/crate_split_readiness.py --domain contract --scope erp-sub --layer-check  # 层次口径（入边+出边）
exit=1
```

文件口径独立复核（工具口径 = 该目录下全部 `.rs`，含 `mod.rs`；目录无嵌套子目录，两口径一致）：

```bash
$ cd /opt/cloudpivot/backend && wc -l app/src/business/erp/contract/*.rs
  24 app/src/business/erp/contract/mod.rs
 283 app/src/business/erp/contract/contract_linkage.rs
 612 app/src/business/erp/contract/contract_bridge.rs
 689 app/src/business/erp/contract/contract_pdf.rs
 747 app/src/business/erp/contract/contract_linkage_tests.rs
 990 app/src/business/erp/contract/contract_approval_tests.rs
1330 app/src/business/erp/contract/contract_dto.rs
1674 app/src/business/erp/contract/contract_model.rs
2426 app/src/business/erp/contract/contract_handler.rs
8775 total
$ find app/src/business/erp/contract -name '*.rs' | wc -l
9
```

**出边 17 行逐行复核**（脚本 7 目标 / 17 行，逐行对得上）：

```
contract/contract_approval_tests.rs:43  use crate::business::flow::engine;                                      → ext:flow
contract/contract_approval_tests.rs:65  use crate::business::erp::fixture_hygiene::SEED_ROLE_LOCK as DB_LOCK;   → erp#root:fixture_hygiene
contract/contract_bridge.rs:53          use crate::business::flow::settle_guard;                                → ext:flow
contract/contract_bridge.rs:54          use crate::business::flow::{engine, model as flow_model};               → ext:flow
contract/contract_dto.rs:675            pub gate: crate::business::erp::approval_gate::SkipApprovalReq,         → erp#root:approval_gate
contract/contract_handler.rs:65         use crate::business::changelog::{self, Action as ChangeAction, …};      → ext:changelog
contract/contract_handler.rs:66         use crate::business::erp::approval_gate;                                → erp#root:approval_gate
contract/contract_handler.rs:74         use crate::business::flow::{engine, model as flow_model};               → ext:flow
contract/contract_linkage.rs:65         use crate::business::erp::linkage;                                      → linkage
contract/contract_linkage_tests.rs:43   use crate::business::erp::linkage;                                      → linkage
contract/contract_linkage_tests.rs:44   use crate::business::erp::linkage::model as linkage_model;               → linkage
contract/contract_linkage_tests.rs:45   use crate::business::erp::linkage::{engine as linkage_engine, spec};    → linkage
contract/contract_linkage_tests.rs:56   use crate::business::erp::fixture_hygiene::SEED_ROLE_LOCK as DB_LOCK;   → erp#root:fixture_hygiene
contract/contract_model.rs:768          let period_key = crate::business::erp::serial_model::period_key_of(now); → erp#root:serial_model
contract/contract_model.rs:1521         use crate::business::changelog::diff::{self, BIZ_CONTRACT};            → ext:changelog
contract/contract_model.rs:1522         use crate::business::changelog::{Action, mask};                        → ext:changelog
contract/contract_pdf.rs:43             use crate::business::erp::order_pdf_model;                             → erp#root:order_pdf_model
```

= **脚本口径 17 行 / 7 目标**：`ext:flow` 4、`linkage` 4、`ext:changelog` 3、`fixture_hygiene` 2、
`approval_gate` 2、`serial_model` 1、`order_pdf_model` 1。

**但脚本对本域少报 10 行 / 6 个目标**（本卡复核时发现，`--scope erp-sub` 的口径缺项，见 §6 第 3 条）。
实际出边 = **27 行 / 13 个目标**：

```
① 花括号形式（正则 `crate::business::erp::(name)` 认不出 `erp::{…}`）：
   contract_model.rs:1158   use crate::business::erp::{purchase::purchase_model, sales::sales_order_model};
                            → purchase（C13 内）+ sales（C13 内）    [1 行 / 2 目标]
② 不在 ext 名单里的四个域（pattern2 只列 flow|hr|impexp|changelog|masking|task）：
   contract_linkage.rs:66   use crate::business::notice;                                         → notice    (C5)
   contract_linkage.rs:67   use crate::business::notice::dto::{NoticeLevel, NoticeType};         → notice    (C5)
   contract_pdf.rs:40       use crate::business::attachment::model::{…DocKind, ListOrder};       → attachment(C4)
   contract_pdf.rs:41       use crate::business::docpdf::doc_pdf_model::{…ExportLogOwned};       → docpdf    (C5)
   contract_pdf.rs:44       use crate::business::pdf::template::{…};                             → pdf       (C5)
   contract_pdf.rs:48       use crate::business::pdf::{engine, handler as pdf_handler};          → pdf       (C5)
   contract_pdf.rs:337      use crate::business::pdf::template::sample_contract;                 → pdf       (C5)
   contract_pdf.rs:397      use crate::business::pdf::template::PAYMENT_TERMS_EMPTY;             → pdf       (C5)
   contract_pdf.rs:567      … HashMap<&str, &crate::business::pdf::template::ContractAttachment>  → pdf       (C5)
```

修正后账目：**erp 包内兄弟 10 行**（`linkage` 4、`fixture_hygiene` 2、`approval_gate` 2、`serial_model` 1、
`order_pdf_model` 1）+ **跨域 17 行 / 8 个域**（`flow` 4、`changelog` 3、`pdf` 5、`notice` 2、
`attachment` 1、`docpdf` 1、`purchase`+`sales` 合并计 1 行）＝ **27 行 / 13 目标**。
即：**跨域接缝 17 行占全部出边的 63%**，比脚本口径（7/17 = 41%）更依赖 C4/C5/C6/C12 四个包。
没有一条指向 `system`/`support`/`task`/`hr`/`impexp`，也没有指向 L1 内核（内核引用另计，见下）。

> 口径核对：`grep -rn 'crate::business::' contract/ | grep -v 'crate::business::erp::contract' | wc -l` = **45 行**
> = 出边 27（含 1 行双目标）+ 内核 `crate::business::shared::*` **15 行** + 3 行文档注释
> （`contract_dto.rs:3/:269`、`contract_model.rs:157` 提到 `erp::purchase`/`erp::dto` 的「同源」说明）。
> 即 27 + 15 + 3 = 45，逐项对得上。

**入边 7 行逐行复核**（脚本 7 来源 / 7 行，全部命中）：

```
business/erp/doc_reader_impl.rs:121        use crate::business::erp::contract::{contract_bridge, contract_model};
business/erp/doc_reader_impl_tests.rs:28   use crate::business::erp::contract::contract_bridge;
business/erp/order_pdf.rs:371              Some(contract_id) => crate::business::erp::contract::contract_model::contract_find(contract_id)
business/erp/purchase/purchase_handler.rs:47        use crate::business::erp::contract::contract_model;
business/erp/sales/sales_order_handler.rs:82        use crate::business::erp::contract::contract_model::{self, ContractRow};
business/erp/stock/stock_pdf.rs:1362       crate::business::erp::contract::contract_pdf::EXPORT_OPERATION,
src/shared/permission_alias.rs:111             use crate::business::erp::contract::contract_handler;   // 实测路径 app/src/shared/permission_alias.rs（mod tests 内）
```

= **7 行 / 7 来源**（生产 5 / 测试 2：`doc_reader_impl_tests` 1 + `permission_alias.rs:111` 是 `mod tests` 内的 `use`）。
另有 **5 行文档注释**提到本域（不计代码级，脚本 `shared/permission_alias.rs` 因此 `code=1 / all=2`）：
`shared/permission_alias.rs:49`、`business/erp/linkage/spec.rs:1094`、`business/hr/leave_model.rs:32`、`:461`、
`business/hr/leave_handler.rs:879`。故 `grep -rn "erp::contract"` 全仓报 **12 行**（7 代码 + 5 注释），两口径一致。

另有 **28 行落在 L1 内核**（C1/C3 内的合法下层依赖，按层次口径不计入切边）：

```
shared::error_code              12 行（approval_tests:44, bridge:56, dto:43, handler:77, linkage_tests:47,
                                     pdf:98/225/226/227/250/264/687）
business::shared::state_machine   9 行（bridge:55, dto:42, handler:75, linkage:68, linkage_tests:46,
                                     model:42/76/80/937）
business::shared::user_name       3 行（dto:917, handler:76/1806）
business::shared::separation      2 行（bridge:471, 488）
business::shared::scope           1 行（model:41）
shared::permission_alias          1 行（handler:1427）
```

**进链接单元靠什么**：`app/src/business/erp/mod.rs:125 pub mod contract;` +
`contract_handler.rs` 里 **6 条路由（12 条 `#[nik::*]` 属性 = 6 `#[nik::permission]` + 6 路由宏）**，
5 个权限码（`list` 声明 2 次、`create`/`update`/`status`/`delete` 各 1 次），路由自注册，无手写 `routes()` 汇编：

```
GET   /business/erp/contract/page              business:erp:contract:list
GET   /business/erp/contract/{id}              business:erp:contract:list
POST  /business/erp/contract                   business:erp:contract:create
PUT   /business/erp/contract/{id}              business:erp:contract:update
PATCH /business/erp/contract/{id}/status       business:erp:contract:status
DEL   /business/erp/contract/{id}              business:erp:contract:delete
```

`mod.rs` 声明 7 个模块：6 个 `pub`（`contract_bridge`/`contract_dto`/`contract_handler`/`contract_model`/`contract_pdf`
+ `pub(crate)` 的 `contract_linkage`）与 2 个 `#[cfg(test)]` 测试模块（`contract_approval_tests`、
`contract_linkage_tests`）；**无 `#[path]` 属性、无 `include_str!`**，整目录平移本身是「路径干净」的
（`grep -rn "include_str!\|include_bytes!" contract/` 0 命中）。真库单测 16 个（`#[test]`/`#[tokio::test]`
= approval_tests 10 + linkage_tests 6），落在两个 `*_tests.rs` —— 搬家时同样不用改路径。

## 3. 为什么不能单独成包

1. **在 51 节点大 SCC 里**（>1 ⇒ 与本批 50 个兄弟不可能拆成两个无环包）。`contract` 两个方向都欠账：
   入边 7 行（7 源）、出边 **27 行 / 13 目标**（脚本口径 17 行 / 7 目标，少报见 §2）。
   入边在 `erp` 子域里算少的（对比 `purchase` 61、`budget` 20），但出边里有 **17 行跨 C4/C5/C6/C12 四个包**
   （`flow` 4、`changelog` 3、`pdf` 5、`notice` 2、`attachment` 1、`docpdf` 1、`purchase`+`sales` 1）——
   这 17 行在 C13 落地前没有任何 crate 形态可依赖，解环账一处不少。
2. **`contract` 是 ERP 的「被多方消费的枢纽」**，不是叶子：`purchase`（采购单 `contract_id` 校验）、
   `sales`（`ContractRow`）、`stock`（合同 PDF 导出操作码）、`order_pdf`（合同号渲染）、
   `doc_reader_impl`（文档识别取合同）五处生产代码直接 `use`；而 `contract` 回头又要
   `flow::engine/settle_guard`（提交审批）、`linkage`（LNK-07 联动 hook 与排除清单）、
   `changelog`（单据变更留痕）、`approval_gate`（免审开关）、`notice`（到期提醒）、
   `pdf`/`attachment`/`docpdf`（合同 PDF 导出）才能跑完一条合同链路。
   这是**双向业务咬合**，与本批「erp 内部 51 节点一个 SCC」的结论互为印证。
3. **旧口径会把方向判反**：`--check` 只看入边，`contract` 入边 7 行 ⇒ 退 1 是「巧合的正确」；
   真正的判据是**出边**（装配层 → 域 crate 合法，域 crate → 单体才是环）。`--layer-check` 对本域同样退 1：
   脚本口径「本步要切 7 条 / 17 行」，**按 §2 修正后是 13 条 / 27 行** —— 两种口径都指向不可抽。
4. **有一条 `crate::` 图看不见的硬耦合（本卡新发现，前面几张卡没遇到）**：
   `business/erp/linkage/tests_wiring.rs` 用**相对路径 `include_str!` 读本域源码**做接线断言：

   ```
   linkage/tests_wiring.rs:802  include_str!("../../erp/contract/contract_linkage.rs")
   linkage/tests_wiring.rs:835  include_str!("../../erp/contract/contract_handler.rs")
   linkage/tests_wiring.rs:850  include_str!("../../erp/contract/contract_bridge.rs")
   ```

   这 3 行**不进 `crate::` 边图**（不是 import），但一旦把目录搬进 `crates/cloudpivot-erp-contract/src/`，
   编译期直接 `couldn't read …` —— 即「抽 crate」的真实改动面 = 17 行跨域 `crate::` 出边 **+ 3 行
   源码路径接线**，且这 3 行还必须反向依赖（`linkage` 读 contract 源码），进一步坐实 §3.2 的双向咬合。
   反过来也说明：**C13 整包搬迁时这 3 行同样不用改**（`erp/contract/` 与 `erp/linkage/` 相对位置不变）。
5. **C13 之前抽 = 反向依赖单体**：出边目标 `flow`/`linkage`/`approval_gate`/`serial_model`/`order_pdf_model`/
   `fixture_hygiene`/`changelog`/`pdf`/`notice`/`attachment`/`docpdf`/`purchase`/`sales` 在 C13（及 C4/C5/C6/C12）
   落地前没有 crate 形态，`cloudpivot-erp-contract` 只能 `use` 回 `app`（`cloudpivot`）⇒
   `cloudpivot → crates/…-contract → cloudpivot`，`error: cyclic package dependency`
   （同一份依赖同时也是入边来源，环是**双向的**）。即假拆分。
6. **C13 之后抽 = erp 内部再切一刀**：C13 收成一个包后这 27 行出边里 10 行变成包内路径、
   17 行变成 `…-contract → cloudpivot-flow/…-pdf/…` 等下层包的合法依赖，能编过，
   但等于在 `erp` 内部再造兄弟接缝，与 §P2-C.6「`erp` 一个包，不拆子域」直接冲突；
   且 `contract` 只占 `erp` 的 **9 / 353 文件 = 2.6%、8 775 / 296 388 行 = 3.0%**，包边界的增量编译收益
   来自「内核 / 装配层 / `erp`」这一级（`erp` 占全仓 50%），不在 3% 的子域。
7. **搬迁成本落点明确**：C13 整包搬迁时，`contract` 的 34 行接缝（入边 7 + 出边 27）**一行都不用改**
   （同包内 `crate::business::erp::…` 路径不变），28 行内核引用不变，2 个测试模块无需 `#[path]`，
   6 条路由随 `contract_handler.rs` 平移即自注册，3 行 `include_str!` 相对路径不变。
   这是本卡作废的正面理由，不只是「做不了」。

## 4. 两条卡面命令的真实返回（说明「失败」是刻意的）

```bash
$ cd /opt/cloudpivot/backend && cargo build --workspace --offline ; echo build_exit=$?
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.14s
build_exit=0

$ cd /opt/cloudpivot/backend && cargo test -p cloudpivot-erp-contract --no-run --offline ; echo stub_exit=$?
error: package ID specification `cloudpivot-erp-contract` did not match any packages
stub_exit=101
```

第二条失败 = **包里没有这个 crate**（本卡就是要不建它），不是构建回归；工作区仍是 P2-40 落地时的 2 个包：

```bash
$ cd /opt/cloudpivot/backend && ls crates/            # cloudpivot-doc（唯一已抽出的域 crate）
$ cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1
members = 2 ['cloudpivot', 'cloudpivot-doc']           # workspace_root = /opt/cloudpivot/backend
```

卡序仿真未受影响（`--plan-order` 里**没有 `contract` 节点**：`erp` 的子域不单独成步，正是本卡作废的形式化体现）：

```bash
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
C11    cloudpivot-hr                                 7     7   OK        切向：flow(7)
C12    cloudpivot-flow                               2     2   OK        切向：task(2)
C13    cloudpivot-erp                               12    12   OK        切向：task(8), business/task(4)
C14    cloudpivot-task                               0     0   OK
C15    收尾小域（project/taskflow/dashboard）            0     0   OK
全序累计要切：66 行
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

## 5. 复核命令（只读，可对任意机器跑）

```bash
cd /opt/cloudpivot
python3 scripts/verify/crate_split_readiness.py --domain contract --scope erp-sub               # 9 文件/8 775 行，SCC 51，入边 7/出边 17（脚本口径）
python3 scripts/verify/crate_split_readiness.py --domain contract --scope erp-sub --layer-check # 退 1：本步要切 7 条/17 行
grep -rn "erp::contract" --include=*.rs backend/app/src | grep -v "business/erp/contract/" | wc -l   # 12（7 代码 + 5 文档注释）
grep -rn "crate::business::" --include=*.rs backend/app/src/business/erp/contract \
  | grep -v "crate::business::erp::contract" | wc -l                                            # 45 = 出边 27 + 内核 business::shared 15 + 注释 3
grep -rn "crate::business::\(pdf\|notice\|attachment\|docpdf\)" backend/app/src/business/erp/contract | wc -l  # 9（脚本少报的跨域边）
grep -rn "crate::business::erp::{" backend/app/src/business/erp/contract                   # 1 行：purchase+sales 花括号形式
grep -rn "crate::shared::" backend/app/src/business/erp/contract | wc -l                        # 13（内核余下 13：error_code 12 + permission_alias 1）
grep -rnE "#\[nik::(get|post|put|delete|patch)" backend/app/src/business/erp/contract/contract_handler.rs | wc -l  # 期望 6（路由宏）
# 注意：`grep -c "#\[nik::"` 得 12 —— 每条路由都挂 `#[nik::permission]` + `#[nik::get/post/…]` 两个属性
grep -rn "include_str!" backend/app/src/business/erp/linkage/tests_wiring.rs | grep contract        # 3 行源码路径接线（搬家硬耦合）
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md   # 退 0：15 步 66 行，绿
cd backend && cargo build --workspace --offline ; echo $?                                      # exit 0
```

## 6. 保留物 / 后继

- **保留**：`scripts/verify/crate_split_readiness.py`（`--domain` / `--layer-check` / `--plan-order`）、
  `docs/planning/20261113-新卡清单.md` §P2-C、本文（单卡实测，再派发时不必重扫）。
- **后继**：`contract`（6 条路由 / 5 个权限码）随 **C13 `cloudpivot-erp`** 整包搬迁，包内保持
  `business/erp/contract/` 目录分层；34 行接缝、28 行内核引用、3 行 `include_str!` 源码路径、
  2 个测试模块均不改。C13 是 **唯一**允许动 `contract` 编码结构的一步（§P2-C.4 施工单第 2~4 条）。
- **给 C13 施工单加一条**：`erp` 整包搬迁后，`linkage/tests_wiring.rs` 的 3 行
  `include_str!("../../erp/contract/…")` 仍在包内、不必改；但**若**将来把 `linkage` 也拆出去，
  这 3 行是必须一并处理的隐藏依赖 —— 建议 C13 落地时在 `docs/` 里点名，避免下一轮拆包漏看。
- **给工具的口径修正建议（不本卡改，需全员统一重跑）**：`crate_split_readiness.py` 在
  `--scope erp-sub` 下少报两类出边，本文 §2 已按真实账目修正 `contract`（17→27 行、7→13 目标）：
  1. `pattern2 = crate::business::(flow|hr|impexp|changelog|masking|task)` 缺 4 个真实域 ——
     `pdf`、`docpdf`、`attachment`、`notice`（合同 PDF 导出正好依赖这 4 个）；
  2. `pattern = crate::business::erp::(names)` 认不出花括号形式 `crate::business::erp::{a::x, b::y}`
     （`contract_model.rs:1158` 就是一例，漏掉 `purchase`/`sales` 两个目标）。
  影响面：**P2-42 §4 台账里 17 张 `erp` 子域卡的「出边/要切」列同口径偏低**（结论方向不变，全是退 1）。
  建议在 C13 开工前一次性扩名单 + 支持 `erp::{…}`，然后按新口径重跑并回写 §4 台账，避免 18 个子域
  各卡各自记一套数字。
- **本卡零生产代码改动**：无 Rust 文件变更 ⇒ 未跑 `cargo fmt`（无格式面可动；且本机为多 worker 共享
  工作区，跑全仓 `cargo fmt` 会改到别人 WIP）；`git status` 下唯一变化是本文（`.md`）。
