# 20261104 · `set_db` 先到先得：`internal_control::ensure_db` 静默复用别人的库（已收口）

> 卡：`t_f38d1f45`（【P2·测试卫生】`internal_control::ensure_db` 在全局 `db()` 已被别人初始化时会静默复用别人的库，`P0FIX_DB_URL` 形同虚设）
> 上游：`t_e2a368f0` / [`20261104-test-hygiene/`](../20261104-test-hygiene/README.md)（同库串跑 60+ 条假红 → 分库跑法）
> 代码：`backend/src/business/flow/internal_control.rs`（+ 跑法脚本 `scripts/verify/run_lib_tests_by_module.sh`）
> 文档：[`docs/测试卫生-真库单测分库跑法.md`](../../测试卫生-真库单测分库跑法.md) §3.1、
> [`docs/测试卫生-真库连库入口清单.md`](../../测试卫生-真库连库入口清单.md)

## 结论（一句话）

`ensure_db()` 过去在 `try_db().is_some()` 时直接 `return true` —— 同进程里更早的用例用 `ERP_DB_URL` 装好连接后，
本组 7 条真库用例就**静默跑在业务库上**，`P0FIX_DB_URL` 与「库名必须含 p0fix」的硬断言**一次都没执行**，
而 `test result` 是**绿的**。现在改成：先 `SELECT current_database()`，与 `P0FIX_DB_URL` 的库名比对，
不一致就 **panic + 三条可执行的下一步**；一致才复用。选 `panic` 而不是 `WARN + SKIP` 的理由见文档 §3.1
（SKIP 之后 `test result` 依旧 `ok`，正是这一串卡要灭掉的假绿；而冲突只发生在操作者**显式设了** `P0FIX_DB_URL` 时）。

## 文件

| 文件 | 内容 | 关键结论 |
|---|---|---|
| `01_before_same_process_flow_segment.txt` | **修复前**：一条命令 `cargo test --lib 'business::flow::'`（`ERP_DB_URL`=A、`P0FIX_DB_URL`=B，同一个进程） | `ok. 345 passed; 0 failed` **全绿**（假绿）——其中 7 条 `p0fix_*` 定义写进了 **A（业务库）** |
| `02_after_same_process_flow_segment.txt` | **修复后**：同一条命令、同一份干净库 | `FAILED. 338 passed; 7 failed`；每条失败都是「当前连的是 A、要求的是 B」+ 三条下一步 |
| `03_after_flow_with_skip.txt` | 修复后：整段 flow + `--skip business::flow::internal_control::db_tests` | `ok. 338 passed` —— 其余 338 条照旧全绿（345 = 338 + 7，一条没少） |
| `04_after_solo_internal_control.txt` | 修复后：本组独占进程（正路） | `ok. 14 passed`（7 纯逻辑 + 7 真库），7 条定义落在 **B（P0FIX 库）** |
| `05_after_legit_reuse_same_db.txt` | 修复后：三个变量指向**同一个** p0fix 库（合法复用） | `ok. 345 passed` + 打印「复用同进程已初始化的全局 db()（库：…，与 P0FIX_DB_URL 一致）」——守卫不误伤合法复用 |
| `06_script_flow_p0fix_runs2.txt` | `MODULES='flow_p0fix' RUNS=2 bash scripts/verify/run_lib_tests_by_module.sh` | 7/0 ×2 全绿；**泄漏校验仍然在**：P0FIX 库 `p0fix_*`=7、模块库=0 |
| `07_script_flow_runs1.txt` | `MODULES='flow' …` | 338/0 全绿（脚本已把那 7 条 `--skip` 掉，改由 `flow_p0fix` 跑） |
| `08_db_row_counts_before_after.txt` | 每次跑完之后**立刻**查两库行数（psql 原文） | 修复前 A 库 `p0fix_defs=7` → 修复后 `0`；B 库在「没连上」的情况下保持 `0`（拒绝假装通过） |
| `09_script_all_modules_runs1.txt` | 4 模块全量回归（changelog / flow_p0fix / flow / erp） | 51/0、7/0、338/0、854/0，`EXIT=0` |
| `10_inventory_scan.txt` | 全仓连库入口扫描原文（`env::var("…_DB_URL")` / `try_db()` / 库身份断言） | 43 处入口全部是「`try_db().is_some()` 就复用」，其中**只有 1 处**对库的身份有断言（本卡这一处） |
| `11_after_p0fix_nonexistent_db.txt` | 修复后：`P0FIX_DB_URL` 指向**根本不存在的库**（上游卡 62 的「更硬反例」复跑） | 7 条全部显式失败（修复前是 345/0 全绿）；证明「没去连」这件事再也不会被当成通过 |
| `raw/` | 脚本每轮 `cargo test` 原始输出（`<模块>_run<轮>.txt`） | — |

