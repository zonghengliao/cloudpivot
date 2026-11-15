# P2-54 导入导出域抽 crate 收口 —— 作废（`impexp` 并入 C9 `cloudpivot-impexp`）

> 卡：`t_ae4586dd`（P2-54【P2-C】抽 `impexp` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `17c41945`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41`（`11f490f2`）、`P2-42`（`e9a31e96`）、`P2-43`（`939ec719`）、`P2-44`（`810ce2c7`）、
> `P2-45`（`5a2bc55a`）、`P2-46`（`ca11c5dc`）、`P2-47`（`4648b330`）、`P2-48`（`6a473699`）、
> `P2-49`（`c1f7aa76`）、`P2-50`（`4b95fad3`）、`P2-51`（`0cc8554c`）、`P2-52`（`088c0586`）、
> `P2-53`（`17c41945`）
> 数据库：145（`172.18.10.145:5432`，用户 `cloudpivot`）· 演练库 `cp_p254_impexp_drill`（**已 DROP**）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-impexp/`，不搬 30 个文件 / 21 260 行，不改任何
`crate::business::impexp` import（**124 处**，见 §5.4），不动 `backend/app/Cargo.toml`，不动
`backend/Cargo.toml`。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；卡面验收命令因此按预期
失败（不是回归）：

```bash
$ cd backend && cargo test -p cloudpivot-impexp --offline -- --test-threads=1
error: package ID specification `cloudpivot-impexp` did not match any packages
exit=101
```

权威依据已入仓：

- `docs/planning/20261113-新卡清单.md` §P2-C.6：**P2-42 ~ P2-58 作废**，新位置 C3 ~ C15；
- 同文件 §P2-C.3 的 **C9** 行：`cloudpivot-impexp`｜节点 `impexp`｜前置 **C1、C3、C6、C7、C8**
  （5 张）｜回边 **40**｜**本步要切 13 行**（hr 11、task 2）｜机械量 `30 / 21 260 · 20 · 1 · 0 · 0`；
- 同节 §P2-C.1（判据看**出边**）、§P2-C.4（每张卡通用施工单）、§P2-C.5（为什么**不**走
  「域 crate 反向依赖单体」）；
- `P2-42` §4 台账 P2-54 行 = `30 / 21 260`｜SCC 24｜入边 `40（4）`｜出边目标 4｜`23 行`｜`退 1`｜
  落点 **`C9 cloudpivot-impexp`**；
- 本卡 2026-11-15 11:09 的改派留言（并入 C9）。

### 1.1 与前面 12 张作废卡的三点区别（实测，不是推断）

前面的 P2-43 ~ P2-53 大多是「**子域并进一个更大的包**」（`erp` 内部 51 节点 SCC，或随
`cloudpivot-erp`/`cloudpivot-hr` 整包）。**本卡不是那一类**：

1. **本卡范围 = C9 全量**，不是被更大的包吸收：`impexp` 是**独立节点、独立包**
   （C9 行 `包 = cloudpivot-impexp`，节点 `impexp`）。P2-54 与 C9 是**同一件事**，
   作废的实质是「**并入 C9 的卡序**」，不是「目标不成立」。
2. **唯一不成立的是时序**：C9 前置 C1（内核）、C3（support）、C6（changelog/permission）、
   C7（system）、C8（masking）。实测当前 `backend/crates/` 只有 `cloudpivot-doc`
   （`# P2-40 已抽出`），**C1 ~ C8 一张都没落地** ⇒ 现在开工只能得到
   `cloudpivot-impexp → cloudpivot(单体)` 的反向依赖，那正是 §P2-C.5 明确拒绝、并列为
   「会给后面每张卡定下错误范式」的过渡路线；且此时付掉的 13 行解环账会写进 **impexp crate**，
   而不是写进内核（C1 的位置）。
3. **13 行解环账成立、且已核到行**（§3）：`impexp → hr` 11 行 + `impexp → task` 2 行，
   其中 **7 行生产代码 / 6 行测试代码**。这 13 行是**唯一**挡在 C9 门口的东西，其余 10 行
   出边（masking 9 + system 1）在 C8/C7 完成后是合法的向下箭头。

⇒ 下一轮要重开，请**直接开 C9**（卡序第 9 位），别再开 P2-54。

## 2. `impexp` 的实测（只读、无库依赖，2026-11-15 于 99）

