# P2-53 任务域抽 crate 收口 —— 作废（`task` 按 §P2-C 并入 C14 `cloudpivot-task`）

> 卡：`t_63e5521f`（P2-53【P2-C】抽 `task` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `088c0586`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41`（`11f490f2`）、`P2-42`（`e9a31e96`）、`P2-43`（`939ec719`）、`P2-44`（`810ce2c7`）、
> `P2-45`（`5a2bc55a`）、`P2-46`（`ca11c5dc`）、`P2-47`（`4648b330`）、`P2-48`（`6a473699`）、
> `P2-49`（`c1f7aa76`）、`P2-50`（`4b95fad3`）、`P2-51`（`0cc8554c`）、`P2-52`（`088c0586`）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-task/`，不搬 55 个文件 / 29 119 行，不改任何
`crate::business::task` / `crate::task` import，不动 `backend/app/Cargo.toml`，不动 `backend/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；
卡面验收命令 `cargo test -p cloudpivot-task` 因此按预期失败（不是回归，见 §5.2）。

权威依据已入仓：

- `docs/planning/20261113-新卡清单.md` §P2-C.6：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同文件 §P2-C.3 的 **C14** 行：`cloudpivot-task`｜节点 **`business/task` + `task`（一个包）**｜前置
  **C1、C2、C3、C4、C5、C6、C7、C9、C10、C12、C13**（11 张）｜回边 **19 / 28**｜**本步要切 0 行**｜
  机械量 `55 / 29 119 · 51 · 1 · 0 · 0`；
- 同节 §P2-C.1（判据看**出边**）、§P2-C.4（每张卡通用施工单，第 1 步「先付解环账」）、
  §P2-C.5（为什么**不**走「域 crate 反向依赖单体」）；
- `P2-42` §4 台账第 **92** 行（P2-53）= `53 / 27 688`｜SCC 24｜入边 `19（6）`｜出边目标 10｜
  `37 行`｜`退 1`｜落点 **`C14 cloudpivot-task`**。

**本卡与前面 11 张作废卡的区别，先写在最前面**（三点都是实测，不是推断）：

1. **`task` 确实是 C14 的落地对象**（`business/task` + `src/task` 一个包），不是「永远留在单体」；
2. **它是 15 张卡里唯一「本步要切 0 行」的一张**：不是没人引用它，而是**引用它的 10 张卡各自卡面
   已经把这几行写进自己的解环账**（C1 1 / C3 5 / C4 1 / C5 3 / C6 1 / C7 1 / C9 2 / C10 1 / C12 2 /
   C13 12 = **29 行**，逐条对得上，见 §3.4）。也就是说：**照 P2-53 卡面先把 `task` 抽出来，等于要求
   `cloudpivot-task` 反向依赖 9 个域 + 内核的这 29 行** —— 这正是 §P2-C.5 明确拒绝的过渡路线；
3. **卡面里的「task」是两个节点的合称**：工具节点 `business/task`（53 文件 / 27 688 行）与
   `src/task`（2 文件 / 1 431 行）。只按字面把 `src/task` 抽走**更糟**：它自己 45 行出边 / 13 目标、
   28 行入边 / 12 源，且是全仓定时任务的注册中心（§4.4）。C14 口径（一个包两个节点）是唯一成立的口径。

换句话说：**本卡的卡面工作量描述不成立**（照它做只能抽成「域包 → 单体」的反向依赖），
而 C14 卡的描述是成立的。下一轮要重开，请直接开 C14，别重开 P2-53。

## 2. `task` 的实测（只读、无库依赖，2026-11-15 于 99）

### 2.1 工具原始输出（逐字）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain business/task --layer-check
域：business/task（scope=top）
  规模：53 个文件 / 27688 行
  SCC：24 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（23）：aiagent, announcement, attachment, business/shared, changelog, docpdf, erp,
    flow, help, hr, impexp, masking, middleware, mutex, notice, pdf, permission, shared, storage,
    support, system, task, utils
  回边（留在应用 crate 的代码 → 本域）：19 处代码级
    app#root               code=  1 all=  1  bootstrap.rs:139
    dashboard              code=  1 all=  2  business/dashboard/model.rs:28
    erp                    code=  4 all=  4  business/erp/linkage/actions.rs:647, actions.rs:651,
                                             actions.rs:677, linkage/spec.rs:1951
    project                code=  2 all=  6  business/project/handler.rs:44, business/project/model.rs:33
    task                   code=  2 all=  2  task/mod.rs:281, task/mod.rs:312
    taskflow               code=  9 all=  9  business/taskflow/approval_impl.rs:42, 287, bridge.rs:49,
                                             50, 2291, 2292, spawn.rs:31, delete_cancel_tests.rs:13, 511
  出边（本域 → 其它）：13 个目标
    business/shared        code= 33 all= 34
    shared                 code= 23 all= 25
    utils                  code= 19 all= 19
    notice                 code=  6 all=  6
    aiagent                code=  6 all=  6
    middleware             code=  6 all=  8
    erp                    code=  5 all=  5
    changelog              code=  4 all=  4
    storage                code=  3 all=  4
    task                   code=  2 all=  4
    docpdf                 code=  2 all=  2
    pdf                    code=  2 all=  2
    system                 code=  1 all=  1
  层次判据：本步要切 10 条 / 37 行 —— 出边指向更下层以外：notice(6), aiagent(6), middleware(6), erp(5),
            changelog(4), storage(3), task(2), docpdf(2), pdf(2), system(1)
            （假设 L1 内核 shared, utils, business/shared 已抽出、装配层 bin 已独立成包）
  结论：不可抽（19 处代码级回边，6 个来源）
exit=1
```

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain task --layer-check
域：task（scope=top）
  规模：2 个文件 / 1431 行
  SCC：24 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  回边（留在应用 crate 的代码 → 本域）：28 处代码级
    announcement           code=  1 all=  3  business/announcement/maintenance.rs:34
    app#root               code=  1 all=  2  bootstrap.rs:592
    business/task          code=  2 all=  4  business/task/hermes_sync.rs:53,
                                             business/task/recurring.rs:318
    erp                    code=  8 all= 10  business/erp/credit.rs:560, depreciation/depreciation_model.rs:814,
                                             payable/payable_bridge.rs:590, 916, reminder.rs:98,
                                             stockin/stockin_bridge.rs:317, transfer/transfer_overdue.rs:60,
                                             voucher/voucher_bridge.rs:457
    flow                   code=  2 all=  2  business/flow/maintain.rs:79, business/flow/timeout.rs:55
    help                   code=  1 all=  1  business/help/model.rs:41
    impexp                 code=  2 all=  4  business/impexp/cleanup.rs:34, business/impexp/worker.rs:55
    notice                 code=  3 all=  5  business/notice/cleanup.rs:22, scheduled.rs:38, sse_hub.rs:67
    shared                 code=  1 all=  1  shared/observability.rs:427
    storage                code=  1 all=  2  business/storage/cleanup.rs:25
    support                code=  5 all=  7  support/job/handler.rs:392, 481, 563, 948, 962
    system                 code=  1 all=  2  system/auth/cleanup.rs:35
  出边（本域 → 其它）：13 个目标
    erp                    code= 17 all= 17
    support                code=  8 all=  8
    shared                 code=  5 all=  5
    notice                 code=  3 all=  3
    impexp                 code=  2 all=  2
    business/task          code=  2 all=  2
    flow                   code=  2 all=  2
    utils                  code=  1 all=  1
    storage                code=  1 all=  1
    announcement           code=  1 all=  1
    changelog              code=  1 all=  1
    help                   code=  1 all=  1
    system                 code=  1 all=  1
  层次判据：本步要切 11 条 / 39 行 —— 出边指向更下层以外：erp(17), support(8), notice(3), impexp(2),
            business/task(2), flow(2), storage(1), announcement(1), changelog(1), help(1), system(1)
  结论：不可抽（28 处代码级回边，12 个来源）
