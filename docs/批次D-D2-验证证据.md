# 批次 D · D2「运行侧数据与引擎」交付与验收证据

> 卡面：`t_3715c446`【批次D·D2】运行侧：V0122(4表)+引擎(并发三件套/会签3语义/退回round)+8种审批人解析+12接口
> 依据文档：`docs/批次D-工作流审批-技术架构.md` §4（运行时）/ §6.2（运行侧 DDL）/ §10.1.2–10.1.4（接口）/ §13（五个坑）
> 真机环境：`172.18.10.144`，验收实例 `127.0.0.1:8027` + 全新空库 `cloudpivot_d2v`；生产 `cloudpivot.service :8023`

---

## 1. 交付物清单

| 类别 | 文件 | 说明 |
|---|---|---|
| 迁移 | `backend/migrations/business/V0122__flow_instance_tables.{up,down}.sql` | 运行侧 4 张表（实例 / 实例节点 / 待办 / 抄送） |
| 迁移 | `backend/migrations/business/V0123__flow_instance_indexes.{up,down}.sql` | POC 实测补出来的 3 条索引 |
| 迁移 | `backend/migrations/business/V0124__flow_instance_permissions.{up,down}.sql` | **实例侧 5 个权限节点**（卡面未列，理由见 §9.1） |
| 新模块 | `backend/src/business/flow/assignee.rs` | 审批人解析：8 种来源 + 三层去重 + 三策略 + ROBOT 过滤（12 条单测） |
| 新模块 | `backend/src/business/flow/engine.rs` | 引擎：发起 / 同意 / 驳回+退回 / 撤回 / 转办 / 加签 / 超时代点（12 条单测） |
| 新模块 | `backend/src/business/flow/timeout.rs` | 超时扫描（REMIND / AUTO_APPROVE / AUTO_REJECT，2 条单测） |
| 扩展 | `flow/model.rs` | 运行侧行结构 + 全部运行侧 SQL（锁顺序/现算计票/派生计数） |
| 扩展 | `flow/dto.rs` | 12 个接口的请求 / 出参 |
| 扩展 | `flow/handler.rs` | 12 个接口（12–23） |
| 扩展 | `flow/mod.rs` | 注册 `assignee` / `engine` / `timeout` |
| 扩展 | `shared/error_code.rs` | 65xxx 段 +5 码（65025–65028 / 65032），22 → 27 |
| 门禁 | `scripts/check.sh` | 闸② 登记 4 张运行侧表（口径见文件内注释） |

---

## 2. 迁移（验收 1）

```
=== ① down（逆序 0124 → 0123 → 0122）===
V0124__flow_instance_permissions.down.sql   rc=0
V0123__flow_instance_indexes.down.sql       rc=0
V0122__flow_instance_tables.down.sql        rc=0
=== ② down 后残留（必须全 0）===
tables=0
indexes=0
perms=0
=== ③ up（0122 → 0123 → 0124）===
V0122 rc=0 / V0123 rc=0 / V0124 rc=0
=== ④ up 之后（应为 tables=4 indexes=21 perms=5）===
tables=4
indexes=21
perms=5
```
（完整日志：`144:/root/flow_d2/mig_down_up.log`；脚本 `144:/root/flow_d2/test_migrations.sh`）

**21 条索引**（含核心去重索引）：

```
uk_biz_flow_instance_no / uk_biz_flow_instance_biz_active / uk_biz_flow_instance_node /
uk_biz_flow_task_pending / uk_biz_flow_cc /
idx_biz_flow_instance_initiator / idx_biz_flow_instance_def / idx_biz_flow_instance_biz /
idx_biz_flow_instance_node_inst / idx_biz_flow_instance_node_stat / idx_biz_flow_instance_node_active /
idx_biz_flow_task_mine / idx_biz_flow_task_inst / idx_biz_flow_task_node / idx_biz_flow_task_due /
idx_biz_flow_cc_mine / idx_biz_flow_cc_unread
```

**验收 2（部分唯一索引真的挡重复待办）**：用真实实例的 ACTIVE 节点插一条同人 `PENDING` 待办：

```
psql rc=1
ERROR:  23505: duplicate key value violates unique constraint "uk_biz_flow_task_pending"
```