### 2.1 工具原始输出（逐字）

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain impexp --layer-check
域：impexp（scope=top）
  规模：30 个文件 / 21260 行
  SCC：24 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  同 SCC 的其它节点（23）：aiagent, announcement, attachment, business/shared, business/task, changelog,
    docpdf, erp, flow, help, hr, masking, middleware, mutex, notice, pdf, permission, shared, storage,
    support, system, task, utils
  回边（留在应用 crate 的代码 → 本域）：40 处代码级
    changelog              code=  1 all=  1  business/changelog/handler.rs:348
    erp                    code= 19 all= 19  business/erp/batch.rs:51, business/erp/batch.rs:52,
                                             business/erp/cash_flow.rs:1482, business/erp/cash_flow.rs:1483,
                                             business/erp/cost/cost_labor_handler.rs:55,
                                             business/erp/cost/cost_labor_import.rs:68
    hr                     code= 18 all= 22  business/hr/attendance_export.rs:170,
                                             business/hr/attendance_export.rs:240,
                                             business/hr/attendance_import_db_tests.rs:41, ...（22 处 all）
    task                   code=  2 all=  2  task/mod.rs:246, task/mod.rs:247
  出边（本域 → 其它）：7 个目标
    hr                     code= 11 all= 11
    masking                code=  9 all= 12
    utils                  code=  8 all=  8
    shared                 code=  8 all=  8
    business/shared        code=  8 all=  8
    task                   code=  2 all=  4
    system                 code=  1 all=  2
  层次判据：本步要切 4 条 / 23 行 —— 出边指向更下层以外：hr(11), masking(9), task(2), system(1)
            （假设 L1 内核 shared, utils, business/shared 已抽出、装配层 bin 已独立成包；完整卡序见 --plan-order）
  结论：不可抽（40 处代码级回边，4 个来源）
exit=1
```

```bash
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
…
C9     cloudpivot-impexp                            13    13   OK
                                              切向：hr(11), task(2)
…
全序累计要切：66 行
卡序覆盖 28 个节点；未进卡序（留在单体）2 个：app#root, business#root
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
exit=0
```

**`--layer-check` 说 23 行、`--plan-order` 说 13 行，两者不矛盾**：`--layer-check` 只假设
「L1 内核 + bin 已抽出」，所以把 masking(9)、system(1) 也算成要切；按卡序，C7（system）与
C8（masking）都在 C9 之前完成，那 10 行就变成**合法的向下箭头**（`impexp → cloudpivot-system`
/ `→ cloudpivot-masking`）。剩下 13 行 = hr(11) + task(2)，因为 C11（hr）/ C14（task）都排在
C9 **之后**。

> 补充：`impexp ⇄ masking` 这个环在 P2-39 已解成单向（`impexp → masking`），见
> `docs/architecture/P2-39-impexp与masking解环.md` 与 `business/masking/independence_tests.rs`；
> 本次实测 masking→impexp **0 行**，与解环结论一致。

### 2.2 规模与结构（30 文件 / 21 260 行）

| 分组 | 文件 | 行数 |
|:--|--:|--:|
| 引擎七件套 `definition / registry / engine / validate / template / model / dto` | 7 | 6 461 |
| 接口与执行 `handler / worker / cleanup / batch_export` | 4 | 3 861 |
| 模块声明 `mod.rs` + 脱敏 E2E 用例 `export_mask_tests.rs` | 2 | 233 |
| `builtin/` 内置业务定义（17 文件：`hr*` 5、`master_data` 8、`dict_item`/`erp_doc`/`user_export`/`mod` 4） | 17 | 10 705 |
| **合计** | **30** | **21 260** |

最大的 5 个文件：`handler.rs` 2 093、`builtin/hr/employee.rs` 2 022、`model.rs` 1 556、
`validate.rs` 1 429、`definition.rs` 1 358 行。

### 2.3 路由与权限码

```
$ rg -o 'permission = "[^"]+"' business/impexp/handler.rs | sort -u | wc -l
14                       # 去重后的权限码数
$ rg -c 'permission = "' business/impexp/handler.rs
20                       # 20 条 #[nik::permission] = 20 条路由（1 码 1 路由，无 no_auth/auth_only）
```

14 个码全部形如 `business:impexp:{def|import|export}:{list|upload|confirm|template|download|cancel|retry|create|delete}`
—— **没有页面显形码**（`*:page`），与本仓库约定 14 的「页面码与接口码解耦」不冲突。
20 条 `#[nik::permission]` 与 20 条路由宏一一对应（无 `no_auth` / `auth_only` / `health` 路由），
但码只有 14 个（如 `import:list` 被 3 条路由共用）。
路由路径都带 `/business/impexp/...` 前缀（宏路径不含 `/api`，约定 8）。

