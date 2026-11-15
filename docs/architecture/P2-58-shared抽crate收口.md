# P2-58 `shared` 抽 crate 收口 —— 作废（`shared` 并入 C1 `cloudpivot-kernel`）

> 卡：`t_82304bcb`（P2-58【P2-C】抽 `shared` 为独立 crate）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `5672ff41`
> 架构拍板来源：`t_b287611c`（提交 `114108b5`，`docs/planning/20261113-新卡清单.md` §P2-C）
> 同批收口：`P2-41`（`11f490f2`）、`P2-42`（`e9a31e96`）、`P2-43`（`939ec719`）、`P2-44`（`810ce2c7`）、
> `P2-45`（`5a2bc55a`）、`P2-46`（`ca11c5dc`）、`P2-47`（`4648b330`）、`P2-48`（`6a473699`）、
> `P2-49`（`c1f7aa76`）、`P2-50`（`4b95fad3`）、`P2-51`（`0cc8554c`）、`P2-52`（`088c0586`）、
> `P2-53`（`17c41945`）、`P2-54`（`3e22d3fe`）、`P2-55`（`632ad268`）、`P2-56`（`74dde8b4`）、
> `P2-57`（`5672ff41`）
> 数据库：演练库 1 个（`cp_p258_kernel` @ 145，182 表，跑完即 DROP）——本域实测**零 DB 断言入口**（§7.2）

## 1. 本卡处置：作废（不建 crate、零生产代码改动）

不建 `backend/crates/cloudpivot-shared/`，不搬 13 个 `.rs` / 7 119 行，不改 7 行 `crate::…` 出边
与 552 行入边 import，不动 `backend/app/Cargo.toml`、不动 `backend/Cargo.toml`、不动
`backend/crates/`（`ls backend/crates/` 仍只有 `cloudpivot-doc`）。

卡面四条步骤（建骨架 / 移代码 / 加依赖 / 修 import）**全部不发生**；卡面验收命令因此按预期
失败（不是回归）：

```bash
$ cd backend && cargo test -p cloudpivot-shared --offline -- --test-threads=1
error: package ID specification `cloudpivot-shared` did not match any packages
STUB_EXIT=101

$ cd backend && cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s
BUILD_EXIT=0
```

工作区当前成员（`cargo metadata`，实跑）：

```
cloudpivot* 包： ['cloudpivot', 'cloudpivot-doc']
workspace_members：2
   path+file:///opt/cloudpivot/backend/app#cloudpivot@0.1.0
   path+file:///opt/cloudpivot/backend/crates/cloudpivot-doc#0.1.0
```

权威依据已入仓：

- `docs/planning/20261113-新卡清单.md` §P2-C.6 第 288 行：**P2-42 ~ P2-58 作废**；
- 同文件第 289 行：**P2-59（共享依赖收敛）→ 提前成第一张 → C1 `cloudpivot-kernel`**；
- 同文件 §P2-C.3 第 214 行的 **C1** 行：节点 `shared` / `utils` / `business/shared`｜前置
  **无（只依赖 L0 `cloudpivot-doc`）**｜回边 **552 / 244 / 480**｜**本步要切 10 行：support 3、
  system 2、aiagent 2、task 1、erp 1、storage 1**｜机械量 `32 / 21 826 · 0 · 0 · 2 · 2`；
- `P2-42` §4 台账（第 97 行）P2-58 行：`18 / 14 384`｜SCC 24｜入边 `480（18）`｜出边目标 2｜`3 行`
  ｜`退 1`｜落点 **`C1 cloudpivot-kernel`（另含 `src/shared`、`utils`）**；
- 本卡 2026-11-15 11:09 的改派留言（并入 C1）。
- 上游 `P2-57` 收口尾注（`5672ff41`）：**子卡 `t_82304bcb` 同为作废卡（并入 C1），建议批量关闭**。

### 1.1 与前 17 张作废卡的不同：本卡的「卡面目标」就是 C1 的**核心子集**

前 17 张作废的理由基本都是「时序 + 批量」（目标成立但不能现在动手）。本卡多一层：
**卡面写的包名和 C1 的包名是两个名字**。

- 卡面：`backend/crates/cloudpivot-shared/` → 外部 import 变成 `cloudpivot_shared::…`；
- C1：`cloudpivot-kernel`（`shared` + `utils` + `business/shared` 三者**同一个包**）→ `cloudpivot_kernel::…`。

