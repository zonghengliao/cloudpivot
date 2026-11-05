# P2-1A：`business/task/handler.rs`（5757 行）按职责拆分

卡：`t_af67b114` ｜ 类型：**纯 refactor（行为零变化）** ｜ 提交：见 `git log --grep P2-1A`

## 1. 为什么做

`backend/src/business/task/handler.rs` 拆分前 **5757 行**（卡面写 5664，卡面数字已过期；以
`git show HEAD:backend/src/business/task/handler.rs | wc -l` = 5757 为准），是全仓最大的单个
`.rs`：任务 CRUD、父子级联、依赖环检测、评论、附件、分派、AI 入口、通知扇出全挤在一个文件里，
改一处要通读半天 —— 可维护性评分 3 分的主因。

## 2. 怎么拆

沿用仓内已有的 TD-2 拆分法（`docs/refactor-methodology.md` + `scripts/refactor/split_module.py`）：
`handler.rs` 退化成 **facade（127 行）**，只做 `pub use` 重导出；实体分居 11 个代码部件 +
3 个测试部件。调用方一行没改（`handler::{page, assign, change_status}` 等路径全部照旧）。

| 文件 | 职责 | 行数 |
|------|------|------|
| `handler.rs` | facade：重导出 + `#[path]` 挂测试部件 | 127 |
| `task_handler_common.rs` | 底座：错误构造 / 入参归一 / 行转换 / 身份与权限闸 / 状态机错误映射 | 432 |
| `task_handler_notice.rs` | 通知扇出：草稿拼装 / 分级发送 / 状态与前置联动提醒 | 362 |
| `task_handler_crud.rs` | 任务 CRUD：分页 / 按单据查 / 建 / 复制 / 改 / 删 / 详情 / 我的任务 | 932 |
| `task_handler_status.rs` | 状态机与进度：改状态 / 改进度（审批门闸 + 事务编排） | 656 |
| `task_handler_hierarchy.rs` | 父子关系：建子任务 / 子任务列表 / 父状态巡检修复 | 344 |
| `task_handler_dep.rs` | 依赖与甘特：加/解依赖（环检测）/ 进 DOING 前置校验 / 甘特取数 | 564 |
| `task_handler_comment.rs` | 评论：发评论（双身份）/ @提及 / 强制通知 | 326 |
| `task_handler_attachment.rs` | 附件：上传（可见性校验）/ 单文件读取 | 228 |
| `task_handler_ai.rs` | AI 执行者入口：ai-progress 回写 / dispatch-ai 派活 | 280 |
| `task_handler_assign.rs` | 分配：分派转派矩阵 / 协作人增删查 / 成员配额与 ROBOT 守卫 | 707 |
| `task_handler_flow.rs` | 流程与统计：任务流程来源查询 / 按执行模式统计 | 260 |
| `task_handler_tests.rs` | 接口层纯逻辑用例（19 个函数） | 356 |
| `task_handler_stats_tests.rs` | 统计口径用例（4 个函数） | 68 |
| `task_handler_copy_db_tests.rs` | 复制接口真库用例（9 个函数） | 395 |

最大文件 932 行（`task_handler_crud.rs`），全部 < 1500 行。

`backend/src/business/task/mod.rs` 追加 11 行 `mod` 声明（含每个部件的职责注释）。
拆分脚本的 plan 落在 `scripts/refactor/plans/task_handler.json`，派生 `use` 清单的
`scripts/refactor/gen_part_uses.py` 一并入库，可复现。

## 3. 验收对照

| 验收项 | 结论 | 证据 |
|--------|------|------|
| 拆成 ≥4 个文件，每个 <1500 行 | ✅ 15 个文件（11 代码 + 3 测试 + facade），最大 932 行 | `line_counts.txt` |
| 行为零变化（全量测试前后同一结果） | ✅ 前 226 passed×2 ／ 后 226 passed×2，用例名单 226 条逐条相同 | `task_run1_before.txt` / `task_run2_before.txt` / `task_module_runs_after.txt` / `test_names_diff.txt` |
| 无新增 dead_code 警告 | ✅ `cargo check --lib` 0 警告；`cargo clippy --lib --tests -D warnings` 通过 | `static_checks.txt` |
| git commit（纯 refactor） | ✅ 单提交 `bae90c0`，仅 `backend/src/business/task/*` + 证据 + 拆分工具 | `git show --stat bae90c0` |

