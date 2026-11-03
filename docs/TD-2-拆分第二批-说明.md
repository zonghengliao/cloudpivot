# TD-2 拆分第二批：全仓最大的两个后端文件

> 纯重构。**没有改一行业务逻辑、没有改任何 SQL、没有改任何状态机语义**，
> 现有单测**一条都没改**（用例函数名、断言、夹具调用逐字相同）。
> 方法论见 `docs/refactor-methodology.md`（§10 是第二批的复盘与新增坑）。

## 一、拆成了哪几个文件、各多少行

### 1. `backend/src/business/task/model.rs`：7604 → **142 行**（facade）

10 个代码部件：

| 文件 | 行数 | 职责 |
|---|---:|---|
| `task_model_base.rs` | 564 | 底座：数据权限锚点（`TASK_TARGET` / `VISIBLE_OR_TAIL` / 参与 id 数组）、常量、状态机纯逻辑（`cascade_target` / `blocks_child_advance`）、时间桶/编号/新任务纯函数、SQL 列清单常量 |
| `task_model_rows.rs` | 812 | 行结构与契约：`TaskCardRow` / `TaskDetailRow` / `TaskFilter` / `VisiblePage` / `NewTask` / `TaskPatch` / `StatusUpdate` / `DependencyRow` / `Gantt*` / `ScheduleRow` / `PeerSchedule` … |
| `task_model_query.rs` | 777 | 只读：分页（`page_visible`）/ 详情 / 我的任务 / 子任务 / 协作人 / 附件 / 用户校验 / AI 轮询目标 |
| `task_model_write.rs` | 526 | 写入：建 / 改 / 软删 / 协作人 / 评论 / 进度 / 转派 |
| `task_model_hierarchy.rs` | 634 | 父子链：深度校验 / 进度重算 / 状态级联 / 取消级联 / 巡检 |
| `task_model_dep.rs` | 441 | 依赖与甘特：前置/后继/环检测/甘特取数 |
| `task_model_ai.rs` | 333 | Hermes 同步：令牌 / 派活 / 状态回写 / AI 附件绑定 |
| `task_model_stats.rs` | 91 | 按执行模式的完成率统计 |
| `task_model_flow.rs` | 230 | 任务 ↔ 流程：来源实例/节点查询、待办人解析、走向字段 |
| `task_model_schedule.rs` | 152 | 改期：排期快照读取与落库 |

9 个测试部件（`#[path]` 挂载，用例名不变）：

`task_model_tests.rs` 333→321 / `_display_file_name_tests.rs` 61 / `_depth_tests.rs` 419 /
`_root_only_tests.rs` 385 / `_statuses_filter_tests.rs` 393 / `_stats_tests.rs` 55 /
`_repair_tests.rs` 331 / `_cascade_tests.rs` 596 / `_cascade_cancel_tests.rs` 529
（后两个由原 `cascade_tests`（1135 行）按「状态级联」/「取消级联」拆开，
原内嵌 `mod t11_cancel` 提升为独立模块 `cascade_cancel_tests`）。

### 2. `backend/src/business/erp/stockin_handler.rs`：9394 → **131 行**（facade）

7 个代码部件：

| 文件 | 行数 | 职责 |
|---|---:|---|
| `stockin_handler_common.rs` | 682 | 底座：错误构造 / 行计划（`plan_lines`）/ 快照与派生总额 / 锁与权限判定 / 表单快照 |
| `stockin_handler_purchase.rs` | 482 | 采购联动：来源单解析 / 供应商守卫 / 行回填 / 过账前复查 / 单位守卫 |
| `stockin_handler_write.rs` | 497 | 写入：建单 / 复制 / 编辑 / 删除 |
| `stockin_handler_post.rs` | 847 | 过账：提交审批 / 审批通过后过账 / 事务内扣库与成本 / 序列号校验 |
| `stockin_handler_cancel.rs` | 612 | 作废：提交/撤回作废审批 / 审批后反冲 |
| `stockin_handler_status.rs` | 259 | 状态流转与其余 HTTP 入口 |
| `stockin_handler_query.rs` | 101 | 只读入口：分页 / 详情 |

13 个测试部件（原 `mod tests` 5990 行，按用例语义分组；夹具与造数工具独立成两部件）：

