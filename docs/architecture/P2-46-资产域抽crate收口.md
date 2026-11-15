# P2-46 资产域抽 crate 收口 —— 作废（`asset` 随 C13 `cloudpivot-erp` 整包搬迁）

> 卡：`t_8b69379c`（P2-46【P2-C】抽 `erp-asset` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `5a2bc55a`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41` §8（`11f490f2`）、`P2-42`（`e9a31e96`）、`P2-43`（`939ec719`）、`P2-44`（`810ce2c7`）、`P2-45`（`5a2bc55a`）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-erp-asset/`，不搬 9 个文件 / 9 684 行，不改任何
`crate::business::erp::asset` import，不动 `app/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；
卡面验收命令 `cargo test -p cloudpivot-erp-asset` 因此按预期失败（不是回归，见 §4）。

权威依据已入仓（不是本卡临时主张）：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 288 行：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同表第 226 行 C13 = `cloudpivot-erp`「`erp`（**一个包，不拆子域**）」；
- 同节收尾段（第 292–295 行）：`erp` 内部 51 个节点互相咬住 ⇒ 一个包，包内继续用目录分层。

## 2. `asset` 的实测（只读、无库依赖，2026-11-15 于 99）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain asset --scope erp-sub
域：asset（scope=erp-sub）
  规模：9 个文件 / 9684 行
  SCC：51 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（50）：budget, contract, cost, customer, erp#root:approval_gate, …,
                           payable, period, price, purchase, receivable, report, sales,
                           shipment, stockin, stockout, stocktake, transfer, voucher, year
  回边（留在应用 crate 的代码 → 本域）：17 处代码级
    depreciation              code= 14 all= 14
    erp#root:doc_reader_impl  code=  1 all=  1
    erp#root:doc_reader_impl_tests code= 1 all= 1
    erp#root:statement        code=  1 all=  1
  出边（本域 → 其它）：7 个目标
    voucher                code=  7 all=  7
    erp#root:seq           code=  3 all=  3
    linkage                code=  3 all=  3
    ext:flow               code=  2 all=  2
    period                 code=  2 all=  2
    erp#root:code_policy   code=  1 all=  1
    erp#root:handler       code=  1 all=  1
  层次判据：本步要切 7 条 / 19 行 —— 出边指向更下层以外：voucher(7), erp#root:seq(3),
            linkage(3), ext:flow(2), period(2), erp#root:code_policy(1), erp#root:handler(1)
  结论：不可抽（17 处代码级回边，4 个来源）

$ python3 scripts/verify/crate_split_readiness.py --domain asset --scope erp-sub --check        # 旧口径（只看入边）
exit=1
$ python3 scripts/verify/crate_split_readiness.py --domain asset --scope erp-sub --layer-check  # 层次口径（看入边+出边）
exit=1
```

文件口径独立复核（工具口径 = 该目录下全部 `.rs`，含 `mod.rs`；目录无嵌套子目录，两口径一致）：

```bash
$ cd /opt/cloudpivot/backend && wc -l app/src/business/erp/asset/*.rs
  595 app/src/business/erp/asset/asset_bridge.rs
  874 app/src/business/erp/asset/asset_compute.rs
 1881 app/src/business/erp/asset/asset_dto.rs
 1991 app/src/business/erp/asset/asset_handler.rs
  530 app/src/business/erp/asset/asset_linkage.rs
 1662 app/src/business/erp/asset/asset_linkage_tests.rs
 1801 app/src/business/erp/asset/asset_model.rs
  333 app/src/business/erp/asset/asset_model_tests.rs
   17 app/src/business/erp/asset/mod.rs
 9684 total
$ find app/src/business/erp/asset -name '*.rs' | wc -l
9
```

**出边 19 行逐行复核**（脚本 7 目标 / 19 行，逐行对得上，无花括号 `use crate::business::erp::{…}` 形式漏计）：

```
app/src/business/erp/asset/asset_bridge.rs:40    use crate::business::erp::voucher;
app/src/business/erp/asset/asset_bridge.rs:41    use crate::business::erp::voucher::voucher_multi;
app/src/business/erp/asset/asset_compute.rs:24   use crate::business::erp::voucher::AccountRef;
app/src/business/erp/asset/asset_compute.rs:25   use crate::business::erp::voucher::voucher_multi::ExplicitLine;
app/src/business/erp/asset/asset_handler.rs:57   use crate::business::erp::voucher::voucher_multi;
app/src/business/erp/asset/asset_handler.rs:695  … crate::business::erp::voucher::find_voucher_by_source(db(), "FIXED_ASSET_ACQ", id)
app/src/business/erp/asset/asset_model.rs:41     use crate::business::erp::voucher::{self, SeqSpec};
app/src/business/erp/asset/asset_dto.rs:234      … crate::business::erp::seq::resolve_manual_code(
app/src/business/erp/asset/asset_dto.rs:236      … crate::business::erp::seq::MAX_MANUAL_CODE_LEN,
app/src/business/erp/asset/asset_handler.rs:56   use crate::business::erp::seq;
app/src/business/erp/asset/asset_linkage.rs:65   use crate::business::erp::linkage;
app/src/business/erp/asset/asset_linkage_tests.rs:66  use crate::business::erp::linkage;
app/src/business/erp/asset/asset_linkage_tests.rs:67  use crate::business::erp::linkage::spec;
app/src/business/erp/asset/asset_bridge.rs:42    use crate::business::flow::model as flow_model;
app/src/business/erp/asset/asset_handler.rs:58   use crate::business::flow::{engine, model as flow_model};
app/src/business/erp/asset/asset_handler.rs:1793  use crate::business::erp::period::period_test_support as support;
app/src/business/erp/asset/asset_linkage_tests.rs:68  use crate::business::erp::period::period_test_support as support;
app/src/business/erp/asset/asset_handler.rs:55   use crate::business::erp::code_policy;
app/src/business/erp/asset/asset_handler.rs:220  super::super::handler::code_conflict("分类", code)
```

