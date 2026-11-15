# P2-44 库存域抽 crate 收口 —— 作废（`stock` 随 C13 `cloudpivot-erp` 整包搬迁）

> 卡：`t_aa175963`（P2-44【P2-C】抽 `erp-stock` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41-成本域抽crate可行性分析.md` §8（`11f490f2`）、
> `P2-42-采购域抽crate收口与旧P2-C链实测.md`（`e9a31e96`）、`P2-43-销售域抽crate收口.md`（`939ec719`）

## 1. 本卡处置：作废（不建 crate、不动生产代码）

不建 `backend/crates/cloudpivot-erp-stock/`，不搬 3 个文件 / 2 108 行，不改任何
`crate::business::erp::stock` import，不动 `app/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；
卡面验收命令 `cargo test -p cloudpivot-erp-stock` 因此按预期失败（不是回归，见 §4）。

权威依据已入仓（不是本卡临时主张）：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 288 行：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
  第 226 行 C13 = `cloudpivot-erp`「`erp`（**一个包，不拆子域**）」。
- 同节收尾段（第 292–295 行）：`erp` 内部 51 个节点互相咬住 ⇒ 一个包，包内继续用目录分层。

## 2. `stock` 的实测（只读、无库依赖，2026-11-15 于 99）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain stock --scope erp-sub
域：stock（scope=erp-sub）
  规模：3 个文件 / 2108 行
  SCC：1 个节点同属一个强连通分量        # ← stock 自己一个分量，不在 51 节点大 SCC 内
  回边（留在应用 crate 的代码 → 本域）：0 处代码级
    （无）
  出边（本域 → 其它）：4 个目标
    erp#root:order_pdf_model code=  2 all=  2
    stockout               code=  1 all=  1
    contract               code=  1 all=  1
    stocktake              code=  1 all=  1
  层次判据：本步要切 4 条 / 5 行 —— 出边指向更下层以外：order_pdf_model(2), stockout(1), contract(1), stocktake(1)
  结论：可抽                              # ← 这一行是【旧口径】只看入边，见 §3 第 1 条

