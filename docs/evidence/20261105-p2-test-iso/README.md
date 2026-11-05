# P2-TEST-ISO：`system::menu` 并行跑用例互相污染（n14 偶发红）—— 收口证据

卡：`t_7f775c91`（test-enginner）　基线：`HEAD 0394c6f`（main）　日期：2026-11-05

## 1. 结论（一句话）

`system::menu::permission_audit_tests` 的**全表审计**断言（N14 / N15 / N18）没有界定判据范围，
把同一进程里**并行兄弟用例正在插的临时菜单行**当成「库里有、源码里 0 落地」的死码。
修法是把审计范围钉成「**本进程开跑之前就存在的种子行**」：`created_at < run_epoch`
（`run_epoch` = 首次连库时取的**库时钟** `now()`）+ 既有夹具前缀白名单 `name !~ FIXTURE_PREFIX_REGEX`。
判据强度不变（对每一条种子权限码仍是全量断言），被排除的行数打在用例输出里，可核对。

- 修前：`cargo test --lib system::menu::` 默认并行 **5 轮 4 轮红**（N14 报 `N13-T<ts>:btn`）；
  把 `auth::cache_tests` / `session_tests` 一起并行跑，**N15 也被 `testc3:tc_c3_c06_<ts>:perm` 打红**。
- 修后：默认并行 **连跑 5 轮，N14 / N15 / N18 全绿**（模块内唯一红是 `n09`，见 §8，确定性数据缺陷，与并发无关）；
  跳过 n09 后 **5 轮 19/19 全绿**；串行 1 轮同口径；跨模块混跑 3 轮 N14/N15 也全绿。

## 2. 根因（代码行号，基于 HEAD 0394c6f）

| 位置 | 内容 |
|---|---|
| `backend/src/system/menu/permission_audit_tests.rs:393-411`（修前） | `menu_auth_codes()` 的 SQL：`SELECT auth_code, name, menu_type FROM sys_menu WHERE deleted_at IS NULL AND auth_code IS NOT NULL AND auth_code <> ''` —— **只按「有码 + 未软删」筛，没有 run 边界**，库表全量 |
| 同上 `:449-465`（修前） | N14 逐条判 `repo.code_refs(code).is_empty()` ⇒ 并行时兄弟用例的临时行必然「源码里 0 落地」⇒ 断言 `dead.is_empty()` 失败 |
| 同上 `:490-506`（修前） | N15 同一份 SQL 的结果做形态校验 ⇒ `N13-T<ts>:btn`（只有 2 段）、`testc3:<tag>:perm` 都会红 |
| `backend/src/system/menu/tests.rs:961-969` | 夹具型用例 N13 用真实 handler 建菜单 `auth_code = "{prefix}:btn"`，`prefix = "N13-{ts}"` |
| `backend/src/system/menu/tests.rs:1134-1142` | 夹具型用例 N14 同上（`N14-{ts}:btn`） |
| `backend/src/system/menu/tests.rs:143-170` | N01 等用例一次建 4 条（`N01-<ts>:a:list` / `:a:create` / `:b:list`） |
| `backend/src/system/auth/cache_tests.rs:211,299` | **跨模块**夹具：`INSERT INTO sys_menu … auth_code='testc3:<tag>:menu|perm'` |
| `backend/src/system/session_tests.rs:110,315` | **跨模块**夹具：`FIX05S0x_<ts>:btn` |
| `backend/src/business/erp/period_test_support.rs:557` | **跨模块**夹具：`business:erp:p2-…` |

实测（修前，隔离库，默认并行）：

```
round=1 rc=101 test result: FAILED. 18 passed; 2 failed; ... finished in 3.79s
  test system::menu::permission_audit_tests::n14_menu_auth_codes_have_source_action_point ... FAILED
  panicked at src/system/menu/permission_audit_tests.rs:460:
  以下菜单权限码在后端源码里 0 落地（勾给角色永不生效，BUG-SYS-05）：
  N13-T1793876996873009:btn
round=2..4 同上；round=5 唯一那 1 红仍是 n09
```

跨模块（修前）：`cargo test --lib -- system::menu:: system::auth::cache_tests system::session_tests`
第 1 轮额外出现

