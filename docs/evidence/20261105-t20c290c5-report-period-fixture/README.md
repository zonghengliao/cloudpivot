# 卡 t_20c290c5 · report 两条真库用例的「无期间行」前提过期（V0221 自动开账后失效）

日期：2026-11-05 · 被测树：`/root/t20-wt`（`git worktree`，HEAD `2b0c690`，只改 1 个文件）
构建产物：`/root/t20-target`（独立 target，不吃共享树里别人的 WIP）
隔离库：`cp_t20c`（`scripts/verify/erp_clean_db.sh` 新建，端口 18171，迁移 max=238 / 179 行）

## 1. 结论（一句话）

产品行为正确、**测试假设过期**。按卡面 **方案 (a)** 修：把「没有期间行」的样本期从当年 12 月
迁到没有任何种子路径的 **2019-12**，并在夹具里显式 `DELETE` + 断言 0 行 + 建 `2020-01`；
`DERIVED` / `previous_period == None` 两条断言**一个字都没放宽**（只换了期间与年份）。

## 2. 复现（干净库 + HEAD 代码 = 红，且只有这两条红）

```
$ ERP_DB_URL=postgres://…/cp_t20c cargo test --lib business::erp::report -- --test-threads=1
test business::erp::report::tests::r1_real_missing_period_is_rejected_but_derivable ... FAILED
test business::erp::report::tests::r1_real_previous_period_comparison ... FAILED
thread '…r1_real_missing_period_is_rejected_but_derivable' panicked at report.rs:2340:
  assertion `left == right` failed: 缺期间行但有账 → DERIVED
thread '…r1_real_previous_period_comparison' panicked at report.rs:2408:
  assertion `left == right` failed: 2026-12 没建期间 → 不出上期对比…
test result: FAILED. 13 passed; 2 failed
```

原文：`red-before-fix.txt`。

## 3. 归因（实测，不是推断）

- 新库自举后 `biz_accounting_period` 实际内容（`cp_t20c`，迁移完 = 2026 年）：

  ```
  2026-01|OPEN  2026-02|OPEN  …  2026-11|OPEN  2026-12|OPEN     ← 12 行，全部 OPEN
  ```

  来源两条，都**只看当年**：
  1. 迁移 `V0221__erp_seed_accounting_period_year.up.sql`（`CURRENT_DATE` 幂等种当年 1–12 月）；
  2. `bootstrap::ensure_accounting_periods()` → `erp::period::ensure_current_year_periods()`，
     每次启动补当年。
- 全仓种期间行的迁移只有两处：`V0137`（只种 `2026-10`）和 `V0221`（当年）。
  ⇒ **2019 / 2020 没有任何种子路径**（`grep -rn "INTO biz_accounting_period" backend/migrations`）。
- 被调用方 `build_snapshot` 无回归：`resolve_period` 缺行 + 有凭证 ⇒ `DERIVED`；
  `previous_period_of` + `load_period` 缺行 ⇒ `previous_period = None`。

## 4. 为什么选 (a) 不选 (b)

- V0221 的产品价值（新库期间表只有 1 行 → 首笔业务即 F2 WARN）与报表口径都对，
  错的是用例的**隐式**假设：「2026-12 没建期间行」。
- 改产品去迁就测试会重新挖开 F2 的坑（那正是卡 `t_3b0f5ac3` 修掉的东西），
  且「没有期间行」这个分支是报表口径的核心，必须能继续被测到。
- 夹具显式构造前提 = 用例不再依赖「正好没人种过」，以后任何新种子都打不穿它。

## 5. 改了什么（`backend/src/business/erp/report.rs`，43 增 / 15 删）

