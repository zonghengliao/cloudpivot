# P2-1B · flow/model.rs 巨型文件拆分（纯搬迁）

卡片：`t_5560689c`　|　类型：纯 refactor（零行为变更）　|　执行日：2026-11-05

## 0. 结论先说

`backend/src/business/flow/model.rs` **4732 行 → 55 行 facade + 12 个部件文件**（10 个代码部件 + 2 个测试部件），
最大文件 **884 行**（全部 < 1500）。调用方路径零改动（`flow::model::xxx` 一行没动）。

四项验收逐条对照：

| 验收项 | 结果 | 证据 |
|---|---|---|
| 拆成 ≥4 个文件，每个 <1500 行 | **PASS**：13 个文件（facade 1 + 代码部件 10 + 测试部件 2），最大 884 行 | `line_counts.txt` |
| 行为零变化（全量测试对比） | **PASS**：`verify_move` 四重多重集全等；`business::flow::` 359 条用例名+结果在拆分前后**逐条相同**（359/359）；`model` 自身 14 条用例两轮均 14 passed/0 failed | `verify_move.txt` / `baseline_flow_module_tests.txt` vs `split_flow_module_tests.txt` / `split_model_tests_round1.txt`/`round2` |
| git commit（纯 refactor） | **PASS**：提交只含 `backend/src/business/flow/{model.rs,mod.rs,flow_model_*.rs}` + `scripts/refactor/verify_move.py`（新增一条比对配方）+ 本证据目录 | `git show --stat` |
| 编译零警告 | **PASS**：`cargo check --lib --tests` EXIT 0，warning 行 0；`rustfmt --check` EXIT 0 | `split_check_lib_tests.txt` |

> 卡面写 4708 行，实测拆分前 **4732 行**（`git show 6a827e2:backend/src/business/flow/model.rs | wc -l`）——卡面数字过期，其余口径一致。

## 1. 拆了哪些文件（职责）

| 文件 | 行数 | 职责 |
|---|---:|---|
| `model.rs`（facade） | 55 | 只保留模块文档 + `pub use` 重导出 10 个部件 + `#[path]` 挂 2 个测试部件 |
| `flow_model_base.rs` | 884 | 底座·模型定义：九张表列清单、JSON/时间/主键参数助手、全部行结构（定义/版本/实例/节点/任务/抄送）、分页与写入入参 |
| `flow_model_def.rs` | 652 | 流程定义 CRUD、版本管理、发布/归档、`load_snapshot` |
| `flow_model_graph.rs` | 534 | 编辑期图（L1）读整图 `load_graph` / 覆盖写 `replace_graph` |
| `flow_model_guard.rs` | 93 | 写路径守卫与不变式断言（PUBLISHED 只读等） |
| `flow_model_instance.rs` | 285 | 实例行读写、实例号、发起 |
| `flow_model_node.rs` | 280 | 节点/节点配置读写 |
| `flow_model_cancel.rs` | 257 | 终止/撤销（实例与节点、结束时间与意见） |
| `flow_model_progress.rs` | 375 | 进度/参与人（含抄送） |
| `flow_model_inbox.rs` | 507 | 三身份列表（我的待办/已办/抄送）口径常量与查询、`instance_visibility` |
| `flow_model_stats.rs` | 309 | 审批统计（总览/效率/卡点）：时间窗、口径常量、汇总与最慢项 |
| `flow_model_tests.rs` | 92 | 纯逻辑单测（5 条） |
| `flow_model_delete_tests.rs` | 553 | 真库用例（8 条，`TASK_DB_URL`）：任务删除 → 审批单自动终止 |

`backend/src/business/flow/mod.rs` 追加 10 行 `mod flow_model_*;` 声明（`mod` 私有，对外仍只经 `model.rs` facade），
声明顺序 = facade 的 `pub use` 顺序（底座 → 定义 → 图 → 守卫 → 实例 → 节点 → 终止 → 进度 → 列表 → 统计）。

## 2. 怎么证明「只搬不移」

`scripts/refactor/verify_move.py`（P2-1A 建立、本卡加一条 flow 配方）：

```
python3 scripts/refactor/verify_move.py 6a827e2 flow/model.rs
  条目多重集      前=207 后=207  MISSING=none ADDED=none
  SQL 字面量      前=111 后=111  MISSING=none ADDED=none
  用例名          前= 14 后= 14  MISSING=none ADDED=none
  代码 token 多重集 前=24998 后=24998 MISSING=none ADDED=none
结论：PASS
```

口径：忽略**注释行 / use 语句整块 / mod 声明**，其余逐 token 比对——即「没有一行函数体被改过」的机械证明。

## 3. 拆分必须做的三处收敛（不是逻辑改动）

1. **测试部件补 import**：拆分前 `mod tests { use super::* }` 住在 `model.rs` 内，靠父模块的私有 `use` 绑定拿到
   `Uuid` / `chrono` / `db` / `set_db` / `Value` / `DatabaseConfig`；`#[path]` 外挂后继承链断开，两个测试部件各补
   `use nik::prelude::*; use nik::uuid::Uuid;`（`chrono`/`db`/`set_db`/`Value` 由 nik prelude 转发）。
