# P2-38：`flow` 去掉对 `task` 的直接依赖（`task ⇄ flow` 2 环彻底解除）

- 卡号：t_69240409（P2-B 解环第 8 张）
- 前置：P2-37（t_c2ecac3f，`task → flow` 归零，提交 `82618985`）；P2-01~26 拆目录已完成
- 结论：**`business::flow → business::task` 不存在**（代码 0 / 注释 0，改前 代码 10 / 注释 1）；
  同时 `business::task → business::flow` 仍为 0（P2-37 的成果未被破坏）

## 1. 问题：为什么这条边摘不掉

`flow` 与 `task` 之间有四条硬连接，**每一条都同时读写两边的表**：

| 时机 | 写谁 | 读谁 | 原落点 |
|---|---|---|---|
| 任务提交验收 | `biz_flow_instance` / `biz_flow_task`（建审批单） | `biz_task`（回写 `approval_instance_id`） | `flow::task_bridge::start_in_tx` |
| 审批结论落地 | `biz_task`（状态 / 时间线 / 父链级联） | `biz_flow_instance`（结论 / 意见） | `flow::task_bridge::settle` |
| 任务状态变更 | `biz_flow_instance_node` / 实例（节点推进 / 终止） | `biz_task`（来源实例 / 节点 / 轮次） | `flow::task_bridge::on_task_status_changed` |
| 流程节点激活 | `biz_task`（造任务 + 协作人 + 变更日志） | —（流程侧算好的字面量） | `flow::task_node::spawn` |

P2-37 把「任务 → 流程」的四条硬连接改成 `task::approval_port` 的注册表，但**实现仍需
既能调流程引擎、又认识任务表**，于是落回 `flow/task_approval_impl.rs` —— `flow` 又多出
2 行任务域引用（`task_approval_impl.rs:44,285`）。

所以根子不是「谁 import 谁」，而是**「既写 A 表又写 B 表的代码该住哪儿」**：
放 `flow` 则 `flow → task`，放 `task` 则 `task → flow`，怎么搬都还在环里。

## 2. 解法：中立接线域 `business::taskflow`

新增第三个域，**两边都认识、两边都不认识它**：

```text
taskflow ──→ flow（引擎 / 实例 / 节点 / 门闸 / settle_guard 的留痕通道）
taskflow ──→ task（任务表 / 状态机 / 任务号 / 协作人）
flow      ──✗ task      （只经 flow::task_port 的注册表，实现由 bootstrap 注入）
task      ──✗ flow      （只经 task::approval_port 的注册表，实现由 bootstrap 注入）
flow      ──✗ taskflow  （闸门钉住：否则只是把 2 环换个名字）
task      ──✗ taskflow  （同上）
```

`taskflow` 是**叶子**：只有 `bootstrap` 在启动期把它的两个实现装进两边的注册表
（`taskflow::install_ports()`，装不上就 prod `exit(1)` / 其它 profile `panic`）。

### 2.1 文件归属（搬家清单，语义一行未改）

| 原文件 | 新落点 | 内容 |
|---|---|---|
| `flow/task_bridge.rs`（2831 行） | `taskflow/bridge.rs` | 写 `biz_task` 的全部段落：`settle` / `cascade_after_flow` / `callback_task_row` / `prepare_accept` / `start_in_tx` / `on_task_status_changed` / `node_action_of` / `start_task_node_approval` / `cancel_on_task_*` / `on_instance_settled` / `on_task_approval_settled` / `settle_flow_node` |
| `flow/task_approval_impl.rs` | `taskflow/approval_impl.rs` | `task::approval_port` 的唯一实现（P2-37 建的适配层） |
| `flow/task_node.rs` 的 ⑥⑦⑧（取号 / 插入 / 协作人 / 变更日志） | `taskflow/spawn.rs` | `FlowTaskPort::spawn_task` 的实现 + 幂等预查 |
| `flow/flow_model_delete_tests.rs` | `taskflow/delete_cancel_tests.rs` | T12 / P11 真库用例（它们测的就是搬走的那条路径） |
| `flow/settle_guard.rs` 的 `check_task` / `gate_task` / `TASK_AWAITING` | `taskflow/bridge.rs` | 任务侧的判定要用任务状态机的类型；留痕仍走 `flow::settle_guard::apply`（同一套口径） |
| — | `flow/task_port.rs`（新） | **调用方定义**的接缝：`FlowTaskPort` trait + `SpawnTaskSpec` + 注册表 + 降级语义 |
| — | `flow/task_independence_tests.rs`（新） | 闸门：`flow` 下不许出现任务域路径（基线 0，扫 47 文件 / 2.0 MB） |

搬完之后 `flow/task_bridge.rs` 只剩 **60 行**：`instance_status_of`（读自己的实例表）
+ 两个常量再导出。`flow` 里所有对 `biz_task` 的**模块级引用**归零（原始 SQL 不构成模块边，
且读写已经不在 `flow` —— 只有 `instance_status_of` 读 `biz_flow_instance`）。

### 2.2 `flow::task_port`：为什么 trait 定义在调用方

与 P2-32（`flow::doc_reader`）/ P2-35（`hr::employee_reader`）同侧：这里传的是**命令**
（「给我造一张任务」），不是读别人的数据。trait 定义在 `task` 就等于 `flow` 必须
`import task`，边还在。

DTO（`SpawnTaskSpec`）**只放字面量**：负责人解析、模板渲染、截止日 / 优先级计算全部在
`flow` 侧算完（`assignee::resolve` 只有一份，不许分叉）。

### 2.3 降级语义（未安装实现时）

