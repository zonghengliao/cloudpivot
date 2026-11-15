# P2-49 报销域抽 crate 收口 —— 作废（`expense` 随 C13 `cloudpivot-erp` 整包搬迁）

> 卡：`t_1153f3a6`（P2-49【P2-C】抽 `erp-expense` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `6a473699`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41-成本域抽crate可行性分析.md` §8（`11f490f2`）、`P2-42-…旧P2-C链实测.md`（`e9a31e96`，§4 已列本卡行）、
> `P2-43-销售域抽crate收口.md`（`939ec719`）、`P2-44-库存域抽crate收口.md`（`810ce2c7`）、
> `P2-45-财务域抽crate收口.md`（`5a2bc55a`）、`P2-46-资产域抽crate收口.md`（`ca11c5dc`）、
> `P2-47-预算域抽crate收口.md`（`4648b330`）、`P2-48-合同域抽crate收口.md`（`6a473699`）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-erp-expense/`，不搬 9 个文件 / 7 444 行，不改任何
`crate::business::erp::expense` import，不动 `app/Cargo.toml`，不动 `backend/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；
卡面验收命令 `cargo test -p cloudpivot-erp-expense` 因此按预期失败（不是回归，见 §4）。

权威依据已入仓（不是本卡临时主张）：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 **288** 行：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同表第 **226** 行 C13 = `cloudpivot-erp`｜`erp`（**一个包，不拆子域**）｜切向仅 `task 8` + `business/task 4` = 12 行；
- 同节第 **292–295** 行：`erp` 内部 51 个节点互相咬住 ⇒ 一个包，包内继续用目录分层；
- `P2-42` §4 的 17 卡台账第 **88 行**（P2-49）= `9 / 7 444`｜SCC 51｜入边 `2（2）`｜出边 `9`｜
  `25 行`｜`退 1`｜落点 `C13 包内` —— 本卡是对该行的逐行复核与入档，**脚本口径数字逐项对得上**（§2）。

## 2. `expense` 的实测（只读、无库依赖，2026-11-15 于 99）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain expense --scope erp-sub
域：expense（scope=erp-sub）
  规模：9 个文件 / 7444 行
  SCC：51 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（50）：asset, budget, contract, cost, customer, erp#root:approval_gate, …,
                           ext:changelog, ext:flow, ext:hr, ext:impexp, ext:task, linkage,
                           payable, period, price, purchase, receivable, report, sales,
                           shipment, stockin, stockout, stocktake, transfer, voucher, year
  回边（留在应用 crate 的代码 → 本域）：2 处代码级
    erp#root:doc_reader_impl       code=  1 all=  1  business/erp/doc_reader_impl.rs:123
    erp#root:doc_reader_impl_tests code=  1 all=  1  business/erp/doc_reader_impl_tests.rs:31
  出边（本域 → 其它）：9 个目标
    budget                 code= 10 all= 10
    ext:flow               code=  4 all=  4
    erp#root:seq           code=  3 all=  3
    linkage                code=  2 all=  2
    voucher                code=  2 all=  2
    erp#root:code_policy   code=  1 all=  1
    erp#root:handler       code=  1 all=  1
    payable                code=  1 all=  1
    period                 code=  1 all=  1
  层次判据：本步要切 9 条 / 25 行 —— 出边指向更下层以外：budget(10), ext:flow(4), erp#root:seq(3),
            linkage(2), voucher(2), erp#root:code_policy(1), erp#root:handler(1), payable(1), period(1)
  结论：不可抽（2 处代码级回边，2 个来源）

$ python3 scripts/verify/crate_split_readiness.py --domain expense --scope erp-sub --check        # 旧口径（只看入边）
exit=1
$ python3 scripts/verify/crate_split_readiness.py --domain expense --scope erp-sub --layer-check  # 层次口径（入边+出边）
exit=1
```

文件口径独立复核（工具口径 = 该目录下全部 `.rs`，含 `mod.rs`；目录无嵌套子目录，两口径一致）：

