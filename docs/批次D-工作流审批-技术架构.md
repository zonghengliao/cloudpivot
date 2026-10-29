# 云枢 · 批次 D「可编辑工作流审批」技术架构设计

> 可配置流程定义 · 运行时引擎 · 任务验收集成 · 可视化编辑器
>
> | 项 | 值 |
> |---|---|
> | 项目 | 云枢通用后台（CloudPivot，Rust 1.94.1 / edition 2024 / alun 0.4.2 / PostgreSQL 18.6） |
> | 代码基线 | `172.18.10.144:/opt/cloudpivot`，分支 `main`，HEAD **`db1e56e`**（`docs(task): T7a 补记合并后复验与生产指纹`） |
> | 基线事实（**本机实测**） | 迁移最新到 **`V0114`**（`business` 45 / `system` 11 / `support` 11 = 67 条 up 迁移）；`biz_task.approval_instance_id` 已存在且恒为 `NULL`；全库**零触发器**（不允许用触发器维持不变式） |
> | 迁移号段 | 本批从 **`V0115`** 起，占用 `V0115`–`V0119` |
> | 本文档性质 | **只做设计，不交付业务代码**。所有 DDL 在真库 **PG 18.6 实跑通过**（`d_flow_v1` 约束负面测试 / `d_flow_v3` 十万级性能实验）；所有性能结论附 `EXPLAIN (ANALYZE, BUFFERS)` 实测；所有代码事实附 `文件:行号` |
> | 验证环境 | 本机 PG 18.6（Ubuntu 26.04 / x86_64），验证库 `d_flow_v1` / `d_flow_v3`；脚本见附录 B，**可一键复跑** |
> | 本地代码镜像 | `/root/.hermes/workspace/architect/baseline3/{src,migrations,frontend-src}`（rsync 自生产 HEAD `db1e56e`，仅供检索定位） |
> | 交付代码位置 | 新模块 `backend/src/business/flow/`；**改一处** `business/task/state.rs`（任务状态机闸门，§5.2 报批项）；`business/changelog/diff.rs` 加 3 条 `ResourceSpec`；`shared/error_code.rs` 加 `65xxx` 段 |
> | 证据文件 | `/root/.hermes/workspace/architect/d_flow/`（`ddl.sql` / `neg_test.sql` / `seed.sql` / `perf*.sql` / `cycle*.sql` / `mech.sql` / 各 `*.out` 原始输出） |

---

## 目录

| 章节 | 内容 | 谁最该先读 |
|---|---|---|
| §0 | 决策摘要（22 条硬结论 + 4 处与任务书的偏差 + 交付边界） | 全体（5 分钟） |
| §1 | 现状核查：`approval_instance_id` 落点 / 变更日志的 11 个 action / 通知签名 / 调度器 / 可见性先例 / 组织与 AI 约束（全部带行号） | 后端实施者 |
| §2 | **需求覆盖表**：任务书 24 个小问逐条「做 / 不做 / 留 P1」 | 老大 + 产品 |
| §3 | 流程模型：图的三个表示 / 7 种节点 / 会签三语义 / 条件 DSL / 并行网关 / 版本 / 发布校验 12 条 | 后端 + 产品 |
| §4 | 运行时：状态机 / 审批人解析 / 会签判定 / **并发（实测丢更新）** / 退回与 round / 撤回 / 加签转办 / 超时 | 后端 + 评审（**重点**） |
| §5 | 与任务管理集成：发起权 / 状态机闸门 / 双向引用 / AI 审批人 | 后端 + 产品 |
| §6 | 数据模型：9 张表完整 DDL（V0115–V0119）+ 24 条负面约束实测 | 后端实施者 |
| §7 | 性能实测：10 万实例 / 30 万待办下 6 条热路径 + **可见性四种写法对拍** + 两条索引缺口 | 评审 + 运维 |
| §8 | 环检测实测：递归 CTE 为什么不可用（含爆炸曲线） | 后端 + 评审 |
| §9 | 数据权限与审计：可见性谓词 / 五类角色 / `sys_change_log` action 映射 | 后端 + 安全 |
| §10 | API 契约：44 个端点 + 12 个权限码 + 22 个错误码 + 3 个样例 | 后端 + 前端 |
| §11 | 前端：SVG 自绘编辑器（坐标变换 / 拖拽 / 连线 / 撤销 / 校验） | 前端 |
| §12 | 实施拆分 D1–D6 | 项目负责人 |
| §13 | 风险与**最容易踩的 5 个坑** | 评审 + 老大 |
| §14 | 未决项（需老大拍板） | 老大 |
| 附录 A–C | 迁移清单 / POC 复跑命令 / 事实索引 | 复核者 |

---

## 0. 决策摘要（先看这一页）

### 0.1 二十二条硬结论

| # | 决策 | 为什么（一句话） | 否决了什么 |
|---|---|---|---|
| **D1** | 流程图有**三个表示**：编辑期行（`biz_flow_node`/`biz_flow_edge`）、发布期快照（`version.graph_snapshot` JSONB）、运行期内存图 | 三者的读者不同：编辑器读行（要能改、要能 SQL 反查）、引擎读快照（要一次读完、要不可变）、内存图供单次流转求值 | 只存 JSONB（改一个节点重写整块、并发编辑互相覆盖、无法 `WHERE` 反查「哪些流程用了这个角色」）；只存行（运行时每步 JOIN 三张表，且历史行被改一行就悄悄改了在跑的单子） |
| **D2** | 实例**绑定 `version_id`**，引擎只读 `graph_snapshot`，**永不跟随 `def.current_version_id`** | 改流程不能影响在跑的单子（版本漂移是审批系统第一号事故） | 实例读「最新发布版」（在跑的实例会中途变道，审批人凭空多一个/少一个）；每实例复制一份快照（10 万实例 × 5 KB = 500 MB 纯冗余） |
| **D3** | 会签三语义 `ALL` / `RATIO` / `ANY`，阈值 `approve_ratio ∈ (0,1]` | 全部同意、按比例、或签是三种真实的组织规则，缺一不可 | 只做 `ALL`（部门会签常要「过半数即可」）；只做 `ANY`（财务会签必须全员） |
| **D4** | `reject_veto`（一票否决）是**正交开关**，不是第四种会签模式 | 否则 `RATIO + 一票否决` 这个最常见组合无法表达 | 把「一票否决」放进 `approval_mode` 枚举（组合爆炸：3×2=6 个枚举值，其中 3 个是别名） |
| **D5** | 条件用 **JSON DSL**（`{op:"and",args:[{field,cmp,value}]}`），字段白名单，深度 ≤ 8、节点 ≤ 64 | 反序列化成强类型 enum，没有「函数调用」这个面，永久无代码执行 | 引入 `evalexpr`/`rhai`（新依赖 + 沙箱逃逸面 + 评审成本）；手写表达式解析器（约 600 行 + 词法/优先级/错误定位测试）；字符串模板 `amount > 10000`（要自己写求值器，等于自己写了个 `eval`） |
| **D6** | `PARALLEL` 有 `fork_mode(AND\|OR)`，汇聚节点有 `join_mode(AND\|OR)`；**禁止混流汇聚**（发布校验 V8 拦截） | XOR 分支汇到同一节点若按 AND 等，会永久卡死（有一支永远不会到达） | 只做 AND（XOR 汇聚必死循环）；只做 OR（并行任务会提前放行）；不做校验（等于把死锁留给运行时） |
| **D7** | **只有显式网关能分叉**：`APPROVAL`/`COUNTERSIGN`/`CC` 出度必须 = 1 | 「一个审批节点两条出边」等于隐式并行，退回时「上一节点」有两个，语义直接崩 | 允许任意节点多出边（BPMN 允许，但 BPMN 靠 `gateway` 语义 + 完整运行时支撑，我们不做 BPMN） |
| **D8** | 退回在**同一实例内**完成（目标节点 `round + 1`），不新开实例；退回到发起人 = 生成一条 `task_kind=RESUBMIT` 的待办 | 「被退了几次」要能在一张单上答出来；新开实例会把时间线割裂成两张单，统计「打回率」算不出来 | 退回发起人 = 实例 `REJECTED` + 重新发起新实例（时间线断裂，且 `prev_instance_id` 串联只是把问题挪到报表层） |
| **D9** | 撤回（`WITHDRAWN`）只允许**发起人**且**零人已审**（所有待办仍 `PENDING`） | 有人审过再撤回 = 抹掉别人的工作意见；管理员另有「终止」（`CANCELLED`）通道 | 允许随时撤回（审批人的意见凭空消失，审计对不上）；只有管理员能撤（发起人误提交后无法自救） |
| **D10** | 超时默认 `REMIND`（只提醒，上限 3 次）；`AUTO_APPROVE` 需 `flow_def.allow_auto_approve` 全局闸 + 行上 `is_auto=true` | 「没人审 = 自动通过」是合规事故的经典成因，必须默认关且留痕 | 默认自动通过（不可接受）；默认自动驳回（业务侧会大面积误杀） |
| **D11** | P0 只做**并行加签**（同一节点加人、应到 +1）；前加签（插到我和流程之间）与委托 P1 | 前加签要改「实例图」——在跑的实例图上动刀是 D2 的反面 | P0 做前加签（要引入「实例级图补丁」，与版本不可变冲突）；P0 做委托（需代理关系表 + 生效期，与转办语义重叠 80%） |
| **D12** | 转办（`TRANSFER`）P0，旧行留 `TRANSFERRED` 痕迹，新行 `transferred_from` 指向旧行 | 「这事不归我管」是最高频的审批操作，不做等于逼用户点驳回 | 转办 = 改 `assignee_id`（历史丢了「原来派给谁」，与 `biz_task` 转派同理） |
| **D13** | 审批人**冻结成行**：解析结果写进 `biz_flow_task`（每人一行）+ `assignee_source` | 角色成员会变、部门会调；在跑的单子必须「点名到人」，否则同一个人一会儿该审一会儿不该审 | 每次打开待办时重新解析（角色一改，待办列表就变，用户会看到任务里没有的单子） |
| **D14** | 去重**三层**：解析期按 `user_id` 去重、落库期 `uk_biz_flow_task_pending` 部分唯一索引、计票期 `COUNT(*)` 现算 | 一个人可能同时是「角色成员」和「发起人主管」；只靠应用层去重，并发下必进两条 | 只做应用层去重（并发双写进两条待办，一个人投两票）；只靠 DB（能挡住但报 500，没有可读文案） |
| **D15** | 并发三件套：`instance_node` 行锁（`SELECT … FOR UPDATE`）+ `instance.revision` CAS + 把 `23505` 当幂等成功 | 实测：无锁时两人同时审批，`approved_count` 从 0 变 1（丢一票）；或两人都判定「节点通过」而重复激活下一节点 | 只用乐观锁（会签人多时重试风暴）；只用唯一索引兜底（第二个人收到 500，体验崩） |
| **D16** | 可见性用「应用层预解析 id 集合 → `id = ANY(?::text::uuid[])`」，与 `biz_task` 的 `VISIBLE_OR_TAIL` 同款 | 实测 0.887 ms vs 相关子查询 227 ms（**256×**）；且 `= ANY(<子查询产出的数组>)` 反而 776 ms（更差） | 相关 `EXISTS` 子查询（10 万实例下 227 ms）；`= ANY(子查询)`（776 ms，规划器拿不到数组值，退化成逐行数组匹配） |
| **D17** | 审计**映射**到 `sys_change_log` 现有的 11 个 `action`（提交=`CREATE`、同意/驳回=`STATUS`、转办/加签=`ASSIGN`、发布=`PUBLISH`、停用=`OFFLINE`） | `sys_change_log.action` 的 CHECK 约束是**分区表**上的约束，扩枚举值要校验所有分区（锁表）；映射零成本 | 扩 CHECK 加 `APPROVE`/`REJECT`（分区表 ALTER 要扫全部历史分区）；另建 `biz_flow_log` 表（与 `sys_change_log` 的查询/导出/脱敏全部重做一遍） |
| **D18** | AI 审批人：**隐式**解析（角色/主管/负责人）默认过滤 `ROBOT`（`node.allow_robot = false`）；**显式**指定 `USER` 不受限 | 「人明确选了这个机器人」是授予行为；「角色里恰好有机器人」不是 | 一律硬拒 ROBOT（与 T5「修正 11」冲突：AI 拿到角色就该能干活）；一律放行（角色里混进机器人会导致机器人自审） |
| **D19** | 任务状态机加闸门：`approval_instance_id` 非空且实例 `PENDING` ⇒ 禁止任何人手动推 `REVIEW→DONE/DOING`（只允许流程回调） | 否则创建人点一下「验收通过」就绕过了整套审批 | 只在前端隐藏按钮（接口裸奔）；把审批人写进 `state.rs` 的角色掩码（审批人可能是动态的、可能是机器人，掩码表达不了） |
| **D20** | 环检测用**应用层 O(V+E) 图遍历**（与 `dep::find_cycle` 同款，`dep.rs:131`），**不用递归 CTE** | 实测：200 节点 + 60 条跳跃边的合法 DAG，递归 CTE 跑 **300 秒未结束**；而「一次读全图」只要 **0.250 ms** | 递归 CTE 路径枚举（组合爆炸，见 §8）；`UNION` 去重递归（能跑完但**识别不出环**，见 §8 证据 B） |
| **D21** | 表单字段 P1：P0 只做「审批意见 + 附件」，`form_data` 仅作为**条件表达式的作用域**（发起时冻结） | 自定义表单要设计控件/校验/渲染/联动，是一个独立批次的工作量 | P0 做自定义表单（会把本批从 6 张卡变成 12 张卡，且与条件 DSL 的字段白名单耦合） |
| **D22** | 抄送**非阻塞**：不产生待办、不参与 `PENDING` 判定、独立表 | 抄送人「已读/未读」与审批进度无关；混进待办表会让「节点通过」的计票逻辑出现幽灵票 | 抄送写进 `biz_flow_task`（要区分「投票行」和「知会行」，每一处计票都得加 `WHERE task_kind='APPROVAL'`，漏一处就错） |

### 0.2 与任务书假设的偏差（必须先对齐，共 4 处）

| # | 任务书假设 | 实测事实 | 本文的处理 |
|---|---|---|---|
| **A** | 「环检测复用批次 C+ T4 的**递归 CTE** 方案」 | T4 的真实实现是**内存 BFS**（`task/dep.rs:131 find_cycle`，`MAX_CYCLE_DEPTH = 100`，`dep.rs:49`），全库**没有**任何用于判环的递归 CTE（`grep RECURSIVE` 命中的 4 处都在 `principal.rs` / `notice/fanout.rs` / `help/model.rs` / `dept/model.rs` 的**部门树**场景）。且本批实测：流程图上的递归 CTE 路径枚举**组合爆炸**（§8 证据 A：200 节点 + 60 跳 > 300 s） | 改为「复用内存图遍历」，并给出 `Kahn` 拓扑排序 + `DFS visited` 两套等价实现与 12 条发布校验的落点（§8） |
| **B** | 会签语义 = 「全部同意 / 按比例 / 一票否决」三选一 | 「一票否决」与「按比例」不是互斥关系：真实业务里 `RATIO + 一票否决`（过 60% 且财务没否决）是最常见的组合 | 拆成 `approval_mode(ALL\|RATIO\|ANY)` × `reject_veto(BOOL)` 两个正交维度（D3/D4） |
| **C** | 表设计给 8 张 | 需要 **9 张**：多一张 `biz_flow_node_assignee`——审批人 id 列表若塞进 `node.config JSONB`，则 ① 无法外键约束（指向已删用户/角色）；② 无法反查「哪些流程引用了这个角色」（改角色前的影响面分析）；③ 本栈驱动**不支持数组参数绑定**（`scope.rs` 文档注释里实测过 `Value::Array → unsupported feature array binding`） | 9 张表（§6.8 逐张解释「为什么是这 9 张」） |
| **D** | 「审批通过 → 任务 `REVIEW → DONE`」当作一个回调即可 | `state.rs` 的 `TRANSITIONS` 是**角色位掩码**白名单（`REVIEW→DONE` 只允许 创建人/分派人/管理员，`state.rs` 的 `TRANSITIONS` 表第 4 条）。流程回调是「系统身份」，不在任何掩码里；而审批人也可能不是这三类角色 | **必须改 `state.rs` 一处**：加 `flow_gate` 判定（D19）——这是本批唯一的「改既有代码」报批项（§5.2） |

### 0.3 交付边界（本批**不做**）

| 不做 | 为什么 | P1 触发条件 |
|---|---|---|
| 自定义表单字段（控件/校验/联动） | 独立工作量，且要先把「字段白名单」跑稳 | 出现「审批时要填报销金额/合同编号」的正式诉求 |
| 前加签 / 委托代理 | 都要动「在跑的实例图」，与 D2 冲突 | 出现「领导出差要人代审」的诉求，且愿意接受「代理期内的审批记在代理人名下 + 标注委托」 |
| BPMN 2.0 导入导出 | bpmn-js 是重型库（前端 +400 KB），且 XML 方言几乎必然有损 | 客户要求与外部 BPM 系统互通 |
| 子流程 / 调用活动 | 实例嵌套会让「卡在哪」的统计变成树形聚合 | 出现「审批单里再发起一张审批单」的诉求 |
| 动态审批人「上一节点的审批人」 | 会与退回 round 交叉出歧义（第 1 轮的审批人 vs 第 2 轮） | 明确要「谁审的谁复审」时，按 round 语义重新设计 |
| 审批单的 SSE 实时推送 | 与批次 C+ 的 D10 同结论：`notice` 的 SSE 通道是**单形态事件契约**（`EVENT_NAME = "notice"`），塞第二种 payload 要改前端消费路径 | 待办实时性成为投诉点 |
| 流程定义的「另存为模板 / 市场」 | 先把单租户编辑跑稳 | 多部门要复制流程 |

---

## 1. 现状核查（全部实测，不是文档推测）

### 1.1 审批相关字段现在长什么样

**`biz_task.approval_instance_id` 已预留，恒为 `NULL`**：

```sql
-- migrations/business/V0100__create_task_tables.up.sql:98
approval_instance_id  UUID, -- 审批流实例（批次 D 预留，本批恒为 NULL）
-- :131
COMMENT ON COLUMN biz_task.approval_instance_id IS '审批流实例 ID（批次 D 预留，本批恒为 NULL）';
```

模型层已经在读它（`task/model.rs:406` / `:652` / `:704` / `:908` / `:1057`），DTO 已下发到前端（`task/dto.rs:485`，`frontend/src/api/business/task-api.ts:160` 声明为 `string | null`）。**这意味着本批不需要给 `biz_task` 加任何列**——只需要写它。

> **这条很重要**：`biz_task` 的 SELECT 列表是手写常量（`model.rs:652`、`:908`、`:1057` 三处各一份），本批**不改**这些 SQL，因此不存在「加了列忘了改三处 SELECT」的经典坑。

### 1.2 任务验收的现有语义（回调要对齐的就是这张表）

`business/task/state.rs` 的 `TRANSITIONS` 是**唯一**的合法流转清单（5×5=25 种里只放 10 条）。与本批相关的两条：

| from → to | 谁能做（角色位掩码） | 附加 | 副作用 |
|---|---|---|---|
| `REVIEW → DONE` | `ROLE_CREATOR \| ROLE_ASSIGNER \| ROLE_ADMIN` | — | `progress = 100`（Force）、`done_at = now()` |
| `REVIEW → DOING` | 同上（打回） | **必填 `reject_reason`（≥5 字）** | `progress = 10`（Reset）、`done_at` 不动 |

由此推出三条集成约束：

1. **「验收人」= 创建人 / 分派人 / 管理员**，负责人自己不算（`state.rs` 模块文档明确写「负责人自己点完成必须被拒」）。所以审批流里的「验收节点」默认审批人应当是 `TASK_CREATOR` + `TASK_ASSIGNER`，而不是 `TASK_OWNER`。
2. **驳回必须带原因（≥5 字）**，与审批的「驳回意见」可以直接复用同一个字段：审批人填的 `comment` 直接写进 `biz_task.reject_reason`（超长截断到 500 字符，`biz_task.reject_reason` 是 `VARCHAR(500)`）。
3. **`REVIEW → DONE` 会把 `progress` 强制拉满**，所以审批通过后不需要额外算进度。

### 1.3 变更日志：`action` 只有 11 个值，这是硬约束

`sys_change_log` 是**按月 RANGE 分区表**（`V0084__create_change_log.up.sql:60`），主键是 `(id, created_at)`（分区表主键必须含分区键，`:53`），且 `action` 列有 CHECK：

```sql
-- migrations/business/V0084__create_change_log.up.sql:55-56
CONSTRAINT ck_sys_change_log_action CHECK (action IN
    ('CREATE','UPDATE','DELETE','STATUS','ASSIGN','PUBLISH','OFFLINE','UPLOAD','DOWNLOAD','CLEAN','LOGIN')),
```

**在分区表上扩 CHECK 枚举值 = 对所有历史分区做约束校验（AccessExclusiveLock）**。审批有「同意/驳回/转办/加签/撤回」五个动词，其中两个（同意/驳回）不在上面 11 个值里。

**结论（D17）**：不扩 CHECK，做**语义映射**——

| 审批动作 | 落 `action` | `action_label` 示例 |
|---|---|---|
| 发起 / 重新提交 | `CREATE` | 「发起审批：任务验收」 |
| 同意 / 驳回 / 超时自动处理 | `STATUS` | 「审批同意」/「审批驳回」 |
| 转办 / 加签 | `ASSIGN` | 「转办给张三」/「加签李四」 |
| 撤回 / 终止 | `STATUS` | 「发起人撤回」「管理员终止」 |
| 流程发布 | `PUBLISH` | 「发布版本 v3」 |
| 流程停用 | `OFFLINE` | 「停用流程」 |

写日志的唯一入口是 `business::changelog::record`（`changelog/mod.rs:119`）与 `record_in`（`:296`），**必须在业务同一事务内调用**（回滚不留假日志）。且资源必须先在 `changelog/diff.rs` 的 `REGISTRY` 登记，否则 `record` 以 `62012` 拒绝（fail-closed）。

### 1.4 通知：一次 dispatch 调用，扇出目标四选一

`business::notice::dispatch(draft: NoticeDraft)`（`notice/mod.rs:290`），关键字段：

| 字段 | 用途 | 审批场景怎么填 |
|---|---|---|
| `target: NoticeTarget` | `User(Vec<Uuid>)` / `Role(Vec<Uuid>)` / `Dept(Vec<Uuid>)` / `All`（`notice/fanout.rs:36-45`） | **只用 `User`**——审批人已经在 `biz_flow_task` 里冻结成具体的人了，不要再用 `Role` 让通知模块二次展开（会与冻结快照不一致） |
| `force: bool` | 是否尊重收件人的通知偏好开关 | 待办相关一律 `true`（同一人可能同时是负责人、创建人和协作人，去重后再发） |
| `template_code` | 走模板渲染 | 建议 4 个模板：`flow_todo` / `flow_result` / `flow_cc` / `flow_remind` |
| `idempotency_key` | 幂等键 | `<event>:<instance_id>:<node_key>:<round>:<user_id>`——重试不会重复发 |
| `link_url` | 跳转链接 | `/flow/instance/<id>`（前端路由） |
| `biz_type` / `biz_id` | 业务关联 | `workflow` / 实例 id |

**通知失败不回滚业务**（与 `task/collab.rs:1345` 同款：`match notice::dispatch(draft).await { Err(e) => tracing::warn!(...) }`）。

### 1.5 调度器：`register_cron(名字, cron, 描述, 闭包)`

```rust
// src/task/mod.rs:306
pub fn register_cron(&self, name: &str, cron: &str, desc: &str,
                     runner: impl Fn() -> tokio::task::JoinHandle<()> + Send + Sync + 'static)
```

业务模块的定时任务统一在 `Scheduler::register_builtin`（`src/task/mod.rs:233`）里挂载，现有 **8 处**已挂（`storage::cleanup` / `notice::cleanup` / `notice::sse_hub` / `notice::scheduled` / `impexp::worker` / `impexp::cleanup` / `announcement::maintenance` / `changelog::partition`，`mod.rs:241`–`:262`）。注意 `mod.rs:239` 的注释：**注册点必须在这里，不能挪到 `bootstrap.rs`**（放在 bootstrap 会被随后的 reload 清空）。本批加 2 个：

```rust
// src/task/mod.rs:250 之后
crate::business::flow::timeout::register_builtin(self);   // 超时提醒 / 自动处理，*/5 * * * *
crate::business::flow::maintain::register_builtin(self);  // 图快照自检 + 实例编号回收，0 30 3 * * *
```

**不发 SSE、不建 WebSocket**：待办页用 30 s 轮询（与批次 C+ 的 D10 同款决策，理由见 §0.3）。

### 1.6 数据权限：分页只有唯一入口，可见性用「id 集合」

`business/shared/scope.rs` 把列表分页收敛成唯一入口 `paginate`（`scope.rs:618`），要求传入 `ScopedPrincipal`（只能由 `principal::resolve` 构造，`principal.rs:612`）。`ScopedPrincipal` 的可见字段：`user_id` / `own_dept_id` / `is_super` / `kind(1..5)` / `dept_ids` / `self_only`。

任务模块给的**先例**（本批照抄，不新发明）：

```rust
// task/model.rs:99
pub const VISIBLE_OR_TAIL: &str =
    " AND ( (1 = 1 {FILTER}) OR t.id = ANY(?::text::uuid[]) )";
// task/model.rs:162  pub async fn my_member_task_ids(me: Uuid) -> DbResult<MyScopeIds>
// task/model.rs:566  pub async fn page_visible(...)
```

即：**数据权限片段（S4：`dept_id = ANY(...)`，不含递归 CTE）OR 我的关系 id 集合**。本批的「我的关系」变成三个来源的并集：我发起的 ∪ 我审批过（含待审）∪ 抄送我的。

> 注意 `scope.rs` 的 S4 硬门槛：`filter_for(DeptAndChild)` 必须是 `= ANY(…)` 形态、**不含** `WITH RECURSIVE`（`scope.rs:925` 的注释与 `:941` 的断言）。所以「我所在部门的子树」由 `principal` 在应用层预解析成 `dept_ids`，SQL 里只有 `= ANY`。**本批不改 `scope.rs`**。

### 1.7 组织：主管只能从 `sys_dept.leader_id` 取

实测 `sys_dept` 的真实列（`migrations/system/V0006__create_sys_dept.up.sql`）：`id / dept_code / dept_name / parent_id / dept_path / leader_id / phone / email / status / sort_order / …`。

- `sys_user` **没有** `leader_id` / `superior_id` 字段（`V0001` 只有 `dept_id`，且注释明确「弱引用不建外键」）。
- `sys_dept.leader_id` **没有索引**（`V0006` 只建了 `code / parent_id / path` 三个）。

**结论**：审批人来源 `INITIATOR_SUPERIOR`（发起人主管）= 「发起人所在部门的 `leader_id`」，向上 N 级 = 沿 `dept_path` 上溯取各级部门的 `leader_id`。`dept_path` 形如 `/1/2/`，上溯一次 SQL 就能拿到全部祖先部门 id，**不需要递归 CTE**。