---

## 3. 引擎设计（engine.rs）

### 3.1 状态机（单测 5×5 矩阵）

* 实例 5 态：`PENDING → {APPROVED, REJECTED, CANCELLED, WITHDRAWN}`，终态**永不回** `PENDING`
  （`transition_allowed` + `instance_transition` 双函数，`instance_state_machine_5x5_matrix` 钉住
  「除 PENDING 的 4 条出边外没有任何迁移」）。
* 节点 6 态：`PENDING`（汇聚等人齐）/ `ACTIVE` / `APPROVED` / `REJECTED` / `SKIPPED` / `CANCELLED`。

### 3.2 会签判定（`judge` 纯函数，3 语义 × veto 全矩阵 60 组单测）

| 场景 | 断言 |
|---|---|
| `ALL` 3 人 2 同意 1 未审 | `Wait` |
| `ALL` 3 人 3 同意 | `Pass` |
| `RATIO=0.6` 五人 3 同意 | `Pass`（**提前通过**，不等剩余 2 人） |
| `ANY` 3 人 1 同意 | `Pass` |
| `RATIO` + `reject_veto` 且已有 1 票反对 | `Reject`（短路，与 mode 正交） |
| `n = 0`（审批人全被跳过） | `AutoPass` |

### 3.3 🔴 并发三件套（`grep` 可验）

| 红线 | 落点 | 证据 |
|---|---|---|
| 计票 `COUNT(*)` 现算，**禁 `+=`** | `model::tally_of_node`（唯一票源）；`update_node_counts` 写绝对值 | 单测 `no_accumulator_on_vote_counts` 静态扫描源码 |
| `FOR UPDATE` 包住「计票+判定+流转」 | 锁顺序 `biz_flow_task` → `biz_flow_instance_node` → `biz_flow_instance` | 验收 11b：另开会话持锁 3 s，审批请求耗时 **2.44 s**（不取锁就 <0.1 s） |
| `23505` 当幂等成功 | `model::is_pending_duplicate` / `is_node_duplicate`；`touch_node` 用 `ON CONFLICT DO UPDATE` | 验收 11a：双连接并发**无 500** |
| 副作用锁外发送 | `Effects` 锁内只 `push`，handler 提交后 `engine::flush_effects` | 代码结构 + 通知失败只 WARN |

### 3.4 退回（坑 4）

* **同实例内**完成，目标节点 `round = max(round) + 1`，第 1 轮行一行不动。
* 三种目标：`PREV`（上一节点）/ `INITIATOR`（生成 `task_kind=RESUBMIT` 待办）/ 节点 key（管理员）/ `NONE`（驳回并终止）。
* **汇聚点（入度 ≥ 2）退回 = 整组回落**：`passed_upstream_sources` 返回**全部**已通过的上游来源，
  否则只回落一支会让 AND 汇聚闸永远等不齐（实测发现，见 §9.4）。
* **所有判定都带 `round`**：`node_arrived(instance, key, round)` —— 退回到第 2 轮后，
  汇聚点读到的是第 2 轮的来源状态，不会复用第 1 轮的 `APPROVED`。

### 3.5 撤回 / 加签 / 转办 / 超时

* 撤回：只允许发起人 + `reviewed_count == 0`（`APPROVED`/`REJECTED` 都不算 `SKIPPED`/`TRANSFERRED`），否则 `409 + 65023`。
* 加签：行锁内落 `is_added_sign=true` 的行，`approver_count` 用 `COUNT(*)` 重算（比文档的 `+=` 更严）。
* 转办：旧行 `TRANSFERRED` + `transferred_to`，新行 `transferred_from` 指回旧行，`approver_count` 不变。
* 超时：默认 `REMIND`（上限 3 次，CAS 幂等）；`AUTO_APPROVE` 缺 `flow_def.allow_auto_approve` 时**降级为提醒**并 WARN。

---

## 4. 审批人解析（assignee.rs）