### 2.4 测试面

`cargo test --lib business::impexp` = **221 用例**（含 29 条真库门禁用例，见 §7）。
测试分布：4 个独立测试文件（`export_mask_tests.rs`、`builtin/hr/db_tests.rs`、
`builtin/hr/import_post_db_tests.rs`、`builtin/master_data/db_tests.rs`）
+ 各模块尾部 `#[cfg(test)] mod tests`（29 处 `#[cfg(test)]`、223 处 `#[test]`/`#[tokio::test]` 属性）。

## 3. 13 行解环账：逐行实证（C9 开工前必须切掉）

判定依据 §P2-C.1：**出边**指向「更下层以外」的才要切。C9 的上游（C1/C3/C6/C7/C8）已成立后，
指向 `hr`（C11）与 `task`（C14）的 13 行仍是反向箭头。

### 3.1 `impexp → hr` 11 行（prod 5 / test 6）

| # | 位置 | prod/test | 引用内容 | 切法 |
|--:|:--|:--:|:--|:--|
| 1 | `builtin/hr.rs:69` | prod | `use crate::business::hr::model::SENTINEL_ORG_ID` | 常量上提到内核（`Uuid::nil()`，见下） |
| 2 | `builtin/hr/employee.rs:109` | prod | `use crate::business::hr::employee_no` | 内核 port：`EmployeeNoAllocator`（hr 实现） |
| 3 | `builtin/hr/employee.rs:110` | prod | `SENTINEL_ORG_ID` | 同 #1 |
| 4 | `builtin/hr/import_post.rs:75` | prod | `use crate::business::hr::attendance_period_model` | 内核 port：`AttendancePeriodReader`（hr 实现，沿用 `employee_reader` 先例） |
| 5 | `builtin/hr/import_post.rs:76` | prod | `SENTINEL_ORG_ID` | 同 #1 |
| 6 | `builtin/hr/import_post.rs:591` | test | `attendance_period_dto::STATUS_LOCKED` | 用字面量 `"LOCKED"` 或 port 返回的状态枚举 |
| 7 | `builtin/hr/import_post.rs:594` | test | `attendance_period_dto::STATUS_DRAFT` | 同上 |
| 8 | `builtin/hr/import_post.rs:595` | test | `attendance_period_dto::STATUS_SUBMITTED` | 同上 |
| 9 | `builtin/hr/import_post_db_tests.rs:33` | test | `hr::model::SENTINEL_ORG_ID` | 同 #1 |
| 10 | `builtin/hr/import_post_db_tests.rs:785` | test | `attendance_period_dto::STATUS_DRAFT` | 同 #6 |
| 11 | `builtin/hr/db_tests.rs:36` | test | `hr::model::SENTINEL_ORG_ID` | 同 #1 |

**11 行全部落在 `builtin/hr*` 五个文件里**（`builtin/hr.rs`、`builtin/hr/{employee,import_post,db_tests,import_post_db_tests}.rs`，
合计 **5 486 行**），这不是巧合：**HR 的导入定义现在住在导出域的 `builtin/` 下**。
所以这 11 行有两条切法（§3.3），且两条都要动 `builtin/hr*`。

### 3.2 `impexp → task` 2 行（prod 2 / test 0）

| # | 位置 | prod/test | 引用内容 | 切法 |
|--:|:--|:--:|:--|:--|
| 12 | `worker.rs:55` | prod | `use crate::task::Scheduler`（`register_builtin(&Scheduler)` 的参数类型） | 内核 port `SchedulerPort`（C14 的 `Scheduler` 实现它），或把 `register_builtin` 改成接受闭包注册器 |
| 13 | `cleanup.rs:34` | prod | 同上 | 同 #12 |

这两行与 C14 的入边成对：`task/mod.rs:246-247` 调
`impexp::worker::register_builtin(self)` / `impexp::cleanup::register_builtin(self)`。
方向合法（C14→C9），但**类型 `Scheduler` 的方向不合法**（C9→C14），所以必须把「被注册方认识的
调度器形状」放进内核。

