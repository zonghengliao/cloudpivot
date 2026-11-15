# P2-56 通知域抽 crate 收口 —— 作废（`notice` 并入 C5 `cloudpivot-notice`）

> 卡：`t_af32bb34`（P2-56【P2-C】抽 `notice` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `632ad268`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41`（`11f490f2`）、`P2-42`（`e9a31e96`）、`P2-43`（`939ec719`）、`P2-44`（`810ce2c7`）、
> `P2-45`（`5a2bc55a`）、`P2-46`（`ca11c5dc`）、`P2-47`（`4648b330`）、`P2-48`（`6a473699`）、
> `P2-49`（`c1f7aa76`）、`P2-50`（`4b95fad3`）、`P2-51`（`0cc8554c`）、`P2-52`（`088c0586`）、
> `P2-53`（`17c41945`）、`P2-54`（`3e22d3fe`）、`P2-55`（`632ad268`）
> 数据库：145（`172.18.10.145:5432`，用户 `cloudpivot`）· 演练库 `cp_p256_notice_drill`（**已 DROP**）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-notice/`，不搬 11 个文件 / 8 224 行，不改 19 行
`crate::business::notice` 自有出边与 76 行入边 import（见 §3 / §4），不动
`backend/app/Cargo.toml`，不动 `backend/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；卡面验收命令因此按预期
失败（不是回归）：

```bash
$ cd backend && cargo test -p cloudpivot-notice --offline -- --test-threads=1
error: package ID specification `cloudpivot-notice` did not match any packages
STUB_EXIT=101
```

```bash
$ cd backend && cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s
BUILD_EXIT=0
```

权威依据已入仓：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 288 行：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同文件 §P2-C.3 第 218 行的 **C5** 行：`cloudpivot-notice`、`-pdf`、`-docpdf`、`-monitor`｜节点
  `monitor`、`notice`、`pdf`、`docpdf`｜前置 **C1、C3**｜回边 **0 / 74 / 28 / 14**｜
  **本步要切 3 行：task 3（卡内先后：docpdf→pdf 3 行）**｜机械量 `24 / 17 825 · 22 · 0 · 2 · 0`；
- `P2-42` §4 台账（第 95 行）P2-56 行 = `11 / 8 224`｜SCC 24｜入边 `74（7）`｜出边目标 1｜`3 行`｜`退 1`｜
  落点 **`C5 cloudpivot-notice`**；
- 本卡 2026-11-15 11:09 的改派留言（并入 C5 `cloudpivot-notice`，与 `pdf`/`docpdf`/`monitor` 同卡）。

### 1.1 与前 15 张作废卡的异同：本卡是 C5 的**四分之一**，解环账 3 行

前面 P2-43 ~ P2-55 有三种形态（子域并进一个大包 / 独立包带解环账 / 同卡两个包的一步）。本卡属第一种
的变体：**C5 是「四个包一张卡」**（`notice` + `pdf` + `docpdf` + `monitor`），
P2-56 与「C5 的 notice 半步」是同一件事，作废的实质是**并入 C5 的卡序**。

1. **`notice` 自己的解环账 = 3 行**（§3）：全部指向 `task`（`use crate::task::Scheduler;`，三个
   `register_builtin` 注册点）。C5 的「本步要切 3 行：task 3」就是这 3 行，**一一对上**。
2. **不成立的是时序**：C5 前置 C1（内核）、C3（support）。实测当前 `backend/crates/` 只有
   `cloudpivot-doc`（`ls backend/crates/` → `cloudpivot-doc`），**C1、C3 都没落地** ⇒ 现在开工只能得到
   `cloudpivot-notice → cloudpivot(单体)` 的反向依赖（§P2-C.5 明确拒绝的假拆分）。
3. **入边 74 行（7 源）无需任何人付账**：`erp` / `flow` / `business/task` / `task` / `system` /
   `announcement` 全是 C5 **之后**抽的包，`notice` 抽成 crate 后这些引用变成合法的向下箭头；
   装配层的 4 行（`business/mod.rs` 2 + `lib.rs` 2）本来就留在单体。

⇒ 下一轮要重开，请**直接开 C5**（卡序第 5 位），别再开 P2-56。

## 2. `notice` 的实测（只读、无库依赖，2026-11-15 于 99）

### 2.1 工具原始输出（逐字）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain notice --layer-check
域：notice（scope=top）
  规模：11 个文件 / 8224 行
  SCC：24 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（23）：aiagent, announcement, attachment, business/shared, business/task,
    changelog, docpdf, erp, flow, help, hr, impexp, masking, middleware, mutex, pdf, permission,
    shared, storage, support, system, task, utils
  回边（留在应用 crate 的代码 → 本域）：74 处代码级
    announcement           code=  5 all=  6
    business#root          code=  2 all=  3
    business/task          code=  6 all=  6
    erp                    code= 40 all= 40
    flow                   code= 17 all= 17
    system                 code=  1 all=  3
    task                   code=  3 all=  3
  出边（本域 → 其它）：4 个目标
    shared                 code= 10 all= 10
    task                   code=  3 all=  5
    business/shared        code=  3 all=  3
    utils                  code=  3 all=  3
  层次判据：本步要切 1 条 / 3 行 —— 出边指向更下层以外：task(3)
            （假设 L1 内核 shared, utils, business/shared 已抽出、装配层 bin 已独立成包；完整卡序见 --plan-order）
  结论：不可抽（74 处代码级回边，7 个来源）
exit=1
```

