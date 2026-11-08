# P3-03 节点兜底人（五层降级第 ③ 层）· 交付证据

卡面：`t_56fdb104` — P3-03 节点兜底人：升级链走完仍无人 → `biz_flow_node.fallback_user_id / fallback_role_id`
日期：2026-11-08 · 主机：`172.18.10.99`（工作仓 `/opt/cloudpivot`）· 二进制：`/root/p303/bin/cloudpivot`

---

## 1. 这张卡解决什么

P3-02 已经能在「自审剔光 / 角色 0 人」时沿 `sys_role.escalation_role_id` 上溯找上级角色。
但**角色只有制单人一人、且该角色没有上级**（小公司：销售员就是老板，老板上面没人）时，
升级链走完仍然是 0 人 → `422 / 65018` → **单据永远发不出去**（ARCH-8 场景 C）。

本卡增加第 ⑦ 步：升级链之后、`empty_assignee` 之前的**节点兜底人**。

```
① 停用剔除 → ② ROBOT 过滤 → ③ 去重 → ④ 自审过滤
      → ⑥ 角色升级链（P3-02） → ⑦ 节点兜底人（本卡） → ⑤ 空审批人策略
```

三条纪律（卡面硬要求，代码里逐条落在注释与单测上）：

1. **兜底不是免审**：兜底人同样过停用 / ROBOT / 自审过滤；兜底人 = 发起人时 **422**
   （报文点名「兜底人即发起人，内控不允许自审」），不放行也不静默换人；
2. **配错要吵（`65042`）**：兜底人不存在 / 已停用 / 是机器人而节点没开 `allow_robot` /
   兜底角色已删 / 兜底角色无可用成员 —— 一律显式失败，不静默当成「没配兜底」；
3. **二选一**：`fallback_user_id` 与 `fallback_role_id` 不许同时配（DB `CHECK` +
   保存期 400 + 发布期 R22 三道）；要多人兜底就配角色。

## 2. 决策落地

| 决策 | 落地 |
|---|---|
| **D3** 错误码 `65042` | `error_code::FLOW_FALLBACK_INVALID`，登记进 65xxx 唯一性断言表（36 → 37 个码）；65043-65044 未占用 |
| **D4** 迁移号 `V0334` | 动手前核对：`migrations/business` 最大号当时为 `V0333`（P3-02 交付），`V0335` 是并行卡片后加的、与本卡不冲突 |
| `assigned_by_rule` 一次加够四种值 | `ROLE` / `ESCALATED` / `FALLBACK` / `DELEGATED`（P3-04 复用同一列，**不再加列**），带值域 CHECK |
| 找人逻辑只在 `assignee.rs` | `apply_fallback()` + `dispatch_hint()` 都在 `assignee.rs`；引擎只读 `Finalized` |
| 内控在任何降级路径上不许被绕过 | 兜底人候选同样过 `filter_candidates`（停用/ROBOT/自审）；引擎的内控 partition 仍在落待办之前 |

## 3. 改动面（19 个文件 / +1396 −58，另加 2 个迁移 + 本证据目录）

**迁移**（`backend/migrations/business/`）
- `V0334__flow_node_fallback.up.sql` / `.down.sql`
  - `biz_flow_node.fallback_user_id` / `fallback_role_id`（可空，无默认）
  - `ck_biz_flow_node_fallback_exclusive`：二选一
  - 两个部分索引（`... WHERE fallback_* IS NOT NULL`，排障「这个人为啥总收到单」）
  - `biz_flow_task.assigned_by_rule varchar(20)` + 值域 CHECK + `idx_biz_flow_task_by_rule`
  - up 自带自检 `DO $$`（列/约束缺失直接 RAISE），down 逆序删索引→约束→列

**后端**
- `assignee.rs`：`Fallback` / `FallbackOutcome` / `apply_fallback()` / `parse_fallback_ids()` /
  `dispatch_hint()` / `ASSIGNED_BY_*` 常量；`Finalized` 新增 `fallback` + `assigned_by_rule`；
  `resolve()` 插入第 ⑦ 步；`65018` 报文改为「升级链 + 兜底人」双段提示
