# P3-08 子流程节点（SUBPROCESS）· 实测证据

> 卡面：`P3-08 子流程节点：SUBPROCESS + 父子实例 + 结算回调（ARCH-5 §4.14）`
> 规格：`docs/architecture/ARCH-5-工作流引擎设计.md` §4.14（1822–1846 行）+ §1.1「层级子流程」+ §18.3
> 结论：**全部验收项 PASS**（迁移 PASS=18 / 真 HTTP e2e PASS=77 FAIL=0 / 代码门禁见 raw/30_checks.txt）

---

## 1. 交付了什么（先看这张表）

| 层 | 改动 | 位置 |
|---|---|---|
| 迁移 | **V0341**：`node_type` 9 → 10（含 `SUBPROCESS`）、`biz_flow_node.subprocess_def_id`（FK RESTRICT）、`ck_biz_flow_node_subprocess_config`、**`ck_biz_flow_node_config_partition` 同步扩 `sub`**、`biz_flow_instance.parent_instance_id` + `parent_node_key`（成对 CHECK）+ 部分索引 | `backend/migrations/business/V0341__flow_subprocess_node.{up,down}.sql` |
| 图模型 | `NodeType::Subprocess`（`as_str` / `parse` / `ALL: [NodeType; 10]` / `is_single_exit`）、`NodeSpec.subprocess_def_id`、R17 覆盖 SUBPROCESS、**R27**（静态半边） | `backend/src/business/flow/graph.rs` |
| 子流程模块 | 新增 `flow::subprocess`：`SubConfig`+`parse_config`（未知键报错）、`nesting_depth`（纯函数）、`validate_publish`（查库半边 R27）、`spawn_child`（事务内建子实例）、`on_instance_settled`（父子结算回调） | `backend/src/business/flow/subprocess.rs` |
| 引擎臂 | `NodeType::Subprocess`：同事务 `engine::start_in_tx` 建子实例 → 父节点 `ACTIVE` → 子实例结算回调 → 父节点 `APPROVED` 继续 / `ABORT` 终止；`settle_subprocess` 同时服务「事务内立即完成」与「事务外回调」两条路径 | `backend/src/business/flow/engine.rs` |
| 父侧回调 | `engine::resume_parent_after_subprocess`：两把幂等闸（父实例非 PENDING / 父节点已终态）+ 行锁 + `revision` CAS + 终态清任务 | 同上 |
| 发布校验 | `publish` 追加 `validate_publish`（定义存在 / 已发布 / 不自递归 / 深度 ≤ 3） | `backend/src/business/flow/handler.rs` |
| 回调接线 | `approve` / `reject` 两个 handler 在提交后调 `subprocess::on_instance_settled` | 同上 |
| 错误码 | **65045**（子流程 def 非法/未发布）、**65046**（自递归/深度超限）、**65047**（子实例发起失败），已登记唯一性断言表（65xxx 段 42 个码） | `backend/src/shared/error_code.rs` |
| DTO/持久化 | `GraphNodeReq/GraphNodeRes.subprocess_def_id`、节点行读写列、快照不含该列（见 §5 决策） | `dto.rs` / `flow_model_graph.rs` / `flow_model_base.rs` |
| 前端 | `FlowNodeType` 加 `SUBPROCESS`；`flow-node-caps.ts` 能力表（`needsConfig='sub'`、`blocksOnHuman=true`）；`flow-node-view.ts` 图标/副标题/class；`graph.ts` NODE_META；**`SubprocessNodeConfig.vue`** 属性面板（子流程选择器 / 版本策略 / 失败动作 / 提示任务 / 双向映射）+ `subprocess-node-config.ts` 纯函数；画布拖得出来、配得了、存得下 | `frontend/src/...` |
| 单测 | 后端 `flow::subprocess` 10 例（配置默认值/未知键/映射方向/自递归/间接环/深度 3 通过 4 拒/分叉/终态白名单/标题截断）；前端 5 例（默认配置、分区不覆盖、映射行收敛） | `subprocess.rs`、`frontend/tests/unit/flow-subprocess-node.test.ts` |