exit=1
```

三个退出码（本节全部实测）：

| 口径 | 命令 | 退出码 | 含义 |
|:--|:--|:--:|:--|
| 层次判据（推荐） | `--domain business/task --layer-check` | **1** | 出边 10 条 / 37 行落在「更下层」之外 ⇒ 现在抽会成环 |
| 层次判据（同包另一半） | `--domain task --layer-check` | **1** | 出边 11 条 / 39 行（其中 2 行指回 `business/task`，包内） |
| 旧口径（方向反了） | `--domain task --check` | 1 | 数的是入边 28，恒不为 0，无法区分「可抽/不可抽」 |
| 卡序模拟 | `--plan-order docs/planning/20261113-新卡清单.md` | **0** | 全序累计 66 行；C14 一行不切 ⇒ **按卡序抽可行** |

`--plan-order` 的关键几行（逐字）：

```text
C1     cloudpivot-kernel                            10    10   OK
                                              切向：support(3), system(2), aiagent(2), task(1), erp(1), storage(1)
C3     cloudpivot-support                            6     6   OK
                                              切向：task(5), system(1)
C4     cloudpivot-storage, cloudpivot-attachment        1     1   OK
                                              切向：task(1)
C5     cloudpivot-notice, cloudpivot-pdf, cloudpivot-docpdf        3     3   OK
                                              切向：task(3)
C6     cloudpivot-changelog, cloudpivot-permission, cloudpivot-announcement, cloudpivot-mutex        4     4   OK
                                              切向：impexp(1), masking(1), system(1), task(1)
C7     cloudpivot-system                             7     7   OK
                                              切向：masking(5), task(1), flow(1)
C9     cloudpivot-impexp                            13    13   OK
                                              切向：hr(11), task(2)
C10    cloudpivot-help, cloudpivot-aiagent           1     1   OK
                                              切向：task(1)
C12    cloudpivot-flow                               2     2   OK
                                              切向：task(2)
C13    cloudpivot-erp                               12    12   OK
                                              切向：task(8), business/task(4)
C14    cloudpivot-task                               0     0   OK
全序累计要切：66 行
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

**读法**：C14 的 0 行不是「`task` 谁都不引用」，而是「引用它的每一张卡，在**自己抽出去的时候**先把
那几行倒转成接缝了」——C1 1 + C3 5 + C4 1 + C5 3 + C6 1 + C7 1 + C9 2 + C10 1 + C12 2 + C13 12 = **29 行**，
加上 bin（C2，合法方向）2 行、更晚的 C15 12 行（taskflow 9 / project 2 / dashboard 1）⇒ 43 行入边全部有着落
（§3.4 是逐条对账）。

### 2.2 SCC 成员资格：两个节点都在顶层 24 节点环里，且**互指 4 行**

| | P2-42~P2-52（`erp` 子域 / `flow`） | **P2-53（`business/task` + `task`）** |
|:--|:--|:--|
| SCC 口径 | `--scope erp-sub` ⇒ 51 节点 / `--scope top` ⇒ 24 节点 | `--scope top` ⇒ **24 节点**（默认） |
| 环的性质 | `erp` 内部咬合 / 顶层 24 个域互相咬合 | 顶层 24 个域互相咬合 |
| 解环方向 | 无（C13 整包搬）/ 按层次序每卡付自己那几行 | **按 C1→C15 层次序，本卡 0 行；但 29 行由前置卡代付** |
| 包内关系 | — | **两个节点互相引用 4 行** ⇒ 必须同包 |

```
business/task/hermes_sync.rs:53   use crate::task::Scheduler;                        # 业务侧 cron 注册的签名
business/task/recurring.rs:318    pub fn register_builtin(scheduler: &crate::task::Scheduler)
task/mod.rs:281                   crate::business::task::hermes_sync::register_builtin(self);
task/mod.rs:312                   crate::business::task::recurring::register_builtin(self);
```

两个节点若各抽一包 ⇒ `cloudpivot-task-a ⇄ cloudpivot-task-b` 直接成环（Cargo 报
`cyclic package dependency`）。**这是全链 15 张卡里唯一一处「双向咬合、且两边都必须在同一步内解决」**，
也是 C14 卡面写「`business/task` + `src/task` 一个包」的实证依据（不是排版偏好）。

### 2.3 workspace 现状：`cloudpivot-task` 不存在，`task` 还在单体里

```bash
$ cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1 \
    | jq -r '"workspace_root=\(.workspace_root)", (.packages[]|"  \(.name) -> \(.manifest_path)"), "members=\(.workspace_members|length)"'
workspace_root=/opt/cloudpivot/backend
  cloudpivot -> /opt/cloudpivot/backend/app/Cargo.toml
  cloudpivot-doc -> /opt/cloudpivot/backend/crates/cloudpivot-doc/Cargo.toml
members=2
```

即：workspace 仍是 P2-40（`35a306be`）落地时的 2 个包（P2-40 的卡面依赖已满足），
**没有** `cloudpivot-task`；C14 的 11 张前置卡（C1~C7、C9、C10、C12、C13）**一张都还没交付**
（`crates/` 里只有 `cloudpivot-doc`）。

### 2.4 规模与机械量（本次实测，与计划表并排）