- `graph.rs`：`NodeSpec` 两列、`SnapshotNode` 两列（`skip_serializing_if` 保 hash 稳定）、
  发布期 **R22**（二选一 / 兜底人 = 指定审批人 / 配在无审批语义节点上）
- `dto.rs`：`GraphNodeReq` / `GraphNodeRes` 两字段 + `parse_fallback()`（保存期 400）
- `flow_model_base.rs` / `flow_model_graph.rs` / `flow_model_instance.rs`：`NODE_COLUMNS`、
  读图、整图保存、`NewTask`、`insert_task`、`TASK_COLUMNS`（新增 `assigned_by_rule`）
- `engine.rs`：捕获 `fallback` / `assigned_by_rule`，PENDING 行写规则值，
  内控 422 报文带 `dispatch_hint`
- `handler.rs` / `settle_guard.rs` / `notify_node.rs` / `internal_control.rs` / `no_assignee.rs`：
  随字段新增的构造点（不改语义）
- `error_code.rs`：`65042` + 码表断言 36→37

**前端**
- `flow-api.ts`：`FlowNode` / `FlowNodeReq` 两字段、`FlowTask.assigned_by_rule`
- `NodeProps.vue`：节点面板新增「兜底审批人」= 方式下拉（不兜底 / 指定兜底人 / 指定兜底角色）
  + `UserSelect` / 角色下拉；切换方式时**主动清空另一种**（不给「两个都填」的机会）
- `def/edit.vue`：保存透传；`graph.ts`：新节点默认两个 `null`

## 4. 验收结果（三份原始输出都在 `raw/`）

### ① 迁移升级：`raw/10_upgrade.txt` — **PASS=23 FAIL=0**

```
克隆 cloudpivot_p302_e2e（停在 V0333）→ cloudpivot_p303_upgrade
首次启动 → 应用 1 个迁移              （V0334，applied_seq 267）
二次启动 → 应用 0 个迁移              （指纹校验通过）
落库：兜底两列 / assigned_by_rule(varchar(20)) / 二选一 CHECK / 值域 CHECK / 3 个索引
负例：同时配两个 → 被 ck_biz_flow_node_fallback_exclusive 拒绝
down.sql → 列 / 约束 / 索引全数消失（版本记录保留，与其它卡同口径）
up.sql 重放 → 全部回来
```

### ② 真 HTTP 场景：`raw/20_e2e_scenarios.txt` — **PASS=61 FAIL=0**

隔离实例：空库首启 **267 个迁移**、端口 `18303`、库 `cloudpivot_p303_e2e`。

**场景 C（本卡主验收）**——角色 1 人（=制单人）+ 该角色无上级 + 节点配兜底人：

```
✅ 场景 C 实例状态 = PENDING
✅ 场景 C PENDING 待办行数（兜底人 1 人） = 1
✅ 场景 C 待办审批人 = 兜底甲
✅ 场景 C assigned_by_rule = FALLBACK          ← 卡面要求的 DB 断言
✅ 场景 C 兜底行不带升级留痕（escalated_from_role 应为空） = 0
✅ 场景 C 原 SKIPPED 留痕行仍在（制单人） = 1
✅ 兜底人审批 → 200 / 场景 C 单据（实例）生效 = APPROVED
[留痕] P3-03 兜底甲 APPROVED rule=FALLBACK | P3-03 制单人 SKIPPED rule=-
[API]  [('P3-03 制单人','SKIPPED',''), ('P3-03 兜底甲','APPROVED','FALLBACK')]
```

**其余场景**

