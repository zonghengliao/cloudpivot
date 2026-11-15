# P2-52 工作流域抽 crate 收口 —— 作废（`flow` 按 §P2-C 并入 C12 `cloudpivot-flow`）

> 卡：`t_8b06fefe`（P2-52【P2-C】抽 `flow` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `0cc8554c`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41`（`11f490f2`）、`P2-42`（`e9a31e96`）、`P2-43`（`939ec719`）、`P2-44`（`810ce2c7`）、
> `P2-45`（`5a2bc55a`）、`P2-46`（`ca11c5dc`）、`P2-47`（`4648b330`）、`P2-48`（`6a473699`）、
> `P2-49`（`c1f7aa76`）、`P2-50`（`4b95fad3`）、`P2-51`（`0cc8554c`）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-flow/`，不搬 48 个文件 / 48 338 行，不改任何
`crate::business::flow` import，不动 `backend/app/Cargo.toml`，不动 `backend/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；
卡面验收命令 `cargo test -p cloudpivot-flow` 因此按预期失败（不是回归，见 §5.2）。

权威依据已入仓：

- `docs/planning/20261113-新卡清单.md` §P2-C.6：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同文件 §P2-C.3 的 **C12** 行：`cloudpivot-flow`｜节点 `flow`｜前置 **C1、C3、C5、C6、C11**｜
  回边 **128**｜**本步要切 2 行（task 2）**｜机械量 `48 / 48 338 · 45 · 1 · 1 · 0`；
- 同节 §P2-C.1（判据看**出边**）、§P2-C.4（每张卡通用施工单，第 1 步「先付解环账」）、
  §P2-C.5（为什么**不**走「域 crate 反向依赖单体」）；
- `P2-42` §4 台账第 **91** 行（P2-52）= `48 / 48 338`｜SCC 24｜入边 `128（7）`｜出边目标 7｜
  `35 行`｜`退 1`｜落点 **`C12 cloudpivot-flow`**。

**本卡与前面 10 张作废卡的区别，先写在最前面**（三点都是实测，不是推断）：

1. **`flow` 确实会成为一个独立 crate**（C12），不是「留在 `erp` 包内永不单抽」（P2-42~P2-50 的落点）；
2. **它是 17 张旧卡里解环账最小的一张**：C12 只欠 **2 行**（`crate::task::Scheduler`），
   比 P2-51（hr 7 行）、P2-54（impexp 13 行）都小 —— 所以本卡作废的理由**不是「抽不动」**，
   而是**位置错**：C12 的 5 张前置（C1 内核 / C3 support / C5 notice+pdf+docpdf / C6 changelog /
   C11 hr）**一张都还没建**（`crates/` 里只有 `cloudpivot-doc`，见 §2.3）；
3. **入边 128 行是 17 张卡里最大的**（第二是 P2-56 notice 74、C1 的 `shared` 552）：
   改 import 的机械面最大，而其中 **99 行来自 `erp`（C13）**、**16 行来自 `taskflow`（C15）**、
   **2 行来自 `src/task`（C14）** —— 这三个域都在 C12 **之后**抽，所以在 C12 时它们是
   **合法的向下箭头**，不用付账（§3.4 有配对表）。

换句话说：**本卡的卡面工作量描述不成立**（照它做，`flow → shared/utils/business::shared` 只能反向
依赖单体 ⇒ 包级环），而 C12 卡的描述是成立的。下一轮要重开，请直接开 C12，别重开 P2-52。

## 2. `flow` 的实测（只读、无库依赖，2026-11-15 于 99）

### 2.1 工具原始输出（逐字，`--layer-check`）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain flow --layer-check
域：flow（scope=top）
  规模：48 个文件 / 48338 行
  SCC：24 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（23）：aiagent, announcement, attachment, business/shared, business/task,
    changelog, docpdf, erp, help, hr, impexp, masking, middleware, mutex, notice, pdf,
    permission, shared, storage, support, system, task, utils
  回边（留在应用 crate 的代码 → 本域）：128 处代码级
    app#root               code=  2 all=  3  bootstrap.rs:140, bootstrap.rs:294
    dashboard              code=  1 all=  2  business/dashboard/model.rs:27
    erp                    code= 99 all=102  business/erp/approval_gate.rs:76, approval_gate.rs:219,
                                             asset/asset_bridge.rs:42, asset/asset_handler.rs:58,
                                             budget/budget_bridge.rs:34, budget/budget_bridge.rs:35 …（共 37 文件）
    hr                     code=  7 all=  7  business/hr/leave_bridge.rs:50, leave_bridge_db_tests.rs:36,
                                             leave_handler.rs:75, leave_handler.rs:76,
                                             salary_bridge.rs:93, salary_sheet_handler.rs:56
    system                 code=  1 all=  1  system/badge/model.rs:52
    task                   code=  2 all=  2  task/mod.rs:283, task/mod.rs:310
    taskflow               code= 16 all= 17  business/taskflow/approval_impl.rs:40, 41, 71, 286,
                                             bridge.rs:44, bridge.rs:45 …
  出边（本域 → 其它）：10 个目标
    shared                 code= 31 all= 32
    business/shared        code= 27 all= 31
    notice                 code= 17 all= 17
    hr                     code= 10 all= 10
    utils                  code=  5 all=  5
    docpdf                 code=  2 all=  2
    pdf                    code=  2 all=  2
    task                   code=  2 all=  2
    changelog              code=  1 all=  1
    support                code=  1 all=  1
  层次判据：本步要切 7 条 / 35 行 —— 出边指向更下层以外：notice(17), hr(10), docpdf(2), pdf(2),
            task(2), changelog(1), support(1)
            （假设 L1 内核 shared, utils, business/shared 已抽出、装配层 bin 已独立成包）
  结论：不可抽（128 处代码级回边，7 个来源）
exit=1
```