```
n15_menu_auth_codes_match_module_resource_action ... FAILED
  panicked at src/system/menu/permission_audit_tests.rs:502:
  以下 auth_code 不是 {模块}:{资源}:{动作} 形态 ...：
  testc3:tc_c3_c06_T1793877066674003:perm（菜单「tc_c3_c06_…_按钮」，menu_type=3）
```

原始日志：`before/module-parallel-round-*.txt`、`before/module-serial-round-*.txt`、`before/cross-module-round-*.txt`。

## 3. 为什么选这条修法（对比卡面给的 a / b / c）

| 方案 | 判定 | 理由 |
|---|---|---|
| a. 审计只扫「非夹具前缀」（`name NOT LIKE 'N%-T%'`） | **否决（覆盖不全，实测漏）** | 夹具命名至少有 4 套互不相同的约定：`N1x-<ts>`（menu）、`FIX05S0x_<ts>`（session_tests）、`testc3:<tag>`（auth::cache_tests）、`business:erp:p2-…`（erp/period_test_support）。按 N×-T 过滤能治 N14 的菜单内污染，但**治不了跨模块**（实测 n15 红在 `testc3:` 上）；而且以后新夹具换个名字又漏，属于「按当下命名打补丁」 |
| b. 夹具行加 `deleted_at` 标记 + 审计过滤 | **否决（语义反了）** | 夹具行必须「活着」才有意义（N02 关键词检索、N11 按角色过滤、N14 建/改/删都要求行可见）。软删它等于让夹具型用例失去被测对象 |
| c. 该模块测试串行化（serial_test / 合并进一个 test / 独立 schema） | **否决（代价与风险都大）** | ① 串行化必须给**跨模块**夹具（cache_tests / session_tests / erp period）也上同一把锁，否则审计照样被它们打红；② 这些模块各有自己的锁（`voucher_query::tests::DB_LOCK` / `period_test_support::DB_LOCK`），再加锁有死锁与顺序风险；③ `serial_test` 只在本进程内生效，同一库上另一个 worker 进程照样能打红 |
| **选用：run 起点快照口径（时间 + 既有夹具白名单）** | **采纳** | 「本次 run 期间造的行」是一个**与命名无关的统一不变量**：种子/迁移行必然早于进程启动。一行 SQL 参数即可覆盖全部 4 套命名，且不引入任何串行/锁；另外叠仓库既有的 `FIXTURE_PREFIX_REGEX`（残留自检本来就用它认夹具名）吸收**上一轮崩溃残留** |

## 4. 改动（2 个文件，+128 / −7）

### 4.1 `backend/src/system/tests_support.rs`：新增 `run_epoch`（+52 行）

- `:879 pub(crate) fn try_run_epoch()` / `:884 pub(crate) fn run_epoch()` —— 本进程「开跑时刻」，**取库时钟**（`SELECT now()`），零缓冲。
- `:896 async fn capture_run_epoch()` —— 在 `ensure_db()` 里钉住：`:71`（库已被别的模块初始化时也要钉）/ `:82`（首次建连后、把控制权交回用例**之前**钉）。
  次序是关键：任何夹具行的 `created_at` 只可能 ≥ 这个时刻。
- 与既有 `suite_start()` 的分工写进文档注释：`suite_start` 服务**残留清扫**（往前留 60s，宁可少删）；
  `run_epoch` 服务**审计判据范围**（零缓冲）。两者方向相反，不能混用。

### 4.2 `backend/src/system/menu/permission_audit_tests.rs`：审计口径收窄（+83 / −7）

- `:398 const AUDITABLE_ROWS` — 审计对象基本形态（未软删 + 有码），与排除条件拼装，避免两处 SQL 漂移。
- `:433 const SEED_AT_RUN_START = "created_at < ? AND name !~ ?"` — 判据范围，文档注释里列了 4 套夹具命名与实测红线。
- `:436 menu_auth_codes()` — N14/N15/N18 共用的数据源，改为 `WHERE … AND created_at < run_epoch AND name !~ FIXTURE_PREFIX_REGEX`。
- `:463 fixture_rows_excluded()` — 被排除行数，N14 输出里打印（**排除是显式的**，不是悄悄放宽）。
- 文件头「口径」加第 4 条，点名这条口径与卡号。

## 5. 验收输出（最终代码；隔离树 `HEAD + 本卡 2 文件`，见 §8 说明）

### 5.1 并行 5 轮（全模块，默认线程数）

