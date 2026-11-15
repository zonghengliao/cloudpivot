# P2-51 人力资源域抽 crate 收口 —— 作废（`hr` 按 §P2-C 并入 C11 `cloudpivot-hr`）

> 卡：`t_1e274276`（P2-51【P2-C】抽 `hr` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `4b95fad3`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41`（`11f490f2`）、`P2-42`（`e9a31e96`）、`P2-43`（`939ec719`）、`P2-44`（`810ce2c7`）、
> `P2-45`（`5a2bc55a`）、`P2-46`（`ca11c5dc`）、`P2-47`（`4648b330`）、`P2-48`（`6a473699`）、
> `P2-49`（`c1f7aa76`）、`P2-50`（`4b95fad3`）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-hr/`，不搬 80 个文件 / 63 378 行，不改任何
`crate::business::hr` import，不动 `backend/app/Cargo.toml`，不动 `backend/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；
卡面验收命令 `cargo test -p cloudpivot-hr` 因此按预期失败（不是回归，见 §5）。

权威依据已入仓：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 **288** 行：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同表第 **224** 行 C11 = `cloudpivot-hr`｜节点 `hr`｜前置 **C1、C3、C6、C8、C9**｜回边 25｜
  **本步要切 7 行（flow 7）**｜机械量 `80 / 63 378 · 54 · 24 · 36 · 87`；
- 同节 §P2-C.1（判据看**出边**）、§P2-C.4（每张卡通用施工单，第 1 步「先付解环账」）、
  §P2-C.5（为什么**不**走「域 crate 反向依赖单体」）；
- `P2-42` §4 台账第 **90** 行（P2-51）= `80 / 63 378`｜SCC 24｜入边 `25（4）`｜出边目标 4｜`42 行`｜`退 1`｜落点 `C11 cloudpivot-hr`。

**本卡与前 9 张（P2-42~P2-50）有一处关键区别，先写在最前面**：

> 前 9 张作废的落点是「**留在 C13 `erp` 包内，永不单独成包**」；
> `hr` **不是**这样 —— 它**确实会成为一个独立的 crate**，就在 **C11 `cloudpivot-hr`**。
> 本卡作废的唯一原因是**卡片位置错了**，不是「抽包不可行」：
>
> 1. C11 的五张前置卡（C1 内核 / C3 support / C6 changelog / C8 masking / C9 impexp）**一张都还没建**
>    （`crates/` 里只有 `cloudpivot-doc`，见 §2.3），而 `hr` 当前出边指向的正是这几个包
>    + `flow`（C12，在 C11 之后）；
> 2. 卡面 4 步（建骨架 → 移代码 → 加依赖 → 修 import）**缺了 §P2-C.4 第 1 步「先付解环账」**：
>    C11 卡面明写本步要切 **7 行 `flow` 接缝**，卡面 P2-51 一行都没提。
>
> 换句话说：本卡的**工作量描述不成立**（照它做必然留下包级环），而 C11 卡的描述是成立的。
> 下一轮要重开，请直接开 C11，别重开 P2-51。

## 2. `hr` 的实测（只读、无库依赖，2026-11-15 于 99）

### 2.1 工具原始输出（逐字）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain hr --layer-check
域：hr（scope=top）
  规模：80 个文件 / 63378 行
  SCC：24 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（23）：aiagent, announcement, attachment, business/shared, business/task,
    changelog, docpdf, erp, flow, help, hr, impexp, masking, middleware, mutex, notice, pdf,
    permission, shared, storage, support, system, task, utils
  回边（留在应用 crate 的代码 → 本域）：25 处代码级
    app#root               code=  1 all=  1  bootstrap.rs:107
    erp                    code=  3 all=  3  business/erp/cost/cost_labor_db_tests.rs:65,
                                                 business/erp/cost/cost_labor_import.rs:67,
                                                 business/erp/cost/cost_labor_import.rs:1153
    flow                   code= 10 all= 10  business/flow/handler.rs:1807, 1809, 1889, 1891,
                                                 1938, 1940, 1965, 1968, 2028, 2031
    impexp                 code= 11 all= 11  business/impexp/builtin/hr.rs:69,
                                                 builtin/hr/db_tests.rs:36, builtin/hr/employee.rs:109,110,
                                                 builtin/hr/import_post.rs:75,76
  出边（本域 → 其它）：7 个目标
    business/shared        code= 82 all= 87
    shared                 code= 73 all= 73
    utils                  code= 25 all= 25
    impexp                 code= 18 all= 22
    masking                code= 14 all= 14
    flow                   code=  7 all=  7
    changelog              code=  3 all=  3
  层次判据：本步要切 4 条 / 42 行 —— 出边指向更下层以外：impexp(18), masking(14), flow(7), changelog(3)
            （假设 L1 内核 shared, utils, business/shared 已抽出、装配层 bin 已独立成包；
              完整卡序见 --plan-order）
  结论：不可抽（25 处代码级回边，4 个来源）
exit=1
```