= **19 行 / 7 目标**。其中 17 行是 `erp::*` 兄弟（voucher 7、seq 3、linkage 3、period 2、
code_policy 1、`erp` 根 `handler` 1，最后一行是 `super::super::handler` 相对路径写法），
2 行是 `business::flow`（C12）。另有 11 行落在 **L1 内核**，按层次口径不计入切边：
`business::shared::state_machine` 4、`business::shared::scope` 1、`business::attachment::{model,rule,test_support}` 4、
`business::notice::{dto}` 2 —— 这些是 C1/C3 内的合法下层依赖，不是解环账。

**入边 17 行逐行复核**（脚本 4 来源 / 17 行，全部命中）：

```
app/src/business/erp/doc_reader_impl.rs:119                use …erp::asset::{asset_bridge, asset_dto};
app/src/business/erp/doc_reader_impl_tests.rs:26           use …erp::asset::asset_bridge;
app/src/business/erp/depreciation/depreciation_dept_tests.rs:36  use …erp::asset::asset_dto::{self, CreateAssetReq};
app/src/business/erp/depreciation/depreciation_dept_tests.rs:37  use …erp::asset::asset_handler;
app/src/business/erp/depreciation/depreciation_dept_tests.rs:38  use …erp::asset::asset_model::{self, AssetInsert};
app/src/business/erp/depreciation/depreciation_dept_tests.rs:244 …erp::asset::asset_compute::start_depreciation_date(purchase)
app/src/business/erp/depreciation/depreciation_handler.rs:21     use …erp::asset::asset_dto::{…
app/src/business/erp/depreciation/depreciation_handler.rs:44     …erp::asset::asset_dto::DEFAULT_PAGE_SIZE
app/src/business/erp/depreciation/depreciation_handler.rs:46     …erp::asset::asset_model::batch_page(&filter, page, page_size)
app/src/business/erp/depreciation/depreciation_linkage_tests.rs:38  use …erp::asset::asset_dto::{self, CreateAssetReq};
app/src/business/erp/depreciation/depreciation_linkage_tests.rs:39  use …erp::asset::asset_model::{self, AssetInsert};
app/src/business/erp/depreciation/depreciation_linkage_tests.rs:246 …erp::asset::asset_compute::start_depreciation_date(
app/src/business/erp/depreciation/depreciation_model.rs:49       use …erp::asset::asset_compute::{self, DepreciationCard};
app/src/business/erp/depreciation/depreciation_model.rs:50       use …erp::asset::asset_dto::{self, DepRunKind, DepRunOutcome, STATUS_DISPOSED};
app/src/business/erp/depreciation/depreciation_model.rs:51       use …erp::asset::asset_model::{self, DepBatchRow, DepDetailRow, DepInsert};
app/src/business/erp/depreciation/depreciation_model.rs:850      …erp::asset::asset_bridge::voucher_error(error, ctx)
app/src/business/erp/statement.rs:105                      use …erp::asset::asset_dto::normalize_page;
```

`grep` 另有 **2 行是文档注释**（`voucher/voucher_query.rs:733`、`linkage/spec.rs:1174`），不计代码级引用 ⇒
严格 17 行，与脚本一致。

**进链接单元靠什么**：`app/src/business/erp/mod.rs:117 pub mod asset;` + `asset_handler.rs` 里
**16 条 `#[nik::*]` 路由宏**（`asset-category` 6 条 + `asset` 10 条），路由自注册，无手写 `routes()` 汇编。

## 3. 为什么不能单独成包

1. **在 51 节点大 SCC 里**（>1 ⇒ 与本批 50 个兄弟不可能拆成两个无环包）。要脱离必须先解环，
   而 `asset` **两个方向都欠账**：入边 17 行（4 源）、出边 19 行（7 目标）——
   是本批里少见的「既被人重度依赖、又重度依赖别人」节点（`depreciation` 一家就 14 行）。
   解环账 = 至少 36 行跨文件改动，换来的只是把 `erp` 296 388 行切成第 19 块。