```bash
$ cd /opt/cloudpivot/backend && wc -l app/src/business/erp/expense/*.rs
  745 app/src/business/erp/expense/expense_bridge.rs
  415 app/src/business/erp/expense/expense_compute.rs
  986 app/src/business/erp/expense/expense_dto.rs
 1758 app/src/business/erp/expense/expense_handler.rs
  178 app/src/business/erp/expense/expense_linkage.rs
  896 app/src/business/erp/expense/expense_linkage_tests.rs
 1348 app/src/business/erp/expense/expense_model.rs
 1094 app/src/business/erp/expense/expense_model_tests.rs
   24 app/src/business/erp/expense/mod.rs
 7444 total
$ find app/src/business/erp/expense -name '*.rs' | wc -l
9
```

### 2.1 出边：脚本口径 25 行 / 9 目标，真实口径 33 行 / 11 目标

**脚本口径 25 行逐行复核**（全部命中，`code` 与 `all` 全等 ⇒ 无文档注释干扰）：

```
expense_dto.rs:688            Vec<crate::business::erp::budget::budget_dto::BudgetWarningRes>  → budget
expense_handler.rs:539     -> Vec<crate::business::erp::budget::budget_dto::BudgetWarningRes>  → budget
expense_handler.rs:549        .map(crate::business::erp::budget::budget_dto::BudgetWarningRes::from) → budget
expense_model_tests.rs:618    vec![crate::business::erp::budget::budget_guard::ExpenseSlotAmount {  → budget
expense_model_tests.rs:623    crate::business::erp::budget::budget_guard::check_for_expense(…)      → budget
expense_model_tests.rs:632    crate::business::erp::budget::budget_guard::occupy_for_expense(…)     → budget
expense_model_tests.rs:643    crate::business::erp::budget::budget_guard::usages_of(…)             → budget
expense_model_tests.rs:649    crate::business::erp::budget::budget_guard::release(…)               → budget
expense_model_tests.rs:653    crate::business::erp::budget::budget_guard::usages_of(…)             → budget
expense_model_tests.rs:725    use crate::business::erp::budget::budget_guard;                       → budget
expense_bridge.rs:45          use crate::business::flow::model as flow_model;                       → ext:flow
expense_bridge.rs:598         crate::business::flow::engine::terminate_instance(…)                  → ext:flow
expense_handler.rs:56         use crate::business::flow::{engine, model as flow_model};            → ext:flow
expense_linkage_tests.rs:53   use crate::business::flow::engine;                                    → ext:flow
expense_dto.rs:230            crate::business::erp::seq::resolve_manual_code(                       → erp#root:seq
expense_dto.rs:232            crate::business::erp::seq::MAX_MANUAL_CODE_LEN,                       → erp#root:seq
expense_handler.rs:51         use crate::business::erp::seq;                                       → erp#root:seq
expense_linkage.rs:50         use crate::business::erp::linkage;                                   → linkage
expense_linkage_tests.rs:52   use crate::business::erp::linkage::spec;                             → linkage
expense_model.rs:25           use crate::business::erp::voucher::SeqSpec;                          → voucher
expense_model_tests.rs:34     use crate::business::erp::voucher;                                   → voucher
expense_handler.rs:42         use crate::business::erp::code_policy;                              → erp#root:code_policy
expense_handler.rs:287        super::super::handler::code_conflict("费用类型", code)               → erp#root:handler ★相对路径
expense_handler.rs:1133       crate::business::erp::payable::payable_payment::amount_number         → payable
expense_handler.rs:1542       use crate::business::erp::period::period_test_support as support;    → period
```

= **脚本口径 25 行 / 9 目标**：`budget` 10、`flow` 4、`seq` 3、`linkage` 2、`voucher` 2、
`code_policy` 1、`handler` 1、`payable` 1、`period` 1。

**脚本对本域少报 8 行 / 2 个目标**（`attachment` 4 行、`notice` 2 行完全没进边图；另 2 行花括号形式
`erp::{…}` 整块未被识别，连已计的 `budget`/`voucher` 也各漏 2 行 —— 沿用 P2-48 §6 已报的两条工具缺陷）。
实际出边 = **33 行 / 11 个目标**：