**迁移号**：动手前 `ls backend/migrations/business | grep -o '^V[0-9]*' | sort -u | tail` 实测最大 **V0340**（P3-07 的 SoD 开关）
⇒ 本卡取 **V0341**（P3-06 占 V0337/V0339、P3-05 占 V0338、P3-07 占 V0340）。

---

## 2. 迁移：升级库只应用 1 个 / 重启 0 个 / down 可回滚

原始输出：`raw/05_migration.txt`、`raw/10_restart.txt`（脚本 `scripts/10_restart.sh`，PASS=18 FAIL=0）

```text
=== 升级库 cloudpivot_p308_upg（由 P3-07 的 V0340 库 createdb -T 克隆，11 张在跑实例）首次启动 ===
NOTICE: V0341 自检通过：node_type 10 值 + config.sub 形状 + 分区 CHECK + 父子实例两列 + 部分索引
数据库迁移完成: 应用 1 个迁移

=== 同库重启 ===
数据库迁移完成: 应用 0 个迁移

=== 空库首启（cloudpivot_p308_v2，e2e 用）===
数据库迁移完成: 应用 274 个迁移
```

`10_restart.sh` 的 18 条断言覆盖：V0341 只记录一次 / `node_type` 含 SUBPROCESS / 分区 CHECK 含 `sub` /
`subprocess_def_id` 列存在 / 父子两列 / 部分索引 / 重启 0 迁移且业务行数不变（11 → 11）/
**down.sql** 后父子两列 + 部分索引 + 子流程约束 + `subprocess_def_id` 全部清除、`node_type` 退回 9 值 /
**up.sql** 重放后结构复原、重启仍「应用 0 个迁移」（指纹未变）。

> 卡面点名的坑：`ck_biz_flow_node_config_partition` 必须同步扩 `sub`，否则插 `config.sub` 直接撞库 CHECK。
> V0341 第 ④ 步就是它，且末尾 `DO $$` 自检断言「分区 CHECK 同时含 task/notify/sub」——漏改 = 迁移失败 = 启动失败，
> 而不是运行期 500。

---

## 3. 隔离实例真 HTTP 实测（PASS=77 / FAIL=0）

原始输出：`raw/20_e2e_subprocess.txt`（脚本 `scripts/20_e2e_main.sh`）
环境：库 `cloudpivot_p308_v2`（空库首启 274 迁移）、端口 `18312`、二进制快照
`/root/p308/bin/cloudpivot`（md5 `e1c33cd1fbfc565cf8fdfd6837ac993e`）。

### S1 正常完成（卡面第一条）

父流程「P3-08 报销审批（含子流程）」：`START → n_sub(SUBPROCESS→预算调整) → n_dept(审批) → END`，
`config.sub = {version_policy: PIN_CURRENT, fail_action: ABORT, param_mapping:{amount:amount, dept:dept_id}, result_mapping:{budget_approved:approved}, spawn_task: true}`。

| 断言 | 实测 |
|---|---|
| 发起父实例 → 200，父实例 PENDING | ✅ |
| 子实例已建，`parent_node_key='n_sub'` | ✅ |
| 子实例 `biz_id = NULL` / `biz_no = NULL`（**不撞 `uk_biz_flow_instance_biz_active`**） | ✅ |
| **父子两行同时并存** | `select count(*) … where id in (父,子)` = **2** |
| `param_mapping` 生效：子 `form_data.amount=5000` / `.dept='D-P308'` | ✅ |
| 父节点 `n_sub = ACTIVE`、子节点 `n_budget = ACTIVE` | ✅ |
| `spawn_task` 生成父侧 `biz_task`（TODO） | ✅ |
| 子流程审批通过后：子实例 `APPROVED` | ✅ |
| **父节点自动 `APPROVED`**、父流程继续到 `n_dept ACTIVE` | ✅ |
| `spawn_task` 任务转 `DONE` | ✅ |
| 父流程再批一次 → **父实例 `APPROVED`（走到 END）** | ✅ |

父子实例 SQL 快照（同一张单的两个实例，子单无业务主键）：