三个退出码（本节全部实测）：

| 口径 | 命令 | 退出码 | 含义 |
|:--|:--|:--:|:--|
| 层次判据（推荐） | `--domain flow --layer-check` | **1** | 出边 7 条 / 35 行落在「更下层」之外 ⇒ 现在抽会成环 |
| 旧口径（方向反了） | `--domain flow --check` | 1 | 数的是入边 128，恒不为 0，无法区分「可抽/不可抽」 |
| 卡序模拟 | `--plan-order docs/planning/20261113-新卡清单.md` | **0** | 全序累计 66 行解环账；C12 排在 C1/C3/C5/C6/C11 之后 ⇒ **按卡序抽可行** |

`--plan-order` 的关键两行（逐字）：

```text
C11    cloudpivot-hr                                 7     7   OK
                                              切向：flow(7)
C12    cloudpivot-flow                               2     2   OK
                                              切向：task(2)
全序累计要切：66 行
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

即：**C12 开工时的真实欠账是 2 行**（`task`），不是 35 行 —— 35 行是「假设只抽出内核」时的
`--layer-check` 口径（`notice`/`hr`/`docpdf`/`pdf`/`changelog`/`support` 那 33 行在 C12 之前
已成包 ⇒ 变成合法向下箭头）。两套数字不冲突，口径不同（对照 P2-51 §2.1 的 42 行 vs C11 的 7 行）。

### 2.2 SCC 成员资格：`flow` 在顶层 24 节点环里

| | P2-42~P2-50（`erp` 子域） | **P2-52（`flow`）** |
|:--|:--|:--|
| SCC 口径 | `--scope erp-sub` ⇒ 51 节点 | `--scope top` ⇒ **24 节点**（默认） |
| 环的性质 | `erp` 内部咬合（`erp` 整体一个包） | 顶层 24 个域互相咬合 |
| 解环方向 | 无（C13 一次性整包搬） | **有**：按 C1→C15 层次序，每卡付自己那几行 |

24 个节点同属一个 SCC 的含义：**任何人**先抽任一域，都会留下「新包 → 还在单体的包」的出边；
合法出路只有「先把内核与装配层抽出（C1/C2），再按出边闭包从浅到深抽域」。`flow` 在闭包里排第 12 位。

### 2.3 workspace 现状：`cloudpivot-flow` 不存在，`flow` 还在单体里

```bash
$ cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1 \
    | jq -r '"workspace_root=\(.workspace_root)", (.packages[]|"  \(.name) -> \(.manifest_path)"), "members=\(.workspace_members|length)"'
workspace_root=/opt/cloudpivot/backend
  cloudpivot -> /opt/cloudpivot/backend/app/Cargo.toml
  cloudpivot-doc -> /opt/cloudpivot/backend/crates/cloudpivot-doc/Cargo.toml
members=2
```

即：workspace 仍是 P2-40（`35a306be`）落地时的 2 个包（P2-40 的卡面依赖已满足），
**没有** `cloudpivot-flow`；C12 的五张前置卡一个都还没交付。

### 2.4 规模与机械量（本次实测，与计划表并排）

| 项 | 计划表 §P2-C.3（C12 行） | 本次实测 | 复核口径 |
|:--|--:|--:|:--|
| 文件 / 行 | 48 / 48 338 | **48 / 48 338** | `find business/flow -name '*.rs'` |
| 占 `backend/app/src` 比例 | — | **8.22%**（48 338 / 587 800） | `app/src/**/*.rs` 共 790 个文件 |
| 路由宏（生产区） | 45 | **45** | `#[nik::get/post/put/delete/patch(`，全部 `/business/flow/…`；45 个 **(method, path)** 组合无重复（`/def/{id}`、`/state-edge`、`/delegate/{id}` 三个资源各有 GET/PUT/DELETE 同名 path，REST 语义正常，不是重复路由） |
| `#[nik::permission]` | — | **45** | 45 条路由 **一对一**挂码；`#[nik::auth_only]` **0** 条；无 `/health` |
| 模块声明 | — | **46** | `mod.rs` 里 36 `pub mod` + 10 私有 `mod flow_model_*`；第 48 个文件 `flow_model_tests.rs` 由 `model.rs:50` 的 `#[cfg(test)] #[path = "flow_model_tests.rs"] mod tests;` 挂到 `model::tests` |
| 真库用例文件（`*_db_tests.rs`） | 1 | **1** | `terminate_db_tests.rs`（4 条用例，靠 `ERP_DB_URL` 开关） |
| 出目录引用（`include_str!` 带 `../`） | 1 | **1** | `edge_admin_tests.rs:176` → `migrations/business/V0206__flow_edge_config_admin.up.sql` |
| 夹具引用（计划表口径） | 0 | **0** | `test_helpers\|fixture_hygiene\|SEED_ROLE_LOCK\|ensure_any_` |
| 模块用例总数（实跑，含真库） | — | **432** | `cargo test --lib business::flow`，见 §5.3 |

