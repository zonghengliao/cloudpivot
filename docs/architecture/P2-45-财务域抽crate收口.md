# P2-45 财务域抽 crate 收口 —— 作废（`erp-finance` 是菜单目录名，不是代码单元；财务能力随 C13 `cloudpivot-erp` 整包）

> 卡：`t_28973dee`（P2-45【P2-C】抽 `erp-finance` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41-成本域抽crate可行性分析.md` §8（`11f490f2`）、
> `P2-42-采购域抽crate收口与旧P2-C链实测.md`（`e9a31e96`）、`P2-43-销售域抽crate收口.md`（`939ec719`）、
> `P2-44-库存域抽crate收口.md`（`810ce2c7`）

## 1. 本卡处置：作废（不建 crate、不动生产代码）

不建 `backend/crates/cloudpivot-erp-finance/`，不搬任何 `.rs`，不改任何 import，不动 `app/Cargo.toml`。
卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；卡面验收命令
`cargo test -p cloudpivot-erp-finance` 因此按预期失败（不是回归，见 §4）。

权威依据已入仓（不是本卡临时主张）：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 **288** 行：**P2-42 ~ P2-58 作废**（`erp` 内部 51 节点 SCC，
  子域不可能单独成包），新位置 C3 ~ C15；
- 第 **226** 行 C13 = `cloudpivot-erp`｜`erp`（**一个包，不拆子域**）；
- 第 **292–295** 行：`erp` 内部 51 个节点互相咬住 ⇒ 一个包，包内继续用目录分层；包边界收益来自
  「内核 / 装配层 / `erp`」这一级。

## 2. 卡面目标对不上任何代码单元（实测，非推断）

仓库里**从来没有** `business/erp/finance/` 这个目录（`git log --all --diff-filter=AD -- '*finance*'`
只命中文档/修复类提交，无目录增删）：

```bash
$ cd /opt/cloudpivot/backend && ls -d app/src/business/erp/finance app/src/business/finance
ls: cannot access 'app/src/business/erp/finance': No such file or directory
ls: cannot access 'app/src/business/finance': No such file or directory

$ grep -rn "business::finance\|erp::finance\|business/erp/finance" app/src
（0 行）
```

`erp` 的 24 个子目录（`asset budget commission contract cost customer depreciation expense linkage
lot payable period price purchase receivable report sales shipment stock stockin stockout stocktake
transfer voucher year`）里**没有** `finance`。

**唯一能对上 `erp-finance` 的实体是菜单目录**（前端导航分组，不是 Rust 模块）：

```sql
-- backend/app/migrations/business/V0154__erp_menu_reorg_business_lines.up.sql:37
('20000000-0000-0000-0000-000000000e02', '财务管理', 1, 'erp-finance',
 '/business/erp/finance-mgmt', NULL, …0940, 30, 'money', true)
-- V0156 把它提为顶级（parent_id=NULL, sort_order=72）
```

即 `sys_menu.name='erp-finance'`、`title='财务管理'`、`menu_type=1`（目录）。它下面挂的页面横跨
**至少 6 个子域 + `erp` 根**：`report/`（V0183 资产负债表、V0184 利润表、V0185 现金流量表）、
`budget/`（V0227）、`asset/`（V0229）、`expense/`（V0242）、`payable/`（V0255）…… 菜单分组与代码目录
**不是一一对应**，所以「把 `erp-finance` 抽成一个 crate」在目录层面无落点；唯一可执行的解读是
「把财务相关子域打成一个包」，见 §3.3 实测（同样退 1）。

## 3. 财务组实测（只读、无库依赖，2026-11-15 于 99）

工具：`scripts/verify/crate_split_readiness.py`（口径：目录下全部 `.rs` 递归计数；出边 = 本域引用
`crate::…` 的**代码级**行数，`all` 含注释）。

### 3.1 六个财务子域：全在 51 节点 SCC，`--layer-check` 全退 1

| 子域 | 文件 | 行 | SCC | 回边（代码/来源） | 出边（目标/行） | `--layer-check` |
|:--|:--:|--:|:--:|:--:|:--:|:--:|
| `voucher` | 8 | 12 298 | 51 | 100 / 35 | 10 / 25 | **1** |
| `period` | 9 | 7 857 | 51 | 34 / 18 | 8 / 19 | **1** |
| `year` | 8 | 5 049 | 51 | 3 / 1 | 4 / 22 | **1** |
| `report` | 4 | 5 428 | 51 | 9 / 6 | 4 / 8 | **1** |
| `payable` | 7 | 10 131 | 51 | 9 / 7 | 10 / 30 | **1** |
| `receivable` | 4 | 5 360 | 51 | 11 / 7 | 10 / 23 | **1** |
| **小计** | **40** | **46 123** | — | **166** | **127** | — |

占 `erp` 的 **40 / 353 文件 = 11.3%**、**46 123 / 296 388 行 = 15.6%**；连同 `expense`（9 / 7 444）
与 `budget`（9 / 7 150，被 `expense` 反向咬住）财务组 8 个目录合计 **58 文件 / 60 717 行 = 20.5%** ——
是 `erp` 内**最大**的子域组。

`--layer-check` 逐条（命令 + 真实 exit code）：

```bash
$ for d in voucher period year report payable receivable; do
    python3 scripts/verify/crate_split_readiness.py --domain $d --scope erp-sub --layer-check >/dev/null
    echo "$d exit=$?"; done
