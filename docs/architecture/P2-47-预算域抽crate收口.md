# P2-47 预算域抽 crate 收口 —— 作废（`budget` 随 C13 `cloudpivot-erp` 整包搬迁）

> 卡：`t_10a0eb68`（P2-47【P2-C】抽 `erp-budget` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `ca11c5dc`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41-成本域抽crate可行性分析.md` §8（`11f490f2`）、`P2-42-…旧P2-C链实测.md`（`e9a31e96`，§4 已列本卡行）、
> `P2-43-销售域抽crate收口.md`（`939ec719`）、`P2-44-库存域抽crate收口.md`（`810ce2c7`）、
> `P2-45-财务域抽crate收口.md`（`5a2bc55a`）、`P2-46-资产域抽crate收口.md`（`ca11c5dc`）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-erp-budget/`，不搬 9 个文件 / 7 150 行，不改任何
`crate::business::erp::budget` import，不动 `app/Cargo.toml`，不动 `backend/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；
卡面验收命令 `cargo test -p cloudpivot-erp-budget` 因此按预期失败（不是回归，见 §4）。

权威依据已入仓（不是本卡临时主张）：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 **288** 行：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同表第 **226** 行 C13 = `cloudpivot-erp`｜`erp`（**一个包，不拆子域**）；
- 同节第 **292–295** 行：`erp` 内部 51 个节点互相咬住 ⇒ 一个包，包内继续用目录分层；
- `P2-42` §4 的 17 卡台账第 **P2-47** 行已提前记入本卡实测（`9 / 7 150`｜SCC 51｜入边 20（6）｜出边 5｜
  要切 10 行｜`--layer-check` 退 1｜落点 C13 包内），本卡是对该行的逐行复核与入档。

## 2. `budget` 的实测（只读、无库依赖，2026-11-15 于 99）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain budget --scope erp-sub
域：budget（scope=erp-sub）
  规模：9 个文件 / 7150 行
  SCC：51 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（50）：asset, contract, cost, customer, erp#root:approval_gate, …,
                           expense, ext:changelog, ext:flow, ext:hr, ext:impexp, ext:task,
                           linkage, payable, period, price, purchase, receivable, report,
                           sales, shipment, stockin, stockout, stocktake, transfer, voucher, year
  回边（留在应用 crate 的代码 → 本域）：20 处代码级
    erp#root:doc_reader_impl       code=  1 all=  1  business/erp/doc_reader_impl.rs:120
    erp#root:doc_reader_impl_tests code=  1 all=  1  business/erp/doc_reader_impl_tests.rs:27
    erp#root:dto                   code=  1 all=  1  business/erp/dto.rs:2568
    erp#root:requisition_bridge    code=  2 all=  3  requisition_bridge.rs:31, :205
    expense                        code= 10 all= 10  expense_dto.rs:688, expense_handler.rs:539, :549,
                                                       expense_model_tests.rs:618, :623, :632, :643, :649, :653, :725
    purchase                       code=  5 all=  5  purchase_bridge.rs:265, :267,
                                                       purchase_handler.rs:46, :573, :585
  出边（本域 → 其它）：5 个目标
    ext:flow               code=  3 all=  3
    voucher                code=  3 all=  3
    erp#root:model         code=  2 all=  2
    erp#root:dto           code=  1 all=  1
    purchase               code=  1 all=  1
  层次判据：本步要切 5 条 / 10 行 —— 出边指向更下层以外：ext:flow(3), voucher(3),
            erp#root:model(2), erp#root:dto(1), purchase(1)
  结论：不可抽（20 处代码级回边，6 个来源）

$ python3 scripts/verify/crate_split_readiness.py --domain budget --scope erp-sub --check        # 旧口径（只看入边）
exit=1
$ python3 scripts/verify/crate_split_readiness.py --domain budget --scope erp-sub --layer-check  # 层次口径（入边+出边）
exit=1
```

文件口径独立复核（工具口径 = 该目录下全部 `.rs`，含 `mod.rs`；目录无嵌套子目录，两口径一致）：

```bash
$ cd /opt/cloudpivot/backend && wc -l app/src/business/erp/budget/*.rs
  395 app/src/business/erp/budget/budget_bridge.rs
  904 app/src/business/erp/budget/budget_dto.rs
  331 app/src/business/erp/budget/budget_dto_tests.rs
 1712 app/src/business/erp/budget/budget_guard.rs
  884 app/src/business/erp/budget/budget_guard_tests.rs
  968 app/src/business/erp/budget/budget_handler.rs
 1232 app/src/business/erp/budget/budget_model.rs
  710 app/src/business/erp/budget/budget_model_tests.rs
   14 app/src/business/erp/budget/mod.rs
 7150 total
$ find app/src/business/erp/budget -name '*.rs' | wc -l
9
```