| 入口 | 未安装 | 为什么是这个值 |
|---|---|---|
| `task_of_node` | `Ok(None)` | 当作「还没造过」，让 `spawn_task` 去撞唯一索引（真冲突仍是 `23505` 幂等） |
| `spawn_task` | `Ok(SpawnOutcome::Failed(500))` | 造不出任务**不能**让节点静默 `ACTIVE` —— 那是「有节点、没活干」的死单；失败经引擎回滚整笔发起 |
| `on_instance_settled` | `None` + `WARN` | 跑在审批事务提交之后，红线：不许因任务侧失败回滚审批单 |

生产不允许落降级：`bootstrap` 装不上就启动失败。

## 3. 边归零的证据（可复核命令）

```bash
# ① flow → task：不存在（改前：代码 10 / 注释 1）
python3 scripts/lint/module_cycle_report.py --edge business::flow business::task
#   边 business::flow → business::task：**不存在**（该域内无任何 crate:: 引用指向 business::task）

# ② flow → taskflow：不存在（防止「把环换个名字」）
python3 scripts/lint/module_cycle_report.py --edge business::flow business::taskflow
#   边 business::flow → business::taskflow：**不存在**

# ③ task → flow：不存在（P2-37 的成果仍在）
python3 scripts/lint/module_cycle_report.py --edge business::task business::flow
#   边 business::task → business::flow：**不存在**

# ④ 反方向存在且只单向（taskflow 是叶子）
python3 scripts/lint/module_cycle_report.py --from business::taskflow
#   → business::changelog（2）/ business::flow（16/注释 1）/ business::task（9）
```

`cargo tree` 证不了这条边：`backend` 是**单 crate**（workspace 化是 P2-C 的活），
`cargo tree | grep -c 'business::'` = 0 —— 模块级依赖它画不出来。这是 P2-31~37 一贯的口径，
本卡沿用 `module_cycle_report.py` + 闸门双层取证。

### 3.1 闸门（进 CI，防回退）

- `flow::task_independence_tests::flow_source_has_no_task_references`
  基线 **0**（`==` 而非 `<=`），扫描面硬断言 `files >= 40` / `bytes >= 300_000`，
  **注释与 intra-doc link 也算**（它们是「下一条直连」的温床）。
- `flow::task_independence_tests::flow_does_not_depend_on_the_wiring_domain`
  `flow` 不许 import 接线域（否则 2 环只是改名）。
- `task::approval_port_tests::task_source_has_no_flow_references`（P2-37 遗留）仍为 0。

红绿反证（两条都实测）：

- 绿：提交时 0 条；
- 红：往 `flow/task_port.rs` 的文档注释里写一行完整任务域绝对路径 → 闸门立刻失败并打印
  `src/business/flow/task_port.rs:6 ...`；`module_cycle_report --edge` 同口径报「存在（代码 0 / 注释 1）」。

## 4. 验证（真实执行输出）

```bash
cd backend
cargo check --lib                       # Finished dev profile · 零 warning
cargo fmt                               # 已格式化
cargo clippy --all-targets -- -D warnings   # 零告警（含 tests/*.rs 集成用例）
cargo test --lib -- --test-threads=1    # 5677 passed; 0 failed（离线，无库）

# 真库（99 → 145:5432，克隆库 cp_p238_taskflow ⊂ cloudpivot_test 182 表）
ERP_DB_URL=… TASK_DB_URL=… SYS_DB_URL=… cargo test --lib business::taskflow -- --test-threads=1
#   52 passed（含 T12/P11 的撤审批真库用例 8 条 + P2-37 接缝等价性用例）
ERP_DB_URL=… TASK_DB_URL=… SYS_DB_URL=… cargo test --lib business::flow::     -- --test-threads=1
#   432 passed（审批引擎 / 节点 / 门闸 / 超时 / 内控）
ERP_DB_URL=… TASK_DB_URL=… SYS_DB_URL=… cargo test --lib business::task::     -- --test-threads=1
#   250 passed（任务状态机 / 门闸 / 审批接缝）

python3 scripts/lint/scope_gate.py --root . --baseline 21    # PASS：0 处；SKIP_MANIFEST 158 条逐条一致
python3 scripts/lint/path_refs_gate.py --root .              # PASS（898 脚本，0 处失效路径）
```

## 5. 事务边界（AGENTS 约定 11）—— 一行未动

| 入口 | 事务 | 失败后果（照旧） |
|---|---|---|
| `start_in_tx` | 调用方 `change_status` 的事务（收 `&Transaction`） | 审批单建不出 → 任务状态整笔回滚 |
| `on_task_status_changed` | 同上 | 流程推不动 → 任务状态一起回滚 |
| `cancel_pending` | 任务删除 / 取消的事务 | 撤不掉 → 整笔回滚（不留孤儿待办） |
| `spawn_task` | 引擎发起 / 节点激活的事务 | 造不出任务 → 整笔回滚（0 任务 0 实例） |
| `settle` / `on_instance_settled` | **事务外自助**（审批已提交） | 失败只 `WARN`，父链级联另开一笔（红线：不回滚审批结论） |

搬家只换归属域，**没有**把一个事务内调用改成事务后 fire-and-forget。

## 6. 遗留 / 后续

- `FLOW_SOURCE`（流程侧写入）与 `task::dto::SOURCE_FLOW`（任务侧判断）仍是两份字面量：
  P2-37 加的相等性用例仍在跑（`taskflow::approval_impl::tests::flow_source_literal_matches_task_side`），
  收口到一份可以让 `biz_task.source` 的取值只归表属主 —— 但那会动 `V0177` 的注释口径，留独立卡。
- HTTP 级 e2e（`scripts/verify/*/p11_flow_orphan_e2e.py` / `t12_delete_flow.sh`）留给上线前发版演练：
  本卡只跑 lib 级真库用例。