三条退出码（本节全部实测，日志见 §6 复现命令）：

| 口径 | 命令 | 退出码 | 含义 |
|:--|:--|:--:|:--|
| 层次判据（推荐） | `--domain hr --layer-check` | **1** | 出边 4 条 / 42 行落在「更下层」之外 ⇒ 现在抽会成环 |
| 旧口径（方向反了） | `--domain hr --check` | 1 | 数的是入边 25，恒不为 0，无法区分「可抽/不可抽」 |
| 卡序模拟 | `--plan-order docs/planning/20261113-新卡清单.md` | **0** | 全序累计 66 行解环账；C11 排在 C1/C3/C6/C8/C9 之后 ⇒ **按卡序抽可行** |

`--plan-order` 退 0 是「本卡不能现在做、但 C11 能做」的机器证据：工具把整条卡序模拟完，
每一步抽包后包图无环；**「退 0」的前提是 C1~C9 先落地**。

### 2.2 SCC 成员资格：`hr` 在顶层 24 节点环里（与 `erp` 子域不同）

`hr` 是**顶层业务域**，不是 `erp` 的子目录，所以它的 SCC 口径与 P2-42~P2-50 完全不同：

| | P2-42~P2-50（`erp` 子域） | **P2-51（`hr`）** |
|:--|:--|:--|
| SCC 口径 | `--scope erp-sub` ⇒ 51 节点 | `--scope top` ⇒ **24 节点**（默认） |
| 环的性质 | `erp` 内部咬合（`erp` 整体一个包） | 顶层 24 个域互相咬合 |
| 解环方向 | 无（C13 一次性整包搬） | **有**：按 C1→C15 层次序，每卡付自己那几行 |

24 个节点同属一个 SCC 的含义：**任何人**先抽任一域，都会留下「新包 → 还在单体的包」的出边；
合法出路只有「先把内核与装配层抽出（C1/C2），再按出边闭包从浅到深抽域」。
`hr` 在闭包里排第 11 位 —— 它前面有 10 张卡要先把它的 4 类出边变成「更下层」。

### 2.3 workspace 现状：`cloudpivot-hr` 不存在，`hr` 还在单体里

```bash
$ cargo metadata --no-deps --offline --format-version 1 --manifest-path backend/Cargo.toml | jq -r '...'
workspace_root= /opt/cloudpivot/backend
  package: cloudpivot      -> /opt/cloudpivot/backend/app/Cargo.toml
  package: cloudpivot-doc  -> /opt/cloudpivot/backend/crates/cloudpivot-doc/Cargo.toml
members= 2
```

即：workspace 仍是 P2-40（`35a306be`）落地时的 2 个包（P2-40 的卡面依赖已满足），
**没有** `cloudpivot-hr`；C11 的五张前置卡一个都还没交付。

### 2.4 规模与机械量（本次实测，与计划表并排）

