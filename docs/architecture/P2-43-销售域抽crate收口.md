# P2-43 销售域抽 crate 收口 —— 作废（`sales` 随 C13 `cloudpivot-erp` 整包搬迁）

> 卡：`t_d79c9064`（P2-43【P2-C】抽 `erp-sales` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 旧链实测总表：`docs/architecture/P2-42-采购域抽crate收口与旧P2-C链实测.md` §4（提交 `e9a31e96`）
> 前序同类收口：`P2-41-成本域抽crate可行性分析.md` §8（`11f490f2`）

## 1. 本卡处置：作废（不建 crate、不动生产代码）

不建 `backend/crates/cloudpivot-erp-sales/`，不搬 11 个文件 / 13 247 行，不改任何
`crate::business::erp::sales` import，不动 `app/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；
卡面验收命令 `cargo test -p cloudpivot-erp-sales` 因此按预期失败（不是回归，见 §4）。

权威依据已入仓（不是本卡临时主张）：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 288 行：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
  第 226 行 C13 = `cloudpivot-erp`「`erp`（**一个包，不拆子域**）」。
- §P2-C.6 收尾段（第 292–295 行）：`erp` 内部 51 个节点互相咬住 ⇒ 一个包，包内继续用目录分层。

## 2. `sales` 的实测（只读，无库依赖，2026-11-15 于 99）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain sales --scope erp-sub
域：sales（scope=erp-sub）
  规模：11 个文件 / 13247 行
  SCC：51 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（50）：asset, budget, contract, cost, customer, erp#root:approval_gate, …,
                           linkage, payable, period, price, purchase, receivable, report,
                           shipment, stockin, stockout, stocktake, transfer, voucher, year
  回边（留在应用 crate 的代码 → 本域）：7 处代码级
    erp#root:doc_reader_impl 1 · erp#root:doc_reader_impl_tests 1 · erp#root:return_pdf 2
    receivable 1 · shipment 2
  出边（本域 → 其它）：19 个目标
    stockin 7 · ext:flow 5 · erp#root:model_base 4 · erp#root:serial_model 3 · erp#root:credit 3
    receivable 3 · erp#root:approval_gate 2 · erp#root:currency 2 · stockout 2 · contract 1
    customer 1 · shipment 1 · erp#root:form_snapshot 1 · erp#root:inventory_writer 1
    erp#root:invoice 1 · period 1 · erp#root:tax_rate 1 · voucher 1 · erp#root:model 1
  层次判据：本步要切 19 条 / 41 行
  结论：不可抽（7 处代码级回边，5 个来源）

$ python3 scripts/verify/crate_split_readiness.py --domain sales --scope erp-sub --layer-check
exit=1
```

文件口径独立复核（工具口径 = 这个目录下全部 `.rs`，含 `mod.rs`）：

```bash
$ cd /opt/cloudpivot/backend && find app/src/business/erp/sales -name '*.rs' | wc -l
11
$ find app/src/business/erp/sales -name '*.rs' -print0 | xargs -0 wc -l | tail -1
 13247 total
```

> 路径注意：P2-40（`35a306be`）把 bin 包搬进 `backend/app/`，源码实际在
> `backend/app/src/business/erp/sales/`，**不是** `backend/src/business/erp/sales/`（旧卡面口径）。

## 3. 为什么 41 行「付掉」也换不到一个小包

1. **41 行里 36 行是 `erp` 包内兄弟**：19 个出边目标中只有 `ext:flow`（5 行）是跨域，
   其余 18 个目标 / **36 行**（`stockin 7`、`model_base 4`、`serial_model 3`、`credit 3`、`receivable 3`、
   `approval_gate 2`、`currency 2`、`stockout 2`、`contract/customer/shipment/form_snapshot/
   inventory_writer/invoice/period/tax_rate/voucher/model 各 1`）全在 `erp` 包内 ——
   单抽 `sales` 就得把这 36 行提前固化成跨包 API，而它们只可能在 C13 内部消化。