> 夹具引用 0 是本卡**最有价值的一条**：`flow` 不碰内核的 `shared::test_helpers`、也没有自己的
> `fixture_hygiene`（`hr`/`erp` 各有一份）。C12 搬迁时**没有「`#[cfg(test)]` 夹具跨包不可见」的坑**
> （这是 §P2-C.5 第 3 条最重的陷阱），测试面的成本只有「1 处 `include_str!` 路径重算 + 1 个真库文件」。

## 3. 逐行复核：入边 128 行 / 出边 98 行（并核对工具口径）

判据脚本：把 `*_tests*.rs` / `test_support*.rs` / `tests_helpers*.rs` 整文件识别为测试区，
`//` 注释单列，字符串字面量按「代码里没有路径」剔除；对 `crate::business::flow`、
`crate::business::{…flow…}`（花括号）、`business::flow`（裸）、`super::flow` 四种写法全扫。

### 3.1 出边：`flow` → 其它（98 行代码 = 10 个目标；内核 63 + 非内核 35）

`business/flow/` 内共 **264 处** `crate::` 引用，其中 **156 处指向自己**
（`crate::business::flow::…`，抽包后变 `crate::…`，同包内改路径 —— 机械量，不算账），
其余 98 处（+10 行注释）才是出边：

| 目标 | prod | test | 代码 | 归属 | C12 时能不能存在 |
|:--|--:|--:|--:|:--|:--|
| `business::shared` | 26 | 1 | **27** | **L1 内核**（C1） | ❌ 等 C1 |
| `shared`（`src/shared`） | 26 | 5 | **31** | **L1 内核**（C1） | ❌ 等 C1 |
| `utils`（`src/utils`） | 5 | 0 | **5** | **L1 内核**（C1） | ❌ 等 C1 |
| `business::notice` | 17 | 0 | **17** | **C5**（前置里） | ✅ 合法向下箭头 |
| `business::hr` | 10 | 0 | **10** | **C11**（前置里） | ✅ 合法向下箭头 |
| `business::docpdf` | 2 | 0 | **2** | **C5**（前置里） | ✅ 合法向下箭头 |
| `business::pdf` | 2 | 0 | **2** | **C5**（前置里） | ✅ 合法向下箭头 |
| `crate::task`（`src/task`） | 2 | 0 | **2** | **C14（在 C12 之后）** | ❌ **C12 的 2 行解环账** |
| `business::changelog` | 1 | 0 | **1** | **C6**（前置里） | ✅ 合法向下箭头 |
| `crate::support` | 1 | 0 | **1** | **C3**（前置里） | ✅ 合法向下箭头 |
| **合计** | **92** | **6** | **98** | | 内核 63 / 合法 33 / 要切 **2** |

校验：63（内核）+ 33（合法）+ 2（要切）= 98 = 工具口径代码级出边总数。
`--layer-check` 的 35 行 = 33 + 2（它只扣内核，不扣「后抽的 C14」）。

### 3.2 非内核 35 行逐行（33 行合法 + 2 行要切，全部是**生产代码**，无测试边）

