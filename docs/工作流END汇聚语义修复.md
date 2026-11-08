# 工作流：END 的汇聚语义（P3-09 `B2b` 静默卡单修复）

- 卡：`t_886189f8`（父卡 `t_91bba6c3` = P3-09 全链路演练，缺陷由该卡实测发现）
- 日期：2026-11-08
- 一句话：**END 的 `join_mode` 不再参与运行期判定**；END 结束后不结束由「本轮上游还有没有在跑的节点」决定，作者不需要（也不该被要求）手工给 END 选 `OR`。
- 改动半径：`engine.rs`（约 40 行判据 + 2 个纯函数）、`flow_model_node.rs`（1 个只读查询）、`graph.rs`（**仅文档口径**）。**零迁移、零快照字段、零 `graph_hash` 变化**。

---

## 1. 现象：最常见的条件分级图会静默卡单

```
s → c1(CONDITION) --amount<10000--> n_low ─┐
                └--default--> c2 --<10万--> n_mid ─┼→ e(END, join_mode 未配置)
                                   └--default--> n_high ┘
```

`amount=5000` 走 `n_low`：

| 环节 | 修复前行为 |
|---|---|
| 发布校验（R8 `check_join_mode`） | **通过**。R8 的文档口径明确写「END 不参与汇聚门控，两个分支各自连到 END 是合法建模」，实现里 `if node.node_type == NodeType::End { continue; }` |
| 运行期 `engine::Run::step` | 对「入度 ≥ 2」的节点**无差别**走汇聚闸：`arrived < in_edges.len()` → `touch_node` 把 END 落成 `PENDING` 并 `Step::Idle` |
| 结果 | `n_low=APPROVED`、`e=PENDING`、实例 **永久 `PENDING`**；无报错、无待办、无超时提醒 —— 用户只看到「单子不结束」 |

实测证据（P3-09 演练原始输出）：`docs/evidence/20261108-p309-wf-drill/raw/00_run.txt` 场景 `B2b`、
`raw/30_db_snapshot.txt` 第 5 节 `FLW-20261108-B04C3FEA | inst=PENDING | END=PENDING`。

> 为什么存量流程没暴露：种子流程 `po_approve` / `pr_approve` 的 END 恰好都写了 `join_mode=OR`。
> **新建图的人不会知道要手动选 OR**，而前端设计器只在 `PARALLEL` / `CONDITION` 节点上暴露「汇聚方式」
> （`NodeProps.vue` 的 `supportsGateway`），END 上根本没有这个控件 —— 这个坑连「手动选 OR」的机会都没有。

---

## 2. 根因

两侧口径矛盾，且失败是**静默**的：

1. 校验侧：R8 认为 END 不参与汇聚（正确的建模口径 —— 条件分支只走一支，要求入边到齐等于要求不可能的事）；
2. 运行期：`step()` 只按「入度 ≥ 2 + `join_mode.unwrap_or(AND)`」判，END 与并行汇聚节点共用同一段代码；
3. `maybe_finish()` 只由 END 分支调用，且 `count_inflight` 会把那条 `PENDING` 的 END 行算作在跑 ⇒ 实例永远判不完。

---

## 3. 决策：为什么选「引擎侧收口」，以及为什么不是另外两条

卡面给了三条路线。**逐条评估如下（含反例）**：

| 路线 | 做法 | 评价 |
|---|---|---|
| **1（卡面推荐）引擎侧收口** | `step()` 对 `NodeType::End` 跳过汇聚闸 | 方向对，但「跳过」的两种朴素写法都会**换一个缺陷**，见 §3.1 —— 所以最终落地的是「路线 1 的收口版」：END 不再看 `join_mode`，改判「上游还有没有在跑的节点」 |
| 2 校验侧收口 | 多入边 END 必须显式 `join_mode=OR`，否则发布期 `65014` | **会制造反向缺陷**：并行分叉两支都执行，正确语义是等齐（AND）；强制 OR 等于强制「先到先结束」，另一支待办变孤儿。另外存量里「END 未配 join」的图会**发布失败**（对已发布版本无害，但改图即被拦） |
| 3 默认值收口 | 快照生成时把 END 缺省补成 `OR` | 与路线 2 同源：把所有 END 语义硬定成 OR，并行图踩同一个孤儿坑；而且它改的是**快照内容** ⇒ 已发布版本的 `graph_hash` 会变，需要评估兼容（卡面自己也在验收项里点了这条） |

### 3.1 「跳过汇聚闸」的两个朴素写法为什么不行