voucher exit=1 period exit=1 year exit=1 report exit=1 payable exit=1 receivable exit=1
```

各步「要切」的出边清单（原样摘录）：

```
voucher      10 条 / 25 行：erp#root:ledger(8), stockin(4), ext:flow(4), linkage(2), erp#root:approval_gate(2),
                           erp#root:currency(1), erp#root:serial_model(1), receivable(1), erp#root:model(1),
                           erp#root:fixture_hygiene(1)
period        8 条 / 19 行：erp#root:model(4), voucher(4), year(3), linkage(3), cost(2), erp#root:dto(1),
                           erp#root:fx_revaluation(1), erp#root:reminder(1)
year          4 条 / 22 行：period(12), voucher(6), linkage(3), erp#root:opening(1)
report        4 条 /  8 行：voucher(3), ext:impexp(2), erp#root:income_statement(2), erp#root:opening(1)
payable      10 条 / 30 行：voucher(8), ext:flow(5), erp#root:approval_gate(5), erp#root:fixture_hygiene(3),
                           linkage(3), ext:impexp(2), stockin(1), report(1), erp#root:form_snapshot(1),
                           erp#root:model(1)
receivable   10 条 / 23 行：stockout(10), voucher(4), erp#root:currency(2), period(1), payable(1), report(1),
                           sales(1), shipment(1), stockin(1), erp#root:model(1)