| 位置 | 改动 |
|---|---|
| `setup_fixture()` 文档注释 | 写清 V0221 / 启动保底只看当年，当年月份**不能**再当「无期间行」样本 |
| `setup_fixture()` SQL | 新增 `DELETE FROM biz_accounting_period WHERE period_code IN ('2019-12','2020-01')`（按 `period_code` 收敛，不整库扫、不碰别的年度）；`2027-01` 的插入改为 `2020-01`（`NOT_OPENED`，自然月区间） |
| `setup_fixture()` 收尾 | 新增 fail-closed 自检：`SELECT count(*) … period_code='2019-12'` 必须 `== 0`，否则 `assert_eq!` 直接炸（不静默降级成「测一个有期间行的月份」） |
| ⑬ `r1_real_missing_period_is_rejected_but_derivable` | ② 段 `2026-12` → `2019-12`（凭证 `2019-12-05`、`period='2019-12'`、区间断言 `2019-12-01 ~ 2019-12-31`）；`period.status == PERIOD_DERIVED` 与 hints「请尽快开账」**原样保留** |
| ⑭ `r1_real_previous_period_comparison` | ② 段 `run("2027-01")` → `run("2020-01")`（本期有行、上期 `2019-12` 无行 ⇒ `previous_period == None`）；逐行 `previous_amount` 全 null 的断言**原样保留**；① 段（2026-09 → 2026-10）不动 |
| 注释 | `2026-08` 现在是被 V0221 种出的 OPEN 期间（本模块只把它当凭证日期用），去掉「故意不建」的过期说法 |

没有改：V0221、`period.rs`、`bootstrap.rs`、任何产品代码。

## 6. 验证（全部真跑，日志同目录）

| 项 | 结果 | 日志 |
|---|---|---|
| HEAD 代码 + 干净库（修复前） | 13 passed / **2 failed**（红的就是那两条） | `red-before-fix.txt` |
| `cargo test --lib business::erp::report` 第 1 轮 | **15 passed; 0 failed**（2.95s） | `green-round1-report.txt` |
| 同上 第 2 轮（同库带残留） | **15 passed; 0 failed**（2.93s） | `green-round2-report.txt` |
| `cargo test --lib business::erp::period` 第 1 轮 | **40 passed; 0 failed**（40.04s） | `green-round1-period.txt` |
| 同上 第 2 轮 | **40 passed; 0 failed**（40.13s） | `green-round2-period.txt` |
| 两条用例 `--exact --nocapture` 单跑 | 各 **1 passed**，打印 `[R1-⑬]` / `[R1-⑭]` 标记（真执行，非 SKIP 假绿） | `green-two-tests-exact.txt` |
| 假绿检查 | 四份日志 `^SKIP` 计数全部 = 0 | 同上 |
| `rustfmt +1.94.1 --edition 2024 --check` | 通过（无 diff） | — |
| `cargo clippy --lib -- -D warnings` | exit 0 | `clippy-lib-only.txt` |
| `cargo clippy --lib --tests -- -D warnings` | **被 HEAD 既有编译错误挡住**（与本卡无关，见 §8） | `clippy-lib-tests-blocked-by-head.txt` |

两条用例单跑输出（原文）：

```
test …::r1_real_missing_period_is_rejected_but_derivable ... [R1-⑬] 空月份 400；有账无期间 → DERIVED + 提示开账 ✓
test result: ok. 1 passed; 0 failed
test …::r1_real_previous_period_comparison ... [R1-⑭] 上期对比：2026-10 → 2026-09（CLOSED 也能出表）；2020-01 无上期 → null ✓
test result: ok. 1 passed; 0 failed
```

## 7. 前提是「夹具自己构造」的两个反证实验

都只改工作树里的 1 行、跑完立刻还原（最终 `git diff` 仍是 43 增 / 15 删）。

**① fail-closed 自检会响**：把那条 `DELETE` 临时换成 `SELECT 1`，并手工往库里塞一行
`2019-12`（模拟未来种子 / 别人残留）：

```
thread '…r1_real_missing_period_is_rejected_but_derivable' panicked at report.rs:2082:
assertion `left == right` failed: 2019-12 必须是「没有期间行」的样本，实际有 1 行
test result: FAILED. 0 passed; 1 failed
```