**出边 10 行逐行复核**（脚本 5 目标 / 10 行，逐行对得上，无花括号 `use crate::business::erp::{…}` 形式漏计 ——
已用 `grep -rn "{budget\|, budget\|super::budget\|budget::"` 反查，命中项全是名为 `budget` 的**局部变量**，
不是 import）：

```
app/src/business/erp/budget/budget_bridge.rs:34    use crate::business::flow::model as flow_model;
app/src/business/erp/budget/budget_bridge.rs:35    use crate::business::flow::settle_guard;
app/src/business/erp/budget/budget_handler.rs:50   use crate::business::flow::{engine, model as flow_model};
app/src/business/erp/budget/budget_dto.rs:30       use crate::business::erp::dto::MATERIAL_TYPES;
app/src/business/erp/budget/budget_guard.rs:1022   order: &crate::business::erp::model::PurchaseOrderRow,
app/src/business/erp/budget/budget_guard.rs:1309   order: &crate::business::erp::model::PurchaseOrderRow,
app/src/business/erp/budget/budget_guard.rs:1042   crate::business::erp::voucher::user_dept_of(ex, creator)
app/src/business/erp/budget/budget_guard.rs:1058   crate::business::erp::purchase::purchase_model::items_of(order.id)
app/src/business/erp/budget/budget_model.rs:32     use crate::business::erp::voucher::{self, SeqSpec};
app/src/business/erp/budget/budget_model_tests.rs:205  crate::business::erp::voucher::format_doc_no(…)
```

= **10 行 / 5 目标**。其中 7 行是 `erp::*` 兄弟（`voucher` 3、`erp` 根 `model` 2、`erp` 根 `dto` 1、
`purchase` 1），3 行是跨域 `business::flow`（C12）—— **没有一条指向 `system`/`support`/`task`/`hr`/`impexp`**，
也没有指向 L1 内核（内核引用另计，见下）。这是本卡与 `purchase`（81 行）、`sales`（41 行）的体量差别所在。

**入边 20 行逐行复核**（脚本 6 来源 / 20 行，全部命中）：

```
business/erp/doc_reader_impl.rs:120                 use crate::business::erp::budget::{budget_bridge, budget_dto};
business/erp/doc_reader_impl_tests.rs:27            use crate::business::erp::budget::budget_bridge;
business/erp/dto.rs:2568                            pub budget_warnings: Vec<…budget::budget_dto::BudgetWarningRes>
business/erp/requisition_bridge.rs:31               use …erp::budget::budget_guard::{self, BudgetVerdict, ControlMode, Slot};
business/erp/requisition_bridge.rs:205              …erp::budget::budget_model::resolve_for_po(
business/erp/expense/expense_dto.rs:688             pub budget_warnings: Vec<…budget::budget_dto::BudgetWarningRes>
business/erp/expense/expense_handler.rs:539         ) -> Vec<…budget::budget_dto::BudgetWarningRes> {
business/erp/expense/expense_handler.rs:549         .map(…budget::budget_dto::BudgetWarningRes::from)
business/erp/expense/expense_model_tests.rs:618     …budget::budget_guard::ExpenseSlotAmount {
business/erp/expense/expense_model_tests.rs:623     …budget::budget_guard::check_for_expense(db(), dept, "2026-11", &slots)
business/erp/expense/expense_model_tests.rs:632     …budget::budget_guard::occupy_for_expense(
business/erp/expense/expense_model_tests.rs:643     …budget::budget_guard::usages_of(db(), "EXPENSE_REPORT", id)
business/erp/expense/expense_model_tests.rs:649     …budget::budget_guard::release(db(), "EXPENSE_REPORT", id, "REJECTED")
business/erp/expense/expense_model_tests.rs:653     …budget::budget_guard::usages_of(db(), "EXPENSE_REPORT", id)
business/erp/expense/expense_model_tests.rs:725     use crate::business::erp::budget::budget_guard;
business/erp/purchase/purchase_bridge.rs:265        crate::business::erp::budget::budget_guard::release(
business/erp/purchase/purchase_bridge.rs:267        crate::business::erp::budget::budget_guard::SOURCE_PURCHASE,
business/erp/purchase/purchase_handler.rs:46       use crate::business::erp::budget::budget_guard;
business/erp/purchase/purchase_handler.rs:573      ) -> Vec<…budget::budget_dto::BudgetWarningRes> {
business/erp/purchase/purchase_handler.rs:585      .map(…budget::budget_dto::BudgetWarningRes::from)
```