| 项 | 计划表 §P2-C.3 | 本次实测 | 口径 |
|:--|--:|--:|:--|
| 文件 / 行 | 80 / 63 378 | **80 / 63 378** | `find business/hr -name '*.rs'` |
| 占 `backend/app/src` 比例 | — | **10.78%**（63 378 / 587 800） | 分母 790 个 `.rs` |
| 路由宏（生产区） | 54 | **50** | `#[nik::get/post/put/delete/patch(` 在 `#[cfg(test)]` 之外；含 `/business/hr/health` |
| `#[nik::permission]` | — | **47** | 47 条受权限码保护；另 2 条 `#[nik::auth_only]`（`/attendance/my`、`/leave/my`） |
| `pub mod` 声明 | — | **47** | `business/hr/mod.rs` |
| 真库用例文件 | 24 | **24** | `business/hr/*_db_tests.rs` |
| 出目录引用（`include_str!` 带 `../`） | 36 | **36** | 全部指向 `migrations/business/*.sql`，C11 要改锚定式 |
| 夹具引用 | 87 | **104** | `test_helpers\|fixture_hygiene\|SEED_ROLE_LOCK\|ensure_any_`（本次口径更宽，含 `#[cfg(test)]` 内） |
| 模块用例总数（实跑，含 24 个真库文件 + 静态守卫用例） | — | **695** | `cargo test --lib business::hr`，见 §5.3 |

> 路由数 50 与计划表 54 的差异：本次只数**属性行**（`#[nik::get("…")]` 这种宏调用）；
> 计划表口径未注明，可能含 `HEALTH_PATH` 常量与 doc 表里的路径行。两者不影响任何判据。

## 3. 逐行复核：入边 25 行 / 出边 43 行（生产 25 · 测试 18）

工具给的是「处数」，本卡按 P2-49/P2-50 的做法把**每一行**都打开核了一遍
（判据脚本把 `*_tests.rs` 整文件 + `#[cfg(test)] mod` 块识别为测试区，注释单列）。

### 3.1 出边：`hr` → 其它（`crate::` 绝对路径 234 处 = 234 行，一行一处）

`hr` 目录内共 **600 行 / 603 处** `crate::` 引用，其中 **369 处指向自己**
（`crate::business::hr::…`，抽包后变 `crate::…`，同包内改路径 —— 机械量，不算账），
剩下的 234 处才是出边：

| 目标 | prod | test | comment | 合计 | 归属 | 现在能不能存在 |
|:--|--:|--:|--:|--:|:--|:--|
| `business::shared` | 39 | 43 | 5 | 87 | **L1 内核**（C1） | ❌ 等 C1 |
| `shared`（`src/shared`） | 35 | 38 | 0 | 73 | **L1 内核**（C1） | ❌ 等 C1 |
| `utils`（`src/utils`） | 25 | 0 | 0 | 25 | **L1 内核**（C1） | ❌ 等 C1 |
| `business::impexp` | 12 | 6 | 4 | 22 | **C9**（前置里） | ✅ 合法向下箭头 |
| `business::masking` | 6 | 8 | 0 | 14 | **C8**（前置里） | ✅ 合法向下箭头 |
| `business::flow` | 6 | 1 | 0 | 7 | **C12（在 C11 之后）** | ❌ **C11 的 7 行解环账** |
| `business::changelog` | 1 | 2 | 0 | 3 | **C6**（前置里） | ✅ 合法向下箭头 |
| `crate::bootstrap` | 0 | 1 | 0 | **1** | **C2 装配层（bin）** | ❌ **工具漏报，见 §3.3** |
| **合计** | **124** | **100** | **10** | **234** | | 代码行 **223**（prod 124 + test 99） |

剥掉内核（185 处：`business/shared` 87 + `shared` 73 + `utils` 25）与非代码行后，
**真正的解环账 = 43 行代码**：

- 等前置卡的（合法）：`impexp` 18（prod 12 / test 6）、`masking` 14（6/8）、`changelog` 3（1/2）、
  `crate::bootstrap` 1（0/1）→ **36 行**（prod 19 / test 17）；
- **C11 自己必须切的**：`flow` **7 行**（prod 6 / test 1）。
- 校验：36 + 7 = **43 行** = 出边 223 行代码（prod 124 + test 99）去掉非代码行后的解环账总额。