（`guard-experiment-fail.txt`）—— 说明自检不是装饰：真没有「无期间行」前提时用例会**响**，
不会静默变成「测一个有期间行的月份」。

**② 夹具能接管外界残留**：`DELETE` 还原，库里**保留**那行手工塞的 `2019-12|OPEN`：

```
跑前：2019-12|OPEN
test …::r1_real_missing_period_is_rejected_but_derivable ... [R1-⑬] … ✓   test result: ok
跑后：count(*) = 0
```

（`guard-takeover-pass.txt`）—— 用例不再依赖「库里正好没有这一行」。

## 8. 如实记录：HEAD 上与本卡无关的既有红（**未修**，另开卡）

`cargo clippy --lib --tests -- -D warnings` 在 **HEAD `2b0c690`** 就编译不过（我的工作树只改了
`report.rs` 的 `mod tests`，这四处全在 `tests/` 下、我一行没碰）：

| 文件 | 错误 |
|---|---|
| `tests/linkage/task_flow_linkage.rs:34` | `E0063` `TaskCreateReq` 缺 `doc_id` / `doc_type` |
| `tests/pflow1a.rs:1143` | 同上 |
| `tests/pflow1b.rs:1395` | 同上 |
| `tests/batchT_t1_task.rs:269,467` | `E0063` `NewTask` 缺 `doc_id` / `doc_type` |

成因：P2-9（`c9b80d5`）给 `TaskCreateReq` / `NewTask` 加了 `doc_type`/`doc_id`，这四个集成测试的
调用点没跟着补。**只是集成测试目标编译不过**，`cargo test --lib`（本卡的全部验证口径）不受影响。
按「不把别人的卡并进来」的规矩未修，已另外开卡。

## 9. 与 t_3b0f5ac3 的协调

`t_3b0f5ac3`（P0-FIX-5，V0221 + 启动保底）状态 **archived / done**（完成于 11-05 01:57），
其交付声明「实现不再改动一行」。⇒ 本卡不存在「等它再改 V0221 再收口」的排序约束，可以现在收口。
若日后 V0221 / 启动保底把种子范围扩大到历史年度，本卡的 fail-closed 自检会立刻把它打红，
而不是让用例悄悄失去意义。

## 10. 收口复验（提交后、在提交自己的树上再跑一轮）

提交 `28006a7` 之后，把工作树切到该提交本身（`git status` 脏文件数 = 0，因为期间又有别人的
`7373914` 落地，所以这一步不是走形式），同一隔离库再跑一次：

```
被测树 = 28006a7 脏文件数 = 0
cargo test --lib business::erp::report  → 15 passed; 0 failed（2.94s）
cargo test --lib business::erp::period  → 40 passed; 0 failed（39.93s）
r1_real_missing_period_is_rejected_but_derivable → [R1-⑬] … ✓ 1 passed
r1_real_previous_period_comparison               → [R1-⑭] … ✓ 1 passed
```

原文：`postcommit-28006a7-report.txt` / `postcommit-28006a7-period.txt`。

## 11. 复跑方法

```bash
# 1) 干净树 + 独立 target
git -C /opt/cloudpivot worktree add /root/t20-wt HEAD
cd /root/t20-wt/backend && CARGO_TARGET_DIR=/root/t20-target cargo build --bin cloudpivot

# 2) 干净库（新建 + 真引擎迁移）
cd /opt/cloudpivot && ERP_DB=cp_t20c RECREATE=1 REPO=/root/t20-wt \
  BIN=/root/t20-target/debug/cloudpivot PORT=18171 bash scripts/verify/erp_clean_db.sh

# 3) 两轮回归
cd /root/t20-wt/backend
export ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_t20c'
export CARGO_TARGET_DIR=/root/t20-target
for r in 1 2; do
  cargo test --lib business::erp::report -- --test-threads=1
  cargo test --lib business::erp::period -- --test-threads=1
done
```