> **索引补丁（报批项）**：`INITIATOR_SUPERIOR` 在「解析审批人」热路径上会 `SELECT … FROM sys_dept WHERE leader_id = ?`。部门表 < 1000 行时全表扫约 0.1 ms，可接受；但若要建索引必须在 `V0119` 里单独报批（与批次 C+ 建议的 `idx_sys_dept_leader` 同一件事，**合并成一条，不重复报**）。

### 1.8 AI 执行者：两条线，审批人只能走「查码」那条

`shared/ai_actor.rs` 把规则收成一张表（模块文档）：

| 操作 | 判定方式 | 错误码 |
|---|---|---|
| 网页登录 | **硬拒**（不看密码/角色） | `63201` |
| 删除任务 | **硬拒**（超管也救不回来） | `63202` |
| 改数据权限 | **硬拒** | `63203` |
| 管 AI 用户 / 改 profile | **硬拒** | `63204` |
| 分派 / 转派 / 协作人 | **只查权限码** | `63002` |
| 改任务状态 / 评论 / 工时 | 只查权限码 | — |

文件里有一句关键的方法论：「本模块只有**永久禁止**的四件事 —— 加第五个之前先问：这件事被授了角色也不该允许吗？」

**结论（D18）**：审批**不进** `ai_actor` 的硬拒表（审批是「被授角色就该能审」的事）。改用**节点级开关** `allow_robot`：
- 隐式解析（`ROLE` / `INITIATOR_SUPERIOR` / `TASK_OWNER` 等）命中 `user_type=ROBOT` ⇒ `allow_robot=false` 时按 `empty_assignee` 策略处理（默认丢弃该人）；
- 显式指定（`assignee_type=USER` 且人是他）⇒ 不检查 `allow_robot`（人选了就是授予）。

### 1.9 错误码：`65xxx` 整段空闲

`shared/error_code.rs` 的现有码段（模块文档 + 单元测试断言）：

| 码段 | 含义 | 已用到 |
|---|---|---|
| `62xxx` | 批次 C（公告/帮助/变更日志）+ 批次 D（偏好 KV） | < 63000 |
| `63xxx` | 批次 C+ 任务管理 | `63001`–`63020`、`63101`–`63105` |
| `632xx` | AI 执行者硬拒 | `63201`–`63204` |
| `64xxx` | 云枢 × Hermes | `64001`–`64014` |

`grep 65` 在 `error_code.rs` 里 **0 命中**。本批占用 **`65xxx`**（`65001`–`65040`，见 §10.3），并加一条与既有三段同性质的稳定性断言（码值一旦发给前端就不能改）。

### 1.10 运行环境（三条影响设计的硬事实）

| 事实 | 来源 | 影响 |
|---|---|---|
| PG 18.6 | 本机 `select version()` = `18.6 (Ubuntu 18.6-0ubuntu0.26.04.1)` | 可用 `CYCLE` 子句；分区表/部分索引/`FILTER` 聚合全部可用 |
| 前端无图库 | `frontend/package.json` 依赖只有 `axios / dayjs / echarts / pinia / tdesign-* / vue / vue-i18n / vue-router`（**无** d3 / vue-flow / bpmn-js / x6） | 流程图编辑器**必须自绘**（SVG），不能引入重型库（D5 of §11.1） |
| 全库零触发器 | `pg_dump -s` 里 `CREATE TRIGGER` 计数 = 0（既有约定） | 「发布后的版本不可改」**不能**靠触发器，只能靠「只写 DRAFT 版本 + 发布时算 hash + 每日自检」（§6.1） |

---
## 2. 需求覆盖表

### 2.1 任务书 24 个小问逐条结论

| # | 任务书问题 | 结论 | 落点 |
|---|---|---|---|
| 一.1 | 节点类型至少 7 种 | **做，7 种全做**；但**只有网关能分叉**（D7），审批节点出度恒 = 1 | §3.2 |
| 一.2 | 会签语义三种可配置 | **做 `ALL`/`RATIO`/`ANY` 三种 + `reject_veto` 正交开关** | §3.3 / D3 / D4 |
| 一.3 | 条件表达式：JSON DSL 或受限表达式，禁 `eval` | **JSON DSL**，字段白名单 + 6 类上限，反序列化成 `enum` | §3.4 / D5 |
| 一.4 | 并行网关：全完成汇聚还是任一汇聚 | **两种都做**（`join_mode`），且**发布期禁止混流汇聚** | §3.5 / D6 |
| 一.5 | 可视化编辑：SVG 自绘，不引重型库 | **做**，纯 SVG + pointer events（前端零新依赖，实测 `package.json`） | §11 |
| 一.6 | 版本管理：改流程不影响在跑的 | **实例绑 `version_id` + 引擎只读 `graph_snapshot`**，实测验证（§3.6） | §3.6 / D2 |
| 一.7 | 表单：P0 只做意见 + 附件 | **同意**，`form_data` 只做条件表达式的作用域 | §3.8 / D21 |
| 二.1 | 实例状态机 5 态 | **做**，但拆出「撤回 `WITHDRAWN`」与「终止 `CANCELLED`」两种终止（语义不同） | §4.1 / D9 |
| 二.2 | 节点状态 `PENDING/APPROVED/REJECTED/SKIPPED` | **扩成 6 态**：多一个 `ACTIVE`（审批中）与 `CANCELLED`（被退回掐断）；`PENDING` = 未到达 | §4.2 |
| 二.3 | 审批人来源 5 种 | **扩成 8 种**：`USER`/`ROLE`/`INITIATOR`/`INITIATOR_SUPERIOR`/`DEPT_LEADER`/`TASK_OWNER`/`TASK_CREATOR`+`TASK_ASSIGNER`/`FORM_VAR`/`ADMIN` | §4.3 |
| 二.4 | 多人命中同一人怎么去重 | **三层去重**：解析期并集去重 + `uk_biz_flow_task_pending` 部分唯一索引 + 计票期 `COUNT(*)` | §4.3 / D14 |
| 二.5 | 会签部分同意时态怎么表示 | **节点 `ACTIVE` + `approved_count/approver_count`**，节点级不设 `PARTIAL` 状态（避免第 6 个状态） | §3.3 |
| 二.6 | 打回三种 | **做三种**：退回上一节点 / 退回发起人 / 退回指定节点（后者限流程管理员） | §4.6 |
| 二.7 | 撤回是否只限未审批 | **是**：零人已审才可撤（D9）；管理员另有「终止」 | §4.7 |
| 二.8 | 催办/超时：自动通过/自动拒绝/只提醒 | **可配置，默认只提醒**；`AUTO_APPROVE` 需全局闸 + `is_auto` 留痕 | §4.9 / D10 |
| 二.9 | 加签/转办/委托 P0/P1 | **转办 + 并行加签 P0**；前加签 + 委托 P1 | §4.8 / D11 / D12 |
| 三.1 | 任务验收走审批谁发起 | **推 `DOING→REVIEW` 的那个人**（通常是负责人），不是创建人/分派人 | §5.1 |
| 三.2 | 通过 → `REVIEW→DONE`，驳回 → `REVIEW→DOING` | **做**，但要给 `state.rs` 加 `flow_gate` 闸门（**报批项**） | §5.2 / D19 |
| 三.3 | 双向引用 | **做**：`biz_task.approval_instance_id` ↔ `instance.biz_type='TASK' + biz_id` | §5.3 |
| 三.4 | AI 能不能当审批人 | **默认不能（隐式解析时过滤），显式指定不受限** | §5.4 / D18 |
| 四.1 | 表设计（8 张）+ 版本防漂移 | **9 张**（+`node_assignee`），防漂移靠版本引用 + 快照 + 每日自检 | §6 |
| 四.2 | 环检测复用递归 CTE | **否**：改应用层 O(V+E)（实测递归 CTE 爆炸） | §8 / D20 |
| 四.3 | 待办列表查询模式 → 索引 | **做**：6 条热路径全部 `EXPLAIN` 实测（0.036 ms ~ 0.887 ms） | §7 |
| 四.4 | 会签并发：乐观锁 or 行锁 | **行锁为主 + revision CAS 兜 + `23505` 幂等**（实测丢更新） | §4.5 / D15 |
| 四.5 | 实例可见性 | **做**：发起人 ∪ 审批人 ∪ 抄送人 ∪ 数据权限覆盖 ∪ 管理员 | §9.1 |
| 四.6 | 每个操作进 `sys_change_log` | **做**，映射到现有 11 个 `action`，不扩 CHECK | §9.3 / D17 |
| 五 | 完整 API 清单 | **44 个端点**（定义 11 / 实例 12 / 审批 9 / 待办 6 / 统计 4 / 其它 2） | §10.1 |
| 六 | 拆 4–6 张子卡 | **拆 6 张 D1–D6**（含依赖顺序与复杂度） | §12 |

### 2.2 我们额外补的 9 条（任务书没想到，但不做会在实施期返工）

| # | 补充 | 为什么必须有 |
|---|---|---|
| X1 | **发起幂等**：`uk_biz_flow_instance_biz_active` 部分唯一索引（同业务单据同时只能有一个 `PENDING` 实例） | 双击「提交验收」/ 前端重试 / 网络重发，都会发起两张单子。应用层查一遍再插是 TOCTOU（实测负面测试 N14 拦截） |
| X2 | **空审批人策略** `empty_assignee(SKIP\|ERROR\|FALLBACK_ADMIN)` | 角色里一个人都没有（新建角色/人员全调走）时，节点会永久卡死。三种策略各有适用场景：抄送用 `SKIP`、财务审批用 `ERROR`（宁可报错也不能漏审） |
| X3 | **自审策略** `self_approval(ALLOW\|SKIP\|FORBID)` | 「我就是主管，我提交的任务我自己审」——不处理就会出现「单人部门的单子永远批不了」或「自己批自己」两种事故 |
| X4 | **抄送独立表**（不混进待办） | 见 D22：混进去会让每一处计票都要 `WHERE task_kind='APPROVAL'` |
| X5 | **`graph_hash` + 每日自检** | 全库零触发器，「发布后不可改」只能靠自检发现「有人直接改库把在跑的单子改了道」 |
| X6 | **`assignee_source` / `is_added_sign` / `is_self_approval` / `is_auto` 四个标记** | 审计必须能回答「为什么是他审我」「这一票是人点的还是系统代点的」 |
| X7 | **节点耗时 `duration_ms`（激活→结束）** | 「卡在哪个节点」的统计需要它；事后从 `created_at` 差值算会被退回重入搞错（round 语义） |
| X8 | **`instance.revision` + `round_total`** | 前者是 CAS 兜底，后者让「打回率」一次聚合就能出 |
| X9 | **`prev_instance_id`** | 撤回/终止后重新发起，要能串起「这是同一个人第二次提交」的链路 |

### 2.3 覆盖度自检

- 任务书 24 个小问：**24 个都有明确结论**，其中 21 个「做」、3 个「改造后做」（会签语义拆分、节点状态扩到 6 态、环检测改方案），**0 个「不做」**。
- 任务书 6 大项：流程模型 / 运行时 / 任务集成 / 技术与数据模型 / API / 实施拆分 —— 全部有独立章节（§3 / §4 / §5 / §6–§9 / §10 / §12）。
- 交付边界（§0.3）7 条「不做」，每条都写了 P1 触发条件。

---

## 3. 流程模型（用户到底在「编辑」什么）

### 3.1 图的三个表示：为什么三个都要

用户在编辑器里拖一个节点，到引擎据此放行一次审批，中间隔了三层数据结构。**必须讲清楚每层是谁的、能不能改**：

| 层 | 存哪 | 谁写 | 谁读 | 可变性 |
|---|---|---|---|---|
| **L1 编辑期行** | `biz_flow_node` / `biz_flow_edge`（`version_id` 外键） | 编辑器保存接口（只写 `DRAFT` 版本） | 编辑器加载、发布校验、影响面反查（「哪些流程用了角色 X」） | **可改**（仅 DRAFT 版本） |
| **L2 发布期快照** | `biz_flow_def_version.graph_snapshot`（JSONB） | `publish` 接口，**从 L1 序列化** | 运行时引擎（一次读完）、详情页画进度图 | **不可改**（PUBLISHED 版本 + `graph_hash` 自检） |
| **L3 运行期内存图** | 进程内 LRU（key = `version_id`） | 第一次流转时从 L2 反序列化 | 引擎单次流转求值 | 每次流转重建（或缓存，见下） |

**为什么不能只留 L2（JSONB）**：
1. 改一个节点要重写整块 JSON → 两个人同时编辑，后保存的把先保存的整块覆盖（连他没动的节点一起回退）；
2. 无法 `WHERE` 反查：「角色 `finance` 被哪些流程引用」要 `jsonb_path_query` 全表扫；
3. 无法外键：`ref_id` 指向已删除的用户/角色，没有任何约束拦。

**为什么不能只留 L1（行）**：
1. 运行时每一步要 JOIN `biz_flow_node` + `biz_flow_edge` 两张表（还会随分支数多次往返）；
2. 更重要：**行是「可改」的**。改一行 = 悄悄改了所有引用该版本的**在跑实例**的定义源。这是「版本漂移」的另一条路径，比换版本更隐蔽。

**L3 的缓存策略**：

```
key   = version_id
value = Arc<FlowGraph>（节点 map + 出边 map + 入边 map，全部预解析好）
失效  = ① 版本发布事件（同进程内 invalidate）
        ② TTL 300 s（跨进程兜底：多实例部署时其他进程收不到事件）
容量  = 512 个版本（≈ 512 × 8 KB = 4 MB）
```

> **为什么 TTL 兜底是安全的**：PUBLISHED 版本的 L2 **不可变**，所以缓存陈旧不会读到错数据；TTL 只是为了让长期不用的版本占位被回收。唯一会让 L2 变化的是「有人直接改库」，那属于要报警的事件（X5 自检）。

### 3.2 七种节点的语义与图约束

| 类型 | 语义 | 是否阻塞 | 入度 | 出度 | 运行时产生什么 |
|---|---|---|---|---|---|
| `START` | 开始 | — | 0 | 1 | 实例创建时该节点 `APPROVED`（`duration_ms=0`） |
| `APPROVAL` | 单签 | 阻塞 | 1 或 N（N 需 `join_mode`） | **1** | 1 条 `biz_flow_task`（`approval_mode` 不适用；人多按 `ANY`+`reject_veto` 处理） |
| `COUNTERSIGN` | 会签 | 阻塞 | 1 或 N | **1** | M 条 `biz_flow_task`（M = 解析出的审批人数） |
| `CONDITION` | 条件分支 | 不阻塞 | 1 | **≥ 2** | 无待办；选一条出边，`taken_edge_key` 记在实例节点上 |
| `PARALLEL` | 并行网关 | 不阻塞 | 1 | **≥ 2** | 无待办；按 `fork_mode` 激活下游 |
| `CC` | 抄送 | **不阻塞** | 1 或 N | **1** | N 条 `biz_flow_cc`（不进 `biz_flow_task`）；立即 `SKIPPED` 并往下走 |
| `END` | 结束 | — | ≥ 1 | 0 | 实例置终态 |

**三条硬约束（发布校验强制，见 §3.7）**：

1. **`APPROVAL`/`COUNTERSIGN`/`CC` 出度必须 = 1**（D7）。这是本设计与 BPMN 最大的分歧：BPMN 允许一个任务节点有多条出边（隐式 XOR/AND），代价是「这条边的语义取决于运行时求值」，而我们的**退回**功能需要「上一节点」是唯一的。允许隐式分叉 → 退回时「上一节点」有歧义 → 退回功能不可实现。
2. **`CONDITION` 的每条出边必须带条件，且恰有一条 `is_default=true`**（DB 层 `ck_biz_flow_edge_default_no_cond` 保证 default 边无条件）。没有 default 边 = 存在一组输入使流程无路可走（运行时才报错，业务已经卡住）。
3. **`END` 至少一个且所有节点必须可达 END**；`START` 恰一个。

### 3.3 会签的三种语义 + 一票否决的正交性

#### 3.3.1 为什么是「模式 × 否决」二维，而不是「三选一」

任务书给的三种候选是「全部同意 / 按比例 / 一票否决」。实测推演后发现三者不是同一维度的东西：

| 组合 | 是否有意义 | 典型场景 |
|---|---|---|
| `ALL` + `veto=on` | ✅ 最常见 | 财务会签：3 人全同意才过，任一人驳回即死 |
| `ALL` + `veto=off` | ✅ 有意义 | 「收集意见」式会签：有人驳回也等其他人说完，最终按「有人驳回 → 驳回」结尾 |
| `RATIO(0.6)` + `veto=on` | ✅ **最实用的组合** | 评审委员会 5 人，过 3 人即通过；但主席一票可否决 |
| `RATIO(0.6)` + `veto=off` | ✅ | 同上但主席无否决权 |
| `ANY` + `veto=on` | ✅ | 或签：「谁在谁审」，任一同意即过、任一驳回即死 |
| `ANY` + `veto=off` | ⚠️ 可表达但别扭 | 「有一个同意就过，驳回不算数」——等于 `RATIO(ε)`，是 `ANY` 的特例 |

如果按任务书的「三选一」实现，「一票否决」就只能是第四个枚举值，那么 `RATIO + 否决` 就**表达不出来**——而它恰恰是最常见的组合。**所以拆成两维（D3/D4）。**

#### 3.3.2 判定算法（纯函数，可单测）

```
输入：mode, ratio, veto, 票集合 votes（每人 APPROVED / REJECTED / PENDING）, 应到 n
输出：NodeVerdict = { Pass, Reject, Wait, AutoPass }

a = #{v == APPROVED}
r = #{v == REJECTED}
p = #{v == PENDING}

1) 若 veto 且 r >= 1                → Reject（立即，短路：其余 PENDING 待办置 CANCELLED）
2) 若 mode == ALL 且 a == n          → Pass
3) 若 mode == ALL 且 a + r == n 且 r > 0 → Reject   （全部表过态且有驳回）
4) 若 mode == RATIO 且 a / n >= ratio → Pass
5) 若 mode == RATIO 且 (n - a) / n < ratio → Reject
     # 剩下的全同意也追不上阈值 → 提前判死，不必等
6) 若 mode == ANY 且 a >= 1          → Pass
7) 若 a + r == n 且 p == 0           → Reject（兜底：都投完了但没通过）
8) 否则                              → Wait
9) n == 0（全部审批人被跳过）        → AutoPass（节点记 SKIPPED，实例 auto_passed_nodes += 1）
```

> **第 5 条是本设计的一个细节但很关键**：`RATIO(0.6)`、n=5，已有 2 人驳回 → 即使剩下 3 人全同意也只有 3/5 = 0.6 ≥ 0.6 能过……等一下，这个例子能过。正确例子：`RATIO(0.7)`、n=5、已有 2 人驳回 → 最多 3/5 = 0.6 < 0.7 → 提前判死。**没有这一条的话，驳回 2 人后还要等 3 个人来点「同意」才发现白点**。实现上就是 `(n - a) / n < ratio`。

#### 3.3.3 「部分同意」的时态怎么表示

节点状态只有 6 个值（§4.2），**没有** `PARTIAL` 状态。部分同意的表示 = `status='ACTIVE'` + 三个计数：

| `status` | `approver_count` | `approved_count` | `rejected_count` | UI 文案 |
|---|---|---|---|---|
| `ACTIVE` | 3 | 0 | 0 | 财务会签（0/3 已同意） |
| `ACTIVE` | 3 | 2 | 0 | 财务会签（2/3 已同意） |
| `APPROVED` | 3 | 3 | 0 | 财务会签（已通过） |
| `REJECTED` | 3 | 2 | 1 | 财务会签（被驳回） |
| `SKIPPED` | 0 | 0 | 0 | 财务会签（无审批人，自动跳过） |

**为什么不开一个 `PARTIAL` 状态**：状态机的每加一个状态，`from→to` 的组合就多一层（6→7 态 = 多 13 种组合），而「部分同意」在**判定**上没有独立语义（它永远等价于 `ACTIVE`）。用一个状态 + 三个计数表达，UI 想怎么显示都行，引擎判定只看计数。**计数是展示冗余，权威值是 `biz_flow_task` 的 `COUNT(*)`**（§4.5 实测了为什么不能信任 `+=`）。

### 3.4 条件分支：JSON DSL 的完整定义

#### 3.4.1 语法

```jsonc
// 边上的 condition_expr
{
  "op": "and",                                  // and | or | not | cmp
  "args": [
    { "op": "cmp", "field": "form.amount", "cmp": "gte", "value": 10000 },
    { "op": "cmp", "field": "form.priority", "cmp": "in", "value": ["P0", "P1"] },
    { "op": "not", "args": [
        { "op": "cmp", "field": "initiator.dept_id", "cmp": "in",
          "value": ["<uuid>", "<uuid>"] } ] }
  ]
}
```

| 维度 | 允许值 | 上限 |
|---|---|---|
| 逻辑算子 | `and` / `or` / `not` / `cmp` | 树深 ≤ **8**，节点数 ≤ **64** |
| 比较符 | `eq` `ne` `gt` `gte` `lt` `lte` `in` `nin` `contains` `is_null` `not_null` | 每层 `args` ≤ 16 |
| 字段根 | `form.*`（发起时快照）/ `initiator.*`（id/name/dept_id）/ `instance.*`（biz_type/biz_no/title）/ `task.*`（仅 `TASK_ACCEPT` 场景：status/priority/estimate_hours） | 字段名 ≤ 64 字符，必须在白名单里 |
| 字面量 | `string`（≤ 200 字符）/ `number` / `bool` / `null` / `array`（≤ 50 项，元素为前三种） | **不允许对象、不允许嵌套数组、不允许函数** |

#### 3.4.2 求值伪码

```
fn eval(expr, ctx) -> Result<bool, EvalError>:
    match expr.op:
        And  => args.iter().all(|e| eval(e, ctx)?)         // 短路
        Or   => args.iter().any(|e| eval(e, ctx)?)
        Not  => !eval(&args[0], ctx)?
        Cmp  => compare(resolve(expr.field, ctx), expr.cmp, expr.value)
```

- `resolve` 只做 `ctx` 的**只读字段取值**，走白名单表（`field -> 取值函数`），未命中 → `EvalError::UnknownField`；
- 字段缺失（`form.amount` 不存在）→ `EvalError::Missing`，**不是** `false`；
- 求值出错时的降级：**走 `is_default` 边**，并在 `biz_flow_instance_node.config` 里记 `{"condition_error":"Missing: form.amount"}`（可审计）。若该节点没有 default 边（发布校验 V7 已拦住）→ 运行时 `422 + 65016`。

#### 3.4.3 否决了什么

| 方案 | 否决理由 |
|---|---|
| 引入 `evalexpr` / `rhai` crate | 新依赖；rhai 是完整脚本语言（有 `import`、有循环），沙箱逃逸是持续风险；评审要读一份 DSL 的行为规范 |
| 手写表达式语法（`amount > 10000 && dept in (...)`） | 要写词法分析 + 递归下降 + 优先级表 + 错误位置提示，约 600 行 + 一大批测试。**与 D5 的 JSON DSL 相比，多出来的只是「用户少打几个引号」**，而代价是两倍的代码和一类新的注入面 |
| 字符串模板 `amount > 10000` 直接拼进 SQL | 注入；且审批条件要读的是 `form_data`，不是库列 |
| 只做「等于某个值」的固定下拉 | 表达不了金额区间，第一批用户就会要求加 |

### 3.5 并行网关与汇聚

#### 3.5.1 分叉（`fork_mode`）

| 模式 | 语义 | 什么时候用 |
|---|---|---|
| `AND` | 所有出边全部激活（无条件） | 「法务 + 财务同时审，都过了才继续」 |
| `OR` | 按出边的 `condition_expr` 求值，命中几条激活几条（至少 1 条，否则走 default） | 「金额 > 10 万加审风控，否则不加」——注意这等价于 `CONDITION` + `OR` 汇聚，是便捷写法 |

#### 3.5.2 汇聚（`join_mode`）

| 模式 | 语义 | 判定 |
|---|---|---|
| `AND` | 等所有「活跃入边」的来源节点都 `APPROVED`/`SKIPPED` | `∀ e ∈ in_edges: src(e).round == 当前轮 且 src(e).status ∈ {APPROVED, SKIPPED}` |
| `OR` | 任一入边到达即通过 | `∃ e ∈ in_edges: src(e).status ∈ {APPROVED, SKIPPED}` |

> **「活跃入边」这个词是关键**：`AND` 汇聚不能等「所有定义上的入边」，因为 `CONDITION`（XOR）分支下有一支永远不会被激活。判定时只看**本轮已激活**的来源节点。而这也正是为什么 `AND` 汇聚只能接在 `PARALLEL(AND)` 后面——见下面的混流禁令。

#### 3.5.3 禁止混流汇聚（发布校验 V8）

一条汇聚节点的入边来源必须是**同一类型**：

| 入边来源 | 允许的 `join_mode` | 违反后果 |
|---|---|---|
| 全部来自同一个 `PARALLEL(fork_mode=AND)` 的下游 | 只能 `AND` | 用 `OR` → 并行任务会提前放行（另一半还没审） |
| 全部来自同一个 `CONDITION` 的下游 | 只能 `OR` | 用 `AND` → 有一条分支永不激活 → **永久卡死** |
| 来自不同网关 / 一半 CONDITION 一半 PARALLEL | **禁止** | 语义无法定义；发布期直接拒绝并指出具体节点 |

> 这条校验的实现是「从汇聚节点反向 BFS，找到最近的分叉网关，检查是否唯一 + 类型是否一致」。**这也是一个必须放在发布期（而不是运行时）的校验**：运行时才发现「这一单永远走不下去」，业务已经发出去了。

### 3.6 版本管理：实例绑版本（含实测证据）

#### 3.6.1 机制

```
biz_flow_def             (稳定标识: flow_code, biz_type)
  ├─ current_version_id ──────┐  ← 只决定「新实例用哪个版本」
  └─ current_version_no       │
                              ↓
biz_flow_def_version     (不可变快照: version_no, graph_hash, graph_snapshot)
  ├─ biz_flow_node   (行, 编辑期)
  └─ biz_flow_edge   (行, 编辑期)
        ↑
        │ version_id (RESTRICT 外键)
biz_flow_instance        (实例: 钉死在发起那一刻的版本)
```

三条不变式：

1. **`instance.version_id` 是 `ON DELETE RESTRICT`**：有实例引用的版本不能被删（实测负面测试 N24 同款约束已验证）。
2. **发布只改 `def.current_version_id`**，不动任何已有版本行 → 在跑的实例读的还是旧版本。
3. **引擎读 `version.graph_snapshot`，不读 `def.current_version_id`** → 代码层面不存在「顺手用最新版」的可能。

#### 3.6.2 实测证据（`mech.sql` 真跑输出）

发布 v2 之后，同一张表里两张实例各自的解析结果：