```bash
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
C1     cloudpivot-kernel                            10    10   OK
C3     cloudpivot-support                            6     6   OK
C5     cloudpivot-notice, cloudpivot-pdf, cloudpivot-docpdf        3     3   OK
                                              切向：task(3)
                                              卡内跨包先后：docpdf→pdf(3)
…
全序累计要切：66 行
卡序覆盖 28 个节点；未进卡序（留在单体）2 个：app#root, business#root
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

**两次口径一致**：`--layer-check` 与 `--plan-order` 都报 `notice` 本步要切 **3 行（task 3）**，
`P2-42` §4 台账的「出边目标 1 / 3 行」也是这 3 行（目标 = `task` 一个）。

### 2.2 工具与卡面的两处口径差（本卡复核发现，写进 C5 施工单）

1. **`monitor` 没进工具解析出的 C5 crate 列**：卡面 C5 写 4 个包（`notice`/`pdf`/`docpdf`/`monitor`），
   `--plan-order` 只列 3 个（`cloudpivot-notice, cloudpivot-pdf, cloudpivot-docpdf`）。
   单独复核 `--domain monitor --layer-check`：5 文件 / 2 727 行、SCC **1**、`可抽`、
   **本步要切 1 条 / 2 行：support(2)**。按卡序 C3（support）在 C5 之前 ⇒ 这 2 行变合法向下箭头，
   不增加 C5 的账；但**卡面 4 包、工具 3 包**这个差要写进 C5 卡面，否则按工具排产会漏 `monitor`。
2. **`notice` 的手写路由不计入工具的宏路由口径**：`P2-42` §4 与 C5 行用的是宏路由条数，
   而 `notice` 另有 1 条 `library` 侧手工注册的 `POST /internal/alert`（§5）—— 数路由时要么统一
   「宏 + 手册」，要么在卡面注明「+1 条装配层手写路由」。

### 2.3 SCC：与 23 个节点同环，但环可以按卡序单向化

`notice` 在顶层 24 节点 SCC 里（环内还有 `pdf`、`docpdf`、`flow`、`erp`、`task` 等）。
拆包不要求先出环，只要求**每一步的出边都指向已抽出的包**：

- `notice → shared/utils/business::shared`（10+3+3 = 16 行）：C1 内核 ⇒ 合法；
- `notice → task`（3 行）：`task` 是 **C14**，在 C5 之后 ⇒ **这就是 C5 本步要切的 3 行**（§3.2）；
- 反向 74 行入边：7 个源全部在 C5 之后（§4 归属表）⇒ 合法，谁都不用付；
- 唯一的双向咬合是 `notice ⇄ business/task`：`business/task → notice` 6 行（C14 之后，合法）；
  `notice → business/task` **0 行**（`notice` 只碰 `crate::task`，不碰 `crate::business::task`）⇒ 不成环。

## 3. 出边：19 行代码（prod 19 / test 0）+ 2 行注释，本步要切 **3 行**

`grep -rn 'crate::' business/notice`（排除自身路径）实测 **21 条命中 = 代码 19 + 注释 2**，
与工具 `code=10+3+3+3=19` 完全一致。

| # | 文件:行 | 目标 | 归属前置 | 行内容 |
|--:|:--|:--|:--:|:--|
| 1 | `dto.rs:18` | `shared` | C1 内核 | `use crate::shared::constants::EnableStatus;` |
| 2 | `dto.rs:19` | `shared` | C1 | `use crate::shared::error_code;` |
| 3 | `fanout.rs:29` | `shared` | C1 | `use crate::shared::error_code;` |
| 4 | `handler.rs:57` | `shared` | C1 | `use crate::shared::constants;` |
| 5 | `handler.rs:58` | `shared` | C1 | `use crate::shared::constants::EnableStatus;` |
| 6 | `handler.rs:59` | `shared` | C1 | `use crate::shared::error_code;` |
| 7 | `mod.rs:90` | `shared` | C1 | `use crate::shared::error_code;` |
| 8 | `render.rs:31` | `shared` | C1 | `use crate::shared::error_code;` |
| 9 | `model.rs:26` | `shared` | C1 | `use crate::shared::constants;` |
| 10 | `model.rs:27` | `shared` | C1 | `use crate::shared::constants::EnableStatus;` |
| 11 | `handler.rs:60` | `utils` | C1 | `use crate::utils;` |
| 12 | `mod.rs:91` | `utils` | C1 | `use crate::utils;` |
| 13 | `model.rs:28` | `utils` | C1 | `use crate::utils;` |
| 14 | `handler.rs:56` | `business::shared` | C1 | `use crate::business::shared::principal;` |
| 15 | `model.rs:24` | `business::shared` | C1 | `use crate::business::shared::principal::ScopedPrincipal;` |
| 16 | `model.rs:25` | `business::shared` | C1 | `use crate::business::shared::scope;` |
| 17 | `cleanup.rs:22` | `task` | **C14（本步要切）** | `use crate::task::Scheduler;` |
| 18 | `scheduled.rs:38` | `task` | **C14（本步要切）** | `use crate::task::Scheduler;` |
| 19 | `sse_hub.rs:67` | `task` | **C14（本步要切）** | `use crate::task::Scheduler;` |

注释 2 条（工具不计入 `code`，搬包后是 rustdoc 链接失效）：
`cleanup.rs:12`、`scheduled.rs:30`，均形如
`` [`crate::task::Scheduler::register_builtin`] ``。

**归类结论**

| 目标 | 行数 | 抽出后指向 | 本步要切 |
|:--|--:|:--|:--:|
| `shared` / `utils` / `business::shared` | 10 + 3 + 3 = **16** | C1 `cloudpivot-kernel` | 0 |
| `task` | **3** | C14 `cloudpivot-task`（**在 C5 之后**） | **3 行** |
| 合计 | **19** | — | **3 行** |

### 3.1 3 行怎么切（C5 施工单的设计决定，本卡只报不动）

三个点都是**调度器注册**，形态完全相同：

```
business/notice/cleanup.rs:22    use crate::task::Scheduler;   // pub fn register_builtin(scheduler: &Scheduler)
business/notice/scheduled.rs:38  use crate::task::Scheduler;
business/notice/sse_hub.rs:67    use crate::task::Scheduler;
```

反向的 `src/task/mod.rs:240/242/244`（`crate::business::notice::{cleanup,sse_hub,scheduled}::register_builtin(self)`）
是「单体 → crate」的合法方向，C14 之后变成 `cloudpivot_task → cloudpivot_notice`，不用动。
可行切法与 P2-53 给出的接缝同一条：**注册签名上提** —— 内核提供
`JobDef{name, cron, desc, runner}` / `trait JobRegistrar`，`notice` 侧改成
`pub fn builtin_jobs() -> Vec<JobDef>` 或 `register_builtin(reg: &mut dyn JobRegistrar)`，
不再直接吃 `&Scheduler` 类型。这条**必须在 C1 施工时定**，留到 C5 就是每张域卡各改一遍。

### 3.2 另有 7 行是「编译级」而非「跨域边」（工具不计，C5 施工单必带）

| # | 位置 | 内容 | 搬包后必须怎么改 |
|--:|:--|:--|:--|
| 1 | `alert_dedup.rs:52` | `use crate::chrono::{DateTime, Duration, Utc};` | `crate::chrono` 是 `use nik::prelude::*` 在 crate 根 glob 出来的重导出 ⇒ 改 `use nik::prelude::chrono::{…};`（或 `chrono::…` + 显式依赖） |
| 2–5 | `handler.rs:1451,1452,1459,1500` | `crate::chrono::Duration/Utc::now()` | 同上（4 行，全在 `#[cfg(test)]` 里） |
| 6 | `handler.rs:847` | ``路由体由 [`crate::run`] 的 `App::post` 手动注册`` | `crate::run` 留在装配层 ⇒ 改成散文或删掉 intra-doc link |
| 7 | `model.rs:657` | ``写入侧在 [`crate::business::announcement::maintenance::dispatch_announcement`]`` | `announcement` 是 C6 包 ⇒ 改 `cloudpivot_announcement::…`（链接失效不报错，但会告警） |