| # | 位置 | 引用 | 用途 | 归属 |
|--:|:--|:--|:--|:--|
| 1 | `engine.rs:73` | `notice::dto::{NoticeLevel, NoticeType}` | 审批通知级别/类型 | C5 |
| 2 | `engine.rs:74` | `notice::{self, NoticeDraft, NoticeTarget}` | 引擎推通知 | C5 |
| 3 | `graph.rs:43` | `notice::render` | 条件渲染通知文案 | C5 |
| 4 | `handler.rs:51` | `notice::dto::ContentFormat` | 通知正文格式 | C5 |
| 5 | `handler.rs:52` | `notice::render` | 同上 | C5 |
| 6 | `notify.rs:44` | `notice::dto::{NoticeLevel, NoticeType}` | 通知扇出 | C5 |
| 7 | `notify.rs:45` | `notice::{self, NoticeDraft, NoticeTarget}` | 同上 | C5 |
| 8 | `notify_node.rs:70` | `notice::dto::{ContentFormat, NoticeLevel, NoticeType}` | 通知节点 | C5 |
| 9 | `notify_node.rs:71` | `notice::{NoticeDraft, NoticeTarget}` | 同上 | C5 |
| 10 | `notify_node.rs:280` | `notice::render::render_paths` | 模板渲染 | C5 |
| 11 | `notify_node.rs:600` | `notice::model::find_enabled_template_by_code` | 取通知模板 | C5 |
| 12 | `task_node.rs:46` | `notice::dto::ContentFormat` | 待办消息格式 | C5 |
| 13 | `task_node.rs:47` | `notice::render` | 待办消息渲染 | C5 |
| 14 | `template_vars.rs:260` | `notice::render::collect_placeholders_paths` | 变量表自检 | C5 |
| 15 | `template_vars.rs:338` | `notice::render::render_paths` | 变量表自检 | C5 |
| 16 | `template_vars.rs:341` | `notice::dto::ContentFormat::Text` | 同上 | C5 |
| 17 | `timeout.rs:53` | `notice`（`use crate::business::notice;`） | 超时催办 | C5 |
| 18 | `handler.rs:1807` | `hr::leave_bridge::on_leave_settled(…, "WITHDRAWN")` | 撤回 → 请假单回写 | C11 |
| 19 | `handler.rs:1809` | `hr::salary_bridge::on_salary_settled(…, "WITHDRAWN")` | 撤回 → 工资单回写 | C11 |
| 20 | `handler.rs:1889` | `hr::leave_bridge::on_leave_settled(…, "CANCELLED")` | 取消 → 请假单回写 | C11 |
| 21 | `handler.rs:1891` | `hr::salary_bridge::on_salary_settled(…, "CANCELLED")` | 取消 → 工资单回写 | C11 |
| 22 | `handler.rs:1938` | `hr::leave_bridge::guard_self_approval(task_id, operator)` | 审批前 SoD 自审拦截 | C11 |
| 23 | `handler.rs:1940` | `hr::salary_bridge::guard_self_approval(task_id, operator)` | 同上（工资单） | C11 |
| 24 | `handler.rs:1965` | `hr::leave_bridge::on_leave_settled(out.instance_id, …)` | 同意 → 请假单回写 | C11 |
| 25 | `handler.rs:1968` | `hr::salary_bridge::on_salary_settled(…)` | 同意 → 工资单回写 | C11 |
| 26 | `handler.rs:2028` | `hr::leave_bridge::on_leave_settled(…)` | 驳回 → 请假单回写 | C11 |
| 27 | `handler.rs:2031` | `hr::salary_bridge::on_salary_settled(…)` | 驳回 → 工资单回写 | C11 |
| 28 | `instance_pdf.rs:56` | `docpdf::doc_pdf_model` | 审批单 PDF 模型 | C5 |
| 29 | `instance_pdf.rs:57` | `docpdf::doc_pdf_render::{self, RenderedDoc}` | 审批单 PDF 渲染 | C5 |
| 30 | `instance_pdf.rs:59` | `pdf::handler as pdf_handler` | PDF 落盘 | C5 |
| 31 | `instance_pdf.rs:60` | `pdf::template::{…}` | PDF 模板 | C5 |
| 32 | `flow_model_cancel.rs:10` | `changelog::{self, Action, ChangeCtx, ChangeTarget, DiffInput}` | 取消实例写变更日志 | C6 |
| 33 | `maintain.rs:78` | `support::log::model::AuditLogModel` | 图哈希漂移写审计 | C3 |
| 34 | **`maintain.rs:79`** | **`use crate::task::Scheduler;`** | **图快照自检的 cron 注册签名** | **C14 ⇒ 要切** |
| 35 | **`timeout.rs:55`** | **`use crate::task::Scheduler;`** | **超时扫描的 cron 注册签名** | **C14 ⇒ 要切** |

**这 2 行要切的全貌**（就是 C12 卡面「本步要切 2 行」）：

```rust
// business/flow/maintain.rs:79 与 business/flow/timeout.rs:55（两行完全相同）
use crate::task::Scheduler;
// 用在：pub fn register_builtin(scheduler: &Scheduler)
// 调用方是 src/task（C14）：task/mod.rs:283  flow::timeout::register_builtin(self);
//                            task/mod.rs:310  flow::maintain::register_builtin(self);
```

`flow` 与 `src/task` 之间的**另一个方向已经不欠账**：`flow ⇄ business::task` 走的是
`flow::task_port` 注册表（§P2-B 先例），所以出边表里**没有** `business/task`。剩下这两个
`&Scheduler` 是「调度器类型」的硬引用 —— C12 施工时把它倒转即可（§7.1 第 2 条给了具体形状）。

### 3.3 入边：其它 → `flow`（128 处代码 / 127 行 / 7 个来源）

