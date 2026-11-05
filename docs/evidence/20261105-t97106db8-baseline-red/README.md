# t_97106db8 · 144 main 基线既有红 13 条：逐条定性与处置

> 卡：`t_97106db8`（来源 `t_e7c3cf7d` 全量回归的归因结论）
> 分支/基线：`/opt/cloudpivot` 工作仓，起始 HEAD `08960c0`
> 复跑口径：每个 target 一个独立库（`cloudpivot_test_t97106_*`，183 迁移 + bootstrap，超管 1 人），
> 命令与卡面一致（`cargo test --test <target>`，库串接 `*_DB_URL`），**未加 `--test-threads=1`**。

## 结论一句话

13 条红里 **没有一条是「代码实现被改坏」**，但也不是「全部改用例就完事」：

| 类别 | 条数 | 处置 |
|---|---|---|
| 用例期望过期（实现自 P16/ARCH-3 起改了语义） | linkage 8 条 | 改用例：补「送审 → 复核通过」两步，**六账断言一条不少** |
| 用例/夹具的**全局状态**在默认多线程下互踩 | impexp 1、pflow1b 2（含偶发 1） | 加进程级串行闸（与 `tests/linkage/common::serial` 同法） |
| 用例的**整库口径**断言写死常量 | batchB_b2 1、pflow1a 1（TOCTOU 部分） | 改成「与库里实际值比」，判据更强 |
| **实现缺陷**（领域错误穿过事务被压成 Db → 409 变 500） | batchC_c4 1 | 改实现：补反解 + HTTP 出口分流 + 回归单测 |
| **种子数据缺陷**（V0211 快照数组非 canonical） | pflow1a 1（修完 TOCTOU 后暴露） | 新增迁移 V0246 修正快照顺序 + hash（带守卫/自检/可回滚） |

## 复跑结果（同库连跑两轮，第二轮 = `logs/run3-*.log`）

| target | 轮1 | 轮2 | 库 |
|---|---|---|---|
| `linkage` | 62 passed / 0 failed | 62 / 0 | `cloudpivot_test_t97106_linkage` |
| `batchC_c4_help` | 11 / 0 | 11 / 0 | `cloudpivot_test_t97106_c4` |
| `impexp_worker` | 7 / 0 | 7 / 0 | `cloudpivot_test_t97106_a4` |
| `pflow1a` | 12 / 0 | 12 / 0 | `cloudpivot_test_t97106_pflow1a` |
| `pflow1b` | 14 / 0 | 14 / 0 | `cloudpivot_test_t97106_pflow1b` |
| `batchB_b2_scope` | 1 / 0 | 1 / 0 | `cloudpivot_test_t97106_b2` |

每个 target 的 `SKIP` 行数 = 0（即**没有一条是靠「缺库静默 SKIP」变绿的**）。

## 逐条

### 1) `linkage` 作废链 8 条（S06–S12、S15）

* **定性**：用例期望过期。实现自 P16（`stockin_handler_status.rs`）起，已过账单据作废必须先走
  `stock_in_cancel` 复核：`POSTED → PENDING_CANCEL`（六本账一动没动），**审批通过的回调**
  （`stockin_bridge::on_cancel_settled` → `cancel_after_approval` → `cancel_in_tx`）才真作废；
  没配流程时是 409 + 66033（不是旧版的静默直作废）。卡面给的另一个选项（夹具里把流程置 DISABLED）
  在当前实现下**拿不到「立即作废」**，只会得到 409，所以只能选「补审批」这条路。
* **处置**：`tests/linkage/stockin_chain.rs` 新增 `cancel_with_approval(ctx, id, reason)`：
  发起 → **断言停在 `PENDING_CANCEL`** → 按生产路径逐条同意待办（受派人身份，遍历到无 PENDING）
  → 断言 `CANCELLED`。原有六账/凭证红冲/应付/序列号/四账回基线的断言**全部保留**，
  等于顺带补上「送审时账不动」这条新语义的判据。

### 2) `batchC_c4_help::h03_slug_unique_and_format`

* **定性**：**实现缺陷**。`create_doc`（批次 C·C6 引入的事务包装）把 `HelpError::DocSlugConflict`
  用 `help_error_to_db` 编码成 `InvalidValue("帮助文档：文档 slug 已存在")` 穿过事务，
  但提交后**没有反解**，`?` 直接把它还原成 `HelpError::Db(..)` ⇒ 契约要求的 409 + 62016 变成 500。
  同一缺陷在 HTTP 侧 5 个事务出口（新建/保存/删除/发布/回滚）都存在。