= **20 行 / 6 来源**。`requisition_bridge.rs` 另有第 3 处（`:171`）是**文档注释**
（``/// 科目解析顺序见 [`crate::business::erp::budget::budget_model::resolve_for_po`]``），不计代码级 ⇒
严格 20 行，与脚本 `code/all = 2/3` 一致。生产代码 12 行 / 测试代码 8 行
（`expense_model_tests` 7 + `doc_reader_impl_tests` 1）。

另有 **20 行落在 L1 内核**（C1/C3 内的合法下层依赖，按层次口径不计入切边）：
`business::shared::state_machine` 3、`business::shared::scope` 4、`business::shared::principal` 2、
`shared::error_code` 3、`shared::test_helpers` 8。**没有** `attachment` / `notice` 引用。

**进链接单元靠什么**：`app/src/business/erp/mod.rs:119 pub mod budget;` +
`budget_handler.rs` 里 **12 条 `#[nik::*]` 路由宏**（8 个权限码复用：`budget:mapping` 4 条、
`budget:list` 2 条、`create`/`update`/`delete`/`submit`/`adjust`/`report` 各 1 条），
路由自注册，无手写 `routes()` 汇编：

```
GET  /business/erp/budget/page            GET  /business/erp/budget/{id}
POST /business/erp/budget                 PUT  /business/erp/budget/{id}
DEL  /business/erp/budget/{id}            POST /business/erp/budget/{id}/submit
POST /business/erp/budget/{id}/adjust     GET  /business/erp/budget/report
GET  /business/erp/budget/mapping/page    POST /business/erp/budget/mapping
PUT  /business/erp/budget/mapping/{id}    DEL  /business/erp/budget/mapping/{id}
```

`mod.rs` 另有 3 个 `#[path]` 测试子模块（`budget_dto_tests`/`budget_guard_tests`/`budget_model_tests`
分别挂在同名文件下）——**搬迁时这 3 行 `#[path]` 值一字不用改**（同目录移动），这也是整目录平移的安全面。

## 3. 为什么不能单独成包

1. **在 51 节点大 SCC 里**（>1 ⇒ 与本批 50 个兄弟不可能拆成两个无环包）。`budget` **两个方向都欠账**：
   入边 20 行（6 源）、出边 10 行（5 目标）——是本批 `erp` 子域里**入边最多**的一个
   （对比 `asset` 17、`purchase` 61/81、`sales` 7/41）。解环账 ≥ 30 行跨文件改动，换来的只是把
   `erp` 296 388 行切成第 19 块。
2. **双向咬合是业务语义决定的，不是历史包袱**：`expense` 10 行、`purchase` 5 行是**预算的直接消费者**
   （费用报销 `check_for_expense`/`occupy_for_expense`/`release`；采购订单 `check_for_purchase`/
   `occupy_for_purchase`/`release`），而 `budget` 回头又要 `purchase::purchase_model::items_of`
   与 `voucher::{user_dept_of, format_doc_no, SeqSpec}` 才能判额和出单 —— 预算正是「挂在别人单据上的」
   管控层，**天然不可能自成一片叶子**。这一点与本批「erp 内部 51 节点一个 SCC」的结论互为印证。
3. **旧口径会把方向判反**：`--check` 只看入边，`budget` 入边 20 行 ⇒ 退 1 是「巧合的正确」；
   真正的判据是**出边**（装配层 → 域 crate 合法，域 crate → 单体才是环）。`--layer-check` 对本域同样退 1：
   本步要切 **5 条 / 10 行**，全部指向 `erp` 包内兄弟或 C12 `flow`。
4. **C13 之前抽 = 反向依赖单体**：出边目标 `voucher`/`model`/`dto`/`purchase` 在 C13 落地前没有 crate
   形态，`cloudpivot-erp-budget` 只能 `use` 回 `app`（`cloudpivot`）⇒ `cloudpivot → crates/…-budget →
   cloudpivot`，`error: cyclic package dependency`（同一份依赖同时也是入边来源，环是**双向的**）。即假拆分。