| END 的判据 | 条件分叉 → 同一个 END | **并行分叉 → 同一个 END** | 并行两支各接一个 END |
|---|---|---|---|
| 入边全部到达（修复前） | ❌ 另两支永远不来 → 永久 `PENDING` | ✅ 等齐 | ✅ |
| **到达即结束**（最直觉的「跳过」） | ✅ | ❌ 第一支到达就把 END 落 `APPROVED`；此时 `maybe_finish` 因另一支仍 `ACTIVE` 而不判完，等另一支签完再驱动 END 时 END 已是终态 → `step` 直接 `Idle`，**`maybe_finish` 再也不会被调到** ⇒ 实例永久 `PENDING` 且 END 显示 `APPROVED`（比原缺陷更难查） | ✅ |
| 全实例「还有没有在跑节点」 | ✅ | ✅ | ❌ 两个 END 互相等对方 → **死锁** |
| **本轮「上游可达闭包」里还有没有在跑的节点**（本卡落地） | ✅ | ✅ | ✅ |

> 关键认知：「END 不参与汇聚门控」的正确含义是 **「作者不需要给 END 选 `join_mode`」**，
> 而不是「END 一到达就结束」。END 仍然需要知道「还有没有人在往我这儿走」。

---

## 4. 落地

### 4.1 判据

END 到达时：

```text
if 本轮（round）存在状态为 ACTIVE / PENDING 的节点 key 属于 upstream(e) 且 key != e
   → 保持 PENDING（等人齐，与旧行为对外可见性一致：不建待办）
else
   → e 置 APPROVED，调 maybe_finish()
```

`upstream(e)` = 沿快照入边反向 BFS 得到的「能走到 e 的节点集合」（含 e 自身）。

### 4.2 代码位置

| 位置 | 作用 |
|---|---|
| `backend/src/business/flow/engine.rs` · `Run::step` | 入度 ≥ 2 的闸门按节点类型分派：`NodeType::End` → `end_waits_upstream`；其它 → 原来的「AND 等齐 / OR 先到先走」**一行未改** |
| `backend/src/business/flow/engine.rs` · `Run::end_waits_upstream` + 自由函数 `end_waits` | 判据本体（唯一的决策点，纯函数部分可单测） |
| `backend/src/business/flow/engine.rs` · `upstream_closure` | 上游可达闭包（迭代 BFS + 步数保险，与 `graph.rs` 的环检测同款；无递归） |
| `backend/src/business/flow/flow_model_node.rs` · `inflight_node_keys` | `SELECT DISTINCT node_key … WHERE round = ? AND status IN ('ACTIVE','PENDING')`（**必须带 `round`**，否则退回后上一轮幽灵行会让 END 永远等下去） |
| `backend/src/business/flow/graph.rs` · `check_join_mode` 文档 | 把 R8「END 不参与」与运行期口径写成同一段话，避免下次又漂移 |

### 4.3 为什么保留「上游」这个约束（而不是「全实例有在跑节点」）

并行分叉两支各接一个 END 是合法建模（`B1` 场景就是）。若 END 等「全实例所有在跑节点」，
两个 END 会互相等待：`e1` 等 `b1`、`e2` 等 `a1`，而 `a1`/`b1` 早已终态 ⇒ 双向死锁。
用「上游可达闭包」就把「与我无关的在跑节点」排除掉了。

---

## 5. 兼容性：为什么零迁移、零哈希变化

- 快照结构、canonical JSON、`graph_hash` 计算路径**一行未改**（`graph.rs` 只改了 `check_join_mode` 的文档注释）。
- 因此已发布版本的 `graph_hash` 不变，无需重发、无需重算；也不需要「兼容已发布快照」的特殊分支 ——
  **判据在运行期**，老快照（END 为 `NULL` / `'AND'` / `'OR'`）一律按新口径跑。
- 实测基线：修复前后各取一次 `biz_flow_def_version` 的 `(flow_code, version_no, graph_hash)`，
  **18 条种子 / 存量流程逐行一致**（`p309_*` 是演练自建流程，快照里嵌当次随机的角色/用户 UUID，
  每次跑本来就不同，与本次改动无关）。

---

## 6. 存量影响与恢复

### 6.1 影响面（修复前会卡单的图形）

任何「**条件分叉或并行分叉后汇到同一个 END，且作者没在 END 上手选 `OR`**」的图。前端设计器不给 END
暴露「汇聚方式」控件，所以这几乎是**默认路径**：只要 END 的入边 ≥ 2，就一定会卡。
卡单是静默的（实例 `PENDING`、END `PENDING`、无报错、无待办），只能靠 `/stats/node-bottleneck` 或对账发现。

### 6.2 线上（144 生产库）实测：当前 0 条卡单

只读查询（`biz_flow_instance` 114 条、`PENDING` 1 条）：