2. **旧口径会把方向判反**：`--check` 只看入边，而 `asset` 入边 17 行 ⇒ 退 1 是「巧合的正确」；
   但即使入边为 0，判据也应看**出边**（装配层 → 域 crate 是合法方向，域 crate → 单体才是环）。
   `--layer-check` 对本域同样退 1：本步要切 **7 条 / 19 行**，全部指向 `erp` 包内或 C12 `flow`。
3. **C13 之前抽 = 反向依赖单体**：出边目标 `voucher`/`seq`/`linkage`/`period`/`code_policy`/`erp` 根
   `handler` 在 C13 落地前没有 crate 形态，`cloudpivot-erp-asset` 只能 `use` 回 `app`（`cloudpivot`）
   ⇒ `cloudpivot → crates/…-asset → cloudpivot`，`error: cyclic package dependency`，即假拆分。
4. **C13 之后抽 = erp 内部再切一刀**：C13 收成一个包后这些出边变成 `…-asset → cloudpivot-erp`，能编过，
   但等于在 `erp` 内部再造 18 个兄弟接缝，与 §P2-C.6「`erp` 一个包，不拆子域」直接冲突；
   且 `asset` 只占 `erp` 的 **9 / 353 文件 = 2.5%、9 684 / 296 388 行 = 3.3%**，
   包边界的增量编译收益来自「内核 / 装配层 / `erp`」这一级（`erp` 占全仓 50%），不在 3.3% 的子域。
5. **搬迁成本落点明确**：C13 整包搬迁时，`asset` 的 36 行接缝**一行都不用改**
   （同包内 `crate::business::erp::…` 路径不变），只需随 `app/src/business/erp/` 目录整体平移
   + 在 `cloudpivot-erp` 的 `lib.rs` 里保留 `pub mod asset;`。这是本卡作废的正面理由，不只是"做不了"。

## 4. 两条卡面命令的真实返回（说明"失败"是刻意的）

```bash
$ cd /opt/cloudpivot/backend && cargo build --workspace --offline ; echo build_exit=$?
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.13s
build_exit=0

$ cd /opt/cloudpivot/backend && cargo test -p cloudpivot-erp-asset --no-run --offline ; echo stub_exit=$?
error: package ID specification `cloudpivot-erp-asset` did not match any packages
stub_exit=101
```

第二条失败 = **包里没有这个 crate**（本卡就是要不建它），不是构建回归；工作区仍是 P2-40 落地时的 2 个包：

```bash
$ cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1
members = 2 ['cloudpivot', 'cloudpivot-doc']
workspace_members = ['path+file:///opt/cloudpivot/backend/app#cloudpivot@0.1.0',
                     'path+file:///opt/cloudpivot/backend/crates/cloudpivot-doc#0.1.0']
```

卡序仿真未受影响：

```bash
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
…
C12    cloudpivot-flow         2   2  OK
C13    cloudpivot-erp         12  12  OK        切向：task(8), business/task(4)
C14    cloudpivot-task         0   0  OK
C15    收尾小域（project/taskflow/dashboard） 0 0 OK
全序累计要切：66 行
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

`--plan-order` 里**没有 `asset` 节点**：`erp` 的子域不单独成步，正是本卡作废的形式化体现。

## 5. 复核命令（只读，可对任意机器跑）

```bash
cd /opt/cloudpivot
python3 scripts/verify/crate_split_readiness.py --domain asset --scope erp-sub               # 9 文件/9 684 行，SCC 51，入边 17/出边 19
python3 scripts/verify/crate_split_readiness.py --domain asset --scope erp-sub --layer-check # 退 1：本步要切 7 条/19 行
grep -rn "erp::asset::" --include=*.rs backend/app/src backend/crates \
  | grep -v "business/erp/asset/" | grep -v "///" | wc -l                                    # 期望 17（入边）
grep -rn "crate::business::" --include=*.rs backend/app/src/business/erp/asset \
  | grep -v "crate::business::erp::asset"                                                    # 逐行核出边
grep -rnE "#\[nik::(get|post|put|delete|patch)" backend/app/src/business/erp/asset/asset_handler.rs | wc -l  # 期望 16（路由宏）
# 注意：`grep -c "#\[nik::"` 得 32 —— 每条路由都挂 `#[nik::permission]` + `#[nik::get/post/…]` 两个属性
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md   # 退 0：15 步 66 行，绿
cd backend && cargo build --workspace --offline ; echo $?                                   # exit 0
```

## 6. 保留物 / 后继

- **保留**：`scripts/verify/crate_split_readiness.py`（`--domain` / `--layer-check` / `--plan-order`）、
  `docs/planning/20261113-新卡清单.md` §P2-C、本文（单卡实测，再派发时不必重扫）。
- **后继**：`asset`（`asset-category` 6 + `asset` 10 = 16 条路由）随 **C13 `cloudpivot-erp`** 整包搬迁，
  包内保持 `business/erp/asset/` 目录分层，三层 import 与 36 行接缝均不改。
- **本卡零生产代码改动**：无 Rust 文件变更 ⇒ 未跑 `cargo fmt`（无格式面可动；且本机为多 worker 共享工作区，
  跑全仓 `cargo fmt` 会改到别人 WIP）；`git status` 下唯一变化是本文（`.md`）。