`flow` 那 7 行逐行如下（这 7 行就是 C11 卡面「本步要切 7 行 flow」的全部内容）：

| # | 位置 | 引用 | 用途 |
|--:|:--|:--|:--|
| 1 | `business/hr/leave_bridge.rs:50` | `flow::{engine, model as flow_model}` | 请假审批：发起 / 撤销 / 读实例状态 |
| 2 | `business/hr/leave_handler.rs:75` | `flow::approval_gate::{self, ApprovalGate}` | 提交前 SoD 自审拦截 |
| 3 | `business/hr/leave_handler.rs:76` | `flow::{engine, instance_pdf, model as flow_model}` | 提交 / 撤回 + 审批单 PDF |
| 4 | `business/hr/salary_bridge.rs:93` | `flow::{engine, model as flow_model}` | 工资单审批桥 |
| 5 | `business/hr/salary_sheet_handler.rs:56` | `flow::approval_gate::{self, ApprovalGate}` | 同上（工资单） |
| 6 | `business/hr/salary_sheet_handler.rs:57` | `flow::{engine, model as flow_model}` | 同上（工资单） |
| 7 | `business/hr/leave_bridge_db_tests.rs:36` | `flow::engine`（**测试边**） | 夹具直接驱动引擎 |

### 3.2 入边：其它 → `hr`（25 行代码 / 4 个来源）

| 来源节点 | prod | test | comment | 代码合计 | 内容 |
|:--|--:|--:|--:|--:|:--|
| `app#root` | 1 | 0 | 0 | 1 | `bootstrap.rs:107` → `hr::employee_reader_impl::install()`（**已有的接缝**，方向正确） |
| `business/flow` | 10 | 0 | 0 | 10 | `handler.rs` 撤回/取消/自审/回调 4 个触发点 → `hr::leave_bridge` / `hr::salary_bridge` |
| `business/impexp` | 5 | 6 | 0 | 11 | `builtin/hr.rs`、`employee.rs`、`import_post.rs` 取 `SENTINEL_ORG_ID` / `employee_no` / `attendance_period_model` |
| `business/erp` | 1 | 2 | 1 | 3 | `cost/cost_labor_import.rs:67` 用 `hr::employee_reader`（+2 处测试） |
| **合计** | **17** | **8** | **2** | **25** | 另有 1 行字符串字面量不计（见 §3.3） |

### 3.3 工具漏报与假阳性（3 条，最终以本节为准）

| 类型 | 位置 | 说明 | 对本卡结论的影响 |
|:--|:--|:--|:--|
| **漏报（出边）** | `business/hr/salary_bridge_db_tests.rs:74` → `crate::bootstrap::install_voucher_port_for_tests()` | 工具只认登记过的节点前缀（`business/*`/`shared`/`utils`/…），`crate::bootstrap` 不在其中 ⇒ **1 行出边被漏** | 出边代码行 42 → **43**；且这行是 **hr → bin（C2）**，C2 之后是反向箭头，C11 必须把它改成 feature/夹具路径（见 §7） |
| 假阳性 | `business/hr/employee_reader_tests.rs:254` `!source.contains("crate::business::")` | 防「员工只读接口反向依赖 business 目录」的**守卫断言**，字符串字面量，不是路径 | 不计边（工具也没算） |
| 假阳性 | `business/hr/employee_reader_tests.rs:246` doc 注释里的 `crate::business::xxx` | 注释里的示例路径 | 不计边 |

> 这条漏报与 P2-49/P2-50 发现的是**同一类工具缺陷**（工具只数它认识的节点名）。
> C11 开工前必须把 `crate::bootstrap` 这类「指向 bin 的测试边」纳入统计，否则搬包当天才发现。

## 4. 为什么本卡不能按卡面 4 步直接抽（五条，都带实证）

### 4.1 顺序不对：前置 5 张卡一张都没建，`hr` 的 185 行内核引用无处可去