```
① 花括号形式（正则 `crate::business::erp::(name)` 认不出 `erp::{…}`，且行内没有域名）：
   expense_bridge.rs:41    use crate::business::erp::{
   expense_bridge.rs:42        budget::budget_guard,
   expense_bridge.rs:43        voucher::{self, voucher_multi},
                           }                                   → budget + voucher   [1 行 / 2 目标]
   expense_handler.rs:52   use crate::business::erp::{
   expense_handler.rs:53       budget::budget_guard,
   expense_handler.rs:54       voucher::{self, voucher_multi},
                           }                                   → budget + voucher   [1 行 / 2 目标]
② 不在 ext 名单里的两个域（pattern2 只列 flow|hr|impexp|changelog|masking|task）：
   expense_handler.rs:40   use crate::business::attachment::model::DocKind as AttachDocKind;        → attachment (C4)
   expense_handler.rs:41   use crate::business::attachment::rule::{…RequiredAt as AttachAt};        → attachment (C4)
   expense_linkage_tests.rs:412  crate::business::attachment::test_support::attach_pdf(             → attachment (C4)
   expense_linkage_tests.rs:413  crate::business::attachment::model::DocKind::ExpenseReport,       → attachment (C4)
   expense_linkage.rs:51   use crate::business::notice;                                            → notice (C5)
   expense_linkage.rs:52   use crate::business::notice::dto::{NoticeLevel, NoticeType};            → notice (C5)
```

修正后账目：**erp 包内兄弟 23 行 / 8 目标**（`budget` 12、`voucher` 4、`seq` 3、`linkage` 2、
`code_policy` 1、`handler` 1、`payable` 1、`period` 1 —— 逐目标合计 25 线次，其中 2 行花括号同时
命中 `budget`+`voucher`，故去重后 23 行）+ **跨域 10 行 / 3 个域**（`flow` 4、`attachment` 4、
`notice` 2）＝ **33 行 / 11 目标**。
即：**跨域接缝 10 行占全部出边的 30%**，比脚本口径（4/25 = 16%）更依赖 C4/C5/C12 三个包。
没有一条指向 `system`/`support`/`task`/`hr`/`impexp`，也没有指向 L1 内核（内核引用另计，见下）。

> 口径核对：`grep -rn 'crate::business::' expense/ | grep -v 'crate::business::erp::expense' | wc -l` = **44 行**
> = 出边 32（crate:: 形式，含 2 行花括号各 2 目标）+ 内核 `crate::business::shared::*` **11 行** + 1 行文档注释
> （`expense_model.rs:8` 的 `//! … [`crate::business::shared::state_machine::transition`] …`）。
> 32 + 11 + 1 = 44，逐项对得上；相对路径那 1 行（`super::super::handler`）不在 `crate::` grep 里，
> 加进来才是 33 行出边。

**入边 2 行逐行复核**（脚本 2 来源 / 2 行，全部命中）：

```
business/erp/doc_reader_impl.rs:123        use crate::business::erp::expense::{expense_bridge, expense_dto};
business/erp/doc_reader_impl_tests.rs:31   use crate::business::erp::expense::expense_bridge;
```

= **2 行 / 2 来源**（生产 1 / 测试 1）。另有 **2 行文档注释**提到本域：
`business/erp/voucher/voucher_linkage.rs:175`、`business/erp/linkage/spec.rs:1148`。
故 `grep -rn "erp::expense"` 全仓报 **4 行**（2 代码 + 2 注释），两口径一致。

### 2.2 内核引用 23 行（L1，合法下层，不计切边）

```
crate::business::shared::*        11 行（bridge:46 state_machine、handler:57 state_machine、
                                     handler:427/452 principal、linkage:53 state_machine、
                                     model:26 principal、model:27 scope、
                                     model_tests:941/951 scope、model_tests:1003 principal、:1004 scope）
crate::shared::*                  12 行（bridge:47、dto:29、handler:58 error_code；
                                     linkage_tests:54 test_helpers；
                                     model_tests:77/82/438/980/982/985/987/989 test_helpers+error_code）
```

内核合计 **23 行**（`business::shared` 11 + `shared` 12），全部落在 C1/C3 包内，属于合法下层依赖。

### 2.3 进链接单元靠什么（16 条路由 / 12 个权限码）

`app/src/business/erp/mod.rs:145 pub mod expense;` + `expense_handler.rs` 里 **16 条路由
（32 条 `#[nik::*]` 属性 = 16 `#[nik::permission]` + 16 路由宏）**，路由自注册，无手写 `routes()` 汇编：