8 种来源 → `SourcePlan`（纯函数，可单测）：`USER` / `ROLE` / `INITIATOR` / `INITIATOR_SUPERIOR` /
`DEPT_LEADER` / `TASK_OWNER` / `TASK_CREATOR` / `TASK_ASSIGNER` / `FORM_VAR` / `ADMIN`，
`EXPRESSION` 落 `Unsupported` → `501 + 65032`（**不**退化成空审批人）。

三层去重（D14）：

1. 解析期 `finalize()` 按 `user_id` 去重（显式 `USER` 优先）；
2. 落库期 `uk_biz_flow_task_pending` + 「23505 当幂等成功」；
3. 计票期 `COUNT(*)`。

三个策略：`self_approval(ALLOW|SKIP|FORBID)`（默认 SKIP，**SKIP 落 SKIPPED 行**）/
`empty_assignee(SKIP|ERROR|FALLBACK_ADMIN)`（默认 SKIP，计入 `auto_passed_nodes`）/
`allow_robot`（隐式来源过滤 ROBOT，显式 `USER` 放行）。

---

## 5. 12 个接口（卡面 §4 的 12–23）

| # | 方法 | 路径 | 权限码 |
|---|---|---|---|
| 12 | POST | `/business/flow/instance` | `business:flow:instance:create` |
| 13 | GET | `/business/flow/instance/page` | `business:flow:instance:read` |
| 14 | GET | `/business/flow/instance/{id}` | `business:flow:instance:read` |
| 15 | POST | `/business/flow/instance/{id}/withdraw` | `business:flow:instance:withdraw` |
| 16 | POST | `/business/flow/task/{id}/approve` | `business:flow:approve` |
| 17 | POST | `/business/flow/task/{id}/reject` | `business:flow:approve` |
| 18 | POST | `/business/flow/task/{id}/transfer` | `business:flow:approve` |
| 19 | POST | `/business/flow/task/{id}/add-sign` | `business:flow:approve` |
| 20 | POST | `/business/flow/task/batch` | `business:flow:approve` |
| 21 | GET | `/business/flow/task/todo` | `business:flow:instance:read` |
| 22 | GET | `/business/flow/task/done` | `business:flow:instance:read` |
| 23 | GET | `/business/flow/task/cc` | `business:flow:instance:read` |

可见性（§9.1 / D16）：`数据范围(initiator_dept_id) ∪ 我发起的 ∪ 我待办/已办 ∪ 抄送我`，
数据范围片段由 `scope::filter_for(…, INSTANCE_TARGET)` 生成后与 id 集合做 **OR**
（`paginate` 的 `{FILTER}` 只能做 AND 注入，故不用它，理由写在 `model::page_instances` 注释里）。

---

## 6. 真机验收（`144:/root/flow_d2/e2e_run4.log`，**PASS 39 / FAIL 0**）

夹具：4 部门 / 8 账号（含 1 ROBOT + 1 只读账号 data_scope=4）/ 5 角色 / 1 关联任务；
15 张流程图全部走**真实** `建定义 → 存整图 → 发布`。