| 场景 | 断言 |
|---|---|
| 兜底角色 3 人 | 3 行待办全 `FALLBACK`；批 1 人仍 `PENDING`（APPROVAL 现行为=全员签，ALL/ANY 归 P3-06）；全批完 → `APPROVED` |
| 兜底人 = 发起人 | `422 / 65018`，报文含「兜底人即发起人」+「内控不允许自审」，**零半截实例** |
| 未配兜底 | `422 / 65018`，报文含「**升级链与兜底人均未配置**」，`data` 结构化诊断仍在 |
| 兜底人不存在 | `422 / 65042`，报文说清「不存在（已被删除或 id 非法）」 |
| 保存期二选一 | 两个都配 → `400`，报文「只能二选一」 |
| 发布期 R22 | 兜底人 = 指定审批人 → 草稿可存、发布 `400 / 65042`（报文「同一个」） |
| 回归 A（3 人角色 2 人签） | 2 行待办全 `ROLE`、无人被升级、制单人 `SKIPPED` |
| 回归 B（升级链） | 2 行待办 `ESCALATED`、`escalated_from_role=P303_CHAIN`、`escalate_reason=SKIP_SELF_APPROVAL` —— **兜底不抢升级链的活** |
| 回归 F（FORBID） | `422 / 65019`，报文含 `self_approval=FORBID`，零半截实例 |

### ③ 静态检查 + 单测 + typecheck：`raw/30_checks.txt` — **PASS=10 FAIL=0**

```
✅ 14 个 Rust 文件 fmt 干净（只对本卡文件跑 --check，不格式化别人的在途改动）
✅ V0334 恰好 up/down 两个文件；全局 4 位版本号无重复
✅ clippy --lib -D warnings -A too_many_arguments：零告警（本卡零新增）
   既有基线（raw/30_clippy_baseline.txt）：erp/sales_order_model.rs update_order 8/7 —— P2-02 起就在，本卡未改该文件
✅ 真库用例没有静默 SKIP = 0（ERP_DB_URL 指向 cloudpivot_p303_upgrade + --nocapture）
✅ 单测 6 组全绿、无 FAILED（raw/30_unit.txt）
✅ pnpm typecheck 退出码 = 0（raw/30_typecheck.txt）
✅ backend/src/system|support 改动数 = 0
```

新增单测（`raw/30_unit.txt`，共 12 条）：

| 组 | 条数 | 内容 |
|---|---|---|
| `graph::task_node_rule_tests::r22_*` | 4 | 二选一正例 / 同人反例 / 非审批节点反例 / 快照未配兜底时**不出现新键**（graph_hash 不变） |
| `assignee::fallback_tests` | 2 | `dispatch_hint` 四种组合（含「一级都没走 = 升级链没配」）/ 快照字符串解析（非法 UUID 不算没配） |
| `assignee::fallback_db_tests` | 6 | ①升级链走完 + fallback_user（`assigned_by_rule=FALLBACK`）②兜底人=发起人 422 ③兜底角色 3 人 ④未配兜底含「均未配置」 ⑤用户不存在 65042 ⑥已停用 65042 |
| 回归 | 11 | P3-02 升级链 10 条 + 码表唯一性 1 条 |

### ③b 全量 flow 模块回归：`raw/40_flow_suite_regression.txt`

`cargo test --lib business::flow::`（真库）→ **452 passed / 3 failed**。

3 条红全部是 `business::flow::linkage::*` 的「联动触发点接线清单」断言：P2-03（`9cf6cc1`）给
`erp/purchase_return_handler.rs` 新增了 `PURCHASE_RETURN` 调用点，但没把期望集合 +1。
**本卡提交 `70cfd8e` 没碰 `linkage/` 与 `purchase_return_handler.rs`**（`git show --stat` 与
`git status` 均可复核），与 P3-02 交付时记录的 `known_preexisting_failures` 是同一批 3 条。
因为门禁要的是「本卡零新增」，这 3 条不计入本卡回归。

## 5. 复跑