```
  instance_no   | version_no | is_current |   node_name
----------------+------------+------------+----------------
 FLW-DRIFT-0001 |          1 | f          | 一级审批          ← 在跑的单子仍是 v1 的图
 FLW-DRIFT-0002 |          2 | t          | 二级审批（新）    ← 新发起的才拿 v2
```

#### 3.6.3 版本状态机

```
DRAFT ──publish──► PUBLISHED ──archive──► ARCHIVED
  │                    │
  └──(可反复编辑)       └──(只读；实例仍可引用它)
```

| 状态 | 可编辑 | 可被实例引用 | 可发起新实例 |
|---|---|---|---|
| `DRAFT` | ✅ | ❌ | ❌ |
| `PUBLISHED` | ❌（应用层冻结） | ✅ | ✅ |
| `ARCHIVED` | ❌ | ✅（历史实例） | ❌ |

**「编辑已发布版本」怎么办**：不允许原地改。点「编辑」= **复制出一份新的 `DRAFT`**（`version_no = max + 1`），改完再发布。这样「v3 是 v2 的修改」在数据上是一条新行，历史版本原封不动。

#### 3.6.4 否决了什么

| 方案 | 否决理由 |
|---|---|
| 实例读「最新发布版」 | 在跑的实例中途变道：审批人凭空多一个、某个节点被删掉导致实例悬空。这是审批系统第一号事故 |
| 每实例复制完整快照 | 10 万实例 × 5 KB ≈ 500 MB 纯冗余；且「改一个节点的文案」要级联改所有在跑实例（复制了就不该改，于是文案永远停在旧版） |
| 原地改已发布版本 + 版本号不变 | 等于没有版本，`version_no` 变成装饰 |
| 只存最新版，历史靠审计日志还原 | 审计日志是「谁改了什么」的文字，不是可执行的图；还原一份能跑的旧图需要重放全部变更 |

#### 3.6.5 P1：紧急版本迁移

有一种真实需求：「v1 的流程有严重错误（比如审批人配错了），在跑的 50 张单子要迁到 v2」。本批**不做**（不自动、不提供接口），理由：迁移要回答「当前停在哪一步 → 新图里对应哪一步」「已经审过的人在新图里算不算数」，这是一个人工决策。P1 触发条件：出现「流程发布后必须回滚在跑实例」的正式诉求时，做 `POST /flow/instance/{id}/migrate`（显式、逐单、要求填理由、写审计、`instance.version_id` 更新为新版 + `prev_version_id` 留痕）。

### 3.7 发布校验 12 条（`POST /flow/def/{id}/publish` 必须全过）

| # | 校验 | 不通过的报错 |
|---|---|---|
| V1 | `START` 恰 1 个 | `65010`「流程必须有且仅有一个开始节点」 |
| V2 | `END` ≥ 1 个，且所有节点都能到达某个 END | `65010`「节点 X 走不到结束」 |
| V3 | **无环**（Kahn 拓扑排序，残留节点即环上节点） | `65011` + **点名环上每个节点的 `node_key`**（与 T4 的 `63101` 同款要求：不许只回「有环」） |
| V4 | 所有节点从 `START` 可达 | `65010`「节点 X 从开始节点走不到」 |
| V5 | `APPROVAL`/`COUNTERSIGN`/`CC` 出度 == 1 | `65012`「审批节点 X 有 N 条出边（审批节点只能有一条出边，分支请用条件网关）」 |
| V6 | `CONDITION`/`PARALLEL` 出度 ≥ 2 | `65012`「条件节点 X 只有 1 条出边（请用审批节点或删除该节点）」 |
| V7 | 每个 `CONDITION` 的出边恰有 1 条 `is_default=true`，其余必须带条件 | `65013` |
| V8 | 汇聚节点（入度 ≥ 2）的入边来源同源同类型，且 `join_mode` 匹配（§3.5.3） | `65014` + 指出冲突的两个来源 |
| V9 | 审批类节点 `assignee_type` 非空；`USER`/`ROLE`/`DEPT_LEADER` 至少 1 条 `node_assignee` 行；`FORM_VAR` 必须有 `form_var_key` | `65015`「节点 X 未配置审批人」 |
| V10 | `COUNTERSIGN` 必须填 `approval_mode`；`RATIO` 必须填 `approve_ratio ∈ (0,1]` | `65015` |
| V11 | 条件 DSL 合法（结构/深度/节点数/字段白名单/字面量类型） | `65016` + 指出具体位置（如 `edge e3: unknown field form.xxx`） |
| V12 | 规模上限：节点 ≤ 200、边 ≤ 400 | `65017` |

> **V3 的实现必须给出环路径**（不是「有环」两个字）。参照 T4 的 `dep::find_cycle` + `orient_cycle_from`（`dep.rs:211`）：后者专门把「从被编辑节点读起」的路径重排成人类可读顺序。流程图这里同理：报错文案 = `检测到环：n_review → n_finance → n_legal → n_review`。

### 3.8 表单：P0 只做意见 + 附件

| 项 | P0 | P1 |
|---|---|---|
| 审批意见 | `biz_flow_task.comment VARCHAR(1000)` | 富文本 / 预设常用语 |
| 附件 | `biz_flow_task.attachments JSONB`（`[{file_id,name,size}]`，文件本体在 `sys_file`） | 附件与表单字段绑定 |
| 业务变量 | `instance.form_data JSONB`，**发起时冻结**，只作为条件表达式作用域（`form.*`） | 自定义表单字段（控件/校验/联动/必填） |
| 意见模板 | 无 | 按节点配置常用语 |

**`form_data` 冻结的语义**：发起那一刻把业务单据的快照写进去（任务场景：`title/status/priority/owner_name/due_date/estimate_hours`）。之后业务单据改了，**审批单里的条件判断仍然按发起时的值**。为什么：条件分支决定的是「这个流程怎么走」，走到一半业务值变了再改道，会让已经审过的人白审。UI 上要标注「以下为发起时快照」。

---
## 4. 运行时（引擎怎么跑）

### 4.1 实例状态机（5 态，8 条迁移）

| # | from → to | 谁能推 | 前置条件 | 副作用 |
|---|---|---|---|---|
| 1 | `(无) → PENDING` | 有 `business:flow:instance:create` 的人 / 任务模块内部调用 | 流程 `PUBLISHED`；`biz_type` 匹配；同 `(biz_type,biz_id)` 无 `PENDING` 实例（唯一索引） | 建实例、建 `START` 节点行、激活下游 |
| 2 | `PENDING → APPROVED` | **只允许引擎**（最后一个 `END` 被通过时） | 无 `ACTIVE` 节点 | `ended_at=now()`；回调业务（§5.2）；通知发起人 |
| 3 | `PENDING → REJECTED` | **只允许引擎**（退回发起人 + 发起人选择「不再提交」） | — | `ended_at=now()`；回调业务（任务 `REVIEW→DOING`） |
| 4 | `PENDING → WITHDRAWN` | **发起人本人** | **零人已审**（无 `APPROVED`/`REJECTED` 的 task 行） | 所有 `PENDING` 待办 → `CANCELLED`；`ended_at=now()`；回调业务（撤销验收申请） |
| 5 | `PENDING → CANCELLED` | `business:flow:admin` | — | 同上 + 记 `result_comment`（必填理由） |
| 6 | `PENDING → CANCELLED` | **业务侧**（任务被取消/删除） | `biz_type=TASK_ACCEPT` 且任务进 `CANCELLED` | 由任务模块调 `flow::cancel_by_biz` |
| 7 | `APPROVED → *` | ❌ 不可能 | 终态 | — |
| 8 | `REJECTED/WITHDRAWN/CANCELLED → PENDING` | ❌ 不可能 | 终态；要重来就**新发起**（`prev_instance_id` 串联） | — |

**为什么 `WITHDRAWN` 与 `CANCELLED` 要分开**：两者的**责任主体**不同。撤回是「我提交错了，我收回」，终止是「管理员/业务叫停」。审计里「谁撤回的」和「谁终止的」不能混成一句「已取消」。同理，两者的通知文案也不同（撤回不通知审批人「被驳回」，终止要说明原因）。

**为什么终态不能回 `PENDING`**：终态意味着「这一次业务意图已经结束」。允许终态复活会让「这张单被审了几次」变成不可数（每次复活都重置计数）；而重新发起 + `prev_instance_id` 既保留了链路，又不破坏单次实例的统计语义。

### 4.2 节点状态机（6 态）

| 状态 | 含义 | 谁会进入 |
|---|---|---|
| `PENDING` | **未到达**（该行还不存在，或存在但是未来轮次的占位） | 实际上「未到达」= **没有行**（懒创建，§6.1）；`PENDING` 只出现在「实例初始化时预建了节点但还没激活」的场景 |
| `ACTIVE` | 审批中（有待办） | 节点被激活时 |
| `APPROVED` | 通过 | 判定 `Pass` |
| `REJECTED` | 驳回 | 判定 `Reject` |
| `SKIPPED` | 跳过 | ① 空审批人（`empty_assignee=SKIP`）② 全部审批人被自审策略跳过 ③ `CC` 节点 |
| `CANCELLED` | 被掐断（被退回时，原路径上未完成的节点） | 退回操作把「当前节点之后、目标节点之前」的活跃节点置 `CANCELLED` |

```
PENDING ──activate──► ACTIVE ──pass────► APPROVED
                        │ ├──reject───► REJECTED
                        │ ├──skip─────► SKIPPED
                        │ └──退回掐断──► CANCELLED（同一行不再复活；重进是 round+1 的新行）
```

### 4.3 审批人解析：8 种来源 + 三层去重 + 冻结

#### 4.3.1 八种来源

| `assignee_type` | 解析规则 | 需要的额外配置 | 空结果典型原因 |
|---|---|---|---|
| `USER` | 直接取 `node_assignee` 里的 `ref_id`（用户 id） | ≥ 1 行 | 用户被软删/停用 |
| `ROLE` | `SELECT user_id FROM sys_user_role WHERE role_id IN (...)` ∩ 用户未删未停 | ≥ 1 行 | 角色内 0 人 |
| `INITIATOR` | 发起人本人 | — | 不可能空 |
| `INITIATOR_SUPERIOR` | 发起人所在部门沿 `dept_path` 上溯 `superior_levels` 级，取各级 `sys_dept.leader_id` | `superior_levels ∈ 1..10` | 上级部门没设负责人 |
| `DEPT_LEADER` | `node_assignee` 里指定的部门的 `leader_id` | ≥ 1 行 | 该部门没设负责人 |
| `TASK_OWNER` | 关联任务的 `owner_id`（仅 `TASK_ACCEPT`） | 必须有 `biz_id` | 任务已删 |
| `TASK_CREATOR` / `TASK_ASSIGNER` | 关联任务的 `creator_id` / `assigner_id`（`assigner_id` 可空→空结果） | 必须有 `biz_id` | 自建任务的 `assigner_id` 为空 |
| `FORM_VAR` | 从 `form_data` 取 `form_var_key` 的值（uuid 或 uuid 数组） | `form_var_key` 必填 | 发起人没填/填错 |
| `ADMIN` | 超管（`role_code = SUPER_ADMIN` 的所有人） | — | 不可能空 |

**`INITIATOR_SUPERIOR` 的解析 SQL（一次查完，不用递归 CTE）**：

```sql
-- 1) 取发起人部门与路径
WITH me AS (
    SELECT u.dept_id, d.dept_path
      FROM sys_user u LEFT JOIN sys_dept d ON d.id = u.dept_id
     WHERE u.id = $1 AND u.deleted_at IS NULL
),
-- 2) 把 path 拆成祖先部门 id 数组（path 形如 '/1/2/3/'）
ancestors AS (
    SELECT d.id, d.dept_name, d.leader_id,
           array_position(string_to_array(trim(both '/' from me.dept_path), '/')::uuid[], d.id) AS lvl
      FROM me, sys_dept d
     WHERE d.id = ANY (string_to_array(trim(both '/' from me.dept_path), '/')::uuid[])
       AND d.deleted_at IS NULL
)
-- 3) 取当前部门往上第 N 级
SELECT a.id, a.dept_name, a.leader_id
  FROM ancestors a, me
 WHERE a.lvl = array_length(string_to_array(trim(both '/' from me.dept_path), '/')::uuid[], 1) - ($2 - 1);
```

> **注意 `superior_levels = 1` 的语义**：发起人所在部门的负责人。若发起人自己就是该部门负责人 → 命中「自审」，走 `self_approval` 策略。第 2 级 = 上级部门的负责人，以此类推。
>
> **为什么不用递归 CTE**：`dept_path` 已经是物化的路径（`V0006` 的设计初衷就是「加速子树查询」），`string_to_array` 一次就能得到全部祖先。递归 CTE 在 `dept_path` 存在的前提下是多余的一次 BFS（`scope.rs` 的 S4 也是同一取向）。

#### 4.3.2 三层去重（D14）

```
第 1 层 · 解析期：把本轮所有来源的结果并成一个 BTreeSet<Uuid>
                    （`sys_dept` 的 leader 可能同时在 ROLE 里；
                      `INITIATOR_SUPERIOR` 的 N 级可能重复命中同一人）
第 2 层 · 落库期：uk_biz_flow_task_pending (instance_node_id, assignee_id) WHERE status='PENDING'
                    （并发下两条同时插入 → 第二条 23505，应用当幂等成功忽略）
第 3 层 · 计票期：COUNT(*) 现算，不信任任何累加器
```

**实测证据（负面测试 N19）**：同一节点同一人插两条 `PENDING`（一条 `assignee_source=USER`、一条 `ROLE`）→ `duplicate key value violates unique constraint "uk_biz_flow_task_pending"`。**这证明第 2 层真的生效。**

> 为什么第 3 层也要：如果依赖 `approved_count += 1`，会签并发下会丢票（§4.5 实测 2 票变 1 票）。

#### 4.3.3 冻结（D13）

解析结果**立刻落成 `biz_flow_task` 行**，每行记 `assignee_id` + `assignee_source` + `assignee_source_ref`（来源角色/部门 id）+ `assignee_name`（名字快照）。

实测（`mech.sql` 第 ③ 段）：一条 `assignee_source=ROLE` 的待办写进去之后，即使把该用户移出角色，这条待办仍然在他名下——因为他已经被点名了。

**为什么必须冻结**：
1. 角色成员变了，在跑的单子会「换审批人」，而前面已经审过的人的票还算不算？——无法定义；
2. 待办列表按 `assignee_id` 查（`idx_biz_flow_task_mine`），如果每次都要 JOIN 角色表重新展开，索引直接失效；
3. 审计要回答「为什么是他审我」——`assignee_source` + `assignee_source_ref` 就是答案。

#### 4.3.4 空审批人策略（X2）

```
parsed = 解析后的用户集合（已过滤停用/软删/隐式命中的 ROBOT）
if parsed.is_empty():
    match node.empty_assignee:
        SKIP            => 节点置 SKIPPED，is_auto_passed = true，继续往下走
                           并写一条 WARN 日志 + 通知流程管理员（"节点X 无审批人，已自动跳过"）
        ERROR           => 事务回滚，抛 422 + 65018（"节点X 没有一个可用审批人"）
        FALLBACK_ADMIN  => 落一条 assignee_type=ADMIN 的待办给超管
```

**默认 `SKIP` 还是 `ERROR`？** 节点默认 `SKIP`（避免一个配置疏漏让流程整体卡死），但**发布校验 V9 会拦**「`USER`/`ROLE` 类型下 0 行引用」这种静态可查的空——所以真正跑到运行时才空的，只有「角色里的人都辞职了」这类真实变化。财务/合规类节点建议显式配 `ERROR`。

#### 4.3.5 自审策略（X3）

```
if 审批人集合包含发起人:
    match node.self_approval:
        ALLOW   => 保留，该待办 is_self_approval = true（UI 上打「自审」标）
        SKIP    => 从集合里剔除发起人（并记一条 SKIPPED 待办留痕，不静默丢）
        FORBID  => 事务回滚，422 + 65019（"节点X 的审批人是发起人本人，流程不允许自审"）
```

**默认 `SKIP`**。为什么默认不是 `ALLOW`：自审在企业内控里是异常项，默认放行等于把「自己批自己」变成常态且无人察觉。为什么不是 `FORBID`：单人部门（一个人的小团队）会永远批不了单。
**注意 `SKIP` 会落一条 `SKIPPED` 的 task 行**（不是删掉）——「本来该他审，因为自审被跳过」这件事必须能在审批时间线里看到。

#### 4.3.6 AI 审批人（D18）

```
for user in parsed:
    if user.user_type == 'ROBOT':
        if node.allow_robot: 保留
        else if 该用户是 assignee_type=USER 显式指定的: 保留      # 人选了 = 授予
        else: 丢弃（记 trace 日志；若因此集合为空 → 走空审批人策略）
```

**与 T5「修正 11」的一致性**：`ai_actor.rs` 只把「登录 / 删任务 / 改数据权限 / 管 AI 用户」列为永久硬拒，其余一律「只查权限码」。审批不在硬拒表里——所以一个拿到了审批权限角色的机器人**可以**审。`allow_robot` 挡的是「机器人恰好混在角色里被自动展开」这条路径，不是「禁止机器人审批」这个策略本身。

### 4.4 一次流转的完整算法

节点结束（`Pass`/`Reject`/`Skip`）之后，引擎要做的是：

```
fn after_node_ended(tx, instance, node, verdict):
    # 1) 收尾当前节点
    node.status = verdict_to_status(verdict)
    node.ended_at = now(); node.duration_ms = ended_at - activated_at
    node.approved_count / rejected_count = 现算的 COUNT(*)
    if verdict == Reject: 同节点其余 PENDING 待办 → CANCELLED

    # 2) 决定下一步
    if verdict == Reject:
        # 退回是**显式指令**，不是自动行为（§4.6）：驳回时审批人必须选退回目标
        apply_return(tx, instance, node, target = 审批人选的)
        return

    # 3) 找下游
    outs = graph.out_edges(node.key)
    if node.type == CONDITION:
        outs = [pick_first_matching(outs, ctx)]          # 命中 first true；都不中走 is_default
        node.taken_edge_key = picked.key
    if node.type == PARALLEL and fork_mode == OR:
        outs = [e for e in outs if eval(e.condition)]     # 至少 1 条，否则 default
    for edge in outs:
        activate(tx, instance, edge.to)

    # 4) 检查实例是否结束
    if 没有 ACTIVE 节点 and 所有 END 都已通过的路径闭合:
        instance.status = APPROVED; instance.ended_at = now()
        callback_to_biz(tx, instance)                    # §5.2
```

`activate(node_key)` 的核心（**幂等**是关键词）：

```
fn activate(tx, instance, node_key):
    round = 当前轮次
    # 汇聚判定：入度 >= 1 且不是首次进入
    if node.join_mode == AND and 尚未全部到齐: node.status = PENDING; return   # 记下"已到达"，等人齐
    if node.join_mode == OR  and 已经通过过:    return

    status = ACTIVE
    # 解析审批人 → 落 task 行（三层去重）
    approvers = resolve_assignees(node, instance)
    for a in approvers: INSERT biz_flow_task(...) ON CONFLICT (uk_biz_flow_task_pending) DO NOTHING
    if approvers.empty(): return after_node_ended(tx, instance, node, AutoPassOrError)

    # 抄送节点：不产生待办，直接 SKIPPED 并往下
    if node.type == CC: INSERT biz_flow_cc(...); return after_node_ended(Skipped)
```

**幂等的三道保险**（防止重复激活导致下一节点出现两套待办）：

| 保险 | 拦什么 |
|---|---|
| `uk_biz_flow_instance_node (instance_id, node_key, round)` | 同一节点同一轮次被激活两次 → `23505`（实测 §4.5 实验 2 真的触发了） |
| `uk_biz_flow_task_pending (instance_node_id, assignee_id) WHERE status='PENDING'` | 同一节点同一人落两条待办 |
| 行锁 `SELECT … FROM biz_flow_instance_node WHERE id=? FOR UPDATE` | 让「判定 + 副作用」串行（D15） |

应用层必须把 `23505` 当**幂等成功**处理（不是 500）。这一点写进 §10.5 的接口约定。

### 4.5 并发：实测丢更新，以及正确的写法

这是本批**最容易写错且不会报错**的地方（§13.1）。三个实验都在真库上双会话并发跑过。

#### 实验 1：无锁 + 应用层算好后写**绝对值** → 丢票

场景：3 人会签，A、B 两人同时点「同意」。应用逻辑是「读 `approved_count` → +1 → 写回」。

```
A 会话：BEGIN; SELECT approved_count → 0; pg_sleep(2); UPDATE … SET approved_count = 0+1; COMMIT;
B 会话（晚 0.4s）：BEGIN; SELECT approved_count → 0; pg_sleep(1); UPDATE … SET approved_count = 0+1; COMMIT;
```

实测输出：

```
 A: 我读到的 approved_count = 0
 B: 我读到的 approved_count = 0
=== 最终计数 ===
 approved_count
----------------
              1          ← 两个人审了，计数是 1（丢了一票）
```

**这不是「可能发生」，是在 PG 默认 `READ COMMITTED` 下必然发生**：两个事务都读不到对方未提交的写。

#### 实验 2：无锁 + `ANY`（或签）→ 两个人**都**判定「节点通过」，重复激活下一节点

```
A: a_sees_approved = 1   → 我判定节点通过 → INSERT n_next
B: b_sees_approved = 1   → 我也判定节点通过 → INSERT n_next
B: ERROR: duplicate key value violates unique constraint "uk_biz_flow_instance_node"
   DETAIL: Key (instance_id, node_key, round)=(aaaa1001-…, n_next, 1) already exists.
B: ERROR: current transaction is aborted
=== n_next 行数 === 1
```

**结论**：唯一索引救回了数据正确性（下一节点只有一行），但 B 的事务整体回滚 → 用户看到 500，且**他自己那一票也可能一起回滚**（如果投票和流转在同一事务，见下）。而且如果 `activate` 里有非幂等副作用（发通知、写审计、调外部 webhook），A 和 B 会**各发一次**——索引管不到这些。

#### 实验 3：`SELECT … FOR UPDATE` → 正确

```
A: BEGIN; SELECT … FOR UPDATE; sleep 2; UPDATE SET approved_count = 1; COMMIT;
B: BEGIN; sleep 0.4; SELECT … FOR UPDATE  ← 被 A 挡住，A 提交后才拿到锁，读到 1
                                   UPDATE SET approved_count = 2; COMMIT;
=== 最终 ===  2   ✅
```

#### 4.5.1 最终方案（D15：三件套）

```rust
// 审批动作的统一入口（伪码，实际在 flow/engine.rs）
async fn approve(tx: &mut PgTx, task_id: Uuid, me: Uuid, comment: &str) -> Result<..> {
    // ① 锁待办行本身（防同一人多点一次）
    let task = SELECT * FROM biz_flow_task WHERE id = $1 AND assignee_id = $2 FOR UPDATE;
    if task.status != 'PENDING' { return Ok(AlreadyDone); }        // 幂等：重复提交返回成功

    // ② 锁实例节点（串行化"计票 + 判定 + 流转"）
    let node = SELECT * FROM biz_flow_instance_node WHERE id = task.instance_node_id FOR UPDATE;

    // ③ 写这一票
    UPDATE biz_flow_task SET status = 'APPROVED', comment = $3, completed_at = now() WHERE id = $1;

    // ④ 现算票数（不信任任何累加器）
    let (a, r, p, n) = SELECT count(*) FILTER (WHERE status='APPROVED'), … FROM biz_flow_task
                        WHERE instance_node_id = node.id AND round = node.round;

    // ⑤ 覆写展示冗余（不是 +=）
    UPDATE biz_flow_instance_node SET approved_count = a, rejected_count = r WHERE id = node.id;

    // ⑥ 判定 + 流转（幂等：下一节点的 INSERT 撞唯一索引时当成功）
    match judge(node.mode, node.ratio, node.veto, a, r, p, n) {
        Pass   => after_node_ended(tx, node, Pass).await,
        Reject => after_node_ended(tx, node, Reject).await,
        Wait   => {}
    }
    // ⑦ CAS 兜底：实例级 revision（多节点并行时，两个分支可能同时推进实例状态）
    UPDATE biz_flow_instance SET revision = revision + 1, updated_at = now()
     WHERE id = $iid AND revision = $rev;   // 0 行 → 重新读 → 重试一次 → 仍失败则 409
    Ok(())
}
```

**锁的顺序固定**：`biz_flow_task` → `biz_flow_instance_node` → `biz_flow_instance`。**所有写路径必须按这个顺序取锁**，否则两个并发请求（一个「同意」、一个「退回」）会互相等死（经典 ABBA 死锁）。这条写进代码注释与 code review 清单。

#### 4.5.2 为什么不用纯乐观锁

| 方案 | 问题 |
|---|---|
| 纯 `revision` CAS（无行锁） | 10 人会签时，10 个请求同时读到 `revision=5`，9 个失败重试，重试又撞在一起 → 重试风暴。会签人越多越糟 |
| 纯行锁 | 够用；`revision` 只是「跨节点并行」场景的补充（两个分支同时推进同一实例） |
| 锁整个实例（`FOR UPDATE` on `biz_flow_instance`） | 并行网关下两个分支本来可以并行推进，锁实例会把并行退化成串行（不影响正确性，但白白限流） |

**所以：锁到「实例节点」这一级**——同一节点串行，不同节点（并行分支）互不影响。

### 4.6 退回（打回）：三种目标 + round 机制

#### 4.6.1 三种退回

| 目标 | 谁可以选 | 效果 | 语义 |
|---|---|---|---|
| **退回上一节点**（默认） | 任何审批人 | 目标 = 当前节点在**本轮的已通过上游**里最近的那一个，`round + 1` | 「这个材料不行，上一个环节改一下」 |
| **退回发起人** | 任何审批人 | 目标 = `START`，`round + 1`；生成 `task_kind=RESUBMIT` 的待办给发起人 | 「这事得重新提」 |
| **退回指定节点** | **仅 `business:flow:admin`** | 管理员在流程进度图上点任一已通过节点 | 「流程走歪了，拉回到第 2 步」 |

**为什么「退回任意节点」要限管理员**：普通审批人退回第 1 步 = 把前面 3 个人已经审过的工作全部作废，这是一个管理决策，不是审批人的个人判断。

#### 4.6.2 `round` 机制（本设计的关键之一）

同一实例内退回重走，**不覆盖历史**：新轮次是 `biz_flow_instance_node` 的新行（`round + 1`）。

实测（`mech.sql` 第 ② 段）：

```
 node_key | round |  status  | approved_count
----------+-------+----------+----------------
 n_a      |     1 | REJECTED |              0     ← 第 1 轮的驳回历史保留
 n_a      |     2 | ACTIVE   |              0     ← 第 2 轮重新进入
```

**统计「打回率」就靠它**：

```sql
SELECT instance_id, max(round) - 1 AS reject_times
  FROM biz_flow_instance_node GROUP BY instance_id;
-- 实测输出：rounds=2 → reject_times=1
```