| 项 | 计划表 §P2-C.3（C14 行） | 本次实测 | 复核口径 |
|:--|--:|--:|:--|
| 文件 / 行 | 55 / 29 119 | **55 / 29 119**（53 / 27 688 + 2 / 1 431） | `find business/task task -name '*.rs'` |
| 占 `backend/app/src` 比例 | — | **4.95%**（29 119 / 587 800） | `app/src/**/*.rs` 共 790 个文件 |
| 路由宏（生产区） | 51 | **51** | `#[nik::get/post/put/delete/patch(`，全部 `/business/task/…`；51 个 **(method, path)** 组合无重复 |
| `#[nik::permission]` | — | **47** | 51 − 4：35 条多行写法 + 12 条单行；码分布 read 32 / update 4 / create 4 / delete 2 / status 2 / assign 1 / subtask 1 / progress 1 |
| `#[nik::no_auth]` | — | **4** | 双身份路由（人类 JWT 或 AI 任务令牌）：`{id}`、`{id}/attachment`、`{id}/comment`、`{id}/ai-progress`；权限在 handler 内 `ensure_task_read_permission` 判 |
| `#[nik::auth_only]` | — | **0** | 无；`/health` 也没有（不是探针模块） |
| 模块声明 | — | **40** | `business/task/mod.rs` 38 条 + `task/mod.rs` 2 条 |
| 真库用例文件（`*_db_tests.rs`） | 1 | **1** | `task_handler_copy_db_tests.rs`（3 条用例，靠 `ERP_DB_URL`/`TASK_DB_URL` 开关） |
| 出目录引用（`include_str!` 带 `../`） | 0 | **0** | 仅 1 处 `include_str!("approval_port.rs")`（同目录，搬包后不变） |
| 夹具引用（计划表口径） | 0 | **0** | `test_helpers\|fixture_hygiene\|SEED_ROLE_LOCK\|ensure_any_` 在 `business/task` + `task` 内 **0 命中** |
| 测试面 | — | **15 个测试文件 + 36 个 `#[cfg(test)]` 块** | 文件口径 `*test*.rs`；17 个文件含内联测试 |
| 用例总数（实跑，含真库） | — | **262**（250 + 12） | 见 §5.3 |

> 夹具引用 0 是本卡的好消息：`task` 不碰内核的 `shared::test_helpers`、也没有自己的 `fixture_hygiene`，
> C14 搬迁时**没有「`#[cfg(test)]` 夹具跨包不可见」的坑**（§P2-C.5 第 3 条最重的陷阱）。
> 唯一的真库文件用 `ERP_DB_URL`/`TASK_DB_URL` 环境变量开关 + 自带 `DB_LOCK`，属自包含。

## 3. 逐行复核：入边 43 行 / 出边 153 行（并核对工具口径）

判据脚本：把 `*test*.rs` 整文件 + **内联 `#[cfg(test)]` 块**（按花括号配平）识别为测试区，`//` 注释单列，
字符串字面量按「代码里没有路径」剔除；对 `crate::business::task`、`crate::business::{…task…}`（花括号）、
`business::task`（裸）、`super::task`、`crate::task`（`src/task`）五种写法全扫，并把两个节点合成一个包统计。

### 3.1 出边（合并成包后 153 行 = 14 个目标；内核 81 + 非内核 72）

单节点工具口径 112（`business/task`）+ 45（`task`）= 157；**减去包内互指 4 行** ⇒ 对外 **153 行**
（prod **134** + test **19**）：

| 目标 | 归属包（卡） | `business/task` | `task` | 合计 | 包内？ | C14 时能不能存在 |
|:--|:--|--:|--:|--:|:--:|:--|
| `business::shared` | **L1 内核**（C1） | 33 | 0 | **33** | | ❌ 等 C1 |
| `shared`（`src/shared`） | **L1 内核**（C1） | 23 | 5 | **28** | | ❌ 等 C1 |
| `utils`（`src/utils`） | **L1 内核**（C1） | 19 | 1 | **20** | | ❌ 等 C1 |
| `business::erp` | **C13**（前置） | 5 | 17 | **22** | | ✅ 合法向下箭头 |
| `business::notice` | **C5**（前置） | 6 | 3 | **9** | | ✅ 合法向下箭头 |
| `support` | **C3**（前置） | 0 | 8 | **8** | | ✅ 合法向下箭头 |
| `business::aiagent` | **C10**（前置） | 6 | 0 | **6** | | ✅ 合法向下箭头 |
| `middleware` | **C2**（前置，装配层） | 6 | 0 | **6** | | ✅ 合法向下箭头 |
| `business::changelog` | **C6**（前置） | 4 | 1 | **5** | | ✅ 合法向下箭头 |
| `business::storage` | **C4**（前置） | 3 | 1 | **4** | | ✅ 合法向下箭头 |
| `business::docpdf` | **C5**（前置） | 2 | 0 | **2** | | ✅ 合法向下箭头 |
| `business::pdf` | **C5**（前置） | 2 | 0 | **2** | | ✅ 合法向下箭头 |
| `system` | **C7**（前置） | 1 | 1 | **2** | | ✅ 合法向下箭头 |
| `business::flow` | **C12**（前置） | 0 | 2 | **2** | | ✅ 合法向下箭头 |
| `business::impexp` | **C9**（前置） | 0 | 2 | **2** | | ✅ 合法向下箭头 |
| `business::announcement` | **C6**（前置） | 0 | 1 | **1** | | ✅ 合法向下箭头 |
| `business::help` | **C10**（前置） | 0 | 1 | **1** | | ✅ 合法向下箭头 |
| `crate::task`（`src/task`） | **包内** | 2 | — | 2 | ✅ | 同包 |
| `business::task` | **包内** | — | 2 | 2 | ✅ | 同包 |
| **合计** | | **112** | **45** | **157 − 4 = 153** | | 内核 81 / 非内核 72 / 包内 4 |

prod/test 拆分（本次按内联 `#[cfg(test)]` 配平重算，比文件名口径更严）：

| 切片 | prod | test | 合计 |
|:--|--:|--:|--:|
| `business/task` 出边（13 目标） | 97 | 15（`business/shared` 9、`shared` 4、`aiagent` 1、`system` 1） | 112 |
| `task` 出边（13 目标） | 41 | 4（`erp` 3、`shared` 1） | 45 |
| **合并后对外** | **134** | **19** | **153** |
| 其中 L1 内核（C1） | 67 | 14 | 81 |
| 其中非内核（C2~C13） | 67 | 5 | 72 |

校验：81（内核）+ 72（非内核）= 153。`--layer-check` 的两份 37 + 39 = 76 行 = 72（非内核）+ 4（包内互指）
—— 它只扣内核，不扣「同包」与「后抽的包」。

**关键**：非内核 72 行的 14 个目标，**全部**落在 C14 的前置集合
{C1, C2, C3, C4, C5, C6, C7, C9, C10, C12, C13} 里 —— 没有一行指向「更晚的、还在单体的域」。
这正是 `--plan-order` 判 C14 `0 行` 的原因，也说明**卡序是对的、卡号是错的**。

### 3.2 非内核 72 行逐行（`business/task` 35 行 + `task` 37 行；`T` = 测试区）

#### 3.2.1 `business/task` → 非内核（35 行：prod 33 + test 2）

