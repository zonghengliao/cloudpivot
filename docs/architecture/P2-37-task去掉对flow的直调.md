# P2-37 · `task` 去掉对 `flow` 的直调（`task → flow` 消边）

> 归属批次：P2-B 解环 · 卡 `t_c2ecac3f` · 前置 P2-01~26（拆目录）、P2-31（`shared` 收口）
> 状态：**`business::task → business::flow` 这条边已不存在**（代码 0 / 注释 0）
> 交接：**P2-38 才有活干** —— 反方向 `flow → task` 仍是代码 10 / 注释 1，见 §6

## 1. 结论

`task` 状态机对审批引擎有 8 处代码直连（4 类用途），全部落在 `flow::task_bridge` 上；
`flow` 反过来又写 `biz_task`（审批回调推进任务 / 流程造任务）—— 这就是 `flow ⇄ task` 环。
本卡只掰**一半**：`task` 不再认识 `flow`。

```text
改前：task ──8 处代码直调──→ flow::task_bridge        （flow → task 7 处，双向 = 环）
本卡：task ──trait + 注册表──→ (抽象)                 ← task 侧零审批域引用（本卡）
                              ↑ 启动期注入
                      flow::task_approval_impl        ← 实现留在审批域（flow → task 原样不动）
P2-38：把写 `biz_task` 的那几段挪出 `flow`，反方向归零
```

## 2. 接缝方向为什么和 P2-32 / P2-35 **相反**

| 接缝 | 谁定义 trait | 为什么 |
|:--|:--|:--|
| `flow::doc_reader`（P2-32）/ `hr::employee_reader`（P2-35） | **被读方的域** | 「单据状态 / 员工基础信息」的**口径**（软删、数据范围、状态取值）归表属主，消费方只拿结果 |
| `task::approval_port`（本卡） | **调用方的域（`task`）** | 这里传的是**命令**（发起审批 / 推进节点 / 撤审批）与**事务内联动**，不是「读别人的数据」。需求归任务状态机，口径归审批引擎 —— trait 定义在 `flow` 就等于 `task` 必须 `use` 审批域，边还在，等于没解 |

换个说法：`doc_reader` 是「我读你」，本接缝是「我请你办事、且要在我这个事务里办」。
后者的抽象只能长在请求方，否则只是「把环挪个位置」。

## 3. 8 处直连怎么消化（逐条对账，缺 = 0）

| # | 原直连 | 用途 | 处置 |
|:--:|:--|:--|:--|
| 1 | `task_handler_common.rs` `use flow::engine` + `engine::is_start_rejected(&err)` | 认出「发起被引擎业务拒绝」哨兵 | → `approval_port::is_start_rejected(&err)`（哨兵判据仍在审批域，任务侧只问「是不是它」） |
| 2 | `task_handler_status.rs` `instance_status_of(id)` | 审批门闸的输入 | → `approval_port::instance_status_of` |
| 3 | `task_handler_status.rs` `prepare_accept(...)` | 能不能发起（没配流程 → `None`） | → `approval_port::prepare_accept(AcceptRequest{..})`，产出**不透明句柄** `PreparedAccept` |
| 4 | `task_handler_status.rs` `start_in_tx(&tx, start, ..)` | 事务内发起 + 回写 `approval_instance_id` | → `approval_port::start_in_tx(&tx, &handle, ..)` |
| 5 | `task_handler_status.rs` `on_task_status_changed(&tx, ..)` | 流程任务状态推进（P-FLOW-1B） | → `approval_port::on_status_changed(&tx, TaskStatusChange{..})` → `ApprovalOutcome` |
| 6 | `task_handler_status.rs` `cancel_on_task_cancelled(&tx, ..)`（含级联子孙逐个） | 取消任务 → 撤在跑审批（P11） | → `approval_port::cancel_pending(&tx, .., CancelCause::Cancelled)` → `CancelledCounts` |
| 7 | `task_handler_crud.rs` `cancel_on_task_deleted(&tx, ..)` | 软删任务 → 终止在跑审批（T12） | → `approval_port::cancel_pending(&tx, .., CancelCause::Deleted)` |
| 8 | `task_model_flow.rs` `use flow::node_config`（`OnComplete/OnReject/OnCancel` 的 `parse`/`as_str`） | `GET /task/{id}/flow` 回显规范化字面量 | 三个枚举**本体搬到 `business::shared::task_node_switch`**，`flow::node_config` 原样 `pub use` 再导出（`flow` 侧 100+ 处调用点一行不改） |