全仓 `cloudpivot_shared` / `cloudpivot-shared` / `cloudpivot_kernel` / `cloudpivot-kernel` 在
`backend/app`、`backend/crates`、`scripts` 里**一处都没有**（实跑 grep 为空），说明还没人押过名字。
所以先按卡面抽出 `cloudpivot-shared`，C1 一开工就要：改包名 + 把 `utils`、`business/shared` 并进来
+ **再改一遍 1 254 行 import**。这 1 254 行是同一笔账付两次。

⇒ 本卡作废的实质：**范围被 C1 完整覆盖、且名字与 C1 冲突**，不是「这件事不值得做」。

## 2. `shared` 的实测（只读工具，2026-11-15 于 99）

### 2.1 三次 `--layer-check`（节点粒度）与两次 `--plan-order`

```bash
$ python3 scripts/verify/crate_split_readiness.py --domain shared --layer-check   ; echo exit=$?
域：shared（scope=top）
  规模：13 个文件 / 7119 行
  SCC：24 个节点同属一个强连通分量（>1 ⇒ 与它们不可能拆成两个无环包）
  回边（留在应用 crate 的代码 → 本域）：552 处代码级（28 个来源）
  出边（本域 → 其它）：6 个目标
    aiagent 2 · support 1 · task 1 · erp 1 · system 1 · storage 1
  层次判据：本步要切 6 条 / 7 行 —— 出边指向更下层以外：…
  结论：不可抽（552 处代码级回边，28 个来源）
exit=1

$ python3 scripts/verify/crate_split_readiness.py --domain business/shared --layer-check ; echo exit=$?
域：business/shared（scope=top）
  规模：18 个文件 / 14384 行
  回边：480 处代码级（18 个来源）
  出边：4 个目标 —— shared 13 · utils 6 · support 2 · system 1
  层次判据：本步要切 2 条 / 3 行 —— support(2), system(1)
exit=1

$ python3 scripts/verify/crate_split_readiness.py --domain utils --layer-check ; echo exit=$?
域：utils（scope=top）
  规模：1 个文件 / 323 行
  出边：1 个目标 —— shared 3
  层次判据：可抽（出边全部落在更下层内核 shared, utils, business/shared）
exit=0
```

（以上为摘录：入边逐源明细见 §4。）

三条一起读，结论只有一条：**`utils` 自己退 0，但它唯一的出边指向 `shared`**；`shared` 自己退 1
（出边 7 行去 aiagent/erp/system/storage/support/task 六个**尚未抽出**的域）；`business/shared` 退 1，
且它一边指向 `shared`(13) / `utils`(6)、一边被 `shared` 之外的 18 个节点引用。反向（`shared →
business/shared`）**代码行 0 行**（只有 `permission_alias.rs:31` 一行文档注释提到
`crate::business::shared::state_machine::AuthSnapshot::has_permission`），所以三者合包后这 22 行
内部边单向消失，不留残余。三者同属 24 节点 SCC ⇒ **拆成两个包必红**。

```bash
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
卡序文件：docs/planning/20261113-新卡清单.md · 15 步 · 图：30 节点
更下层基准（L1 内核）：shared, utils, business/shared
C1     cloudpivot-kernel                            10    10   OK
                                              切向：support(3), system(2), aiagent(2), task(1), erp(1), storage(1)
…
全序累计要切：66 行
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环
```

`--plan-order` 的 10/10 与 `--domain shared` 的 7 行、`--domain business/shared` 的 3 行
**逐目标相加正好相等**（support 1+2=3、system 1+1=2、aiagent 2、task 1、erp 1、storage 1）
—— 这是本卡两次独立测量与冻结卡序三边对账的硬证据。

### 2.2 卡面「`shared`」到底指哪个目录 —— 两个读法都落到 C1（本卡新发现 ①）

仓里有**两个** `shared`：

| 节点 | 路径 | 文件 | 行 | 工具入边 | 卡/台账口径 |
|:--|:--|--:|--:|--:|:--|
| `shared` | `backend/app/src/shared/` | 13 | 7 119 | 552（28 源） | 卡面字面（「抽 `shared`」）、C1 行、`P2-41` §L1 描述 |
| `business/shared` | `backend/app/src/business/shared/` | 18 | 14 384 | 480（18 源） | `P2-42` §4 台账第 97 行（P2-58 行） |
| `utils` | `backend/app/src/utils/` | 1 | 323 | 244（23 源） | 只有 C1 提到 |