```text
FLW-20261108-23FD6CD0 |-                                  |-      | p308_expense_sub   | APPROVED | NULL
FLW-20261108-3BA27001 | 01a1b98b-5fec-76ce-bdae-2b90ed8dd48d | n_sub | p308_budget_adjust | APPROVED | NULL
```

状态流转时间线（`t` = 父实例，`f` = 子实例）：

```text
t | n_sub     | SUBPROCESS | 1 | APPROVED | 13:04:41 → 13:04:41   ← 子单结算的那一刻
t | s         | START      | 1 | APPROVED
f | s         | START      | 1 | APPROVED | 13:04:41
f | n_budget  | APPROVAL   | 1 | APPROVED | 13:04:41 → 13:04:41   ← 人在这里批的
f | e         | END        | 1 | APPROVED
t | n_dept    | APPROVAL   | 1 | APPROVED | 13:04:41 → 13:04:41   ← 父流程继续
t | e         | END        | 1 | APPROVED
```

### S2 `fail_action=ABORT`：子流程被驳回 → 父实例终止

| 断言 | 实测 |
|---|---|
| 子流程驳回并终止（`return_to=FINAL`）→ 200 | ✅ |
| 子实例 `REJECTED` | ✅ |
| **父节点 `n_sub = REJECTED`** | ✅ |
| **父实例 `REJECTED`** | ✅ |
| 父流程**没有**继续到 `n_dept`（节点行 0 条） | ✅ |
| 父侧提示任务转 `CANCELLED` | ✅ |

### S3 `fail_action=CONTINUE`（对照组）：子流程驳回也继续

子实例 `REJECTED`，**父节点 `APPROVED`**，父实例仍 `PENDING` 且继续到 `n_dept ACTIVE`。

### S4/S5/S6 发布期拦截

| 场景 | 实测 |
|---|---|
| 自递归 def（`subprocess_def_id` 指向自己）发布 | **400 + `65046`**，文案「指向了本流程自己…自递归会在发起时无限建子实例」 |
| 深度链 D0（叶子）→ D1（1 层）→ D2（2 层）→ D3（**3 层，上限内**） | 全部 200 ✅ |
| 深度 4 层（D4 → D3 → D2 → D1 → D0）发布 | **400 + `65046`**，文案「子流程嵌套深度 4 层超过上限 3（链路 …）」 |
| 指向**未发布**草稿 def 发布 | **400 + `65045`** |

### S7 幂等

重复点同一条已完成的子流程待办 → 200（引擎幂等分支），并且：
父实例状态不变（`APPROVED`）、**父节点行数不变**、`n_sub` 仍然只有 1 行。
实现上回调走两把闸（父实例非 `PENDING` / 父节点已终态），都在 `SELECT … FOR UPDATE` 行锁内判定，
与 `task_bridge::on_instance_settled` 同款写法（失败只 `warn`，不回滚子单结论）。

### S8 回归：不含 SUBPROCESS 的既有流程完整走通

| 流程 | 实测 |
|---|---|
| `po_approve`（种子，9 节点，金额三档分流） | 发起 → 审批 → **实例 `APPROVED`** ✅ |
| `pr_approve`（种子，5 节点，预算内/超预算条件分支） | 发起 → 审批 → **实例 `APPROVED`** ✅ |

（种子流程的审批节点是角色口径，脚本把测试审批人 `p308_boss` 放进 `DEPT_LEADER` 等种子角色；
**不改种子流程一个字**。次外 `cargo test --lib business::flow` 501 passed，见 raw/30_checks.txt。）

---

## 4. 代码门禁

原始输出：`raw/30_checks.txt`

| 项 | 结果 |
|---|---|
| `rustfmt --edition 2024 --check`（本卡 9 个文件） | ✅ 全干净 |
| `cargo clippy --lib -- -D warnings` | 只剩**既有基线** `erp/sales_order_model.rs:699 too_many_arguments`；本卡文件零命中 |
| `cargo test --lib business::flow` | **501 passed / 3 failed**；3 条失败是 `linkage` 触发点计数**基线**（HEAD 的 `TRIGGERS` 已 16 条，测试仍断言 15 条，P3-08 动手前就红） |
| `flow::subprocess` 单测 | **10 passed**（含卡面要求的 4 例：正常完成/子驳回 ABORT 的判定、自递归、深度超限，见下） |
| `pnpm typecheck` | **exit 0** |
| 前端单测 `flow-subprocess-node` | 5 passed |

