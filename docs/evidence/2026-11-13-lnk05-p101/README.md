# 卡 t_cc1e65a4 · P1-01【清红】lnk05 费用联动全链（6 条用例）

一句话：**红的是夹具（C），不是接线（A）也不是断言（B）** —— 六条用例共用一张
「全库唯一」的联动配置表 `biz_flow_linkage_rule`，并行跑时互相按前缀清场；
给它们加一把模块级互斥锁后，**默认并行档也 6 passed / 0 failed**。

本目录是本轮（run 1593）的原始输出。代码侧锁已由 `79425867` 落地（见 §4），
本轮补的是**「main 可编译」的修复 + 实测口径校正 + 这 5 份原始日志**。

## 1. 根因定性（先定性再动手）

| 假设 | 判定 | 依据 |
|---|---|---|
| A 联动接线缺失 | **排除** | `expense_linkage::run_in_tx` / `run_deferred_after_commit` 存在，且被 5 个触发点调用（`report_submit` / `execute_approval` / `report_pay`（仅 PAID）/ `report_cancel` / `revert_to_draft`）；同库串行跑 6/6 绿（`02_before_serial.txt`） |
| B 断言过期 | **排除** | 断言口径 = 代码口径：通知 1 条（幂等键 `LNK:EXPENSE:<状态>:<id>`）、计提凭证恰 1 张（`SOURCE_REPORT`）、付款按笔数、作废红冲 1 张。本卡**一行断言都没改** |
| C 用例间全局夹具互踩 | **确诊** | `biz_flow_linkage_rule` 唯一键 `(trigger_entity, trigger_status, action_code)` 全库唯一；六条用例开头 `clear_rules()` 是 `DELETE … WHERE name LIKE 'LNK05%'`、`add_rule()` 是 `ON CONFLICT … DO UPDATE` 抢同一行 ⇒ 并行时 A 插好规则、B 把它删掉，A 推进状态时库里没规则 → 通知 0 条 |

失败指纹（加锁前的并行档）：红点全落在同一处、同一句断言 ——
`必须真的发出一条通知  left: 0 / right: 1`（`expense_linkage_tests.rs:603 / 662 / 712 / 784`）。
接线若缺失，串行档不可能 6/6；断言若过期，红的是「值不对」而不是「0 条」。

## 2. 实测矩阵（本轮 run 1593，同一棵树 `9f2c2d1a`；每个跑法一份独立克隆库）

克隆库来源：`cloudpivot_test_p260`（295 条迁移 + 演示种子，177 张表 / 14 用户），
`CREATE DATABASE … TEMPLATE` 克隆，物理独立库，互不干扰。

| # | 跑法 | 结果 | 日志 |
|---|---|---|---|
| 1 | **加锁前** `cargo test --lib business::erp::expense_linkage_tests`（默认并行，4 核） | **2 passed / 4 failed**，4 条全红在「通知 0 条」 | `01_before_parallel.txt` |
| 2 | **加锁前** 同库 `-- --test-threads=1` | **6 passed / 0 failed**（⇒ 接线在、断言对） | `02_before_serial.txt` |
| 3 | **加锁后** 默认并行 | **6 passed / 0 failed** | `03_after_parallel.txt` |
| 4 | **加锁后** `cargo test --lib expense_linkage -- --test-threads=1`（卡面验收命令，干净库） | **6 passed / 0 failed** | `04_after_serial_clean.txt` |
| 5 | **加锁后** 同一个库再跑一遍（带第一轮残留 = 幂等性） | **6 passed / 0 failed** | `05_after_serial_rerun.txt` |

第 3 行是本卡真正的价值：**不靠 `--test-threads=1` 的调用方约定**，全量回归
（`cargo test --lib` 默认并行）里这 6 条也绿。

⚠️ 加锁前的红条数是**竞态**，别死记：同一天两次独立实测分别是
2 passed / 4 failed（本轮）与 3 passed / 3 failed（`79425867` 的提交说明）；
只要并行就随机。**判定「是不是夹具问题」看红点位置与串行反证，不看红条数。**