```
after/module-parallel-summary.txt
round=1 rc=101 test result: FAILED. 19 passed; 1 failed; ... finished in 4.xx s
  FAILED: test system::menu::tests::n09_menu_icons_exist_in_tdesign ... FAILED   ← 确定性数据缺陷，见 §8
round=2..5 同上（N14 / N15 再未出现）
```

### 5.2 并行 5 轮（`-- --skip n09_menu_icons`，验收线「全绿」）

```
round=1 rc=0 test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 2612 filtered out; finished in 4.54s
round=2 rc=0 test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 2612 filtered out; finished in 4.92s
round=3 rc=0 test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 2612 filtered out; finished in 4.04s
round=4 rc=0 test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 2612 filtered out; finished in 4.00s
round=5 rc=0 test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 2612 filtered out; finished in 4.08s
```

日志：`after/module-parallel-skipn09-round-{1..5}.txt`。

### 5.3 串行 1 轮（`--test-threads=1`）

```
round=1 rc=101 test result: FAILED. 19 passed; 1 failed; ... finished in 5.24s
  FAILED: test system::menu::tests::n09_menu_icons_exist_in_tdesign
```

即「串行也红的那条」只有 n09 —— 反过来证明 n14/n15 的红**只在并行**出现，属并发互踩。

### 5.4 跨模块混跑 3 轮（menu + `auth::cache_tests` + `session_tests`，并行）

```
round=1 rc=101 test result: FAILED. 34 passed; 1 failed; ...   （唯一红 n09）
round=2 rc=101 test result: FAILED. 34 passed; 1 failed; ...   （唯一红 n09）
round=3 rc=101 test result: FAILED. 34 passed; 1 failed; ...   （唯一红 n09）
```

修前同一命令第 1 轮红的是 n15（`testc3:` 夹具），见 §2。日志：`after/cross-module-round-*.txt`。

### 5.5 审计口径原样打印（`--nocapture`）

```
[N15] 315 条种子菜单行权限码形态合规 ✓（判据范围 = run 起点前的种子行）
[N18] 例外台账 1 条、白名单 16 条，全部有理由且仍然成立 ✓
[N19] 35 处 ROLE 指派的审批节点，全部持有 business:flow:approve ✓
[N16] 502 条路由：486 条有权限注解 + 16 条白名单例外 ✓
[N14] 285 个种子菜单权限码全部有源码落地 ✓（273 条走 #[nik::permission] 注解闸，11 条走行级校验；
      判据范围 = run 起点前的种子行，已排除本次/历史夹具 0 行）
test result: ok. 6 passed; 0 failed
```

安静环境里 **0 行被排除**、审计 315 条种子行 / 285 个码 —— 与修前的审计对象**完全一致**，说明收窄只作用于「本次 run 造的行」。

## 6. 判据强度自证（防止「过滤 → 永远绿」）

在探针库（隔离库的一次性副本）上做三组：

| 实验 | 造的行 | 期望 | 实测 |
|---|---|---|---|
| ① 种子型死码 | `PROBE-死码 / probe:dead:code`，`created_at = now() - 1 day`（看着像种子） | 必红 | **RED**：`以下菜单权限码…0 落地：probe:dead:code`（`probe/seed-like-deadcode-RED.txt`） |
| ② 历史夹具残留 | `N99-T-stale:btn`，`created_at = now() - 1 day`（夹具命名） | 被排除、绿 | **GREEN**，输出「已排除本次/历史夹具 **1** 行」（`probe/fixture-prefix-row-EXCLUDED.txt`） |
| ③ 真并发行 | 测试进程起来后（run_epoch 之后）每 100ms 插一条 `live:probe:code`，共 20 条 | 被排除、绿 | **GREEN**，输出「已排除本次/历史夹具 **16** 行」（`probe/live-fixture-EXCLUDED.txt`） |

实验 ③ 就是本卡缺陷的原始形态（别人正在插行）；实验 ① 证明**没有**把「种子型死码」一并放过。

## 7. 同类隐患调查（卡面第 4 项）

`system/menu/permission_audit_tests.rs` 内逐个看：