`crate::bootstrap::…` 反查（P2-49/P2-51 的第三种漏报形态）：**0 处**。

## 4. 入边：76 行代码（工具 74 + 装配层 2），prod **72** / test **4**

`grep -rn 'business::notice'`（排除本模块）实测 **88 条 = 代码 76 + 注释 12**。
工具报 74 / 7 源 —— 差的 2 行是 **`lib.rs`（装配层 bin 根）**，工具只登记 `business#root` 不登记
`app#root`（`--plan-order` 末尾也自认「未进卡序 2 个：app#root, business#root」）。

| # | 文件:行 | prod/test | 行内容 |
|--:|:--|:--:|:--|
| 1 | `business/announcement/maintenance.rs:32` | prod | `use crate::business::notice::dto::{CHANNEL_INBOX, ContentFormat, NoticeLevel, NoticeType};` |
| 2 | `business/announcement/maintenance.rs:33` | prod | `use crate::business::notice::{self, DispatchResult, NoticeDraft, NoticeTarget};` |
| 3 | `business/announcement/maintenance.rs:270` | prod | `biz_type: Some(crate::business::notice::model::BIZ_TYPE_ANNOUNCEMENT.to_string()),` |
| 4 | `business/announcement/model.rs:41` | prod | `use crate::business::notice::model::{BIZ_TYPE_ANNOUNCEMENT, BizWithdrawReport, Executor as NoticeExecutor};` |
| 5 | `business/announcement/model.rs:1470` | prod | `crate::business::notice::model::withdraw_by_biz(&notice_tx, …).await?;` |
| 6 | `business/mod.rs:62` | prod | `use crate::business::notice::dto::{ContentFormat, NoticeLevel, NoticeType};` |
| 7 | `business/mod.rs:63` | prod | `use crate::business::notice::{NoticeDraft, NoticeTarget, dispatch};` |
| 8 | `business/task/collab.rs:65` | prod | `use crate::business::notice::dto::{NoticeLevel, NoticeType};` |
| 9 | `business/task/collab.rs:66` | prod | `use crate::business::notice::{self, NoticeDraft, NoticeTarget};` |
| 10 | `business/task/hermes_sync.rs:47` | prod | 同 #8 |
| 11 | `business/task/hermes_sync.rs:48` | prod | 同 #9 |
| 12 | `business/task/task_handler_notice.rs:11` | prod | 同 #8 |
| 13 | `business/task/task_handler_notice.rs:12` | prod | 同 #9 |
| 14 | `business/erp/asset/asset_linkage.rs:66` | prod | `use crate::business::notice;` |
| 15 | `business/erp/asset/asset_linkage.rs:67` | prod | `use crate::business::notice::dto::{NoticeLevel, NoticeType};` |
| 16 | `business/erp/contract/contract_linkage.rs:66` | prod | 同 #14 |
| 17 | `business/erp/contract/contract_linkage.rs:67` | prod | 同 #15 |
| 18 | `business/erp/depreciation/depreciation_linkage.rs:60` | prod | 同 #14 |
| 19 | `business/erp/depreciation/depreciation_linkage.rs:61` | prod | 同 #15 |
| 20 | `business/erp/expense/expense_linkage.rs:51` | prod | 同 #14 |
| 21 | `business/erp/expense/expense_linkage.rs:52` | prod | 同 #15 |
| 22 | `business/erp/linkage/actions.rs:39` | prod | `use crate::business::notice::{self, NoticeDraft, NoticeTarget};` |
| 23 | `business/erp/linkage/engine.rs:55` | prod | `use crate::business::notice::NoticeDraft;` |
| 24 | `business/erp/linkage/flow_instance_trigger.rs:93` | prod | 同 #15 |
| 25 | `business/erp/linkage/flow_instance_trigger.rs:94` | prod | `use crate::business::notice::{self, NoticeTarget};` |
| 26 | `business/erp/linkage/tests.rs:34` *(test)* | **test** | `use crate::business::notice::dto::{NoticeLevel, NoticeType};` |
| 27 | `business/erp/linkage/tests.rs:35` *(test)* | **test** | `use crate::business::notice::{NoticeDraft, NoticeTarget};` |
| 28 | `business/erp/payable/payable_linkage.rs:44` | prod | 同 #15 |
| 29 | `business/erp/payable/payable_linkage.rs:45` | prod | `use crate::business::notice::{self};` |
| 30 | `business/erp/period/period_linkage.rs:62` | prod | 同 #14 |
| 31 | `business/erp/period/period_linkage.rs:63` | prod | 同 #15 |
| 32 | `business/erp/period/period_remind.rs:95` | prod | 同 #15 |
| 33 | `business/erp/period/period_remind.rs:96` | prod | 同 #9 |
| 34 | `business/erp/purchase/purchase_linkage.rs:46` | prod | 同 #15 |
| 35 | `business/erp/purchase/purchase_linkage.rs:47` | prod | `use crate::business::notice::{self, NoticeTarget};` |
| 36 | `business/erp/purchase/purchase_return_handler.rs:67` | prod | 同 #14 |
| 37 | `business/erp/purchase/purchase_return_handler.rs:68` | prod | 同 #15 |
| 38 | `business/erp/reminder.rs:94` | prod | 同 #15 |
| 39 | `business/erp/reminder.rs:95` | prod | `use crate::business::notice::{self, DispatchError, NoticeDraft, NoticeTarget};` |
| 40 | `business/erp/stockin/stockin_handler_post.rs:1004` | prod | `let mut draft = crate::business::notice::NoticeDraft::text(` |
| 41 | `business/erp/stockin/stockin_handler_post.rs:1005` | prod | `crate::business::notice::dto::NoticeType::Todo,` |
| 42 | `business/erp/stockin/stockin_handler_post.rs:1006` | prod | `crate::business::notice::dto::NoticeLevel::Info,` |
| 43 | `business/erp/stockin/stockin_handler_post.rs:1009` | prod | `crate::business::notice::NoticeTarget::User(row.create_by.into_iter().collect()),` |
| 44 | `business/erp/stockout/stockout_linkage.rs:23` | prod | `use crate::business::notice::{self, dto::NoticeLevel, dto::NoticeType};` |
| 45 | `business/erp/stocktake/stocktake_linkage.rs:45` | prod | 同 #15 |
| 46 | `business/erp/stocktake/stocktake_linkage.rs:46` | prod | 同 #29 |
| 47 | `business/erp/transfer/transfer_linkage.rs:43` | prod | `use crate::business::notice::{self, dto::NoticeLevel, dto::NoticeType};` |
| 48 | `business/erp/transfer/transfer_overdue.rs:58` | prod | 同 #15 |
| 49 | `business/erp/transfer/transfer_overdue.rs:59` | prod | 同 #9 |
| 50 | `business/erp/voucher/voucher_linkage.rs:63` | prod | 同 #14 |
| 51 | `business/erp/voucher/voucher_linkage.rs:64` | prod | 同 #15 |
| 52 | `business/erp/year/year_end_linkage.rs:76` | prod | 同 #14 |
| 53 | `business/erp/year/year_end_linkage.rs:77` | prod | 同 #15 |
| 54 | `business/flow/engine.rs:73` | prod | 同 #15 |
| 55 | `business/flow/engine.rs:74` | prod | 同 #9 |
| 56 | `business/flow/graph.rs:43` | prod | `use crate::business::notice::render;` |
| 57 | `business/flow/handler.rs:51` | prod | `use crate::business::notice::dto::ContentFormat;` |
| 58 | `business/flow/handler.rs:52` | prod | `use crate::business::notice::render;` |
| 59 | `business/flow/notify.rs:44` | prod | 同 #15 |
| 60 | `business/flow/notify.rs:45` | prod | 同 #9 |
| 61 | `business/flow/notify_node.rs:70` | prod | `use crate::business::notice::dto::{ContentFormat, NoticeLevel, NoticeType};` |
| 62 | `business/flow/notify_node.rs:71` | prod | `use crate::business::notice::{NoticeDraft, NoticeTarget};` |
| 63 | `business/flow/notify_node.rs:280` | prod | `crate::business::notice::render::render_paths(template, vars, format)…` |
| 64 | `business/flow/notify_node.rs:600` | prod | `let found = crate::business::notice::model::find_enabled_template_by_code(code)` |
| 65 | `business/flow/task_node.rs:46` | prod | 同 #57 |
| 66 | `business/flow/task_node.rs:47` | prod | 同 #56 |
| 67 | `business/flow/template_vars.rs:260` | prod | `let Ok(paths) = crate::business::notice::render::collect_placeholders_paths(template) else {` |
| 68 | `business/flow/template_vars.rs:338` *(test)* | **test** | `let out = crate::business::notice::render::render_paths(` |
| 69 | `business/flow/template_vars.rs:341` *(test)* | **test** | `crate::business::notice::dto::ContentFormat::Text,` |
| 70 | `business/flow/timeout.rs:53` | prod | `use crate::business::notice;` |
| 71 | `system/badge/model.rs:53` | prod | `use crate::business::notice;` |
| 72 | `task/mod.rs:240` | prod | `crate::business::notice::cleanup::register_builtin(self);` |
| 73 | `task/mod.rs:242` | prod | `crate::business::notice::sse_hub::register_builtin(self);` |
| 74 | `task/mod.rs:244` | prod | `crate::business::notice::scheduled::register_builtin(self);` |
| 75 | `lib.rs:94` | prod（装配） | `.post("/internal/alert", business::notice::handler::internal_alert)` |
| 76 | `lib.rs:100` | prod（装配） | `business::notice::sse_hub::shutdown();` |