`hr` 出边里 **185 处（占出边 79%）** 指向 L1 内核（`business/shared` 87 / `shared` 73 / `utils` 25）。
C1 之前，`crate::shared` / `crate::utils` 只存在于 **单体 `cloudpivot` 包**里；
此时把 `hr` 抽成 `cloudpivot-hr` 并改 import，得到的是
`cloudpivot-hr → cloudpivot`，而 `cloudpivot`（app）同时必须 `→ cloudpivot-hr`（否则 50 条路由的
`inventory` 项进不了链接单元 ⇒ 静默 404，§P2-C.4 第 6 步）⇒ **包级环，`cargo check` 直接红**。
这正是 §P2-C.5 明确「不采用」的过渡路线。

### 4.2 双向咬合：`hr ↔ flow`、`hr ↔ impexp` 各有一对反向边，且**账单归属不同卡**

| 边 | 行数（prod/test） | 谁付这笔账 | 依据 |
|:--|--:|:--|:--|
| `impexp → hr` | 5 / 6 | **C9**（impexp 先抽） | C9 卡面「本步要切 **13 行**：`hr` 11、`task` 2」——这 11 行就是它 |
| `hr → impexp` | 12 / 6 | **不需要付** | impexp 已在 C9 成包 ⇒ 合法向下箭头 |
| `flow → hr` | 10 / 0 | **不需要付** | C12 前置含 C11 ⇒ `flow → hr` 合法 |
| `hr → flow` | 6 / 1 | **C11（本卡族）** | C12 在 C11 之后 ⇒ 必须由 C11 切成接缝（C11 卡面：7 行） |

规则的形状很清楚：**同一对域的两条反向边，由「先抽的那张卡」付清**，后抽的只剩合法向下箭头。
P2-51 的卡面 4 步里**没有任何一条**认领这笔账 —— 照它做，抽完 `hr` 就同时和 `flow`、`impexp`
成环（`hr → flow → hr` 在生产代码里是活的：`flow/handler.rs` 撤回/取消/回调 4 个触发点调
`hr::leave_bridge`，`hr/leave_handler.rs` 提交调 `flow::engine`）。

### 4.3 卡面漏了 §P2-C.4 第 1 步「先付解环账」

§P2-C.4 是每张卡的通用施工单，第 1 步就是「按本卡『本步要切』清单，把那几行倒转成接缝
（照抄 P2-B 先例：`voucher_port` / `flow::doc_reader` 注册表 / `hr::employee_reader` /
`task::approval_port`），切完 `--plan-order` 必须绿、`cargo check --workspace` 必须过」。
C11 卡面写的是「7 行 flow」；P2-51 卡面写的是「建骨架 / 移代码 / 加依赖 / 修 import」——
**同一件事在 C11 有解环账、在 P2-51 没有**。这是本卡作废的第二个硬理由（不是工作量估算问题）。

### 4.4 装配层（C2 bin）尚未独立：`hr → bin` 的 1 行测试边现在无法安放

`business/hr/salary_bridge_db_tests.rs:74` 调 `crate::bootstrap::install_voucher_port_for_tests()`。
`crate::bootstrap` 是 **bin 的启动链**（`app/src/bootstrap.rs`）。C2 之前 bin 与域在一个包里，
这行还能编译；`hr` 一旦成包而 bin 还没独立，它就成了 `cloudpivot-hr → cloudpivot` 的反向箭头，
**测试配置下就该红**。C11 施工时必须把它换成「bin 暴露的测试 feature」或内核里的测试夹具
（§P2-C.4 第 5 步的 `test-support` feature）。

### 4.5 测试面：695 个真库用例、24 个 `*_db_tests.rs`、104 处夹具引用

`hr` 的真库用例**不只在 `*_db_tests.rs` 里**（`employee_sensitive.rs:860`、`attendance_record_model.rs:766`
等生产文件里的 `#[cfg(test)] mod` 也有）。§P2-C.5 第 3 条讲的陷阱在这里最重：

- 夹具 `crate::shared::test_helpers::{ensure_any_dept, ensure_any_user}` 与 `business/shared` 的
  `SEED_ROLE_LOCK` 现在都是 `#[cfg(test)]` —— 抽包后**跨包不可见**；