### 3.3 两种切法（`builtin/hr*` 的处理决定 11 行的代价）

| 切法 | 做法 | 13 行是否归零 | 代价 / 风险 |
|:--|:--|:--:|:--|
| **A. 接缝法（推荐）** | `SENTINEL_ORG_ID` 上提内核（纯常量，1 行改 3 处 import）；新增内核 port `EmployeeNoAllocator` + `AttendancePeriodReader`（照 `business/shared` 的 `employee_reader` / `voucher_port` 先例）；测试里 3 个状态常量改字面量 | **13 → 0** | 5 处 prod import + 2 个 port + 3 个测试常量；不动 `builtin/` 归属，impexp 保持「配置驱动注册表」自洽 |
| **B. 搬家法** | 把 `builtin/hr*`（`hr.rs` 1 281 + `hr/employee.rs` 2 022 + `hr/import_post.rs` 618 + 2 个 db_tests 1 565 行 = 5 486 行）整体搬到 HR 域（C11），impexp 只留 `DefinitionRegistry` 扩展点 | **13 → 0**（全部随文件搬走） | 需要先给 `DefinitionRegistry` 开**跨包注册入口**：现在 `registry.rs:169` 在**首次初始化时自己调** `builtin::register(&mut registry)`，且 `registry.rs:246` 有用例断言「重复注册必须失败」⇒ 注册点必须重设计（装配层注入 / link-time 注册表）。C9 时 hr 还在单体里，能做，但要改注册语义 |

**推荐 A**：11 行里只有 5 行生产代码，且 3 行是同一个 `Uuid::nil()` 常量；port 先例已有两处
（`hr::employee_reader`、`flow::doc_reader`）。B 的吸引点是「定义归业务域」更干净，
但它把 C9 的活变成「注册表重设计 + 1.5 万行搬迁」，且会与 C11 的 HR 卡撞同一批文件。

> 不论 A/B，**C9 的 13 行都不会为 0 以外的结果留口子**：`--plan-order` 要求 C9 行
> 「切向 hr(11), task(2)」全部消掉后工具才绿。

## 4. 40 行回边逐行实证（C9 之后要改的 import 面）

### 4.1 按来源（prod 33 / test 7）

| 来源 | 行数 | prod | test | 位置（抽样） |
|:--|--:|--:|--:|:--|
| `erp` | 19 | 18 | 1 | `statement.rs:1094-1095`、`report/mod.rs:1583-1584`、`cash_flow.rs:1482-1483`、`income_statement.rs:984-985`、`batch.rs:51-52`、`payable/payable_aging.rs:919-920`、`cost/cost_labor_import.rs:68,69,1093`（prod）+ `:1421`（test，在 `#[cfg(test)] mod tests` 内）、`cost/cost_labor_handler.rs:55`、`cost/cost_period_sheet.rs:55,443` |
| `hr` | 18 | 12 | 6 | `attendance_export.rs:170,240`、`employee_import_handler.rs:23-26`、`attendance_import_handler.rs:54-56,233`、`salary_export.rs:213,604`（prod）+ `attendance_import_db_tests.rs:41,42,180,529,590,697`（test） |
| `task` | 2 | 2 | 0 | `task/mod.rs:246-247`（`register_builtin(self)`） |
| `changelog` | 1 | 1 | 0 | `changelog/handler.rs:348`（`engine::write_sheet`） |
| **合计** | **40** | **33** | **7** | |

这 40 行**不需要在 C9 里剪掉**（方向合法：后落地的卡引用先落地的包），C9 只需把
`crate::business::impexp::…` 改成 `cloudpivot_impexp::…`。其中 2 行（`task/mod.rs`）会随 C14 一起搬，
7 行 test 边在对应卡片搬包时要跟着改。

### 4.2 工具口径核对（本域 5 类漏报形态实测 0）

`rg -n "impexp" -g '*.rs' -g '!business/impexp/**'` 得 **152 行**提到 `impexp`。逐行分类
（口径：去掉行尾 `//` 注释与 `"…"` 字符串字面量后，仍出现 `impexp` 才算代码级引用；
命令见附录 A 第 4 步）：