注：#26/#27 在 `erp/linkage/tests.rs`（`linkage.rs:64` 的 `#[cfg(test)] mod tests;`），
#68/#69 在 `flow/template_vars.rs:277` 的 `#[cfg(test)]` 之后 ⇒ 计 test。

注释 12 条（工具不计入，搬包后是 rustdoc 失效，需顺手改）：
`announcement/model.rs:674,1442`、`erp/linkage/actions.rs:9`、`erp/linkage.rs:51`、
`erp/transfer/transfer_overdue.rs:13,34`、`flow/notify.rs:18`、`business/task/mod.rs:44`、
`business/mod.rs:56`、`hr/mod.rs:89`、`system/badge/model.rs:24`、`system/authz_tests.rs:431`。

**入边的形态高度收敛，对改 import 有利**：

| 形态（脚本逐行分类，合计 76） | 行数 | 改法 |
|:--|--:|:--|
| A. `use crate::business::notice;`（光杆） | 10 | → `use cloudpivot_notice;` |
| B1. `use …::dto::{…}`（`NoticeLevel`/`NoticeType`/`ContentFormat` 等） | 26 | → `cloudpivot_notice::dto::{…}` |
| B2. `use …::{self, …}` / `{NoticeDraft, NoticeTarget}` 等花括号组合 | 19 | 同上（`self` → `cloudpivot_notice`） |
| B3. `render` / `model::*` / `NoticeDraft` 单点 | 3 + 1 + 1 = 5 | 同上 |
| C. 行内全限定 `crate::business::notice::…`（含 `lib.rs` 装配 2 行） | 16 | 同上（装配 2 行随 C2 搬，见 §8 第 4 条） |