**如果不做 round**（复用同一行）：① 「这个节点被驳回过几次」永久丢失；② 第 1 轮的 `duration_ms` 被第 2 轮覆盖；③ 详情页的审批时间线会出现「同一个人在这一节点同意了两次」而没有任何区分。

#### 4.6.3 退回时必须做的 6 件状态回滚

这是任务书点名要回答的「退回后状态回滚」问题。退回不是「改一个状态」，是一次**小型的反向迁移**：

| # | 对象 | 动作 | 漏了会怎样 |
|---|---|---|---|
| 1 | 当前节点 | `status = REJECTED`，`ended_at`/`duration_ms` 落账 | 统计里这个节点耗时算不出来 |
| 2 | 同节点其余待办 | → `CANCELLED` | 会签里其他人还能继续投无效票 |
| 3 | 当前节点到目标节点之间**所有 `ACTIVE` 节点** | → `CANCELLED` | 并行分支上的待办永远悬着（幽灵待办） |
| 4 | 目标节点 | **新开 `round+1` 行**，`status=ACTIVE`，重新解析审批人落待办 | 复用旧行 → 历史被覆盖 |
| 5 | 目标节点之后、原本 `APPROVED` 的节点 | **不动**（保留它们的通过事实），但**不能再被「已通过」判定覆盖**——所以判定必须带 `round` 条件 | 「上一轮的通过」被当成本轮的通过 → 流程直接跳过去 |
| 6 | 实例 | `round_total = max(round)`，`revision + 1` | 打回率统计失真 |

> **第 5 条是最隐蔽的一条**：如果 `join_mode=AND` 的汇聚判定写成「所有入边来源 `status='APPROVED'`」而不带 `round` 条件，退回到第 1 步再走到汇聚点时，汇聚点会看到**上一轮**留下的「已通过」记录 → 全部条件满足 → **直接跳过应该重新审的节点**。正确写法：`src(e).round = 当前轮 且 src(e).status = 'APPROVED'`。

#### 4.6.4 驳回 = 退回吗？

**不是。** 这是两个动作，接口也分开：

| 动作 | 接口 | 结果 |
|---|---|---|
| **驳回**（`reject`） | `POST /flow/task/{id}/reject` | 必填 `return_to`（`PREV` / `INITIATOR` / 节点 key）+ `comment`；实例继续 `PENDING` |
| **驳回并终止**（`reject_final`） | 同上，`return_to = NONE`（仅管理员/流程配置允许） | 实例 → `REJECTED`（终态），回调业务驳回 |

**为什么默认是「退回」而不是「终止」**：99% 的审批驳回是「改一下再来」，不是「这事不做了」。如果驳回 = 终止，那么每次驳回都要重新发起一整张单，前面所有人的审批记录全部作废且不在同一张单上。

### 4.7 撤回与终止

| 动作 | 谁 | 条件 | 结果 | 通知 |
|---|---|---|---|---|
| **撤回** `WITHDRAWN` | 发起人 | **零人已审** | 实例终态；所有 `PENDING` 待办 → `CANCELLED` | 通知当前审批人「发起人已撤回」（尊重偏好 `force=false`） |
| **终止** `CANCELLED` | `business:flow:admin` 或业务侧 | 无 | 同上 + `result_comment` 必填 | 通知全部相关人（`force=true`） |

**「零人已审」的判定必须精确**：

```sql
SELECT count(*) FROM biz_flow_task
 WHERE instance_id = $1 AND status IN ('APPROVED','REJECTED');
-- 必须 = 0
```

注意**不算** `SKIPPED`（自动跳过的人没有「审过」）和 `TRANSFERRED`（转办出去的人也没审）。

**业务侧终止的触发点**（任务场景）：任务被 `CANCELLED` 或删除时，任务模块调 `flow::cancel_by_biz(instance_id, reason)`。此时审批单已是终态，不能再审。

### 4.8 加签与转办（P0），前加签与委托（P1）

#### 4.8.1 转办 `TRANSFER`（P0）

```
old.assignee 想转给 new：
  ① 校验 new 未停用/未软删、不是 ROBOT（除非显式允许）、不等于 old
  ② 锁 old 待办行 → old.status = 'TRANSFERRED'，old.transferred_to = new 待办 id
  ③ 插入 new 待办行：assignee_id = new, transferred_from = old.id
  ④ 节点 approver_count 不变（人换了，人数没变）
  ⑤ 通知 new（force=true）
```

**为什么要留旧行**：与 `biz_task` 转派同一个理由（`diff.rs` 的 `BIZ_TASK` 白名单里 `owner_id` 的注释：「原来派给谁」是转派审计的核心）。删掉旧行 = 转办历史消失。

**转办的权限**：只有**待办持有人自己**可以转办（不是任何人可以替别人转）。管理员另有「指派」通道（P1）。

#### 4.8.2 并行加签 `ADD_SIGN`（P0）

```
在 ACTIVE 节点上增加审批人 X：
  ① 校验 X 不在本节点现有审批人里（三层去重会兜住）
  ② 插入待办行：is_added_sign = true, assignee_source 记为 'USER'（加签是显式指定）
  ③ node.approver_count += 1      ← 注意：这是唯一允许 += 的地方
     理由：应到人数是"配置"，不是"票数"；但为了防并发，仍在行锁内做
  ④ TaskVerdict 重算（若 mode=ANY 且已有人同意，加签不影响已通过的判定）
```

**为什么 `approver_count` 可以 `+=` 而 `approved_count` 不行**：前者是人数的「配置变更」（加签是一次显式的、串行的管理动作，有行锁保护且不与他人并发冲突）；后者是「票数的聚合」，必须从权威行现算。

**前加签（P1）为什么不做**：「先由 X 审，再回到我」需要在**在跑的实例图**里插入一个节点。而 D2 说实例绑定的版本图不可变。要做前加签，就得引入「实例级图补丁」这个概念——代价是引擎要在两个图上求值。等到真有诉求时，正确的做法可能是「退回到上一个节点并由 X 复核」，而不是改图。

#### 4.8.3 委托（P1）

需要：`biz_flow_delegate (delegator_id, delegate_id, start_at, end_at, biz_type, status)` + 「代理期内 delegate 看到的待办 = 自己的 + delegator 的（标注委托）」+ 审批记录写「代理人代 XXX 审」。

**不做 P0 的理由**：它与转办的语义重叠 80%（都是「别人替我审」），差别只在「是否自动、是否有期限」。而委托的坑在于「代理人审了，责任算谁的」——这是一个需要产品明确回答的问题，不是技术问题。

### 4.9 超时与催办

#### 4.9.1 配置矩阵

| `timeout_action` | 含义 | 需要的全局闸 | 备注 |
|---|---|---|---|
| `REMIND`（**默认**） | 只发提醒，不改状态 | 无 | 提醒间隔 = `timeout_hours / 2`，上限 `max_remind_times`（默认 3） |
| `ESCALATE` | 提醒 + 通知「上一级」（主管的上级） | 无 | 加签式升级：给上级落一条并加签待办（人仍是原审批人，上级是督办） |
| `AUTO_APPROVE` | 超时自动通过 | **`flow_def.allow_auto_approve = true`**（默认 false） | 自动票记 `is_auto=true`、`comment='超时自动通过'`，并通知发起人 |
| `AUTO_REJECT` | 超时自动驳回 | 无 | 记 `is_auto=true`、`comment='超时未处理，自动驳回'` |

**默认 `REMIND` 的三条理由**：① 自动通过与自动驳回都会**替一个人做决定**，而这个决定可能有法律/财务后果；② 「系统自动通过」是最常见的合规事故成因；③ 只提醒是唯一「错了也不产生业务后果」的选项。

#### 4.9.2 定时任务

```rust
// flow/timeout.rs
pub const TASK_FLOW_TIMEOUT: &str = "flow_timeout_scan";
pub const CRON_FLOW_TIMEOUT: &str = "0 */5 * * * *";   // 每 5 分钟

pub fn register_builtin(scheduler: &Scheduler) {
    scheduler.register_cron(TASK_FLOW_TIMEOUT, CRON_FLOW_TIMEOUT, "系统任务：审批超时提醒/处理", || {
        tokio::spawn(async move { /* scan + act */ })
    });
}
```

扫描 SQL（走部分索引 `idx_biz_flow_task_due`，实测 0.008 ms / 0 行）：

```sql
SELECT id, instance_id, instance_node_id, assignee_id, due_at, remind_count
  FROM biz_flow_task
 WHERE status = 'PENDING' AND due_at < now()
 ORDER BY due_at
 LIMIT 500;                 -- 单批上限，处理完下一轮继续
```

**为什么要有 `LIMIT 500`**：如果某次故障导致 5 万条同时超时，一次全捞出来会把事务拖长到分钟级，还会把通知模块打爆（`dispatch` 单次上限 5 万人）。分批 + 下一轮继续，天然限流。

**幂等**：每个待办在一个「提醒周期」里只处理一次——用 `last_remind_at` + `remind_count` 判定，更新时带 `WHERE remind_count = $old`（CAS），并发两个实例的扫描任务不会重复提醒。

### 4.10 抄送

- `CC` 节点：解析收件人 → 写 `biz_flow_cc`（`uk(instance_id, node_key, round, receiver_id)` 防重）→ 节点 `SKIPPED` → 立即往下走（**不阻塞**）。
- 节点上还可配 `cc_on_approved = true`：该节点**通过时**抄送给一批人（常见：「财务通过后抄送财务总监」）。
- 已读：`is_read` / `read_at`，接口 `POST /flow/cc/{id}/read`。
- 通知：一次 `dispatch(target = User([...]), template = 'flow_cc')`。

**为什么抄送不进 `biz_flow_task`**（D22）：见 §0.1。补充一条实测理由：`uk_biz_flow_task_pending (instance_node_id, assignee_id) WHERE status='PENDING'` 是本设计的核心防重索引——如果抄送行也进这张表，抄送人会占用这个唯一槽位，导致「同一个人既是抄送人又是审批人」时插不进去。

### 4.11 三个关键时序

#### 时序 1：单签审批通过（含任务回调）

```
用户          前端              后端 flow::handler            PG                        notice
 │  点"同意"    │                    │                        │                          │
 ├────────────►│ POST /flow/task/{id}/approve               │                          │
 │             ├───────────────────►│ BEGIN                  │                          │
 │             │                    ├─ SELECT task FOR UPDATE ───────►│                 │
 │             │                    ├─ SELECT node FOR UPDATE ───────►│                 │
 │             │                    ├─ UPDATE task → APPROVED ──────►│                 │
 │             │                    ├─ COUNT(*)... 现算 ───────────►│                 │
 │             │                    ├─ UPDATE node (overwrite) ─────►│                 │
 │             │                    ├─ judge() = Pass                                  │
 │             │                    ├─ activate(next) ─ INSERT node + task ──►│        │
 │             │                    ├─ changelog::record(STATUS) ───►│                 │
 │             │                    ├─ if 实例结束: update instance + callback_to_biz    │
 │             │                    │      └─ UPDATE biz_task REVIEW→DONE (flow_gate)   │
 │             │                    ├─ COMMIT              │                          │
 │             │                    ├─ notice::dispatch(下一节点审批人, force=true) ────►│
 │             │◄───────────────────┤ 200 {instance_status, next_nodes, done:false}     │
```

**注意事务边界**：`dispatch` 在 `COMMIT` **之后**调用（通知失败不回滚业务）。但 `changelog::record` 在事务**内**（回滚不留假日志）。

#### 时序 2：会签 3 人，第 2 个同意时达到阈值（`RATIO(0.6)`）

```
A 同意 (t=0)  → a=1, r=0, p=2, n=3 → 1/3=0.33 < 0.6 → Wait；节点 approved_count=1
B 同意 (t=5s) → 【行锁串行】→ a=2, r=0, p=1, n=3 → 2/3=0.67 ≥ 0.6 → Pass
                 → 同节点剩余 PENDING 待办（C）→ CANCELLED，comment='已达通过比例，无需再投'
                 → 激活下游节点
C 打开待办 (t=6s) → 待办已 CANCELLED → 页面显示"该节点已由 2/3 通过，无需处理"
```

#### 时序 3：驳回并退回发起人 → 发起人重新提交

```
审批人 驳回(return_to=INITIATOR, comment="金额凭证缺失")
  → 当前节点 REJECTED（round=1）
  → 其余待办 CANCELLED
  → START 节点开 round=2 行，status=ACTIVE
  → 生成 task(kind=RESUBMIT, assignee=发起人)
  → 实例 round_total=2，仍 PENDING
  → 通知发起人（force=true，模板 flow_return）
  → 【如果 biz_type=TASK_ACCEPT】回调 biz_task: REVIEW→DOING，reject_reason=comment

发起人 重新提交(comment="已补凭证")
  → RESUBMIT 待办 → APPROVED
  → START(round=2) → APPROVED → 激活第一个审批节点（round 取当前轮的 2）
  → 注意：第 1 轮的审批记录仍在时间线上，UI 分组显示"第 1 轮 / 第 2 轮"
```

---
## 5. 与任务管理集成（本批的关键价值点）

### 5.1 谁来发起

| 场景 | 发起人 | 说明 |
|---|---|---|
| **任务验收**（主场景） | **把任务从 `DOING` 推进到 `REVIEW` 的那个人**（`state.rs` 里 `DOING→REVIEW` 只允许 `ROLE_OWNER`，所以实际是**负责人**） | 他提交验收，他是发起人，审批人从他的部门/角色/创建人/分派人里解析 |
| 通用发起（`GENERIC`） | 任何有 `business:flow:instance:create` 的人 | 用于将来「请假/报销」等业务 |
| 系统发起（P1） | `trace_source=SYSTEM` | 例如「到期自动发起」 |

**为什么不是创建人/分派人发起**：发起人的语义是「我提出了这个请求，我要对它负责」。验收申请是「我做完了，请验」——提出者是负责人。而且发起人决定了两件事：① `INITIATOR_SUPERIOR` 从**谁的部门**往上找；② 自审策略拿谁跟审批人比。如果把发起人定成创建人，那么在「跨部门分派」场景下主管链会走到创建人的部门去，完全错误。

### 5.2 回调与状态机闸门（**唯一的既有代码改动，报批项**）

#### 5.2.1 问题

`state.rs` 的 `TRANSITIONS` 用**角色位掩码**表达「谁能推」（§1.2）。流程回调需要推 `REVIEW→DONE`，但：

1. 回调的 actor 是「系统」，不在 `ROLE_OWNER|ROLE_ASSIGNER|ROLE_CREATOR|ROLE_ADMIN` 任何一位里；
2. 审批通过的人可能是**任何被解析出来的人**（比如一个被指定为审批人的协作人），他不在掩码里；
3. 让回调「借用创建人的身份」是伪造审计主体——`sys_change_log.operator_id` 会写错人。

#### 5.2.2 方案：加一个 `flow_gate` 闸门（不改掩码表）

在 `Transition` 结构体上加一个字段：

```rust
// task/state.rs 的 Transition 结构体（现有 8 个字段之后）
/// 审批流闸门：该流转是否受审批实例约束。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowGate {
    /// 不受审批流影响（默认；本批之前的所有流转都是这个）。
    None,
    /// 任务已挂审批单且实例 PENDING 时，**禁止任何人手动**推送本流转。
    /// → 拒：409 + 65030「该任务正在审批中（FLW-…），请在审批单里处理」
    BlockedWhilePending,
    /// 只允许「流程回调」推送（人工一律拒）。
    CallbackOnly,
}

// TRANSITIONS 表的两行改动
Transition { from: Review, to: Done,  flow_gate: FlowGate::CallbackOnly,      .. }  // 有审批单时
Transition { from: Review, to: Doing, flow_gate: FlowGate::CallbackOnly,      .. }
// 其余 8 条不变（FlowGate::None）
```

判定逻辑放在 `state::check_transition` 里（**一处**，不是每个 handler 各写一遍）：

```
fn check(transition, actor, task) -> Result<(), TransitionError>:
    // ① 有审批单且实例 PENDING → 人工路径一律拦
    if task.approval_instance_id.is_some() and instance_status == PENDING:
        if actor.kind == Human { return Err(ApprovalPending) }        // 65030
        if actor.kind == FlowCallback and transition.flow_gate == CallbackOnly { return Ok(()) }
        return Err(ApprovalPending)
    // ② 无审批单 → 老逻辑（掩码判定）
    ...
```

**为什么用「任务上有没有 `approval_instance_id`」而不是「流程定义上开没开审批」**：任务可能是在「开审批之前」创建的（存量任务），也可能流程后来被停用了。以**任务自身的字段**为准，语义是「这张单确实挂了审批」，而不是「配置上应该有审批」。

#### 5.2.3 三种 actor

```rust
pub enum ActorKind {
    Human,                    // HTTP 请求，claims 里的用户
    FlowCallback(Uuid),       // 流程引擎回调，携带 instance_id（审计主体记为"系统(流程 FLW-…)"）
    Robot,                    // AI 执行者（user_type=ROBOT）
}
```

**审计**：`FlowCallback` 的 `sys_change_log.operator_id` 记 `NULL`，`operator_name` 记 `"系统(审批 FLW-202610-000123)"`，`trace_source` 记 `'SYSTEM'`（`sys_change_log` 的 `trace_source` CHECK 已有这个值）。**不要**借发起人或审批人的 id。

#### 5.2.4 报批说明

这是本批**唯一**动既有业务代码的地方（`task/state.rs` 的 `Transition` 结构体 + `check_transition` 函数）。改动量：+1 个枚举（3 值）、+1 个字段（10 条 `TRANSITIONS` 构造点各补一行）、+1 个错误分支。**不改** `TRANSITIONS` 的 10 条流转语义、不改掩码、不改 `biz_task` 表。

### 5.3 双向引用

```
biz_task.approval_instance_id ──┐
                                ├──► biz_flow_instance.id
biz_flow_instance.biz_type ─────┘
biz_flow_instance.biz_id  ──────────► biz_task.id
```

| 方向 | 怎么取 | 接口 |
|---|---|---|
| 任务 → 审批单 | `biz_task.approval_instance_id`（单值） | `GET /business/task/{id}` 的响应里加 `approval` 块 |
| 审批单 → 任务 | `instance.biz_type='TASK_ACCEPT' AND instance.biz_id = task_id` | `GET /business/flow/instance/{id}` 的响应里加 `biz` 块 |

**任务详情的 `approval` 块**（加进 `TaskDetailRes`，`task/dto.rs:452`——**注意这是 DTO 改动，不是表结构改动**）：

```json
{
  "approval": {
    "instance_id": "…",
    "instance_no": "FLW-202610-000123",
    "status": "PENDING",
    "current_nodes": [
      { "node_key": "n_finance", "node_name": "财务会签", "status": "ACTIVE",
        "approved_count": 1, "approver_count": 3 }
    ],
    "my_task_id": "…",           // 当前用户在待办里有单子就给他 id，前端直接出"去审批"按钮
    "started_at": "2026-10-29T10:00:00+08:00"
  }
}
```

**性能**：这是详情页的单条查询，用 `biz_task` 主键 + `idx_biz_flow_instance_biz` 两次索引查找，实测 `0.026 ms`（§7.2 Q8 同款）。

### 5.4 AI 审批人

| 场景 | 判定 | 结果 |
|---|---|---|
| 流程里 `USER` 显式指定了一个 ROBOT | **允许**（人选了） | 落待办；该 ROBOT 通过 `task_token` 流程接口审批 |
| `ROLE` 展开后混进 ROBOT | `node.allow_robot = false`（默认）→ 丢弃 | 若丢弃后为空 → 空审批人策略 |
| `ROLE` 展开后混进 ROBOT | `node.allow_robot = true` | 保留 |
| `INITIATOR_SUPERIOR` 命中的部门负责人是 ROBOT | 同 `ROLE` 规则 | 同上 |
| ROBOT 用网页登录审批 | **硬拒**（`ai_actor::login_forbidden`，`63201`） | 走 AI 专用 API 流程（T5 已交付） |

**验证清单（D5 卡验收要点）**：① ROBOT 用 `/business/flow/task/{id}/approve` + 任务级令牌能审；② ROBOT 用网页 cookie 调同一接口得 `63201`；③ `allow_robot=false` 时角色里的 ROBOT 不出现在待办里；④ `allow_robot=true` 时出现。

### 5.5 边界：任务侧的各种变化怎么传导到审批单

| 任务侧动作 | 审批单该怎么办 | 实现 |
|---|---|---|
| 任务被 `CANCELLED` | 审批单 → `CANCELLED` | 任务状态机进 `CANCELLED` 的分支里调 `flow::cancel_by_biz(id, "任务已取消")` |
| 任务被软删除 | 审批单 → `CANCELLED` | 同上（删除前先取消） |
| 任务被转派（`owner_id` 变） | **不动**审批单（发起人不变；若审批人来源是 `TASK_OWNER`，已冻结的人不变） | 无 |
| 任务被打回（`REVIEW→DOING`） | 审批单此时应该已经 `REJECTED`（是审批驳回触发的打回），**不重复处理** | 回调带 `source=flow`，任务侧不反向再调 flow |
| 任务的 `need_review` 从 true 改 false | **不动**（在跑的审批继续） | 无 |
| 审批单 `WITHDRAWN` | 任务 `REVIEW → DOING`？**不自动**：发起人撤回验收申请 = 他想继续干，任务回 `DOING` 是合理的 | 回调：`REVIEW→DOING`，`reject_reason='发起人撤回验收申请'` |

**最后一条需要拍板**（§14 未决项 O1）：撤回是否自动把任务拉回 `DOING`？本文倾向「是」（否则任务停在 `REVIEW` 且没有审批单，成为一张谁也不能动的死单）。

---

## 6. 数据模型（9 张表 · V0115–V0119）

### 6.0 表清单与关系

```
biz_flow_def ──1:N──► biz_flow_def_version ──1:N──► biz_flow_node ──1:N──► biz_flow_node_assignee
   │                        │                          │
   │                        └──1:N──► biz_flow_edge ───┘（from/to 各一条 FK）
   │
   └──1:N──► biz_flow_instance ──1:N──► biz_flow_instance_node ──1:N──► biz_flow_task
                    │                          ↑                               │
                    └──1:N──► biz_flow_cc       └───────── instance_node_id ────┘
```

| # | 表 | 行数量级（1 万用户 / 100 万实例） | 可变 | 归属迁移 |
|---|---|---|---|---|
| 1 | `biz_flow_def` | ~50 | ✅ | `V0115` |
| 2 | `biz_flow_def_version` | ~300 | 仅 DRAFT 可变 | `V0115` |
| 3 | `biz_flow_node` | ~3000 | 仅 DRAFT 可变 | `V0115` |
| 4 | `biz_flow_node_assignee` | ~5000 | 仅 DRAFT 可变 | `V0115` |
| 5 | `biz_flow_edge` | ~4000 | 仅 DRAFT 可变 | `V0115` |
| 6 | `biz_flow_instance` | 100 万 | 状态推进 | `V0116` |
| 7 | `biz_flow_instance_node` | ~300 万 | 状态推进 | `V0116` |
| 8 | `biz_flow_task` | ~300 万 | 一次写入后可改状态 | `V0116` |
| 9 | `biz_flow_cc` | ~100 万 | 已读位 | `V0116` |

**索引单独一条 `V0117`**（与批次 C+ 的 `V0100`/`V0101` 同款惯例：表与索引分文件，便于逐条 `EXPLAIN` 对应）。权限种子 `V0118`，定时任务与（可选）部门索引 `V0119`。

### 6.1 `V0115`：定义侧 5 张表（完整 DDL，已实跑通过）

```sql
-- ======================================================
-- V0115：审批流 · 定义侧（批次 D · D1）
--
-- 依据：cloudpivot-approval-architecture.md §3 / §6
-- 回滚：V0115__create_flow_def_tables.down.sql（逆序 DROP，无残留）
--
-- 为什么固定 5 张表一次建完：
--   它们是同一张"定义图"的 5 个切面（def 是身份、version 是快照边界、
--   node/edge 是图、node_assignee 是审批人引用）。拆成 5 条迁移只会让
--   同一个 schema 决策散落在 5 个文件里，回滚还要记清 DROP 顺序。
--
-- 三个刻意的建模决策：
-- 1. 枚举一律 VARCHAR + CHECK（不用 PG 原生 ENUM）——与 biz_task / sys_announcement
--    同一理由：原生 ENUM 加值要 DDL 且不能在事务里回滚。
-- 2. 节点/边的"呈现坐标"（position_x/y）**存在行上**而不是只存 JSONB：
--    编辑器保存时整图提交，坐标与语义一起落库，避免"图在 JSONB 里、坐标在别处"
--    两份真相。
-- 3. graph_snapshot 只在 PUBLISHED 时非空（CHECK 强制），DRAFT 阶段以行为准。
-- ======================================================

-- ---------- 1) 流程定义（稳定身份） ----------
CREATE TABLE biz_flow_def (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    flow_code          VARCHAR(64) NOT NULL,
    flow_name          VARCHAR(100) NOT NULL,
    biz_type           VARCHAR(32) NOT NULL,
    category           VARCHAR(32),
    description        TEXT,
    status             VARCHAR(16) NOT NULL DEFAULT 'DRAFT'
        CHECK (status IN ('DRAFT', 'PUBLISHED', 'DISABLED')),
    current_version_id UUID,
    current_version_no INT NOT NULL DEFAULT 0 CHECK (current_version_no >= 0),
    is_default         BOOLEAN NOT NULL DEFAULT FALSE,
    allow_auto_approve BOOLEAN NOT NULL DEFAULT FALSE,
    owner_dept_id      UUID,
    created_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    create_by          UUID,
    update_by          UUID,
    deleted_at         TIMESTAMPTZ,
    CONSTRAINT ck_biz_flow_def_code CHECK (flow_code ~ '^[a-z][a-z0-9_]{1,63}$'),
    CONSTRAINT ck_biz_flow_def_published_has_version
        CHECK (status <> 'PUBLISHED' OR current_version_id IS NOT NULL)
);

CREATE UNIQUE INDEX uk_biz_flow_def_code ON biz_flow_def (flow_code) WHERE deleted_at IS NULL;
CREATE UNIQUE INDEX uk_biz_flow_def_default ON biz_flow_def (biz_type)
    WHERE is_default AND deleted_at IS NULL;
CREATE INDEX idx_biz_flow_def_status ON biz_flow_def (status, biz_type);
```

> **`ck_biz_flow_def_published_has_version` 实测踩到一次**（写 seed 时）：
> ```
> ERROR: new row for relation "biz_flow_def" violates check constraint
>        "ck_biz_flow_def_published_has_version"
> ```
> 因为它强制「先有版本行，才能置 PUBLISHED」。这不是麻烦，是**发布事务的正确顺序被约束住了**：不可能出现「PUBLISHED 但指向一个不存在的版本」的半成品。