```
代码级引用 46 行 · 注释/字符串/路径字面量/标识符 106 行
46 行里 40 行是真依赖（与工具 code=40 一致），其余 6 行是「代码形态但不是依赖」：
  business/mod.rs:38                     pub mod impexp;（模块声明，搬包时删除）
  masking/independence_tests.rs:24       IMPEXP_TOKEN = concat!("crate::business::", "impexp")（闸门 token）
  masking/independence_tests.rs:68,153   fn masking_source_has_no_impexp_references /
                                         fn impexp_engine_consumes_the_masker_seam（函数名里带词）
  masking/independence_tests.rs:107,109,110  跨行字符串的尾行（assert 报文）
```

⇒ **工具没有漏报**：`impexp` 的 40 条真依赖 = 工具报的 40 条，逐条对得上。106 行非代码按类：

| 类 | 例子 | 是否代码边 |
|:--|:--|:--:|
| 指标名常量/断言 | `shared/observability.rs:95` `"cloudpivot_impexp_import_pending"`（10 行）、`task/metrics_sampler.rs`（15 行） | ✗ |
| 闸门 token 与报文 | `masking/independence_tests.rs`（9 行）、`masking/tests.rs`（2 行） | ✗（token **搬包会空转**，见 §6） |
| 路径字面量 | `masking/independence_tests.rs:154` `"/src/business/impexp/engine.rs"` | ✗（**搬包会红**，见 §6） |
| 原因/说明字符串 | `permission/scope_model.rs:118,125` `reason: "…(business/impexp/model.rs:470)"` | ✗ |
| 文档注释 | `hr/mod.rs`(4)、`erp/batch.rs`(4)、`notice/model.rs`(2)、`business/shared/layering_tests.rs:41`、`system/preference/model.rs:42` 等 | ✗ |

另核对三类「花括号/裸路径/`super::`」：

- **花括号 `use crate::business::{…, impexp}`**：全仓 **0 处**（本域与外部都没有）；
- **裸路径 `hr::`**：`builtin/mod.rs:59,62` 的 `hr::AttendanceImport` / `hr::employee::EmployeeImport`
  指向**本目录自己的** `builtin::hr`，**不是** `crate::business::hr`（最容易被误判成 2 条回边的地方）；
- **`super::`**：29 处，全在包内（`builtin/master_data/location.rs` 等），搬包后仍成立；
- **`crate::bootstrap`**：0 处（与 P2-51 的 flow 不同，本域不碰初始化器）；
- **引号里的代码路径**（`"crate::business::hr"` 这类）：0 处。

## 5. 机械量复核：计划表对得上的 4 项、要改的 4 项

### 5.1 对得上的

| 项 | 计划表（C9 行） | 实测 |
|:--|:--|:--|
| 文件 / 行 | 30 / 21 260 | 30 / 21 260 ✓（13 顶层 + 17 `builtin/`） |
| 路由 | 20 | 20 条 `#[nik::permission]` + 20 条路由宏，14 个去重码 ✓ |
| `include_str!` 出目录引用 | 0 | 1 处 `handler.rs:2014` `include_str!("handler.rs")` —— **同目录自读**，搬包后仍成立 ⇒ 出目录 = 0 ✓ |
| 测试夹具引用 | 0 | `test_helpers` / `fixture_hygiene` / `SEED_ROLE_LOCK` / `ensure_any_*` 全仓 0 命中 ✓（本域真库用例自带 `ensure_db()`，不借内核夹具） |

### 5.2 要改的四项

1. **真库用例：计划表记 1，实测 29 条**。计划表口径是「`*_db_tests.rs` 文件数」⇒ 只数到
   `builtin/hr/import_post_db_tests.rs` 一个；但按**行为**数，本域有 4 处 DB 门禁：
   `builtin/hr/db_tests.rs`（11 条）、`builtin/hr/import_post_db_tests.rs`（8 条）、
   `builtin/master_data/db_tests.rs`（8 条）、`builtin/erp_doc.rs`（2 条内联，`:879` `:953`）
   = **29 条**。C9 搬包时这 29 条要真库跑绿（§7），不能按「1 个文件」估。
2. **门禁/脚本硬编码源码路径 8 处**（计划表的机械量列**没有这一列**）：见 §6，其中 1 处会
   **静默失效**（不是变红）。