```bash
cd /opt/cloudpivot/docs/evidence/20261108-p303-fallback/scripts
export PGPASSWORD=<本机测试库口令>      # 仓库里不写明文
cargo build --manifest-path /opt/cloudpivot/backend/Cargo.toml   # 或复用 /root/p303/bin/cloudpivot
bash 10_upgrade_migration.sh   # 迁移升级/回滚（PASS=23）
bash 20_e2e_scenarios.sh       # 真 HTTP 场景 C + A/B/F 回归（PASS=61）
bash 30_checks.sh              # fmt/clippy/单测/typecheck（PASS=10）
```

脚本约定（与 `docs/evidence` 其它卡一致）：

- 实例环境从**在跑的实例** `/proc/<pid>/environ` 抄（JWT / 主密钥 / admin 口令），
  只覆盖库名与端口；环境文件写到 `/root/p303/*.env`，**不落仓库**；
- 实例按「监听端口反查 pid」管理（`setsid` 会 fork，`$!` 不可靠）；
- 原始实例日志在 `/root/p303/20_e2e_instance.log`（大且含 SQL 参数），
  仓库里只留 `raw/20_e2e_instance_trace.txt` 摘录。

## 6. 本轮踩到的坑（已写入 skill）

| 坑 | 现象 | 解法 |
|---|---|---|
| 场景 B（升级链）与场景 C（兜底）抢同一个夹具角色 | 给 `P303_MAKER` 配了上级 → 场景 C 的前提「升级链走完仍无人」不成立，兜底根本没被触发 | 两个场景**各用各的角色**：`P303_MAKER`（无上级）+ `P303_CHAIN`（有上级，同一个人是其唯一成员） |
| `change_note` 里拼 `extra` JSON | `"change_note":"…（SKIP / ERROR, "fallback_user_id":"…"）"` → `Failed to parse the request body as JSON` | 附加字段只拼在**节点对象**里，绝不拼进字符串字面量 |
| 发布校验的 HTTP 状态 | 以为 R22 → 422，实际发布校验统一 `400`（业务码 65042 区分） | 断言写成 `400 + code=65042`（`handler.rs:160` 的口径） |
| APPROVAL 节点的会签语义 | 以为 `approval_mode=ANY` 会「一人签即过」，实际 APPROVAL 走全员签（3 人必须批 3 次） | 断言改为「批 1 人仍 PENDING → 全批完 APPROVED」，ALL/ANY 归 P3-06 |
| 「升级链已走完」被误用 | 角色**没有上级**时 `escalate()` 也返回 `Exhausted`（`levels` 为空），报文写成「升级链已走完：…」会让用户去找一条不存在的链 | `dispatch_hint` 的判据改成 **`levels` 非空才算走过**；一级没走 = 链没配 → 「升级链与兜底人均未配置」 |
| 迁移号重复检查 | `ls migrations/*/ \| cut -c1-5 \| uniq -d` 把每个版本都判成重复（up/down 共享版本号） | 只按 `*.up.sql` 统计 |
| 真库单测静默跳过 | `cargo test` 不设 `ERP_DB_URL` 时打印 SKIP 但用例「通过」= 假绿；单测成功时 stdout 还被吞 | 必须 `ERP_DB_URL=… --nocapture`，并在脚本里断言 `SKIP: 未设置 ERP_DB_URL` 计数为 0 |
| 证据脚本里的 `Authorization` 头 | P3-02 提交的 `lib.sh` 被脱敏成 `Bearer ***`，直接复跑必 401 | 本卡 `lib.sh` 用 `$token` 插值，脱敏只针对 `raw/` 输出（不脱敏脚本） |

## 7. 未做 / 留给后续卡

- **ALL/ANY 语义**：兜底角色 3 人当前按节点现行为落 3 行、APPROVAL 全员签；按人/按比例签由 **P3-06** 定；
- **委托（`DELEGATED`）**：列与值域已就绪，逻辑在 **P3-04**；
- **从节点后台「模拟」兜底**：预演接口（`POST /flow/node/task/preview`）预演的是任务节点，
  不含兜底；审批类节点的兜底预演未纳入本卡范围。