改法唯一：`crate::business::notice::` → `cloudpivot_notice::`。

### 4.1 `notice` 没有 `test_support` 夹具跨包问题（比 attachment / flow 幸运）

- `grep 'test_support\|fixture_hygiene\|test_helpers' business/notice` = **0**；
- 4 行测试入边（#26/#27/#68/#69）全是普通 `use` + 调用公开 API，**不需要 `test-support` feature**；
- 也没有 `include_str!` / `CARGO_MANIFEST_DIR` / `repo_root()` —— 搬包不改任何相对路径深度。

## 5. 机械量（与 C5 行 `24 / 17 825 · 22 · 0 · 2 · 0` 对得上）

| 项 | notice 实测 | C5 合计（notice+pdf+docpdf+monitor） | 说明 |
|:--|:--|:--|:--|
| 文件 / 行 | **11 / 8 224** | 24 / 17 825 ✔（pdf 5/6 243 + docpdf 3/631 + monitor 5/2 727 + notice 11/8 224） | 全平铺无子目录：`mod/handler/dto/model/render/fanout/delivery/sse_hub/alert_dedup/cleanup/scheduled` |
| HTTP 端点 | **18** = 17 宏 + 1 手写 | 22 = notice 17 + pdf 3 + docpdf 0 + monitor 2（**手工路由未计**，见 §2.2） | 宏路由：`handler.rs` 17 条；手写：`lib.rs:94` `.post("/internal/alert", …)`（豁免 `[router] prefix`） |
| 权限码 | **9**（17 处 `#[nik::permission]`） | — | `send×1` `list×3` `delete×1` `mine×6` `template:list×2` `template:create×1` `template:update×1` `template:delete×1` `channel:config×1`；**17 条宏路由全部挂码**，0 条裸路由 |
| 免认证 | **1** `#[nik::no_auth("!/internal/alert")]` · `auth_only` **0** | — | 内部告警入口，处理器内还有 `internal_caller_allowed` 回环闸 |
| 菜单行 | **13** | — | 1 目录（`notice-center` 0880）+ 3 页面（`消息通知` 0410 / `消息中心` 0010 / `发送通知` 0020）+ 9 按钮（0411–0419，9 个码各一行）；按迁移拆：`V0045` 10 行 + `V0047` 2 行 + `V0133` 1 行（并把 3 个页面挂进 0880 目录） |
| 表 | **5** | — | `sys_notice`、`sys_notice_receiver`、`sys_notice_delivery`、`sys_notice_template`、`sys_notice_alert_dedup` |
| 迁移 | **9 个版本 / 18 文件**（全在 `migrations/business/`） | — | `V0043` 建 notice、`V0044` 建模板、`V0045` 9 码种子 + 10 行菜单、`V0047` impexp 通知码 + 2 行菜单、`V0049` 索引、`V0051` 定时派发、`V0125` 流程模板、`V0133` 公告与消息目录（1 行 + 改 3 页面归属）、`V0404` 告警去重表 |
| `#[cfg(test)]` | **12** | — | alert_dedup / cleanup / delivery / dto / fanout / handler / model / mod / render×2 / scheduled / sse_hub |
| 用例 | **77**（`#[test]`+`#[tokio::test]` 合计 = 77，与真跑一致） | — | 其中**行内真库用例 2 条**（`handler.rs:1308` / `1434` 走 `ensure_db()`）；`*_db_tests.rs` 文件 **0** |
| 出目录 `include_str!` | **0** | 2（属 pdf/docpdf，见 P2-57） | 搬包不改相对深度 |
| 夹具引用 | **0** | 0 ✔ | 无 `test_support` / `test_helpers` / `fixture_hygiene` |