| # | 位置 | 引用 | 用途 | 归属 |
|--:|:--|:--|:--|:--|
| 1–2 | `hermes_sync.rs:46/1036` | `aiagent::hermes::{Hermes, KanbanCreate, KanbanSnapshot}`、`aiagent::model::find_by_user` | AI 派活 / 状态同步 | C10 |
| 3 T | `hermes_sync.rs:1188`（内联 `mod tests`） | `aiagent::hermes::validate_idempotency_key` | 幂等键单测 | C10 |
| 4–5 | `task_handler_ai.rs:216/237` | `aiagent::model::find_by_user`、`aiagent::hermes::Hermes::from_env` | AI 执行者解析 | C10 |
| 6 | `workspace_files.rs:67` | `aiagent::hermes::HermesConfig` | AI 工作区文件 | C10 |
| 7–10 | `schedule.rs:56`、`task_handler_assign.rs:16`、`task_handler_hierarchy.rs:15`、`task_handler_status.rs:18` | `changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput, RecordOutcome}` | 状态 / 转派 / 层级变更留痕 | C6 |
| 11–12 | `task_pdf.rs:62/63` | `docpdf::doc_pdf_model`、`docpdf::doc_pdf_render::{self, RenderedDoc}` | 任务单 PDF 模型 | C5 |
| 13–14 | `task_pdf.rs:64/65` | `pdf::handler as pdf_handler`、`pdf::template::{…}` | 任务单 PDF 渲染/落盘 | C5 |
| 15–19 | `task_handler_notice.rs:229/230/238/239/242` | `erp::linkage::engine::{LinkageContext, status_notice}`、`erp::linkage::spec::TRIGGER_TASK`、`erp::linkage::NoticeRoute` | 任务状态 → ERP 联动通知路由 | C13 |
| 20–25 | `collab.rs:65/66`、`hermes_sync.rs:47/48`、`task_handler_notice.rs:11/12` | `notice::dto::{NoticeLevel, NoticeType}`、`notice::{self, NoticeDraft, NoticeTarget}` | 任务通知扇出（协作人/@/打回） | C5 |
| 26–28 | `collab.rs:68`、`hermes_sync.rs:50`、`task_handler_attachment.rs:14` | `storage` | 附件/工作区文件走存储模块 | C4 |
| 29–34 | `hermes_sync.rs:380`、`task_handler_ai.rs:19`、`task_handler_attachment.rs:18`、`task_handler_comment.rs:18`、`task_handler_common.rs:18`、`task_handler_crud.rs:24` | `middleware::task_token::{new_token, AiTaskActor}` | 任务级令牌（AI 双身份） | C2 |
| 35 T | `task_model_tests.rs:116` | `system::badge::model::TASK_TODO_WINDOW_DAYS` | 角标窗口常量单测 | C7 |

#### 3.2.2 `task`（`src/task`）→ 非内核（37 行：prod 34 + test 3）

| # | 位置 | 引用 | 用途 | 归属 |
|--:|:--|:--|:--|:--|
| 36 | `task/mod.rs:249` | `announcement::maintenance::register_builtin(self)` | 公告三个 cron | C6 |
| 37 | `task/mod.rs:254` | `changelog::partition` | 变更日志分区运维 cron | C6 |
| 38–45 | `task/mod.rs:285/288/292/297/302/303/305/308` | `erp::transfer::transfer_overdue`、`erp::reminder`、`erp::stockin::stockin_bridge`、`erp::payable::payable_bridge::{register_builtin, register_cancel_builtin}`、`erp::voucher::voucher_bridge`、`erp::depreciation::depreciation_model`、`erp::credit` | 8 个 ERP 内置 cron 注册 | C13 |
| 46–51 | `task/mod.rs:596/634/635/636/653/654` | `erp::reminder::{RemindKind, run_kind}`、`erp::credit::{TASK_NAME, recompute_all}` | 手动触发 `sys_job` 时的分发（`invoke_target` 分派表） | C13 |
| 52–54 T | `task/mod.rs:726/759/767`（内联 `mod tests`） | `erp::reminder::RemindKind::ALL` | 分派表单测 | C13 |
| 55–56 | `task/mod.rs:283/310` | `flow::timeout::register_builtin`、`flow::maintain::register_builtin` | 审批超时扫描 / 图快照自检 cron | C12 |
| 57 | `task/mod.rs:279` | `help::model::register_builtin` | 帮助历史版本清理 cron | C10 |
| 58–59 | `task/mod.rs:246/247` | `impexp::worker`、`impexp::cleanup::register_builtin` | 导出执行 / 回收 cron | C9 |
| 60–62 | `task/mod.rs:240/242/244` | `notice::{cleanup, sse_hub, scheduled}::register_builtin` | 过期通知清理 / SSE 回收 / 定时投递 cron | C5 |
| 63 | `task/mod.rs:238` | `storage::cleanup::register_builtin` | 存储对象回收 cron | C4 |
| 64–71 | `task/mod.rs:60/61/322/594/644/646(×2)/649` | `support::job::model::{JobLogStatus, SysJobLogModel, SysJobModel}`、`support::log::model`、`support::invariant::runner` | 动态任务装载（`sys_job`）/ 运行日志 / 账务巡检 cron | C3 |
| 72 | `task/mod.rs:316` | `system::auth::cleanup::register_builtin` | 令牌吊销表清理 cron | C7 |

> 单行两处：`task/mod.rs:646` 同一行出现两次 `crate::support::invariant::runner`（`run_and_store(...Trigger::Manual)`），
> 按**行数**计 `task` 出边是 42 行 / 按**处数**是 43（含包内 2 行）= 工具口径 45。
> 本卡表里一律按处数（与工具一致），并在 §3.1 注明。

### 3.3 入边：其它 → `cloudpivot-task`（43 行 / 14 源；包内 4 行不计）