也就是说：**`P2-42` 台账把 P2-58 记成了 `business/shared`，卡面写的是 `shared`，而 C1 是三者并包。**
三个口径指向同一个落点，但施工时如果按台账只搬 `business/shared`、或按卡面只搬 `src/shared`，
都会立刻撞上 §2.3 的 22 行内部边。本卡在这里把账钉死：**C1 = 三者一个包，32 文件 / 21 826 行。**

### 2.3 三者之间的 22 行内部边（拆两个包就成环的那部分）

工具三分量入边和 = 552 + 244 + 480 = **1 276 行**；本卡把目录内互指的行单列后：

| 内部边 | 行数 | 证据 |
|:--|--:|:--|
| `business/shared → shared` | 13 | `use crate::shared::error_code;` ×…（`cost_switch.rs:46`、`separation.rs:65` 等） |
| `business/shared → utils` | 6 | `principal.rs:637,646`、`state_machine.rs:3013,3018,3132,3277` |
| `utils → shared` | 3 | `utils/mod.rs:10`（`use crate::shared::constants;`）、`:43`（`error_code::PARAM_INVALID`）、`:294`（`error_code::INTERNAL_ERROR`） |
| **合计** | **22** | ⇒ 1 276 − 22 = **1 254 行**（= §4 的 C1 外部入边实测值，两条独立路径互证） |

## 3. 出边：C1 口径 10 行（本卡 `shared` 自己 7 行），逐行

工具报的行号 = 该 `crate::…` 出现在哪一行；「归属」= C1 施工单里怎么处理。

| # | 位置 | 原文（截断） | 目标 | prod/test | 归属（C1） |
|--:|:--|:--|:--|:--:|:--|
| 1 | `shared/startup_guard.rs:55` | `use crate::business::storage::cipher::{ENV_STORAGE_MASTER_KEY, MASTER_KEY_HEX_LEN, MasterKey};` | storage | prod（顶层 use） | 主密钥**解析**上提内核（AES 加解密留 storage） |
| 2 | `shared/startup_guard.rs:808` | `use crate::business::aiagent::hermes::Hermes;` | aiagent | prod（`fn warn_missing_safety_block` 内） | 该检查下移成 aiagent 插件启动回调 |
| 3 | `shared/startup_guard.rs:809` | `use crate::business::aiagent::safety;` | aiagent | prod（同上） | 同上（2、3 同一函数） |
| 4 | `shared/idle_session.rs:49` | `use crate::support::config::model::SysConfigModel;` | support | prod（顶层 use） | 配置读取走内核 `config_port` 注册表 |
| 5 | `shared/session_cache.rs:110` | `let flags = crate::system::user::model::load_session_flags(user_id).await?;` | system | prod（函数体） | 会话标志读取走内核 `session_flags_port` |
| 6 | `shared/observability.rs:427` | `let emitted = crate::task::metrics_sampler::Snapshot::default().gauges();` | task | **test**（`#[test] fn every_sampler_metric_is_registered`） | **用例搬回 app**（跨轴一致性断言归装配层） |
| 7 | `shared/permission_alias.rs:111` | `use crate::business::erp::contract::contract_handler;` | erp | **test**（`mod tests` 内） | **用例搬回 app**（同上） |
| 8 | `business/shared/cost_switch.rs:47` | `use crate::support::config::model::{SysConfigModel, parse_bool};` | support | prod（顶层 use） | 同 4（同一个 `config_port`） |
| 9 | `business/shared/cost_switch.rs:48` | `use crate::system::menu::dto::MenuRes;` | system | prod（顶层 use） | 403 闸的菜单过滤下移到 support/system |
| 10 | `business/shared/separation.rs:66` | `use crate::support::config::model::{self as config_model, SysConfigModel};` | support | prod（顶层 use） | 同 4 |

**合计 prod 8 / test 2。** 两个要点：

1. **4 行（#4/#8/#10 + 一半的 #5）都是「读 `sys_config` 的进程内快照」**——它们应该是**同一个**
   内核侧 `config_port`，不是四个 port。C1 一次性做完，后面 14 张卡就都不用再碰配置读取。