- 36 处 `include_str!("../../../migrations/business/V0383…")` 在包根变化后**路径全断**
  （C11 施工单第 4 步要求改 `concat!(env!("CARGO_MANIFEST_DIR"), …)` 锚定式）；
- 若 24 个真库文件留在 `app` 包空跑，这 695 条会退化成**静默 SKIP 假绿**
  （实测：不设 `ERP_DB_URL` 时单条用例 0.00s 打印 `SKIP: 未设置 HR_DB_URL / ERP_DB_URL`，
  设了以后同一条 3.13s 真连库 —— 见 §5.3）。

### 4.6 收益：不是「不值得抽」，是「必须整卡按 C11 抽」

`hr` 占 `backend/app/src` **10.78%**（63 378 / 587 800 行），80 文件、50 条路由、47 个 `pub mod`。
这是一个**值得成包**的体量（对比 C5 的 `docpdf` 631 行、C6 的 `mutex` 2 278 行都单独成包）。
所以本卡的正确处置是**并入 C11**，而不是像 P2-42~P2-50 那样「留在 `erp` 包内永不单抽」。

## 5. 卡面两条命令的真实返回（说明「失败」是刻意的）

### 5.1 `cargo build --workspace`

```bash
$ cd /opt/cloudpivot/backend && cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.13s
$ echo $?
0
```

（零改动 ⇒ 命中缓存，0.13s；退出码 0 是本次单独捕获的，不是管道里 `tail` 的状态。）

### 5.2 `cargo test -p cloudpivot-hr -- --test-threads=1`

```bash
$ cargo test -p cloudpivot-hr --no-run --offline
error: package ID specification `cloudpivot-hr` did not match any packages

help: a package with a similar name exists: `cloudpivot`
$ echo $?
101
```

**这是「不建该 crate」的直接证据**，不是回归：包里没有 `cloudpivot-hr`，命令本身不成立。
（`--no-run` 也失败，说明连编译单元都不存在；卡面要求的这套验收随作废一并失效。）

### 5.3 替代验证：`hr` 模块全量用例绿（拆前基线，含 24 个真库用例文件）

作废不等于不验证 —— 本卡给了「`hr` 留在单体时基线是绿的」这一条实测（695 条里含 24 个
`*_db_tests.rs` 的真库用例，其余是静态守卫/契约用例），让 C11 将来有可比对的拆前/拆后口径：

```bash
# 99 上跑：连 145 的 PG（99 本机 PG 已 purge）
$ HOST=172.18.10.145
$ PGPASSWORD=… createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_p251_hr_drill
$ ERP_DB_URL="postgres://cloudpivot:…@$HOST:5432/cp_p251_hr_drill" \
    cargo test -p cloudpivot --lib business::hr -- --test-threads=1
...
test result: ok. 695 passed; 0 failed; 0 ignored; 0 measured; 4986 filtered out; finished in 222.67s
EXIT=0
```

反假绿对照（同一套用例，不设 URL）：

```bash
$ env -u ERP_DB_URL cargo test -p cloudpivot --lib business::hr::self_service_db_tests::db::salary_my \
    -- --test-threads=1 --nocapture
test …salary_my_hides_pending_sheets_and_returns_only_mine ... SKIP: 未设置 HR_DB_URL / ERP_DB_URL（不连真实数据库）
ok           ← 0.00s
# 设 URL 后同一条：
test …salary_my_hides_pending_sheets_and_returns_only_mine ... HR41 真库用例已连接：database=cp_p251_hr_drill
[HR 夹具卫生] 清场：工资单 0/考勤记录 0/…/员工 0/孤儿状态日志 0（库 cp_p251_hr_drill）
ok           ← 3.13s
```