| 来源节点 | prod | test | 代码合计 | 内容（代表行） |
|:--|--:|--:|--:|:--|
| `business/erp` | 70 | 29 | **99** | 50 个文件（prod 37 / test 13，最大单源）。生产：11 个 ERP 子域的审批桥（`purchase_handler.rs` 11 行、`payable_bridge.rs` 4、`sales_approval_bridge.rs` 3、`voucher_bridge/write` 2）+ `approval_gate.rs:76` 的 `pub use flow::approval_gate::{…}` **再导出** + `doc_reader_impl.rs:134` 注册 PDF 读取器 + `linkage/flow_instance_trigger.rs:92/302` 的实例落定钩子；测试：13 个 `*_tests*/test_support` 文件直接驱动引擎（`stockin_handler_tests_helpers.rs` 6、`doc_reader_impl_tests.rs` 3、`pflow3b_tests.rs` 3 …） |
| `business/taskflow` | 15 | 1 | **16** | `bridge.rs` 8 + `approval_impl.rs` 4 + `spawn.rs` 3（任务侧认 `flow::task_port` / 引擎）+ `delete_cancel_tests.rs` 1 |
| `business/hr` | 6 | 1 | **7** | `leave_handler.rs:75/76`（提交/撤回）、`leave_bridge.rs:50`、`salary_bridge.rs:93`、`salary_sheet_handler.rs:56`、`leave_bridge_db_tests.rs:36`（测试） |
| `app#root`（bin） | 2 | 0 | **2** | `bootstrap.rs:140`（启动日志里取 `flow::task_port::port_name()`）、`bootstrap.rs:294`（启动跑 `flow::sign_freeze::freeze_published_versions()`） |
| `src/task` | 2 | 0 | **2** | `task/mod.rs:283`、`task/mod.rs:310`（注册两个 cron） |
| `business/dashboard` | 1 | 0 | **1** | `dashboard/model.rs:27`（待办统计） |
| `system` | 1 | 0 | **1** | `system/badge/model.rs:52`（侧栏待办角标） |
| `shared`（`src/shared`） | 0 | 0 | **0** | 仅 1 行**注释**（`shared/error_code.rs`），不计 |
| **合计** | **97** | **31** | **128** | 行数 127（`purchase_handler.rs:3132` 一行两处）+ 7 行注释 = 134 行命中 |

### 3.4 反向边配对：同一对域的两条边，由「先抽的那张卡」付清

| 边对 | 行数（prod/test） | 谁付这笔账 | 依据 |
|:--|--:|:--|:--|
| `erp → flow` / `flow → erp` | 99（70/29）/ **0** | **不需要付** | C13 在 C12 之后 ⇒ `erp → cloudpivot-flow` 是合法向下箭头（单向，无环） |
| `taskflow → flow` / `flow → taskflow` | 16（15/1）/ **0** | **不需要付** | C15 在 C12 之后，同上（`flow ⇄ business::task` 已走 `task_port`，所以 0 行） |
| `hr → flow` / `flow → hr` | 7（6/1）/ 10（10/0） | **C11** | C11 卡面「本步要切 7 行：flow 7」就是这 7 行；付完后 `flow → hr` 变合法（C11 先抽） |
| `system → flow` / `flow → system` | 1（1/0）/ **0** | **C7** | C7 卡面「7 行：masking 5、task 1、flow 1」里的 flow 1；`--plan-order` 的 C7 行逐字有 `切向：… flow(1)` |
| **`task → flow` / `flow → task`** | 2（2/0）/ **2（2/0）** | **C12** | 本域**唯一双向**的一对，但只付一半：`flow → task` 那 2 行（`&Scheduler`）由 **C12** 付（卡面「2 行：task 2」）；反方向的 `task → flow` 2 行（`task/mod.rs:283/310` 注册 cron）在 C12 成包后是**合法向下箭头**（C14 在 C12 之后，C14 卡面 0 行） |
| `dashboard → flow` / `flow → dashboard` | 1（1/0）/ **0** | **不需要付** | C15 在 C12 之后 |
| `bin → flow` / `flow → bin` | 2（2/0）/ **0** | **不需要付** | C2 装配层可依赖任何域包（`§P2-C.4` 第 6 步） |

> `task → flow` 这一对是**本卡唯一真正的双向咬合**（`flow → task` 2 行 + `task → flow` 2 行，
> 都是 `Scheduler` / `register_builtin` 这一件事的两头）。C12 只付自己那 2 行；
> 剩下的方向在 C12 成包后自动合法 —— 但**同一天必须两边一起改**，否则 `cargo check` 红在
> 「`cloudpivot-flow → cloudpivot`（含 C14）」这个反向箭头上。施工单见 §7.1。

### 3.5 工具口径核对：本卡**零漏报、零假阳性**（与前 4 张卡不同）

前几张卡（P2-49/P2-50/P2-51）都发现 `crate_split_readiness.py` 的口径比代码窄，本卡专门复核了
四类漏报形态，结论是**本域一条都没有**：

| 漏报形态 | 本域实测 | 证据 |
|:--|:--|:--|
| 花括号 `use crate::business::{flow, …}` | **0 处** | 全仓 `grep -rn 'crate::business::{[^}]*flow'` 在 `business/flow/` 之外为空；`business/flow/` 内的花括号导入也没有跨域项 |
| 裸 `business::flow`（无 `crate::`） | **0 处代码**（7 行注释） | 逐行扫描，7 行全在 `//!` / `///` 里（`hr/no_erp_edge_tests.rs`、`task/mod.rs` 顶部表等） |
| `super::flow`（从 `business/*` 相对引用） | **0 处** | `flow` 之外没有 `super::flow::`；`flow` 内的 `super::*` 全是自己模块的子模块 |
| `crate::bootstrap`（指向 bin，P2-51 漏过的那类） | **0 处** | 本域 264 处 `crate::` 里没有 `crate::bootstrap`，也没有任何「未识别节点」的目标 |
| 出边未识别目标 | **0 个** | 98 处出边 100% 落到已登记节点（10 个目标） |