2. **#6/#7 是 test-only，但不是「加个 port 就完事」**：抽包后它们是**包级环**（test 配置下
   `kernel → erp` → `erp → kernel`）。正确处置是把这两条用例**搬回 `app`**——它们本来就是
   「内核的指标目录 vs 采样器实现」「权限码别名 vs ERP 合同处理器」这种**跨轴一致性**断言，
   归装配层天经地义。本卡如果照卡面搬 `shared` 而不处置这两行，`cargo test` 直接报
   `cyclic package dependency`，卡面验收第二条（`cargo test -p cloudpivot-shared`）不可能过。

### 3.1 卡面没写、但按卡面做完必然炸的一条：`test_helpers` 是 `#[cfg(test)]`

`shared/mod.rs:30-32`：

```rust
/// 真库用例的前置体检（TST-02）：`require_seed_data()` —— 空库缺种子时点名缺什么、怎么补。
#[cfg(test)]
pub mod test_helpers;
```

`shared` 一旦成为独立 crate，它作为**依赖**被编译时 `cfg(test)` 为假 ⇒ `test_helpers` 这个模块
在依赖方眼里**根本不存在**。而它被全仓 132 行 / 39 个文件引用：

| 使用者 | 行数 | 文件 |
|:--|--:|--:|
| `business/hr` | 92 | — |
| `business/erp` | 40 | — |
| **合计** | **132** | **39** |

⇒ 必须特性化成 `#[cfg(any(test, feature = "test-support"))]`，域包在 `[dev-dependencies]` 打开该
feature（这正是 `docs/planning/20261113-新卡清单.md` §P2-C.4 施工单第 5 条）。**P2-58 卡面只有四步
（建骨架 / 移代码 / 加依赖 / 修 import），没有这一步**；照卡面做完，`business/hr` 与 `business/erp`
的 39 个文件在测试配置下全线 `unresolved import`。这是本卡作废的第四条理由，也是最硬的一条：
卡面的验收命令在卡面步骤下**不可达**。

### 3.2 本域没有 `crate::chrono` / `crate::run` 这类编译级改造面

`shared` / `utils` / `business/shared` 三个目录里 `#[nik::route]` / `#[nik::post]` / … / `inventory::submit`
**一处都没有**（实跑 grep 为空）；3 处 `#[nik::permission]` 全在**注释**里
（`permission_alias.rs:30`、`batch.rs:33`、`batch.rs:68`）。⇒

- 抽包时「路由条数必须『拆前 − 0』」这条门禁对 C1 **天然成立**（本来就是 0）；
- `ROUTE_CONTRACT` / `permission_audit_tests` 的 N14/N18 / `perm_matrix_tests` **不受影响**；
- 也**不会有**「域包漏进 bin 的 `[dependencies]` ⇒ 路由静默 404」这个坑（C1 是唯一零路由的域，
  这一点提醒后面 14 张卡不要照抄 C1 的验收口径）。

## 4. 入边：C1 整体 **1 254 行代码**（prod 928 / test 326）/ 26 个节点

口径：只数**代码行**（`//` / `///` / `//!` 开头的整行剔除），只数目录**外**的文件，
`#[cfg(test)]` 之后或文件名含 `test`/`tests` 的算 test。

| 节点 | 行数 | 其中 shared / business-shared / utils |
|:--|--:|:--|
| `business/erp` | 604 | 230 / 250 / 124 |
| `business/hr` | 180 | 73 / 82 / 25 |
| `system` | 95 | 56 / 25 / 14 |
| `business/task` | 75 | 23 / 33 / 19 |
| `business/flow` | 63 | 31 / 27 / 5 |
| `support` | 51 | 27 / 10 / 14 |
| `business/impexp` | 24 | 8 / 8 / 8 |
| `business/storage` / `business/attachment` | 17 / 17 | 9 / 5 / 3 · 12 / 1 / 4 |
| `business/notice` / `business/permission` / `business/masking` / `business/changelog` | 16 / 15 / 13 / 11 | — |
| `app#root`（`bootstrap.rs`/`lib.rs`/`main.rs`） | 9 | 6 / 3 / 0 |
| 其余 12 个节点 | 64 | aiagent 6、help 9、announcement 8、mutex 7、middleware 7、task 6、project 5、docpdf 4、monitor 4、taskflow 4、dashboard 2、pdf 2 |
| **合计** | **1 254** | 工具三分量 552 + 244 + 480 = 1 276，减 §2.3 的 22 行内部边 |

### 4.1 这笔账谁来付：不需要「逐处改」