2. **`MyTaskOrder::order_sql` 可见性**：`flow_model_inbox.rs` 里原是模块私有 `fn`，跨部件单测要调 → 改 `pub(super)`
   （仍在 `flow` 模块内，等于拆分前的可见范围，不出模块）。
3. **每个部件只保留自己用到的 import**：搬出来的整块 import 按各部件实际用量裁剪（否则 `-D warnings` 不干净）。

上面 1/2 是"文件边界"带来的编译期副作用，没有任何表达式、SQL、签名变化——`verify_move` 的 token 多重集 24998=24998 已经覆盖证明。

## 4. 实测输出

### 4.1 编译（干净隔离树 `/root/p21b/wt` @ 6a827e2 + 本卡文件，独立于并发修改）

```
cargo check --lib --tests     →  Finished dev profile in 1m 54s   EXIT=0
warning 行数                  →  0
rustfmt --edition 2024 --check →  EXIT=0
```

> 为什么用隔离树：`/opt/cloudpivot` 是**多 worker 共用工作区**，跑 checks 时 `flow/linkage/admin.rs`
> 正被另一张卡改到一半（9 条 E0277：`Uuid: Deserialize` 未满足），会污染本卡的全量编译结论。
> 隔离树用 `git worktree add <path> 6a827e2` + 只拷本卡 13 个文件，`CARGO_TARGET_DIR` 指共用 target 复用依赖。

### 4.2 行为对比（真库，PostgreSQL；`cp_p21b_flow` 的干净副本）

`business::flow::model`（本卡直接范围）——拆分前后各一份、每份两轮：

```
baseline(6a827e2 未拆) : 14 passed; 0 failed  (8.14s)
split  round 1         : 14 passed; 0 failed  (7.37s)
split  round 2         : 14 passed; 0 failed  (7.29s)
用例名 14=14，逐条 `... ok` diff 为空
```

`business::flow::`（全部调用方/同模块 359 条，验证 facade 对外 API 未变）：

```
baseline : 359 passed; 0 failed  (46.73s)
split    : 359 passed; 0 failed  (47.17s)
逐条「用例名 ... 结果」359=359，diff 为空
```

两个方向都设了 `TASK_DB_URL`（指向 `cp_p21b_flow` 的**独立副本**），因此 8 条真库用例真正执行、没有 SKIP 假绿：

```
cp_p21b_verify            ← split model 用例（两轮复用）
cp_p21b_verify_base       ← baseline model 用例
cp_p21b_verify_splitflow  ← split flow 全模块用例
```

### 4.3 规模

```
55     backend/src/business/flow/model.rs
884    flow_model_base.rs      652 flow_model_def.rs     534 flow_model_graph.rs
375    flow_model_progress.rs  553 flow_model_delete_tests.rs
507    flow_model_inbox.rs     309 flow_model_stats.rs   285 flow_model_instance.rs
280    flow_model_node.rs      257 flow_model_cancel.rs   93 flow_model_guard.rs
92     flow_model_tests.rs
```

## 5. 复现步骤

```bash
# 1) 纯搬迁校验（基准 = 拆分前提交）
python3 scripts/refactor/verify_move.py 6a827e2 flow/model.rs

# 2) 编译
cd backend && cargo check --lib --tests

# 3) 行为对比（真库；库名任意，需已有迁移）
export TASK_DB_URL='postgres://alun:***@127.0.0.1:5432/<db>'
cargo test --lib business::flow::model -- --test-threads=1
cargo test --lib business::flow::        -- --test-threads=1
```

## 6. 偏差 / 未做的事（诚实登记）

- **卡面 4708 行已过期**：实测拆分前 4732 行（`6a827e2`）。
- **未跑无关模块回归**：按卡面「只跑受影响的模块」，只跑 `business::flow::`（359 条）+ 全库 `cargo check --lib --tests`；
  ERP/任务等模块一行未动（`verify_move` 也只覆盖 model.rs）。
- **未跑 `cargo clippy -- -D warnings` 全库**：共用工作区里另一张卡正在改 `erp/*`、`flow/linkage*`，
  clippy 全库结果会被别人的半成品染色；本卡以 `cargo check` 零警告 + `rustfmt --check` 作为门禁。
- **`/opt/cloudpivot/backend/target` 是共用的**：隔离树复用了它（只重编 cloudpivot crate，依赖复用），
  可能与并发卡互相触发一次重编，但不影响本卡结论。
- 顺手发现（**不在本卡范围，未修**）：`scripts/refactor/fix_unused_imports.py` 处理「单名 use」时会把它写成
  `use Uuid;`（丢掉路径前缀），误用会把 `use nik::uuid::Uuid;` / `use super::x::y;` 改坏。本卡改用逐文件快照 +
  回滚处理，未依赖该脚本。

## 7. 相关提交

（见卡内汇报与 `git log`）