| 来源节点 | prod | test | 代码合计 | 行位置 | 内容 |
|:--|--:|--:|--:|:--|:--|
| `business/erp` | 11 | 1 | **12** | `task`：`credit.rs:560`、`depreciation_model.rs:814`、`payable_bridge.rs:590/916`、`reminder.rs:98`、`stockin_bridge.rs:317`、`transfer_overdue.rs:60`、`voucher_bridge.rs:457`；`business/task`：`linkage/actions.rs:647/651/677`；`linkage/spec.rs:1951`（**测试**） | 8 个 `register_builtin(&Scheduler)` 签名 + 联动建任务（`month_key`/`NewTask`/`create_task`）；测试 1 行是 `TaskStatus` 中文名单测 |
| `business/taskflow` | 4 | 5 | **9** | `approval_impl.rs:42`、`bridge.rs:49/50`、`spawn.rs:31`（prod）；`approval_impl.rs:287`、`bridge.rs:2291/2292`、`delete_cancel_tests.rs:13/511`（测试） | 用 `task::model` / `task::state` / `approval_port` 推进验收 |
| `support` | 3 | 2 | **5** | `job/handler.rs:392/481/563`（prod）；`948/962`（测试） | 运行时 API：`scheduler().run_job`、`scheduler().reload()`、`is_registered_target` |
| `business/notice` | 3 | 0 | **3** | `cleanup.rs:22`、`scheduled.rs:38`、`sse_hub.rs:67` | `register_builtin(&Scheduler)` 签名 |
| `app#root`（bin） | 2 | 0 | **2** | `bootstrap.rs:139`（`task::approval_port::port_name()`）、`bootstrap.rs:592`（`task::scheduler().reload()`） | 启动装配 |
| `business/project` | 2 | 0 | **2** | `handler.rs:44`、`model.rs:33` | 项目页复用任务 DTO/model |
| `business/flow` | 2 | 0 | **2** | `maintain.rs:79`、`timeout.rs:55` | `use crate::task::Scheduler;` |
| `business/impexp` | 2 | 0 | **2** | `cleanup.rs:34`、`worker.rs:55` | 同上 |
| `business/dashboard` | 1 | 0 | **1** | `model.rs:28` | 待办统计 |
| `business/announcement` | 1 | 0 | **1** | `maintenance.rs:34` | 同上 |
| `business/help` | 1 | 0 | **1** | `model.rs:41` | 同上 |
| `business/storage` | 1 | 0 | **1** | `cleanup.rs:25` | 同上 |
| `shared`（**内核**） | 0 | 1 | **1** | `observability.rs:427`（`#[test] fn every_sampler_metric_is_registered`） | `/metrics` 目录自检：采样器写出的每个名字必须已登记 |
| `system` | 1 | 0 | **1** | `auth/cleanup.rs:35` | 同上 |
| **合计** | **34** | **9** | **43** | 43 个不同行（无一行两处） | 另有 **21 行注释**（见下） |

**两条结构性发现（本次复核新增，对 C1/C13 施工有直接影响）**：

1. **内核 → `task` 的 1 行是测试边**：`shared/observability.rs:427` 在 `#[test] fn every_sampler_metric_is_registered`
   里，断言「采样器写出的指标名都已在 `observability::CATALOG` 登记」。⇒ **内核生产代码对 `task` 零依赖**，
   C1 那 1 行只需把断言改走 `test-support` feature（或让内核只暴露 `CATALOG`、断言留在域内），不是接缝。
2. **C13 的 4 行 `business/task` 里 1 行是测试**（`linkage/spec.rs:1951` 的 `TaskStatus` 中文名单测），
   生产边只有 3 行（`linkage/actions.rs:647/651/677` 的「联动建任务」）—— C13 施工时的接缝面比卡面写的小。

**注释口径**：本卡按**行**统计注释 **21 行**（`business/task` 侧 7：`project` 4、`shared` 2、`dashboard` 1；
`task` 侧 14：`announcement` 2、`erp` 2、`impexp` 2、`notice` 2、`support` 2、`bootstrap` 1、
`changelog` 1、`storage` 1、`system` 1）。工具只把**有代码边**的来源列进回边表，所以两处纯注释边
（`shared/error_code.rs:572/585` 指向 `business::task::doc_ref`、`changelog/partition.rs:16` 指向
`crate::task::Scheduler`）在它的输出里不出现 —— 21 = 18（工具 `all−code` 合计）+ 3（这两条纯注释边）。

### 3.4 入边逐条对账：43 行全部被某张卡面认领（本卡 0 行）

| 源 | 行数 | 归属包（卡） | 卡面怎么写的 | 对得上？ |
|:--|--:|:--|:--|:--:|
| `business/erp` | 12 | **C13** | 「**12 行**：task 8、business/task 4」 | ✅ 8 + 4 |
| `business/taskflow` | 9 | C15（**后抽**） | C15 前置含 C14 ⇒ 合法向下箭头，不需付 | ✅ |
| `support` | 5 | **C3** | 「**6 行**：task 5、system 1」 | ✅ 5 |
| `business/notice` | 3 | **C5** | 「**3 行**：task 3」 | ✅ 3 |
| `app#root`（bin） | 2 | **C2** | 装配层：出边不算账 | ✅ |
| `business/project` | 2 | C15（后抽） | 同上 | ✅ |
| `business/flow` | 2 | **C12** | 「**2 行**：task 2」 | ✅ 2 |
| `business/impexp` | 2 | **C9** | 「**13 行**：hr 11、task 2」 | ✅ 2 |
| `business/dashboard` | 1 | C15（后抽） | 同上 | ✅ |
| `business/announcement` | 1 | **C6** | 「**4 行**：impexp 1、masking 1、system 1、task 1」 | ✅ 1（C6 包内 announcement 这一行） |
| `business/help` | 1 | **C10** | 「**1 行**：task 1」 | ✅ 1 |
| `business/storage` | 1 | **C4** | 「**1 行**：task 1」 | ✅ 1 |
| `shared`（内核） | 1 | **C1** | 「**10 行**：support 3、system 2、aiagent 2、**task 1**、erp 1、storage 1」 | ✅ 1（`observability.rs:427`，测试边） |
| `system` | 1 | **C7** | 「**7 行**：masking 5、task 1、flow 1」 | ✅ 1 |
| **合计** | **43** | | 前置卡代付 **29** + bin **2** + C15 后抽 **12** | **43/43** |

**这张表是本卡最硬的一条证据**：`task` 的 43 行入边，**没有一行需要 C14 自己付** —— 29 行已被
C1/C3~C13 的卡面认领（前置卡抽出去时必须把那几行倒转成接缝），2 行是装配层的合法方向，
12 行来自更晚的 C15（`taskflow`/`project`/`dashboard`，抽的时候 `cloudpivot-task` 已是包）。
反过来说：**P2-53 若先开工，这 29 行一行都没人认领**，`cloudpivot-task` 只能反向依赖 9 个单体内域 + 内核。

### 3.5 工具口径核对：本卡**零漏报、零假阳性**（与 P2-49/P2-50/P2-51 不同）

| 漏报形态 | 本域实测 | 证据 |
|:--|:--|:--|
| 花括号 `use crate::business::{task, …}` | **0 处** | 全仓 `crate::business::{[^}]*\btask\b` 在 `business/task` + `task` 之外为空 |
| 裸 `business::task`（无 `crate::`） | **0 处** | 代码行扫描 0 命中（注释不在此列） |
| `super::task` / `super::business::task` | **0 处** | 0 命中 |
| `crate::bootstrap`（P2-51 漏过的那类） | **本域 0 处** | 全仓唯一 1 处在 `business/hr/salary_bridge_db_tests.rs:74`，与本域无关 |
| 出边未识别目标 | **0 个** | 153 行 100% 落到已登记节点（14 个目标）；扫描器报的 2 个 `UNKNOWN`（`crate::chrono`×5 在 notice、`crate::module_enums!`×1 在 support）都在**别的域**，与本卡无关 |
| 字符串字面量假阳性 | **本域 0 处** | 8 处 `crate::business::…` 全在其他域的**独立性断言**里（`flow/task_independence_tests.rs` 的 `concat!("crate::business::", "task")` 等） |