C1 落地时这 1 254 行是机械替换（`crate::shared::x` / `crate::utils::y` / `crate::business::shared::z`
→ `cloudpivot_kernel::…`），**但没有一张下游卡需要提前付**：它们全是「域 → 内核」的合法向下箭头。
反过来说，本卡单独抽 `shared` 时这 1 254 行里的 552 行会变成「域 → `cloudpivot_shared`」，
而 `cloudpivot_shared` 又要依赖单体（因为 `utils` / `business/shared` 还在单体里，§2.3 的 22 行）
⇒ 假拆分（`P2-C.5` 明确拒绝的那条路）。

### 4.2 `error_code.rs` 是这根枢纽里最硬的一根（本卡新发现 ②）

`shared/error_code.rs` 单文件 **2 505 行 / 318 个 `pub const` / 326 个去重 5 位码（10 001 ~ 68 999）**，
全仓 **425 行**引用它（`grep -rn 'shared::error_code'`，排除自身）。

这就是 C1「必须先落地」的量化理由：每一个域的错误出口都经过这张表。`error_code` 留在单体一天，
任何域抽包都要么反向依赖单体、要么再复制一份错误码表（后者是灾难）。本卡只报不动。

## 5. 机械量（与 C1 行 `32 / 21 826 · 0 · 0 · 2 · 2` 对账）

### 5.1 规模：32 个文件 / 21 826 行（逐文件）

`src/shared/` —— 13 文件 / 7 119 行

| 文件 | 行 | | 文件 | 行 |
|:--|--:|--|:--|--:|
| `error_code.rs` | 2 505 | | `token_revocation.rs` | 260 |
| `startup_guard.rs` | 1 333 | | `session_cache.rs` | 256 |
| `test_helpers.rs` | 567 | | `enums.rs` | 226 |
| `observability.rs` | 551 | | `permission_alias.rs` | 206 |
| `constants.rs` | 523 | | `ai_actor.rs` | 180 |
| `idle_session.rs` | 347 | | `cache_gate.rs` | 133 |
| `mod.rs` | 32 | | **小计** | **7 119** |

`src/utils/` —— 1 文件 / 323 行（`mod.rs`：无状态纯函数，零副作用；唯一出边是指向 `shared` 的 3 行）

`src/business/shared/` —— 18 文件 / 14 384 行

| 文件 | 行 | | 文件 | 行 |
|:--|--:|--|:--|--:|
| `state_machine.rs` | 6 281 | | `batch.rs` | 368 |
| `scope.rs` | 1 371 | | `user_name.rs` | 332 |
| `content.rs` | 1 180 | | `period_key.rs` | 179 |
| `principal.rs` | 1 128 | | `task_node_switch.rs` | 165 |
| `separation.rs` | 995 | | `doc_lines.rs` | 162 |
| `voucher_port.rs` | 530 | | `layering_tests.rs` | 141 |
| `guard.rs` | 485 | | `mod.rs` | 84 |
| `cost_switch.rs` | 484 | | `doc_kind_entity_tests.rs` | 53 |
| `seq.rs` | 409 | | `conn.rs` | 37 |
| | | | **小计** | **14 384** |

**总计 32 / 21 826** —— 与 C1 行逐字相等（13+1+18=32；7 119+323+14 384=21 826）。

### 5.2 其余五格对账

| C1 行字段 | 计划值 | 本卡实测 | 判 |
|:--|:--:|:--|:--:|
| 路由 | 0 | `#[nik::route]`/`#[nik::get]`/`inventory::submit` = 0（3 处 `#[nik::permission]` 全在注释） | §3.2 ✅ |
| 真库用例文件 | 0 | 目录内无 `*_db_tests.rs`；236 用例真库/假库同绿（§7.2） | ✅ |
| 出目录引用 | 2 | `business/shared/cost_switch.rs:469,481` → `../../../migrations/business/V0385__cost_module_switch.{up,down}.sql` | ✅ |
| 夹具引用 | 2 | 复核为**定义本体 2 处**：`shared/test_helpers.rs:299`(`ensure_any_dept`)、`:316`(`ensure_any_user`)；目录**外**的真实引用是 132 行 / 39 文件（§3.1），靠特性化解决、不逐处改 | 口径差（§5.3） |
| 入边 | 552 / 244 / 480 | 工具复现一致；合包后 1 254 行（减 22 行内部边） | ✅ |

### 5.3 三处口径差（C1 施工单必须带）