| # | 验收点 | 实测输出（节选） |
|---|---|---|
| 3 | 单签通过 | 实例=APPROVED；时间线 `[('n_a1','APPROVED')]` |
| 4 | 驳回终止 | `return_to=NONE` → 实例=REJECTED，节点 `['1','REJECTED','1','0','1']` |
| 4b | 驳回意见 <5 字 | `400 code=65005 msg=驳回意见至少 5 个字（当前 2 个）` |
| 5 | 零人已审撤回 | 实例=WITHDRAWN；待办行 `CANCELLED` |
| 6 | 已有人审后撤回 | `409 code=65023 msg=已有人审批（1 票），不能撤回…` |
| 7 | ALL 3 人 | 2 票时 `ACTIVE 3/2`；第 3 票 → `APPROVED 3/3` |
| 8 | RATIO 0.6 五人 | 3 票 → 实例=APPROVED；其余 2 条 `CANCELLED` |
| 9 | ANY 3 人 | 1 票 → 实例=APPROVED；其余 2 条 `CANCELLED` |
| 10 | RATIO(0.8)+veto | 否决前 `ACTIVE 5/3`；1 人否决 → 节点 `REJECTED 5/3/1`、实例 REJECTED |
| 11a | 真双连接并发 | `approved_count=2 / COUNT(*)=2`；A `…954.171→954.177`、B `…954.172→954.182`（HTTP 200/0，无 500） |
| 11b | 行锁真的生效 | 外部持锁 3 s，审批请求耗时 **2.44 s** |
| 12 | 20 轮并发一致性 | 20/20 轮 `approved_count = COUNT(*) = 3` |
| 13a | DEPT_LEADER / INITIATOR_SUPERIOR / TASK_OWNER / TASK_CREATOR | 逐节点命中 u2 / u4（根部门负责人）/ u3 / u2，走完 APPROVED |
| 13b | FORM_VAR | `form_data.approver = u6` → 待办给 u6 |
| 13c | USER | 待办 `assignee_source=USER` |
| 14a | 去重（两部门同负责人） | 只 1 行待办（DEPT_A + DEPT_C 都是 u1） |
| 14b | 去重（两角色同成员 + ROBOT 过滤） | 只 1 行（u5），robot 被过滤 |
| 15 | 自审 SKIP 落 SKIPPED 行 | 待办 `SKIPPED is_self_approval=true`；`auto_passed_nodes=1` |
| 17 | 危险组合 | 整单 APPROVED 且**详情页** `auto_passed_nodes=1`、节点 `is_auto_passed=true` |
| 16 | 空审批人 SKIP | 节点 `SKIPPED`、无待办行、`auto_passed_nodes=1`、实例 APPROVED |
| 18 | 隐式来源过滤 ROBOT | ROLE 成员 {robot,u5} → 待办只有 u5 |
| 19 | 显式 USER 指定 ROBOT | 待办 `D2-d2robot / USER`，来源标记 `USER` |
| 20 | 退上一节点 | 节点行 `n_a1(1)APPROVED / n_a2(1)REJECTED / n_a1(2)ACTIVE`；第 2 轮走完 APPROVED，`round_total=2` |
| 21 | 退发起人 | `n_start(2) RESUBMIT → u2`；重新提交后 `n_a1(2)ACTIVE`，同标题实例数=1（**不新开实例**） |
| 22 | 汇聚退回 | 两支一起回落到 round 2；汇聚点第 2 轮**重新出待办**（未复用第 1 轮 APPROVED），第 2 轮通过才 APPROVED |
| 23 | 转办 | 旧行 `TRANSFERRED` + `transferred_to` 非空；新行 `transferred_from` 指向旧行；被转办人可审完 |
| 24 | 加签 | `approver_count=2`、新人 `is_added_sign=true`；两人都同意才 APPROVED（ALL） |
| 列表 | 分页可见性 | 审批人列表命中本单；`data_scope=4` 的无关账号可见 0 条（scope 真在裁剪） |
| 列表 | 我的已办 | 审过的单出现在 `/task/done` |

---

## 7. 单测与门禁（验收 25 / 26）

```
cargo test --lib        → 760 passed; 0 failed  （D1 后基线 734 → +26）
cargo clippy --all-targets -- -D warnings → 0 error / 0 warning
bash scripts/check.sh   → 全部检查通过（闸① 0 处 / 闸② 0 处 / C7 G1–G6 全绿）
```

新增 26 条单测的分布：

| 文件 | 条数 | 覆盖 |
|---|---|---|
| `assignee.rs` | 12 | 8 来源 → 查询计划、层级夹取、三层去重、自审 3 策略、空审批人 3 策略、ROBOT 隐式/显式、停用剔除、FORM_VAR 标量/数组 |
| `engine.rs` | 12 | 会签 3 语义 × veto（60 组矩阵）、单签、n=0、状态机 5×5、票数禁自增（静态扫描）、`return_to` 解析、START 识别、副作用去重 |
| `timeout.rs` | 2 | 任务名/CRON 常量、报告摘要 |

闸② 口径：运行侧 4 张表登记进 `BATCHB_SCOPED_TABLES`；`flow/model.rs` 内**接 scope**
（`page_instances` 用 `scope::filter_for`）；`flow/timeout.rs`（后台任务）与
`flow/engine.rs`（单条按主键 + 已授权流转）写 `SCOPE-EXEMPT` 理由。

---

## 8. 生产部署与 §6.2 验收（验收 27）