```sql
-- 静默卡单的识别口径：实例 PENDING + 有 PENDING 的 END 行 + 没有任何其它在跑节点 + 零待办
select count(*) from biz_flow_instance i
 where i.status = 'PENDING'
   and not exists (select 1 from biz_flow_instance_node n
                    where n.instance_id = i.id and n.status in ('ACTIVE','PENDING') and n.node_type <> 'END')
   and exists (select 1 from biz_flow_instance_node n
                where n.instance_id = i.id and n.status = 'PENDING' and n.node_type = 'END')
   and not exists (select 1 from biz_flow_task t where t.instance_id = i.id and t.status = 'PENDING');
```

结果：`pending_total=1`、`pending_silent_stuck=0`、`instance_total=114` ⇒ **无需数据修复**。

### 6.3 万一存在（或将来在旧版本上产生）

修复是**运行期**的，但它只在实例「有新流转」时被驱动。已经彻底卡死（无待办、无定时任务驱动）的旧实例
不会被自动唤醒。恢复手段（按侵入从小到大）：

1. **管理员终止**（`CANCELLED`）后重新发起 —— 推荐，最干净，语料可查；
2. 若必须保留单据，人工按上面的识别 SQL 核对「确实无在跑节点 + END 入边都已到达」后，
   把 END 行改 `APPROVED` 并按 `maybe_finish` 的口径把实例改 `APPROVED`（同事务，留审计）；
   **不要**在还有其它在跑节点时这么做（会造出孤儿待办）。

---

## 7. 验证证据

| 项 | 结果 | 证据 |
|---|---|---|
| 纯单测（新加 3 条：闭包覆盖 / 不跨 END / 只在有存活上游时等） | 18/18 passed | `cargo test --lib business::flow::engine::tests` |
| flow 模块全量单测 | **504 passed / 3 failed（既有失败，与本卡无关）** | 失败全在 `business::flow::linkage::{tests_wiring, admin}`：`触发点计数守卫断言 15/16，实际 16/17`。`git diff HEAD -- backend/src/business/flow/linkage/` 为空、`git show HEAD:…/linkage/spec.rs` 里 `wired: true` 已是 16 ⇒ **HEAD 上就红**，属另一张卡（新增第 16 个联动触发点未同步这两个硬编码计数） |
| clippy（`--lib`，`-D warnings`） | 本卡 3 个文件 0 warning | 唯一 error 是 `erp/sales_order_model.rs::update_order` 参数 8/7，属别人未提交的改动 |
| P3-09 全链路演练（17 场景） | **PASS=243 FAIL=0** | `docs/evidence/20261108-p309-wf-drill/raw/00_run_fix.txt`；`B2b` 由「缺陷复现」改为「必须 APPROVED」并通过，新增 `B2c`（并行两支汇同一个 END 必须等齐）12 条断言全绿 |
| 存量 `graph_hash` | 18 条非演练流程逐行一致 | 见 §5 |
| 二进制 | `/root/p309/bin/cloudpivot`，md5 `7df255ef3b8f3dacbd5c207b569e9000`（`38b813c` + 本卡改动的 debug 构建） | 演练实例 `:18091`，跑完 `监听行数=0 / health=000` |

---

## 8. 已知边界（明确写死，避免下次再问）

1. **END 的 `join_mode` 现在完全无效**（`AND` / `OR` / 空值都一样）。老图里显式配的 `OR` 也不会被读 —— 语义等价于「等所有上游跑完」，对条件图没有可见差异。
2. **并行分支永远不完成**（例如某一臂被外部系统卡死）时，END 会一直等 —— 与修复前 AND 汇聚的行为一致；这不是本卡引入的，改由节点级 `timeout_hours` / 人工终止处理。
3. **`CC` / `NOTIFY_NODE` 是非阻塞节点**（进入即终态），不会让 END 多等（它们本来就不会长期停在 `ACTIVE`）。
4. **退回（`round + 1`）**：`inflight_node_keys` 带 `round` 过滤，上一轮的幽灵行不参与判定；退回时 `cancel_active_nodes` 会把旧轮次的 `ACTIVE`/`PENDING` 节点（含那个卡住的 END 行）置 `CANCELLED`。
5. 本卡**不改前端**：设计器本来就没在 END 上暴露「汇聚方式」，无需改；`frontend/.../graph.ts` 里给 END 填的 `'AND'` 现在是无害占位（将来可清，不影响行为）。

---

## 9. 回滚

纯代码回滚，无数据/迁移：
`git revert <本卡提交>` → 重新构建 → 重启实例。已按新口径结束的实例不受影响（终态不回退）；
回滚后**新**产生的条件分叉图会重新落回「END 等人齐 → 卡单」，所以回滚必须同时停用这类图。