$ python3 scripts/verify/crate_split_readiness.py --domain stock --scope erp-sub --check      # 旧口径
exit=0
$ python3 scripts/verify/crate_split_readiness.py --domain stock --scope erp-sub --layer-check  # 层次口径
exit=1
```

文件口径独立复核（工具口径 = 该目录下全部 `.rs`，含 `mod.rs`）：

```bash
$ cd /opt/cloudpivot/backend && wc -l app/src/business/erp/stock/*.rs
   16 app/src/business/erp/stock/mod.rs
 1374 app/src/business/erp/stock/stock_pdf.rs
  718 app/src/business/erp/stock/stock_take_pdf.rs
 2108 total
```

**出边逐行复核（脚本口径漏计 1 行，见下）**：

```
app/src/business/erp/stock/stock_pdf.rs:48       use crate::business::erp::order_pdf_model::{self, MaterialInfo};
app/src/business/erp/stock/stock_pdf.rs:49       use crate::business::erp::{stockin::stockin_model, stockout::stockout_model};
app/src/business/erp/stock/stock_pdf.rs:452          crate::business::erp::stockout::stockout_base::status_label(&row.status),
app/src/business/erp/stock/stock_pdf.rs:1362         crate::business::erp::contract::contract_pdf::EXPORT_OPERATION,
app/src/business/erp/stock/stock_take_pdf.rs:45  use crate::business::erp::order_pdf_model::{self, MaterialInfo};
app/src/business/erp/stock/stock_take_pdf.rs:46  use crate::business::erp::stocktake::stocktake_model::{self, StockTakeItemRow};
```

= **6 行 / 5 个目标**（`order_pdf_model 2`、`stockin 1`、`stockout 1`、`contract 1`、`stocktake 1`）。
脚本报 4 目标 / 5 行，差的那 1 行是 `stock_pdf.rs:49` 的**分组 `use crate::business::erp::{a, b}`** ——
脚本正则只认 `erp::<name>` 直连写法，抓不到 `erp::{…}` 花括号形式（量差 1 行，不改结论）。

**入边为真的证据**（不是"脚本没扫到"）：

```bash
$ grep -rn "erp::stock::" --include=*.rs app/src crates
app/src/business/erp/stock/mod.rs:4        //! `erp::stock_*` 变为 `erp::stock::stock_*` …
app/src/business/erp/stock/stock_pdf.rs:873      /// cargo test --lib business::erp::stock::stock_pdf …
app/src/business/erp/stock/stock_take_pdf.rs:408 /// cargo test --lib business::erp::stock::stock_take_pdf …
```

3 处命中全在 `stock/` 目录**自身**（文档与测试注释）⇒ 目录外代码级引用 **0**。模块进链接单元靠的是
`app/src/business/erp/mod.rs:197 pub mod stock;` + 三条 `#[nik::get]` 路由宏（inventory 自注册）：

```
app/src/business/erp/stock/stock_pdf.rs:576      #[nik::get("/business/erp/stock-in/{id}/pdf")]
app/src/business/erp/stock/stock_pdf.rs:598      #[nik::get("/business/erp/stock-out/{id}/pdf")]
app/src/business/erp/stock/stock_take_pdf.rs:241 #[nik::get("/business/erp/stock-take/{id}/pdf")]
```

## 3. 为什么「SCC 里没它、入边 0」也仍然不能单独成包

1. **判据用错会得到相反答案**。旧口径 `--check`（只看入边）对 `stock` 返 **0**，脚本那行
   `结论：可抽` 就是它的产物；但入边是**合法方向**（装配层必须依赖域 crate，否则路由静默 404）。
   `Cargo` 包图能不能无环只取决于**出边**：`stock` 的 6 行出边全指向 `erp` 包内兄弟
   ⇒ `--layer-check` 退 **1**（本步要切 4 条 / 5 行）。工具文档里已写明「旧口径把方向搞反了」。
2. **C13 之前抽 = 反向依赖单体**。出边目标（`order_pdf_model`、`stockin`、`stockout`、`stocktake`、
   `contract`）在 C13 `cloudpivot-erp` 落地前根本没有 crate 形态；此时 `cloudpivot-erp-stock`
   想 `use` 它们，只能回头依赖 `app`（`cloudpivot` 本身）⇒ `cloudpivot → crates/…-stock → cloudpivot`
   直接 `error: cyclic package dependency`，即"假拆分"。
3. **C13 之后抽 = erp 内部再切一刀**。C13 把 `erp` 收成一个包后，`stock` 的 6 行出边就变成
   `cloudpivot-erp-stock → cloudpivot-erp`：能编过，但**换来的是**「erp 内部 18 个兄弟 × 各自接缝」
   的分层账，与 §P2-C.6「`erp` 一个包，不拆子域」直接冲突；而且 `stock` 只占 `erp` 的
   **3 / 353 文件、2 108 / 296 388 行 = 0.7%**，包边界该给的增量编译收益来自「内核 / 装配层 / `erp`」
   这一级，不在 0.7% 的叶子上。
4. **入边 0 ⇒ 抽出来是个没人 import 的包**。目录外零引用（§2），路由靠 inventory 宏自注册，
   也就是说这个 crate 即使建成也只是"多一个 path 依赖、编译单元数不减"：`stock` 的两份 PDF 渲染
   代码本来就随 `erp` 一起编，抽包不改变任何下游。
5. **更正一条旧说法**（本卡 P2-42 收口时留的评论）：`stock` **不是**唯一不在 51 节点 SCC 的 erp 子域。
   实测（见下）共 **4 个**：`commission`、`depreciation`、`lot`、`stock`。但结论不变 ——
   四个的 **`--layer-check` 全部退 1**（出边都落在 erp 包内兄弟），"不在 SCC"只说明它不必先解环，
   不说明它抽出来能无环。

```bash
$ for d in commission depreciation lot stock; do
    python3 scripts/verify/crate_split_readiness.py --domain $d --scope erp-sub --layer-check >/dev/null; echo "$d exit=$?"; done
commission exit=1     # 4 条 / 5 行：erp#root:model(2), period(1), voucher(1), erp#root:model_base(1)
depreciation exit=1   # 5 条 / 24 行：asset(14), period(4), linkage(3), voucher(2), erp#root:ledger(1)
lot exit=1            # 1 条 / 1 行：erp#root:model_base(1)（另有 1 处回边）
stock exit=1          # 4 条 / 5 行：order_pdf_model(2), stockout(1), contract(1), stocktake(1)
```

> 附：四者里 `lot` 还有 1 处代码级回边，`commission`/`depreciation`/`stock` 入边为 0；
> 无论入边多少，出边不落 L1 内核就抽不出无环包 —— 这就是 P2-C 改层次切法的原因。

## 4. 两条卡面命令的真实返回（说明"失败"是刻意的）

```bash
$ cd /opt/cloudpivot/backend && timeout 1200 cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.14s
build_exit=0

$ cd /opt/cloudpivot/backend && cargo test -p cloudpivot-erp-stock --no-run
error: package ID specification `cloudpivot-erp-stock` did not match any packages
test_exit=101
```

第二条失败 = **包里没有这个 crate**（本卡就是要不建它），不是构建回归；工作区仍是 P2-40 落地时的 2 个包：

```bash
$ cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1
members=2 ["cloudpivot","cloudpivot-doc"]
```

## 5. 复核命令（只读，可对任意机器跑）

```bash
cd /opt/cloudpivot
python3 scripts/verify/crate_split_readiness.py --domain stock --scope erp-sub               # 3 文件/2 108 行，SCC 1，入边 0
python3 scripts/verify/crate_split_readiness.py --domain stock --scope erp-sub --layer-check # 退 1：要切 4 条/5 行
grep -rn "erp::stock::" --include=*.rs backend/app/src | grep -v "business/erp/stock/"        # 期望 0 行（真入边 0）
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md  # 退 0：15 步 66 行，判定绿
cd backend && cargo build --workspace --offline && cargo metadata --no-deps --offline --format-version 1  # exit 0 / members=2
```

`--plan-order` 本次输出未变：C1 10 · C3 6 · C4 1 · C5 3 · C6 4 · C7 7 · C9 13 · C10 1 · C11 7 ·
C12 2 · **C13 12** · 其余 0，全序累计 **66 行**，判定绿，`exit=0`。

## 6. 保留物 / 后继

- **保留**：`scripts/verify/crate_split_readiness.py`（`--domain/--layer-check/--plan-order`）、
  `docs/planning/20261113-新卡清单.md` §P2-C、本文（单卡实测，被再派发时不必重扫）。
- **后继**：`stock`（三张 PDF 导出：收货入库单 / 出库单 / 盘点表）随 **C13 `cloudpivot-erp`** 整包搬迁，
  包内保持 `business/erp/stock/` 目录分层；`--layer-check` 与三层 import 不用改。
- **本卡零生产代码改动**：无 Rust 文件变更 ⇒ 未跑 `cargo fmt`（无格式面；且本机为多 worker 共享工作区，
  跑全仓 `cargo fmt` 会改到别人的 WIP）；`git status` 下唯一变化是本文（`.md`）。