## 6. 搬包会失效的门禁（按源码路径硬编码，本卡只报不动）

| # | 位置 | 现状 | 搬迁后 | 严重度 |
|--:|:--|:--|:--|:--|
| 1 | `system/menu/permission_audit_tests.rs:747` `src_root() = CARGO_MANIFEST_DIR/src` | 只扫 `backend/app/src` | **N14 直接变红**：9 个码在 `app/src` 内**外部引用 = 0**（见下），`dead` 非空即断言失败 | 🔴 红 |
| 2 | 同上（`scan_routes`） | 17 条 notice 宏路由在扫描面内 | 17 条路由**静默逃出** N16（裸路由）/N21（静态路由被模板遮蔽）；`routes.len() > 300` 仍过 | 🟠 静默 |
| 3 | `scripts/lint/scope_gate.py` `SKIP_MANIFEST` | **没有** `business/notice/**` 条目（实测 `grep -n 'business/notice'` 0 行） | 搬走后扫描面少一目录，清单无需改 ⇒ **不响**（与 attachment 的「会响」相反，静默风险更高） | 🟠 静默 |
| 4 | `business/shared/layering_tests.rs:44` `MIN_DOMAINS = 18` | `src/business` 下 **21** 个目录（不含 `shared`） | 20 ≥ 18 ⇒ **静默变绿**，notice 的跨域引用从此无人扫 | 🟠 静默 |
| 5 | `scripts/lint/path_refs_gate.py`（`$SRC` → `backend/app/src`） | 当前 899 个脚本 0 失效 | 任何脚本/hook 写 `$SRC/business/notice/…` 即红（今天实测 0 处，**预防性**） | 🟡 预防 |