```
GET    /business/erp/expense-type/page            business:erp:expense-type:list
GET    /business/erp/expense-type/list            business:erp:expense-type:list
POST   /business/erp/expense-type                 business:erp:expense-type:create
GET    /business/erp/expense-type/next-code       business:erp:expense-type:create
PUT    /business/erp/expense-type/{id}            business:erp:expense-type:update
DELETE /business/erp/expense-type/{id}            business:erp:expense-type:delete
GET    /business/erp/expense/page                 business:erp:expense:list
GET    /business/erp/expense/pay/page             business:erp:expense:pay
GET    /business/erp/expense/{id}                 business:erp:expense:detail
POST   /business/erp/expense                      business:erp:expense:create
PUT    /business/erp/expense/{id}                 business:erp:expense:update
DELETE /business/erp/expense/{id}                 business:erp:expense:delete
POST   /business/erp/expense/{id}/submit          business:erp:expense:submit
POST   /business/erp/expense/{id}/withdraw        business:erp:expense:submit
POST   /business/erp/expense/{id}/cancel          business:erp:expense:cancel
POST   /business/erp/expense/{id}/pay             business:erp:expense:pay
```

`mod.rs` 声明 6 个模块（5 个 `pub`：`expense_bridge`/`expense_compute`/`expense_dto`/`expense_handler`/
`expense_model`，1 个 `pub(crate)`：`expense_linkage`）与 1 个 `#[cfg(test)]` 测试模块（`expense_linkage_tests`）。
**无 `include_str!`**（`grep -rn "include_str!\|include_bytes!" expense/` 0 命中），但有一处
`#[path]`：`expense_model.rs:1325 #[path = "expense_model_tests.rs"]` —— 该测试模块**不在 `mod.rs` 声明**，
由宿主文件以相对路径挂载；整目录平移时 `#[path]` 值不变、仍能解析（这也是 `mod.rs` 文件头注释特意记下的事）。
真库单测 **57 个**（`#[test]`/`#[tokio::test]`：compute 11 + model_tests 20 + dto 9 + linkage_tests 6 + bridge 5 + handler 4），
分布在 6 个文件，搬家时同样不用改路径。

## 3. 为什么不能单独成包

1. **在 51 节点大 SCC 里**（>1 ⇒ 与本批 50 个兄弟不可能拆成两个无环包）。`expense` 两个方向都欠账：
   入边 2 行（2 源）、出边 **33 行 / 11 目标**（脚本口径 25 行 / 9 目标，少报见 §2.1）。
   入边是 18 个子域里**第二少的一档**（只有 `stock` 的 0 更少，`price` 3；对比 `purchase` 61、`budget` 20、
   `contract` 7 —— 见 `P2-42` §4 台账），但出边里有 **10 行跨 C4/C5/C12 三个包**（`flow` 4、`attachment` 4、`notice` 2）——
   这 10 行在 C13（及 C4/C5/C12）落地前没有任何 crate 形态可依赖，解环账一处不少。
   **只看入边会把它误判成叶子**（`--check` 只看入边，2 行 ⇒ 退 1 是「巧合的正确」）。
2. **`expense` 是 ERP 财务侧的「双向咬合体」**：`doc_reader_impl`（文档识别→报销审批回调
   `on_expense_settled`/`guard_self_approval`/`internal_control_creator`）从外面直接 `use` 进来；
   而 `expense` 回头要 `budget`（预算占用/释放/预警，**12 线次最多的一项**）、`voucher`（计提/付款/红冲
   凭证）、`flow`（提交审批 / 作废终止流程）、`payable`（付款金额换算）、`seq`（费用类型编码）、
   `period`（会计期间）、`attachment`（发票/附件必传规则）、`notice`（待付款通知）、`code_policy`（编码策略）
   才能跑完一条报销链路。这是**双向业务咬合**，与本批「erp 内部 51 节点一个 SCC」的结论互为印证。
3. **旧口径会把方向判反**：`--check` 只看入边；真正的判据是**出边**（装配层 → 域 crate 合法，
   域 crate → 单体才是环）。`--layer-check` 对本域同样退 1：脚本口径「本步要切 9 条 / 25 行」，
   **按 §2.1 修正后是 11 条 / 33 行** —— 两种口径都指向不可抽。