```sql
-- ---------- 2) 版本快照（不可变） ----------
CREATE TABLE biz_flow_def_version (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    def_id         UUID NOT NULL REFERENCES biz_flow_def(id) ON DELETE CASCADE,
    version_no     INT NOT NULL CHECK (version_no >= 1),
    version_status VARCHAR(16) NOT NULL DEFAULT 'DRAFT'
        CHECK (version_status IN ('DRAFT', 'PUBLISHED', 'ARCHIVED')),
    graph_hash     CHAR(64),
    graph_snapshot JSONB,
    node_count     INT NOT NULL DEFAULT 0 CHECK (node_count >= 0),
    edge_count     INT NOT NULL DEFAULT 0 CHECK (edge_count >= 0),
    change_note    VARCHAR(500),
    published_at   TIMESTAMPTZ,
    published_by   UUID,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    create_by      UUID,
    update_by      UUID,
    CONSTRAINT uk_biz_flow_def_version UNIQUE (def_id, version_no),
    CONSTRAINT ck_biz_flow_def_version_published
        CHECK (version_status <> 'PUBLISHED'
               OR (graph_hash IS NOT NULL AND graph_snapshot IS NOT NULL
                   AND published_at IS NOT NULL))
);

CREATE INDEX idx_biz_flow_def_version_def ON biz_flow_def_version (def_id, version_no DESC);
```

> **`uk_biz_flow_def_version`（UNIQUE 约束）已经生成唯一索引，不要再单独建一条同名索引。** 我在 POC 的初稿里多写了一条 `uk_biz_flow_def_version_current (def_id, version_no)`，实测 `pg_indexes` 里是**两条完全重复的索引**——已删。这类重复索引不会报错，只会白占写入开销，属于「上线后才发现」的典型。

```sql
-- ---------- 3) 节点 ----------
CREATE TABLE biz_flow_node (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    version_id          UUID NOT NULL REFERENCES biz_flow_def_version(id) ON DELETE CASCADE,
    node_key            VARCHAR(64) NOT NULL,
    node_name           VARCHAR(100) NOT NULL,
    node_type           VARCHAR(16) NOT NULL
        CHECK (node_type IN ('START','APPROVAL','COUNTERSIGN','CONDITION','PARALLEL','CC','END')),
    assignee_type       VARCHAR(24)
        CHECK (assignee_type IN ('USER','ROLE','INITIATOR','INITIATOR_SUPERIOR','DEPT_LEADER',
                                 'TASK_OWNER','TASK_CREATOR','TASK_ASSIGNER','FORM_VAR','ADMIN')),
    superior_levels     SMALLINT NOT NULL DEFAULT 1 CHECK (superior_levels BETWEEN 1 AND 10),
    form_var_key        VARCHAR(64),
    approval_mode       VARCHAR(8) CHECK (approval_mode IN ('ALL','RATIO','ANY')),
    approve_ratio       NUMERIC(4,3) CHECK (approve_ratio > 0 AND approve_ratio <= 1),
    reject_veto         BOOLEAN NOT NULL DEFAULT TRUE,
    empty_assignee      VARCHAR(16) NOT NULL DEFAULT 'SKIP'
        CHECK (empty_assignee IN ('SKIP','ERROR','FALLBACK_ADMIN')),
    self_approval       VARCHAR(8) NOT NULL DEFAULT 'SKIP'
        CHECK (self_approval IN ('ALLOW','SKIP','FORBID')),
    allow_robot         BOOLEAN NOT NULL DEFAULT FALSE,
    join_mode           VARCHAR(8) NOT NULL DEFAULT 'AND' CHECK (join_mode IN ('AND','OR')),
    fork_mode           VARCHAR(8) NOT NULL DEFAULT 'AND' CHECK (fork_mode IN ('AND','OR')),
    timeout_hours       INT CHECK (timeout_hours IS NULL OR timeout_hours BETWEEN 1 AND 8760),
    timeout_action      VARCHAR(16) NOT NULL DEFAULT 'REMIND'
        CHECK (timeout_action IN ('REMIND','AUTO_APPROVE','AUTO_REJECT','ESCALATE')),
    remind_before_hours INT CHECK (remind_before_hours IS NULL OR remind_before_hours >= 0),
    max_remind_times    SMALLINT NOT NULL DEFAULT 3 CHECK (max_remind_times BETWEEN 0 AND 20),
    cc_on_approved      BOOLEAN NOT NULL DEFAULT FALSE,
    position_x          INT NOT NULL DEFAULT 0,
    position_y          INT NOT NULL DEFAULT 0,
    sort_order          INT NOT NULL DEFAULT 0,
    config              JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uk_biz_flow_node UNIQUE (version_id, node_key),
    CONSTRAINT ck_biz_flow_node_key CHECK (node_key ~ '^[A-Za-z][A-Za-z0-9_]{0,63}$')
);

CREATE INDEX idx_biz_flow_node_version ON biz_flow_node (version_id, node_type);
```

```sql
-- ---------- 4) 节点审批人引用 ----------
CREATE TABLE biz_flow_node_assignee (
    node_id       UUID NOT NULL REFERENCES biz_flow_node(id) ON DELETE CASCADE,
    assignee_type VARCHAR(24) NOT NULL CHECK (assignee_type IN ('USER','ROLE','DEPT_LEADER')),
    ref_id        UUID NOT NULL,
    sort_order    INT NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (node_id, assignee_type, ref_id)
);

CREATE INDEX idx_biz_flow_node_assignee_ref ON biz_flow_node_assignee (assignee_type, ref_id);
```

> **CHECK 只允许三种 `assignee_type`**：因为只有这三种是「可枚举的 id」。`INITIATOR_SUPERIOR` / `TASK_OWNER` / `FORM_VAR` / `ADMIN` / `INITIATOR` 是运行时才能算出来的，落行没有意义（实测负面测试 N22 拒绝往这里插 `INITIATOR_SUPERIOR` 行）。

```sql
-- ---------- 5) 连线（条件在边上） ----------
CREATE TABLE biz_flow_edge (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    version_id      UUID NOT NULL REFERENCES biz_flow_def_version(id) ON DELETE CASCADE,
    edge_key        VARCHAR(64) NOT NULL,
    from_node_id    UUID NOT NULL REFERENCES biz_flow_node(id) ON DELETE CASCADE,
    to_node_id      UUID NOT NULL REFERENCES biz_flow_node(id) ON DELETE CASCADE,
    condition_expr  JSONB,
    condition_label VARCHAR(100),
    priority        INT NOT NULL DEFAULT 0,
    is_default      BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uk_biz_flow_edge UNIQUE (version_id, edge_key),
    CONSTRAINT ck_biz_flow_edge_not_self CHECK (from_node_id <> to_node_id),
    CONSTRAINT ck_biz_flow_edge_default_no_cond
        CHECK (NOT is_default OR condition_expr IS NULL)
);

CREATE INDEX idx_biz_flow_edge_from ON biz_flow_edge (version_id, from_node_id);
CREATE INDEX idx_biz_flow_edge_to   ON biz_flow_edge (version_id, to_node_id);
```

### 6.2 `V0116`：运行侧 4 张表（完整 DDL）

```sql
-- ======================================================
-- V0116：审批流 · 运行侧（批次 D · D2）
--
-- 四个刻意的建模决策：
-- 1. instance.version_id 是 ON DELETE RESTRICT：有实例引用的版本不能删。
-- 2. instance_no 全表唯一（含软删行）：编号不复用，审计可回溯（与 biz_task.task_no 同款）。
-- 3. uk_biz_flow_instance_biz_active 部分唯一索引：同一业务单据同时只能有一个
--    在跑的实例 —— 双击/重试/并发提交的数据库级兜底。
-- 4. instance_node 按"到达才建行"懒创建：不清空铺满全部节点，1 万实例 × 10 节点
--    的 10 万行浪费里，实际用到的可能只有 3 万。
-- ======================================================

-- ---------- 6) 实例 ----------
CREATE TABLE biz_flow_instance (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_no       VARCHAR(32) NOT NULL,
    def_id            UUID NOT NULL REFERENCES biz_flow_def(id) ON DELETE RESTRICT,
    version_id        UUID NOT NULL REFERENCES biz_flow_def_version(id) ON DELETE RESTRICT,
    version_no        INT NOT NULL,
    flow_code         VARCHAR(64) NOT NULL,
    flow_name         VARCHAR(100) NOT NULL,
    biz_type          VARCHAR(32) NOT NULL,
    biz_id            UUID,
    biz_no            VARCHAR(32),
    title             VARCHAR(200) NOT NULL,
    status            VARCHAR(16) NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING','APPROVED','REJECTED','CANCELLED','WITHDRAWN')),
    initiator_id      UUID NOT NULL REFERENCES sys_user(id),
    initiator_name    VARCHAR(50),
    initiator_dept_id UUID,
    form_data         JSONB NOT NULL DEFAULT '{}'::jsonb,
    result_comment    VARCHAR(1000),
    auto_passed_nodes INT NOT NULL DEFAULT 0,
    round_total       INT NOT NULL DEFAULT 1 CHECK (round_total >= 1),
    prev_instance_id  UUID REFERENCES biz_flow_instance(id),
    revision          INT NOT NULL DEFAULT 0,
    started_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at          TIMESTAMPTZ,
    due_at            TIMESTAMPTZ,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    create_by         UUID,
    update_by         UUID,
    deleted_at        TIMESTAMPTZ,
    CONSTRAINT ck_biz_flow_instance_end
        CHECK ((status = 'PENDING') = (ended_at IS NULL))
);

CREATE UNIQUE INDEX uk_biz_flow_instance_no ON biz_flow_instance (instance_no);
CREATE UNIQUE INDEX uk_biz_flow_instance_biz_active ON biz_flow_instance (biz_type, biz_id)
    WHERE status = 'PENDING' AND biz_id IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX idx_biz_flow_instance_initiator ON biz_flow_instance (initiator_id, started_at DESC);
CREATE INDEX idx_biz_flow_instance_def ON biz_flow_instance (def_id, status, started_at DESC);
CREATE INDEX idx_biz_flow_instance_biz ON biz_flow_instance (biz_type, biz_id);
```

> **`ck_biz_flow_instance_end` 用等值写法 `(status='PENDING') = (ended_at IS NULL)`** 而不是两个分支的 OR：它同时拦住「`PENDING` 却有结束时间」和「终态却没有结束时间」两种脏值（实测负面测试 N16/N17 双向都拒绝）。写成一个等式的好处是漏一个方向都不可能。

```sql
-- ---------- 7) 实例节点 ----------
CREATE TABLE biz_flow_instance_node (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id    UUID NOT NULL REFERENCES biz_flow_instance(id) ON DELETE CASCADE,
    node_id        UUID REFERENCES biz_flow_node(id) ON DELETE SET NULL,
    node_key       VARCHAR(64) NOT NULL,
    node_name      VARCHAR(100) NOT NULL,
    node_type      VARCHAR(16) NOT NULL,
    round          INT NOT NULL DEFAULT 1 CHECK (round >= 1),
    status         VARCHAR(16) NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING','ACTIVE','APPROVED','REJECTED','SKIPPED','CANCELLED')),
    approval_mode  VARCHAR(8),
    approve_ratio  NUMERIC(4,3),
    reject_veto    BOOLEAN,
    approver_count INT NOT NULL DEFAULT 0 CHECK (approver_count >= 0),
    approved_count INT NOT NULL DEFAULT 0 CHECK (approved_count >= 0),
    rejected_count INT NOT NULL DEFAULT 0 CHECK (rejected_count >= 0),
    taken_edge_key VARCHAR(64),
    is_auto_passed BOOLEAN NOT NULL DEFAULT FALSE,
    activated_at   TIMESTAMPTZ,
    ended_at       TIMESTAMPTZ,
    duration_ms    BIGINT,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uk_biz_flow_instance_node UNIQUE (instance_id, node_key, round)
);

CREATE INDEX idx_biz_flow_instance_node_inst ON biz_flow_instance_node (instance_id, round);
CREATE INDEX idx_biz_flow_instance_node_stat ON biz_flow_instance_node (node_key, status, duration_ms)
    WHERE ended_at IS NOT NULL;
CREATE INDEX idx_biz_flow_instance_node_active ON biz_flow_instance_node (activated_at DESC)
    WHERE status = 'ACTIVE';
```

> **`node_id` 是 `ON DELETE SET NULL` 而不是 `CASCADE`**：定义侧的节点被删（只可能发生在删 DRAFT 版本时）不该把运行实例的节点行带走。而且 `instance_node` 已经冗余了 `node_key/node_name/node_type`，`node_id` 只是「指回定义」的便利外键，丢了不影响运行。
>
> **不能在 `instance_node` 上加「每个实例只能有一条 ACTIVE」的唯一索引**：并行网关下同时会有多条 `ACTIVE`（这正是并行的定义）。我第一版真加了一条 `uk_biz_flow_instance_node_active ON (instance_id) WHERE status='ACTIVE'`，写完 200 行就意识到它会把并行网关**焊死**，当场删掉——这类约束「看着很安全，实际把功能砍掉一半」。

```sql
-- ---------- 8) 待办 / 审批记录 ----------
CREATE TABLE biz_flow_task (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id         UUID NOT NULL REFERENCES biz_flow_instance(id) ON DELETE CASCADE,
    instance_node_id    UUID NOT NULL REFERENCES biz_flow_instance_node(id) ON DELETE CASCADE,
    node_key            VARCHAR(64) NOT NULL,
    round               INT NOT NULL DEFAULT 1,
    task_kind           VARCHAR(16) NOT NULL DEFAULT 'APPROVAL'
        CHECK (task_kind IN ('APPROVAL','RESUBMIT')),
    assignee_id         UUID NOT NULL REFERENCES sys_user(id),
    assignee_name       VARCHAR(50),
    assignee_source     VARCHAR(24) NOT NULL
        CHECK (assignee_source IN ('USER','ROLE','INITIATOR','INITIATOR_SUPERIOR','DEPT_LEADER',
                                   'TASK_OWNER','TASK_CREATOR','TASK_ASSIGNER','FORM_VAR','ADMIN')),
    assignee_source_ref UUID,
    status              VARCHAR(16) NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING','APPROVED','REJECTED','CANCELLED','SKIPPED','TRANSFERRED')),
    is_added_sign       BOOLEAN NOT NULL DEFAULT FALSE,
    is_self_approval    BOOLEAN NOT NULL DEFAULT FALSE,
    is_auto             BOOLEAN NOT NULL DEFAULT FALSE,
    comment             VARCHAR(1000),
    attachments         JSONB NOT NULL DEFAULT '[]'::jsonb,
    transferred_from    UUID REFERENCES biz_flow_task(id) ON DELETE SET NULL,
    transferred_to      UUID,
    due_at              TIMESTAMPTZ,
    remind_count        SMALLINT NOT NULL DEFAULT 0,
    last_remind_at      TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at        TIMESTAMPTZ,
    CONSTRAINT ck_biz_flow_task_done CHECK ((status = 'PENDING') = (completed_at IS NULL))
);

CREATE UNIQUE INDEX uk_biz_flow_task_pending ON biz_flow_task (instance_node_id, assignee_id)
    WHERE status = 'PENDING';
CREATE INDEX idx_biz_flow_task_mine ON biz_flow_task (assignee_id, status, created_at DESC);
CREATE INDEX idx_biz_flow_task_inst ON biz_flow_task (instance_id, created_at);
CREATE INDEX idx_biz_flow_task_node ON biz_flow_task (instance_node_id, status);
CREATE INDEX idx_biz_flow_task_due ON biz_flow_task (due_at)
    WHERE status = 'PENDING' AND due_at IS NOT NULL;
```

> **`idx_biz_flow_task_node` 是 POC 实测补出来的**（§7.4）：会签计票要从 `instance_node_id` 出发跨状态数票，而 `uk_biz_flow_task_pending` 是 `WHERE status='PENDING'` 的部分索引，服务不了「数 APPROVED 有几条」。补之前 **41.4 ms**，补之后 **0.099 ms**（418×）。
>
> **`ck_biz_flow_task_done` 同样用等值写法**：拦住「待办却有完成时间」（负面测试 N20 已验）。

```sql
-- ---------- 9) 抄送 ----------
CREATE TABLE biz_flow_cc (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    instance_id   UUID NOT NULL REFERENCES biz_flow_instance(id) ON DELETE CASCADE,
    node_key      VARCHAR(64) NOT NULL,
    round         INT NOT NULL DEFAULT 1,
    receiver_id   UUID NOT NULL REFERENCES sys_user(id),
    receiver_name VARCHAR(50),
    source_type   VARCHAR(24) NOT NULL,
    source_ref    UUID,
    is_read       BOOLEAN NOT NULL DEFAULT FALSE,
    read_at       TIMESTAMPTZ,
    notice_id     UUID,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uk_biz_flow_cc UNIQUE (instance_id, node_key, round, receiver_id)
);

CREATE INDEX idx_biz_flow_cc_mine ON biz_flow_cc (receiver_id, created_at DESC);
CREATE INDEX idx_biz_flow_cc_unread ON biz_flow_cc (receiver_id) WHERE NOT is_read;
```

### 6.3 `V0117`：索引汇总（新增 3 处，其余随表建）

```sql
-- 全部索引统一在此核对（与表内联建的不重复）。三条是 POC 补的：
-- ① idx_biz_flow_task_node      —— 会签计票 41.4ms → 0.099ms
-- ② idx_biz_flow_instance_node_active —— 审批中看板 39.1ms → 0.116ms
-- ③ idx_biz_flow_cc_unread      —— 「抄送未读」红点
CREATE INDEX IF NOT EXISTS idx_biz_flow_task_node            ON biz_flow_task (instance_node_id, status);
CREATE INDEX IF NOT EXISTS idx_biz_flow_instance_node_active ON biz_flow_instance_node (activated_at DESC)
    WHERE status = 'ACTIVE';
CREATE INDEX IF NOT EXISTS idx_biz_flow_cc_unread            ON biz_flow_cc (receiver_id) WHERE NOT is_read;
```

### 6.4 `V0118`：权限种子（10 个权限码 + 10 个菜单节点）

菜单 UUID 段：本批取 **`…0780`–`…0789`**。

**已用段（实测 `grep -rhoE '20000000-0000-0000-0000-[0-9]{12}' migrations/ | sort -u` 全库枚举，不是照抄注释）**：

| 段 | 占用者 |
|---|---|
| `…0001` | 根菜单 |
| `…0410`–`…0419` | 批次 A |
| `…0600`–`…0615`、`…0620`–`…0624` | 批次 B |
| `…0700`–`…0701` | C2 用户侧 |
| `…0710`–`…0716` | C3 公告 |
| `…0730`–`…0733` | C5 变更日志 |
| `…0740`–`…0749` | C4 帮助文档 |
| `…0750` | 内容管理目录 |
| `…0760`–`…0769` | 批次 C+（T1 任务 / T5b Hermes 集成） |
| `…0770`–`…0774` | **T7a 甘特图 + T6b 评论/工时**（`V0110` / `V0113`） |
| `…0790` | 文档结构根（`V0089`） |
| **`…0780`–`…0789`** | **本批（批次 D）** |

> **这一格是用血换来的**：本文初稿写的是 `…0770`–`…0779`，依据是 `V0103` 注释里那份"已用段"清单——但那份注释停在批次 C+ 的 T1，而 `V0110`（T7a 甘特）和 `V0113`（T6b 评论工时）后来又占走了 `…0770`–`…0774`。**照抄注释就会撞号**，所以本批的做法是：**每次都重新全库枚举，不信任何注释**。实施 `V0118` 时再枚举一次（期间可能又有别的卡动了 `…0775`–`…0779`）。

| 菜单 UUID 尾号 | `auth_code` | 类型 | 说明 |
|---|---|---|---|
| `…0780` | `business:flow:instance:read` | 2（页面） | 「审批中心」`/flow` |
| `…0781` | `business:flow:instance:read` | 3 | 查看审批单（待办/我发起/抄送我/详情） |
| `…0782` | `business:flow:instance:create` | 3 | 发起审批 |
| `…0783` | `business:flow:approve` | 3 | 同意/驳回/转办/加签 |
| `…0784` | `business:flow:instance:withdraw` | 3 | 撤回自己发起的 |
| `…0785` | `business:flow:def:read` | 3 | 查看流程定义 |
| `…0786` | `business:flow:def:write` | 3 | 新建/编辑/保存草稿 |
| `…0787` | `business:flow:def:publish` | 3 | 发布 / 停用 |
| `…0788` | `business:flow:admin` | 3 | 终止 / 退回任意节点 / 改派 |
| `…0789` | `business:flow:stats:read` | 3 | 效率统计 |

```sql
-- 幂等：固定 UUID + ON CONFLICT (id) DO NOTHING（不覆盖运维改名后的标题）
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
 ('20000000-0000-0000-0000-000000000780'::uuid,'审批中心',2,'business:flow:instance:read',
  '/flow',NULL,'business/flow/index',70,1,'flow','审批中心',TRUE,'business'),
 ('20000000-0000-0000-0000-000000000781'::uuid,'business:flow:instance:read',3,
  'business:flow:instance:read',NULL,'20000000-0000-0000-0000-000000000780'::uuid,
  NULL,10,1,NULL,'查看审批单（待办 / 我发起 / 抄送我 / 详情）',FALSE,'business'),
 ('20000000-0000-0000-0000-000000000782'::uuid,'business:flow:instance:create',3,
  'business:flow:instance:create',NULL,'20000000-0000-0000-0000-000000000780'::uuid,
  NULL,20,1,NULL,'发起审批',FALSE,'business'),
 ('20000000-0000-0000-0000-000000000783'::uuid,'business:flow:approve',3,
  'business:flow:approve',NULL,'20000000-0000-0000-0000-000000000780'::uuid,
  NULL,30,1,NULL,'审批（同意 / 驳回 / 转办 / 加签）',FALSE,'business'),
 ('20000000-0000-0000-0000-000000000784'::uuid,'business:flow:instance:withdraw',3,
  'business:flow:instance:withdraw',NULL,'20000000-0000-0000-0000-000000000780'::uuid,
  NULL,40,1,NULL,'撤回自己发起的审批',FALSE,'business'),
 ('20000000-0000-0000-0000-000000000785'::uuid,'business:flow:def:read',3,
  'business:flow:def:read',NULL,'20000000-0000-0000-0000-000000000780'::uuid,
  NULL,50,1,NULL,'查看流程定义',FALSE,'business'),
 ('20000000-0000-0000-0000-000000000786'::uuid,'business:flow:def:write',3,
  'business:flow:def:write',NULL,'20000000-0000-0000-0000-000000000780'::uuid,
  NULL,60,1,NULL,'新建 / 编辑流程（草稿）',FALSE,'business'),
 ('20000000-0000-0000-0000-000000000787'::uuid,'business:flow:def:publish',3,
  'business:flow:def:publish',NULL,'20000000-0000-0000-0000-000000000780'::uuid,
  NULL,70,1,NULL,'发布 / 停用流程',FALSE,'business'),
 ('20000000-0000-0000-0000-000000000788'::uuid,'business:flow:admin',3,
  'business:flow:admin',NULL,'20000000-0000-0000-0000-000000000780'::uuid,
  NULL,80,1,NULL,'流程管理员（终止 / 退回任意节点 / 改派）',FALSE,'business'),
 ('20000000-0000-0000-0000-000000000789'::uuid,'business:flow:stats:read',3,
  'business:flow:stats:read',NULL,'20000000-0000-0000-0000-000000000780'::uuid,
  NULL,90,1,NULL,'审批效率统计',FALSE,'business')
ON CONFLICT (id) DO NOTHING;
```

> **授权放种子迁移 `S__flow_permissions.sql`，不放在版本化迁移里**——这是 `V0103` 用血换来的时序结论：版本化迁移先跑、种子后跑，在**全新库**上「`INSERT … SELECT FROM sys_role`」会命中 0 行（那时 `SUPER_ADMIN` 还没建）。

### 6.5 `V0119`：定时任务种子 + 可选报批项

```sql
-- 两个内置任务由代码注册（Scheduler::register_builtin），这里只登记元数据供运维查看
INSERT INTO sys_job (id, job_name, job_group, cron_expr, invoke_target, status, remark)
VALUES
 ('30000000-0000-0000-0000-000000000f01'::uuid,'审批超时扫描','FLOW','0 */5 * * * *',
  'flow_timeout_scan',1,'超时提醒 / 按节点配置的自动处理'),
 ('30000000-0000-0000-0000-000000000f02'::uuid,'审批图快照自检','FLOW','0 30 3 * * *',
  'flow_graph_selfcheck',1,'核对 graph_snapshot 与行数据的 sha256；不一致告警')
ON CONFLICT (id) DO NOTHING;
```

**可选（需单独报批）**：`CREATE INDEX idx_sys_dept_leader ON sys_dept (leader_id) WHERE deleted_at IS NULL;` —— 服务于 `INITIATOR_SUPERIOR` 解析。部门表 < 1000 行时全表扫 0.1 ms 级，**不批也能跑**。

### 6.6 为什么是 9 张表（而不是 8 张或 12 张）

| 候选表 | 决定 | 理由 |
|---|---|---|
| `biz_flow_node_assignee` | ✅ **加**（任务书没列） | 见 §0.2 偏差 C：外键、反查、数组绑定三个理由 |
| `biz_flow_instance_edge`（记录走过哪条边） | ❌ **不加** | 只需知道「这个 CONDITION 走了哪条边」→ `instance_node.taken_edge_key` 一个字段够了。为每条边建一行是 3 倍数据量换一个字段 |
| `biz_flow_history`（审批历史单独表） | ❌ **不加** | `biz_flow_task` 本身就是「待办 + 已办」合一（这行一旦 `APPROVED` 就是历史）。再拆一张等于详情页要 `UNION` 两张表排序 |
| `biz_flow_log`（操作日志专表） | ❌ **不加** | `sys_change_log` 已有（分区表 + 脱敏 + 导出），映射 action 即可（D17） |
| `biz_flow_form_field`（表单字段定义） | ❌ P1 | §0.3 |
| `biz_flow_category`（流程分类字典） | ❌ **不加** | 用现有 `sys_dict`，`biz_flow_def.category` 存字典值 |
| `biz_flow_delegate`（委托代理） | ❌ P1 | §4.8.3 |

### 6.7 24 条负面约束实测（全部真拒绝）

`neg_test.sql` 在 `d_flow_v1` 上跑，每一条都在真库上返回 ERROR：