**这也是本卡能给出「精确账」的原因**：入边 128 = 工具 128、出边 98 = 工具 98（内核 63 + 非内核 35），
逐行复核与工具**完全对齐**。所以 C12 开工时可以直接引用本文的 35 行清单，不必重扫。

> 附带一条口径修正（不影响判据）：工具报「回边 128 处」是**处数**，行数是 **127**
> （`business/erp/purchase/purchase_handler.rs:3132` 一行里出现两次
> `crate::business::flow::model::…`）；`all` 列 134 = 127 代码行 + 7 注释行。

## 4. 为什么本卡不能按卡面 4 步直接抽（五条，都带实证）

### 4.1 顺序不对：C12 的 5 张前置一张都没建，63 行内核引用无处可去

`flow` 出边里 **63 行（64%）** 指向 L1 内核（`shared` 31 / `business::shared` 27 / `utils` 5）。
C1 之前，`crate::shared` / `crate::utils` / `crate::business::shared` 只存在于**单体 `cloudpivot` 包**里；
此时把 `flow` 抽成 `cloudpivot-flow` 并改 import，得到的是
`cloudpivot-flow → cloudpivot`，而 `cloudpivot`（app）同时必须 `→ cloudpivot-flow`
（否则 45 条路由的 `inventory` 项进不了链接单元 ⇒ **静默 404**，§P2-C.4 第 6 步）⇒ **包级环**。
这正是 §P2-C.5 明确「不采用」的过渡路线。

### 4.2 33 行出边指向的包，正好都是前置（C3/C5/C6/C11）—— 顺序对了它们才合法

`notice` 17 + `hr` 10 + `docpdf` 2 + `pdf` 2 + `changelog` 1 + `support` 1 = **33 行**，
目标全在 C12 的前置集合 {C1, C3, C5, C6, C11} 里。**这不是巧合**：`--plan-order` 的卡序就是
按出边闭包排的（§2.1 输出）。所以本卡的 35 行「本步要切」里，**33 行是等前置卡就自动清零的**，
真正要动手的只有 `task` 2 行。

### 4.3 卡面漏了 §P2-C.4 第 1 步「先付解环账」

§P2-C.4 是每张卡的通用施工单，第 1 步就是「按本卡『本步要切』清单，把那几行倒转成接缝
（照抄 P2-B 先例：`voucher_port` / `flow::doc_reader` 注册表 / `hr::employee_reader` /
`task::approval_port`），切完 `--plan-order` 必须绿、`cargo check --workspace` 必须过」。
C12 卡面写的是「**2 行**：task 2」；P2-52 卡面写的是「建骨架 / 移代码 / 加依赖 / 修 import」——
**同一件事在 C12 有解环账、在 P2-52 没有**。这是本卡作废的第二个硬理由。

### 4.4 `flow` 是**被引用最多**的域：128 行入边里有 31 行测试边，搬包后跨包不可见

入边 128 行中 **31 行是测试代码**（`erp` 29 + `hr` 1 + `taskflow` 1），它们通过
`crate::business::flow::{engine, model, test_support}` 直接驱动引擎（如
`stockin_handler_tests_helpers.rs` 6 行、`voucher/mod.rs:3465/3474` 调
`flow::test_support::approve_all_pending`）。C12 之后这些都得改成 `cloudpivot_flow::…`，
且 **`flow::test_support` 必须随包带 `test-support` feature 出口**
（`mod.rs:121` 现为 `#[cfg(test)] pub mod test_support;` —— 整个模块只在**本包**的 test 配置下存在，
跨包一律不可见），否则 C13 `erp`、C14 `task`、C15 `taskflow` 的夹具全部编不过。
这是本卡最容易被漏的一条：`flow` 自己的 432 条用例里有相当一部分依赖它，而搬包后
**域外**的 31 行测试边也全指着它。

### 4.5 收益：这是**最该抽**的一张之一

`flow` 占 `backend/app/src` **8.22%**（48 338 / 587 800 行）、48 文件、45 条路由、46 个模块声明，
且它与 `erp`（99 行入边）、`taskflow`（16 行）的耦合全是「业务向审批引擎单向调用」——
抽出来后 `erp`/`taskflow`/`task` 都只依赖 `cloudpivot-flow` 的公开 API，是**真正的向下箭头**。
`flow` 还是**唯一一个把 `erp ⇄ flow` 单向化**的包：它自己 0 行出边到 `erp`。
所以本卡的正确处置是**并入 C12**，而不是像 P2-42~P2-50 那样「留在 `erp` 包内永不单抽」。

## 5. 卡面两条命令的真实返回（说明「失败」是刻意的）

### 5.1 `cargo build --workspace`

```bash
$ cd /opt/cloudpivot/backend && cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.13s
exit= 0
```

（零改动 ⇒ 命中缓存，0.13s；退出码 0 是单独捕获的，不是管道里 `tail` 的状态。）

### 5.2 `cargo test -p cloudpivot-flow -- --test-threads=1`

```bash
$ cd /opt/cloudpivot/backend && cargo test -p cloudpivot-flow -- --test-threads=1
error: package ID specification `cloudpivot-flow` did not match any packages

help: a package with a similar name exists: `cloudpivot-doc`
exit= 101
```