9 个码的「外部引用」实测（`backend/app/src`，排除 `business/notice/**`，按 `\b` 精确匹配）：

```
business:notice:send             0
business:notice:list             0
business:notice:delete           0
business:notice:mine             0
business:notice:template:list    0（唯一命中 system/authz_tests.rs:803 是注释行，非代码）
business:notice:template:create  0
business:notice:template:update  0
business:notice:template:delete  0
business:notice:channel:config   0
```

即：这 9 个码的**唯一落地**就是 `business/notice/**` 里的 `#[nik::permission]`。搬出 `app/src` ⇒
N14 看不到 ⇒ 13 行菜单（`V0045` 10 行 + `V0047` 2 行 + `V0133` 1 行）里的 9 个码判成死码。

**建议（并入 C2 扫描根收口，与 P2-54/P2-55 同一条）**：`Repo::load` 的扫描根扩成
`app/src + crates/*/src`，`scope_gate.py` 的 `SCAN_DIRS`、`layering_tests` 的目录枚举同源化。
这是**所有域抽包卡的共同前置**，不是 notice 独有。

## 7. 验证与基线（真库全绿 + 假绿对照 + 门禁）

```bash
# 场景 A（99 上跑，连 145 的 PG）
PGPASSWORD=*** createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_p256_notice_drill   # 182 表
ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p256_notice_drill" \
HR_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p256_notice_drill" \
  cargo test -p cloudpivot --lib 'business::notice' -- --test-threads=1
```

```
test result: ok. 77 passed; 0 failed; 0 ignored; 0 measured; 5604 filtered out; finished in 2.41s
```

- **真库真的连上了**：2.41s、`SKIP` 行 **0 条**，2 条真库用例（`internal_alert_persists_one_notice_to_super_admin`、
  `alert_dedup_ten_minute_window`）实跑。
- **假绿对照**（同一条命令，`env -u ERP_DB_URL -u HR_DB_URL`）：

  ```
  test result: ok. 77 passed; 0 failed; 0 ignored; 0 measured; 5604 filtered out; finished in 0.01s
  ```

  **同样 77 passed，但 0.01s（差 240 倍）** —— notice 的真库入口是 `handler.rs:1202 ensure_db()`
  （`ERP_DB_URL` 未设 → `None` → 用例提前 return，`println!("SKIP: …")` 被 libtest 默认捕获，
  **日志里看不到**）。判真跑只能看**耗时量级**。
- 演练库已清理：`cp_p256_notice_drill` 已 `DROP`。

门禁基线（搬迁前，全绿）：

```bash
$ python3 scripts/lint/scope_gate.py                       → exit=0（PASS：跳过清单与 SKIP_MANIFEST 逐条一致，160 条；0 处越界）
$ python3 scripts/lint/path_refs_gate.py                   → exit=0（899 个脚本，10 条白名单，0 处失效路径）
$ python3 scripts/lint/module_cycle_report.py              → exit=0
$ cd backend && cargo build --workspace --offline          → exit=0（Finished dev in 0.12s）
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md → exit=0（C5 3/3 OK；全序 66 行）
$ python3 scripts/verify/crate_split_readiness.py --domain notice --layer-check → exit=1（74 回边 / 7 源；本步要切 3 行）
```