5. **C13 之后抽 = erp 内部再切一刀**：C13 收成一个包后这 10 行出边变成 `…-budget → cloudpivot-erp`，
   能编过，但等于在 `erp` 内部再造兄弟接缝，与 §P2-C.6「`erp` 一个包，不拆子域」直接冲突；
   且 `budget` 只占 `erp` 的 **9 / 353 文件 = 2.5%、7 150 / 296 388 行 = 2.4%**，包边界的增量编译收益
   来自「内核 / 装配层 / `erp`」这一级（`erp` 占全仓 50%），不在 2.4% 的子域。
6. **搬迁成本落点明确**：C13 整包搬迁时，`budget` 的 30 行接缝（入边 20 + 出边 10）**一行都不用改**
   （同包内 `crate::business::erp::…` 路径不变），3 个 `#[path]` 测试模块值不变，12 条路由随
   `budget_handler.rs` 平移即自注册。这是本卡作废的正面理由，不只是「做不了」。

## 4. 两条卡面命令的真实返回（说明「失败」是刻意的）

```bash
$ cd /opt/cloudpivot/backend && cargo build --workspace --offline ; echo build_exit=$?
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.13s
build_exit=0

$ cd /opt/cloudpivot/backend && cargo test -p cloudpivot-erp-budget --no-run --offline ; echo stub_exit=$?
error: package ID specification `cloudpivot-erp-budget` did not match any packages
stub_exit=101
```

第二条失败 = **包里没有这个 crate**（本卡就是要不建它），不是构建回归；工作区仍是 P2-40 落地时的 2 个包：

```bash
$ cd /opt/cloudpivot/backend && ls crates/            # cloudpivot-doc（唯一已抽出的域 crate）
$ cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1
members = 2 ['cloudpivot', 'cloudpivot-doc']
```

卡序仿真未受影响（`--plan-order` 里**没有 `budget` 节点**：`erp` 的子域不单独成步，正是本卡作废的形式化体现）：

```bash
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
…
C12    cloudpivot-flow         2    2   OK
C13    cloudpivot-erp         12   12   OK        切向：task(8), business/task(4)
C14    cloudpivot-task         0    0   OK
C15    收尾小域（project/taskflow/dashboard） 0 0 OK
全序累计要切：66 行
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

## 5. 复核命令（只读，可对任意机器跑）

```bash
cd /opt/cloudpivot
python3 scripts/verify/crate_split_readiness.py --domain budget --scope erp-sub               # 9 文件/7 150 行，SCC 51，入边 20/出边 10
python3 scripts/verify/crate_split_readiness.py --domain budget --scope erp-sub --layer-check # 退 1：本步要切 5 条/10 行
grep -rn "erp::budget" --include=*.rs backend/app/src | grep -v "business/erp/budget/" \
  | grep -v ':///' | wc -l                                                                   # 期望 20（入边；含 1 行文档注释时为 21）
grep -rn "crate::business::" --include=*.rs backend/app/src/business/erp/budget \
  | grep -v "crate::business::erp::budget"                                                    # 19 行 = 出边 10 + 内核 business::shared 9（scope4/state_machine3/principal2）
grep -rn "crate::shared::\(error_code\|test_helpers\)" backend/app/src/business/erp/budget | wc -l  # 11（内核余下 11 行：error_code 3 + test_helpers 8）
grep -rnE "#\[nik::(get|post|put|delete|patch)" backend/app/src/business/erp/budget/budget_handler.rs | wc -l  # 期望 12（路由宏）
# 注意：`grep -c "#\[nik::"` 得 24 —— 每条路由都挂 `#[nik::permission]` + `#[nik::get/post/…]` 两个属性
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md   # 退 0：15 步 66 行，绿
cd backend && cargo build --workspace --offline ; echo $?                                    # exit 0
```

## 6. 保留物 / 后继

- **保留**：`scripts/verify/crate_split_readiness.py`（`--domain` / `--layer-check` / `--plan-order`）、
  `docs/planning/20261113-新卡清单.md` §P2-C、本文（单卡实测，再派发时不必重扫）。
- **后继**：`budget`（12 条路由 / 8 个权限码）随 **C13 `cloudpivot-erp`** 整包搬迁，包内保持
  `business/erp/budget/` 目录分层；30 行 `erp::*` 接缝、3 个 `#[path]` 测试模块、20 行 L1 内核引用
  均不改。C13 是 **唯一**允许动 `budget` 编码结构的一步（§P2-C.4 施工单第 2~4 条）。
- **本卡零生产代码改动**：无 Rust 文件变更 ⇒ 未跑 `cargo fmt`（无格式面可动；且本机为多 worker 共享
  工作区，跑全仓 `cargo fmt` 会改到别人 WIP）；`git status` 下唯一变化是本文（`.md`）。