## 「同一个进程里先跑 ERP 用例、再跑本组」是怎么证明的

`01` / `02` 是**同一个进程**的两份日志（同一条命令、`--test-threads=1`），执行顺序可直接从行号读出：

| 日志 | 先跑的（用 `ERP_DB_URL`） | 后跑的（用 `P0FIX_DB_URL`） | 结果 |
|---|---|---|---|
| `01`（修复前） | `:57` `edge_admin_tests::bad_patch_is_rejected_before_any_approval … ok`、`:58` `…::biz_id_is_stable_per_edge … ok` | `:187` / `:188` `internal_control::db_tests::…` | 全是 `ok`；而 `p0fix_*` 定义落在 **ERP 库**（见 `08`） |
| `02`（修复后） | `:57` / `:58` 同上（照旧 `ok`，它们本来就该跑在 ERP 库） | `:187` / `:188` 同两条 | **`FAILED`**，panic 点名「当前连的是 A、要求的是 B」 |

即：`edge_admin_tests` 先 `set_db(ERP 库)` → 同一个进程里 `internal_control::db_tests` 随后 `ensure_db()`。
修复前它`return true` 静默复用，修复后它比对库名并**显式失败**。


## 环境（可复现）

- 库：本机 PostgreSQL 18.6（`alun@127.0.0.1:5432`）。模板库 `cloudpivot_libtest_template`
  = 仓库 HEAD 的迁移 + 种子（`public` 102 张表、`biz_flow_def` 11 行、`sys_user` 1 行）。
  本卡所有库都是**一次性副本**（`hyg_f38d_erp` / `hyg_f38d_p0fix` / `cp_f38d_*`），跑一次建一次，
  建库走 `su - postgres`（`alun` 无 CREATEDB 权限）；**生产库（144）全程未写**。
- 代码：`/opt/cloudpivot`，`HEAD=5dd36c0`。⚠ 该工作树里另有**另一张卡（V0213 取消级联）未提交**的代码，
  本卡只改了 2 个文件（`internal_control.rs`、`run_lib_tests_by_module.sh`）+ 文档/证据；
  因此 `erp` 模块的计数里包含那张卡的用例（854 条，上游卡当时是 840 条）。本卡未触碰它们。
- 复跑（本卡最省事的两条）：
  ```bash
  # ① 同进程冲突：必须显式失败（修复后）
  ERP_DB_URL=…/A TASK_DB_URL=…/A P0FIX_DB_URL=…/B \
    cargo test --lib 'business::flow::' -- --test-threads=1        # 期望 338 passed / 7 failed
  # ② 正路：本组独占进程 + 守门脚本
  MODULES='flow_p0fix' RUNS=2 bash scripts/verify/run_lib_tests_by_module.sh   # 期望 7/0 ×2 + 泄漏校验通过
  ```

## 备注

- 本卡**没有**改任何业务逻辑（业务代码 / SQL / 接口签名 0 改动）；改动只在 `#[cfg(test)]` 的 `db_tests`
  与跑法脚本 / 文档里。
- 上游卡留下的 `flow_p0fix` 泄漏校验（P0FIX 库 ≥1 条 `p0fix_*`、模块库 0 条）**原样保留**，
  见 `06_script_flow_p0fix_runs2.txt` 里的「泄漏校验：P0FIX 库 p0fix 夹具=7，模块库=0」。