| # | 试图写入的脏数据 | 被哪条约束拒绝 |
|---|---|---|
| N1 | `flow_code='Bad-Code'`（大写+短横线） | `ck_biz_flow_def_code` |
| N2 | `status='PUBLISHED'` 但无 `current_version_id` | `ck_biz_flow_def_published_has_version` |
| N3 | `status='RUNNING'` | `biz_flow_def_status_check` |
| N4 | 同 `biz_type` 两个 `is_default` | `uk_biz_flow_def_default` |
| N5 | 同 def 同 `version_no` | `uk_biz_flow_def_version` |
| N6 | `PUBLISHED` 版本缺 `graph_hash`/`snapshot` | `ck_biz_flow_def_version_published` |
| N7 | `node_type='GATEWAY'` | `biz_flow_node_node_type_check` |
| N8 | `node_key='1abc'` | `ck_biz_flow_node_key` |
| N9 | 同版本 `node_key` 重复 | `uk_biz_flow_node` |
| N10 | `approve_ratio=1.5` | `biz_flow_node_approve_ratio_check` |
| N11 | `approval_mode='MAJORITY'` | `biz_flow_node_approval_mode_check` |
| N12 | 边自环 | `ck_biz_flow_edge_not_self` |
| N13 | `is_default=true` 且带条件 | `ck_biz_flow_edge_default_no_cond` |
| N14 | **同 `biz_id` 第二个 PENDING 实例** | `uk_biz_flow_instance_biz_active` |
| N15 | `instance_no` 重复 | `uk_biz_flow_instance_no` |
| N16 | `APPROVED` 但 `ended_at` 为空 | `ck_biz_flow_instance_end` |
| N17 | `PENDING` 但有 `ended_at` | `ck_biz_flow_instance_end` |
| N18 | 同实例同 `node_key` 同 `round` 重复 | `uk_biz_flow_instance_node` |
| N19 | **同节点同人两条 PENDING 待办** | `uk_biz_flow_task_pending` |
| N20 | 待办却带 `completed_at` | `ck_biz_flow_task_done` |
| N21 | `task_kind='CC'` | `biz_flow_task_task_kind_check` |
| N22 | `node_assignee` 落 `INITIATOR_SUPERIOR` 行 | `biz_flow_node_assignee_assignee_type_check` |
| N23 | 抄送重复 | `uk_biz_flow_cc` |
| N24 | 删除有实例引用的 `flow_def` | `biz_flow_instance_def_id_fkey`（RESTRICT） |

**这 24 条的意义**：它们各自对应一个「上线后才会发现」的脏数据形态。其中 N14（防双击发起）、N19（防重复投票）、N18（防重复激活）三条直接对应本设计的三个并发风险点——**数据库级兜底，即使应用层写漏了也不会脏**。

### 6.8 迁移回滚顺序

```sql
-- V0116.down.sql（先删引用方）
DROP TABLE IF EXISTS biz_flow_cc;
DROP TABLE IF EXISTS biz_flow_task;
DROP TABLE IF EXISTS biz_flow_instance_node;
DROP TABLE IF EXISTS biz_flow_instance;      -- 注意 prev_instance_id 自引用，DROP 整表可解

-- V0115.down.sql（再删被引用方）
DROP TABLE IF EXISTS biz_flow_edge;
DROP TABLE IF EXISTS biz_flow_node_assignee;
DROP TABLE IF EXISTS biz_flow_node;
DROP TABLE IF EXISTS biz_flow_def_version;
DROP TABLE IF EXISTS biz_flow_def;
```

**`biz_task.approval_instance_id` 不删**（它本来就存在，属于批次 C+ 的表）；本模块回滚前必须先把所有 `approval_instance_id` 置 `NULL` 并确认没有 `PENDING` 实例（否则回滚等于丢审批单）。

---
## 7. 性能实测（10 万实例 / 30 万节点 / 30 万待办 / 10 万抄送）

### 7.1 数据规模与体积

灌数脚本 `seed.sql`（`d_flow_v3`），实测耗时：

```
INSERT 0 300000   300000 实例节点   3.64 s
INSERT 0 300000   300000 待办       5.21 s
INSERT 0 100000   100000 抄送       1.21 s
ANALYZE                             3.59 s
```

| 表 | 行数 | 总大小 | 堆 | 索引 | 每行均摊 |
|---|---|---|---|---|---|
| `biz_flow_task` | 300,000 | **110 MB** | 55 MB | 54 MB | ~366 B |
| `biz_flow_instance_node` | 300,000 | **95 MB** | 54 MB | 41 MB | ~317 B |
| `biz_flow_instance` | 100,000 | **65 MB** | 37 MB | 27 MB | ~650 B |
| `biz_flow_cc` | 100,000 | **29 MB** | 12 MB | 17 MB | ~290 B |
| `biz_flow_def` | 1 | 80 kB | 8 kB | 72 kB | — |
| `biz_flow_def_version` | 1 | 80 kB | 8 kB | 72 kB | — |
| `biz_flow_node` | 5 | 64 kB | 8 kB | 56 kB | — |
| `biz_flow_edge` | 0 | 40 kB | 0 | 40 kB | — |
| `biz_flow_node_assignee` | 0 | 16 kB | 0 | 16 kB | — |

**容量推算**：100 万实例 ≈ **3.0 GB**（4 张运行表合计 299 MB × 10）；加上索引膨胀与分区粒度，规划 **5 GB / 100 万实例**。定义一个流程约 2–8 KB（含快照），100 个流程不到 1 MB——**定义侧可以忽略不计**。

> 索引体积占比 ~38%（54/110）偏高，原因是本设计对「我的待办」「某实例时间线」「计票」「超时」四条路径各建了一条索引。这是刻意的取舍：**这四条的查询频率远高于写入频率**（一张单被看 20 次、被写 5 次）。

### 7.2 六条热路径的真实数字

| # | 查询 | 入口页 | 计划 | 实测 |
|---|---|---|---|---|
| **Q1** | 我的待办（第 1 页，20 条 + JOIN 实例摘要） | 审批中心首屏 | `Bitmap Index Scan on idx_biz_flow_task_mine` → `Nested Loop` → `Index Scan biz_flow_instance_pkey` | **0.473 ms** |
| **Q2** | 我的待办总数（红色角标） | 顶栏 | `Bitmap Index Scan on idx_biz_flow_task_mine` | **0.071 ms** |
| **Q4** | 我发起的（分页） | 「我发起」tab | `Bitmap Index Scan on idx_biz_flow_instance_initiator` | **0.296 ms** |
| **Q5** | 抄送我的 | 「抄送我」tab | `Bitmap Index Scan on idx_biz_flow_cc_mine` | **0.214 ms** |
| **Q6** | 超时扫描（定时任务，LIMIT 500） | 后台 | `Index Scan using idx_biz_flow_task_due` | **0.008 ms**（0 行） |
| **Q8** | 审批详情页（实例 + 节点 + 时间线，3 条 SQL） | 详情页 | `Index Scan pkey` / `idx_..._node_inst` / `idx_biz_flow_task_inst` | **0.026 / 0.198 / 0.103 ms** |
| **Q11** | 某流程下的进行中单据（管理视角） | 流程详情 | `Index Scan using idx_biz_flow_instance_def` | **0.049 ms** |
| **Q12** | 平均审批时长（实例级聚合） | 统计页 | `Parallel Seq Scan`（3 worker） | **24.8 ms** |

**结论**：前 7 条全部 < 1 ms（索引全部命中），统计类 25 ms。这在「1 万用户 / 10 万实例」的量级上完全够用。

### 7.3 可见性：四种写法的对拍（本节是本批最重要的性能结论）

审批单的可见性谓词是「我发起的 ∪ 我审批过 ∪ 抄送我的 ∪ 数据权限覆盖的部门」。四种写法在**同一份数据、同一台机器**上实测：

| 写法 | SQL 形态 | 执行计划 | 实测 | 结论 |
|---|---|---|---|---|
| **A. 相关 `EXISTS`** | `initiator_id=? OR EXISTS(task…) OR EXISTS(cc…) OR dept IN (…)` | `Seq Scan on biz_flow_instance`（100k 行）+ 每行 2 次索引查找 | **227.3 ms** | ❌ OR 打断了索引选择；相关子查询按行执行 |
| **B. `= ANY(<子查询产出的数组>)`** | `id = ANY((SELECT '{…}'::text::uuid[]))` | `Parallel Seq Scan` + **逐行数组匹配** | **776.2 ms** | ❌ **比 A 还差 3.4×**：数组来自子查询，规划器拿不到常量值，无法转成索引条件 |
| **C. `id IN (SELECT …)`** | 非相关 UNION 子查询 | `Parallel Seq Scan` + Hash Semi Join | **36.6 ms** | ⚠️ 可用但慢 40×：还是一次 10 万行扫描 |
| **D. 应用层预解析 id 集合 → 参数绑定** | `id = ANY($1::text::uuid[])`（**常量参数**） | `Nested Loop` + `Index Scan biz_flow_instance_pkey` × N | **0.887 ms** | ✅ **采用** |
| **A+D 混合**（部门范围 + id 集合） | `dept_id = ANY($1) OR id = ANY($2)` | `Parallel Seq Scan` | **22.9 ms** | ⚠️ 见下 |

**关键发现（可以直接写进代码注释的三条）**：

1. **`= ANY` 只有在右侧是「常量/绑定参数」时才能进索引**。右侧是子查询产出的数组时，PG 拿不到值，退化成「每行做一次数组匹配」——776 ms，是所有写法里**最差**的。这个坑很隐蔽：写起来最"现代"的写法性能最糟。
2. **相关 `EXISTS` 在 10 万行上是灾难**：227 ms。它的成本是「候选行数 × 每行 2 次索引查找」。任务模块早就踩过这个坑并给出了正确形态（`model.rs:99` 的 `VISIBLE_OR_TAIL`），本批照抄。
3. **混合谓词（dep OR id-set）仍然会退化成并行全扫**（22.9 ms）。这是可接受的：并行全扫 10 万行只要 22 ms。但如果实例数到 100 万，这条会变成 200 ms+，届时的解法是「把部门集合也预解析成 id 集合，合成一个 id 的 `IN`」（P1）。

**最终实现（与 `biz_task` 同款，`scope.rs` 一行不改）**：

```rust
// flow/model.rs
pub const FLOW_VISIBLE_OR_TAIL: &str =
    " AND ( (1 = 1 {FILTER}) OR i.id = ANY(?::text::uuid[]) )";

/// 我的关系实例 id 集合（发起 ∪ 审批过 ∪ 抄送）。上限 5000，超出置 truncated。
pub async fn my_instance_ids(me: Uuid) -> DbResult<MyFlowScopeIds> {
    // 实测：这三个 UNION 全走索引，0.245 ms（Q10）
}
```

> **上限与截断**：`my_instance_ids` 的返回条数上限 **5000**，超出时返回 `truncated = true`（DTO 字段，前端提示「你参与的单据过多，已按最近 5000 条过滤，请用筛选器缩小范围」）。这是批次 C+ 的 `member_scope_truncated` 同款处理——**不截断的后果是一条 SQL 带 3 万个 uuid 参数，要么超 PG 参数上限，要么慢到超时**。

### 7.4 两条索引缺口的发现与修复（POC 的真实价值）

初版 DDL 里我以为索引够了，实测发现两处**计划根本没用到索引**：

| 场景 | 初版实测 | 缺的索引 | 补索引后 |
|---|---|---|---|
| **会签计票**（同一 `instance_node` 数 APPROVED/REJECTED） | `Parallel Seq Scan` **41.4 ms** | `idx_biz_flow_task_node (instance_node_id, status)` | `Index Only Scan` **0.099 ms**（**418×**） |
| **审批中看板**（`status='ACTIVE'` 的节点列表） | `Parallel Seq Scan` **39.1 ms** | `idx_biz_flow_instance_node_active (activated_at DESC) WHERE status='ACTIVE'` | `Index Scan` **0.116 ms**（**337×**） |

**为什么会漏**：
- 会签计票那条，我原以为 `uk_biz_flow_task_pending (instance_node_id, assignee_id) WHERE status='PENDING'` 就够了。**它是部分索引**——只能服务「找 PENDING 的待办」，服务不了「数已通过的票」。部分索引的 `WHERE` 把其他状态排除在索引之外，这是一个非常容易想当然的地方。
- 审批中看板那条，我原来的 `idx_biz_flow_instance_node_stat (node_key, status, duration_ms) WHERE ended_at IS NOT NULL` 的条件恰好是 `ended_at IS NOT NULL`——把 `ACTIVE`（`ended_at` 为空）**排除在外**了。两条部分索引的谓词互相矛盾。

**这两个数字是本设计最实在的收获**：如果不做 POC，这两条会以「41 ms 和 39 ms」上线，在「审批中看板」这种每 30 s 轮询的页面上，100 个人在线就是每秒 13 次全表扫。

### 7.5 统计：卡在哪个节点

```sql
SELECT node_key, node_name, count(*) AS ended_cnt,
       round(avg(duration_ms))::bigint AS avg_ms,
       round(percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_ms))::bigint AS p95_ms
  FROM biz_flow_instance_node
 WHERE ended_at IS NOT NULL AND duration_ms IS NOT NULL
 GROUP BY node_key, node_name ORDER BY avg_ms DESC;
```

实测：**159.9 ms**（`Seq Scan` 245,000 行 + `external merge` 9.4 MB 排序）。

| 结论 | 数字 |
|---|---|
| 全量聚合（不设时间窗） | 159.9 ms，走 `Seq Scan`（部分索引 `idx_..._stat` 用不上：分组要读全部行，索引扫描反而更贵） |
| 加时间窗（近 7 天） | 210.8 ms（我的种子数据全在 7 天内，等于没筛掉；真实场景会显著下降） |
| 管理员页面可接受？ | 可以（159 ms，非高频） |

**P0 的处理**：
- 统计接口**强制带时间窗**（默认近 30 天，上限 180 天，与 `dep::MAX_RANGE_DAYS` 同款）；
- 结果**缓存 300 s**（key = `def_id + 时间窗`，进程内 LRU）；
- **不做**按天预聚合表（那是 BI 的工作，见大数据批次已交付的 Doris 管道）。

**P1 触发条件**：统计页打开超过 1 s，或实例数破 100 万 → 加 `biz_flow_stats_daily (stat_date, def_id, node_key, cnt, sum_ms)` 每日汇总。

### 7.6 会签计票的并发成本

同一节点 3 人会签，计票 SQL 在**行锁保护下**执行（§4.5.1 第 ④ 步）：

| 版本 | 计划 | 实测 |
|---|---|---|
| 无 `idx_biz_flow_task_node` | `Parallel Seq Scan` | 41.4 ms |
| 有该索引 | `Index Only Scan` | **0.099 ms** |

行锁持有的时间 = 0.099 ms（计票）+ 两条 UPDATE + 一条 INSERT，总事务约 0.5–1 ms。**这个量级意味着 3 个人同时点「同意」，后面的等 1 ms 就能进**——不会出现「会签人多就卡」的问题。

---

## 8. 环检测：为什么不能用递归 CTE（任务书假设的纠正）

### 8.1 三条实测证据

#### 证据 A：路径枚举式递归 CTE 在 200 节点上就爆了

造了一张**合法 DAG**：200 个节点、一条主链（0→1→…→199，199 条边）+ 60 条「跳跃边」（i→i+2，模拟条件分支后的汇合），共 259 条边、**无环**。

用 `UNION ALL + CYCLE` 跑「从 START 出发枚举所有路径」：

```
EXPLAIN (ANALYZE) WITH RECURSIVE walk(…) … CYCLE node_id SET is_cycle USING path …
→ 执行 300 秒未结束，最终被 pg_cancel_backend 取消
```

**原因**：`CYCLE` 子句只保证「同一条路径上不重复访问同一节点」，**不阻止枚举所有不同的简单路径**。跳跃边让路径数按斐波那契级增长——60 条跳跃边 ≈ $1.618^{60} \approx 10^{12}$ 条路径。

#### 证据 B：`UNION`（去重）能跑完，但**识别不出环**

```
--- 人为加一条回边 n199 -> n5 制造环 ---
WITH RECURSIVE reach(node_id) AS (
    SELECT id FROM biz_flow_node WHERE … node_type='START'
    UNION                                   -- ← 去重：同一个节点只进一次
    SELECT e.to_node_id FROM reach r JOIN biz_flow_edge e ON e.from_node_id = r.node_id
)
SELECT count(*) FROM reach;
→ 200 行，正常结束（有环也是 200 行）
```

**`UNION` 的去重语义让递归天然终止**——所以「这条 SQL 跑完了」**不能**证明「图无环」。这是最危险的误判：一个 `UNION` 写法的判环函数会 100% 通过所有环图。

#### 证据 C：`UNION ALL` + 深度上限，环被「截断」而不是被识别

```
WITH RECURSIVE walk(node_id, depth) AS (… UNION ALL … WHERE w.depth < 200)
SELECT count(*) FROM walk;
→ 跑到深度 200 就停，返回一堆重复行，没有任何 is_cycle 标记
```

「加个深度上限防止死循环」是常见补丁，代价是**再次失去正确性**（只防挂死，不判环）。

#### 证据 D：数据量证明「根本不需要在数据库里做」

```
-- 一次把整张图（200 节点 + 199 边）读出来
SELECT n.id, n.node_key, n.node_type, e.to_node_id
  FROM biz_flow_node n LEFT JOIN biz_flow_edge e ON e.from_node_id = n.id
 WHERE n.version_id = …;
→ Execution Time: 0.250 ms
```

**0.250 ms 就能把整张图装进内存**，然后在内存里做 O(V+E)=399 步的遍历（微秒级）。在所有方案里，**「读出来在内存里算」是唯一一个既能保证正确性、又没有任何爆炸风险的**。

### 8.2 应用层方案（与 T4 同款）

```rust
// flow/graph.rs —— 纯逻辑，不碰数据库，可单测

/// 发布校验 V3：无环。返回环路径（闭合，首尾相同）。
pub fn find_cycle(nodes: &[NodeRow], edges: &[EdgeRow]) -> Option<Vec<String>> {
    let mut indeg: HashMap<&str, usize> = nodes.iter().map(|n| (n.key.as_str(), 0)).collect();
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in edges {
        *indeg.get_mut(e.to.as_str())? += 1;
        out.entry(e.from.as_str()).or_default().push(e.to.as_str());
    }
    // Kahn：入度 0 入队，摘掉后减入度；队列空而仍有节点 → 剩下的都在环上
    let mut queue: VecDeque<&str> = indeg.iter().filter(|(_, d)| **d == 0).map(|(k, _)| *k).collect();
    let mut visited = 0usize;
    while let Some(k) = queue.pop_front() {
        visited += 1;
        for to in out.get(k).into_iter().flatten() {
            let d = indeg.get_mut(to).unwrap();
            *d -= 1;
            if *d == 0 { queue.push_back(to); }
        }
    }
    if visited == nodes.len() { return None; }
    // 剩下的节点都在环上 → DFS 找一条闭合路径
    Some(extract_cycle_path(&indeg, &out))
}
```

**与 T4 的对应关系**：

| T4（任务依赖） | D（流程定义） | 相同点 |
|---|---|---|
| `dep::find_cycle(edges, task_id, depends_on_id)`（`dep.rs:131`） | `graph::find_cycle(nodes, edges)` | 都在内存里，都返回**具体路径**而不是布尔值 |
| `dep::orient_cycle_from(path)`（`dep.rs:211`）：把路径重排成「从被编辑节点读起」 | `graph::orient_cycle_from(path)` | 同款：报错文案要从用户正在编辑的那个节点读起 |
| `MAX_CYCLE_DEPTH = 100`（`dep.rs:49`） | `MAX_NODES = 200` / `MAX_EDGES = 400`（发布校验 V12） | 都用上限而不是深度截断来兜底 |

**报错文案（V3）**：

```
检测到环：n_legal → n_finance → n_review → n_legal
（流程必须是无环的；分支请用「条件」节点并让所有分支最终汇合）
```

> 与 T4 的硬性要求一致：**不许只回「有环」**。用户拿着「有环」两个字在 200 个节点的图上找环，等于让他重画。

### 8.3 为什么不在应用层也做「深度上限截断」

流程图的规模上限是 **200 节点**（V12 硬限）。在 200 节点的图上，O(V+E) 遍历最多 600 步——**不存在跑不完的可能**。加「深度上限」只会引入「超过上限就当作无环」的假阴性。所以：
- **规模上限拦在发布期**（V12，`65017`）；
- **遍历本身不设深度上限**；
- 但**要设一个迭代次数保险**（`visited > nodes.len() * 2` 即判为内部错误 `500`），防止代码 bug 导致死循环。

---

## 9. 数据权限与审计

### 9.1 实例可见性谓词

```
可见(实例 I, 用户 U) ⇔
     I.deleted_at IS NULL
 AND (
        数据权限片段(U) 覆盖 I.initiator_dept_id          -- scope.rs 的 filter_for
     OR I.initiator_id = U
     OR I.id ∈ my_approval_instance_ids(U)                 -- 我待审 + 我已审
     OR I.id ∈ my_cc_instance_ids(U)                       -- 抄送我
     OR U 是 SUPER_ADMIN
 )
```

`my_approval_instance_ids` / `my_cc_instance_ids` 在应用层合并成一个 id 集合（一次 `UNION` 查询，实测 0.245 ms）。

**「已审批人」要不要能看**：**要**。他审过的单子后来被别人驳回了、被撤回了，他应该知道结果——否则「我审完就再也看不到这张单」是反直觉的。

### 9.2 五类角色的权限矩阵

| 角色 | 看列表 | 看详情 | 审批 | 撤回 | 终止 | 退回任意节点 | 改派 | 看统计 |
|---|---|---|---|---|---|---|---|---|
| **发起人** | ✅（自己发起的） | ✅ | ❌（自己不是审批人时） | ✅（零人已审） | ❌ | ❌ | ❌ | ❌ |
| **当前审批人** | ✅（待办里） | ✅ | ✅ | ❌ | ❌ | ❌ | ❌（可转办） | ❌ |
| **已审批人** | ✅（我审过的） | ✅（只读） | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **抄送人** | ✅（抄送我） | ✅（只读） | ❌ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **流程管理员** | ✅（全量，走数据权限） | ✅ | ❌（不代替审批人） | ❌ | ✅ | ✅ | ✅（P1） | ✅ |

> **「流程管理员不能代替审批人投票」是刻意的**：管理员能终止、能退回，但不能替别人点「同意」。否则审计里会出现「管理员同意了 12 张单」这种无法解释的记录。真需要时走「转办/改派」路径，责任落到具体的人。

### 9.3 `sys_change_log` 接入（映射表 + 白名单）

**三个 `ResourceSpec` 登记到 `changelog/diff.rs` 的 `REGISTRY`**（`diff.rs:170` 是 `BIZ_TASK` 的现成样例）：

| 资源 | `action` | 白名单字段 | 为什么这些字段 |
|---|---|---|---|
| `biz_flow_def` | `CREATE` / `UPDATE` / `PUBLISH` / `OFFLINE` / `DELETE` | `flow_name` `biz_type` `status` `current_version_id` `is_default` `allow_auto_approve` | 「流程什么时候从 v2 换到 v3」「谁把它停用了」是审计的核心问题 |
| `biz_flow_instance` | `CREATE` / `STATUS` | `status` `ended_at` `result_comment` `round_total` `version_id` | 一张单的生死与版本绑定 |
| `biz_flow_task` | `STATUS` / `ASSIGN` | `status` `comment` `transferred_to` `is_added_sign` `is_auto` | 「谁在第几步签了什么」「这一票是不是系统代点的」 |

**刻意不入白名单的字段**（与 `BIZ_TASK` 同一个理由）：
- `updated_at` / `update_by`：进白名单 = 把审计表变成访问日志；
- `form_data`：大 JSONB，每次流转都不变，进白名单只白占 `big` 摘要；
- `remind_count` / `last_remind_at`：提醒是系统行为，不是业务变更（有独立日志）。

**注意 `sys_change_log` 的分区特性**：主键是 `(id, created_at)`，**不派生单主键 `Model::find`**，详情接口必须带 `created_at` 才能分区裁剪（`V0084` 文件头第 1 点）。审批详情的「操作历史」查询必须写成：

```sql
SELECT … FROM sys_change_log
 WHERE resource = 'biz_flow_instance' AND resource_id = $1
   AND created_at >= $2 AND created_at < $3     -- ← 必须带，否则扫全部分区
 ORDER BY created_at;
```

### 9.4 审计必须记的 12 件事

| # | 事件 | `resource` / `action` | 必须带的上下文 |
|---|---|---|---|
| 1 | 发起 | `instance` / `CREATE` | `version_no`、`form_data` 摘要、`biz_id` |
| 2 | 同意 | `task` / `STATUS` | `node_key`、`round`、`comment`、`is_auto` |
| 3 | 驳回（含退回目标） | `task` / `STATUS` | `return_to`（PREV/INITIATOR/node_key）、`reason` |
| 4 | 转办 | `task` / `ASSIGN` | 原 `assignee_id`、新 `assignee_id`、原因 |
| 5 | 加签 | `task` / `ASSIGN` | 被加签人、`approver_count` 变化 |
| 6 | 撤回 | `instance` / `STATUS` | 撤回时的节点快照、`result_comment` |
| 7 | 终止 | `instance` / `STATUS` | 终止人、理由（必填） |
| 8 | 超时自动处理 | `task` / `STATUS` | `is_auto=true`、`timeout_action`、`due_at` |
| 9 | 空审批人跳过 | `instance_node` / `STATUS`（P1 或走 tracing） | 节点、`empty_assignee` 策略 |
| 10 | 流程保存草稿 | `def` / `UPDATE` | `version_no` |
| 11 | 流程发布 | `def` / `PUBLISH` | `version_no`、`graph_hash`、`node_count`/`edge_count` |
| 12 | 流程停用 | `def` / `OFFLINE` | 停用时有几张在跑的实例（影响面） |

> 第 12 条的「影响面」值得单独说：停用一个还有 50 张在跑实例的流程，运维必须知道这 50 张会继续跑完（`DISABLED` 只拦新发起）。这个数字要写进审计，事后才说得清。

---

## 10. API 契约

### 10.1 端点清单（44 个）

`/api` 前缀由 `[router].prefix` 注入（与 `task/handler.rs` 一致）。

#### 10.1.1 流程定义（11）

| # | 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|---|
| 1 | GET | `/business/flow/def/page` | `business:flow:def:read` | 定义列表（分页 + 状态/业务类型筛选） |
| 2 | GET | `/business/flow/def/{id}` | `business:flow:def:read` | 定义详情（含当前发布版本摘要） |
| 3 | POST | `/business/flow/def` | `business:flow:def:write` | 新建（同时建 v1 DRAFT + START/END 两个默认节点） |
| 4 | PUT | `/business/flow/def/{id}` | `business:flow:def:write` | 改元信息（名称/分类/描述/是否默认） |
| 5 | DELETE | `/business/flow/def/{id}` | `business:flow:def:writ` → `write` | 软删（有 `PENDING` 实例 → 409 `65021`） |
| 6 | GET | `/business/flow/def/{id}/graph` | `business:flow:def:read` | **取图**（`?version_id=` 缺省取最新 DRAFT） |
| 7 | PUT | `/business/flow/def/{id}/graph` | `business:flow:def:write` | **保存整图**（节点+边+坐标，一次事务；只允许 DRAFT） |
| 8 | POST | `/business/flow/def/{id}/validate` | `business:flow:def:write` | 只校验不保存（返回 V1–V12 全部不通过项） |
| 9 | POST | `/business/flow/def/{id}/publish` | `business:flow:def:publish` | 发布（跑 V1–V12，算 `graph_hash`，写快照，切 `current_version_id`） |
| 10 | POST | `/business/flow/def/{id}/disable` | `business:flow:def:publish` | 停用（不改在跑实例） |
| 11 | GET | `/business/flow/def/{id}/versions` | `business:flow:def:read` | 版本列表（含发布人/时间/节点数/当前标记） |