卡面要求的「新臂至少 4 例」对照：

| 卡面要求 | 覆盖位置 |
|---|---|
| 正常完成 | e2e S1（子实例 APPROVED → 父节点 APPROVED → 父实例 APPROVED） |
| 子驳回 ABORT | e2e S2（子实例 REJECTED → 父实例 REJECTED）+ 单测 `settle_subprocess` 判定表 |
| 自递归 | 单测 `nesting_rejects_self_recursion` / `nesting_rejects_indirect_cycle` + e2e S4（65046） |
| 深度超限 | 单测 `nesting_allows_three_levels_and_rejects_four` + e2e S5（65046） |

> 说明：S1/S2 属**引擎臂**（要真实例），因此在隔离实例上用真 HTTP 跑（PASS=77）；
> 表驱动的纯判定（配置解析 / 嵌套 / 映射）在后端单测里钉住，避免「只有 e2e 一条证据」。

---

## 5. 关键决策（与规格偏差为 0，逐条说明为什么这么落）

1. **真子实例，不展开子图**：展开会撞 `node_key`、子流程改版要重展开所有父流程、子流程的审批记录/超时/统计混进父实例。
2. **子实例 `biz_id` 为 NULL**（卡面决策）：`uk_biz_flow_instance_biz_active` 是
   `(biz_type, biz_id) WHERE status='PENDING' AND biz_id IS NOT NULL` 的部分唯一索引 ——
   子单留空既不撞父单、也不与兄弟子单相撞，`parent_instance_id + parent_node_key`（部分索引）负责定位。
   §3 的「父子两行并存 = 2」就是这条的 SQL 证明。
3. **`subprocess_def_id` 放物理列，不放进 `config.sub`**：外键只能在列上生效；「哪些流程引用了这个子流程定义」要能反查；
   且**快照格式一个字节都不改**（存量 4 个 PUBLISHED 版本的 `graph_hash` 不受影响）。
   运行期按 `(version_id, node_key)` 读列 —— PUBLISHED 版本的节点行是冻结的（`replace_graph` 只动 DRAFT），
   与「快照冻结」等价。
4. **版本策略语义**（`config.sub.version_policy`）：
   - `PIN_CURRENT`（默认）：同一 `(父实例, 节点)` 已经有子实例 → 复用它绑的 `version_id`（退回后重走同一节点不换版本）；
   - `FOLLOW_LATEST`：每次进入都重新解析 `biz_flow_def.current_version_id`；
   - 两者共同点：子实例永远绑死一个具体 `version_id`（引擎 D2）。
5. **`param_mapping` 方向 = `{子流程键: 父表单路径}`**，`result_mapping` 反向 = `{父表单路径: 子表单路径}`。
   方向定死的理由：R27 要校验「映射到的键在**子流程**表单里存在」——只有键是子侧这句话才成立。
6. **`param_mapping` 的「键必须在子流程表单里存在」这一条跳过**（卡面允许）：本仓**没有表单定义（schema）表**
   （`grep -rn "form_schema" backend/src` 零命中，`biz_flow_*` 也没有表单字段表），`form_data` 是各业务单据自带的 JSONB。
   硬造一份字段注册表等于给二开加一个必须同步维护的新概念；当前只校验映射形状，运行期取不到的路径按「缺值不填」处理（不写 null）。
   代码里 `validate_publish` 的注释写明了这条与「将来有 schema 表时怎么补」。
7. **嵌套深度 ≤ 3 的判定口径**：根流程自身 = 0 层，它的子流程 = 1 层 …；`D3`（第 3 层）可发布、`D4`（第 4 层）被拒。
   **根的子流程引用取「正在发布的这张图」**而不是按 `def.current_version_id` 查库 ——
   发布事务最后一步才把 `current_version_id` 指向这张草稿，按当前版本 JOIN 会永远查空、深度恒为 0
   （本卡第一版就栽在这，e2e S5 当场抓到；已修 + 在代码注释里写明）。