`cargo fmt`：本卡**零 Rust 变更**（只新增本文档），无格式面；且 `/opt/cloudpivot` 是多 worker 共享工作区，
跑全仓 `cargo fmt` 会改到别人的 WIP ⇒ 未跑。

## 8. 下一轮怎么开（直接开 C5，卡内 4 包）

1. **前置**：C1 `cloudpivot-kernel`（shared/utils/business::shared）、C3 `cloudpivot-support`。二者未落地前
   开工只会得到反向依赖（现在 `backend/crates/` 只有 `cloudpivot-doc`）。
2. **卡内 4 包与顺序**：`docpdf` → `pdf`（卡内 3 行：docpdf→pdf）→ `notice` → `monitor`
   （monitor 与三包无互指，放最后；它自己 2 行 `support` 由 C3 前置抵付）。
3. **notice 本步要切 3 行**：`cleanup.rs:22` / `scheduled.rs:38` / `sse_hub.rs:67` 的
   `use crate::task::Scheduler;` —— 注册签名上提（§3.1），与 C1 施工同批定。
4. **装配层 2 行随 C2 搬**：`lib.rs:94` 的 `.post("/internal/alert", …)` 与 `lib.rs:100` 的
   `sse_hub::shutdown()`。搬包后 bin 侧要改成 `cloudpivot_notice::handler::internal_alert` /
   `cloudpivot_notice::sse_hub::shutdown()`；**手工路由必须在 C2 的装配顺序里保留**（豁免 `prefix` + 回环闸两条语义），
   漏了就等于把 `/internal/alert` 从对外面挪走或暴露。
5. **改 import 的面**：19 行出边（16 行 C1 + 3 行 task）+ 76 行入边（prod 72 / test 4）→
   `crate::business::notice::` → `cloudpivot_notice::`；顺手改 12 条入边注释 + 2 条出边注释的 rustdoc 链接，
   以及 §3.2 的 5 行 `crate::chrono`、1 行 `crate::run`、1 行 `crate::business::announcement`。
6. **门禁同批改**（否则 C5 一落地就红）：§6 的 #1（N14 扫描根）、#2（N16/N21 路由扫描面）、#4（`MIN_DOMAINS`）。
   #3 是**静默**的，必须靠 #1 同源化兜住。
7. **卡面口径修正**：C5 写 4 个包，但 `--plan-order` 只解析出 3 个 crate（`monitor` 缺）——C5 卡面请按 4 包写，
   并在验收里加一条 `--domain monitor --layer-check`（今天报 2 行 support）。

## 附录 A：核对清单（本卡实测 vs 计划表）

| 项 | 计划表（§P2-C.3 C5 行 / P2-42 §4） | 本卡实测 | 结论 |
|:--|:--|:--|:--|
| 文件 / 行 | 11 / 8 224 | 11 / 8 224 | 一致 |
| SCC | 24 | 24（同环 23 节点） | 一致 |
| 入边 | 74（7 源） | 74 工具 + 2 `lib.rs` 装配 = **76 行代码**（prod 72 / test 4） | 一致；工具口径不含 `app#root`，计划表未区分 prod/test |
| 出边 | 目标 1 / 3 行 | 4 目标 / 19 行代码（内核 16 + task 3） | 一致（计划表的「1 目标」= 扣掉内核后的 task） |
| 本步要切 | 3 行：task 3 | 3 行：task 3（`--layer-check` 与 `--plan-order` 双口径一致） | 一致 |
| 路由 | C5 22（四包合计） | notice 18（17 宏 + 1 手写）；宏路由口径 17 → 17+3+0+2 = 22 | 一致；**手写路由计划表未计** |
| `include_str!` | C5 2 | notice **0**（2 处属 pdf/docpdf） | 一致 |
| 夹具引用 | 0 | 0 | 一致 |
| 真库用例 | 0（按 `*_db_tests.rs` 文件数） | 0 文件，但**行内 2 条** | 建议 C5 卡面写明「notice 真库用例 2 条，必须带库跑」 |

## 附录 B：本卡未做的事（明确边界）

- 未建 `backend/crates/cloudpivot-notice/`，未搬任何文件，未改任何 `Cargo.toml`；
- 未改任何 import（19 行出边 + 76 行入边原样保留）、未改 14 条 rustdoc 链接；
- 未改门禁脚本（§6 的 5 处只是**报告**，改动随 C2 的扫描根收口一起做）；
- 未新增迁移；5 张表 / 9 个权限码 / 13 行菜单 / 9 个迁移版本的库内状态不变。