4. **两条 `crate::` 图看不见的硬耦合（本卡实测）：**
   - **`include_str!` 读本域源码（P2-48 同款，本域 3 行）**：

     ```
     business/erp/linkage/tests_wiring.rs:628  let hook    = include_str!("../../erp/expense/expense_linkage.rs");
     business/erp/linkage/tests_wiring.rs:657  let handler = include_str!("../../erp/expense/expense_handler.rs");
     business/erp/linkage/tests_wiring.rs:672  let bridge  = include_str!("../../erp/expense/expense_bridge.rs");
     ```

     这 3 行**不进 `crate::` 边图**（不是 import），但一旦把目录搬进 `crates/cloudpivot-erp-expense/src/`，
     编译期直接 `couldn't read …`。而且方向是 `linkage` **读 `expense` 源码**做接线断言
     （`expense_wires_generic_engine_and_excludes_voucher`、`expense_excluded_actions_are_routed_elsewhere`），
     即「抽 crate」的真实改动面 = 33 行出边 **+ 3 行源码路径接线**，且这 3 行还要求反向依赖。
   - **相对路径 `super::super::`（本域独有，前面几张卡没遇到）**：
     `expense_handler.rs:287 super::super::handler::code_conflict("费用类型", code)` —— 这个 `super::super`
     指的是 `business::erp`。抽成 crate 后 `super::super` 变成**新 crate 的 crate root**，
     语义直接错位（找不到 `handler`）。脚本的 `pattern` 认 `super::super::`（所以 `erp#root:handler = 1` 计到了），
     但**只 grep `crate::` 的口径会漏掉这一行** —— 复核时两条都要跑（§5）。
5. **C13 之前抽 = 反向依赖单体**：出边目标 `budget`/`voucher`/`seq`/`linkage`/`code_policy`/`handler`/
   `payable`/`period`/`flow`/`attachment`/`notice` 在 C13（及 C4/C5/C12）落地前没有 crate 形态，
   `cloudpivot-erp-expense` 只能 `use` 回 `app`（`cloudpivot`）⇒ `cloudpivot → crates/…-expense → cloudpivot`，
   `error: cyclic package dependency`（同一份依赖同时也是入边来源，环是**双向的**）。即假拆分。
6. **C13 之后抽 = erp 内部再切一刀**：C13 收成一个包后这 33 行出边里 23 行变成包内路径、
   10 行变成 `…-expense → cloudpivot-flow / …-attachment / …-notice` 的合法依赖，能编过，
   但等于在 `erp` 内部再造兄弟接缝，与 §P2-C.6「`erp` 一个包，不拆子域」直接冲突；
   且 `expense` 只占 `erp` 的 **9 / 353 文件 = 2.55%、7 444 / 296 388 行 = 2.51%**，包边界的增量编译收益
   来自「内核 / 装配层 / `erp`」这一级（`erp` 占全仓 50%），不在 2.5% 的子域。
7. **搬迁成本落点明确**：C13 整包搬迁时，`expense` 的 35 行接缝（入边 2 + 出边 33）**一行都不用改**
   （同包内 `crate::business::erp::…` 与 `super::super::handler` 路径都不变），23 行内核引用不变，
   1 处 `#[path]` 与 1 个测试模块无需改动，16 条路由随 `expense_handler.rs` 平移即自注册。
   唯一要留意的是 `linkage/tests_wiring.rs` 那 3 行 `include_str!`：C13 整包搬迁时相对位置不变、仍可编译；
   **但若将来连 `linkage` 也拆出去，这 3 行必须一并处理**（与 P2-48 §6 给 C13 施工单的建议合并成一条）。
   这是本卡作废的正面理由，不只是「做不了」。

## 4. 两条卡面命令的真实返回（说明「失败」是刻意的）

```bash
$ cd /opt/cloudpivot/backend && cargo build --workspace --offline ; echo build_exit=$?
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 5m 58s
build_exit=0

$ cd /opt/cloudpivot/backend && cargo test -p cloudpivot-erp-expense --no-run --offline ; echo stub_exit=$?
error: package ID specification `cloudpivot-erp-expense` did not match any packages
stub_exit=101
```