见 §10（部署指纹）与 §11（生产冒烟）。要点：迁移上生产（V0122/V0123/V0124 进 `schema_migrations`）、
release 编译并同盘 `mv` 替换、新接口匿名访问 **401（不是 404）**、管理员真实登录发起一张单并走完。

---

## 9. 与卡面的偏差 / 实现中发现的问题（诚实记录）

### 9.1 多出一条 V0124（权限节点）

卡面「工作方式·只改」只列 `V0122*` / `V0123*`。仍新增 `V0124__flow_instance_permissions`，理由：

* D1 的 `V0121` 文件头**明确预留**「D2/D3 落地实例侧时追加码（`business:flow:instance:read` / `business:flow:approve` …）」；
* 不种码的后果是**隐形故障**：超管有权限短路，验收全过；但任何非超管角色在角色树上**勾不到**这 5 个码，
  接口对所有人 403（skill `cailu-dev` §9.13-6 记过同款）。

本迁移**只做加项**，不动 V0121 的 5 个码；down 连授权一起清零（实测 `perms=0`）。

### 9.2 「一个人同时是角色成员 + 发起人主管」在数据上无法构造

`biz_flow_node` 的模型是**一个节点一个 `assignee_type`**，解析期只展开该类型下的 `node_assignee` 行，
因此**跨类别**重复（ROLE ↔ INITIATOR_SUPERIOR）根本不会进入同一个候选集合。
去重仍然必要且已实测两种情况（**同类别多目标**）：

* `DEPT_LEADER` 指向两个部门、两部门负责人同一人 → 只 1 行（验收 14a）；
* `ROLE` 指向两个角色、两角色共享同一成员 → 只 1 行（验收 14b，同时验证 ROBOT 过滤）。

### 9.3 RATIO + veto 用 0.8 而非卡面的 0.6

卡面要求「`RATIO + reject_veto`：3 同意但 1 人否决 → 立即 REJECTED」。但 `RATIO=0.6`、五人时
**3 票就已经达到阈值**（3/5 = 0.6），节点在第 3 票时已经通过，构造不出「3 同意后再被否决」。
因此该场景用 `RATIO=0.8`（五人三票 = 0.6 < 0.8 仍在等票），实测：否决前 `ACTIVE 5/3`，
1 人否决后立即 `REJECTED`（`judge` 的单测另有 0.6 阈值下 veto 的独立断言）。

### 9.4 修复的三处真实缺陷（都在本卡范围内）

| # | 问题 | 现象 | 修法 |
|---|---|---|---|
| 1 | **D1 遗留**：`approve_ratio` 绑文本却写 `?::numeric` | RATIO 流程**保存整图 500**（`error serializing parameter 9`）—— D1 没有端到端跑过 RATIO 流程，因此从未暴露 | SQL 改 `?::text::numeric`（与 skill 里 `?::date` 同一个驱动坑） |
| 2 | RATIO「提前判死」公式 | `RATIO(0.8)` 五人、2 同意 3 待审 → 被误判成**驳回**（架构 §3.3.2 的 `(n-a)/n` 只在 r=0 时等于「还能达到的最大通过率」） | 改为 `(approved + pending) / total < ratio` |
| 3 | 汇聚点退回只回落一支 | 汇聚点驳回后另一支停在上一轮，AND 汇聚闸永远等不齐（流程卡死，不是「退回后直接过」） | `passed_upstream_sources`：入度 ≥ 2 时**所有**已通过来源一起开新轮次 |

> 另：节点通过后驱动下游时必须从**出边目标**起算（`drive(out_targets(...))`），
> 早期写成 `drive(vec![当前节点])` 会被「本节点已终态」的幂等分支直接吃掉 —— 现象是
> 节点 APPROVED、待办消失、整单永远 PENDING。这条已写进 `engine.rs` 的注释。

### 9.5 本卡刻意没做的