附带两处常量口径：

- `task_bridge::FLOW_SOURCE` → `task::dto::SOURCE_FLOW`（`biz_task.source` 的值，**表属主是 task**）；
  两份字面量必须相等这一点由 `flow::task_approval_impl::tests::flow_source_literal_matches_task_side` 钉住
  （漂移的后果是静默的：`is_flow_task` 判成 `false`，流程任务完成后再也不推进节点）。
- `task_bridge::TASK_CANCEL_COMMENT`（仅用于一条 `info` 日志）→ `approval_port::cancel_comment(cause)`，
  文案真源仍在审批域，任务侧只问不管。

**新增两个文件**：

| 文件 | 内容 | 约 |
|:--|:--|--:|
| `backend/src/business/task/approval_port.rs` | `TaskApprovalPort` trait + 注册表 + 7 个降级入口 + `PreparedAccept` 不透明句柄 | 460 行 |
| `backend/src/business/task/approval_port_tests.rs` | 注册表语义（离线 10 条）+ **依赖边闸门** | 305 行 |
| `backend/src/business/flow/task_approval_impl.rs` | 唯一实现（逐方法委派 `task_bridge`）+ 纯转换用例 7 条 | 285 行 |
| `backend/src/business/shared/task_node_switch.rs` | 三个走向枚举的真源 + 用例 3 条 | 175 行 |

## 4. 三条等价性（改造不许改变行为）

1. **事务边界一行没动**：`start_in_tx` / `on_status_changed` / `cancel_pending` 仍收 `&Transaction`，
   仍只在 `change_status` / `delete` 的**同一个事务**里执行。接缝没有把任何一步挪到事务外
   —— 这是 AGENTS 约定 11（不许留死单）的直接体现：任务状态与审批动作同生共死。
2. **`prepare_accept` 仍在事务外**（只是几次只读查询），事务里只交回不透明句柄；
   句柄内容（`AcceptStart`：预生成实例 id + 发起参数）任务侧**不解释**。
3. **四种结果逐值对应**：`NotFlowTask` / `Advanced{next_nodes}` / `Ignored{instance_status}` /
   `Conflict(ApiError)` 一一映射，`flow::task_approval_impl::tests::flow_outcome_maps_one_to_one` 钉住
   （少一个分支 = 「任务完成但流程没动」静默通过）。
4. **`Deleted` 与 `Cancelled` 两条撤销入口不许合并**：文案不同（`任务已删除…` / `任务已取消…`），
   审批中心里是不同的动作证据，`cancel_comments_are_distinct` 钉住。

### 4.1 未安装时的降级语义（模块注释那张表，逐行有用例）

| 入口 | 未安装 | 为什么是这个值 |
|:--|:--|:--|
| `instance_status_of` | `Ok(None)` | 门闸既有口径「读不到实例状态 → 不拦」 |
| `prepare_accept` | `Ok(None)` | 既有口径「没配流程 → 走原两级验收，不算错」 |
| `start_in_tx` | `Ok(Uuid::nil())` + `WARN` | 不可达，留防御，绝不 panic |
| `on_status_changed` | `Ok(NotFlowTask)` + `WARN` | 宁可流程不动，也不让用户点不动按钮 |
| `cancel_pending` | `Ok(CancelledCounts::default())` + `WARN` | 没能力就没有可撤的待办 |
| `is_start_rejected` | `false` | 交 `utils::db_error` 兜成 500 |

生产**不允许**落到降级：`bootstrap::register_task_approval_port` 装不上就
prod `exit(1)` / 其它 profile `panic`（与 `register_doc_readers` / `register_flow_hooks` / `register_hr_employee_reader` 同款）。

## 5. 依赖边证据

### 5.1 `cargo tree` 为什么看不到这条边（如实说明，同 P2-31/32/33/34/36 口径）

后端仍是**单 crate**（workspace 化 = P2-C），`task` / `flow` 是**模块**不是 package：