**⚠ 库名必须含 `drill` / `test`**（`business/hr/fixture_hygiene.rs:140` 的 `assert_scratch_db`）：
本卡第一次跑用卡面模板的 `cp_p251_hr`，**41 条用例 panic 拒跑**
（`[HR 夹具卫生] 拒绝在库 cp_p251_hr 上清场`），改名 `cp_p251_hr_drill` 后 695 条全绿。
卡面「数据库约束」给的模板 `cp_<卡号>_<用途>` 对 HR 不成立，**用途位必须带 `drill`/`test`**。
这是环境口径，不是代码缺陷（守卫是对的：清场会删全库员工）。

## 6. 复核命令（只读，任意机器可跑）

```bash
cd /opt/cloudpivot

# ① 工具三个口径（期望：layer-check 退 1 / 旧 --check 退 1 / plan-order 退 0）
python3 scripts/verify/crate_split_readiness.py --domain hr --layer-check ; echo "exit=$?"
python3 scripts/verify/crate_split_readiness.py --domain hr --check      ; echo "exit=$?"
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md ; echo "exit=$?"

# ② 规模与机械量
cd backend/app/src
find business/hr -name '*.rs' | wc -l                                     # 80
find business/hr -name '*.rs' -exec cat {} + | wc -l                      # 63378
grep -rhE '^[[:space:]]*#\[nik::(get|post|put|delete|patch)\(' business/hr | wc -l   # 50
grep -rhE '^[[:space:]]*#\[nik::permission\b' business/hr | wc -l         # 47
grep -rhE '^[[:space:]]*#\[nik::auth_only\b' business/hr | wc -l          # 2
ls business/hr/*_db_tests.rs | wc -l                                      # 24
grep -rn 'include_str!\|include_bytes!' business/hr | grep -c '\.\./'     # 36

# ③ 入边（25 行 / 4 源）
cd /opt/cloudpivot/backend/app/src
grep -rn 'business::hr' --include=*.rs . | grep -v '^\./business/hr/' | wc -l   # 28（含 2 注释 + 1 字符串）

# ④ 出边目标分布
grep -rhoE 'crate::(business::[a-z_]+|shared|utils|bootstrap)' business/hr | sort | uniq -c | sort -rn

# ⑤ workspace 里没有 cloudpivot-hr
cd /opt/cloudpivot/backend && cargo metadata --no-deps --offline --format-version 1 | jq -r '.packages[].name'
```

## 7. 保留物 / 后继

### 7.1 给 C11 `cloudpivot-hr` 的施工单（本卡唯一「产出」，不影响卡序）

1. **前置**：C1（内核）、C3（support）、C6（changelog）、C8（masking）、C9（impexp）全绿后再开工 ——
   尤其 C9 必须已把 `impexp → hr` 那 **11 行**付掉（C9 卡面写的 13 行里的 hr 部分），
   否则 C11 一开就顶在 `impexp ↔ hr` 上。
2. **解环账 = 7 行 `flow`**（§3.1 表已逐行列全）：`apply` 方向是 `flow → hr`（合法，C12 后抽），
   要切的是 `hr → flow`：建议把 `flow::engine`（提交/撤回）+ `flow::approval_gate`（SoD 自审）
   + `flow::instance_pdf`（审批单 PDF）收成一个 `flow_port` 注册表（照 `business/shared::voucher_port`
   的现有范式），`hr` 只依赖 trait、由 bin 或 `flow` 在启动时注册；`leave_bridge_db_tests.rs:36`
   那行测试边一并改到 trait 上。切完 `--domain hr --layer-check` 应退 0。
3. **`hr → bin` 的 1 行测试边**（`salary_bridge_db_tests.rs:74` 的 `crate::bootstrap::install_voucher_port_for_tests()`）
   改走 C1 的 `test-support` feature 或 C2 暴露的测试夹具 —— 工具不报这条，别漏。
4. **36 处 `include_str!("../../../migrations/business/…")`** 改锚定式
   `concat!(env!("CARGO_MANIFEST_DIR"), "/../../app/migrations/business/…")`（§P2-C.4 第 4 步）。
5. **104 处夹具引用**：内核 `test_helpers` 随 C1 走 feature；`hr/fixture_hygiene.rs` 随本域走
   （它是 HR 自己的，且带 `assert_scratch_db` 护栏，**搬包时不许弱化**）。