| 项 | 为什么 |
|---|---|
| 审计写入 `sys_change_log` | 需要把 `biz_flow_instance` / `biz_flow_task` 登记进 `business/changelog/diff.rs` 的 `REGISTRY`，该文件不在本卡改造范围（D3/D5 接）。本卡用 `tracing::info!` 留业务痕迹 |
| `AUTO_APPROVE` 的定时任务接线 | `timeout.rs` 已交付 `register_builtin` + 扫描/处理逻辑，但接线点在 `src/task/mod.rs`（调度器），不在本卡「只改」清单。接线是一行，留给 D5/D6 与前端开关一起上 |
| `EXPRESSION` 来源的 e2e | 库 CHECK 不允许该值入库（P0 设计如此），501 路径只有 `plan_of` 单测覆盖 + 代码分支 |
| `ESCALATE`（升级加签） | P1；本卡按提醒处理（与 `REMIND` 同路径），注释写明 |
| 前端页面 | D6；本卡只交付接口与权限点 |

---

## 10. 部署指纹

| 项 | 值 |
|---|---|
| 功能提交 | `15f6d49` feat(flow): 批次D·D2 运行侧——V0122 四表 + 引擎（并发三件套/会签3语义/退回round）+ 8种审批人解析 + 12接口 |
| 文档提交 | 本文件所在的 docs 提交（见 `git log --oneline -1 -- docs/批次D-D2-验证证据.md`） |
| release 二进制 | `backend/target/release/cloudpivot`，sha256 `fb14ff85f5709a0cf576b6d6f972be3212e810222d6aaf243778f78f17d03fea` |
| 生产迁移 | `schema_migrations` 含 `120 / 121 / 122 / 123 / 124`（V0122/V0123/V0124 由启动迁移器写入） |
| 生产表 / 权限节点 | 运行侧表 4 张；实例侧权限节点 5 个（`…0800`–`…0804`） |
| 验收实例 | `127.0.0.1:8027` + 空库 `cloudpivot_d2v`（与 dev 8024 / prod 8023 隔离） |
| 日志留档 | `144:/root/flow_d2/e2e_run4.log`（39/39）、`mig_down_up.log`、`run/d2v.log` |

---

## 11. 生产冒烟（§6.2，`144:/root/flow_d2/prod_smoke.py`，PASS 9 / FAIL 0）

```
① 生产库迁移落地 V0122/V0123/V0124     schema_migrations=['120','121','122','123','124']
① b 4 表 + 5 权限节点                  运行侧表=4 / 实例侧权限节点=5
② 新接口匿名访问全部 401（不是 404）    /instance/page 401、/task/todo 401、/instance/{id} 401、
                                        POST /instance 401
③ 生产保存整图                          HTTP 200 code=0
③ 生产发布                              HTTP 200 code=0 snapshot_hash=b0fb59bc…
③ 发起后管理员的待办里出现该单           待办 node=n_a1 round=1 source=USER
③ 审批通过                              HTTP 200 code=0 instance_status=APPROVED
③ 详情：实例 APPROVED + 时间线完整        timeline=[('n_a1','APPROVED')] auto_passed_nodes=0
④ 冒烟数据已清理                        残留实例=0；删流程 HTTP 200 code=0
```

> 首次冒烟的**额外证据**（值得单独记）：把「发起人 = 审批人」的节点用默认
> `self_approval=SKIP` 跑一遍，生产上得到的正是坑 3 的语义 ——
> 节点 `SKIPPED`、`auto_passed_nodes=1`、整单直接 `APPROVED`、待办行是
> `assignee_source=USER / status=SKIPPED`（不是静默丢弃）。随后把节点改为
> `self_approval=ALLOW` 才走成「真实登录 → 发起 → 待办 → 同意 → APPROVED」。

### 11.1 生产清理

冒烟单与冒烟流程定义已删除（实例/节点/待办行硬删、流程软删），`biz_flow_instance` 里不留演示数据。

---

## 12. 一句话结论

D2 的 4 张运行侧表、引擎（并发三件套 / 会签三语义 / 退回 round / 撤回 / 转办 / 加签 / 超时）、
8 种审批人解析（三层去重 + 三策略 + D18）、12 个接口全部落地并**逐条真机留证**：
39 项端到端断言全绿（含真双连接并发、行锁阻塞实测、20 轮计数一致性），单测 734 → 760，
clippy / check.sh 全绿，生产已部署并冒烟通过。
