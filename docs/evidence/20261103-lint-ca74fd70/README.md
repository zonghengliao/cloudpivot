# t_ca74fd70 · check.sh 门禁修复证据（internal_control erasing_op）

提交：`55a4f04` — `chore(flow): 修 internal_control 测试夹具的 erasing_op，恢复 check.sh 门禁`
分支：`main` == `origin/main`(144 bare) == `github/main`（三方 ls-remote 实测一致）

## 1. 根因

`backend/src/business/flow/internal_control.rs` 的 `db_tests::skipped_task_is_not_in_my_todo`
把 `seed` 硬编码成 `0`，三处夹具写成 `uid(0 * 100 + n)` —— `0 * 100` 恒为 0，触发 clippy
默认 `deny` 的 `erasing_op`（外加 `identity_op` warning），于是
`cargo clippy --all-targets -- -D warnings` **在 main 上编译即失败**，`scripts/check.sh`
的 clippy 关卡等于常年失效。

修法：`let seed = 7u128;`（同文件已用 1–6，7 未被占用），`uid(seed * 100 + n)` 与
`Scenario::start(tag, seed, …)` 共用同一个 seed —— 保持「每个用例独占一个 uid 命名空间，
重跑时先删后建」的原意，而不是把 `0 * 100 + 5` 直接压成 `uid(5)`（那样会让 seed 与夹具脱钩，
将来改 seed 就会撞号）。

## 2. 同批清掉的 warning（整仓 `-D warnings` 要求为 0）

| 文件 | 规则 | 处数 |
|---|---|---|
| `src/business/flow/internal_control.rs` | `useless_vec`（users 改数组）、`let_and_return`（build 直接返回 Self） | 1 + 1 |
| `src/business/erp/reminder.rs` | `field_reassign_with_default`（ScanReport 结构体字面量 + `..Default::default()`） | 1 |
| `src/business/erp/stockout_tests.rs` | `let_and_return`（`location_ledger` / `make_doc` 直接返回表达式） | 2 |
| `tests/testd/{linkage,mutex_help_ai,project_log}.rs` | `borrow_deref_ref`（`&*db()` → `db()`） | 7 |
| `tests/testd/project_log.rs` | `manual_contains`（`contains(&expected)`） | 1 |
| `tests/testd/impexp.rs` | `let_and_return` 1 处、`useless_vec` 7 处（`vec![]` → `[]`） | 8 |
| `tests/testd/{announcement,common,impexp,linkage,project_log}.rs` | `cargo fmt` 漂移（HEAD 上本就 fmt 不干净）落地 | 仅格式 |

业务逻辑**一行未动**：`internal_control.rs` 只改测试夹具的 uid 写法 / `vec!` 字面量 /
`build` 的返回写法；未使用 `#[allow(clippy::erasing_op)]`。

## 3. 实跑结果（全部本机真跑，见同目录 raw 日志）

| # | 命令 | 结果 | 日志 |
|---|---|---|---|
| 1 | `cargo fmt -- --check` | exit 0 | `01_fmt_clippy.log` |
| 2 | `cargo clippy --all-targets -- -D warnings` | **exit 0（整仓，含 testd 集成测试 target）** | `01_fmt_clippy.log` |
| 3 | `cargo test --lib business::flow::internal_control`（`P0FIX_DB_URL` → `cloudpivot_test`） | **14 passed / 0 failed**，9.57s（真库，非 SKIP） | `02_internal_control_tests.log` |
| 4 | `cargo test --lib business::erp::stockout_model::tests`（`ERP_DB_URL` → `cp_dc13_t1`） | **7 passed / 0 failed**，10.55s（真库） | `03_stockout_reminder_tests.log` |
| 5 | `cargo test --lib business::erp::reminder` | 33 passed / 0 failed | `03_stockout_reminder_tests.log` |
| 6 | `bash scripts/check.sh` | fmt / clippy / cargo check / pnpm typecheck / 闸① / C7 G1–G4·G6 全绿；**批次B 闸②、C7 G5 红** | `04_check_sh.log` |
| 7 | `BATCHB_GATE=warn bash scripts/check.sh` | 同上，闸② 降级为告警后其余全绿（能跑到 C7 最后一条） | `05_check_sh_batchb_warn.log` |
| 8 | HEAD(26a1111) 干净 worktree vs 提交后树：闸② / C7 G5 失败清单 diff | **(IDENTICAL)** | `06_preexisting_gate_diff.txt` |

### 关于 6/7 里的两条红：存量债，不是本卡引入

`scripts/check.sh` 自己在文件头就写明：*「批次B 门禁是**故意**会在当前树上报错的 ——
它度量的是『还没接入数据权限的存量查询』」*。实测两条：

- 批次B 闸②：**35 个文件**（`sys_user` / `biz_flow_*` / `biz_task*` 等登记表未接 `scope::`
  且无 `SCOPE-EXEMPT`）；
- C7 G5：**37 个 `record/record_in` 调用点**不在 `transaction` 闭包内或没传 `&tx`。

把 `check.sh` 里的这两段门禁函数原样抽出来，分别对 **HEAD(26a1111) 的干净 worktree** 和
**本提交后的树** 各跑一遍：失败清单逐行一致（`diff` 为空），闸② 都是 35、C7 G5 都是 37。
⇒ 这两条红在本卡之前就存在，本卡既没有引入也没有扩大；要清零得按架构口径逐文件补
`SCOPE-EXEMPT` 理由 / 改事务边界，属于独立卡片（已在本卡完成备注里提出）。

## 4. 未做 / 边界

- 未改 `internal_control` 的业务逻辑（只动 `db_tests` 夹具）—— 按卡面「不要做」第 1 条。
- 未用 `#[allow(clippy::erasing_op)]` 掩盖 —— 按卡面「不要做」第 2 条。
- 未上线生产（144 仍是 `e35a7c0` 之后的部署版本，本提交只进 main / origin / github）。
- 工作区里 `backend/src/test_db_guard.rs`（未跟踪）与 `frontend/**` 的改动属于其它在跑卡片
  （t_0d7c51f8 / NOTIFY-POP），本提交按路径 `git add`，没有卷进来。