1. **「夹具引用 2」是定义数，不是引用数**：真正要付的是 132 行 / 39 文件（hr 92、erp 40），
   但付法是「1 个 feature + 39 个文件的 `[dev-dependencies]`/cfg 面」而不是 132 处改动。
2. **`include_str!` 共 5 处，其中 3 处不受影响**：`constants.rs:501`（`"constants.rs"` 自读）、
   `voucher_port.rs:402`（同）、`user_name.rs:323`（`concat!(env!("CARGO_MANIFEST_DIR"), …)` 已锚定）
   ——只有 `cost_switch.rs:469/481` 需要重算（计划表记 2 处，判对）。
3. **`P2-42` 台账第 97 行的 P2-58 = `business/shared`**，与卡面「`shared`」不是同一个目录
   （§2.2）。C1 卡面已写明三者并包，施工时按 C1 读，不要按本卡台账读。

## 6. 搬包会失效的门禁（按源码路径硬编码，本卡只报不动）

| 门禁 | 现状（实跑） | 搬包后 |
|:--|:--|:--|
| `scripts/lint/scope_gate.py` | exit 0（`PASS：扫描范围已覆盖 backend/app/src/{business/task,business,system,support,shared}`；跳过清单 160 条逐条一致；0 处） | 扫描根要加 `backend/crates/cloudpivot-kernel/src`，否则内核从扫描面**静默消失**（与 P2-57 §6 同型风险） |
| `scripts/lint/path_refs_gate.py` | exit 0（899 脚本 / 白名单 10 条 / 0 处失效） | 内核对 `cost_switch.rs:469/481` 的迁移路径要把锚改 `CARGO_MANIFEST_DIR`，否则 2 处失效 |
| `business/shared/layering_tests.rs`（141 行） | 在应用内断言模块层次 | 搬包后它测的是「包内层次」，要改成断言 `cloudpivot_kernel` 的模块可见性，或搬去 C2 的 bin 包做跨包断言 |
| `permission_audit_tests` N14/N18、`perm_matrix_tests` | 与本域无关（本域 0 权限码） | 不变 |
| `ROUTE_CONTRACT`（编译期 50 行） | 与本域无关（本域 0 路由） | 不变 |

## 7. 验证与基线（本卡零生产代码改动，以下是「拆前是绿的」的证据）

### 7.1 构建 / 格式 / 卡序

```bash
$ cd backend && cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.12s        # exit 0

$ cd backend && cargo fmt --check                                              # exit 0（零 Rust 变更，未跑 cargo fmt）
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环                              # exit 0
$ python3 scripts/lint/scope_gate.py                                           # exit 0
$ python3 scripts/lint/path_refs_gate.py                                       # exit 0
```

### 7.2 236 个用例：真库绿 + 假库同绿（证明本域零 DB 断言入口）

演练库按卡面约束建在 **145**（99 本机 PG 已 purge）：`createdb -T cloudpivot_test cp_p258_kernel`
（182 表），跑完 `dropdb`（复核 `select count(*) … like 'cp_p258%'` = 0）。

```bash
$ ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p258_kernel" \
  cargo test -p cloudpivot --lib --offline -- --test-threads=1 \
  shared:: utils:: business::shared::
test result: ok. 236 passed; 0 failed; 0 ignored; 0 measured; 5445 filtered out; finished in 4.70s
EXIT=0

# 对照：故意指向不存在的库
$ ERP_DB_URL="postgres://nobody@127.0.0.1:1/none" cargo test -p cloudpivot --lib --offline -- \
  --test-threads=1 shared:: utils:: business::shared::
test result: ok. 236 passed; 0 failed; ...; finished in 0.47s
EXIT=0
```

236 = `shared` 73 + `utils` 0 + `business/shared` 163（`#[cfg(test)]` 块 39 个，
`#[tokio::test]` 35 条）。**两次同数同绿 ⇒ 这 236 条里没有一条断言库内容**，与
C1 行「真库用例 0」一致；本卡建的那个库只是为了「万一有"顺手连库"的用例，不能靠假绿糊过去」，
跑完即 DROP（复核：145 上 `datname like 'cp_p258%'` 计数 **0**）。

## 8. 下一轮怎么开（C1 施工单，按依赖序）

1. **先付 10 行解环账**（按 §3 的归属列）：`config_port`（4 行合并成 1 个）→ `session_flags_port`
   （1 行）→ 主密钥解析上提（1 行）→ aiagent 启动检查下移（2 行）→ 两条**跨轴一致性用例搬回 `app`**
   （2 行，`observability.rs:427` 与 `permission_alias.rs:111` 所在的 `#[test]` 函数整体搬）。
   切完 `--plan-order` 必须仍绿（C1 从 10 变 0 是不允许的——卡序文档冻的是**开工时**的值）。