#### 10.1.2 实例（12）

| # | 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|---|
| 12 | GET | `/business/flow/instance/page` | `business:flow:instance:read` | 实例分页（**可见性过滤** + 状态/流程/业务类型/时间窗） |
| 13 | POST | `/business/flow/instance` | `business:flow:instance:create` | 发起（`flow_code` 或 `def_id` + `title` + `form_data` + 可选 `biz_type/biz_id`） |
| 14 | GET | `/business/flow/instance/{id}` | `business:flow:instance:read` | 详情（实例 + 图 + 节点状态 + 时间线 + 抄送 + `my_task_id`） |
| 15 | GET | `/business/flow/instance/{id}/progress` | `business:flow:instance:read` | 轻量进度（只返回节点状态数组，供详情页轮询） |
| 16 | POST | `/business/flow/instance/{id}/withdraw` | `business:flow:instance:withdraw` | 撤回（零人已审才允许，`409 + 65020`） |
| 17 | POST | `/business/flow/instance/{id}/cancel` | `business:flow:admin` | 终止（必填理由） |
| 18 | GET | `/business/flow/instance/my` | `business:flow:instance:read` | 我发起的（按状态分组：进行中/已通过/已驳回/已撤回） |
| 19 | GET | `/business/flow/instance/{id}/tasks` | `business:flow:instance:read` | 审批时间线（按 `round` 分组，含 `is_auto`/`is_self_approval`） |
| 20 | POST | `/business/flow/instance/{id}/cc-read` | `business:flow:instance:read` | 批量置抄送已读 |
| 21 | GET | `/business/flow/instance/{id}/graph-progress` | `business:flow:instance:read` | 图的进度叠加（节点状态 + 当前节点高亮 + 走过的边） |
| 22 | POST | `/business/flow/instance/{id}/resubmit` | `business:flow:instance:create` | 退回发起人后的「重新提交」（改 `form_data` + 意见） |
| 23 | POST | `/business/flow/instance/{id}/migrate` | `business:flow:admin` | **P1 占位**（升到新版本，本批不实现，返回 501） |

#### 10.1.3 审批动作（9）

| # | 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|---|
| 24 | POST | `/business/flow/task/{id}/approve` | `business:flow:approve` | 同意（`comment` 可选） |
| 25 | POST | `/business/flow/task/{id}/reject` | `business:flow:approve` | 驳回（`return_to` + `comment` 必填） |
| 26 | POST | `/business/flow/task/{id}/transfer` | `business:flow:approve` | 转办（`to_user_id` + `reason`） |
| 27 | POST | `/business/flow/task/{id}/add-sign` | `business:flow:approve` | 并行加签（`user_ids[]`，≤ 10） |
| 28 | POST | `/business/flow/task/batch` | `business:flow:approve` | 批量同意/驳回（≤ 50 条，**逐条独立事务**） |
| 29 | GET | `/business/flow/task/{id}` | `business:flow:approve` | 待办详情（含业务上下文 + 可用动作按钮） |
| 30 | POST | `/business/flow/task/{id}/read` | `business:flow:instance:read` | 标记已查看（P1，本批只留接口形状） |
| 31 | POST | `/business/flow/task/{id}/remind` | `business:flow:approve` | 催办（发起人催当前审批人，24h 内限 1 次） |
| 32 | GET | `/business/flow/task/{id}/attachments` | `business:flow:approve` | 意见附件列表（下载走专用代理端点） |

#### 10.1.4 待办与抄送（6）

| # | 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|---|
| 33 | GET | `/business/flow/task/todo` | `business:flow:instance:read` | **我的待办**（`APPROVAL` + `RESUBMIT`，按截止/创建排序） |
| 34 | GET | `/business/flow/task/todo/count` | `business:flow:instance:read` | 待办数（顶栏角标，实测 0.071 ms） |
| 35 | GET | `/business/flow/task/done` | `business:flow:instance:read` | 我已审（含被转办出去的） |
| 36 | GET | `/business/flow/cc/my` | `business:flow:instance:read` | 抄送我的（未读优先） |
| 37 | GET | `/business/flow/cc/unread-count` | `business:flow:instance:read` | 抄送未读数 |
| 38 | POST | `/business/flow/cc/{id}/read` | `business:flow:instance:read` | 单条置已读 |

#### 10.1.5 统计与管理（6）

| # | 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|---|
| 39 | GET | `/business/flow/stats/overview` | `business:flow:stats:read` | 总览（进行中/今日新增/今日完成/平均时长） |
| 40 | GET | `/business/flow/stats/duration` | `business:flow:stats:read` | 平均审批时长（按流程 / 按节点，带时间窗） |
| 41 | GET | `/business/flow/stats/bottleneck` | `business:flow:stats:read` | **卡在哪个节点**（`avg_ms` / `p95_ms` / `ended_cnt`） |
| 42 | GET | `/business/flow/stats/approver` | `business:flow:stats:read` | 审批人工作量（谁手上积压最多） |
| 43 | GET | `/business/flow/def/{id}/impact` | `business:flow:admin` | 停用/改流程的影响面（在跑实例数、按节点分布） |
| 44 | GET | `/business/flow/meta` | `business:flow:instance:read` | 元数据（可选业务类型、可选审批人来源、我是否有管理权限） |

### 10.2 权限码（10 个，已含在 `V0118`）

```
business:flow:instance:read        查看（列表/详情/待办/抄送）
business:flow:instance:create      发起
business:flow:instance:withdraw    撤回自己的
business:flow:approve              审批（同意/驳回/转办/加签/催办）
business:flow:def:read             查看流程定义
business:flow:def:write            编辑（草稿）
business:flow:def:publish          发布/停用
business:flow:admin                流程管理员（终止/退回任意节点/改派/迁移）
business:flow:stats:read           统计
business:flow:cc:manage            抄送规则管理（P1）
```

> **中间件闸的选择原则**（照抄 `task/handler.rs` 的血泪经验）：分派、转办这类「行级关系决定权限」的接口，`#[alun::permission]` 只挂**粗粒度**的码（`business:flow:approve`），真正的判定在 handler 里现查（「这条待办是不是我的」）。**不要**给「转办」单独造一个 `business:flow:transfer` 码——否则「拿到审批权但没拿到转办权」这种配置会让人卡死，而真实语义是「能审的人就能转」。

### 10.3 错误码 `65xxx`（22 个）

| 码 | HTTP | 常量 | 文案 |
|---|---|---|---|
| 65001 | 404 | `FLOW_NOT_FOUND` | 流程不存在 |
| 65002 | 404 | `FLOW_INSTANCE_NOT_FOUND` | 审批单不存在或无权查看 |
| 65003 | 404 | `FLOW_TASK_NOT_FOUND` | 待办不存在或已处理 |
| 65004 | 403 | `FLOW_FORBIDDEN` | 无权执行该操作 |
| 65005 | 400 | `FLOW_FIELD_INVALID` | 字段取值非法 |
| 65010 | 400 | `FLOW_GRAPH_STRUCTURE` | 流程结构非法（V1/V2/V4） |
| 65011 | 400 | `FLOW_GRAPH_CYCLE` | **流程存在环：n_a → n_b → n_a**（V3） |
| 65012 | 400 | `FLOW_GRAPH_DEGREE` | 节点出入度非法（V5/V6） |
| 65013 | 400 | `FLOW_GRAPH_DEFAULT_EDGE` | 条件节点缺少兜底分支（V7） |
| 65014 | 400 | `FLOW_GRAPH_JOIN_MODE` | 汇聚方式与分支来源不匹配（V8） |
| 65015 | 400 | `FLOW_NODE_ASSIGNEE_MISSING` | 节点未配置审批人 / 会签模式缺失（V9/V10） |
| 65016 | 400 | `FLOW_CONDITION_INVALID` | 条件表达式非法（V11） |
| 65017 | 400 | `FLOW_GRAPH_TOO_LARGE` | 流程过大（V12：节点 ≤ 200 / 边 ≤ 400） |
| 65018 | 422 | `FLOW_NO_ASSIGNEE` | 节点「X」没有可用审批人 |
| 65019 | 422 | `FLOW_SELF_APPROVAL_FORBIDDEN` | 节点「X」的审批人是发起人本人，流程不允许自审 |
| 65020 | 409 | `FLOW_TASK_ALREADY_DONE` | 该待办已被处理（幂等返回，前端刷新即可） |
| 65021 | 409 | `FLOW_DEF_IN_USE` | 流程有进行中的审批单，不能删除 |
| 65022 | 409 | `FLOW_VERSION_NOT_EDITABLE` | 该版本已发布，不能直接编辑（请新建版本） |
| 65023 | 409 | `FLOW_INSTANCE_NOT_WITHDRAWABLE` | 已有人审批，不能撤回（如需终止请联系管理员） |
| 65024 | 409 | `FLOW_INSTANCE_NOT_PENDING` | 审批单已结束 |
| 65030 | 409 | `TASK_APPROVAL_PENDING` | 该任务正在审批中（FLW-…），请在审批单里处理（**任务侧使用**） |
| 65031 | 422 | `FLOW_TIMEOUT_CONFIG_INVALID` | 超时自动通过需要流程级开关（`allow_auto_approve`） |

> **`65020` 语义特殊**：它是「幂等成功」，HTTP 用 409 但前端应按「已处理，刷新」处理，不是错误弹窗。这条写进 §10.5。

### 10.4 三个端点契约样例

#### 10.4.1 `GET /business/flow/instance/{id}`（详情）

```json
{
  "code": 0,
  "data": {
    "id": "…",
    "instance_no": "FLW-202610-000123",
    "title": "任务验收：完成批次 D 架构文档",
    "status": "PENDING",
    "flow": { "code": "task_accept", "name": "任务验收", "version_no": 2 },
    "initiator": { "id": "…", "name": "武鹏", "dept_name": "研发中心" },
    "biz": { "type": "TASK_ACCEPT", "id": "…", "no": "TSK-202610-000042",
             "title": "完成批次 D 架构文档", "status": "REVIEW" },
    "form_data": { "task_status": "REVIEW", "priority": "P1",
                   "estimate_hours": 8, "task_owner_name": "武鹏" },
    "started_at": "2026-10-29T10:00:00+08:00",
    "round_total": 1,
    "auto_passed_nodes": 0,
    "my_task_id": "…",
    "my_actions": ["APPROVE", "REJECT", "TRANSFER", "ADD_SIGN"],
    "nodes": [
      { "node_key": "n_start",   "name": "开始",     "type": "START",
        "round": 1, "status": "APPROVED", "duration_ms": 0 },
      { "node_key": "n_leader",  "name": "主管审批", "type": "APPROVAL",
        "round": 1, "status": "APPROVED", "duration_ms": 3600000,
        "tasks": [ { "assignee_name": "李经理", "source": "INITIATOR_SUPERIOR",
                     "status": "APPROVED", "comment": "同意",
                     "completed_at": "2026-10-29T11:00:00+08:00" } ] },
      { "node_key": "n_finance", "name": "财务会签", "type": "COUNTERSIGN",
        "round": 1, "status": "ACTIVE", "approval_mode": "RATIO",
        "approve_ratio": 0.6, "reject_veto": true,
        "approved_count": 1, "approver_count": 3, "activated_at": "…" }
    ],
    "edges": [ { "from": "n_start", "to": "n_leader", "taken": true }, … ],
    "cc": [ { "name": "王五", "is_read": false } ],
    "actions": [
      { "at": "2026-10-29T10:00:00+08:00", "who": "武鹏", "what": "发起审批" },
      { "at": "2026-10-29T11:00:00+08:00", "who": "李经理", "what": "同意（主管审批）" },
      { "at": "2026-10-29T11:05:00+08:00", "who": "赵会计", "what": "同意（财务会签 1/3）" }
    ]
  }
}
```

#### 10.4.2 `POST /business/flow/task/{id}/reject`（驳回）

```json
// 请求
{ "return_to": "PREV", "comment": "金额凭证缺失，请补充后重新提交", "attachments": ["<file_id>"] }
// return_to: PREV | INITIATOR | <node_key> | NONE(=驳回终止，仅管理员/流程允许)

// 成功 200
{ "code": 0, "data": {
    "instance_status": "PENDING",
    "returned_to": "n_leader", "returned_round": 2,
    "task_status": "REJECTED",
    "biz_callback": { "task_status": "DOING", "reject_reason": "金额凭证缺失…" }
} }

// 失败（该待办已被别人处理）
{ "code": 65020, "msg": "该待办已被处理", "data": { "current_status": "APPROVED" } }
```

#### 10.4.3 `PUT /business/flow/def/{id}/graph`（保存整图）

```json
// 请求（整图覆盖式保存；服务端只做结构校验，不做发布校验）
{
  "version_id": "…",              // 必须是 DRAFT 版本
  "nodes": [
    { "key": "n_start",  "name": "开始",     "type": "START",
      "x": 80,  "y": 200 },
    { "key": "n_leader", "name": "主管审批", "type": "APPROVAL",
      "assignee_type": "INITIATOR_SUPERIOR", "superior_levels": 1,
      "self_approval": "SKIP", "x": 260, "y": 200 },
    { "key": "n_finance","name": "财务会签", "type": "COUNTERSIGN",
      "assignee_type": "ROLE", "assignees": [ { "type": "ROLE", "ref_id": "…" } ],
      "approval_mode": "RATIO", "approve_ratio": 0.6, "reject_veto": true,
      "empty_assignee": "ERROR", "x": 440, "y": 200 },
    { "key": "n_end",    "name": "结束",     "type": "END", "x": 620, "y": 200 }
  ],
  "edges": [
    { "key": "e1", "from": "n_start", "to": "n_leader" },
    { "key": "e2", "from": "n_leader", "to": "n_finance" },
    { "key": "e3", "from": "n_finance", "to": "n_end" }
  ]
}

// 成功 200
{ "code": 0, "data": { "version_no": 2, "node_count": 4, "edge_count": 3,
                       "warnings": [] } }
// 注意：warnings 是"不阻断保存但发布会被拦"的提示（如"条件节点缺兜底分支"）
```

**为什么保存不做发布校验**：编辑过程中图必然是不完整的（刚拖了一个节点还没连线）。保存时只做**引用完整性 + 结构合法性**（key 唯一、from/to 存在、不重复边），发布时才跑 V1–V12。

### 10.5 四个接口层约定

| 约定 | 内容 |
|---|---|
| **幂等** | `approve` / `reject` / `transfer` / `withdraw` 全部幂等：待办已是终态 → `65020` + `current_status`；实例已是终态 → `65024`。前端一律按「已处理，刷新」展示 |
| **并发** | 同一待办的并发提交由行锁 + `uk_biz_flow_task_pending` 兜住；应用层把 PG `23505` 映射成 `65020`（**不是 500**） |
| **分页** | 统一 `page` / `page_size`（`utils::normalize_page`），上限 100；列表走 `scope::paginate`（闸①/闸② 盯着它） |
| **时间窗** | 统计类接口必须带 `from`/`to`（默认近 30 天，上限 180 天，超限 `400 + 65005`） |

---
## 11. 前端：SVG 自绘流程编辑器

### 11.1 技术底座（零新依赖）

实测 `frontend/package.json` 的运行依赖只有：

```
axios ~1.15  dayjs ~1.11  echarts ~5.4  pinia ~3.0
tdesign-icons-vue-next ~0.4  tdesign-vue-next ~1.19
vue ~3.5  vue-i18n ~11.1  vue-router ~4.6
```

**没有** d3 / vue-flow / @vue-flow/core / bpmn-js / antv-x6。本批**不新增任何依赖**。

| 候选方案 | 否决理由 |
|---|---|
| `bpmn-js` | 运行时 +400 KB（含 diagram-js + moddle），且它渲染的是 BPMN 2.0 语义（任务/网关/事件/边界事件 20+ 种元素），我们只有 7 种节点。**引入它是为了 20% 的功能付 100% 的体积**，而且它的「元素模型」与我们的 `node/edge` 表结构对不上，中间要写一层转换 |
| `@vue-flow/core` | 轻量（~40 KB）但把「节点渲染」交给 Vue 组件、把「连线」交给它自己的模型；我们要的「条件挂在边上 + 端口语义 + 后退 round 高亮」都要绕过它的抽象层写自定义边/自定义节点，最终代码量比自己画少不了多少，还多一个版本升级风险 |
| `antv-x6` | 功能最全，但 +500 KB，且它的 API 风格（graph.addNode/addEdge）与 Vue 响应式要手动同步 |
| `d3` | 只解决「数据驱动的 DOM」，拖拽/连线/吸附还得自己写；为了一个 `drag` 引入 250 KB 不值 |
| **纯 SVG + pointer events** | ✅ **本设计选它**：7 种节点 = 7 个 `<g>` 模板；连线 = `<path>` 三次贝塞尔或折线；拖拽 = `pointerdown/move/up`；缩放 = `viewBox`。核心约 700 行，全部可控 |

### 11.2 坐标系与变换（最容易写错的地方）

三层坐标，混了就出 bug：

| 层 | 单位 | 谁用 |
|---|---|---|
| **画布坐标**（`node.x/y`） | 逻辑像素，落库就是这个 | 节点位置、连线的起终点计算 |
| **屏幕坐标** | 浏览器 CSS 像素 | `pointer` 事件的 `clientX/clientY` |
| **SVG 用户坐标** | `viewBox` 内的单位 | `<path d="...">` 里的数字 |

```ts
// 屏幕 → 画布（关键：必须用 getScreenCTM 的逆矩阵，不要手算）
function screenToCanvas(evt: PointerEvent, svg: SVGSVGElement): { x: number; y: number } {
  const pt = svg.createSVGPoint();
  pt.x = evt.clientX;
  pt.y = evt.clientY;
  const m = svg.getScreenCTM()!.inverse();
  const p = pt.matrixTransform(m);
  return { x: p.x, y: p.y };
}
```

**为什么不用 `(clientX - rect.left) / scale`**：一旦 `viewBox` 的 `minX/minY` 不为 0（平移后就会），或者 SVG 有 `preserveAspectRatio` 造成的非均匀缩放，手算的公式就错。`getScreenCTM().inverse()` 由浏览器给出**完整**变换，永远不会错。

**缩放/平移**：不改任何节点的 `x/y`，只改 `viewBox`：

```
缩放：滚轮 delta → scale *= 1.1^(-deltaY/100)，clamp [0.4, 2.5]
      以鼠标位置为锚点（否则会"缩放时内容乱跑"）：
        const before = screenToCanvas(evt, svg);
        setViewBox(scale);
        const after = screenToCanvas(evt, svg);
        pan(before.x - after.x, before.y - after.y);
平移：空格 + 拖拽 / 中键拖拽 / 双指（移动端）
```

### 11.3 节点拖拽（pointer events，含移动端）

```ts
// 一个节点 <g class="node" :transform="`translate(${n.x},${n.y})`">
function onPointerDown(evt: PointerEvent, node: NodeVM) {
  if (evt.button !== 0) return;
  const start = screenToCanvas(evt, svg);
  const orig = { x: node.x, y: node.y };
  (evt.target as Element).setPointerCapture(evt.pointerId);   // ← 关键：捕获后 move 不会丢
  let raf = 0, latest = start;
  const onMove = (e: PointerEvent) => {
    latest = screenToCanvas(e, svg);
    if (raf) return;
    raf = requestAnimationFrame(() => {          // ← 节流到帧，不是每个 move 都算
      raf = 0;
      node.x = snapGrid(orig.x + (latest.x - start.x));
      node.y = snapGrid(orig.y + (latest.y - start.y));
      dirty = true;                              // 只标记，不提交
    });
  };
  const onUp = () => {
    window.removeEventListener('pointermove', onMove);
    window.removeEventListener('pointerup', onUp);
    pushUndo();                                  // 一次拖拽 = 一条撤销记录（不是每帧一条）
    autosaveDebounced();
  };
  window.addEventListener('pointermove', onMove);
  window.addEventListener('pointerup', onUp);
}
```

**四个必须做对的点**：
1. **`setPointerCapture`**：不捕获的话，鼠标快速移动出节点边界后 `pointermove` 就断了（经典「拖到一半卡住」）。
2. **`requestAnimationFrame` 节流**：`pointermove` 在 120 Hz 触控板上每秒 120 次，每次都改 `reactive` 对象 + 重算连线 = 掉帧。节流到 rAF 后稳定 60 fps。
3. **`snapGrid`**：10 px 网格吸附，让手工摆的图看起来是「画出来」的而不是「手抖的」。
4. **拖拽结束才 pushUndo**：每帧 push 会让「撤销」变成「撤销 3 毫秒前的 1 像素」。

### 11.4 连线绘制与吸附

**端口语义**（每个节点固定两个端口）：

```
      ┌─────────────┐
      │  主管审批   │
   ○──┤             ├──○      ← 左：入口（in），右：出口（out）
      └─────────────┘
```

**连线手势**：从出口端口 `pointerdown` → 拖出一条虚线跟随鼠标 → 悬停到目标节点的**入口区域**（半径 24 px 命中）→ 高亮该节点 + 端口变绿 → `pointerup` 建边。

**路径算法**（正交折线，不用贝塞尔）：

```
若 to.x > from.x + 40:            // 正常向右
   mid = (from.x + to.x) / 2
   d = M from → L mid,from.y → L mid,to.y → L to
否则:                              // 回流（下一节点在左边，如退回边）
   d = 绕行：向下 40 px 走一圈
```

**为什么用正交折线而不是贝塞尔**：审批流程图是「从左到右的管道」，正交线让「第 3 步 → 第 5 步」的跨越关系一目了然；贝塞尔在节点密集时会互相叠。而且正交线可以加**箭头中点标签**（`条件：金额 ≥ 1 万`），标签位置就是折线中段，计算简单。

**边的条件标签**：`CONDITION` 出边必须在线上显示条件摘要（`condition_label` 由后端在保存时生成中文摘要，前端只显示）。默认边显示「其他情况」。

### 11.5 撤销/重做

**命令栈（不是快照栈）**：

```ts
type Cmd =
  | { t: 'move',   key: string, from: {x,y}, to: {x,y} }
  | { t: 'addNode', node: NodeVM }
  | { t: 'delNode', node: NodeVM, edges: EdgeVM[] }     // 删节点要连边一起记
  | { t: 'addEdge', edge: EdgeVM }
  | { t: 'delEdge', edge: EdgeVM }
  | { t: 'prop',   key: string, field: string, from: any, to: any };
```

| 方案 | 否决理由 |
|---|---|
| 每次变更存一份完整图的深拷贝 | 200 节点的图 ≈ 40 KB，100 步撤销 = 4 MB 内存；且 `JSON.stringify` 深拷贝 40 KB 在 rAF 里会掉帧 |
| 命令栈 | ✅ 每条命令几十字节；`undo` = 反向应用；`redo` = 正向应用 |
| 只做「重新加载服务端」当撤销 | 会丢掉未保存的编辑 |

`Ctrl+Z` / `Ctrl+Shift+Z`（Mac 上 `Cmd`），上限 100 步。

### 11.6 实时校验：前端只做提示，后端才是权威

| 校验 | 前端（即时，黄色提示，不阻断保存） | 后端（发布时，红色阻断） |
|---|---|---|
| 孤立节点（无入无出） | ✅ 节点描边变黄 + 右下角角标 | V2/V4 |
| 有环 | ✅ 保存时调 `POST /validate` | V3（权威，给环路径） |
| 条件节点缺兜底 | ✅ 角标 | V7 |
| 审批人未配置 | ✅ 节点内「未配置审批人」占位文案 | V9 |
| 汇聚模式不匹配 | ❌ 太复杂，前端不算 | V8 |
| 规模上限 | ✅ 拖第 201 个节点时直接拦 | V12 |

**为什么前端不自己判环**：前端判环和后端判环是两份实现，第一次分歧就会「前端说没问题、保存报错」或反之。**正确做法**：前端只在自己改图后 debounce 300 ms 调 `POST /validate`（只校验不保存），拿后端的结果渲染。一次请求 ~5 ms，完全可接受。

### 11.7 页面清单（5 个）

| # | 路由 | 页面 | 权限码 | 要点 |
|---|---|---|---|---|
| 1 | `/flow` | 审批中心 | `business:flow:instance:read` | 三 tab：待我审批 / 我发起的 / 抄送我的。待办卡片显示 `title / 流程名 / 当前节点 / 等待时长 / 发起人` |
| 2 | `/flow/instance/:id` | 审批详情 | 同上 | 上方流程图（只读，带进度高亮），下方时间线（按 round 分组），右侧审批操作区（同意/驳回/转办/加签） |
| 3 | `/flow/def` | 流程定义列表 | `business:flow:def:read` | 卡片式：名称/业务类型/当前版本/状态/在跑实例数 |
| 4 | `/flow/def/:id/edit` | **流程编辑器** | `business:flow:def:write` | 三栏：左（节点面板）/ 中（画布）/ 右（选中节点的属性面板） |
| 5 | `/flow/stats` | 效率统计 | `business:flow:stats:read` | 4 个卡片 + 2 个 echarts（卡点柱状图用现有 echarts，不引新库） |

**详情页的「驳回」交互**（最容易做错）：

```
点「驳回」→ 抽屉打开 →
  ┌ 退回方式（必选，默认"上一节点"）
  │   ○ 退回上一节点（主管审批）
  │   ○ 退回发起人
  │   ○ 退回指定节点  ← 仅管理员可见
  ├ 驳回意见（必填，≥5 字，与 task 的 REJECT_REASON_MIN_CHARS 一致）
  ├ 附件（可选，≤ 5 个）
  └ [取消] [确认驳回]
```

**为什么要「必选退回方式」而不是默认不选**：默认值（上一节点）在单节点流程里无处可退（上一节点就是 START），此时应自动切成「退回发起人」。前端要根据节点位置动态算默认值。

### 11.8 移动端（断点 768 px）

| 页面 | 移动端处理 |
|---|---|
| 审批中心 | 卡片列表（已是最简单的布局），tab 变成顶部分段控件 |
| 审批详情 | 流程图**横向滚动**（不缩放到看不清）；时间线纵向；操作按钮固定在底部（`position: sticky`） |
| **流程编辑器** | **明确不支持**：显示「流程编辑请在电脑上操作」。理由：200 节点的拖拽 + 属性面板在 375 px 宽的屏幕上无法完成；硬做出来的体验会让用户误操作改坏流程 |
| 统计 | 卡片单列，图表改用 `echarts` 的 mobile 配置 |

### 11.9 配色兼容

前端有 22 套配色（`pnpm check:palettes` 是门禁）。流程图的语义色**只能用 CSS 变量**，不得硬编码：

| 语义 | 变量 | 用途 |
|---|---|---|
| 已通过 | `--td-success-color` | 节点边框 + 图标 |
| 审批中 | `--td-brand-color` | 节点边框 + 呼吸动画 |
| 已驳回 | `--td-error-color` | 节点边框 |
| 跳过 / 未到达 | `--td-gray-color-5` | 虚线边框 |
| 画布网格 | `--td-component-stroke` | 20 px 细网格 |