3. **124 处自身路径改写**：计划表只写「包内 `crate::business::<域>::x` → `crate::x`」，没给量级。
   实测 `crate::business::impexp` 在包内出现 **124 行**；再加内核 24 行（`utils` 8 / `shared` 8 /
   `business/shared` 8 → `cloudpivot_kernel::`）、下游 10 行（masking 9 + system 1）。
   ⇒ C9 单卡机械改写 ≈ **158 行包内 + 13 行解环 + 40 行外部 = 211 行**。
4. **`builtin/hr*` 5 文件（5 486 行）的归属**是 C9 的**设计决定**（§3.3 A/B），计划表没写；不定就会把
   「HR 的导入定义住在导出域」这个既有事实再带进新包。

## 6. 新发现：8 处「按源码路径硬编码」的门禁会在搬包时失效（跨卡议题）

抽 crate 不只是改 `use`。本仓有一批**按源码路径扫描**的门禁测试/脚本，它们用
`concat!(env!("CARGO_MANIFEST_DIR"), "/src/business/…")` 或清单里的相对路径定位文件。
`impexp` 涉及 **8 处**：

| # | 位置 | 形态 | 搬包后的后果 |
|--:|:--|:--|:--|
| 1-4 | `scripts/lint/scope_gate.py:208-211` | `SKIP_MANIFEST` 里 4 条 `business/impexp/…` 路径 | 清单指向不存在的文件 ⇒ 越界扫描名单失效（**静默变宽/变窄**） |
| 5 | `business/masking/independence_tests.rs:35` | `CARGO_MANIFEST_DIR + "/src/business/masking"` | 同时受 C8（masking 搬包）影响，扫描 0 文件 ⇒ 用例要么 panic 要么空转（有 `MASKING_SOURCE_MIN_FILES=4` 兜底，会红，属「好红」） |
| 6 | `business/masking/independence_tests.rs:24` | `IMPEXP_TOKEN = concat!("crate::business::", "impexp")`，配 `MASKING_TO_IMPEXP_REFS_BASELINE`（基线 0）计数闸 | 新代码里是 `cloudpivot_impexp::` ⇒ 计数恒 0 ⇒ 「masking→impexp 直连为 0」**空转**，P2-39 的解环闸门失守 |
| 7 | `business/masking/independence_tests.rs:154` | `/src/business/impexp/engine.rs` 读文件 | 文件不存在 ⇒ `unwrap_or_else(panic!)` 直接红（**好红**，会拦住合并） |
| 8 | `system/menu/permission_audit_tests.rs:232/744-748` | `Repo::load(backend_root().join("src"))` 只扫 `app/src` | **20 条 impexp 路由从审计面消失**（N16/N18 仍绿，但审计面缩小）；之后每抽一个 crate 都再少一批 ⇒ **静默失效，最危险** |

不受影响的（要说清，避免过度反应）：`scripts/verify/**` 里的 `impexp` 全是 **HTTP 路径**
（`/api/business/impexp/{def/list,import/page,export/sync,…}`，见 `v_smoke.py`、`v_a.py`、
`testd_http.sh`、`hr/attendance_import_route_smoke.sh`、`hr_salary_e2e.sh`、`common.py`）
—— 路由不变，脚本不用改。

**建议（给总管）**：这不是 impexp 一域的问题，`flow`（3 处）、`hr`（1 处）、`task`（1 处）、
`erp`（多处）都有同款硬编码。**每张 C* 卡都会再触发一次**。应该在 **C2（`cloudpivot-server`）**
里统一成「源码根可配置」：门禁扫描根 = `app/src` + `crates/*/src`（逐个 `CARGO_MANIFEST_DIR`
反推），否则 C3 ~ C15 每张卡都要各自补一遍、且补漏了是静默失效。

## 7. 基线测试（拆前全绿，2026-11-15 于 99，库在 145）

### 7.1 真库跑（演练库 `cp_p254_impexp_drill`）

```bash
PGPASSWORD=*** createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_p254_impexp_drill
# → created；182 张 public 表（模板库现为 182，非 181）
ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p254_impexp_drill" \
HR_DB_URL="同串" \
  cargo test --lib business::impexp --offline -- --test-threads=1
```

```text
test result: ok. 221 passed; 0 failed; 0 ignored; 0 measured; 5460 filtered out; finished in 22.84s
SKIP 行数：0                    ← 29 条真库用例全部真跑（没有一条落到 SKIP）
```

`builtin` 子集单独跑一遍（真库用例集中在这里）：