```

文件口径独立复核（`wc -l` 递归）：注意 `payable` 有嵌套目录 `payable_payment/tests/`，直接
`wc -l app/src/business/erp/payable/*.rs` 只得 9 654 行（漏 477 行），**以递归 10 131 为准**：

```bash
$ for d in voucher period year report payable receivable; do
    echo "$d $(find app/src/business/erp/$d -name '*.rs' | wc -l) files / $(find app/src/business/erp/$d -name '*.rs' -exec cat {} + | wc -l) lines"; done
voucher 8 files / 12298 lines      period 9 / 7857       year 8 / 5049
report 4 / 5428                    payable 7 / 10131     receivable 4 / 5360
```

### 3.2 单独抽任一个：C13 前只能反向依赖单体

以 `voucher`（财务组里最"核心"的一个）为例，出边第一站是 `erp#root:ledger`(8) —— `ledger` 是
`app/src/business/erp/ledger.rs`（`erp` 的**根文件**，不在任何子目录）。C13 落地前它没有 crate 形态，
`cloudpivot-erp-voucher` 想 `use` 它只能回头依赖 `app`（`cloudpivot` 本身）⇒
`cloudpivot → crates/…-voucher → cloudpivot` 直接 `error: cyclic package dependency`，即"假拆分"
（§P2-C.5 已论证这条路不采用）。六个子域无一例外都引用了包内兄弟或 `erp` 根文件。

### 3.3 即使把六个子域打成一个 `cloudpivot-erp-finance`：仍有 82 条组外出边

按卡的 crate 名（`erp-finance` 含连字符、对应一个包）把 `voucher period year report payable
receivable` 合并计算：组内互引 45 条（`voucher` 25、`period` 13、`year` 3、`report` 2、`payable` 1、
`receivable` 1）抽包后变内部边，但**组外仍有 20 个目标 / 82 条代码级出边**：

```
linkage 11   stockout 10   ext:flow 9   erp#root:ledger 8   erp#root:approval_gate 7
erp#root:model 7   stockin 6   erp#root:fixture_hygiene 4   ext:impexp 4   erp#root:currency 3
cost 2   erp#root:opening 2   erp#root:income_statement 2   erp#root:serial_model 1
erp#root:dto 1   erp#root:fx_revaluation 1   erp#root:reminder 1   erp#root:form_snapshot 1
sales 1   shipment 1
```

入边同样跨包：组外 **121 条 / 33 个来源**指向组内（`asset budget commission cost depreciation
expense linkage price purchase sales stockin stockout stocktake transfer` + `erp` 根的
`aux_tests cancel_cascade cash_flow currency_admin_handler doc_reader_impl fx_revaluation
income_statement invoice ledger model_tests opening pflow3b_tests receipt receipt_refund reminder
reminder_tests serial_model statement`）。

⇒ 组包同样**不成无环包**：`--layer-check` 的判据（出边必须全落 L1 内核）不满足。金额上它还要先付
82 行解环账 = §P2-C.6 明确拒绝的「erp 内部再切一刀」。

> 口径说明：工具的 `--domain` 只接受单个域、没有 group 模式，上面的「组外 82 条 / 组内 45 条 /
> 组外回边 121 条」是按六个域各自 `--json` 的 `outbound` / `inbound` 明细在脚本里求和、再扣掉
> 组内互引得出的（脚本见复核命令段末的复现方式：`--json` 逐域取 `outbound[].to/code`）。
> 不是新工具输出，但每条都能回到 §3.1 逐域清单对上账。

## 4. 两条卡面命令的真实返回（说明"失败"是刻意的）

```bash
$ cd /opt/cloudpivot/backend && timeout 1200 cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.22s
build_exit=0

$ cd /opt/cloudpivot/backend && cargo test -p cloudpivot-erp-finance --no-run
error: package ID specification `cloudpivot-erp-finance` did not match any packages
test_exit=101
```

第二条失败 = **包里没有这个 crate**（本卡就是要不建它），不是构建回归；工作区仍是 P2-40 落地时的 2 个包：

```bash
$ cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1
workspace members: ['…/backend/app#cloudpivot@0.1.0', '…/backend/crates/cloudpivot-doc#0.1.0']
packages: [('cloudpivot','0.1.0'), ('cloudpivot-doc','0.1.0')]
```

## 5. 复核命令（只读，可对任意机器跑）

```bash
cd /opt/cloudpivot
ls -d backend/app/src/business/erp/finance                      # 期望 No such file or directory
grep -rn "business::finance\|erp::finance" backend/app/src      # 期望 0 行
for d in voucher period year report payable receivable; do
  python3 scripts/verify/crate_split_readiness.py --domain $d --scope erp-sub --layer-check >/dev/null
  echo "$d exit=$?"; done                                       # 期望六个都是 exit=1
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
                                                                # 退 0：15 步 66 行，判定绿，不含 P2-45
cd backend && cargo build --workspace --offline                  # exit 0
cargo test -p cloudpivot-erp-finance --no-run                    # exit 101（未建该 crate，符合预期）
```

§3.3 的组包账复现（逐域取 JSON，扣组内互引；工具无 group 模式）：

```bash
cd /opt/cloudpivot && for d in voucher period year report payable receivable; do
  python3 scripts/verify/crate_split_readiness.py --domain $d --scope erp-sub --json; done > /tmp/fin6.jsonl
# 合并规则：outbound 的 to 落在 {voucher,period,year,report,payable,receivable} 内 ⇒ 内部边，否则计出边；
# inbound 的 from 落在组内 ⇒ 内部回边，否则计真回边。期望：组外 20 目标/82 行、组内 6 目标/45 行、真回边 121 条/33 源。
```

`--plan-order` 本次输出未变：C1 10 · C3 6 · C4 1 · C5 3 · C6 4 · C7 7 · C9 13 · C10 1 · C11 7 ·
C12 2 · **C13 12** · 其余 0，全序累计 **66 行**，判定绿，`exit=0`；卡序覆盖 28 个节点，
未进卡序（留在单体）`app#root`、`business#root` —— **没有 `finance` 节点**。

## 6. 保留物 / 后继

- **保留**：`scripts/verify/crate_split_readiness.py`（`--domain / --layer-check / --plan-order`）、
  `docs/planning/20261113-新卡清单.md` §P2-C、本文（单卡实测，被再派发时不必重扫）。
- **后继**：财务能力（凭证 `voucher`、期间 `period`、年结 `year`、报表 `report`、应付 `payable`、
  应收 `receivable`，外加 `expense`/`budget` 与 `erp` 根 `ledger.rs` / `income_statement.rs` /
  `cash_flow.rs` / `opening.rs` / `fx_revaluation.rs`）随 **C13 `cloudpivot-erp`** 整包搬迁，
  包内保持 `business/erp/<子域>/` 目录分层；`--layer-check` 与三层 import 不用改。
- **本卡零生产代码改动**：无 Rust 文件变更 ⇒ 未跑 `cargo fmt`（无格式面；且本机为多 worker
  共享工作区，跑全仓 `cargo fmt` 会改到别人的 WIP）；`git status` 下唯一变化是本文（`.md`）。