```text
$ cargo tree --prefix none --depth 1
cloudpivot v0.1.0 (/opt/cloudpivot/backend)
ammonia v4.2.0
async-trait v0.1.92 (proc-macro)
axum v0.8.9
...
$ cargo tree | grep -c "business::"
0
```

树里没有任何 `business::task` / `business::flow` 节点 —— 这条边在这个粒度上**无法表达**。
模块级边用仓库自带的只读工具取证（Tarjan SCC + 按域聚合，**含注释**：intra-doc link 是 rustdoc 依赖）。

### 5.2 改造后（绿，同一工具同一命令）

```text
$ python3 scripts/lint/module_cycle_report.py --edge business::task business::flow
源码根：/opt/cloudpivot/backend/src
扫描面：778 个 .rs / 节点 21 个 / 跨域有向边 61 条 （口径：仅 business::*）

边 business::task → business::flow：**不存在**（该域内无任何 crate:: 引用指向 business::flow）

$ python3 scripts/lint/module_cycle_report.py --from business::task
business::task 的出边：8 条
  → business::aiagent   (代码 6 / 注释 0)
  → business::changelog (代码 4 / 注释 0)
  → business::docpdf    (代码 2 / 注释 0)
  → business::erp       (代码 5 / 注释 0)
  → business::notice    (代码 6 / 注释 0)
  → business::pdf       (代码 2 / 注释 0)
  → business::shared    (代码 33 / 注释 1)   ← `task_node_switch` 落这儿
  → business::storage   (代码 3 / 注释 1)

$ python3 scripts/lint/module_cycle_report.py --mutual
双向边（互为依赖，2 环）：**7 对**              ← 比 P2-31 基线的 8 对少一对
  …（`flow ⇄ task` 已不在其中）
```

### 5.3 改造前（红，同一工具同一命令）

```text
边 business::task → business::flow：存在（代码 8 / 注释 3）
  [code] business/task/task_handler_common.rs:13
  [code] business/task/task_handler_crud.rs:16
  [code] business/task/task_handler_crud.rs:700
  [code] business/task/task_handler_status.rs:19
  [code] business/task/task_handler_status.rs:296
  [code] business/task/task_handler_status.rs:342
  [comment] business/task/mod.rs:60
  [comment] business/task/task_handler_status.rs:53
  [comment] business/task/task_handler_status.rs:56
  （另含 `task_model_flow.rs:13` 的 `use flow::node_config`，工具按行聚合共 8 处代码）
```

### 5.4 闸门（进 `cargo test` 的那一条）

`task::approval_port_tests::task_source_has_no_flow_references`：
扫描 `backend/src/business/task/*.rs` 全部文件，判据 `crate::business::flow`，基线 **0**（`==` 而非 `<=`，
P2-37 之后不是「只降不升」而是「必须为零」）。**扫描面硬断言**：`files >= 40`（现 48）、`bytes >= 300_000` ——
扫空时「0 命中」是假绿，先证明真的扫到了东西。

**红绿反证**（往 `business/task/` 放一个探针文件 `zz_probe_red.rs`，内含一行注释形式的直连，跑完即删）：

```text
① 闸门用例 exit = 101
   panicked at src/business/task/approval_port_tests.rs:295:5
   task/ 里出现了 1 处审批域引用（基线 0）：
   zz_probe_red.rs:2: // use crate::business::flow::task_bridge;
② 模块级报告： 边 business::task → business::flow：存在（代码 0 / 注释 1）
   [comment] business/task/zz_probe_red.rs:2
删掉探针后：① ok / 1 passed；② 边不存在
反证结论：红绿成立（两个工具都抓得住，注释形式也判红）
```

> 顺带被拦住的一次「真红」：`shared::layering_tests::shared_never_references_business_domains`
> 抓到 `task_node_switch.rs` 的**文档注释**里写了一句 `use crate::business::flow::node_config`
> —— 改成散文后转绿。这是 P2-31 立的分层护栏在工作，不是噪音。

## 6. 还剩什么（P2-38 的活，不是本卡的遗漏）

`flow → task` 仍是**代码 10 / 注释 1**（本卡前 7 / 1；+3 全部是接缝实现在审批域这一侧）：