```text
running 116 tests
test result: ok. 116 passed; 0 failed; … finished in 22.45s
SKIP 行数：0
```

### 7.2 假绿对照（不设库地址）

```bash
env -u ERP_DB_URL -u HR_DB_URL cargo test --lib business::impexp::builtin -- --test-threads=1 --nocapture
```

```text
running 116 tests
test result: ok. 116 passed; 0 failed; … finished in 0.04s     ← 22.45s → 0.04s
SKIP 行数：4                                                    ← 每个文件各打一次（OnceCell）
  SKIP: 未设置 ERP_DB_URL（不连真实数据库）                        ×2（erp_doc / master_data）
  SKIP: 未设置 HR_DB_URL / ERP_DB_URL（不连真实数据库）             ×2（hr/db_tests / import_post_db_tests）
```

⇒ 29 条真库用例在无库时是「打印 SKIP 后返回」而不是假绿；本次以 22.84s / 0 SKIP 证明它们真跑了。

### 7.3 其它门

| 命令 | 结果 |
|:--|:--|
| `cd backend && cargo build --workspace --offline` | `Finished dev profile … in 0.13s`，exit 0 |
| `python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md` | 全序 66 行，C9 行 13/13 OK，`判定：绿`，exit 0 |
| `cargo test -p cloudpivot-impexp …` | exit 101 `package ID did not match`（符合预期，本卡不建包） |
| `cp_p254_impexp_drill` 生命周期 | 建 → 跑 → **DROP**（`pg_database` 计数 0，见附录 A） |

## 8. 结论与 C9 施工单

**本卡：作废**（不建 `backend/crates/cloudpivot-impexp/`、零生产代码改动、零迁移改动）。
本卡唯一产出 = 本文档：把 C9 的 13 行解环账核到了**行号**、把 40 行回边核到了**prod/test**、
把计划表 4 处口径补齐、并新发现 8 处路径门禁。

C9 `cloudpivot-impexp` 开工时的推荐顺序（照 §P2-C.4「通用施工单」落本域）：

1. **先付 13 行**（§3.3 A）：`SENTINEL_ORG_ID` 上提内核；新增内核 port
   `EmployeeNoAllocator`（`generate_emp_no`）与 `AttendancePeriodReader`（期间状态 + 锁定判定），
   hr 侧实现；测试里 3 个 `STATUS_*` 改字面量；`Scheduler` 走内核 `SchedulerPort`。
   完成后 `--plan-order` 必须仍绿（C9 行切向归 0）。
2. **建包搬文件**：`git mv backend/app/src/business/impexp → backend/crates/cloudpivot-impexp/src`，
   `mod.rs` 的 12 个 `pub mod` 保留；包内 124 行 `crate::business::impexp::` → `crate::`。
3. **改跨包路径**：内核 24 行 → `cloudpivot_kernel::`；masking 9 行 → `cloudpivot_masking::`；
   system 1 行（`export_mask_tests.rs:32` 的 `system::tests_support`，`pub(crate) mod`，
   需要 C7 侧 `pub` + `#[cfg(any(test, feature = "test-support"))]` 才能跨包可见 —— 这一行容易漏，
   它是 `#[cfg(test)]` 支持模块，属 §P2-C.5 点 3 那类问题）。
4. **外部 40 行**：erp 19 / hr 18 / task 2 / changelog 1 → `cloudpivot_impexp::…`；
   删除 `business/mod.rs:38` 的 `pub mod impexp;`。
5. **门禁/脚本 8 处**（§6）逐条改；其中 `permission_audit_tests` 的扫描根**建议在 C2 统一做**，
   C9 若先到就先在本卡补 `crates/*/src`（否则 20 条路由静默逃出审计）。
6. **路由条数不许静默少**：拆前 20 条；bin 必须 `[dependencies] cloudpivot-impexp`，
   否则 20 条路由在最终链接单元里**全部静默 404**。
7. **验收**：`cargo build --workspace` + `cargo test -p cloudpivot-impexp -- --test-threads=1`
   （221 用例 / 29 条真库，真库库名含 `drill`）+ `--plan-order` 绿 + `bash scripts/verify/check.sh`
   的 L0/L1。真库用例的 SQL 里只有 `sys_import_task` / `sys_export_task` / `biz_*` 表，
   搬包不影响迁移路径（`migrations/` 仍留在 `backend/app/migrations/`）。

---