* **处置**：`help/model.rs` 加 `help_error_from_db` + `HelpError::from_message`（文案 ↔ 变体一一对应，
  由新单测 `help_error_round_trips_through_transaction` 锁住），5 个包装与 5 个 handler 出口全部反解；
  handler 新增 `db_or_domain`，真正的数据库异常仍走原 `utils::db_error`。

### 3) `impexp_worker::worker_marks_failed_when_definition_missing`

* **定性**：**用例并发缺陷**（不是实现问题，也不是「canceled vs failed」）。
  实测复跑报告是 `failed=1 canceled=0`，但**自己那条行仍是 `PENDING/attempt=0`** ——
  `worker::run_once_with` 认领的是「全表最老的 PENDING」，不认 id；并行时别的用例刚
  `insert_export_task` 的行被抢走并失败，本用例再查自己那行就还是干净态。
* **处置**：本文件所有用例加进程级串行闸 `serial()`（僵尸/过期用例把行从 PENDING 改成 RUNNING
  的那一步同样会被插进来）。

### 4) `pflow1a::publish_regression_keeps_graph_hash`

* **定性（两层）**：
  1. **用例口径**：`WHERE status='PUBLISHED'` 取的是全库流程，而库里还有同套其它用例用
     `create_draft` 造的客户副本（`is_system = FALSE`），它们会在遍历期间被自己的用例软删 →
     TOCTOU，报「流程 pflow_aprd_03be0414 不存在」。→ 按 `is_system = TRUE`（内置模板）过滤。
  2. **真缺陷（种子数据）**：过滤后暴露出 `stock_take_post` 重新发布后 `graph_hash` 变了
     （`cfc610… → 331f25…`）。逐字节对比快照后确认**内容完全一致、只有数组顺序不同**：
     V0211 把 `graph_snapshot.nodes/edges` 按「画布 sort_order」写，而 canonical 规则是
     nodes 按 `key`、edges 按 `(from,to,key)`；种子写死的 hash 是对非 canonical 顺序算的。
     影响：① 任何一次「另存草稿→重新发布」都会改 hash；② D4d 每日自检会把这条种子报成
     `SnapshotEdited` 漂移。用独立 canonical 序列化器（Python：递归键排序 + 紧凑 + sha256）
     双向验过：`canonical(重排后的种子快照) == canonical(重新发布后的快照) == 331f25…`。
* **处置**：新增 `V0246__flow_stock_take_snapshot_canonical.{up,down}.sql`：按 canonical 规则重排
  该版本的两个数组并把 hash 改成引擎口径的值。守卫三重（版本行/旧 hash/6 节点 6 边的内容集合），
  用户已重新发布过的库自动跳过；迁移末尾自检 hash 与首项不是 canonical 就抛错。可回滚（down 还原 V0211 形态）。

### 5) `pflow1b`（`task_accept_approval_advances_node` + `concurrent_complete_only_one_advances`）

* **定性**：用例并发缺陷。本文件有 3 个用例把种子的 `task_accept` 流程临时置 `DISABLED`
  （模拟「没配验收审批」的两级降级），而 libtest 默认多线程：
  * s14 刚恢复 PUBLISHED，隔壁用例立刻又停用 → `prepare_accept` 返回 None →
    「提交验收必须自动发起验收单」红；
  * s10 在「建流程 → 存图 → 发布」窗口里被打断 → publish 读到空图（65010）。
* **处置**：全部用例加 `serial()`（有全局状态的用例本来就不该并行）。

### 6) `batchB_b2_scope::b2_real_database_acceptance`

* **定性**：用例的**整库口径断言过期**（两处）：
  * `超级管理员看到全部用户 == 12`（硬编码）：库里多一行就红（实测 36 ≠ 12）。
    改成「等于库里未软删用户的实际总数」——比常量**更强**（同时证明过滤一行没漏也没多），
    并保留「≥ 夹具人数」的前置断言。
  * `preflight_permission().checked_tables == 10`：`SCOPED_TABLES` 是**开放清单**，
    后来接了新表（已 11 张），写死条数必红。改成 「== `SCOPED_TABLES.len()`」。

## 备注

* 未改动任何其它模块；`cargo fmt --check`（edition 2024）对本卡改动的 7 个 Rust 文件全绿。
* 工作区里其它 worker 的在途改动（`backend/src/business/erp/payable_payment.rs`、
  `serial_handler.rs` 等）未纳入本次提交。