| 用例 | 是否全表 | 结论 |
|---|---|---|
| N14 死码 | 是 | **已修**（本次） |
| N15 形态 | 是（同一 SQL） | **已修**（同一 SQL 共用 → 自动生效；修前实测会红在 `testc3:`） |
| N18 例外台账 | 是（同一 SQL） | **已修**（共用 `menu_auth_codes()`） |
| N19 审批节点角色 | 否（按 `assignee_type='ROLE'` + 当前 PUBLISHED 版本 join） | 不受菜单夹具影响；但**另一类**跨模块隐患：`biz_flow_def_version.is_current` 会被 flow 模块用例并发翻转 —— 属 skill §11 已登记的「共享流程行要共用一把锁」，本卡不动，留作 finding |
| N16 路由白名单 / N17 扫描器自检 | 不查库 | 无隐患 |

`menu::tests` 内其它全表读：

| 用例 | 过滤条件 | 结论 |
|---|---|---|
| N08 路径 | `menu_type = 2 AND path IS NOT NULL` | 夹具 `path=None` ⇒ 免疫（**但若将来夹具带 path 就会红**，建议后续一律用 §4 的 `SEED_AT_RUN_START` 口径） |
| N09 图标 | `icon IS NOT NULL` | 夹具 `icon=None` ⇒ 免疫（同上） |
| N10 注解 → 菜单 | 判「注解码 ⊆ 菜单码」 | 夹具只**加**行 ⇒ 方向安全 |

跨模块「往 `sys_menu` 插临时行」的模块（本卡修复后都被 `run_epoch` 统一吸收）：
`system/auth/cache_tests.rs:211,299`、`system/session_tests.rs:110`、`business/erp/period_test_support.rs:557`。

其它「审计型 + 夹具型」混跑面：ERP 的 `cash_flow::tests::r3_real_config_covers_all_accounts`
（`biz_account` 全表）已由 `voucher_query::tests::DB_LOCK` 合锁收口（见 skill §11 与
`aux_tests.rs:20,350`、`model_tests.rs:180,890`、`pflow3b_tests.rs:58`），形态与本卡一致，只是它们选了「合锁」，
本卡选了「划定范围」——后者不引入串行代价。

## 8. 边界与免责（重要）

1. **n09 是确定性数据缺陷，不是本卡范围**：`我的报销 → invoice` 不在 TDesign 图标集（`V0242__erp_expense_tables.up.sql:263`），
   串行/单跑同样红，属 V0242 种子数据问题。已另开卡（backend-developer）修图标名；本卡验收用
   `--skip n09_menu_icons` 的 5 轮全绿 + 全模块 19/1 说明。
2. **验证跑在隔离树**：`/opt/cloudpivot` 在验证期间被别的卡改到编译不过（`business/flow/flow_model_*.rs` 拆分重构中途，
   `cargo test --lib --no-run` 报 92 个错误）。按 `skill/SKILL.md §8`：`git archive HEAD` + 只拷本卡 2 个文件 + 私有
   `CARGO_TARGET_DIR=/root/menu-iso-target` 编译（`scripts/make_tree.sh` / `launch_build.sh`），并在树里
   `ln -s` 前端 `node_modules`，确保 n08/n09 是**真跑**而不是 SKIP 假绿。
3. **隔离库**：克隆自 P1-FIX-3 证据库（193 条迁移 / 328 菜单行 / 315 个权限码）；库名与账号不入仓库。
4. `probe/ab-attribution-timing-caveat.txt`：把树回滚到 HEAD 再跑同一「并发行」实验时，插入时机与审计查询时机
   先后不稳（有一次 HEAD 版本也绿），该实验**不作为 A/B 依据**；A/B 以 §2 的模块级 5 轮复现为准。

## 9. 复跑方法

```bash
# 隔离库（库名/账号从环境变量给，不落仓库）
export SYS_DB_URL=postgres://<user>:<pass>@127.0.0.1:5432/<isolated_db>
export ERP_DB_URL=$SYS_DB_URL TASK_DB_URL=$SYS_DB_URL

cd backend
cargo test --lib system::menu::                                        # 默认并行：应只剩 n09（V0242 数据缺陷）
cargo test --lib system::menu:: -- --skip n09_menu_icons               # 应 19/19 全绿，连跑 5 轮
cargo test --lib system::menu:: -- --test-threads=1                    # 串行对照
cargo test --lib -- system::menu:: system::auth::cache_tests system::session_tests   # 跨模块混跑
```

`scripts/run_tree_rounds.sh` / `run_cross_module.sh` 是本次用的多轮取数脚本（已脱敏，库名/连接串走环境变量）。