逐行复核与工具**完全对齐**：

- 入边：工具 19（`business/task`）+ 28（`task`）= 47；包内 4 ⇒ **43** = 本表 43（43 个不同行）。
- 出边：工具 112 + 45 = 157；包内 4 ⇒ **153** = 本表 153（prod 134 / test 19）。
- 注释：21 行 = 工具 `all−code` 的 18 处 + 两条纯注释边 3 行（工具不回边表里列，见 §3.3 注）。

> 因此 §3.2 的 72 行 + §3.3 的 43 行可直接作为 C14 的**施工清单**，不必重扫；
> 工具唯一的已知缺陷仍是「`crate::bootstrap` 不映射到 app#root」与「纯注释边不进回边表」
> （本域代码边为 0 影响，见 P2-51/P2-52 的建议）。

## 4. 为什么本卡不能按卡面 4 步直接抽（五条，都带实证）

### 4.1 顺序不对：C14 的 11 张前置一张都没建，81 行内核引用无处可去

`task` 出边里 **81 行（53%）** 指向 L1 内核（`business::shared` 33 / `shared` 28 / `utils` 20）。
C1 之前，`crate::shared` / `crate::utils` / `crate::business::shared` 只存在于**单体 `cloudpivot` 包**里；
此时把 `task` 抽成 `cloudpivot-task` 并改 import，得到的是 `cloudpivot-task → cloudpivot`，
而 `cloudpivot`（app）同时必须 `→ cloudpivot-task`（否则 51 条路由的 `inventory` 项进不了链接单元
⇒ **静默 404**，§P2-C.4 第 6 步）⇒ **包级环**。这正是 §P2-C.5 明确「不采用」的过渡路线。

### 4.2 72 行非内核出边的目标，正好都是前置（C2~C13）—— 顺序对了它们才合法

`erp` 22 + `notice` 9 + `support` 8 + `aiagent` 6 + `middleware` 6 + `changelog` 5 + `storage` 4 +
`docpdf` 2 + `pdf` 2 + `system` 2 + `flow` 2 + `impexp` 2 + `announcement` 1 + `help` 1 = **72 行**，
目标**全部**在 C14 的前置集合里。**这不是巧合**：`--plan-order` 的卡序就是按出边闭包排的（§2.1 输出）。
所以本卡 `--layer-check` 报的 76 行「本步要切」里，**72 行是等前置卡就自动清零的**，4 行是包内互指。

### 4.3 「本步 0 行」不等于「不用付账」：29 行是前置卡代付的

§3.4 的 43 行入边逐条对账已经证明：C1（1）+ C3（5）+ C4（1）+ C5（3）+ C6（1）+ C7（1）+ C9（2）+
C10（1）+ C12（2）+ C13（12）= **29 行**，每一行都写在对应卡面的「本步要切」里。
P2-53 卡面只有「建骨架 / 移代码 / 加依赖 / 修 import」四步，**没有 §P2-C.4 第 1 步「先付解环账」**；
按它做，等于把 29 行解环账留给「以后」，而 Cargo 不允许「以后」——包图必须先无环。

### 4.4 卡面里的「task」有歧义；只抽 `src/task` 更糟：它是全仓 cron 注册中心

工具里 `task` 只指 `backend/app/src/task/`（2 文件 / 1 431 行），而业务任务在
`backend/app/src/business/task/`（53 文件 / 27 688 行）。若按字面只抽 `src/task`：

- 它自己 **45 行出边 / 13 目标**、**28 行入边 / 12 源**，且 11 个目标里的 10 个是前置包（顺序问题照旧）；
- 更关键：`src/task/mod.rs::register_builtin` 是**全仓定时任务的唯一注册中心** ——
  **21 处 `domain::register_builtin(self)` + 4 处 `register_cron(...)`**（日志清理 5 个、存储回收、
  通知 3 个、导入导出 2 个、公告 3 个、变更日志分区、帮助清理、审批超时、图自检、调拨超期、
  应付提醒包、收货/付款/红冲孤儿回收、折旧、信用重算、周期任务、令牌吊销清理、账务巡检、观测采样），
  而 **8 个包以 `use crate::task::Scheduler;` / `&crate::task::Scheduler` 暴露注册函数（19 行）**，
  另有 `support` 的 5 行**运行时 API**（`scheduler().run_job` / `reload()` / `is_registered_target`）——
  合计 **24 行**（§3.3）；
- 抽走它 ⇒ 这 9 个包（8 个注册签名 + `support` 运行时 API）的签名全部指向一个包外类型；而它们又必须**先于** C14 抽出
  （C3/C4/C5/C6/C7/C9/C10/C12/C13）⇒ 谁先谁后都编不过，除非先把「注册签名」换成接缝
  （这正是 §7.1 第 2 条要给的施工单）。

**结论**：`src/task` 与 `business/task` 必须同包（§2.2 的 4 行互指），且必须**最后**抽（C14 是全序倒数第二张）。

### 4.5 4 条双身份路由把「C2 必须在 C14 之前」钉死

51 条路由里有 **4 条 `#[nik::no_auth]`**（任务详情 / 附件上传 / 评论 / AI 进度回写）：它们既接受人类 JWT，
也接受 **AI 任务令牌**（`Authorization: Bearer …`，中间件 `middleware::task_token::guard` 校验令牌 ↔ 路径任务一致）。
权限不是靠宏判的，而是 handler 内 `ensure_task_read_permission` —— 也就是说
**task 包对 `middleware/task_token` 有 6 行硬依赖**（§3.2.1 第 29–34 行）。
`middleware` 是 C2 装配层包的节点；C14 前置里 C2 排第一，顺序天然满足；但若照 P2-53 卡面先抽 `task`，
这 6 行同样是反向依赖。C14 合并前必须回归：**51 条路由一条不少**，且 4 条 `no_auth` 的
「AI 令牌仍能进、人类无码仍 403」两组用例不退化。

### 4.6 收益：这是「最后一刀」里最值的一张

`task` 占 `backend/app/src` **4.95%**（29 119 / 587 800 行）、55 文件、51 条路由、40 个模块声明，
12 张已抽/待抽的包都靠它的 `Scheduler` 挂 cron。抽出来后：
`cloudpivot-task → {kernel, server, support, storage, notice, changelog, impexp, help/aiagent, flow, erp}` 全是
**向下箭头**，而 `flow`/`erp`/`taskflow`/`project`/`dashboard` 对它的引用全部变成向上（合法）——没有一行回边。
所以本卡的正确处置是**并入 C14**，不是「留在单体」。