## 3. 复现方式

```bash
# ① 克隆一份干净库（clone 自已有的全量迁移 + 演示种子库；物理独立，不碰别人）
su - postgres -c "psql -c 'CREATE DATABASE cloudpivot_test_lnk05c OWNER alun TEMPLATE cloudpivot_test_p260'"

# ② 跑（卡面验收命令）
cd backend
ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/cloudpivot_test_lnk05c" \
  cargo test --lib expense_linkage -- --test-threads=1
# 期望：6 passed / 0 failed

# ③ 反向验证：把 6 处 `let _lnk05 = LNK05_LOCK.lock().await;` 去掉后，**不加** --test-threads=1
ERP_DB_URL=… cargo test --lib business::erp::expense_linkage_tests
# 实测：2 passed / 4 failed（另一次 3/3）
```

不设 `ERP_DB_URL` 时全部 SKIP（打印 `SKIP: 未设置 ERP_DB_URL`），那是假绿；
上面每一步都必须带真实 URL，并核对耗时（真跑约 24s，全 SKIP 会是 0.0x s）。

## 4. 代码侧改了什么（两个提交，都只碰这一个文件）

### 4.1 `79425867` —— 加锁本体（作者 `frontend-developer`，代提交）

`backend/src/business/erp/expense_linkage_tests.rs`：

1. 新增模块级 `static LNK05_LOCK: tokio::sync::Mutex<()>`，6 条用例在 `ensure_db()` 之后、
   `ensure_fixtures()` 之前 `let _lnk05 = LNK05_LOCK.lock().await;`，全程持有到用例结束；
2. 模块头 `# 串行口径` 段 + 锁的 doc 注释写清「为什么换唯一前缀没用」（唯一键行全库只有一行）。

**没有**改产品代码、**没有**改任何断言、**没有**动迁移。
与仓内既有铁律同口径：`fixture_hygiene::FINANCE_SEED_LOCK` / `SEED_ROLE_LOCK`
「同一批全局行 = 同一把锁」。

### 4.2 本轮提交 —— 修 `79425867` 带进来的两处问题

1. **main 编不过**：`79425867` 顺手把并行卡 P2-60 的在途改造
   （`ensure_db()` 改调 `crate::shared::test_fixture::connect`）一起提交了，而
   `backend/src/shared/test_fixture.rs` 与 `shared/mod.rs` 的 `pub mod test_fixture;`
   **都没进提交** ⇒ `cargo test` 直接 `error[E0433]: failed to resolve: could not find
   test_fixture in shared`（HEAD 上 11 条编译错）。本轮把 `ensure_db()` 恢复成
   `Db::connect_with(...)` 原实现，main 恢复可编译；P2-60 的改造留在其自己的工作区，
   等它连模块一起提交时再落地。
2. **实测口径校正**：锁的 doc 里原本写「加锁前 3 passed / 3 failed」，那是竞态的一次采样。
   本轮把表格换成 4 行实测（加锁前并行 2/4、加锁前串行 6/0、加锁后并行 6/0、加锁后串行 6/0 ×2），
   并注明「红条数本身随机」，避免后人拿数字当判据。

## 5. 验证环境备注

- 主机：172.18.10.99（工作仓 `/opt/cloudpivot`），PG 本地 5432，账号 `alun`。
- 因为共享工作区当时有并行卡 P2-60 未提交的 `backend/src/shared/test_fixture.rs`（编译失败中），
  本轮验证在 `git worktree add /root/wt-lnk05 HEAD` 的**隔离工作树**里跑，
  `CARGO_TARGET_DIR=/opt/cloudpivot/backend/target` 复用依赖产物 —— 与本卡无关的红不算到卡上。
- `cargo fmt -- --check`（隔离树，仓库 `backend/rustfmt.toml` + edition 2024）= 0。