第二条失败 = **包里没有这个 crate**（本卡就是要不建它），不是构建回归；工作区仍是 P2-40 落地时的 2 个包：

```bash
$ cd /opt/cloudpivot/backend && ls crates/            # cloudpivot-doc（唯一已抽出的域 crate）
$ cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1 | grep -o '"name":"[a-z-]*"' | head
"name":"cloudpivot"
"name":"cloudpivot-doc"                              # 本地包仅这两个，其余是 registry 依赖
```

卡序仿真未受影响（`--plan-order` 里**没有 `expense` 节点**：`erp` 的子域不单独成步，正是本卡作废的形式化体现）：

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

`cargo fmt --check` 亦为 0（无 Rust 变更 ⇒ 天然无格式面）。

### 4.1 基线复核：`expense` 现有 57 个单测在真库上全绿（证明「不搬」不是把坏账留在原地）

作废的正当性还要求「现状没有待修的坏账」，所以本卡额外跑了一遍 `expense` 的全部真库单测
（99 上跑、PG 连 `172.18.10.145:5432`，库 `cp_p249_expense` 由模板 `cloudpivot_test` 克隆，**用完即 DROP**）：

```bash
$ ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p249_expense" \
    cargo test --lib business::erp::expense --offline -- --test-threads=1
running 57 tests
test result: ok. 57 passed; 0 failed; 0 ignored; 0 measured; 5624 filtered out; finished in 39.99s
```

**中间遇到一次 `1 failed`，查清后确认是模板库脏数据、与本卡零改动无关**（如实记录，不藏）：

```bash
---- business::erp::expense::expense_handler::tests::type_auto_code_generated_when_blank ----
panicked at app/src/business/erp/expense/expense_handler.rs:1654:9:
assertion `left == right` failed: 取号数应与号段推进一致
  left: 29   right: 28          # 期望 before+3，实际 before+4
```

根因：共享模板库 `cloudpivot_test` 里残留 21 行 `type_code LIKE 'ET-%'`（`ET-06` ~ `ET-26`，历次 e2e/drill 留下的），
而 `biz_seq` 的 `ET` 号段 `current_no` 只有 **25** ⇒ 下一次取号 `ET-26` 撞上存量行，
`next_master_code_free` 会**跳号**再取，于是 3 次新建推进了 4。这与迁移种子无关
（`V0242` 只种 8 条 `TRAVEL/MEAL/…/PENALTY` 业务码，不占 `ET-xx`），也与本卡零改动无关。

复现/消除法（**只动自己的克隆库，不碰模板**）：

```bash
psql -d cp_p249_expense -c "DELETE FROM biz_expense_type WHERE type_code LIKE 'ET-%';" \
                         -c "UPDATE biz_seq SET current_no=0 WHERE seq_type='ET';"
# 再跑同一命令 ⇒ 57 passed / 0 failed（上面的结果）
```

> 给团队的提示（不是本卡范围）：模板库的 `ET-xx` 残留在任何「按 yyyy 号段取号」的单测上都会造成跳号误判；
> 建议 `scripts/` 里给模板库加一条 `ET-%` 清理，或让相关单测在 `setup()` 里自检号段与行是否一致。


## 5. 复核命令（只读，可对任意机器跑）