| 文件 | 行数 | 内容 |
|---|---:|---|
| `_tests_fixtures.rs` | 445 | 夹具常量 / `ensure_db` / `setup` / 清理 / 小工具 |
| `_tests_helpers.rs` | 816 | 造单 / 造采购单 / 造库位 / 查询小工具 |
| `_tests_posting.rs` | 468 | 过账主链 |
| `_tests_update.rs` | 377 | 编辑/锁定/序列号/分页筛选 |
| `_tests_location.rs` | 328 | 库位账 |
| `_tests_cancel.rs` | 623 | 作废反冲（含序列号、并发、不变量 E4） |
| `_tests_p10_locked.rs` | 274 | P10 已过账字段锁 |
| `_tests_dc03.rs` | 352 | DC-03 来源单关闭/重复开单 + 过账与作废并发 |
| `_tests_p16.rs` | 705 | P16 作废走审批流 |
| `_tests_d4_purchase.rs` | 426 | D4 在途/待收口径 |
| `_tests_pflow3a.rs` | 448 | P-FLOW-3A 过账走流程 |
| `_tests_d9_copy.rs` | 377 | D9 复制入库单 |
| `_tests_a4.rs` | 547 | A4 供应商来源派生 |

**拆分后最大的文件 847 行**（`stockin_handler_post.rs`），全部 41 个新文件 < 1000 行
（`docs/evidence/20261103-td2/06-wc-l.txt`）。

## 二、对外接口一个没变

- 两个原文件保留为 **facade**：只留模块文档 + `pub use super::<部件>::*;` + 测试挂载点；
  `handler` / `bridge` / `router` 里所有 `use crate::business::{task::model, erp::stockin_handler}::xxx`
  **一行未改**（`git diff --stat` 里没有这些文件）。
- 部件模块在 `task/mod.rs`、`erp/mod.rs` 里声明为**私有 `mod`**，外部只能走 facade。
- 唯一的可见性改动（与 TD-1 同规则，crate 内、不进公开 API）：
  `fn` → `pub(super) fn`；3 个跨部件访问的结构体字段 → `pub(super)` 字段
  （`LinePlan.new`、`ParentPointer.supplier_id` / `.is_sourceless`）。
- `stockin_handler_purchase` 部件里没有任何 `pub` 项，因此**不进 facade 重导出**
  （拆分前它们也是模块私有，对外可见性不变）。

## 三、验收

| 项 | 结果 | 证据 |
|---|---|---|
| `cargo build` | 通过 | 07-build-clippy.txt |
| `cargo check --all-targets` | 0 error / **0 warning** | 07-build-clippy.txt |
| `cargo clippy --lib -- -D warnings` | 通过 | 07-build-clippy.txt |
| 受影响面单测（拆分前后逐条同数） | `business::task` **210 → 210**；`business::erp::stockin` **133 → 133**，0 failed | 01-baseline.txt / 02-after-tests.txt |
| 全量 `cargo test --lib` | **2087 passed / 0 failed**（1074.52s，干净克隆库） | 04-full-lib.txt |
| 19 个集成 target + testd | 18 个 target 157 passed / 0 failed（`--test-threads=1`）+ testd **67 / 0** | 04-full-lib.txt |
| 每个新文件 <1000 行 | 最大 847 | 06-wc-l.txt |
| 保真度（条目 / SQL / 用例名 / 代码 token 四重多重集） | 全部 MISSING=none ADDED=none | 05-fidelity.txt |
| 单测是否被改 | 用例名多重集 77→77、122→122；断言与夹具调用逐 token 相同 | 05-fidelity.txt |
| 原有 `pub` 接口一个没变 | `task/model.rs` 140 → 140、`stockin_handler.rs` 23 → 23，丢失/新增均为 none | 08-pub-api.txt |
| 调用方零改动 | 提交里 `task/handler.rs`、`erp/handler.rs`、`*_bridge.rs`、router、bootstrap 出现 0 次 | 09-git.txt |

不变量的自查（32/32）：本卡**只搬代码**，未触碰任何业务分支/SQL/状态机；
`verify_move.py` 的 token 多重集（36807 / 54942 个 token 逐一相同）是比「人眼 diff」
更强的机械证据 —— 任何一处逻辑改动都会让多重集对不上。

## 四、发现的问题（只报告，未修）

1. **`cash_flow::tests::r3_real_config_covers_all_accounts` 的既有「假红」**
   （方法论 §9② 已记录）：`voucher_query` 的夹具会往 `biz_account` 插 `P2D1A` / `P2D1B`
   两个科目且不回收；用**跑过一整轮的库克隆**当模板，`r3` 必红。
   实测：`cp_td2_after`（tdh 克隆）里 `select account_code from biz_account where account_code like 'P2D1%'`
   → `P2D1A / P2D1B`；删掉这两行后重跑全量 → 见 04-full-lib.txt。与本卡改动无关。
2. **测试模块路径变了**（拆超限测试 mod 的唯一代价）：如
   `stockin_handler::tests::x` → `stockin_handler::stockin_handler_tests_cancel::x`。
   用例名/断言不变；文档里出现旧路径的地方已同步（`docs/refactor-methodology.md` §10.3）。
3. `scripts/refactor/split_module.py` 的多行属性解析 bug 已在本卡修掉（否则 stockin 的
   27 处 `#[route(...)]` 会被撕开，编译直接 `unexpected closing delimiter`）。