2. **`test_helpers` 特性化**：`#[cfg(any(test, feature = "test-support"))]`；`hr`（92 行）与
   `erp`（40 行）在 `[dev-dependencies]` 打开 feature —— **这一步必须在搬包前做**，否则搬完
   `cargo test --workspace` 直接红一片（§3.1）。
3. **建包搬文件**：`git mv app/src/shared app/src/utils app/src/business/shared` →
   `crates/cloudpivot-kernel/src/{shared,utils,business_shared}/`（包内 `crate::` 不变，
   三目录合包后的模块层级命名是 C1 要拍的口径——`business_shared` 还是保留 `business::shared`
   两级，取决于 C1 卡面；本卡不改）。
4. **改 1 254 行 import**（§4 的 26 个节点）→ `cloudpivot_kernel::…`；`app/Cargo.toml` 加 1 行依赖。
5. **重算 2 处迁移路径**（`cost_switch.rs:469/481` → `concat!(env!("CARGO_MANIFEST_DIR"), …)`），
   走 §P2-C.4 第 4 条的锚定式。
6. **门禁跟改**（§6）：`scope_gate.py` 扫描根、`layering_tests.rs` 的断言口径。
7. **验收**：`cargo build --workspace` + `cargo test -p cloudpivot-kernel`（236 条迁过去的用例）
   + `cargo test --workspace` + `--plan-order` 绿。

**不要做的事**（本卡留给 C1 的红线）：

- 不要建 `cloudpivot-shared` 这个名字（§1.1，会让 1 254 行 import 改两遍）；
- 不要把 `business/shared` 或 `utils` 留到后面「顺手」搬（§2.3 的 22 行内部边 ⇒ 包级环，`cargo check` 直接红）；
- 不要把 `#[cfg(test)]` 的 `test_helpers` 原样搬进新包然后在应用里「先写个转发垫片」
  （垫片在依赖配置下同样看不到 `cfg(test)` 的符号，等于没修）。

## 附录 A：核对清单（本卡实测 vs 计划表）

| 项 | 计划（C1 行 / P2-42 §4） | 本卡实测 | 判 |
|:--|:--|:--|:--:|
| 包名 | `cloudpivot-kernel` | 全仓无 `cloudpivot-shared`/`cloudpivot-kernel` 落地 | ✅ 一致 |
| 节点 | `shared` / `utils` / `business/shared` | 13 + 1 + 18 文件 | ✅ |
| 规模 | 32 / 21 826 | 32 / 21 826 | ✅ 逐字相等 |
| 入边 | 552 / 244 / 480 | 552 / 244 / 480（减 22 内部边 = 1 254） | ✅ |
| 本步要切 | 10 行：support 3、system 2、aiagent 2、task 1、erp 1、storage 1 | 逐行核到 file:line（§3，prod 8 / test 2） | ✅ |
| 前置 | 无（只依赖 L0 `cloudpivot-doc`） | `cargo tree -p cloudpivot-doc` 单节点；本域 2 处代码 + 2 处注释引 `cloudpivot_doc` | ✅ |
| SCC | 24 | 24（含 `shared`/`utils`/`business/shared` 与 21 个业务节点） | ✅ |
| `--layer-check` | 退 1（`P2-42` §4 第 97 行） | `shared` 退 1、`business/shared` 退 1、`utils` 退 0 | ✅ |
| 卡面验收 | — | `cargo build --workspace` 绿；`cargo test -p cloudpivot-shared` 101（包不存在，符合作废预期） | ✅ |

## 附录 B：本卡未做的事（明确边界）

- 未建 `backend/crates/cloudpivot-shared/`、未建 `backend/crates/cloudpivot-kernel/`；
- 未 `git mv` 任何文件、未改任何 `.rs`、未动 `Cargo.toml`（`git status` 只有本 `.md`）；
- 未跑 `cargo fmt`（零 Rust 变更、多 worker 共享工作区，只在 §7.1 跑 `--check` 证明基线是干净的）；
- 未修 §6 列出的门禁（那是 C1 的活）；
- 演练库 `cp_p258_kernel` 已 DROP，145 上无 `cp_p258%` 残留。