```bash
cd /opt/cloudpivot
python3 scripts/verify/crate_split_readiness.py --domain expense --scope erp-sub               # 9 文件/7 444 行，SCC 51，入边 2/出边 25（脚本口径）
python3 scripts/verify/crate_split_readiness.py --domain expense --scope erp-sub --layer-check # 退 1：本步要切 9 条/25 行
grep -rn "erp::expense" --include=*.rs backend/app/src | grep -v "business/erp/expense/" | wc -l   # 4（2 代码 + 2 文档注释）
grep -rn "crate::business::" --include=*.rs backend/app/src/business/erp/expense \
  | grep -v "crate::business::erp::expense" | wc -l                                            # 44 = 出边 32 + 内核 business::shared 11 + 注释 1
grep -rn "crate::business::\(attachment\|notice\)" backend/app/src/business/erp/expense | wc -l    # 6（脚本少报的跨域边）
grep -rn "crate::business::erp::{" backend/app/src/business/erp/expense                        # 2 行：budget+voucher 花括号形式
grep -rn "super::super::" backend/app/src/business/erp/expense                                  # 1 行：handler:287（crate:: 口径看不见）
grep -rn "crate::shared::" backend/app/src/business/erp/expense | wc -l                         # 12（内核余下 12）
grep -rnE "#\[nik::(get|post|put|delete|patch)" backend/app/src/business/erp/expense/expense_handler.rs | wc -l  # 期望 16（路由宏）
# 注意：`grep -c "#\[nik::"` 得 32 —— 每条路由都挂 `#[nik::permission]` + `#[nik::get/post/…]` 两个属性
grep -rn "include_str!" backend/app/src/business/erp/linkage/tests_wiring.rs | grep expense    # 3 行源码路径接线（搬家硬耦合）
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md   # 退 0：15 步 66 行，绿
cd backend && cargo build --workspace --offline ; echo $?                                      # exit 0
# 基线绿（可选，需真库）：克隆 cp_p249_* 后（ET 残留按 §4.1 清掉）
cd backend && ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p249_expense" \
  cargo test --lib business::erp::expense --offline -- --test-threads=1                        # 57 passed / 0 failed
```

## 6. 保留物 / 后继

- **保留**：`scripts/verify/crate_split_readiness.py`（`--domain` / `--layer-check` / `--plan-order`）、
  `docs/planning/20261113-新卡清单.md` §P2-C、本文（单卡实测，再派发时不必重扫）。
- **后继**：`expense`（16 条路由 / 12 个权限码 / 57 个单测）随 **C13 `cloudpivot-erp`** 整包搬迁，
  包内保持 `business/erp/expense/` 目录分层；35 行接缝、23 行内核引用、1 处 `#[path]`、6 个模块声明、
  3 行 `include_str!` 源码路径均不改。C13 是 **唯一**允许动 `expense` 编码结构的一步（§P2-C.4 施工单第 2~4 条）。
- **给 C13 施工单合并一条**（P2-48 §6 已提 `contract` 版，本卡把主语扩到 `expense`）：
  `erp` 整包搬迁后 `linkage/tests_wiring.rs` 的全部 `include_str!("../../erp/<子域>/…")` 仍在包内、
  不必改；**若**将来把 `linkage` 单独拆出去，这些行是必须一并处理的隐藏依赖 ——
  本卡实测指向 `expense` 的有 3 行（628/657/672），P2-48 指向 `contract` 的有 3 行（802/835/850），
  同文件还有 stockout/transfer/purchase/payable/stocktake/voucher/period/asset/depreciation/year 等 20+ 行。
  建议 C13 落地时在 `docs/` 里一次性点名该文件的**完整清单**，避免下一轮拆包逐卡重复发现。
- **给工具的口径修正建议（不本卡改，需全员统一重跑；与 P2-48 §6 同一份）**：
  `crate_split_readiness.py` 在 `--scope erp-sub` 下少报两类出边，本文 §2.1 已按真实账目修正 `expense`
  （25→33 行、9→11 目标）：
  1. `pattern2 = crate::business::(flow|hr|impexp|changelog|masking|task)` 缺 4 个真实域 ——
     `pdf`、`docpdf`、`attachment`、`notice`（本卡命中 `attachment` 4 行 + `notice` 2 行）；
  2. `pattern = crate::business::erp::(names)` 认不出花括号形式 `crate::business::erp::{a::x, b::y}`
     （本卡 `expense_bridge.rs:41`、`expense_handler.rs:52` 两处，漏掉 `budget`/`voucher` 各 2 行）。
  影响面：**P2-42 §4 台账里 17 张 `erp` 子域卡的「出边/要切」列同口径偏低**（结论方向不变，全是退 1）。
  建议在 C13 开工前一次性扩名单 + 支持 `erp::{…}`，然后按新口径重跑并回写 §4 台账。
- **本卡零生产代码改动**：无 Rust 文件变更 ⇒ `cargo fmt` 无格式面可动（`--check` exit 0）；
  `git status` 下唯一变化是本文（`.md`）。`expense` 现有 57 个真库单测在清掉模板库 `ET-` 残留后
  **全绿**（§4.1），即「作废」不是把坏账留在原地。