## 5. 卡面两条命令的真实返回（说明「失败」是刻意的）

### 5.1 `cargo build --workspace`

```bash
$ cd /opt/cloudpivot/backend && cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.13s
build_exit=0
```

（零改动 ⇒ 命中缓存；退出码单独捕获，不是管道里 `tail` 的状态。）

### 5.2 `cargo test -p cloudpivot-task -- --test-threads=1`

```bash
$ cd /opt/cloudpivot/backend && cargo test -p cloudpivot-task -- --test-threads=1
error: package ID specification `cloudpivot-task` did not match any packages
pkg_exit=101
```

**这是「不建该 crate」的直接证据**，不是回归：包里没有 `cloudpivot-task`，命令本身不成立。

### 5.3 替代验证：`task` 两个节点的用例全绿（拆前基线，含 1 个真库文件）

作废不等于不验证 —— 本卡给出「`task` 留在单体时基线是绿的」这一条实测，
让 C14 将来有可比对的拆前/拆后口径：

```bash
# 99 上跑：连 145 的 PG（99 本机 PG 已 purge；卡面「数据库约束」场景 A）
$ HOST=172.18.10.145
$ PGPASSWORD=… createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_p253_task_drill   # exit=0（182 表）
$ ERP_DB_URL="postgres://cloudpivot:***@$HOST:5432/cp_p253_task_drill" \
    cargo test -p cloudpivot --lib 'business::task::' -- --test-threads=1
running 250 tests
...
test result: ok. 250 passed; 0 failed; 0 ignored; 0 measured; 5431 filtered out; finished in 4.64s

$ ERP_DB_URL="…/cp_p253_task_drill" \
    cargo test -p cloudpivot --lib 'task::' -- --test-threads=1 --skip business::
running 12 tests
...
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 5669 filtered out; finished in 0.00s
```

合计 **262 条**（250 + 12）全绿。其中真库 3 条在 `business::task::handler::copy_db_tests`
（`d9_task_copy_keeps_fields_but_not_children` / `d9_task_copy_requires_create_permission` /
`zz_teardown_purges_super_admin_fixture_user`），设了 URL 的那次运行日志里 `grep -c SKIP = 0`。

反假绿对照（同一批真库用例，不设 URL）：

```bash
$ env -u ERP_DB_URL -u TASK_DB_URL cargo test -p cloudpivot --lib \
    'business::task::handler::copy_db_tests' -- --test-threads=1 --nocapture
test business::task::handler::copy_db_tests::d9_task_copy_keeps_fields_but_not_children ...
    SKIP: 未设置 ERP_DB_URL / TASK_DB_URL（不连真实数据库）
ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 5678 filtered out; finished in 0.00s
```

判据：不设 URL ⇒ 打印 SKIP + 0.00s（假绿可识别）；设 URL 那次 SKIP 出现 **0 次**、耗时 4.64s ⇒ 真连库。
（`grep -c` 返回 1 是「无匹配」，即 0 行 SKIP。）

> 库名按卡面模板带 `drill`：`cp_p253_task_drill`（模板 `cloudpivot_test` 克隆，182 表）。**本卡用完已 `dropdb`。**

## 6. 复核命令（只读，任意机器可跑）

```bash
cd /opt/cloudpivot

# ① 工具三个口径（期望：两个 layer-check 退 1 / plan-order 退 0）
python3 scripts/verify/crate_split_readiness.py --domain business/task --layer-check ; echo "exit=$?"
python3 scripts/verify/crate_split_readiness.py --domain task --layer-check          ; echo "exit=$?"
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md ; echo "exit=$?"

# ② 规模与机械量
cd backend/app/src
find business/task task -name '*.rs' | wc -l                                        # 55
find business/task task -name '*.rs' -exec cat {} + | wc -l                         # 29119
grep -rhE '^[[:space:]]*#\[nik::(get|post|put|delete|patch)\(' business/task task | wc -l  # 51
grep -rhE '^[[:space:]]*#\[nik::permission\b' business/task | wc -l                 # 47
grep -rhE '^[[:space:]]*#\[nik::no_auth\b' business/task | wc -l                    # 4
grep -rhE '^[[:space:]]*#\[nik::auth_only\b' business/task | wc -l                  # 0
ls business/task/*_db_tests.rs task/*_db_tests.rs 2>/dev/null | wc -l               # 1
grep -rn 'include_str!\|include_bytes!' business/task task | grep -c '\.\./'         # 0（唯一 1 处是同目录）
grep -rn -E 'test_helpers|fixture_hygiene|SEED_ROLE_LOCK|ensure_any_' business/task task | wc -l  # 0
grep -cE 'register_builtin\(self\)' task/mod.rs                                     # 21
grep -cE 'register_cron\(' task/mod.rs                                              # 4

# ③ 入边（代码级 19 + 26 行；注释 7 + 14 行）
grep -rnE 'crate::business::task\b' --include=*.rs . | grep -v '^\./business/task/' | wc -l   # 26 = 19 代码 + 7 注释
grep -rnE 'crate::task\b' --include=*.rs . | grep -vE '^\./(task|business/task)/' | wc -l     # 40 = 26 代码 + 14 注释

# ④ 出边目标分布（代码级 112 + 45 = 157，包内 4）
grep -rhoE 'crate::(business::[a-z_]+|shared|utils|support|system|middleware|task)' business/task | sort | uniq -c | sort -rn
grep -rhoE 'crate::(business::[a-z_]+|shared|utils|support|system|middleware|task)' task | sort | uniq -c | sort -rn

# ⑤ workspace 里没有 cloudpivot-task
cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1 \
  | jq -r '.packages[].name'                                                        # cloudpivot / cloudpivot-doc
```

## 7. 保留物 / 后继

### 7.1 给 C14 `cloudpivot-task` 的施工单（本卡唯一「产出」，不影响卡序）

1. **前置**：C1~C7、C9、C10、C12、C13 **全绿**后再开工（11 张，`--plan-order` 的顺序）。
   尤其：C3/C4/C5/C6/C7/C9/C10/C12/C13 必须**已把 §3.4 里各自那 28 行倒转成接缝**，否则 C14 一开就顶在
   「9 个包都拿着 `&crate::task::Scheduler` 或 `task::scheduler()`」上（Cargo 直接 `cyclic package dependency`）。
2. **接缝形状（本卡建议，施工时二选一）**：
   - **① 注册表上提**：内核（C1）加一个极小的 `JobDef { name, cron, desc, runner }`（或
     `trait JobRegistrar { fn register_cron(...) }`），各域把 `pub fn register_builtin(scheduler: &Scheduler)`
     改成 `pub fn builtin_jobs() -> Vec<JobDef>`（或 `fn register_builtin(reg: &dyn JobRegistrar)`）；
     `src/task` 的 `Scheduler` 实现 `JobRegistrar`，继续做唯一注册中心。
     **好处**：各域一行不用改语义（只换参数类型），cron 细节仍只在 `src/task`。
   - **② 运行时端口**：`support`（C3）那 5 行是**运行时 API**（`scheduler().run_job` / `reload()` /
     `is_registered_target`），不是签名 —— 必须走注册表端口（照 `voucher_port` 先例：C14 启动时把
     `Arc<dyn JobApi>` 注册进 `support`，`support` 只持 trait object）。
   - 两条都要在 C1/C3 施工时定，**不要留到 C14 再动**：那时 10 个包已各自成包，改动面 ×10。