## 附录 A 复现命令（全部只读，除建/删演练库）

```bash
# 1) 层次判据（本步要切 23 行 / 4 目标）
cd /opt/cloudpivot && python3 scripts/verify/crate_split_readiness.py --domain impexp --layer-check   # exit 1
# 2) 卡序一致性（C9 = 13 行）
python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md       # exit 0
# 3) 规模
find backend/app/src/business/impexp -name '*.rs' | wc -l                                   # 30
find backend/app/src/business/impexp -name '*.rs' -exec cat {} + | wc -l                    # 21260
# 4) 出边逐行（13 行解环账 + 10 行合法向下箭头）
cd backend/app/src && rg -n "crate::business::hr|business::hr::" business/impexp/            # 11
rg -n "masking" business/impexp/ | rg -v '^\S+:\s*[0-9]+://' | …                             # 9（另 3 行注释）
rg -n "crate::task" business/impexp/                                                         # 2（另 2 行注释）
rg -n "crate::system" business/impexp/                                                       # 1（另 1 行注释）
# 4b) 入边 152 行分类（去掉 // 注释与 "…" 字面量后再判）：把下面 12 行存成 /tmp/cls.py 后 `python3 /tmp/cls.py`
#   import re,subprocess,collections
#   SRC="/opt/cloudpivot/backend/app/src"
#   out=subprocess.run(["rg","-n","--no-heading","impexp","-g","*.rs","-g","!business/impexp/**","."],
#                      cwd=SRC,capture_output=True,text=True).stdout
#   code=[];non=collections.Counter()
#   for l in out.splitlines():
#       p,_,t=l.split(":",2)
#       if t.strip().startswith("//"): non[p]+=1; continue
#       probe=re.sub(r"//.*$","",re.sub(r'"(?:[^"\\]|\\.)*"','""',t))
#       (code.append(p) if "impexp" in probe else non.update([p]))
#   print(len(code)); print(collections.Counter(code).most_common()); print(non.most_common())
#   → 46 / 106；6 行「代码形态非依赖」见 §4.2
# 5) 入边逐行
rg -n "business::impexp|impexp::" business/erp/ business/hr/ task/ business/changelog/       # 40 代码行
# 6) 路由
rg -c 'permission = "' business/impexp/handler.rs                                             # 20
rg -o 'permission = "[^"]+"' business/impexp/handler.rs | sort -u | wc -l                     # 14
# 7) 真库基线（145）
PGPASSWORD=*** createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_p254_impexp_drill
cd backend && ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p254_impexp_drill" \
  HR_DB_URL="同串" cargo test --lib business::impexp --offline -- --test-threads=1            # 221 passed
# 8) 假绿对照
env -u ERP_DB_URL -u HR_DB_URL cargo test --lib business::impexp::builtin -- --test-threads=1 --nocapture  # 0.04s / 4 条 SKIP
# 9) 清理
PGPASSWORD=*** dropdb -h 172.18.10.145 -U cloudpivot cp_p254_impexp_drill
psql -h 172.18.10.145 -U cloudpivot -d postgres -tAc \
  "select count(*) from pg_database where datname='cp_p254_impexp_drill'"                      # 0
```

## 附录 B 40 行入边原始清单（`rg` 逐条）

```text
business/changelog/handler.rs:348                                   ← 1
business/erp/statement.rs:1094,1095                                 ← 2
business/erp/report/mod.rs:1583,1584                                ← 2
business/erp/cash_flow.rs:1482,1483                                 ← 2
business/erp/income_statement.rs:984,985                            ← 2
business/erp/batch.rs:51,52                                         ← 2
business/erp/payable/payable_aging.rs:919,920                       ← 2
business/erp/cost/cost_labor_import.rs:68,69,1093,(1421=test)       ← 4
business/erp/cost/cost_labor_handler.rs:55                          ← 1
business/erp/cost/cost_period_sheet.rs:55,443                       ← 2
business/hr/attendance_export.rs:170,240                            ← 2
business/hr/employee_import_handler.rs:23,24,25,26                  ← 4
business/hr/attendance_import_handler.rs:54,55,56,233               ← 4
business/hr/salary_export.rs:213,604                                ← 2
business/hr/attendance_import_db_tests.rs:41,42,180,529,590,697     ← 6（test）
task/mod.rs:246,247                                                 ← 2
                                                          合计 40（prod 33 / test 7）
```