6. **路由条数门禁**：50 条路由宏（47 权限码 + 2 `auth_only` + 1 `/health`）搬完由 C2 的 bin 收集；
   合并前 `cargo test --lib system::menu::permission_audit_tests` + `business::hr::perm_matrix_tests`
   + `bash scripts/verify/menu_invariants.sh <库>` 必须全绿。
7. **真库口径**：拆前基线 = **695 passed / 0 failed**（§5.3，库名含 `drill`）；
   拆后同一条命令（`cargo test -p cloudpivot-hr --lib -- --test-threads=1`）条数不许少。

### 7.2 工具缺陷（建议给 `crate_split_readiness.py` 打补丁，本卡不改工具）

`crate_split_readiness.py` 的出边统计只认「已登记的节点前缀」，因此
**`crate::bootstrap`（指向 bin）这类边一律漏数**（本卡 1 行，位于 `salary_bridge_db_tests.rs:74`）。
建议：把 `bootstrap` / `lib` / `main` 加入节点前缀映射到 `app#root`，并在 `--layer-check` 里
把「域 → app#root」的**测试边**单列一行（它是 C2 之后必然违约的方向，不能静默通过）。
与 P2-49/P2-50 报的是同一类缺陷（工具口径比代码窄），累积三张卡了。

### 7.3 本卡没有留下的东西（避免下游误会）

- 没有新增/删除任何 `.rs`、`Cargo.toml`、迁移、脚本；
- 没有新增 crate 目录（`backend/crates/` 仍只有 `cloudpivot-doc`）；
- 没有提交任何测量脚本（判据脚本留在工作区，§3 的逐行结论已全部落表，可凭 §6 复现）。

## 附录 A 本卡实测原始日志（关键片段）

```text
# 出边按目标 × prod/test/comment（逐行复核脚本输出）
business::shared      prod= 39 test= 43 comment= 5 total= 87
shared                prod= 35 test= 38 comment= 0 total= 73
utils                 prod= 25 test=  0 comment= 0 total= 25
business::impexp      prod= 12 test=  6 comment= 4 total= 22
business::masking     prod=  6 test=  8 comment= 0 total= 14
business::flow        prod=  6 test=  1 comment= 0 total=  7
business::changelog   prod=  1 test=  2 comment= 0 total=  3
business::xxx         prod=  0 test=  0 comment= 1 total=  1   ← 注释示例，不计
business              prod=  0 test=  1 comment= 0 total=  1   ← 字符串字面量，不计
bootstrap             prod=  0 test=  1 comment= 0 total=  1   ← 工具漏报，实为出边
合计                  prod=124 test=100 comment=10 total=234

# 入边按来源 × prod/test/comment
business/impexp  prod= 5 test= 6 comment= 0 total= 11
business/flow    prod=10 test= 0 comment= 0 total= 10
business/erp     prod= 1 test= 3 comment= 1 total=  5   （test 里 1 行是字符串字面量）
app#root         prod= 1 test= 0 comment= 0 total=  1
shared           prod= 0 test= 0 comment= 1 total=  1   ← 注释
合计             prod=17 test= 9 comment= 2 total= 28 → 代码 25
```

```text
# 第一次跑（库名不含 drill/test）：41 fail，全部是夹具卫生护栏
thread '…salary_my_hides_pending_sheets_and_returns_only_mine' panicked at
  app/src/business/hr/fixture_hygiene.rs:142:5:
[HR 夹具卫生] 拒绝在库 `cp_p251_hr` 上清场：薪资引擎取数是全量读，本套夹具会删除库里所有员工 / 考勤 / 工资单。
请把 HR_DB_URL / ERP_DB_URL 指到演练库（库名含 `drill` 或 `test`，例如 `…/hr37_drill`）
test result: FAILED. 654 passed; 41 failed; …

# 改名 cp_p251_hr_drill 后
test result: ok. 695 passed; 0 failed; 0 ignored; 0 measured; 4986 filtered out; finished in 222.67s
EXIT=0
```