3. **`erp → business/task` 的 4 行**（`linkage/actions.rs:647/651/677` 生产 3 行 + `linkage/spec.rs:1951` 测试 1 行）
   由 **C13** 付：ERP 联动「建任务」要走端口（task 定义/实现、ERP 调用），不能直接 `task::model::create_task`。
   `--plan-order` 已把这 4 行记在 C13 的「business/task 4」里；测试那 1 行随 C13 的测试一起搬。
4. **`shared → task` 的 1 行**（`shared/observability.rs:427`）是**测试边**，由 **C1** 付（C1 卡面「10 行」里的
   `task 1`）：把「采样器指标都已登记」的断言留到域内/走 `test-support` feature，内核生产代码无需任何改动
   —— 这是本卡对 C1 的口径修正（生产回边为 0）。
5. **测试面（本卡最省事的一处）**：15 个测试文件 + 36 个内联 `#[cfg(test)]`，**夹具引用 0**、
   **`include_str!` 无跨目录**；跨包测试入边 9 行（`taskflow` 5 / `support` 2 / `erp` 1 / `shared` 1）
   全部只用公开 API（`task::model` / `approval_port` / `state`），**不需要** `test-support` feature。
6. **路由条数门禁**：51 条路由（47 `#[nik::permission]` + 4 `#[nik::no_auth]`、0 `auth_only`、无 `/health`）
   搬完由 C2 的 bin 收集；合并前 `cargo test --lib system::menu::permission_audit_tests` +
   `bash scripts/verify/menu_invariants.sh <库>` 必须全绿，`inventory` 条数「拆前 − 0」；
   4 条 `no_auth` 的「AI 任务令牌可进 / 人类无码 403」两组用例必须不退化（§4.5）。
7. **入边 import 改写**：C14 开工时实际只剩 **taskflow 9 + project 2 + dashboard 1**（C15，改
   `cloudpivot_task::`）+ bin 2（C2）+ 21 行注释；其余 29 行在 §3.4 里已被前置卡的接缝消化，
   **不要按今天的 43 行原样改**（会改到已经不存在的调用点）。
8. **真库口径**：拆前基线 = **262 passed / 0 failed**（250 + 12，含 3 条真库用例，§5.3）；
   拆后 `cargo test -p cloudpivot-task --lib -- --test-threads=1` 条数不许少。

### 7.2 工具口径（本卡的好消息）

`crate_split_readiness.py` 对 `business/task` / `task` **入边/出边都精确**（§3.5 六类形态实测为 0，
逐行复核 43/43、153/153 全对齐）。P2-49/P2-50/P2-51 报的「工具口径比代码窄」（花括号、`crate::bootstrap`、
字符串/注释假阳性）在本域**一个都没有** —— 因为本域的跨域引用全部写成规范形态。
**建议**：C14 施工时用 §3.2 的 72 行 + §3.3 的 43 行作为验收清单；工具缺陷（`crate::bootstrap` 未映射到
`app#root`、纯注释边不进回边表）仍应修，否则 C13 `erp` 那种大域还会再踩。

### 7.3 本卡没有留下的东西（避免下游误会）

- 没有新增/删除任何 `.rs`、`Cargo.toml`、迁移、脚本；
- 没有新增 crate 目录（`backend/crates/` 仍只有 `cloudpivot-doc`）；
- 没有提交测量脚本（逐行判据留在工作区，§3 的结论已全部落表，可凭 §6 复现）；
- 演练库 `cp_p253_task_drill` 已 `DROP`。

## 附录 A 本卡实测原始日志（关键片段）

```text
# 出边按目标 × prod/test（剔除注释与测试区；扫描器口径，两节点分别统计）
business/task  business/shared 33 (test 9)   shared 23 (test 4)   utils 19
               notice 6  aiagent 6 (test 1)  middleware 6  erp 5  changelog 4
               storage 3  task 2  docpdf 2  pdf 2  system 1 (test 1)
               TOTAL 112 = prod 97 + test 15
task           erp 17 (test 3)  support 8  shared 5 (test 1)  notice 3  impexp 2
               business/task 2  flow 2  utils 1  storage 1  announcement 1
               changelog 1  help 1  system 1
               TOTAL 45 = prod 41 + test 4
合并：157 − 包内 4 = 153（内核 81 = prod 67 + test 14；非内核 72 = prod 67 + test 5）
```

```text
# 入边按来源 × prod/test（测试区 = *test*.rs ∪ 内联 #[cfg(test)] 块）
erp             prod= 11 test= 1   代码=12  （src/task 8 + business/task 4，其中 1 条测试）
taskflow        prod=  4 test= 5   代码= 9
support         prod=  3 test= 2   代码= 5
notice          prod=  3 test= 0   代码= 3
app#root(bin)   prod=  2 test= 0   代码= 2
project         prod=  2 test= 0   代码= 2
flow            prod=  2 test= 0   代码= 2
impexp          prod=  2 test= 0   代码= 2
dashboard       prod=  1 test= 0   代码= 1
announcement    prod=  1 test= 0   代码= 1
help            prod=  1 test= 0   代码= 1
storage         prod=  1 test= 0   代码= 1
shared(内核)    prod=  0 test= 1   代码= 1   ← 只在 #[test] 里
system          prod=  1 test= 0   代码= 1
合计            prod= 34 test= 9   代码=43  （43 个不同行，无一行两处）+ 21 行注释
```

```text
# 包内互指（C14 必须同包的实证）
business/task/hermes_sync.rs:53   use crate::task::Scheduler;                       → task
business/task/recurring.rs:318    pub fn register_builtin(scheduler: &crate::task::Scheduler) → task
task/mod.rs:281                   crate::business::task::hermes_sync::register_builtin(self); → business/task
task/mod.rs:312                   crate::business::task::recurring::register_builtin(self);    → business/task
```

```text
# src/task = 全仓 cron 注册中心（机械量）
task/mod.rs: register_builtin(self) 21 处 + register_cron(...) 4 处 = 25 个注册点
被注册的域：storage / notice(3) / impexp(2) / announcement / changelog(分区) / help / business/task(2) /
            flow(2) / erp(8) / system / support(账务巡检) / task::metrics_sampler(自身)
10 个包以 `&crate::task::Scheduler` 为注册函数参数（§3.3 的 24 行）
```