> 回归是**提交后**（`bae90c0`，工作树与提交一致）跑的第二组两轮，日志见
> `task_module_runs_after.txt`；提交前在同样的工作树上也跑过两轮，结果相同。

### 3.1 「只搬不移」的机械证明（`verify_move.txt`）

用仓内 `scripts/refactor/verify_move.py`（TD-2 起就在用的保真度校验器），以 `HEAD`（拆分前）
为基准，对 `facade + 14 个部件` 拼回后的整体做四重多重集比对：

```
条目多重集        前=137   后=137   MISSING=none  ADDED=none
SQL 字面量        前= 24   后= 24   MISSING=none  ADDED=none
用例名            前= 24   后= 24   MISSING=none  ADDED=none
代码 token 多重集  前=28013 后=28013 MISSING=none  ADDED=none
结论：PASS（只搬不移）
```

- **代码 token 多重集完全相等**是关键：去掉注释 / `use` / `mod` 结构行后逐 token 计数一致，
  等价于「没有一行代码被改写」（不受 rustfmt 换行影响）。
- 24 条 SQL 字面量逐条相同 ⇒ 查询/更新语句一字未动。
- 137 个顶层条目（fn / struct / enum / const / impl）一个不多一个不少。

### 3.2 逐条对账（`move_audit.txt`）

1. 代码条目：HEAD 107 个顶层条目 = 部件 103 + 3 个测试 mod（已落独立文件）+ `__head__` 伪项；
   部件独有 0、跨部件重复 0。
2. 测试函数：32 个（19+4+9）逐函数正文与 HEAD 一致；唯一差异是 rustfmt 对
   `user_comment_content_limit_is_strict` 一行的换行重排（去空白后完全相同）。
3. 可见性：HEAD 20 个 `pub` 条目**无一被改窄**；58 个原本私有的辅助项放宽为 `pub(super)`
   —— 这是跨文件调用的必要代价（拆分前同文件调用不需要）。

## 4. 过程中发现并修掉的问题（都是拆分本身的副作用，不涉业务逻辑）

| 现象 | 根因 | 处置 |
|------|------|------|
| 4 处 `E0451`: `TaskNotice` 字段私有 | 拆分器原样搬迁，但结构体字段被别的部件拿去构造 | 字段加 `pub(super)`（`task_handler_notice.rs`），可见性不放宽到 `pub` |
| 171 条 unused-import 警告 | 拆分器把原文件整块 import 复制进每个部件 | `scripts/refactor/fix_unused_imports.py` 按编译器警告裁剪，直到 0 警告 |
| `E0252`: `model` 重复导入 | 测试部件既有文件级 import 块，body 内又有一条 `model::{self, NewTask}` | 文件级保留 `model`，body 内改成 `model::NewTask` |
| facade 的 `pub use` 报「no imported item is public enough」 | `common` 条目原本就是 `pub(crate)`（被 `burndown`/`collab` 等兄弟模块引用） | facade 用 `pub(crate) use super::task_handler_common::*`，与拆分前可见性一致；`notice` 拆分前即私有，不再对外重导出 |

## 5. 怎么复跑

```bash
# 保真度（只搬不移）
cd /opt/cloudpivot && python3 scripts/refactor/verify_move.py HEAD business/task/handler.rs

# 静态检查
cd backend && cargo check --lib && cargo clippy --lib --tests -- -D warnings

# task 模块回归（连库，两轮）
cd backend
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_libtest_task' \
TASK_DB_URL="$ERP_DB_URL" \
P0FIX_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_libtest_task_p0fix' \
cargo test --lib 'business::task::' -- --test-threads=1
```

## 6. 范围说明

- 只拆 `task/handler.rs`；`task/model.rs`（TD-2 已拆）、`task/state.rs`（1760 行）、
  `task/dto.rs`（1816 行）、`task/collab.rs`（1497 行）**不在本卡范围**，未动。
- 未跑无关模块的全量回归（卡面要求）；`business::task::` 面 226 条用例覆盖本次改动的全部代码路径。
- 共享工作区提示：`backend/src/system/auth/cache_tests.rs` 是他人未提交文件，卡内已留言；
  本次提交未包含它。