---

## 12. 实施拆分（D1–D6）

### 12.1 六张卡

| 卡 | 范围 | 依赖 | 验收要点 | 复杂度 |
|---|---|---|---|---|
| **D1**<br>定义侧数据与校验 | `V0115`（5 张表）+ `flow/model.rs`（def/version/node/edge 的 CRUD）+ `flow/graph.rs`（环检测 / 拓扑 / 12 条发布校验）+ 定义 API 1–11 + `V0118` 权限种子 | 无（新模块，可与 D4 并行） | ① `V0115` 在空库与生产同构库各跑一次通过；② 24 条负面约束里与定义相关的 16 条全部拒绝；③ **§8 的环检测单测**：纯链 200 节点返回 `None`、加一条回边返回闭合路径且首尾相同、`UNION` 式误判写法不出现在代码里；④ 12 条发布校验每条都有正例和反例单测；⑤ 保存整图后 `SELECT count(*)` 与 `node_count/edge_count` 一致 | **大** |
| **D2**<br>运行侧数据与引擎 | `V0116`（4 张表）+ `V0117` 索引 + `flow/engine.rs`（激活 / 判定 / 流转 / 退回 / 撤回）+ `flow/assignee.rs`（8 种解析 + 三层去重 + 空/自审/机器人策略）+ 实例 API 12–23 | D1（要有 PUBLISHED 版本才能发起） | ① 发起 → 单签通过 → 实例 `APPROVED` 全链路；② 会签 `ALL`/`RATIO`/`ANY` 各一组用例（含「RATIO 提前判死」）；③ **并发用例**：两会话同时审批，`approved_count` 最终 = 2（复现 §4.5 实验 3），且无 500；④ 退回三种目标各一组，`round` 递增且第 1 轮历史保留；⑤ 撤回（零人已审）成功、已审后撤回返回 `65023`；⑥ 单调自检：`approved_count`/`rejected_count` 与 `COUNT(*)` 现算值永相等 | **大** |
| **D3**<br>任务集成 + 审计 + 通知 | `task/state.rs` 的 `flow_gate`（**报批项**）+ `flow/task_bridge.rs`（发起 / 回调 / 取消传导）+ `changelog/diff.rs` 3 条 `ResourceSpec` + 4 个通知模板 + 任务详情 DTO 的 `approval` 块 | D2 | ① 任务 `DOING→REVIEW` 自动发起审批单并写回 `approval_instance_id`；② 审批通过 → `REVIEW→DONE`（`progress=100`、`done_at` 有值）；③ 驳回 → `REVIEW→DOING` + `reject_reason` = 审批意见；④ **绕过测试**：实例 `PENDING` 时创建人手动 `PATCH /status` → `409 + 65030`；⑤ 任务取消 → 审批单 `CANCELLED`；⑥ `sys_change_log` 里能查到发起/同意/驳回三条（`action` 分别是 `CREATE`/`STATUS`/`STATUS`）；⑦ ROBOT 能审（带令牌）、不能网页登录 | **中** |
| **D4**<br>待办 / 抄送 / 统计 | 待办 API 33–38 + 统计 API 39–44 + `flow/timeout.rs`（超时扫描 + 催办）+ `flow/maintain.rs`（`graph_hash` 自检）+ `V0119` 任务种子 | D2 | ① 我的待办 / 我发起 / 抄送我 三个列表在 10 万实例下 P95 < 20 ms（复跑 §7.2 的 Q1/Q4/Q5）；② 超时扫描 `LIMIT 500` 分批且幂等（同一待办不重复提醒）；③ 卡点统计与手算一致（造 3 个已知耗时的节点）；④ `graph_hash` 自检能在「直接 UPDATE 快照」后报警 | **中** |
| **D5**<br>前端：审批中心 + 详情 + 编辑器 | 5 个页面（§11.7）+ `api/business/flow-api.ts` + SVG 编辑器（拖拽 / 连线 / 缩放 / 撤销 / 校验提示） | D1（编辑器只依赖定义 API）、D4（列表） | ① 编辑器能画出「开始→主管→财务会签→结束」并保存；② 拖拽在 120 Hz 触控板下不掉帧（DevTools Performance 无长任务）；③ 撤销/重做 100 步正确；④ 22 套配色门禁 `pnpm check` 通过；⑤ 移动端详情页可用、编辑器页给出「请用电脑」提示 | **大** |
| **D6**<br>联调 / 灰度 / 文档 | 端到端联调脚本、`docs/批次D-审批流-使用手册.md`（给管理员讲「怎么画一个流程」）、默认「任务验收」流程种子、灰度开关 | D3、D5 | ① 从「新建流程」到「任务验收走审批」全流程人工走通一遍（含退回一次）；② 种子流程 `task_accept` 能在全新库上一键可用；③ 手册里有 3 个可照抄的流程范例（单签 / 会签+条件 / 并行） | **小** |

### 12.2 依赖顺序与并行度

```
D1 ──┬──► D2 ──┬──► D3 ──┐
     │         ├──► D4 ──┼──► D6
     └──► D5（编辑器只依赖 D1，可与 D2/D3/D4 并行）
```

| 阶段 | 可并行 | 关键路径 |
|---|---|---|
| 第 1 波 | D1 + D5（编辑器骨架） | D1 |
| 第 2 波 | D2 + D5（编辑器接线） | D2 |
| 第 3 波 | D3 + D4 | D3（有报批项，最可能被卡） |
| 第 4 波 | D6 | — |

**D3 的报批要提前**：它要动 `task/state.rs`，而 `task` 是已经交付验收的模块。**建议 D1 一开始就提交报批材料**（改动 3 处：1 个枚举 + 1 个字段 + 1 个判定分支），不要等 D2 做完再提。

---

## 13. 风险与最容易踩的 5 个坑

### 13.1 坑 1：会签并发 —— 丢票与重复激活（都有实测）

**表现**：3 人会签，2 人同时点同意，`approved_count` 显示 1；或者「节点明明该等 3 个人，第 1 个人点完就通过了」。

**实测（§4.5）**：

```
无锁 + 绝对值写    → 两人都读到 0，都写 1 → 最终 approved_count = 1     ← 丢一票，不报错
无锁 + ANY 双触发  → 两人都判定"节点通过" → 第二个 INSERT 撞唯一索引
                      ERROR: duplicate key ... uk_biz_flow_instance_node  ← 用户看到 500
有行锁 FOR UPDATE  → B 阻塞、读到 1、写 2 → 最终 = 2                    ← 正确
```

**为什么容易踩**：三个坑都是「本地看着对」的写法。单机单测（顺序执行）永远通过；上生产并发才炸；而且**丢票不报错**——数据看起来正常，直到有人发现「这单只有 2 个人同意就通过了」。

**必须做的四件事**：
1. 计票 `COUNT(*)` 现算，**任何 `+=` 都不许出现在票数上**；
2. `SELECT … FROM biz_flow_instance_node WHERE id=? FOR UPDATE` 包住「计票 + 判定 + 流转」；
3. 应用层把 `23505` 当**幂等成功**，不返回 500；
4. `after_node_ended` 里的**所有副作用**（发通知、写审计、调外部）必须在锁内收集、提交后发送——否则锁内做网络 IO 会把行锁持有时间从 1 ms 拉到几百 ms。

**验收方法**：D2 的并发用例必须是**真双连接**（不是同一个连接顺序跑两次）。本文的 `s1a.sql` / `s1b.sql` / `s3a.sql` / `s3b.sql` 可直接复用为 CI 里的集成测试。

### 13.2 坑 2：流程版本漂移

**表现**：流程改了（删了一个审批节点 / 换了审批人），在跑的 30 张单子突然变了路径；或者更隐蔽——**没有人发现**，直到某张单跳过了一个本该审的环节。

**两条漂移路径**（都要堵）：

| 路径 | 怎么发生 | 怎么堵 |
|---|---|---|
| **换版本** | 实例读 `def.current_version_id` | 实例绑 `instance.version_id`；引擎只读 `version.graph_snapshot`（D2） |
| **改行** | 直接改了 `biz_flow_node` / `biz_flow_edge` 的行，而实例正在 JOIN 这些行求值 | 引擎**不 JOIN** 行，只读 JSONB 快照；PUBLISHED 版本的行在应用层只读；`graph_hash` 每日自检发现「直接改库」 |

**实测证据（§3.6.2）**：

```
 FLW-DRIFT-0001 | v1 | is_current=f | 一级审批       ← 发布 v2 后，老单仍是 v1 的图
 FLW-DRIFT-0002 | v2 | is_current=t | 二级审批（新）  ← 新单才用 v2
```

**最隐蔽的子坑**：`version_id` 有了，但代码里某个地方为了「显示流程名」顺手 `JOIN biz_flow_def` 取 `flow_name`——这没问题（名称是展示）；但如果顺手取了 `def.current_version_id` 去判断「这个节点还要不要审批」，漂移就回来了。**所以 `instance` 上冗余了 `flow_code` / `flow_name` / `version_no`**，就是为了让「展示」不需要 JOIN 定义表。

### 13.3 坑 3：审批人与发起人是同一人（自审）

**表现的三种形态**：

| 形态 | 后果 |
|---|---|
| 发起人就是部门负责人（`INITIATOR_SUPERIOR` 命中自己） | 单人部门/小团队的单子永远批不了，或自己批自己 |
| `TASK_CREATOR` 与发起人同一人（自己建的任务自己提交验收，创建人=负责人） | 「验收」变成「自己验自己」，验收环节形同虚设 |
| `ROLE` 展开后包含发起人（发起人也在财务角色里） | 同上，而且**不显眼**——用户不会想到「我竟然审了自己」 |

**实测事实**：`biz_task` 的默认创建流程里「负责人 = 创建人自己」（`task/handler.rs:create` 的默认值），所以「自建任务自己提交验收」是**最常见**的路径，不是边缘情况。

**方案（§4.3.5）**：`self_approval(ALLOW|SKIP|FORBID)`，默认 `SKIP`，且 **`SKIP` 要落一条 `SKIPPED` 的待办行**（不是静默丢弃）——审批时间线上要能看到「本来该他审，因为自审被跳过」。

**最危险的组合**：`self_approval=SKIP` + `empty_assignee=SKIP` + 单节点流程 → 整张单没有一个人审，直接 `APPROVED`。所以 `instance.auto_passed_nodes` 必须计数并在详情页显著提示「本单有 1 个节点因无有效审批人自动通过」。**这一条是审计能查出来的关键。**

### 13.4 坑 4：退回后的状态回滚

**表现**：

| 现象 | 根因 |
|---|---|
| 退回重走后，汇聚节点直接通过，跳过了该重新审的节点 | `join_mode=AND` 的判定写成「所有入边来源 `status='APPROVED'`」**没带 `round` 条件**，读到了上一轮的通过记录 |
| 详情页显示「张三在财务会签同意了 2 次」 | 复用同一行 `instance_node` 覆盖了上一轮状态 |
| 「这个节点被驳回过几次」统计不出来 | 没有 `round` 维度 |
| 退回后并行分支上的待办永远挂着 | 退回时只改了当前节点，没把其他 `ACTIVE` 节点置 `CANCELLED` |

**方案（§4.6）**：`round + 1` 新行 + 6 件状态回滚 + **所有判定带 `round`**。

**验证要点（写进 D2 验收）**：
```
造一条 3 节点流程 A→B→C，在 C 驳回退回到 A：
  断言 ① A 有两条 instance_node（round=1 的 REJECTED、round=2 的 ACTIVE）
  断言 ② B 仍然是 round=1 的 APPROVED（历史事实保留）
  断言 ③ 走到汇聚点时，判定只认 round=2 的 B —— 所以 B 必须重新出现 round=2 的行
  断言 ④ 第 1 轮的 B 行不被改写（updated_at 不变）
```
第 ③ 条是核心：**退回到 A 之后，B 也要以 round=2 重新走一遍**，而不是「因为 B 上一轮通过了所以跳过」。这就是「状态回滚」的真正含义——不是把 B 改成未通过，而是**新一轮重新生成 B**。

### 13.5 坑 5：角色里一个人都没有（空审批人）

**表现**：流程发布时校验通过（`ROLE` 有 1 条引用），上线后该角色的人全调走/离职 → 节点激活时解析出 0 人 → 节点卡死或整个单子直接通过。

**为什么发布校验拦不住**：V9 只能校验「配置里有引用」，校验不了「这个角色现在有几个人」——人员流动是运行时事实。

**方案（§4.3.4）**：`empty_assignee(SKIP|ERROR|FALLBACK_ADMIN)`。

| 场景建议 | 策略 |
|---|---|
| `CC` 抄送节点 | `SKIP`（抄送不到不是事故） |
| 普通审批节点 | `SKIP` + 强提醒（节点 `SKIPPED`，`auto_passed_nodes+1`，详情页黄条提示） |
| 财务 / 法务 / 合规审批 | **`ERROR`**（宁可让单子卡住并报错，也不能漏审） |
| 关键节点兜底 | `FALLBACK_ADMIN`（落给超管，至少有人能看到） |

**配套的运维工具**（D4 交付）：`GET /business/flow/def/{id}/impact` 要返回「每个 `ROLE` 来源节点当前能解析出几个人」——上线前跑一次，0 人的节点直接标红。这是**比校验更实用**的防线。

### 13.6 其余风险（简表）

| # | 风险 | 等级 | 应对 |
|---|---|---|---|
| R1 | `graph_snapshot` 与行数据不一致（有人直接改库） | 高 | `graph_hash` 每日自检 + 告警 |
| R2 | 前端提交的图与后端 schema 不一致（字段名拼错） | 中 | 保存接口 `deny_unknown_fields`，拼错直接 400（不静默忽略） |
| R3 | 统计接口无时间窗导致全表扫 | 中 | 强制时间窗（默认 30 天）+ 缓存 300 s |
| R4 | `my_instance_ids` 超过 5000 条 | 中 | 截断 + `truncated` 标志 + 前端提示（同 `member_scope_truncated`） |
| R5 | 通知风暴：一次会签 50 人 × 每个节点 | 中 | `dispatch` 单次收件人上限 5 万（底座已有）；同一事件合并成一次调用 |
| R6 | 超时扫描把 5 万条一次捞出来 | 中 | `LIMIT 500` 分批 + 幂等 CAS |
| R7 | `sys_change_log` 分区边界（下个月没预建分区） | 高 | 底座已有按月预建任务（`V0084` + C6 维护任务），**本批不改**，但要在发布检查单里确认分区已建到 +2 月 |
| R8 | 驳回意见超长截断到 500 字符（`reject_reason` 限制） | 低 | 写入前 `chars().take(500)`，并在响应里回传被截断 |
| R9 | 撤回后任务停在 `REVIEW`（死单） | 中 | §14 未决项 O1，倾向「撤回自动把任务拉回 `DOING`」 |
| R10 | 管理员误停用流程导致新单发不出 | 低 | 停用返回「当前在跑 N 张」+ 二次确认 |
| R11 | **菜单 UUID / 权限码撞号** | 高 | 本文初稿按 `V0103` 注释取 `…0770`–`…0779`，实测发现已被 `V0110`（T7a 甘特）与 `V0113`（T6b 评论工时）占用 → 改取 `…0780`–`…0789`。**规矩：每次动迁移都重新全库枚举 `grep -rhoE '20000000-0000-0000-0000-[0-9]{12}' migrations/ \| sort -u`，不信任何注释** |

---

## 14. 未决项（需老大 / 产品拍板）

| # | 问题 | 我的建议 | 影响面的卡 |
|---|---|---|---|
| **O1** | 发起人撤回验收申请后，任务是否自动回 `DOING`？ | **是**。否则任务停在 `REVIEW` 且 `approval_instance_id` 指向一张已撤回的单，成为谁都推不动的死单 | D3 |
| **O2** | 任务验收是否**强制**走审批（还是保留「直接通过」）？ | **默认不强制**：`biz_task.need_review=true` 时才自动发起审批；`false` 时保持现有「负责人直接完成」。给管理员一个「本部门验收必须走审批」的配置项（P1） | D3 |
| **O3** | 自审策略默认 `SKIP`（自动跳过）还是 `ALLOW`（允许自审）？ | **`SKIP`**，且自动跳过要在详情页黄条提示 | D2 |
| **O4** | 会签 `RATIO` 的分母是「应到人数」还是「实到人数」？ | **应到人数**（`approver_count`）。理由：分母按实到算的话，「大家都不来审」反而更容易通过 | D2 |
| **O5** | 超时 `AUTO_APPROVE` 默认关闭，是否需要**全流程级**的开关（`flow_def.allow_auto_approve`）还是**系统级**？ | **流程级**（本设计）：不同流程的风险等级不同，系统级一刀切太粗 | D2 |
| **O6** | 「退回指定节点」是否只限管理员？ | **是**（`business:flow:admin`）。普通审批人退回第 1 步 = 作废前面所有人的工作 | D2 |
| **O7** | 流程编辑器是否支持**导入/导出 JSON**？ | **支持导出**（给运维备份/跨环境搬运），**不做导入**（导入 = 未经校验的图直接进库，风险大于便利）。P1 再说导入 | D5 |
| **O8** | 需要几个内置流程种子？ | **1 个**：`task_accept`（任务验收：主管 → 完成）。其余交给用户在界面上画——种子太多会让「流程列表」一开始就是一堆没人用的模板 | D6 |

---

## 附录 A：迁移清单与实跑结果

| 迁移 | 内容 | 实跑 |
|---|---|---|
| `V0115__create_flow_def_tables` | `biz_flow_def` / `biz_flow_def_version` / `biz_flow_node` / `biz_flow_node_assignee` / `biz_flow_edge` + 内联索引 + 全部 COMMENT | ✅ `d_flow_v1`、`d_flow_v3` 各跑一次通过 |
| `V0116__create_flow_runtime_tables` | `biz_flow_instance` / `biz_flow_instance_node` / `biz_flow_task` / `biz_flow_cc` + 内联索引 | ✅ 同上 |
| `V0117__flow_indexes` | 3 条 POC 补出来的索引（`task_node` / `instance_node_active` / `cc_unread`） | ✅ 补后实测 418× / 337× |
| `V0118__seed_flow_permissions` | 10 个菜单节点 | ✅ 幂等（`ON CONFLICT DO NOTHING`） |
| `V0119__flow_job_and_optional` | 2 个 `sys_job` 元数据 + （报批后）`idx_sys_dept_leader` | ✅ |
| `S__flow_permissions.sql`（种子） | 把 10 个权限码授给 `SUPER_ADMIN` | 待 D1 实施时随代码提交 |

**实跑核对**：

```
$ psql -d d_flow_v1 -f ddl.sql      → 无 ERROR（9 张表 + 20 条索引 + 全部 COMMENT）
$ psql -d d_flow_v1 -f neg_test.sql → 24 条负面测试全部 ERROR（见 §6.7）
$ psql -d d_flow_v3 -f seed.sql     → 100,000 实例 / 300,000 节点 / 300,000 待办 / 100,000 抄送
$ psql -d d_flow_v3 -f perf2.sql    → Q1 0.473ms / Q2 0.071ms / Q9-A 227.3ms / Q9-B 0.887ms
$ psql -d d_flow_v3 -f cycle2.sql   → 纯链 12.6ms / +3跳 19.4ms / +10跳 278ms / +60跳 >300s 未完成
$ psql -d d_flow_v3 -f mech.sql     → 版本漂移 v1/v2 分流正确；round=2 历史保留
```

**文档自身可执行性核对**（关键：不是"POC 脚本能跑"，而是"**文档里印的 DDL 能跑**"）：

```
# 从本文 §6 抽出全部 sql 代码块，在全新库上执行
$ awk 'NR>=<§6起> && NR<<§6.8' cloudpivot-approval-architecture.md \
    | awk '/^```sql$/{f=1;next} /^```$/{f=0;next} f' > doc_ddl.sql
$ psql -d d_flow_v6 -f doc_stubs.sql   # 最小底座替身（sys_user/sys_dept/sys_menu/sys_job/biz_task）
$ psql -d d_flow_v6 -f doc_ddl.sql     # → 0 ERROR
$ psql -d d_flow_v6 -tAc "select count(*) from pg_class where relname like 'biz_flow%' and relkind='r'"
 9
$ psql -d d_flow_v6 -tAc "select count(*) from sys_menu where auth_code like 'business:flow%'"
 10
$ psql -d d_flow_v6 -f neg_test.sql    # 24 条负面约束 → 24 个 ERROR（与 §6.7 一致）
```

> 这一步抓到两个真问题：① 初稿 DDL 里有一条**与 UNIQUE 约束重复的索引**（`uk_biz_flow_def_version_current`）；② 菜单 UUID 段 `…0770`–`…0779` **撞号**（已被 `V0110`/`V0113` 占用，见 §6.4 / R11）。两者都不会报错，都会在上线后变成"索引白占写入"和"权限节点覆盖别人的菜单"。

## 附录 B：POC 脚本与复跑命令

脚本全部在 `/root/.hermes/workspace/architect/d_flow/`：

| 文件 | 作用 | 复跑 |
|---|---|---|
| `ddl.sql` | 9 张表完整 DDL（含 `sys_user`/`sys_dept`/`biz_task` 最小替身） | `psql -d <新库> -f ddl.sql` |
| `neg_test.sql` | 24 条负面约束 | `psql -d <库> -f neg_test.sql` |
| `seed.sql` | 10 万实例压测数据 | `psql -d <库> -f seed.sql`（约 15 s） |
| `perf.sql` / `perf2.sql` / `perf3.sql` / `perf4.sql` | 六条热路径 + 可见性四种写法 + 索引缺口对拍 | `psql -d <库> -f perf2.sql` |
| `cycle.sql` / `cycle2.sql` | 环检测：递归 CTE 的爆炸曲线 | `psql -d <库> -f cycle2.sql`（`+60跳` 那条会跑很久，用 `statement_timeout` 兜） |
| `mech.sql` | 版本漂移 / round / 审批人冻结 | `psql -d <库> -f mech.sql` |
| `con_setup.sql` + `s1a/s1b/s3a/s3b/s2a/s2b.sql` | 会签并发四场景（需要两个终端交替跑） | 见下 |

**并发 POC 的跑法**（必须两个真连接）：

```bash
export PGPASSWORD=…
psql -d <库> -f con_setup.sql
# 场景 S1（无锁丢票）
(psql -d <库> -f s1a.sql > s1a.out 2>&1 &) ; sleep 0.4 ; psql -d <库> -f s1b.sql > s1b.out 2>&1
sleep 3 ; psql -d <库> -c "SELECT approved_count FROM biz_flow_instance_node WHERE id='…'"
# 期望 1（丢一票）——这就是"不修"的样子

psql -d <库> -f con_reset.sql
# 场景 S3（FOR UPDATE）
(psql -d <库> -f s3a.sql > s3a.out 2>&1 &) ; sleep 0.4 ; psql -d <库> -f s3b.sql > s3b.out 2>&1
sleep 3 ; psql -d <库> -c "SELECT approved_count FROM biz_flow_instance_node WHERE id='…'"
# 期望 2（正确）
```

## 附录 C：事实索引（结论 → 证据）

| 结论 | 证据 |
|---|---|
| `biz_task.approval_instance_id` 已存在、恒 NULL | `V0100__create_task_tables.up.sql:98`、`:131`；`task/mod.rs:59`；`task/model.rs:406` |
| 任务状态机只有 10 条白名单流转 | `task/state.rs` 的 `TRANSITIONS`（含 `REVIEW→DONE` 角色位掩码） |
| 驳回原因 ≥ 5 字 | `task/state.rs` 的 `REJECT_REASON_MIN_CHARS = 5` |
| 变更日志 action 只有 11 个值 | `V0084__create_change_log.up.sql:55-56` |
| 变更日志是分区表、主键含 `created_at` | `V0084` 文件头第 1/2 点 + `:53` |
| 写日志必须事务内、必须登记 REGISTRY | `changelog/mod.rs:119`（`record`）、`:296`（`record_in`）、`diff.rs:170`（`BIZ_TASK`） |
| 通知唯一入口与四类目标 | `notice/mod.rs:290`（`dispatch`）、`notice/fanout.rs:36-45`（`NoticeTarget`） |
| 通知失败不回滚业务 | `task/collab.rs:1345` |
| 调度器注册签名 | `src/task/mod.rs:306`（`register_cron`）、`:233`（`register_builtin`） |
| 可见性用「数据权限 OR id 集合」 | `task/model.rs:99`（`VISIBLE_OR_TAIL`）、`:162`（`my_member_task_ids`）、`:566`（`page_visible`） |
| `scope.rs` 的 S4：不含递归 CTE | `shared/scope.rs:925`（注释）、`:941`（断言） |
| `sys_dept` 有 `leader_id`，`sys_user` 无上级字段 | `V0006__create_sys_dept.up.sql:8`；`V0001__create_sys_user.up.sql`（无该列） |
| `sys_dept.leader_id` 无索引 | `V0006` 只建 `code/parent_id/path` 三条 |
| AI 硬拒只有 4 件事，其余只查码 | `shared/ai_actor.rs` 模块文档的对照表 |
| 错误码 `65xxx` 未被占用 | `shared/error_code.rs` 全文 `grep 65[0-9]{3}` = 0 命中 |
| 前端无图库 | `frontend/package.json` 的 `dependencies` |
| T4 的环检测是内存 BFS 而非递归 CTE | `task/dep.rs:131`（`find_cycle`）、`:49`（`MAX_CYCLE_DEPTH`）、`:211`（`orient_cycle_from`） |
| 递归 CTE 在流程图上爆炸 | 本批 POC：`cycle.sql` 跑 300 s 未完成；`cycle2.sql` 的 12.6/19.4/278 ms 曲线 |
| 无锁丢票 | 本批 POC：`s1a.out` + `s1b.out` → 最终 `approved_count = 1` |
| 行锁修复 | 本批 POC：`s3a.out` + `s3b.out` → 最终 `approved_count = 2` |
| 会签计票索引缺口 | 本批 POC：41.4 ms → 0.099 ms（`idx_biz_flow_task_node`） |
| 审批中看板索引缺口 | 本批 POC：39.1 ms → 0.116 ms（`idx_biz_flow_instance_node_active`） |
| 可见性四种写法性能 | 本批 POC：227.3 / 776.2 / 36.6 / 0.887 ms |
| 版本漂移防护有效 | 本批 POC：`mech.sql` 输出 `is_current=f` 且节点名仍是 v1 |
| `round` 保留历史 | 本批 POC：`mech.sql` 输出 round=1 REJECTED / round=2 ACTIVE |

---

> **本文档的性质**：设计文档，不含业务代码。所有 DDL 与性能数字来自本机 PG 18.6 实跑（脚本可复跑）；所有代码事实来自生产 HEAD `db1e56e` 的本地镜像。**未做**的事：没有在真实生产库上跑过（`d_flow_*` 是独立验证库），没有跑 `cargo test`（本机无该仓库的构建链），前端编辑器是设计不是实现。