```text
[code] business/flow/task_approval_impl.rs:44    ← 实现 import approval_port（本卡新增，方向正确）
[code] business/flow/task_approval_impl.rs:285   ← 用例断言 SOURCE_FLOW 口径（本卡新增）
[code] business/flow/flow_model_delete_tests.rs:11, :510
[code] business/flow/settle_guard.rs:64
[code] business/flow/task_bridge.rs:2250, :2251
```

P2-38 的落点建议（本卡已把地基铺好）：

1. `task_bridge` 里**写 `biz_task` 的行**（`start_in_tx` 的回写、`on_task_status_changed` 的推进、
   流程造任务）挪进一个中立的接线域（`business::taskflow` 或既有 `bootstrap` 装配层），
   由它同时依赖 `task` 与 `flow`；
2. 反方向改成「审批落终态 → 发事件」：`flow::instance_hooks`（P2-34 已有的订阅表）已经能承载，
   订阅者放接线域，`flow` 侧一行不 import `task`；
3. 届时 `task_bridge` 只剩「引擎调用 + 纯映射」，`flow → task` 才能真正归零。

## 7. 验证证据（2026-11-15）

| 判据 | 命令 | 实测 |
|:--|:--|:--|
| 编译（lib） | `cargo check --lib` | `Finished dev profile`，**零 warning** |
| 编译（含测试面） | `cargo check --lib --tests` | 零 warning |
| 静态检查 | `cargo clippy --all-targets -- -D warnings` | `Finished` 1m34s，**零告警** |
| 格式 | `cargo fmt -- --check` | 通过 |
| 全量离线单测 | `env -u ERP_DB_URL -u TASK_DB_URL -u P0FIX_DB_URL cargo test --lib -- --test-threads=1` | **5663 passed / 0 failed**（改前 5642，+21 = 本卡新增用例） |
| 真库 · 任务域 | `ERP_DB_URL/TASK_DB_URL=…172.18.10.145/cp_p237_fork_drill cargo test --lib -- business::task::` | **250 passed / 0 failed**（33.6s） |
| 真库 · 审批域（除内控会签） | 同上 `business::flow:: --skip business::flow::internal_control::db_tests` | **478 passed / 0 failed**（100.8s） |
| 真库 · 内控会签（独占进程 + 独占库） | `P0FIX_DB_URL=…/cp_p237_p0fix_drill cargo test --lib -- business::flow::internal_control` | **16 passed / 0 failed**（11.6s） |
| 真库 · **接缝等价性**（新） | 同上 flow 段内的 `cancel_through_approval_port_is_equivalent_to_direct_call` | **ok**（三列计数 1/1/1 + 同一句 `result_comment` + 待办/节点同撤） |
| 闸②（数据范围口径） | `python3 scripts/lint/scope_gate.py --root .` | PASS（156 条跳过清单逐条一致，新测试面已登记） |
| 模块级无环（本边） | `module_cycle_report.py --edge business::task business::flow` | **不存在** |
| 闸门红绿自证 | 探针 → 两条工具都判红 → 删掉转绿 | 见 §5.4 |

### 7.1 一处**必要**的测试面登记

`scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST` 新增 `business/task/approval_port_tests.rs`
（闸② 的判据是「实测跳过集合与清单逐条一致，多一个少一个都不放行」）。
同 P2-34 §5.2 的口径：新增测试面文件**必须**同步改这张表，否则闸② 每次都红。

## 8. 未做的事（如实说明）

- **没有跑 HTTP 级 e2e**（`scripts/verify/p11_flow_orphan_e2e.py` / `t12_delete_flow.sh` 需要起独立实例 + 真流程种子）。
  等价性由两条覆盖：真库接缝用例（同一夹具下与直调逐字比对）+ 现有流程/任务真库用例全绿；
  全链路的 HTTP 断言留给上线前的发版演练。
- **`flow → task` 的 3 处新增引用没有削弱**：接缝实现在审批域，P2-38 拆接线域时会整体搬走，
  届时 `task_approval_impl.rs` 的两个 `crate::business::task` 引用随文件一起移动即可。
- **`node_config` 只搬了 3 个「走向」枚举**：`DueMode` / `DueUnit` / `TaskNodeConfig` 的校验留在 `flow`
  —— 任务侧不读它们，搬过去只会把 `shared` 变成第二个上帝模块。