**这是「不建该 crate」的直接证据**，不是回归：包里没有 `cloudpivot-flow`，命令本身不成立。

### 5.3 替代验证：`flow` 模块全量用例绿（拆前基线，含 1 个真库用例文件）

作废不等于不验证 —— 本卡给出「`flow` 留在单体时基线是绿的」这一条实测，
让 C12 将来有可比对的拆前/拆后口径：

```bash
# 99 上跑：连 145 的 PG（99 本机 PG 已 purge）
$ HOST=172.18.10.145
$ PGPASSWORD=… createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_p252_flow_drill   # exit=0
$ ERP_DB_URL="postgres://cloudpivot:***@$HOST:5432/cp_p252_flow_drill" \
    cargo test -p cloudpivot --lib business::flow -- --test-threads=1
running 432 tests
...
test result: ok. 432 passed; 0 failed; 0 ignored; 0 measured; 5249 filtered out; finished in 56.72s
exit= 0
```

反假绿对照（同一批真库用例，不设 URL）：

```bash
$ env -u ERP_DB_URL cargo test -p cloudpivot --lib business::flow::terminate_db_tests \
    -- --test-threads=1 --nocapture
test business::flow::terminate_db_tests::terminate_missing_instance_is_404 ...
    SKIP: 未设置 ERP_DB_URL（不连真实数据库）
ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 5677 filtered out; finished in 0.00s
```

设了 URL 的那次运行日志里 `SKIP` 出现 **0 次**（`grep -c SKIP = 0`），432 条全部真跑，
其中 `terminate_db_tests` 4 条真连库 —— 与 P2-51 的「不设 URL 就 0.00s 假绿」是同一套判据。

> 库名这次直接用 `cp_p252_flow_drill`（含 `drill`）：`flow` 没有 `hr` 那样的
> `fixture_hygiene::assert_scratch_db` 护栏，但卡面「数据库约束」的模板位仍建议带 `drill`/`test`，
> 免得与将来的清场型夹具冲突。本卡用完已 `dropdb`。

## 6. 复核命令（只读，任意机器可跑）

```bash
cd /opt/cloudpivot

# ① 工具三个口径（期望：layer-check 退 1 / 旧 --check 退 1 / plan-order 退 0）
python3 scripts/verify/crate_split_readiness.py --domain flow --layer-check ; echo "exit=$?"
python3 scripts/verify/crate_split_readiness.py --domain flow --check      ; echo "exit=$?"
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md ; echo "exit=$?"

# ② 规模与机械量
cd backend/app/src
find business/flow -name '*.rs' | wc -l                                          # 48
find business/flow -name '*.rs' -exec cat {} + | wc -l                           # 48338
grep -rhE '^[[:space:]]*#\[nik::(get|post|put|delete|patch)\(' business/flow | wc -l   # 45
grep -rhE '^[[:space:]]*#\[nik::permission\b' business/flow | wc -l              # 45
grep -rhE '^[[:space:]]*#\[nik::auth_only\b' business/flow | wc -l               # 0
ls business/flow/*_db_tests.rs | wc -l                                           # 1
grep -rn 'include_str!\|include_bytes!' business/flow | grep -c '\.\./'           # 1
grep -rn -E 'test_helpers|fixture_hygiene|SEED_ROLE_LOCK|ensure_any_' business/flow | wc -l  # 0

# ③ 入边（128 处代码 / 7 源）：134 行命中 = 127 代码行（1 行两处）+ 7 注释行
grep -rn 'crate::business::flow' --include=*.rs . | grep -v '^\./business/flow/' | wc -l   # 134

# ④ 出边目标分布（代码级 98 = 内核 63 + 非内核 35）
grep -rhoE 'crate::(business::[a-z_]+|shared|utils|support|task)' business/flow | sort | uniq -c | sort -rn

# ⑤ workspace 里没有 cloudpivot-flow
cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1 \
  | jq -r '.packages[].name'                                                     # cloudpivot / cloudpivot-doc
```

## 7. 保留物 / 后继

### 7.1 给 C12 `cloudpivot-flow` 的施工单（本卡唯一「产出」，不影响卡序）

1. **前置**：C1（内核）、C3（support）、C5（notice+pdf+docpdf）、C6（changelog）、C11（hr）全绿后再开工。
   尤其 **C11 必须已把 `hr → flow` 那 7 行付掉**（它已切成 `flow_port` 之类的接缝），否则
   C12 一开就顶在 `hr ⇄ flow` 上（`flow/handler.rs` 撤回/取消/自审/回调 10 个触发点是活的）。
2. **解环账 = 2 行 `crate::task::Scheduler`**（§3.2 第 34/35 行）：`flow` 的
   `maintain::register_builtin(&Scheduler)` / `timeout::register_builtin(&Scheduler)` 只用到
   `Scheduler::register(name, cron, handler)` 这一个能力。**建议照 `flow::task_port` 的现有范式倒转**：
   `flow` 改为导出 `pub fn builtin_jobs() -> Vec<BuiltinJob>`（name + cron + 函数指针/trait 对象，
   类型全在 flow 内），由 `src/task`（C14）或 bin 侧遍历注册。切完
   `--domain flow --layer-check` 应退 0、`--plan-order` 仍退 0。
   **同一天要改两头**：`task/mod.rs:283/310` 两处调用点同批调整，否则中间态编不过。