2. **那 5 行 `ext:flow` 在新卡序里不用付**：`flow` 是 C12，早于 C13；`--plan-order` 实测
   C13 本步只切 **12 行（task 8、business/task 4）**，`erp → flow` 已属合法下行边。
   即 `sales` 的"解环账"在新卡序里是 **0** —— 付不出任何东西，只多一次搬迁。
3. **同一个 51 节点 SCC**：先抽 `sales` 后，`purchase`/`stockin`/`voucher`/`cost` 再抽都顶在
   `A → sales(包外) → … → A` 的间接环上；先抽哪张都一样（同 P2-42 §3、P2-41 §7）。
4. **收益方向不对**：`erp` 占全仓 50%，包边界的增量编译收益来自「内核 / 装配层 / `erp`」这一级；
   在 `erp` 内部再切 18 刀，换不到编译单元的减少（§P2-C.6 第 292–295 行已定稿）。

实据抽样（出边 / 入边各一处，`grep` 可复现）：

```
app/src/business/erp/sales/sales_order_handler.rs:90:use crate::business::flow::engine;          # 跨域：ext:flow
app/src/business/erp/sales/sales_model.rs:47:use crate::business::erp::serial_model::period_key_of;  # 包内兄弟
app/src/business/erp/shipment/shipment_posting.rs:46 …                                           # 入边：shipment → sales
```

## 4. 两条卡面命令的真实返回（说明"失败"是刻意的）

```bash
$ cd /opt/cloudpivot/backend && cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.25s
exit=0

$ cd /opt/cloudpivot/backend && cargo test -p cloudpivot-erp-sales --no-run
error: package ID specification `cloudpivot-erp-sales` did not match any packages
exit=101
```

第二条失败 = **包里没有这个 crate**（本卡就是要不建它），不是构建回归；工作区仍是
P2-40 落地时的 2 个包：

```bash
$ cargo metadata --no-deps --offline --format-version 1   # 经 scratch/p243_meta.py 解析
workspace_root= /opt/cloudpivot/backend
  package: cloudpivot      -> /opt/cloudpivot/backend/app/Cargo.toml
  package: cloudpivot-doc  -> /opt/cloudpivot/backend/crates/cloudpivot-doc/Cargo.toml
members= 2
has cloudpivot-erp-sales ? False
```

## 5. 复核命令（只读，可对任意机器跑）

```bash
cd /opt/cloudpivot
python3 scripts/verify/crate_split_readiness.py --domain sales --scope erp-sub               # 11 文件/13 247 行，SCC 51
python3 scripts/verify/crate_split_readiness.py --domain sales --scope erp-sub --layer-check # 退 1：要切 19 条/41 行
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md  # 退 0：15 步 66 行，判定绿
cd backend && cargo metadata --no-deps --offline --format-version 1                          # members=2
```

`--plan-order` 本次输出未变：C1 10 · C3 6 · C4 1 · C5 3 · C6 4 · C7 7 · C9 13 · C10 1 · C11 7 ·
C12 2 · **C13 12** · 其余 0，全序累计 **66 行**，判定绿，`exit=0`。

## 6. 保留物 / 后继

- **保留**：`scripts/verify/crate_split_readiness.py`（`--domain/--layer-check/--plan-order`）、
  `docs/planning/20261113-新卡清单.md` §P2-C、本文（单卡实测，被再派发时不必重扫）。
- **后继**：`sales` 随 **C13 `cloudpivot-erp`** 整包搬迁（包内保持 `business/erp/sales/` 目录分层）。
  本链剩余 P2-44 ~ P2-58 均作废，不需要再开工。
- **本卡零生产代码改动**：无 Rust 文件变更 ⇒ 未跑 `cargo fmt`（无格式面；且本机为多 worker 共享工作区，
  跑全仓 `cargo fmt` 会改到别人的 WIP）；`git status` 下唯一变化是本文（`.md`）。
