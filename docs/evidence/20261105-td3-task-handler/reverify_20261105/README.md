# P2-1A 复核（2026-11-05 19:2x，HEAD = 64af38f）

本目录是 `t_af67b114` 在**当前 HEAD** 上对分隔结果的再次独立复跑，用于回应「拆分是几小时前
做的、期间 main 前进了 20+ 个提交」的时效性问题。拆分本体提交仍是 `bae90c0`（纯 refactor，
29 files），本目录不含任何代码改动。

## 复跑时的仓库状态

- HEAD = `64af38f`（`bae90c0` 之后 main 又进了 TEST-ISO-2、导出 status_code、impexp 证据等提交）
- `backend/src/business/task/handler.rs` = 127 行 facade，15 个部件最大 932 行（`line_counts.txt`）

## 四项复核结果

| 验收项 | 本次实测 | 证据 |
|---|---|---|
| ≥4 文件、每个 <1500 行 | 15 个文件，最大 `task_handler_crud.rs` 932 行 | `line_counts.txt` |
| 只搬不移（行为零变化） | `verify_move.py bae90c0^` **PASS**：条目 137=137 / SQL 24=24 / 用例名 24=24 / 代码 token 28013=28013，MISSING=ADDED=none | `verify_move.txt` |
| 任务面全量测试（连库，两轮） | round1 **226 passed / 0 failed**；round2 **226 passed / 0 failed**；与拆分前 226 条用例名逐条 `diff` 为空 | `task_module_round1.txt` / `task_module_round2.txt` / `test_results.txt` / `test_names_diff.txt` |
| 无新增 dead_code/编译警告 | `cargo check --lib` EXIT 0，warning 行 **0**；`cargo clippy --lib --tests -- -D warnings` EXIT 0 | `cargo_check.txt` / `cargo_clippy.txt` / `check_warning_count.txt` |

复跑命令：

```bash
python3 scripts/refactor/verify_move.py bae90c0^ business/task/handler.rs
cd backend && cargo check --lib
cd backend && cargo clippy --lib --tests -- -D warnings
TASK_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_libtest_task' \
  cargo test --lib 'business::task::' -- --test-threads=1   # ×2 轮
```

`business::task::` 面 2405 条其它模块用例被 filter 掉（卡面要求「不跑无关的全量回归」）。