8. **`spawn_task` 的落地**：复用 `task_node::spawn` 在父侧生成一条 `biz_task`（负责人 = 父单发起人，
   `self_approval=ALLOW`），子流程结算时 `DONE`（通过）/ `CANCELLED`（未通过）。
   生成失败只 `warn` 不回滚 —— 它是可见性附加物，不该让一张已建好的子单被回滚（子实例建不出来才是 65047）。
9. **回调接线点**：`POST /business/flow/task/{id}/approve` 与 `/reject` 在事务提交后调用
   `subprocess::on_instance_settled`（与 `task_bridge::on_instance_settled` 并列）。
   失败只 `warn`（子单结论已落库），父节点保持 `ACTIVE` 待人工介入 —— 日志里带父子两个 id。
10. **`65045/65046/65047` 分工**：65045 = 静态配置问题（发布期就能查出来）；65046 = 自递归/深度；
    65047 = 运行期才出现的发起失败（角色被删、子流程没有可发版本等）。

### 已知边界（写在明处，不藏）

- **回滚 V0341 会让已发布的含 SUBPROCESS 版本无法执行**：`NodeType::parse("SUBPROCESS")` 返回 `None`
  （引擎按 APPROVAL 兜底），子实例的父指针会永久丢失。`down.sql` 的注释写了「回滚前先确认没有 PUBLISHED 版本含 SUBPROCESS、没有在跑的子实例」。
- 子实例结算回调目前接在**人工审批**（approve/reject）两个入口；`terminate_instance`（管理员强制终止）
  还没有接回调，被强制终止的子单不会回推父节点 —— 属后续小卡，当前 `fail_action` 语义在人工路径上完整。
- `spawn_task` 生成的任务标题固定为「子流程运行中：节点名」（不渲染模板变量），避免把用户表单变量插进系统提示任务。
- 本卡**未部署 145/144**：迁移为纯结构扩展、默认行为不变（没有 SUBPROCESS 节点的流程走的是同一套引擎分支）。

---

## 6. 复跑方法

```bash
# 0) 前置：本机 PG（alun 账号）+ /root/p307/instance.env（三把密钥 + admin 口令）
cd /opt/cloudpivot/backend && cargo build && mkdir -p /root/p308/bin && cp target/debug/cloudpivot /root/p308/bin/

# 1) 空库 + 起隔离实例（274 个迁移）
su - postgres -c "psql -v ON_ERROR_STOP=1 -c 'CREATE DATABASE cloudpivot_p308_v2 OWNER alun'"
cd /opt/cloudpivot/backend && setsid env $(grep -v '^#' /root/p307/instance.env | grep -v '^$' | tr '\n' ' ') \
  NIK_DATABASE_NAME=cloudpivot_p308_v2 NIK_SERVER_LISTEN=127.0.0.1:18312 \
  /root/p308/bin/cloudpivot >/root/p308/e2e_instance.log 2>&1 &

# 2) e2e（PASS=77）
cd /opt/cloudpivot/docs/evidence/20261108-p308-subprocess
PGPASSWORD=alun DB=cloudpivot_p308_v2 bash scripts/20_e2e_main.sh

# 3) 迁移幂等 / 回滚（PASS=18；需要一个停在 V0340 的库，本卡用 createdb -T cloudpivot_p307_ut 克隆）
PGPASSWORD=alun DB=cloudpivot_p308_upg bash scripts/10_restart.sh

# 4) 代码门禁
PGPASSWORD=alun bash scripts/30_checks.sh > raw/30_checks.txt 2>&1
```

> 踩坑记录（未入库）：第一版 e2e 脚本把图 JSON 写在 `"$(cat <<JSON … JSON)"` 里，
> bash 在这种嵌套下会报 `unexpected EOF while looking for matching ')'`（脚本整篇解析失败，
> 而且 bisect 时 `head -N` 会误判为「截断处坏了」）。修法就是本目录现在这版：
> **图 JSON 先 `cat > $RAW/xxx.json`，再用 `save_graph "$VID" "$RAW/xxx.json"`**。
> 原残骸留在本机 `scripts/20_e2e.sh.broken`，**故意不提交**（它本身是解析不了的脚本）。