3. **`flow::test_support` 走 feature**：`mod.rs:121` 现在是 `#[cfg(test)] pub mod test_support;`，
   C12 要把它（连同 `task_independence_tests` 等只在包内用的测试模块按需）放到
   `[features] test-support = []` + `#[cfg(any(test, feature = "test-support"))]` 下，并让 C13 `erp`/
   C14 `task`/C15 `taskflow` 在 `[dev-dependencies]` 打开 —— 31 行测试入边（`erp` 29 行是主用户）
   全靠它。这是本卡最容易被漏的一条（`hr`/`erp` 的教训：`#[cfg(test)]` 夹具跨包不可见）。
4. **1 处 `include_str!` 改锚定式**：`edge_admin_tests.rs:176` 的
   `include_str!("../../../migrations/business/V0206__flow_edge_config_admin.up.sql")` →
   `include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../app/migrations/business/V0206__…sql"))`
   （§P2-C.4 第 4 步；`migrations/` 仍留在 `backend/app/migrations/`）。
5. **路由条数门禁**：45 条路由（45 权限码、0 `auth_only`、无 `/health`）搬完由 C2 的 bin 收集；
   合并前 `cargo test --lib system::menu::permission_audit_tests` + `bash scripts/verify/menu_invariants.sh <库>`
   必须全绿，且 `inventory` 条数「拆前 − 0」。
6. **入边 128 行同步改 import**（机械量，按 §3.3 的表逐源推进）：
   `erp` 99（含 `approval_gate.rs:76` 那条 `pub use` 再导出）、`taskflow` 16、`hr` 7、
   `app#root` 2（bin 依赖，合法）、`task` 2、`dashboard` 1、`system` 1。
7. **真库口径**：拆前基线 = **432 passed / 0 failed**（§5.3）；拆后
   `cargo test -p cloudpivot-flow --lib -- --test-threads=1` 条数不许少。

### 7.2 工具口径（本卡的好消息）

`crate_split_readiness.py` 对 `flow` **入边/出边都精确**（§3.5 五类漏报形态实测为 0）。
P2-49/P2-50/P2-51 报的「工具口径比代码窄」（花括号、`crate::bootstrap`、字符串/注释假阳性）
在本域不存在 —— 因为 `flow` 的跨域引用**全部**写成
`crate::business::<域>::…` / `crate::shared` / `crate::utils` / `crate::task` 四种规范形态。
**建议**：C12 施工时用 §3.2 的 35 行表 + §3.3 的 128 行按源清单作为验收清单，不必再跑自定义扫描；
但工具缺陷本身仍应修（`crate::bootstrap` 映射到 `app#root`），否则下一张卡（C13 `erp`、C14 `task`）会再踩。

### 7.3 本卡没有留下的东西（避免下游误会）

- 没有新增/删除任何 `.rs`、`Cargo.toml`、迁移、脚本；
- 没有新增 crate 目录（`backend/crates/` 仍只有 `cloudpivot-doc`）；
- 没有提交测量脚本（逐行判据留在工作区，§3 的结论已全部落表，可凭 §6 复现）；
- 演练库 `cp_p252_flow_drill` 已 `DROP`。

## 附录 A 本卡实测原始日志（关键片段）

```text
# 入边按来源 × prod/test（逐行复核脚本输出；测试区=*_tests*/test_support*/tests_helpers*）
erp          prod= 70 test= 29 代码= 99 comment=3
taskflow     prod= 15 test=  1 代码= 16 comment=1
hr           prod=  6 test=  1 代码=  7 comment=0
app#root     prod=  2 test=  0 代码=  2 comment=1
task         prod=  2 test=  0 代码=  2 comment=0
dashboard    prod=  1 test=  0 代码=  1 comment=1
system       prod=  1 test=  0 代码=  1 comment=0
shared       prod=  0 test=  0 代码=  0 comment=1   ← 注释
合计         prod= 97 test= 31 代码=128 comment=7   （行数 127，purchase_handler.rs:3132 一行两处）
```

```text
# 出边按目标 × prod/test（剔除注释后）
business/shared  prod= 26 test= 1  = 27
shared           prod= 26 test= 5  = 31
utils            prod=  5 test= 0  =  5
notice           prod= 17 test= 0  = 17
hr               prod= 10 test= 0  = 10
docpdf           prod=  2 test= 0  =  2
pdf              prod=  2 test= 0  =  2
task             prod=  2 test= 0  =  2   ← C12 解环账
changelog        prod=  1 test= 0  =  1
support          prod=  1 test= 0  =  1
合计             prod= 92 test= 6  = 98   （内核 63 + 非内核 35）
```

```text
# flow 自己的 crate:: 引用构成（机械量）
crate:: 总处数 264  ·  指向自己 156（crate::business::flow::…，抽包后改 crate::…）
= 出边 98 代码 + 10 注释
```
