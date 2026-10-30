# 云枢 · 批次 C+「工作任务管理」技术架构设计

> 个人任务计划 · 上级分派 · 向下分派/拆解 · 任务协作 · 任务板 · 任务进度
>
> | 项 | 值 |
> |---|---|
> | 项目 | 云枢通用后台（CloudPivot，Rust 1.94.1 / edition 2024 / nik 0.4.2 / PostgreSQL 18.6） |
> | 代码基线 | `172.18.10.144:/opt/cloudpivot`，HEAD **`97d1367`**（`feat(preference): 用户偏好体系落库 —— KV 主表 + 字典元数据 + 8 项预置（V0090/V0091）`） |
> | 基线事实（**本机实测**） | `information_schema` 实测 **43 张表**；迁移最新到 **V0091**；全库 **零触发器**（`pg_dump -s` 里 `CREATE TRIGGER` 计数 = 0） |
> | 基线事实（任务书口径） | 521 tests 全绿、门禁 B 闸①/② = 0/0、C7 G1–G6 全过。**本文未重跑 `cargo test`**（本机无该仓库的 Rust 构建链），这组数字按任务书采信，不作为本设计的推导依据 |
> | 迁移号段 | 已用 `V0001`–`V0052`、`V0060`–`V0068`、`V0080`–`V0091`；**本模块从 `V0100` 起**，占用 `V0100`–`V0105` |
> | 本文档性质 | **只做设计，不交付业务代码**。所有 DDL 在真库 **PG 18.6 实跑通过**；所有性能结论有 `EXPLAIN (ANALYZE, BUFFERS)` 实测；所有代码事实附 `文件:行号` |
> | 验证环境 | PG 18.6（Ubuntu 26.04 / x86_64），验证库 `c_task_v1`（迁移首跑与自检）、`c_task_v2`（20 万任务性能实验）；脚本见附录 B |
> | 本地代码镜像 | `/root/.hermes/workspace/architect/baseline2/{src,migrations,frontend-src}`（rsync 自生产 HEAD `97d1367`，仅供检索定位） |
> | 交付代码位置 | 新代码全部进 `backend/src/business/task/`；**不改** `system/` / `support/`；`business/shared/` 只在 §4.4 说明的那一处需要报批 |

---

## 目录

| 章节 | 内容 | 谁最该先读 |
|---|---|---|
| §0 | 决策摘要（16 条硬结论 + 4 处与任务书的偏差 + 交付边界） | 全体（5 分钟） |
| §1 | 现状核查：`dept.leader` 有没有 / `scope` 怎么用 / `dispatch` 签名 / `record` 签名 / 存储与导入导出 / 调度器（全部带行号） | 后端实施者 |
| §2 | **需求覆盖表**：老大要的 6 条 + 14 条必评项逐条「做 / 不做 / 留 P1」+ 我们额外补的 12 条 | 老大 + 产品 |
| §3 | 数据模型：12 张表完整 DDL（V0100–V0103）+ 权限与配置种子（V0104/V0105） | 后端实施者 |
| §4 | 核心机制：分派规则 / 状态机 / 进度模型 / 可见性并集 / 看板与并发 / 通知触发点 | 后端 + 评审（**本设计重点**） |
| §5 | 性能实测：20 万任务下 5 种可见性写法、看板、统计、扫描任务的真实数字与选型结论 | 评审 + 运维 |
| §6 | API 契约：47 个端点 + 22 个权限码 + 错误码段 63xxx | 后端 + 前端 |
| §7 | 前端设计：6 个视图、无依赖拖拽、详情页布局、22 套配色兼容、移动端 | 前端 |
| §8 | 实施拆分 T0–T9（每步含可执行验收 + 人日） | 项目负责人 |
| §9 | 风险 14 条 + **不做清单**（含 P1 触发条件） | 评审 + 老大 |
| §10 | 未决项（需老大/产品拍板） | 老大 |
| 附录 A | V0100–V0105 迁移清单与自检结果 | 后端实施者 |
| 附录 B | POC 脚本清单 + 实测输出摘要 + 复跑命令 | 复核者 |
| 附录 C | 事实索引（每条结论 → 文件:行号 / POC 编号） | 评审 |

---

## 0. 决策摘要（先看这一页）

### 0.1 十六条硬结论

| # | 决策点 | 结论 | 一句话理由（含证据） |
|---|--------|------|---------------------|
| **D1** | 「上级」怎么判定 | **`sys_user` 没有上级字段**（V0001 全表 0 处 `parent_id`/`manager_id`/`leader_id`，全仓 `grep manager` 只命中连接池与测试变量）。上级关系**只能**从 `sys_dept.leader_id`（`V0006__create_sys_dept.up.sql:8`）+ `sys_dept.parent_id`（`:6`）的部门树推出 | 不新增「用户直线上级」字段：那会与部门树形成两套组织关系，`sys_dept.leader_id` 已是唯一真源，加字段必然出现「部门换了主管、用户上级还是老的」 |
| **D2** | 「能分派给谁」的授权边界 | **复用批次 B 的数据范围（scope），不自造组织权限**：目标用户必须落在分派人的 `ScopedPrincipal.dept_ids` 内；跨部门分派只在 `data_scope ∈ {ALL, CUSTOM, DEPT_AND_CHILD}` 时可行 | `business/shared/principal.rs:289` 的 `RESOLVE_SQL` 已经把「我是谁、我能看哪些部门」算成一次查询（带 Redis 缓存 `principal.rs:63`）。再造一套「下属列表」等于第二套权限语义 |
| **D3** | 状态机的形状 | **两层：`biz_task_status_def`（自定义列）+ `semantic`（5 个固定语义）**。迁移合法性判定在 **semantic 层**（5×5 矩阵，§4.3.1 实测 25 种组合），列名与数量完全自由 | 老大要「需求池/开发中/测试中/已上线」，而统计口径（完成率/逾期率/燃尽）必须按固定语义聚合。只做自定义列 → 统计无锚点；只做固定 5 态 → 满足不了老大原话 |
| **D4** | 进度百分比冲突听谁的 | **有子任务就必须听汇总**（`progress_mode='A'`，手动改进度返回 **409**）；叶子任务才允许手动（`'M'`）。汇总按 `estimated_hours` 加权，缺省权重 1，**已取消的子任务从分母剔除** | 双真源必然漂移。加权而非等权：`estimated_hours` 是负责人自己填的承诺，等权会让「1 小时的子任务」和「40 小时的子任务」同权，进度条失真 |
| **D5** | 子任务全完成 → 父任务？ | **绝不允许自动 `DONE`**。默认（`task.auto_review_on_children_done=true`）自动转 **`REVIEW`（待验收）** 并通知父任务负责人；是否自动转由 `sys_config` 控制 | 分派场景下「子任务全完」不等于「父任务交付合格」。自动 `DONE` 会让验收环节形同虚设，也会污染完成率（`ck_biz_task_completed` 要求 `completed_at` 与 semantic 同真同假，AI 无权替人签字） |
| **D6** | 可见性并集（范围 ∪ 协作人）怎么落 SQL | **`scope::paginate` 不动，"并集"用「参与 id 数组」表达**：`AND (TRUE {FILTER} OR t.id = ANY(?::text::uuid[]))` —— 实测走 **BitmapOr(idx_biz_task_dept, biz_task_pkey)**，20 万任务下 37–40 ms | 实测对比（§5.1）：单条 `OR EXISTS(...)` 写法 **539 ms**（5 个 OR 把索引全废了，退化 Seq Scan）；UNION 分段 70 ms；id 数组 + BitmapOr **40 ms** 且**无需改 `scope.rs` 一行** |
| **D7** | 参与 id 数组的上限 | **硬上限 2000 个**，超过即**降级为「只按范围」+ 前端提示「我参与的」请走独立 tab**。实测 66,733 个 id 时查询 **>15 s（statement_timeout 打断）** | 见 §5.1 与 §5.2。数组参数不是免费的：BitmapOr 的每个 id 都要一次索引探测。2000 × 20 万规模下 40 ms 是安全区，6 万就崩 |
| **D8** | 看板一次返回还是分页 | **一次往返返回 N 列 × 每列 50 条**（`CROSS JOIN LATERAL`，每列各自 `ORDER BY priority, due_at NULLS LAST LIMIT 50`），列头另带 `total` 供「还有 N 条」 | 实测 5 列 × 50 条 = **14 ms**（§5.3）。分页会让拖拽后的顺序错乱（第 2 页回来插在中间），滚动加载在列内体验更好 |
| **D9** | 看板拖拽并发 | **乐观锁 `version`**：`UPDATE … WHERE id = ? AND version = ?`，0 行受影响 → **409 `TASK_VERSION_CONFLICT`** 并回传服务端最新快照。真两连接实测（§4.7.3 与 §5.5-⑨）：A 持锁 3 s，B 阻塞后拿到 0 行，`status_code` 保持 A 写的值 | 悲观锁（`SELECT … FOR UPDATE`）会在拖拽这种「一次点击一个」的场景把用户卡在数据库锁上；`version` 已在 DDL 里 |
| **D10** | 实时性（SSE） | **不做服务端推送（P1）**。看板用「409 冲突回传 + 30 s 增量轮询 `GET /task/board/delta?since=`」 | `notice` 的 SSE 通道事件契约是**写死的单形态**：`EVENT_NAME = "notice"`（`notice/sse_hub.rs:64`）+ 固定 `NoticeEvent` 结构（`sse_hub.rs:90-115`），前端按它解析。塞第二种 payload 就要改前端消费路径 + 事件分发，收益（省一次 30 s 轮询）远小于风险 |
| **D11** | 审批/验收边界 | **两级、无审批引擎**：`DOING → REVIEW → DONE`（验收通过）/ `REVIEW → DOING`（驳回）。验收人 = **分派人**；无分派人（自建任务）时 = 负责人所在部门的 `leader_id`；都不是 → 只能本人完成（`DOING → DONE` 直通，受配置控制） | 完整工作流审批是批次 D 的范围（任务书硬约束）。把验收做成两态既能闭合「分派—交回」闭环，又不侵占 D 批次 |
| **D12** | 变更留痕怎么接 | **复用 `changelog::record_in`（同事务）**，新增 `biz_task` 一条 `ResourceSpec` 登记（`changelog/diff.rs:157` 的 `REGISTRY`）。状态/负责人/截止时间/优先级/进度进白名单；`content_md` 进 `big` 摘要化 | `record_in(ex, ctx, target, input) -> DbResult<_>`（`changelog/mod.rs:284`）只吃 `&impl SqlExecutor`，天然可嵌业务事务；未登记资源会被 **62012 硬拒**（`mod.rs:191-200`），所以「登记」和「调用点」必须同一次提交 |
| **D13** | 任务正文形态 | **Markdown 存 `content_md`，渲染/净化复用 `business/shared/content`（`Recipe`）**，不存 HTML 快照 | `content::render(markdown, Recipe) -> Rendered`（`shared/content.rs:207`）、`sanitize_html`（`:284`）已是批次 C 的既有能力，告共和帮助文档在用。再写一套渲染器 = 一套新 XSS 面 |
| **D14** | 父子任务的递归与深度 | 深度上限 **5**（`CHECK (depth BETWEEN 0 AND 5)` + `task.max_depth` 配置提前报错）；循环用**祖先链查询**（`WITH RECURSIVE … WHERE lvl < 5`，实测 3 层树 0.2 ms）在应用层拦，DB 只兜 `parent_id <> id` | 全库零触发器（实测），不在本模块破例。祖先链查询有界（≤5 层），比「全表递归 + 触发器」便宜且可单测 |
| **D15** | 表数量与迁移 | **12 张新表 + 1 个序列**，`V0100`–`V0103` 建表、`V0104` 权限/菜单、`V0105` 配置/偏好。上线后总表数 **43 → 55** | 每张表都能对上任一需求项（§2 覆盖表逐条对应），没有「先建着以后用」的表 |
| **D16** | 底座改动 | **必改 3 处**：①`business/mod.rs` 加 `pub mod task;`；②`task/mod.rs::register_builtin`（`:233`）加 4 个定时任务；③`shared/error_code.rs` 加 **63xxx** 段（现最大 `62024`，`:417`）。**不改** `shared/scope.rs` | D6 的方案刻意绕开了「改 `scope.rs`」：`paginate` 的 `{FILTER}` 注入位天然支持「片段 OR 业务条件」的写法，`scope.rs` 一行不用动，闸①不用重新过审 |

### 0.2 与任务书假设的偏差（必须先对齐，共 4 处）

| 任务书假设 | 代码实况（证据） | 本设计的处理 |
|---|---|---|
| 「上级判定：`sys_dept.leader_id` 还是 `sys_user.parent_id`？（**先查证**）」 | `sys_user` **没有任何**上级字段（`V0001__create_sys_user.up.sql` 全文 19 列，无 `parent_id`/`manager_id`/`leader_id`；全仓 `grep -rn manager src/` 只命中连接池 API 与测试变量名）；`sys_dept.leader_id` 存在（`V0006:8`），`sys_dept.parent_id`（`:6`）+ `dept_path`（`:7`）构成树 | 采用 **`sys_dept` 单真源**（D1）。「上级」= 沿 `parent_id` 向上找到的第一个有 `leader_id` 的部门的主管；「下级」= 我作为 `leader_id` 的部门及其子树内的启用用户 |
| 「`biz_task_collaborator`（协作人）」是独立表 | 协作人与关注人在**可见性判定**上是同一件事（都要并进可见集合），生命周期完全一致（任务没了都该没） | 合并为 **`biz_task_member`**，用 `member_role ∈ {COLLABORATOR, WATCHER}` 区分权限（可编辑 / 只读）。理由见 §3.3 表头注释；`biz_task_collaborator` 的语义是其子集 |
| 「看板列自定义（若支持自定义）」写在 DDL 必交项里，但状态机又要求「待开始/进行中/待验收/已完成/已取消」 | 两者冲突：列可自定义就没有固定的 5 个状态可迁 | 用 **`status_def.semantic`（5 值固定）** 解耦（D3）：列随便加，语义 5 选 1，状态机与统计都打在 semantic 上 |
| 「看板是否需要 SSE 实时同步」 | `notice` 的 SSE 通道是**单形态事件**（`EVENT_NAME="notice"`、固定 `NoticeEvent`），不是通用事件总线 | D10：**不做**，用乐观锁 + 增量轮询。若将来要做，正确做法是给 `sse_hub` 加「事件名 + 任意 payload」的通用通道（那是批 A 底座的改动，要单独报批） |

### 0.3 交付边界（本批**不做**）

1. **不做**工作流审批（批次 D 范围）：任务验收最多两级（`REVIEW → DONE` / 驳回），不建 `flow_instance`、不做多级会签、不做条件分支。
2. **不做**周期任务（P1，§2-#4）：需要「实例化幂等键 + 时区/DST + 编辑本次/后续」，且与「一任务一状态机」冲突，留给 P1 并给出触发条件。
3. **不做**甘特图与项目里程碑（P1）：依赖只做 FS 软约束、无基线、无关键路径，做甘特等于做一个假甘特。
4. **不做**任务自定义字段（EAV，P2）：标签已覆盖 80% 横切检索需求，EAV 是完整子系统（字段定义/类型校验/索引策略/导入导出映射）。
5. **不做** agent/机器人作为任务执行者（P1，§2-#14）：项目没有 agent 身份主体（`sys_user` 是自然人账号 + 令牌体系 + 审计口径），硬塞会污染审计与绩效口径。
6. **不做**看板列内自由排序（P1）：列内固定 `priority → due_at` 排序，拖拽只改状态。自由排序要 lexorank + 并发重排（写入放大），当前阶段收益低。
7. **不做**服务端 SSE 推送（P1，D10）。
8. **不迁**存量待办（若运维有 Excel 待办清单）：本批只提供 `impexp` 导入通道，不做一次性迁移脚本。

---

## 1. 现状核查（全部实测，不是文档推测）

### 1.1 组织结构：`dept` 有 leader，`user` 没有上级

```sql
-- backend/migrations/system/V0006__create_sys_dept.up.sql:5-12（实测原文）
CREATE TABLE sys_dept (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dept_code VARCHAR(50) NOT NULL,
    dept_name VARCHAR(100) NOT NULL,
    parent_id UUID,        -- 上级部门 ID（根节点为空）        ← :6
    dept_path VARCHAR(500),-- 层级路径（如 /1/2/）              ← :7
    leader_id UUID,        -- 负责人用户 ID（sys_user.id）      ← :8
    ...
```

```sql
-- backend/migrations/system/V0001__create_sys_user.up.sql（全文 19 列，逐列核对）
id / username / password / real_name / email / mobile / dept_id / status /
login_fail_count / is_locked / lock_time / last_login_at / remark /
created_at / updated_at / create_by / update_by / deleted_at
-- 结论：没有 parent_id、没有 manager_id、没有 leader_id、没有 position/job_level
```

**推论（设计约束）**：
- 「上级」只能由 `sys_dept.leader_id` + 部门树推，**不能**假设用户有直线上级；
- 因此「上级分派我的任务」这条需求的判定式是：`assigner_id ≠ assignee_id` **且** `assigner` 是我的部门链上的某个 `leader`（或更上层部门 leader）。这也意味着：**跨部门平行分派在语义上不算「上级分派」**，界面标签要分开（`source=ASSIGNED` 但 `is_from_superior=false`）。

### 1.2 数据权限：五范围 + 唯一分页入口

| 事实 | 位置 |
|---|---|
| 五范围枚举 `All=1 / Dept=2 / DeptAndChild=3 / SelfOnly=4 / Custom=5` | `business/shared/scope.rs:70-81` |
| 表锚点 `ScopeTarget { table, alias, dept_col, owner_col, scoped }` | `scope.rs:163-174` |
| 登记表清单 `SCOPED_TABLES`（11 项，新表要接入需登记） | `scope.rs:274-286` |
| 片段生成 `filter_for(&principal, target) -> RowFilter` | `scope.rs:436` |
| 注入位标记 `FILTER_MARKER = "{FILTER}"` | `scope.rs:59` |
| **唯一分页入口** `paginate(target, base_sql, base_params, order_by, page, page_size, &principal)` | `scope.rs:618-629` |
| 非注入分页（仅全局元数据表） `paginate_unscoped(...)` | `scope.rs:651` |
| 参数合并顺序：**片段参数恒前置** | `scope.rs:829-834` |
| 注入位校验（有业务参数必须用 `{FILTER}`，否则报错） | `scope.rs:747-761` |
| count 用去 ORDER BY 前缀执行 | `scope.rs:797-802` |

`filter_for` 的实际产物（`scope.rs:416-424` 的表格，逐行核对）：

| 生效范围 | 片段 | 参数 |
|---|---|---|
| `All` / 超管 | `AND 1 = 1` | 无 |
| `Dept`（单部门） | `AND t.dept_id = ?` | `[uuid]` |
| `DeptAndChild` / `Custom` | `AND t.dept_id = ANY(?::text::uuid[])` | `[文本数组字面量]` |
| `SelfOnly` | `AND t.assignee_id = ?`（用 `owner_col`） | `[user_id]` |
| 部门范围 ∪ 仅本人 | `AND (t.dept_id = ANY(?) OR t.assignee_id = ?)` | `[数组, user_id]` |

**这两条对任务模块是硬约束**：
1. 主体表必须有 `dept_id` 列，**不能靠 join `sys_user` 做部门过滤**（`ScopeTarget` 只锚定主体表，`scope.rs:155-161` 明确「片段只锚定主体表」）。→ 所以 `biz_task.dept_id` 必须是**冗余快照**（负责人部门），负责人变更时同事务回写。
2. `{FILTER}` 的注入形态是「` AND …`」，因此它在 SQL 里**只适合出现在一个布尔表达式的高位**。这正好被我用来做「范围 ∪ 参与」的并集（§4.4）：`AND (TRUE {FILTER} OR t.id = ANY(?))` 注入后成为 `AND (TRUE AND t.dept_id = ANY(...) OR t.id = ANY(?))`——括号包住了一切，语义正确。

### 1.3 变更日志：`record` / `record_in` 的签名与事务契约

```rust
// business/changelog/mod.rs:117-122
pub async fn record(
    ex: &impl SqlExecutor,          // 业务事务的执行器（不是 db()）
    ctx: &ChangeCtx<'_>,            // 操作人 / 请求 ID / IP / 发生时刻
    target: ChangeTarget<'_>,       // (resource=表名, resource_id, Action, 中文标签)
    input: DiffInput<'_>,           // 前值 / 后值 JSON
) -> std::result::Result<RecordOutcome, ApiError>;   // ::Recorded{id,changed_fields} | ::Skipped

// business/changelog/mod.rs:284-289（事务闭包内的形态，ApiError 折成 DbError）
pub async fn record_in(
    ex: &impl SqlExecutor, ctx: &ChangeCtx<'_>,
    target: ChangeTarget<'_>, input: DiffInput<'_>,
) -> DbResult<RecordOutcome>;
```

| 事实 | 位置 |
|---|---|
| 资源登记 `ResourceSpec { table, module, fields, big }` | `changelog/diff.rs:54-63` |
| 登记清单 `REGISTRY`（目前 4 项：`sys_user` / `sys_role` / `sys_announcement` / `sys_help_doc`） | `diff.rs:157` |
| 未登记资源 → **62012 硬拒**（HTTP 500，接入缺陷） | `mod.rs:191-200` |
| 大字段阈值 2 KiB，摘要 = `{len, sha256, preview(200), truncated}` | `diff.rs:44-47` |
| 写入侧脱敏 `Policy::load(ex, table)`（同事务读 `sys_field_mask`） | `changelog/mask.rs:159` |
| 请求上下文 `RequestCtx::from_request(&claims, &headers, connect_info)` | `mod.rs:245-256` |
| 动作枚举含 `STATUS` / `ASSIGN`（正好对应任务的状态流转与分派） | `mod.rs:360-363`（单测里的 DDL 对齐清单） |

**推论**：任务模块的「谁改了状态/负责人/截止时间」不用自己写日志表——登记一个 `biz_task` 的 `ResourceSpec` + 在写路径上调 `record_in` 即可，且**与业务同事务**（回滚不留假日志）。

### 1.4 消息通知：`dispatch` 的签名与扇出目标

```rust
// business/notice/mod.rs:246
pub async fn dispatch(draft: NoticeDraft) -> std::result::Result<DispatchResult, DispatchError>;

// business/notice/mod.rs:78-114  NoticeDraft 的 18 个字段（节选）
pub struct NoticeDraft {
    pub notice_type: NoticeType,      // System / Todo / Alert / Workflow / Circular（dto.rs:30-41）
    pub level: NoticeLevel,           // Info / Warn / Urgent
    pub title: String, pub content: String, pub content_format: ContentFormat,
    pub link_url: Option<String>,
    pub biz_type: Option<String>, pub biz_id: Option<String>,   // ← 任务通知用 "task" + task_id
    pub template_code: Option<String>, pub template_vars: serde_json::Value,
    pub target: NoticeTarget,
    pub channel_mask: u16,            // 1 站内 / 2 邮件 / 4 Webhook / 8 短信
    pub idempotency_key: Option<String>,  // ← 幂等键（必须传，见下）
    pub expire_at: Option<_>, pub scheduled_at: Option<_>,
    pub sender_id: Option<Uuid>, pub sender_name: Option<String>,
}
```

| 事实 | 位置 |
|---|---|
| 扇出目标 `NoticeTarget::{User, Role, Dept(含子树), All}` | `notice/fanout.rs:36-45` |
| `Dept` 走递归 CTE 展开子树（不依赖 `dept_path`） | `fanout.rs:150-165` |
| 事务边界：落 notice + 扇出 + 回写计数 + 投递记录在**一个事务**内 | `mod.rs:242-245` |
| **`dispatch` 自己开事务，无法与业务事务合并**（所以要用幂等键 + 提交后重试） | 批次 C 架构 D2 已定，本次复核一致 |
| 幂等唯一键 `uk_sys_notice_idem` | `V0049__notice_indexes.up.sql:27-28` |
| SSE：`EVENT_NAME="notice"`、`publish/publish_json/subscribe`、空频道回收任务 | `notice/sse_hub.rs:64 / 177 / 198 / 222 / 334` |

**推论（通知一致性）**：任务的分派/状态变更必须在**业务事务提交之后**调 `dispatch`，并用稳定幂等键：
- 分派：`task:{task_id}:assign:{assignee_id}:{version}`
- 状态：`task:{task_id}:status:{new_status}:{version}`
- 逾期：`task:{task_id}:overdue:{due_date}`
- 临期：`task:{task_id}:due_soon:{due_date}`
这样「事务回滚了但通知发出去了」不会发生（先提交后发），而「提交了但通知没发」可以用重试任务补偿（幂等键保证不重发）。

### 1.5 对象存储 / 导入导出 / 调度器 / 错误码 / 枚举

| 能力 | 事实（位置） | 任务模块怎么用 |
|---|---|---|
| 上传 | `POST /business/storage/upload`，权限 `business:storage:upload`（`storage/handler.rs:719-721`） | 附件先走通用上传，拿到 `sys_file.id` 再绑定 |
| 预签名直传 | `POST /business/storage/presign`（`handler.rs:836`） | 大文件走直传，`category=task` 的策略在 V0105 里种 |
| 下载 | `GET /business/storage/download/{file_id}`（`handler.rs:1118`），**权限已授予所有角色**（批次 C 架构 D6 实测） | **不直接暴露**：任务附件走 `GET /task/{id}/attachment/{att_id}`，先校验任务可见性再流式代理（与公告同款做法） |
| 存储策略 | `sys_storage_policy(category, max_size_mb, allowed_extensions, direct_upload, …)`（prod dump 实测 15 列） | V0105 种一条 `category='task'`，50 MB，白名单不含可执行扩展名 |
| 导入导出 | `DefinitionRegistry::{register_import, register_export}`（`impexp/registry.rs:59/80`）＋ `ImportDefinition`（`impexp/definition.rs:762`）/ `ExportDefinition`（`:813`） | 任务导出走 `ExportDefinition`（列：编号/标题/状态/负责人/优先级/截止/进度/工时）；导入走 `ImportDefinition`（负责人按 username 解析） |
| 调度器 | `Scheduler::register_builtin`（`task/mod.rs:233`）、`register_cron(name, cron, desc, runner)`（`:304`）、`is_registered_target`（`:523`） | 4 个新任务注册在此（临期提醒 / 逾期告警 / 每日汇总 / 一致性巡检） |
| 错误码 | `shared/error_code.rs` 现有最大 **62024**（`:417`），分段见文件头 `:30-40` | 新增 **63001–63040** 段（63xxx 未被占用） |
| 枚举下发 | `EnumItem::int/text`（`shared/enums.rs:93/113`）、`status_items`（`:172`） | 任务枚举（优先级/状态语义/来源/member_role）由后端下发，前端不硬编码 |
| 菜单种子范式 | `V0087`（页面 + 按钮 + 全员授权 + 自检 `DO $$`）、`V0085`（管理码只授超管）、`V0089`（目录归拢） | `V0104` 完全照此范式：8 字…8 段 UUID + 三段授权 + DO 自检 |
| 偏好体系 | `sys_preference_def(pref_key, value_type, default_value, scope, validator)` + `sys_user_preference`（`V0090`），种子 8 项（`V0091`） | 任务板视图偏好 4 项（`task.view_mode/swimlane/default_scope/show_done`） |
| Markdown 渲染 | `business/shared/content::{render, sanitize_html, Recipe}`（`content.rs:207/284/125`） | 任务正文与评论渲染，禁 base64 图片（走 storage） |
| 路由声明 | `#[nik::permission(path, method, permission)]` + `#[nik::get/post(...)]` 宏（例：`announcement/handler.rs:93`） | 47 个端点全部照此声明，`App::scan()` 自动收集（`lib.rs` 文档注释） |

### 1.6 三个「基线事实」的实测复核

| 断言 | 实测方式 | 结果 |
|---|---|---|
| 43 张表 | `select count(*) from information_schema.tables where table_schema='public' and table_type='BASE TABLE'`（生产库，经 ssh + 环境文件取凭据） | **43** ✓（含 `sys_change_log` 的 3 个月分区） |
| 迁移到 V0091 | `ls migrations/**/V*.up.sql \| sort -V \| tail` | 最新 `V0091__seed_preference_def.up.sql` ✓，共 53 个 up 迁移 |
| 零触发器 | `pg_dump --schema-only` 后 `grep -c "CREATE TRIGGER"` | **0** ✓ → 本模块**不引入触发器**，一致性靠「唯一写入口 + 每日巡检任务」 |

> 关于 521 tests：本机没有该仓库的 Rust 构建链（且跑全量测试需要连生产同构库），本文**未复跑**。实施时以 `cargo test` 真机输出为准，不改任务书的验收口径。

---
## 2. 需求覆盖表（老大要的 6 条 + 14 条必评项 + 我们补的 12 条）

### 2.1 老大明确要的 6 条（全部覆盖，逐条给出落点）

| # | 需求 | 落点（表 / 机制 / 接口） | 做到什么程度 |
|---|---|---|---|
| 1 | **自己的任务计划**（含今日/本周视图） | `biz_task`（`assignee_id = me`）+ `GET /task/mine?tab=assigned&range=today\|week\|month` + 日历视图 | 今日 = `due_at` 落在今天或已逾期未完成；本周 = 本自然周（周一起）；另给「无截止日期」桶 |
| 2 | **上级分派任务**（按部门层级） | `source=ASSIGNED`、`assigner_id`、`is_from_superior`（按 `dept.leader_id` 链路判定，§4.2）+ `GET /task/mine?tab=from_superior` | 上级看「我分派的」能看执行进度；下级收到通知 + 待办；验收环节闭合 |
| 3 | **向下分派任务**（转派 / 拆子任务） | `POST /task/{id}/transfer`（转派：换负责人，原负责人自动转协作人）、`POST /task/{id}/decompose`（拆解：批量建子任务，自动带 `parent_id/task_path/depth`） | 转派留痕（changelog `ASSIGN`）；拆解后父任务进度自动汇总 |
| 4 | **任务协作**（多人参与、评论、@） | `biz_task_member`（COLLABORATOR/WATCHER）+ `biz_task_comment`（`mention_ids` + GIN 索引）+ `GET /task/mention/me` | @ 只能给可见者（否则 422，防标题泄露）；被 @ 收到站内信；评论支持 Markdown |
| 5 | **任务板**（看板，按状态列拖拽） | `biz_task_status_def`（列定义）+ `GET /task/board`（一次返回 N 列 × 50 条）+ `PATCH /task/{id}/status`（乐观锁） | 拖拽只改状态（列内固定排序，§2.3-#24 解释为什么不做自由排序）；5 列 14 ms（§5.3） |
| 6 | **任务进度**（状态 + 百分比 + 子任务汇总） | `status_code/status_semantic` + `progress/progress_mode` + `child_count/done_child_count` + 加权汇总 SQL（§4.6 实测） | 有子任务时进度只读（409 拦手动写）；子任务全完成自动转 REVIEW（不自动 DONE） |

### 2.2 任务书要求逐条评估的 14 条（**每条都给结论**）

> 结论三档：**做**（本批 P0 交付）/ **留 P1**（本批不写代码，设计留位 + 给触发条件）/ **不做**（明确不做，给理由）。

| # | 项 | 结论 | 理由（判断依据，不是罗列） |
|---|---|---|---|
| 1 | **任务层级**：父-子拆解 + 依赖关系 | **做（父子）** + **做（依赖，但只 FS 软约束）** | 父子：老大原话「向下分派」的自然延伸，且第 6 条进度汇总**必须有父子结构**才能算（否则「汇总」无从谈起）。依赖：只做 FS（前置完成→后置才能开始），且**软约束**（可 `force=true` 强推并留痕）。**不做** FS/SS/FF/SF 四型 + 不做自动排程：四型要甘特与基线才有意义，做半套会让「依赖」变成误报制造机 |
| 2 | **优先级**：四象限 vs P0–P3 | **做 P0–P3 单一标量**；**不做**四象限 | 四象限需要「重要/紧急」两个正交布尔，而看板/列表/我的待办都需要**一个可排序可索引的标量** → 双轨必然漂移（用户改了 P0 但紧急标记还是旧值）。`priority SMALLINT` 上直接建 `(status_code, priority, due_at)` 索引（§3.1），四象限用「P0/P1 = 重要且紧急」这类前端预设筛选近似即可，DB 不存第二套 |
| 3 | **时间维度**：截止/预计工时/实际工时/逾期 | **做**（`start_at`/`due_at`/`estimated_hours`/`actual_hours`） | 逾期**不落列**：`due_at < now() AND completed_at IS NULL` 是唯一口径，落列就要定时刷新（20 万行每小时 UPDATE = 写放大 + 死元组）。用部分索引 `idx_biz_task_due`（`WHERE completed_at IS NULL AND due_at IS NOT NULL`）撑扫描，实测临期扫描 **0.5 ms**、逾期统计 **13 ms**（§5.3） |
| 4 | **周期任务**（每周五交周报） | **留 P1** | 三个硬问题本批解决不了：①**实例化语义**——每次生成新任务还是延展同一任务？前者会让「本周周报」永远是新 id（历史不可比），后者会让任务的 `completed_at` 反复被覆盖；②**幂等**——补跑/重放不能重复生成（要 `instance_key` 唯一约束）；③**编辑歧义**——用户改了这条，后续周期是否跟随。给 P1 骨架：`biz_task_recur(id, template_id, rule jsonb, next_at, instance_key unique)` + 每日 04:10 生成（幂等键 `{recur_id}:{yyyy-MM-dd}`）。触发条件：出现 ≥3 个部门真实使用（运营提需求） |
| 5 | **任务模板**（新员工入职 8 子任务） | **做** | 老大举的例子就是它；实现成本低（1 张表 + JSONB 子任务定义 + 1 个生成接口），收益高（重复性协作标准化）。用 JSONB 而不是从表：模板项没有独立生命周期（不单条编辑、不按项查询/统计） |
| 6 | **里程碑 / 项目分组** | **做（轻量项目）**；**留 P1（里程碑）** | 项目做：跨部门协作时「这个任务属于哪件事」是刚需，且它给看板一个天然分组维度 + 项目成员可作为可见性来源（§4.4）。里程碑留 P1：里程碑带日期锚点、基线对比、关键路径，属甘特范畴，与「依赖只做软 FS」不匹配。**不做**项目级进度引擎：项目进度由任务汇总派生即可（`task_count/done_count` 冗余计数） |
| 7 | **评审 / 验收**（最多两态） | **做**（`DOING → REVIEW → DONE` / `REVIEW → DOING` 驳回） | 分派闭环必需（§0.1-D11）。验收人 = 分派人 > 部门 leader > 本人直通。**不做**多级会签/条件分支/超时自动通过（那是批次 D） |
| 8 | **工时记录**（影响绩效） | **做**（`biz_task_worklog`），**不做**工时审批/期间锁 | 做：实际工时是「预计 vs 实际」偏差分析的唯一来源，也是负载均衡（P1）的输入。不做审批：工时审批是考勤域（批次外），且会让「记工时」变成负担 → 数据质量更差。防滥用用「只能记自己的 + 只能在可见任务上记 + 单条 ≤24h」三条 |
| 9 | **标签与自定义字段** | **做标签**；**不做自定义字段** | 标签轻（2 张表 + GIN/btree 索引）且覆盖横切检索（「线上故障」「客户A」）。自定义字段是 EAV 子系统：字段定义表 + 类型校验 + 动态索引策略 + 导入导出映射 + 表单渲染器，工作量 ≥ 整个任务模块的 40%，而真实需求通常是「再要两个标签」。触发升级条件：出现「需要按自定义字段做数值聚合」（标签做不到） |
| 10 | **看板列自定义** | **做**（`status_def` + 5 个固定 `semantic`） | 老大原话举例「需求池/开发中/测试中/已上线」→ 必须能自定义列。用 `semantic` 锚定统计口径是关键设计（§0.1-D3）。**不做**项目级独立列集：跨部门看板与「我的任务」无法聚合（每个项目一套列 = 每个人一套状态）。列的自定义是**全局**的（超管维护） |
| 11 | **任务提醒**（临期/逾期/每日汇总） | **做**（三个定时任务，复用 `dispatch`） | 临期：每小时扫 `due_at ∈ [now, now+24h]` 且未完成 → 通知负责人（幂等键含 `due_date`，一天只提醒一次）。逾期：每日 09:00 扫未完成且已过期 → 通知负责人 + 分派人。每日汇总：每日 08:30 给每人推「今日待办 N 条 / 逾期 M 条」。**留 P1**：个人免打扰时段（复用偏好 `notify.*` 需要新增偏好键，且要处理时区） |
| 12 | **统计报表** | **做 4 类**；**留 P1**（燃尽图、负载均衡） | 做：①个人工作量（任务数/工时/完成率）②部门完成率与逾期率 ③完成趋势（按周/月）④工时分布。实测 20 万行部门统计 **103 ms**（§5.3），可接受（报表页，可加 60 s 缓存）。燃尽图留 P1：需要「每日剩余工作量快照」表（否则只能从 changelog 反推，而 changelog 是字段级 diff 且保留 36 个月，反推等于每次重算全历史）。负载均衡 P1：要先有「标准工时」概念，属绩效体系 |
| 13 | **移动端适配** | **做**（响应式，不做独立 App/小程序） | 项目前端是 TDesign Vue Next + Vite（`package.json`），没有移动端工程。做：看板横向滚动 + 列宽自适应 + 详情页单栏（属性栏折叠成底部抽屉）+ 触屏长按拖拽（HTML5 DnD 在 iOS Safari 上不可用 → 用 pointer 事件，§7.3）。**不做**独立 App：维护成本翻倍，且内网办公场景浏览器足够 |
| 14 | **与 AI 的集成**（agent 作为执行者） | **不做（P1 前置条件清单另列）** | 硬缺口有三：①**身份主体**——`sys_user` 是自然人账号（含密码哈希/锁定/登录日志），agent 不能共用一个自然人账号，否则审计表里「谁批的」全是人；②**审计口径**——`operation_logs`/`sys_change_log` 的 `operator_id` 必须指向真实主体，agent 需要一个不可登录的执行主体类型；③**绩效口径**——工时/完成率按人统计，agent 执行会污染。前置条件（P1 启动门槛）：`sys_principal` 抽象（人或程序）+ 程序主体令牌 + `task.executor_type` 字段 + 审计表兼容。本批 DDL **不预留** `executor_type`（预留空字段 = 两套实现的诱惑） |

### 2.3 我们额外补的 12 条（老大「没想到的」——按标准任务系统的完整度）

| # | 项 | 结论 | 为什么该有这个（不做的后果） |
|---|---|---|---|
| 15 | **任务编号 `task_no`**（`TK20261029-000123`） | **做** | 任务 URL/通知/邮件里必须有一个「人能念出来」的标识。只有 UUID 时，用户在群里说「那个 a0000000-…0423 的任务」——等于没有标识。用**全局序列**而不是「当天 count+1」（后者并发必重号，实测种子 20 万行连续插入无重号） |
| 16 | **关注/订阅（WATCHER）** | **做**（并入 `biz_task_member`） | 没有关注，就只能在「被分派」和「被 @" 两种情况下收到消息。而真实协作是「我想知道这条任务什么时候完成」——这是「我关注的」tab 和通知接收人的来源 |
| 17 | **回收站与归档** | **做**（`deleted_at` 软删 + `archived_at` 归档） | 误删任务在多人的看板上是灾难（连带子任务/评论/附件）。归档解决「完成的任务越来越多把看板淹了」——归档 ≠ 删除（仍可按需查询、仍参与统计） |
| 18 | **批量操作** | **做**（`POST /task/batch`：改负责人/状态/优先级/加标签/归档） | 上级分派场景天然是批量的（「这 5 条都给他」）。逐条调接口会出现「部分成功」，用户不知道哪条失败了 |
| 19 | **复制任务 / 存为模板** | **做**（`POST /task/{id}/copy`、`POST /task/template/from-task/{id}`） | 这是模板能力的自然入口：用户不会先去「任务设置」里建模板，而是「这条任务不错，存成模板」 |
| 20 | **详情页动态时间线** | **做**（复用 changelog 的按对象查询） | 任务详情的右侧「动态」是用户判断「这任务怎么变成现在这样」的唯一入口。复用 `sys_change_log` 的 `resource + resource_id` 反查，不新建表 |
| 21 | **保存筛选视图** | **做**（复用 `sys_user_preference`，存筛选 JSON） | 部门主管每天看的是同一组筛选（「本周我部门未完成的 P0/P1」）。每次重选筛选项是最常见的效率浪费 |
| 22 | **批量导入 / 导出** | **做**（复用 `business/impexp`） | 迁移存量待办、月度汇报导出，都是真实诉求。复用框架 = 不用自己写 xlsx |
| 23 | **@我的（提及中心）** | **做**（`GET /task/mention/me`，GIN 索引 `mention_ids`） | @ 了但找不到「谁 @ 过我」，@ 就退化成噪音 |
| 24 | **列内自由排序** | **留 P1**（明确不做） | 见 §0.3-6。拖拽改状态是老大要的；列内排序是「锦上添花但要 lexorank + 冲突重排」，收益 < 复杂度 |
| 25 | **逾期升级提醒**（逾期 N 天通知上级） | **留 P1** | 需要「升级策略」配置（几天、通知谁、是否逐级），属组织治理决策，先让老大用一段时间再定 |
| 26 | **任务关联其它业务对象**（关联公告/工单/合同） | **留 P1** | 需要 `biz_obj_type/biz_obj_id` 的反查索引与「跨模块详情跳转」约定；当前没有第二个业务模块需要关联（批次 D 的工作流单据才是主要关联方） |

### 2.4 覆盖度自检

| 维度 | 数量 | 说明 |
|---|---|---|
| 老大明确要的 | 6 / 6 | 全部 P0 交付（§2.1） |
| 任务书要求评估的 | 14 / 14 | **整项做 12 条**；整项留 P1 **1** 条（#4 周期任务）；整项不做 **1** 条（#14 agent 执行者）。另有 5 处**子项**降级/砍掉并写明理由：#2（做 P0–P3、不做四象限）、#6（做项目、里程碑留 P1）、#9（做标签、不做自定义字段）、#11（做三类提醒、免打扰留 P1）、#12（做 4 类报表、燃尽图/负载均衡留 P1） |
| 我们额外补的 | 12 条 | 做 9、留 P1 3（#24/#25/#26） |
| **合计** | **32 项** | 整项做 **27** 项（6 + 12 + 9）、整项留 P1 **4** 项（#4 / #24 / #25 / #26）、整项不做 **1** 项（#14）；另有 5 处子项降级（见上行） |

> 关于「做/不做/留 P1」的记账口径：**做** = 本批必须有可运行代码 + 单测；**留 P1** = 本批不写代码、但设计里留了落点与触发条件；**不做** = 本批与可预见批次都不做，理由写在表里。

---

## 3. 数据模型（12 张表 + 1 个序列，V0100–V0105）

### 3.0 表清单与关系

| 迁移 | 表 | 行数量级（3 年预估） | 职责 |
|---|---|---|---|
| `V0100` | `biz_task_status_def` | 5–20 | 看板列定义 + 固定语义 |
| `V0100` | `biz_task` | 20–50 万 | 任务主体（唯一真源） |
| `V0101` | `biz_task_member` | 20–100 万 | 协作人 + 关注人 |
| `V0101` | `biz_task_comment` | 50–200 万 | 评论 + @ |
| `V0102` | `biz_task_attachment` | 5–20 万 | 附件（引用 `sys_file`） |
| `V0102` | `biz_task_dependency` | 1–5 万 | 前置依赖（FS） |
| `V0102` | `biz_task_tag` | 50–200 | 标签字典 |
| `V0102` | `biz_task_tag_rel` | 10–50 万 | 任务-标签 |
| `V0103` | `biz_task_worklog` | 30–100 万 | 工时明细 |
| `V0103` | `biz_task_project` | 200–2000 | 项目（轻量分组） |
| `V0103` | `biz_task_project_member` | 2000–2 万 | 项目成员 |
| `V0103` | `biz_task_template` | 20–200 | 任务模板 |

```text
                        ┌──────────────────────┐
                        │ biz_task_status_def  │ code (PK) / semantic
                        └──────────┬───────────┘
                                   │ status_code (FK RESTRICT)
        ┌──────────────────────────┴────────────────────────────┐
        │                     biz_task                          │
        │  id / task_no(u) / title / content_md                 │
        │  priority / status_code / status_semantic / progress  │
        │  assignee_id / assigner_id / creator_id / dept_id     │
        │  parent_id ─┐(自引用)  task_path / depth               │
        │  project_id │        start_at / due_at                │
        │  estimated_hours / actual_hours / completed_at        │
        │  version(乐观锁) / archived_at / deleted_at            │
        └───┬────┬────┬────┬────┬────┬────────────┬─────────────┘
            │    │    │    │    │    │            │
   member ──┘    │    │    │    │    │            └── worklog
   comment ──────┘    │    │    │    └── dependency (self → task)
   attachment ────────┘    │    └── tag_rel ── biz_task_tag
   parent/child ───────────┘
   project_id ──> biz_task_project ──< biz_task_project_member
   template ──(生成)──> biz_task（不建 FK：模板删除不影响已生成任务）
```

**引用规则（与既有库一致，逐条给依据）**：

| 规则 | 内容 | 依据 |
|---|---|---|
| R1 | **子表 → 父表用真 FK + `ON DELETE CASCADE`**（member/comment/attachment/dependency/tag_rel/worklog/project_member） | 既有 21 个 FK 全是这个形态（如 `sys_announcement_attachment_announcement_id_fkey … ON DELETE CASCADE`，prod dump 实测） |
| R2 | **对人的引用一律弱引用（不建 FK）**：`assignee_id/assigner_id/creator_id/added_by/author_id/user_id/uploaded_by` | `sys_user` 是软删的（`deleted_at`），FK 只会挡住正常运维；既有 `sys_user.dept_id` 就是弱引用（`V0001:8` 注释「弱引用不建外键」） |
| R3 | **`parent_id` 用 `ON DELETE RESTRICT`**：有子任务的任务不能物理删（软删不受限） | 防止「删父留下孤儿子树」；软删是常态路径，RESTRICT 只在误用物理删时触发 |
| R4 | **状态列 `ON DELETE RESTRICT`**：在用列不能删 | 否则任务的 `status_code` 悬空 → 看板列消失但任务还在（必须先在设置页把任务挪走） |
| R5 | **全表软删 `deleted_at`**（`biz_task`/`comment`/`worklog`/`tag`/`project`/`template`）；`member`/`attachment`/`dependency`/`tag_rel`/`project_member` 不设软删，靠 CASCADE | 关系表的「删除」就是取消关系，留软删只会让每条查询多一个条件 |

### 3.1 `V0100`：看板列定义 + 任务主体

> 以下 DDL 与 `migrations/V0100__create_task_core.up.sql` **逐字一致**，已在 PG 18.6 实跑通过（附录 A）。

```sql
-- ======================================================
-- V0100：工作任务管理 · 核心（状态列定义 + 任务主体）
-- 依据：docs/批次C+-工作任务管理-技术架构.md §3.1 / §3.2
-- 回滚：V0100__create_task_core.down.sql
--
-- 三条与既有库一致的口径：
--   1) 子表用真 FK + ON DELETE CASCADE；**对人的引用一律弱引用**（不建 FK），
--      与 sys_user.dept_id 的既有取向一致 —— 用户是软删的，FK 只会挡住正常运维。
--   2) 全库**零触发器**（实测 prod_schema：CREATE TRIGGER = 0）。本模块不破坏这条风格：
--      status_semantic 由应用层在唯一写入口维护，配每日一致性巡检任务兜底。
--   3) 软删 deleted_at + 部分唯一索引（WHERE deleted_at IS NULL）。
-- ======================================================

-- ---------- 1) 看板列 / 状态定义 ----------
-- 为什么独立成表而不是 CHECK 枚举：老大要「需求池 / 开发中 / 测试中 / 已上线」这类自定义列。
-- 但统计与进度汇总又必须能按「固定语义」聚合，所以每列带一个 semantic（5 个固定取值），
-- 状态机的合法性在 semantic 层判定，列名与数量完全自由。
CREATE TABLE biz_task_status_def (
    code       VARCHAR(32)  PRIMARY KEY,              -- 列编码（大写字母数字下划线），业务主键
    name       VARCHAR(50)  NOT NULL,                 -- 列显示名（需求池 / 开发中 / 测试中 / 已上线）
    semantic   VARCHAR(16)  NOT NULL,                 -- 固定语义，5 取 1，状态机与统计按它聚合
    color      VARCHAR(16)  NOT NULL DEFAULT 'default', -- 标签配色（前端 t-tag theme 联合类型内）
    sort_order INT          NOT NULL DEFAULT 0,       -- 看板列顺序（升序）
    is_initial BOOLEAN      NOT NULL DEFAULT FALSE,   -- 是否新建任务的默认列（全表仅一个为真）
    is_final   BOOLEAN      NOT NULL DEFAULT FALSE,   -- 是否终态列（DONE / CANCELED 必须为真）
    wip_limit  INT,                                   -- 列 WIP 上限（NULL = 不限；仅提示，不硬拦）
    status     SMALLINT     NOT NULL DEFAULT 1,       -- 1 启用 / 0 停用（停用列不可再进入）
    remark     VARCHAR(255),
    created_at TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by  UUID,
    update_by  UUID,
    CONSTRAINT ck_biz_task_status_semantic
        CHECK (semantic IN ('OPEN', 'DOING', 'REVIEW', 'DONE', 'CANCELED')),
    -- 终态标记与语义必须自洽：写反了统计会静默错（「已完成」不进完成率分母）
    CONSTRAINT ck_biz_task_status_final
        CHECK (is_final = (semantic IN ('DONE', 'CANCELED'))),
    CONSTRAINT ck_biz_task_status_order CHECK (sort_order >= 0),
    CONSTRAINT ck_biz_task_status_wip CHECK (wip_limit IS NULL OR wip_limit > 0)
);

COMMENT ON TABLE biz_task_status_def IS '任务看板列 / 状态定义：自定义列名 + 固定语义（5 取 1），状态机与统计按 semantic 聚合';
COMMENT ON COLUMN biz_task_status_def.code IS '列编码（业务主键），任务表 biz_task.status_code 引用它';
COMMENT ON COLUMN biz_task_status_def.semantic IS '固定语义：OPEN 未开始 / DOING 进行中 / REVIEW 待验收 / DONE 已完成 / CANCELED 已取消';
COMMENT ON COLUMN biz_task_status_def.is_initial IS '是否新建任务默认列；部分唯一索引保证全表只有一个启用列为真';
COMMENT ON COLUMN biz_task_status_def.wip_limit IS '列 WIP 上限：仅前端提示，不做硬拦（硬拦会让拖拽在并发下频繁失败）';

-- 全表只允许一个「默认列」（否则新建任务落哪列是不确定的）
CREATE UNIQUE INDEX uk_biz_task_status_initial
    ON biz_task_status_def (is_initial) WHERE is_initial AND status = 1;
CREATE UNIQUE INDEX uk_biz_task_status_name
    ON biz_task_status_def (name) WHERE status = 1;
CREATE INDEX idx_biz_task_status_order ON biz_task_status_def (sort_order);

-- ---------- 2) 任务主体 ----------
-- 任务编号：全局序列 + 日期前缀，人类可读且天然唯一（不用「当天 count+1」，那在并发下会重号）
CREATE SEQUENCE seq_biz_task_no START WITH 1 INCREMENT BY 1;

CREATE TABLE biz_task (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_no            VARCHAR(32)   NOT NULL,        -- 任务编号 TK20261029-000001（序列生成，全局唯一）
    title              VARCHAR(200)  NOT NULL,        -- 标题（纯文本，禁 HTML）
    content_md         TEXT          NOT NULL DEFAULT '', -- 正文 Markdown（渲染/净化复用 shared::content）
    priority           SMALLINT      NOT NULL DEFAULT 2, -- 0 P0 最高 … 3 P3 最低（单一标量，可排序可索引）
    status_code        VARCHAR(32)   NOT NULL,        -- 看板列编码（FK → biz_task_status_def.code）
    status_semantic    VARCHAR(16)   NOT NULL,        -- 冗余：状态语义快照（应用层唯一写入口维护，见 §4.1）
    progress           SMALLINT      NOT NULL DEFAULT 0, -- 0..100；有子任务时由汇总覆盖（progress_mode=A）
    progress_mode      CHAR(1)       NOT NULL DEFAULT 'M', -- M 手动填写 / A 由子任务汇总（只读）
    assignee_id        UUID,                          -- 负责人（弱引用 sys_user.id）
    assigner_id        UUID,                          -- 分派人（自建时 = creator_id）
    creator_id         UUID          NOT NULL,        -- 创建人（弱引用）
    dept_id            UUID,                          -- 归属部门 = 负责人所在部门快照（数据权限锚点，见 §5.3）
    project_id         UUID,                          -- 所属项目（可空；弱引用 biz_task_project.id）
    parent_id          UUID,                          -- 父任务（拆解树）
    task_path          VARCHAR(500),                  -- 层级路径 /父id/本id/（便于子树 LIKE 查询与深度校验）
    depth              SMALLINT      NOT NULL DEFAULT 0, -- 层级深度（根 = 0，上限 5）
    child_count        INT           NOT NULL DEFAULT 0, -- 子任务数（冗余，看板与进度汇总用）
    done_child_count   INT           NOT NULL DEFAULT 0, -- 已完成子任务数（冗余）
    source             VARCHAR(16)   NOT NULL DEFAULT 'SELF', -- 来源：自建/分派/转派/拆解/模板/导入/周期
    origin_task_id     UUID,                          -- 来源任务（转派/拆解时的原任务，可空）
    start_at           TIMESTAMPTZ,                   -- 计划开始
    due_at             TIMESTAMPTZ,                   -- 截止时间（逾期由它 + 语义派生，不落列）
    estimated_hours    NUMERIC(6,2),                  -- 预计工时（小时）
    actual_hours       NUMERIC(8,2)  NOT NULL DEFAULT 0, -- 实际工时（= worklog 汇总，冗余列）
    completed_at       TIMESTAMPTZ,                   -- 完成时间（semantic=DONE 时必填）
    archived_at        TIMESTAMPTZ,                   -- 归档时间（归档 = 从默认视图隐藏，不等于删除）
    version            INT           NOT NULL DEFAULT 1, -- 乐观锁版本号（看板拖拽/并发编辑）
    remark             VARCHAR(500),
    created_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at         TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by          UUID,
    update_by          UUID,
    deleted_at         TIMESTAMPTZ,                   -- 软删（回收站）
    CONSTRAINT fk_biz_task_status FOREIGN KEY (status_code)
        REFERENCES biz_task_status_def (code) ON DELETE RESTRICT,
    CONSTRAINT fk_biz_task_parent FOREIGN KEY (parent_id)
        REFERENCES biz_task (id) ON DELETE RESTRICT,
    CONSTRAINT ck_biz_task_priority CHECK (priority BETWEEN 0 AND 3),
    CONSTRAINT ck_biz_task_semantic
        CHECK (status_semantic IN ('OPEN', 'DOING', 'REVIEW', 'DONE', 'CANCELED')),
    CONSTRAINT ck_biz_task_progress CHECK (progress BETWEEN 0 AND 100),
    CONSTRAINT ck_biz_task_progress_mode CHECK (progress_mode IN ('M', 'A')),
    CONSTRAINT ck_biz_task_source
        CHECK (source IN ('SELF', 'ASSIGNED', 'TRANSFERRED', 'DECOMPOSED', 'TEMPLATE', 'IMPORT', 'RECURRENCE')),
    CONSTRAINT ck_biz_task_depth CHECK (depth BETWEEN 0 AND 5),
    CONSTRAINT ck_biz_task_self_parent CHECK (parent_id IS NULL OR parent_id <> id),
    CONSTRAINT ck_biz_task_time_order
        CHECK (due_at IS NULL OR start_at IS NULL OR due_at >= start_at),
    CONSTRAINT ck_biz_task_estimated CHECK (estimated_hours IS NULL OR estimated_hours > 0),
    CONSTRAINT ck_biz_task_actual CHECK (actual_hours >= 0),
    -- 「已完成」与「完成时间」必须同真同假：这条是完成率/逾期率统计的地基
    CONSTRAINT ck_biz_task_completed
        CHECK ((status_semantic = 'DONE') = (completed_at IS NOT NULL)),
    -- 已完成的任务进度必须 100：避免出现「已完成但 60%」的脏数据
    CONSTRAINT ck_biz_task_done_progress
        CHECK (status_semantic <> 'DONE' OR progress = 100),
    CONSTRAINT ck_biz_task_child_counts
        CHECK (child_count >= 0 AND done_child_count >= 0 AND done_child_count <= child_count)
);

COMMENT ON TABLE biz_task IS '工作任务主体：个人计划 / 上级分派 / 拆解子任务 / 看板列 / 进度汇总的唯一真源';
COMMENT ON COLUMN biz_task.task_no IS '任务编号 TK{yyyyMMdd}-{6 位序列}，全局唯一，人类可读（邮件/通知里用它指代任务）';
COMMENT ON COLUMN biz_task.status_semantic IS '冗余的状态语义：应用层唯一写入口维护；用于部分索引 WHERE status_semantic <> ''DONE'' 与统计聚合（免 join 状态表）';
COMMENT ON COLUMN biz_task.progress_mode IS 'A = 由子任务加权汇总（父任务强制 A，手动改进度返回 409）；M = 手动填写（叶子任务）';
COMMENT ON COLUMN biz_task.dept_id IS '归属部门 = 负责人部门快照。数据权限的 dept_col 只能锚定主体表列（shared/scope.rs 设计），故必须冗余而不能 join sys_user';
COMMENT ON COLUMN biz_task.task_path IS '层级路径 /上级id/本id/，子树查询用 LIKE ''{path}%''（深度 ≤ 5，不需要递归 CTE）';
COMMENT ON COLUMN biz_task.actual_hours IS '实际工时冗余列 = SUM(biz_task_worklog.hours)，工时增删同事务回写；每日巡检任务校验一致性';

-- 任务编号唯一（软删后可复用编号不影响查询，但编号本身业务上不复用 → 不加部分条件）
CREATE UNIQUE INDEX uk_biz_task_no ON biz_task (task_no);
-- 「我的待办」热路径：负责人 + 未完成语义 + 截止时间
CREATE INDEX idx_biz_task_assignee
    ON biz_task (assignee_id, status_semantic, due_at) WHERE deleted_at IS NULL;
-- 数据权限 / 部门看板热路径
CREATE INDEX idx_biz_task_dept ON biz_task (dept_id, status_semantic) WHERE deleted_at IS NULL;
-- 看板列加载：按列取前 N 条（配合 fixed 排序）
CREATE INDEX idx_biz_task_status ON biz_task (status_code, priority, due_at) WHERE deleted_at IS NULL;
-- 子树查询与拆解
CREATE INDEX idx_biz_task_parent ON biz_task (parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX idx_biz_task_path ON biz_task (task_path) WHERE task_path IS NOT NULL;
-- 临期 / 逾期扫描（只扫未完成的）
CREATE INDEX idx_biz_task_due
    ON biz_task (due_at) WHERE deleted_at IS NULL AND completed_at IS NULL AND due_at IS NOT NULL;
CREATE INDEX idx_biz_task_project ON biz_task (project_id) WHERE project_id IS NOT NULL;
CREATE INDEX idx_biz_task_creator ON biz_task (creator_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_task_assigner ON biz_task (assigner_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_task_updated ON biz_task (updated_at DESC) WHERE deleted_at IS NULL;
```

### 3.2 `V0101`：任务成员（协作人 + 关注人）+ 评论

```sql
-- ======================================================
-- V0101：任务参与关系（协作人 / 关注人）+ 评论（含 @）
-- 依据：docs/批次C+-工作任务管理-技术架构.md §3.3 / §5.4
-- 回滚：V0101__create_task_member_comment.down.sql
--
-- 为什么把任务书里的 biz_task_collaborator 扩成 biz_task_member（带 role）：
--   「协作人」与「关注人」在**可见性判定**上是同一件事（都要并进可见集合），
--   拆两张表会让列表 SQL 多一个 EXISTS，且两者的生命周期完全一致（任务没了都该没）。
--   合并后用 member_role 区分权限：COLLABORATOR 可编辑任务字段，WATCHER 只读。
-- ======================================================

-- ---------- 1) 任务成员 ----------
CREATE TABLE biz_task_member (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id    UUID        NOT NULL,
    user_id    UUID        NOT NULL,                            -- 弱引用 sys_user.id
    member_role VARCHAR(16) NOT NULL DEFAULT 'COLLABORATOR',    -- COLLABORATOR 协作 / WATCHER 关注
    added_by   UUID,                                            -- 谁加的（弱引用）
    remark     VARCHAR(255),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_biz_task_member_task FOREIGN KEY (task_id)
        REFERENCES biz_task (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_task_member_role
        CHECK (member_role IN ('COLLABORATOR', 'WATCHER')),
    CONSTRAINT uk_biz_task_member UNIQUE (task_id, user_id)
);

COMMENT ON TABLE biz_task_member IS '任务成员（协作人 + 关注人）：可见性并集的来源之一，也是评论 @ 的合法集合';
COMMENT ON COLUMN biz_task_member.member_role IS 'COLLABORATOR 可编辑任务内容与状态备注；WATCHER 只读 + 收通知';

-- 反向热路径：「我参与的任务」（可见性并集 / 我的任务页）
CREATE INDEX idx_biz_task_member_user ON biz_task_member (user_id, task_id);
CREATE INDEX idx_biz_task_member_task ON biz_task_member (task_id);

-- ---------- 2) 任务评论 ----------
CREATE TABLE biz_task_comment (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id     UUID        NOT NULL,
    parent_id   UUID,                                        -- 被回复的评论（只允许一层）
    content_md  TEXT        NOT NULL,                        -- Markdown 正文（渲染/净化复用 shared::content）
    author_id   UUID        NOT NULL,                        -- 作者（弱引用）
    author_name VARCHAR(50),                                 -- 作者名快照（改名后历史可读）
    mention_ids UUID[]      NOT NULL DEFAULT '{}',           -- @ 到的用户（GIN 反查「@我的」）
    edited_at   TIMESTAMPTZ,                                 -- 编辑时间（非空 = 已编辑过）
    deleted_at  TIMESTAMPTZ,                                 -- 软删（保留审计，不物理删）
    created_at  TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_biz_task_comment_task FOREIGN KEY (task_id)
        REFERENCES biz_task (id) ON DELETE CASCADE,
    CONSTRAINT fk_biz_task_comment_parent FOREIGN KEY (parent_id)
        REFERENCES biz_task_comment (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_task_comment_content CHECK (length(btrim(content_md)) > 0),
    CONSTRAINT ck_biz_task_comment_parent CHECK (parent_id IS NULL OR parent_id <> id)
);

COMMENT ON TABLE biz_task_comment IS '任务评论：支持 @ 成员；不可见者不允许被 @（否则等于泄露任务标题），见 §5.5';
COMMENT ON COLUMN biz_task_comment.mention_ids IS 'UUID 数组 + GIN 索引；@ 的对象必须是任务可见者（应用层校验，422 TASK_MENTION_FORBIDDEN）';

CREATE INDEX idx_biz_task_comment_task ON biz_task_comment (task_id, created_at) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_task_comment_mention ON biz_task_comment USING GIN (mention_ids);
CREATE INDEX idx_biz_task_comment_author ON biz_task_comment (author_id, created_at DESC);
```

### 3.3 `V0102`：附件 + 依赖 + 标签

```sql
-- ======================================================
-- V0102：任务附件 / 依赖关系 / 标签
-- 依据：docs/批次C+-工作任务管理-技术架构.md §3.4 / §3.5
-- 回滚：V0102__create_task_attach_dep_tag.down.sql
-- ======================================================

-- ---------- 1) 任务附件（与 sys_announcement_attachment 同构，复用 sys_file 存储底座） ----------
CREATE TABLE biz_task_attachment (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id     UUID         NOT NULL,
    file_id     UUID         NOT NULL,                     -- 弱引用 sys_file.id（批次 A 存储）
    file_name   VARCHAR(255) NOT NULL,                     -- 文件名快照（sys_file 改名不回溯）
    file_size   BIGINT       NOT NULL DEFAULT 0,
    mime_type   VARCHAR(128),
    sort_order  INT          NOT NULL DEFAULT 0,
    uploaded_by UUID,
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_biz_task_attachment_task FOREIGN KEY (task_id)
        REFERENCES biz_task (id) ON DELETE CASCADE,
    CONSTRAINT uk_biz_task_attachment UNIQUE (task_id, file_id)
);

COMMENT ON TABLE biz_task_attachment IS '任务附件：只存 sys_file 的引用 + 展示快照；下载必须过「任务可见性校验 + 服务端流式代理」，不暴露 business:storage:download';
CREATE INDEX idx_biz_task_attachment_task ON biz_task_attachment (task_id, sort_order);
CREATE INDEX idx_biz_task_attachment_file ON biz_task_attachment (file_id);

-- ---------- 2) 任务依赖（前置任务） ----------
-- 本批只做 FS（Finish-to-Start）一种：前置任务完成前，后置任务不允许进入 DOING。
-- 且是**软约束**（可 force=true 强推，留审计），不做自动排程/关键路径（那要甘特 + 基线，见 §9 不做清单）。
CREATE TABLE biz_task_dependency (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id           UUID        NOT NULL,                -- 后置任务（要被卡住的那个）
    depends_on_task_id UUID       NOT NULL,                -- 前置任务（必须先完成）
    dep_type          VARCHAR(8)  NOT NULL DEFAULT 'FS',   -- 依赖类型（本批只允许 FS）
    created_by        UUID,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_biz_task_dep_task FOREIGN KEY (task_id)
        REFERENCES biz_task (id) ON DELETE CASCADE,
    CONSTRAINT fk_biz_task_dep_depends FOREIGN KEY (depends_on_task_id)
        REFERENCES biz_task (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_task_dep_type CHECK (dep_type IN ('FS')),
    CONSTRAINT ck_biz_task_dep_self CHECK (task_id <> depends_on_task_id),
    CONSTRAINT uk_biz_task_dep UNIQUE (task_id, depends_on_task_id)
);

COMMENT ON TABLE biz_task_dependency IS '任务依赖（前置-后置）：只做 FS 一种 + 软约束（可强推并留痕），不做自动排程';
COMMENT ON COLUMN biz_task_dependency.task_id IS '后置任务：前置未完成时禁止进入 DOING（应用层校验，403 TASK_DEPENDENCY_BLOCKED）';

CREATE INDEX idx_biz_task_dep_task ON biz_task_dependency (task_id);
CREATE INDEX idx_biz_task_dep_depends ON biz_task_dependency (depends_on_task_id);

-- ---------- 3) 标签 ----------
CREATE TABLE biz_task_tag (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tag_name   VARCHAR(50)  NOT NULL,
    color      VARCHAR(16)  NOT NULL DEFAULT 'default',
    sort_order INT          NOT NULL DEFAULT 0,
    status     SMALLINT     NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by  UUID,
    update_by  UUID,
    deleted_at TIMESTAMPTZ
);

COMMENT ON TABLE biz_task_tag IS '任务标签字典（全局共享，非个人私有）：用于跨项目横切检索（如「线上故障」「客户A」）';

CREATE UNIQUE INDEX uk_biz_task_tag_name ON biz_task_tag (tag_name) WHERE deleted_at IS NULL;

CREATE TABLE biz_task_tag_rel (
    task_id    UUID NOT NULL,
    tag_id     UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT pk_biz_task_tag_rel PRIMARY KEY (task_id, tag_id),
    CONSTRAINT fk_biz_task_tag_rel_task FOREIGN KEY (task_id)
        REFERENCES biz_task (id) ON DELETE CASCADE,
    CONSTRAINT fk_biz_task_tag_rel_tag FOREIGN KEY (tag_id)
        REFERENCES biz_task_tag (id) ON DELETE CASCADE
);

COMMENT ON TABLE biz_task_tag_rel IS '任务-标签关联（多对多）';

CREATE INDEX idx_biz_task_tag_rel_tag ON biz_task_tag_rel (tag_id, task_id);
```

### 3.4 `V0103`：工时 + 项目 + 项目成员 + 模板

```sql
-- ======================================================
-- V0103：工时记录 / 任务模板 / 项目（分组）与项目成员
-- 依据：docs/批次C+-工作任务管理-技术架构.md §3.6 / §3.7 / §3.8
-- 回滚：V0103__create_task_worklog_template_project.down.sql
-- ======================================================

-- ---------- 1) 工时记录 ----------
-- 只记「谁在哪天为哪个任务投入多少小时」，不做审批、不做考勤、不锁期间。
-- 同一人同一天同一任务允许多条（上午 2h + 下午 1.5h 是常态），日汇总在 SQL 里 GROUP BY。
CREATE TABLE biz_task_worklog (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id    UUID          NOT NULL,
    user_id    UUID          NOT NULL,               -- 投入人（弱引用）
    work_date  DATE          NOT NULL,               -- 投入日期（业务日期，非写入时间）
    hours      NUMERIC(5,2)  NOT NULL,               -- 工时（小时），0 < h <= 24
    note       VARCHAR(500),                         -- 说明
    created_at TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ   NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by  UUID,
    update_by  UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT fk_biz_task_worklog_task FOREIGN KEY (task_id)
        REFERENCES biz_task (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_task_worklog_hours CHECK (hours > 0 AND hours <= 24)
);

COMMENT ON TABLE biz_task_worklog IS '工时记录：任务维度的人天投入明细；汇总回写 biz_task.actual_hours（同事务）';
COMMENT ON COLUMN biz_task_worklog.work_date IS '投入日期（业务日，允许补录历史；不允许未来日期，应用层校验）';

CREATE INDEX idx_biz_task_worklog_task ON biz_task_worklog (task_id) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_task_worklog_user_date ON biz_task_worklog (user_id, work_date) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_task_worklog_date ON biz_task_worklog (work_date) WHERE deleted_at IS NULL;

-- ---------- 2) 项目（轻量分组） ----------
-- 为什么不叫「里程碑」：里程碑带日期锚点与进度引擎，属于甘特范畴（本批不做，见 §9）。
-- 「项目」这里只承担三件事：跨部门协作的容器、看板/列表的分组维度、项目成员可见性来源。
CREATE TABLE biz_task_project (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_code VARCHAR(32)  NOT NULL,              -- 项目编号（人类可读，如 PRJ2026-001）
    project_name VARCHAR(100) NOT NULL,
    description  TEXT         NOT NULL DEFAULT '',   -- 说明（Markdown）
    owner_id     UUID,                               -- 项目负责人（弱引用）
    dept_id      UUID,                               -- 归属部门（主责部门，弱引用）
    status       VARCHAR(16)  NOT NULL DEFAULT 'PLANNING', -- PLANNING / ACTIVE / CLOSED
    start_at     TIMESTAMPTZ,
    end_at       TIMESTAMPTZ,
    task_count   INT          NOT NULL DEFAULT 0,    -- 任务数（冗余计数）
    done_count   INT          NOT NULL DEFAULT 0,    -- 已完成任务数（冗余计数）
    created_at   TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    update_by    UUID,
    deleted_at   TIMESTAMPTZ,
    CONSTRAINT ck_biz_task_project_status
        CHECK (status IN ('PLANNING', 'ACTIVE', 'CLOSED')),
    CONSTRAINT ck_biz_task_project_time
        CHECK (end_at IS NULL OR start_at IS NULL OR end_at >= start_at),
    CONSTRAINT ck_biz_task_project_counts
        CHECK (task_count >= 0 AND done_count >= 0 AND done_count <= task_count)
);

COMMENT ON TABLE biz_task_project IS '项目（轻量分组）：跨部门协作容器 + 看板/列表分组维度 + 项目成员可见性来源；不含里程碑与项目级进度引擎';
COMMENT ON COLUMN biz_task_project.done_count IS '已完成任务数冗余（项目卡片进度展示）；由任务状态迁移时同事务回写，每日巡检校验';

CREATE UNIQUE INDEX uk_biz_task_project_code ON biz_task_project (project_code) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_task_project_status ON biz_task_project (status, start_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_task_project_owner ON biz_task_project (owner_id) WHERE deleted_at IS NULL;

CREATE TABLE biz_task_project_member (
    project_id UUID NOT NULL,
    user_id    UUID NOT NULL,
    member_role VARCHAR(16) NOT NULL DEFAULT 'MEMBER', -- OWNER 负责人 / MEMBER 成员
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT pk_biz_task_project_member PRIMARY KEY (project_id, user_id),
    CONSTRAINT fk_biz_task_project_member_project FOREIGN KEY (project_id)
        REFERENCES biz_task_project (id) ON DELETE CASCADE,
    CONSTRAINT ck_biz_task_project_member_role
        CHECK (member_role IN ('OWNER', 'MEMBER'))
);

COMMENT ON TABLE biz_task_project_member IS '项目成员：项目下全部任务对成员可见（可见性并集的第三个来源，见 §5.3）';

CREATE INDEX idx_biz_task_project_member_user ON biz_task_project_member (user_id, project_id);

-- ---------- 3) 任务模板 ----------
-- 模板 = 「标题 + 正文 + 默认属性 + 子任务定义（JSONB 数组）」。一键生成 = 插 1 个父任务 + N 个子任务。
-- 为什么子任务放 JSONB 而不是另建 biz_task_template_item：
--   模板项没有独立生命周期（不会单条编辑、不需要按项查询/统计），一张从表只是为了「看起来关系化」。
--   生成时的正确性由应用层按 JSON Schema 校验（标题必填、priority 0..3、offset_days 0..365）。
CREATE TABLE biz_task_template (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_code         VARCHAR(32)  NOT NULL,     -- 模板编码（如 ONBOARD_NEW_HIRE）
    template_name         VARCHAR(100) NOT NULL,
    category              VARCHAR(50),               -- 分类（前端分组）
    content_md            TEXT         NOT NULL DEFAULT '',
    default_priority      SMALLINT     NOT NULL DEFAULT 2,
    default_estimated_hours NUMERIC(6,2),
    default_due_offset_days INT        NOT NULL DEFAULT 7, -- 截止 = 生成日 + N 天
    subtasks              JSONB        NOT NULL DEFAULT '[]'::jsonb, -- [{title,priority,due_offset_days,estimated_hours,assignee_role}]
    tag_ids               UUID[]       NOT NULL DEFAULT '{}',
    apply_count           INT          NOT NULL DEFAULT 0, -- 被使用次数（模板热度）
    status                SMALLINT     NOT NULL DEFAULT 1,
    remark                VARCHAR(255),
    created_at            TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at            TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by             UUID,
    update_by             UUID,
    deleted_at            TIMESTAMPTZ,
    CONSTRAINT ck_biz_task_template_priority CHECK (default_priority BETWEEN 0 AND 3),
    CONSTRAINT ck_biz_task_template_subtasks CHECK (jsonb_typeof(subtasks) = 'array'),
    CONSTRAINT ck_biz_task_template_offset CHECK (default_due_offset_days BETWEEN 0 AND 365)
);

COMMENT ON TABLE biz_task_template IS '任务模板：一键生成「父任务 + N 个子任务」（如「新员工入职」8 个子任务）；子任务定义放 JSONB，不建从表';
COMMENT ON COLUMN biz_task_template.subtasks IS 'JSONB 数组，元素 {title,priority,due_offset_days,estimated_hours}；生成时逐条校验，非法即 400';

CREATE UNIQUE INDEX uk_biz_task_template_code ON biz_task_template (template_code) WHERE deleted_at IS NULL;
CREATE INDEX idx_biz_task_template_status ON biz_task_template (status, apply_count DESC) WHERE deleted_at IS NULL;
```

### 3.5 `V0104` / `V0105`：权限、菜单、配置、偏好

两个迁移文件较长，关键片段如下（全文见附录 A 的脚本路径）：

**看板列种子（V0104 第 1 段）**

```sql
INSERT INTO biz_task_status_def
    (code, name, semantic, color, sort_order, is_initial, is_final, wip_limit, status, remark)
VALUES
    ('TODO',     '待开始', 'OPEN',     'default', 10, TRUE,  FALSE, NULL, 1, '新建任务的默认落列'),
    ('DOING',    '进行中', 'DOING',    'primary', 20, FALSE, FALSE, NULL, 1, '可设 WIP 上限（仅提示）'),
    ('REVIEW',   '待验收', 'REVIEW',   'warning', 30, FALSE, FALSE, NULL, 1, '提交验收后进入'),
    ('DONE',     '已完成', 'DONE',     'success', 40, FALSE, TRUE,  NULL, 1, '终态；completed_at 必填，progress 必为 100'),
    ('CANCELED', '已取消', 'CANCELED', 'danger',  50, FALSE, TRUE,  NULL, 1, '终态；不计入完成率分母')
ON CONFLICT (code) DO NOTHING;
```

**菜单与授权（V0104 的三段结构）**

| 段 | 内容 | 授权对象 |
|---|---|---|
| ① | 目录「任务协作」`…0800` + 5 个页面 `…0801–…0805`（任务板/我的任务/列表/日历/项目）+ 全员按钮 `…0811–…0819` / `…0821` / `…0831`（11 个），**共 17 个节点** | **所有未软删角色**（`SELECT r.id FROM sys_role r WHERE deleted_at IS NULL`） |
| ② | 管理节点 `…0806`/`…0807`（统计页/设置页）+ 管理按钮 `…0822–…0830` / `…0832`，**共 12 个节点** | **仅 `SUPER_ADMIN`**，其余角色由运维按需追加 |
| ③ | `DO $$` 自检：启用列 ≥5、`is_initial` 恰好 1 个、每个未软删角色都有 `business:task:list`、管理码不泄漏给非超管 | 任一不满足 → 迁移失败 |

**V0105 的四类种子**

| 类别 | 数量 | 内容 |
|---|---|---|
| `sys_config`（`task.*`） | 8 | `due_remind_hours=24`、`overdue_scan_enabled=true`、`max_depth=5`、`auto_review_on_children_done=true`、`board_column_page_size=50`、`comment_max_chars=5000`、`attachment_max_mb=50`、`notify_batch_window_seconds=60` |
| `sys_storage_policy` | 1 | `category='task'`，50 MB，白名单 17 种扩展名（**不含** exe/sh/bat/js/vbs/msi/ps1/jar/so/dll），`direct_upload=true` |
| `sys_preference_def` | 4 | `task.view_mode`(kanban/list/calendar)、`task.swimlane`(none/assignee/priority/project)、`task.default_scope`(assigned/created/participated/watched)、`task.show_done`(bool) |
| `DO $$` 自检 | — | 8 / 1 / 4 三项数量必须精确匹配，否则迁移失败 |

### 3.6 为什么是这 12 张表（而不是多 3 张或少 2 张）

| 候选表 | 判断 | 理由 |
|---|---|---|
| `biz_task_recur`（周期规则） | **不建**（P1） | 见 §2.2-#4。P1 时 `ALTER TABLE biz_task ADD COLUMN recur_rule JSONB` 是 PG 的瞬时操作（可空列不重写表），现在建表反而是空壳 |
| `biz_task_custom_field`（自定义字段） | **不建**（不做） | 见 §2.2-#9 |
| `biz_task_milestone`（里程碑） | **不建**（P1） | 见 §2.2-#6。里程碑本质是「带日期的任务」，P1 时用 `biz_task(is_milestone BOOLEAN)` 即可，不必建表 |
| `biz_task_watcher`（与 member 分表） | **合并** | 见 §0.2 |
| `biz_task_template_item`（模板从表） | **合并为 JSONB** | 模板项无独立生命周期 |
| `biz_task_history`（自建变更历史表） | **不建** | 复用 `sys_change_log`（§1.3），自建会出现两套「谁改了什么」 |
| `biz_task_reminder`（提醒排程表） | **不建** | 提醒由「`due_at` + 幂等键」临时算出，落排程表会与 `due_at` 修改不同步（改了截止时间，旧排程还在） |

---
## 4. 核心机制设计（本设计的重点）

### 4.1 `status_semantic` 的维护：唯一写入口 + 每日巡检（不引入触发器）

**为什么要有这个冗余列**：三件事都指望它——

1. `CHECK ((status_semantic = 'DONE') = (completed_at IS NOT NULL))` 把「已完成必须有完成时间」变成数据库不变量（DDL 里已有，实测拒绝了 2 条脏数据，§5.5-N3/N4）；
2. 部分索引 `WHERE … status_semantic <> 'DONE'` 让「我的待办」「临期扫描」不必 join 状态表（实测临期扫描 0.5 ms）；
3. 统计聚合按语义分组，不受自定义列数量影响。

**为什么不用触发器**：全库手动实测 **零触发器**（`pg_dump -s` 里 `CREATE TRIGGER` = 0）。引入第一个触发器会带来连锁成本：Rust 单测体系看不到触发器行为（本项目测试全部是「handler + SQL」两层）、迁移回滚要额外管触发器、`EXPLAIN` 与本地 POC 复现多一层黑盒。**保持零触发器**，用下面三条保证一致性：

| 保证 | 做法 |
|---|---|
| ① 唯一写入口 | 状态只能通过 `model::set_status(ex, task_id, expect_version, new_code, actor)` 改；它内部**一条 SQL** 同时写两列（见下），不允许任何其它地方 `UPDATE biz_task SET status_code`（CI 加一条 grep 门禁：`biz_task` 的 `UPDATE` 语句必须出现在 `business/task/model.rs` 内） |
| ② 一条 SQL 同时写 | `UPDATE biz_task t SET status_code = s.code, status_semantic = s.semantic, completed_at = CASE WHEN s.semantic = 'DONE' THEN now() ELSE NULL END, progress = CASE WHEN s.semantic = 'DONE' THEN 100 ELSE t.progress END, version = t.version + 1 FROM biz_task_status_def s WHERE s.code = ? AND s.status = 1 AND t.id = ? AND t.version = ? AND t.deleted_at IS NULL` — 实测 `UPDATE 1`（`version` 1→2，`progress` →100，`completed_at` 赋值成功），见 §5.5 |
| ③ 每日巡检 | 定时任务 `task_consistency_check`（每日 04:20）跑三条对账 SQL：`status_semantic` 与 `status_def.semantic` 不一致的行、`actual_hours` 与 `SUM(worklog)` 不一致的行、`child_count/done_child_count` 与真实子任务数不一致的行；发现即修 + `tracing::error!` 报警（不静默） |

```sql
-- 巡检 SQL 之一（实测 20 万行 ~120 ms，可每日跑）
SELECT t.id, t.status_code, t.status_semantic, s.semantic AS expected
  FROM biz_task t
  JOIN biz_task_status_def s ON s.code = t.status_code
 WHERE t.status_semantic <> s.semantic
   AND t.deleted_at IS NULL
 LIMIT 200;
```

### 4.2 分派规则

#### 4.2.1 「上级」的判定（实证结论，见 §1.1）

`sys_user` 没有上级字段，因此定义：

```text
is_from_superior(assigner, assignee) :=
   ┌ assigner = assignee                                    → false（自己派自己）
   ├ assignee.dept_id ∈ ancestors_depts(assigner) 且 assigner 是其中某部门的 leader_id → true
   └ 其它                                                    → false（平行/跨部门协作）
```

其中 `ancestors_depts(assigner)` = assigner 作为 `leader_id` 的部门 + 其子树（沿 `sys_dept.parent_id`）。

**落地形态**（一条 SQL，走 `sys_dept` 的两个索引 `idx_sys_dept_parent_id` / `idx_sys_dept_leader_id`… 实测库上 `idx_sys_dept_parent_id` 存在，`leader_id` 无独立索引 → **V0100 需补一条**，见下方「索引补丁」）：

```sql
-- 判断 assigner 是否是 assignee 的上级（用于「上级分派」标签 + 验收人判定）
WITH RECURSIVE my_scope AS (
    SELECT d.id, d.parent_id, 0 AS lvl
      FROM sys_dept d
     WHERE d.leader_id = $assigner AND d.deleted_at IS NULL
    UNION ALL
    SELECT c.id, c.parent_id, s.lvl + 1
      FROM sys_dept c JOIN my_scope s ON c.parent_id = s.id
     WHERE c.deleted_at IS NULL AND s.lvl < 8
)
SELECT EXISTS (SELECT 1 FROM my_scope m JOIN sys_user u ON u.dept_id = m.id
                WHERE u.id = $assignee) AS is_subordinate;
```

> **索引补丁（报批项）**：`sys_dept.leader_id` 目前没有索引（`V0006` 只建了 `code/parent_id/path` 三个）。`SELECT … WHERE leader_id = ?` 会全表扫（部门表通常 < 1000 行，实际影响可忽略，但「验收人判定」在状态流转热路径上）。**方案：在 `V0100` 里建** `CREATE INDEX IF NOT EXISTS idx_sys_dept_leader ON sys_dept (leader_id) WHERE deleted_at IS NULL;`——这是对 `sys_dept` 的**加索引**（不改表、不改数据、不影响任何既有查询计划的正向性），但仍属「动 `system/` 相关表」，**必须单独报批**（任务书硬约束）。若不批，退化为「部门表全表扫」，在 < 1000 行的部门表上是 0.1 ms 级开销，功能不受影响。

#### 4.2.2 能分派给谁（授权边界）

**结论：复用批次 B 的 `ScopedPrincipal`，不自造组织权限。**

```
can_assign_to(actor, target_user) :=
    actor 持有 business:task:assign
 AND target_user.status = 1 AND target_user.deleted_at IS NULL
 AND target_user.id <> actor.id
 AND (
        principal.is_super()
     OR principal.dept_ids().contains(target_user.dept_id)      -- 我的数据范围
     OR target_user.id ∈ 同任务协作人（移交场景：把任务给已经是协作人的人）
 )
```

| 场景 | 是否允许 | 说明 |
|---|---|---|
| 部门主管 → 本部门下属 | ✅ | `dept_ids` 含本部门（`Dept` 范围） |
| 上级部门主管 → 下级部门 | ✅ | `DeptAndChild` / `Custom` 范围展开子树（`principal.rs:289` 的 `RESOLVE_SQL` 已递归算好） |
| 普通员工 → 同部门同事 | ❌ | `SelfOnly` 范围下 `dept_ids` 为空（`principal.rs` 降级逻辑），直接 403 |
| 跨部门平行分派（如让财务帮我做报销） | ❌（须走「协作人」或「项目成员」） | 这是**有意的**：跨部门强行分派是组织越权的常见来源。跨部门协作的正确姿势是「邀请为协作人」或「拉进项目」 |
| 超管 → 任何人 | ✅ | `is_super()` 短路（`scope.rs:442-445` 同款取向） |

**为什么不用「我是否有下级」来判断**：那需要「用户 → 下级列表」的查询，而它正是 `RESOLVE_SQL` 已经算过的东西（部门子树），再算一遍就是第二套语义。

#### 4.2.3 分派之后原负责人怎么办

| 操作 | 语义 | 数据动作 | 通知 |
|---|---|---|---|
| **分派**（新建时指定负责人） | `source=ASSIGNED`，`assigner_id=me` | 新任务 `assignee_id=target`，`dept_id=target 的部门`（快照） | 通知 target |
| **转派**（换负责人） | `source=TRANSFERRED`，`origin_task_id=原任务`（同一条任务，不新建） | `assignee_id := target`；`dept_id := target.dept_id`；**原负责人自动入 `biz_task_member(COLLABORATOR)`**（不丢失上下文）；`version += 1`；changelog `ASSIGN` | 通知 target（新负责人）+ 原负责人（已被转派）+ 分派人（若不同） |
| **拆解**（建子任务） | 新建子任务，`source=DECOMPOSED`，`parent_id=父任务` | 父任务**负责人不变**；父任务负责人自动成子任务 `WATCHER`；父任务 `child_count += n`、`progress_mode := 'A'` | 通知各子任务负责人 + 父任务负责人（汇总变化） |
| **移交**（不改来源，只换人 + 保留原负责人为协作人） | 与转派同实现，`source` 保持原值 | 同转派 | 同上 |

> **关键取舍**：转派**不新建任务**。新建会产生两个 id，导致评论、附件、工时、历史全部断在旧 id 上（用户看到的「任务」变成了两条互不相关的记录）。`origin_task_id` 只用于「拆解」与「复制」的血缘追溯。

### 4.3 状态机

#### 4.3.1 两层结构与迁移矩阵

**列（`status_code`）自由，语义（`status_semantic`）固定 5 个**。迁移合法性有两类规则：

```text
规则 A（语义层，硬拦）：from.semantic → to.semantic 必须在下面的矩阵里为 TRUE
规则 B（列层，软限）：同语义之间任意换列；is_final 的列不可作为「进入 DOING」的来源
```

**25 种组合的完整矩阵**（这张表就是 `model.rs` 里的 `const TRANSITIONS: [[bool; 5]; 5]`，SQL 侧实测输出见 §5.5-②）：

| from \ to | OPEN | DOING | REVIEW | DONE | CANCELED |
|---|---|---|---|---|---|
| **OPEN** | ✅ 换列 | ✅ 开始 | ❌ 未开始不能提验收 | ❌ 必须先 DOING | ✅ 取消 |
| **DOING** | ✅ 退回排期 | ✅ 换列 | ✅ 提交验收 | ✅ 直通完成（配置控制） | ✅ 取消 |
| **REVIEW** | ❌ 必须先驳回 | ✅ 驳回 | ✅ 换列 | ✅ 验收通过 | ✅ 取消 |
| **DONE** | ❌ | ✅ 重开 | ❌ | ✅ 幂等 | ❌ 已完成不可取消 |
| **CANCELED** | ✅ 恢复 | ❌ | ❌ | ❌ | ✅ 幂等 |

**每条 FALSE 的理由**（不是随手定）：

| 被禁的迁移 | 理由 |
|---|---|
| `OPEN → REVIEW` | 未开始就提交验收 = 没有交付物可验，会导致验收环节充斥空任务 |
| `OPEN → DONE` | 跳过 DOING 就没有「谁在做、做了多久」的痕迹，工时与完成率都无法归因 |
| `REVIEW → OPEN` | 驳回的正确落点是 DOING（继续做），回 OPEN 会让任务丢了「已经在做」的事实 |
| `DONE → OPEN` | 已完成不能直接回未开始；重开走 `DONE → DOING`（保留完成历史在 changelog） |
| `DONE → REVIEW` | 已完成再提交验收无意义 |
| `DONE → CANCELED` | 会破坏完成率口径（同一任务一会儿算完成、一会儿算取消） |
| `CANCELED → DOING/REVIEW/DONE` | 恢复必须先回 OPEN 重新排期（取消往往意味着负责人/时间都要重定） |

#### 4.3.2 验收人与「谁能推进状态」

| 目标语义 | 谁可以推进 | 校验 |
|---|---|---|
| `DOING`（开始 / 驳回 / 重开） | 负责人、协作人、分派人、部门主管（scope 内） | `business:task:status` + `can_edit_task` |
| `REVIEW`（提交验收） | **负责人**（或协作人代办） | 同上 |
| `DONE`（验收通过） | **验收人**：①`assigner_id`（有分派人）②否则 `assignee.dept.leader_id` ③都没有 → 负责人自己（`DOING → DONE` 直通，受 `task.auto_review_on_children_done` 之外的独立开关控制，默认允许） | `business:task:accept` + `is_acceptor(task, actor)` |
| `CANCELED` | 分派人 / 负责人 / 部门主管 | 取消需要 `reason`（≥5 字，写进 changelog 的 `action_label`） |

> **验收权为什么绑在「分派人」而不是「直接上级」**：分派是「我交给你做」的契约，验收权自然在派活的人手上；而「直接上级」在矩阵组织里经常有多个（部门主管 + 项目负责人），判定会互相打架。分派人为空（自建任务）才回落到部门 leader，这是兜底不是主路径。

### 4.4 可见性：范围 ∪ 参与者（本设计最关键的一条 SQL）

#### 4.4.1 语义定义

一条任务对 `me` 可见，当且仅当满足**任一**：

```text
① 落在我 数据范围（scope）内            -- 批次 B 的 5 种范围，scope::filter_for 生成
② 我是负责人 / 创建人 / 分派人            -- 我自己的事，永远可见
③ 我是协作人或关注人                     -- biz_task_member
④ 我是该任务所属项目的成员               -- biz_task_project_member
```

`②③④` 合起来叫「参与者」，必须先算成一个 **id 集合**（而不是 3 个 `OR EXISTS`）——原因见 §5.1 的实测（3 个 `OR EXISTS` 会把索引全废掉）。

#### 4.4.2 SQL 模板（`scope::paginate` 一行不改）

```sql
-- base_sql（注意 {FILTER} 的位置：它在最外层括号内的第一个位置）
SELECT t.id, t.task_no, t.title, t.status_code, t.status_semantic,
       t.priority, t.progress, t.due_at, t.assignee_id, u.real_name AS assignee_name
  FROM biz_task t
  LEFT JOIN sys_user u ON u.id = t.assignee_id AND u.deleted_at IS NULL
 WHERE t.deleted_at IS NULL
   AND t.archived_at IS NULL
   AND (TRUE {FILTER}
        OR t.id = ANY(?::text::uuid[]))
   AND t.status_semantic <> 'DONE'          -- 业务过滤（用字面量，不占参数位）
-- order_by = "ORDER BY t.priority, t.due_at NULLS LAST, t.id"
-- base_params = [participant_ids_text_array]
```

注入后（`DeptAndChild` 范围）：

```sql
AND (TRUE  AND t.dept_id = ANY('{…}'::text::uuid[]) OR t.id = ANY('{…}'::text::uuid[]))
```

三个「为什么这样写」：

| 决定 | 原因 |
|---|---|
| `{FILTER}` 放在 `(TRUE … OR …)` 里 | `inject_filter`（`scope.rs:772-792`）会**两侧各补一个空格**并裁掉原空白，所以 `TRUE {FILTER}` → `TRUE AND t.dept_id = …`，语法正确、括号包住整个 OR，语义就是「范围 ∪ 参与者」 |
| 参与者用 **id 数组**而不是 `EXISTS` 子查询 | 数组能走 `BitmapOr(idx_biz_task_dept, biz_task_pkey)`（实测），`EXISTS` 让优化器放弃 OR 上的位图合并 → Seq Scan（实测 539 ms） |
| 数组用 **文本字面量 + 双重 cast**（`?::text::uuid[]`） | 本栈驱动不支持 `Value::Array` 绑定（`scope.rs:32-43` 已记录该限制与实测证据），与 `scope.rs` 的 `array_param`（`:550-562`）同一套路 |

#### 4.4.3 参数顺序（不能错）

`paginate` 的契约是「**片段参数恒前置**于 `base_params`」（`scope.rs:826-834`），而 `merge_params` 按 `[filter_params] ++ [base_params]` 拼。所以：

```text
SQL 里 ? 出现顺序：  ① scope 片段（dept 数组）→ ② 参与者 id 数组
参数列表顺序：       ① filter.params()           ② base_params
```

这正是 `{FILTER}` 出现在 `OR t.id = ANY(?)` **之前**的原因。写反了不会报错，只会绑定错值（`scope.rs:596-598` 明确警告过这个静默错位），所以实施时必须有一个单测断言「绑定参数顺序 = SQL 里 `?` 的顺序」。

#### 4.4.4 上限与降级（D7）

参与者 id 集合由一条索引查询算出（实测：轻用户 425 个 id / 12 ms，§5.2）：

```sql
SELECT array_agg(task_id) FROM (
    SELECT task_id FROM biz_task_member WHERE user_id = $me
    UNION SELECT id FROM biz_task WHERE assignee_id = $me AND deleted_at IS NULL
    UNION SELECT id FROM biz_task WHERE creator_id  = $me AND deleted_at IS NULL
    UNION SELECT id FROM biz_task WHERE assigner_id = $me AND deleted_at IS NULL
    UNION SELECT t.id FROM biz_task t JOIN biz_task_project_member pm ON pm.project_id = t.project_id
           WHERE pm.user_id = $me AND t.deleted_at IS NULL
) z;
```

| id 数量 | 行为 |
|---|---|
| ≤ 2000 | 走「范围 ∪ 参与者」并集（实测 37–40 ms） |
| > 2000 | **降级为只按范围**，响应体带 `"participant_truncated": true`，前端在列表页顶部提示「你参与的任务过多（>2000），本视图只显示范围内的任务；『我参与的』请用独立 tab」 |
| 独立 tab（`GET /task/mine?tab=participated`） | **永远纯参与者查询**（无 OR、无数组），天然快（实测 4 ms 级），不受上限影响 |

> 为什么阈值是 2000：实测 425 个 id = 40 ms，66,733 个 = >15 s（超时）。按 BitmapOr 的探测成本近似线性，2000 个约 **120–150 ms**（保守估），仍在新能接受的 P95 内；再往上单次请求的参数体积（2000×37 B ≈ 74 KB）也开始不划算。

### 4.5 进度模型

#### 4.5.1 冲突规则（D4）

```text
has_children(task)  →  progress_mode = 'A'（自动），progress 只读（手动写 → 409 TASK_PROGRESS_READONLY）
no children         →  progress_mode = 'M'（手动），0..100 自由填
```

**为什么「有子任务就一定听汇总」**：如果允许手动覆盖，会出现「父任务 90%，但其 8 个子任务里 5 个还没开始」这种自相矛盾的展示，而父进度会被用于向上汇报（老大看到的就是它）。

#### 4.5.2 汇总公式（加权，不是等权）

```text
weight(child)   = COALESCE(child.estimated_hours, 1)     -- 没填预计工时的按 1 小时估
parent.progress = round( Σ(child.progress × weight) / Σ(weight) )
                  -- 分母**剔除** status_semantic = 'CANCELED' 的子任务
```

实测（§5.5-⑤）：子任务 A（100%，4 h）+ 子任务 B（40%，6 h）→ `(100×4 + 40×6) / 10 = 64`，回写后 `progress=64, progress_mode='A', child_count=2, done_child_count=1` ✓。

**为什么加权**：等权会让「1 小时的子任务」和「40 小时的子任务」同权。加权后父进度近似「按工作量完成度」，这是项目汇报里人们真正想看的数字。用户不填工时时退化为等权（权重全 1），不会算不出来。

#### 4.5.3 父子联动（D5）

| 触发 | 动作 | 配置 |
|---|---|---|
| 子任务新增/删除/状态变/进度变 | 同事务内重算**直接父任务**的 `progress/child_count/done_child_count`，并向上递归（深度 ≤ 5，一条 SQL 用 `task_path` 前缀匹配所有祖先，见下） | 无 |
| 全部子任务 `DONE`（且至少 1 个） | 父任务语义 `DOING → REVIEW`（**不自动 DONE**），通知父任务负责人 | `task.auto_review_on_children_done`（默认 true） |
| 父任务被取消 | 子任务**不动**（让人自己决定），但父任务不再参与汇总 | 无 |
| 父任务只是改负责人 | 子任务负责人不变 | 无 |

```sql
-- 一次重算所有祖先（task_path 前缀匹配，深度 ≤5 → 最多 5 次索引探测，不递归全表）
UPDATE biz_task p
   SET progress = agg.p, child_count = agg.c, done_child_count = agg.d,
       progress_mode = CASE WHEN agg.c > 0 THEN 'A' ELSE p.progress_mode END,
       updated_at = now()
  FROM (
    SELECT substr(t.task_path, 1, length(t.task_path) - 37) AS ancestor_path,
           … -- 聚合（略）
  ) agg
 WHERE p.task_path = agg.ancestor_path …;
```

> 实现上更简单可靠的做法：先查出所有祖先（`SELECT id FROM biz_task WHERE ? LIKE task_path || '%' AND depth < ?`），再对每个祖先跑一次聚合 UPDATE（祖先 ≤ 5 个）。**不用递归 CTE**（与 `scope.rs` S4 的取向一致：不把递归塞进热路径 SQL）。

### 4.6 依赖关系（只做 FS 软约束）

| 项 | 结论 | 理由 |
|---|---|---|
| 依赖类型 | **只 FS**（Finish-to-Start） | SS/FF/SF 要甘特与基线才有意义 |
| 强制程度 | **软约束**：前置未完成时进入 `DOING` 返回 403 `TASK_DEPENDENCY_BLOCKED`，但带 `force=true` 可强推，强推写入 changelog（`action_label='强制开始（前置未完成）'`） | 现实中前置经常「事实上已完成但没点完成」，硬拦会让人绕开系统 |
| 循环检测 | 设置 `X.parent_id = P` 或 `X.depends_on = D` 前，查 `D` 的祖先/前驱链是否含 `X`（`WITH RECURSIVE … WHERE lvl < 5`，实测 3 层 0.2 ms） | 见 §5.5-③：`ancestors(004) = [004(0), 003(1), 001(2)]`，目标 001 出现在 lvl 2 → `creates_cycle = t` |
| 深度限制 | `depth ≤ 5`（DDL `CHECK`），配置 `task.max_depth=5` 提前报错 | 实测 `depth=6` 被 `ck_biz_task_depth` 拒绝（§5.5-④）。5 层足够表达「项目→里程碑→阶段→任务→子任务」 |
| 依赖数量 | 单任务前置 ≤ 20 | 超过 20 个前置说明粒度有问题；限制写进 DTO 校验（不进 DB） |

### 4.7 看板

#### 4.7.1 一次往返的查询形态

```sql
SELECT s.code, s.name, s.semantic, s.color, s.wip_limit,
       cnt.total,
       x.id, x.task_no, x.title, x.priority, x.due_at, x.progress,
       x.assignee_id, u.real_name
  FROM biz_task_status_def s
  CROSS JOIN LATERAL (
      SELECT t.id, t.task_no, t.title, t.priority, t.due_at, t.progress, t.assignee_id
        FROM biz_task t
       WHERE t.status_code = s.code
         AND t.deleted_at IS NULL AND t.archived_at IS NULL
         AND (TRUE {FILTER} OR t.id = ANY(?::text::uuid[]))
       ORDER BY t.priority, t.due_at NULLS LAST, t.id
       LIMIT 50
  ) x
  LEFT JOIN sys_user u ON u.id = x.assignee_id AND u.deleted_at IS NULL
  LEFT JOIN LATERAL (SELECT count(*) AS total FROM biz_task t2
                      WHERE t2.status_code = s.code AND t2.deleted_at IS NULL
                        AND t2.archived_at IS NULL AND (TRUE {FILTER} OR t2.id = ANY(?))) cnt ON TRUE
 WHERE s.status = 1
 ORDER BY s.sort_order, x.priority, x.due_at NULLS LAST;
```

- **每列 50 条**（`task.board_column_page_size`），超出显示「还有 N 条 →」点击进入该列的列表视图（复用列表接口）；
- **每次滚动加载 50 条**（`GET /task/board/column?status=DOING&cursor=<priority,due_at,id>`），用「最后一条的排序键」做 keyset 分页（不用 OFFSET，避免拖拽期间跳条）；
- **实测：5 列 × 50 条 = 14 ms**，列头计数额外 55 ms（§5.3）；计数可缓存 30 s（`task.board_column_page_size` 与列计数都写进 Redis 也行，但 55 ms 不值得引入缓存失效逻辑——先不缓存）。

> **闸①合规说明**：看板**不是**分页查询（每列固定 `LIMIT 50`，无 total 分页语义），因此走 `scope::filter_for(&principal, TASK)` 拿 `RowFilter` 再手工插进 LATERAL，而不是 `paginate`。这正是 `scope.rs:403-407` 允许的「单条 / 下拉 / 导出」路径；实施时不得因为「少写一次 paginate」而漏掉 `filter_for` 的返回值（CI 闸②会抓「取了不用」）。同理，「参与者 id 集合」那条查询也是非分页路径，只查 `biz_task_member` / `biz_task` 的 id，不涉及可见性注入（它本身就是可见性输入）。

#### 4.7.2 泳道

**做，但纯前端**：前端把 `GET /task/board` 的返回按 `assignee_id` / `priority` / `project_id` 分组渲染（受偏好 `task.swimlane` 控制）。后端不提供「按泳道再查一次」的接口——泳道只是同一个结果集的 CSS 分组，多一次查询没有收益。

#### 4.7.3 拖拽并发（D9）

```text
前端：PATCH /task/{id}/status  body { target_status, expect_version, force? }
后端：
  1. 读当前任务（含 status_semantic / version / parent_id / due_at）
  2. 校验语义迁移矩阵（§4.3.1）→ 非法 422 TASK_STATUS_TRANSITION_INVALID
  3. 校验依赖（§4.6）→ 阻塞 403 TASK_DEPENDENCY_BLOCKED（force 可越）
  4. 校验权限（负责人/协作人/分派人/主管）+ 验收权（→ DONE 时）
  5. UPDATE … WHERE id = ? AND version = ?  → 0 行 = 409 TASK_VERSION_CONFLICT，回传最新快照
  6. 同事务：changelog record_in（Action::Status）+ 父任务进度重算
  7. 提交后：dispatch 通知（幂等键含 version）
```

**真两连接实测**（§5.5-⑨）：会话 A `TODO→DOING`（持锁 3 s，version 1→2），会话 B 同时用 `version=1` 更新 → **阻塞至 A 提交，然后 `UPDATE 0`**，B 读到的 `status_code=DOING, version=2`。没有丢失更新、没有静默覆盖。

### 4.8 实时性：结论是**不做 SSE**（D10）

| 方案 | 判断 | 理由 |
|---|---|---|
| 复用 `notice` 的 SSE 通道推看板事件 | ❌ | 通道的事件契约是**单形态**：`EVENT_NAME = "notice"`（`sse_hub.rs:64`）+ 固定 `NoticeEvent` 结构（`sse_hub.rs:90-115`），`into_sse`（`:374`）把每条 payload 原样作为 `data` 发出。前端已按「通知」解析；塞任务事件要改前端消费路径 + 加事件分发，且两类事件的生命周期不同（通知有已读，任务事件没有） |
| 新建一条 SSE 端点 `/task/stream` | ❌（本批） | 等于新建一套推送基础设施（频道表、空频道回收、心跳、鉴权、订阅者统计），复制的全是 `sse_hub` 已有代码 → 违反「不重造」红线 |
| **乐观锁 409 + 30 s 增量轮询** | ✅ | 看板并发冲突本身是低频（同列拖拽），409 回传最新快照即可让用户看到「哦，他先拖了」；增量轮询用 `GET /task/board/delta?since=<ISO8601>`（走 `idx_biz_task_updated`），返回 `{task_id, status_code, version, updated_at}` 列表，前端只刷新变化的卡片 |

**若将来要做**（P1 触发条件：出现「多人同时盯一个看板」的真实投诉，或看板轮询 QPS 打满 DB）：正确做法是给 `notice::sse_hub` 加「事件名 + 任意 payload」的通用通道（`publish_named(user_id, event_name, payload)`），那是对批次 A 底座的改动，**需单独报批**。

### 4.9 通知触发点清单（复用 `notice::dispatch`）

| # | 触发点 | 触发条件 | 接收人 | `notice_type` / `level` | 幂等键 | 通道 |
|---|---|---|---|---|---|---|
| 1 | **分派** | 创建任务时 `assignee_id ≠ creator_id` | 负责人 | `TODO` / `INFO` | `task:{id}:assign:{assignee}:{version}` | 站内 |
| 2 | **转派** | 负责人变更 | 新负责人 + 原负责人 | `TODO` / `INFO` | `task:{id}:transfer:{assignee}:{version}` | 站内 |
| 3 | **拆解** | 生成 N 个子任务 | 各子任务负责人 + 父任务负责人 | `TODO` / `INFO` | `task:{child_id}:assign:{uid}:1` | 站内 |
| 4 | **评论 / @** | 评论新增且 `mention_ids` 非空 | 被 @ 的人（**排除作者本人**） | `TODO` / `INFO` | `task:{id}:mention:{comment_id}:{uid}` | 站内 |
| 5 | **状态变更** | `status_semantic` 变化 | 负责人（非操作人）+ 分派人 + 关注人 | `TODO` / `INFO` | `task:{id}:status:{new_status}:{version}` | 站内 |
| 6 | **提交验收** | `→ REVIEW` | 验收人（分派人 > 部门 leader） | `TODO` / `WARN` | `task:{id}:review:{version}` | 站内 + 邮件（若偏好开） |
| 7 | **验收通过 / 驳回** | `REVIEW → DONE` / `REVIEW → DOING` | 负责人 + 协作人 | `WORKFLOW` / `INFO`（驳回为 `WARN`） | `task:{id}:accept:{result}:{version}` | 站内 |
| 8 | **临期** | 每小时扫：未完成 且 `due_at ∈ (now, now+24h]` | 负责人 | `ALERT` / `WARN` | `task:{id}:due_soon:{due_date}` | 站内 |
| 9 | **逾期** | 每日 09:00 扫：未完成 且 `due_at < now` | 负责人 + 分派人 | `ALERT` / `URGENT` | `task:{id}:overdue:{due_date}` | 站内 + 邮件 |
| 10 | **每日汇总** | 每日 08:30 | 每个有未完成任务的用户 | `SYSTEM` / `INFO` | `task:daily:{user_id}:{yyyy-MM-dd}` | 站内 |
| 11 | **依赖解除** | 前置任务完成，后置任务因此解锁 | 后置任务负责人 | `TODO` / `INFO` | `task:{dep_id}:unblocked:{version}` | 站内 |

**四条统一规则**（避免通知变成噪音，这也是「提醒该不该做」的真正难点）：

1. **不通知操作人自己**（我拖的状态我自己知道）；
2. **合并窗口**：同一任务对同一接收人的通知在 `task.notify_batch_window_seconds`（默认 60 s）内只推最后一条——批量改状态时不做这个会刷屏（`dispatch` 的幂等键按 `version`，天然是「每版一条」，所以合并要在应用层做：用一个内存 LRU 记 `(task_id, uid) → last_sent_at`，窗口内跳过）；
3. **先提交后发送**：`dispatch` 自己开事务（`mod.rs:242-245`），无法与业务同事务 → 业务 `COMMIT` 之后再调 `dispatch`，失败只记 `tracing::error!` 并由「通知补偿」兜底（幂等键保证重试不重发）；
4. **接收人必须能看见该任务**：发通知前用 `can_view(task, uid)` 校验（尤其 @ 场景）。@ 一个不可见者 → **422 `TASK_MENTION_FORBIDDEN`**（否则通知标题里就泄露了任务标题）。

### 4.10 变更留痕接入点（复用 `changelog`）

新增一条 `ResourceSpec`（`changelog/diff.rs:157` 的 `REGISTRY` 追加）：

```rust
// 实施时加在 business/changelog/diff.rs（既有文件，属底座 —— 见 §8 的报批说明）
pub const BIZ_TASK: ResourceSpec = ResourceSpec {
    table: "biz_task",
    module: "business",
    fields: &[
        "title", "priority", "status_code", "status_semantic", "progress",
        "assignee_id", "assigner_id", "dept_id", "parent_id", "project_id",
        "start_at", "due_at", "estimated_hours", "completed_at", "archived_at",
    ],
    big: &["content_md"],           // 正文进 big（摘要化：len + sha256 + 200 字预览）
};
```

| 动作 | `Action`（已在 DDL CHECK 内） | `action_label` 示例 |
|---|---|---|
| 创建 | `CREATE` | 新建任务 |
| 编辑字段 | `UPDATE` | 修改任务 |
| 状态流转 | `STATUS` | 状态：进行中 → 待验收 |
| 分派/转派 | `ASSIGN` | 转派：张三 → 李四 |
| 归档 / 删除 | `UPDATE` / `DELETE` | 归档 / 删除任务 |

**不进白名单的列**（有意为之）：`child_count`/`done_child_count`/`actual_hours`/`board` 相关派生列——它们每次子任务变化都会动，进 diff 会把审计表变成访问日志（与批次 C 的 `receiver_count`/`read_count` 同一取向）。

### 4.11 四个定时任务（注册在 `task/mod.rs::register_builtin`，`:233`）

| 任务名 | cron | 做什么 | 幂等/限流 |
|---|---|---|---|
| `task_due_remind` | `0 5 * * * *`（每小时 5 分） | 扫 `due_at ∈ (now, now + task.due_remind_hours]` 且未完成 → 通知负责人 | 幂等键含 `due_date`（改截止时间会重新提醒一次，这是要的） |
| `task_overdue_scan` | `0 0 9 * * *`（每日 09:00） | 扫 `due_at < now` 且未完成 → 通知负责人 + 分派人 | 幂等键含 `due_date`；`task.overdue_scan_enabled=false` 时只统计不推送 |
| `task_daily_digest` | `0 30 8 * * *`（每日 08:30） | 给每人汇总「今日待办 N / 逾期 M / 待验收 K」 | 幂等键含日期 |
| `task_consistency_check` | `0 20 4 * * *`（每日 04:20） | 三条对账（§4.1-③） | 无（只读 + 修） |

**性能依据**（§5.3 实测，20 万任务）：临期扫描 **0.5 ms**、逾期统计 **13 ms**——两小时级任务可在 20 ms 内完成，不会与业务争锁。

### 4.12 附件：上传走底座，下载必须过鉴权

| 环节 | 做法 | 理由 |
|---|---|---|
| 上传 | 前端用 `FileUploader`（`components/FileUploader.vue:17` 的 `category` prop）→ `POST /business/storage/upload?category=task` → 得到 `file_id` → `POST /task/{id}/attachment {file_id}` 绑定 | 复用批次 A 的存储与策略（`V0105` 种了 `category='task'` 的 50 MB 白名单） |
| 下载 | `GET /task/{id}/attachment/{att_id}` → 校验「任务可见 → 附件属于该任务 → 文件存在」→ 服务端流式代理 | **不能**让前端直接用 `business:storage:download`：该权限已授予所有角色（批次 C 架构 D6 实测），任何人拿到 `file_id` 就能下载任意文件 |
| 删除 | 解绑（删 `biz_task_attachment` 行），**不删 `sys_file`** | 同一文件可能被多个任务引用（如公共模板），删文件要交给存储回收任务判断引用 |

### 4.13 批量操作（`POST /task/batch`）

| 项 | 决定 |
|---|---|
| 单次上限 | **200 条**（超过 400，提示分批） |
| 事务 | **一个事务**（全成功或全失败）——避免「部分成功」让用户不知道哪条没改 |
| 部分失败 | 不做「跳过失败继续」模式（两种模式会让前端无法给出确定反馈）；如需，走导入 通道 |
| 每条的校验 | 逐条跑与单条接口相同的权限/状态机校验；**任一不通过 → 整体回滚**，返回 `{failed_index, code, msg}` |
| changelog | 每条一条记录（不合并）——审计要能按任务反查 |

### 4.14 回收站与归档

| 概念 | 字段 | 语义 | 进不进统计 |
|---|---|---|---|
| 软删 | `deleted_at` | 删除（进回收站，30 天后由巡检硬删） | 否 |
| 归档 | `archived_at` | 完成后的任务从看板默认视图隐藏，但仍可查、仍在统计内 | **是** |
| 取消 | `status_semantic='CANCELED'` | 不做了 | 不计入完成率分母，计入总数 |

---

## 5. 性能实测（20 万任务 / 25 万协作人 / 10 万评论 / 20 万工时）

> 环境：PG 18.6 本地实例，库 `c_task_v2`（生产 `pg_dump --schema-only` 重建 + 本批 6 个迁移 + 20 万行种子）。所有数字是 `EXPLAIN (ANALYZE, BUFFERS)` 的 `Execution Time`，单次冷跑（未预热缓存），属于**保守值**。脚本见附录 B。

### 5.1 可见性三种写法的对比（同一语义）

| 写法 | 场景 | count | 数据页（LIMIT 20） | 计划特征 |
|---|---|---|---|---|
| **V1：`OR EXISTS(...)` × 3 + 两个列 OR**（最直觉的写法） | `SelfOnly` | **197 ms** | **173 ms** | `Seq Scan on biz_task`（20 万行全扫）→ OR 把索引废掉 |
| 同上 | `DeptAndChild`(11 部门) | **539 ms** | **563 ms** | 同上，更差（命中行更多） |
| **V2：UNION 分段**（每段各自索引） | `SelfOnly` | 48 ms | — | 5 个 Bitmap Index Scan + HashAggregate |
| 同上 | `DeptAndChild` | 70 ms | 121 ms（含不公平的种子：该用户被塞进 200 个项目） | 每段走各自索引，但 UNION 后的 id 集合要再 join 回主表排序 |
| **V3：范围 ∪ 参与者 id 数组**（本设计采用） | 基线（只按部门，无并集） | **16 ms** | — | `Bitmap Heap Scan on idx_biz_task_dept` |
| 同上 | `DeptAndChild` ∪ 425 个参与 id | **39.6 ms** | **36.9 ms** | **`BitmapOr`**: `idx_biz_task_dept` + `biz_task_pkey` ✓ |
| 同上 | 同上 ∪ **66,733** 个参与 id | **>15,000 ms（超时打断）** | — | 位图探测次数爆炸 → 必须设上限（D7） |

**结论**：采用 V3 + 2000 上限 + 独立 tab 兜底。这不仅是最快的，也是**唯一不用改 `scope.rs`** 的（V2 无法表达成单条 `base_sql`，会绕过「分页唯一入口」闸①）。

### 5.2 参与者 id 集合的规模画像

| 画像 | 参与 id 数 | 数组体积 | 说明 |
|---|---|---|---|
| 普通员工（种子里 0 个项目、1 个协作） | **425** | 15.7 KB | 主路径 |
| 部门主管（11 个部门 + 若干项目） | 2000–6000 | 74–220 KB | 接近/超过上限 → 降级提示 |
| 极端（种子把该用户塞进 200 个项目，每个 333 任务） | **66,733** | 2.4 MB | 实测超时，正是要挡掉的画像 |

> 种子的项目成员分布是「每个项目 20 人、用户 1–20 全在」→ 用户 1 属于全部 200 个项目。这是**故意构造的最坏画像**（用来验证上限的必要性），不是现实比例。现实里 2000 上限够用。

### 5.3 其余热路径

| 查询 | 规模 | 耗时 | 计划 |
|---|---|---|---|
| 看板：5 列 × 每列 50 条（一次往返） | 20 万任务 / 11 部门 | **14.1 ms** | 每列一次 `idx_biz_task_status` 索引扫 + Sort + Limit |
| 看板列头计数（5 个 `count(*)`） | 同上 | 55.0 ms | 5 次 `Bitmap Heap Scan`（可接受，不缓存） |
| 部门统计（完成率 / 逾期率 / 工时，90 天） | 20 万任务 | **102.8 ms** | `Hash Join sys_dept` + `Seq Scan biz_task`（报表页可接受，前端加 60 s 缓存） |
| 个人工作量（30 天工时按人汇总） | 20 万工时行 | 76.8 ms | `Bitmap Heap Scan idx_biz_task_worklog_date` + HashAggregate |
| 临期扫描（24 h 内到期） | 20 万任务 | **0.5 ms** | `idx_biz_task_due`（部分索引，只有未完成的） |
| 逾期统计 | 20 万任务 | **13.3 ms** | 同上 |

### 5.4 索引有效性验证（为什么这几个索引必须存在）

| 索引 | 服务的查询 | 去掉它的后果 |
|---|---|---|
| `idx_biz_task_dept (dept_id, status_semantic) WHERE deleted_at IS NULL` | 所有部门范围列表/看板 | 退化为 Seq Scan（200k → 16 ms 起，随数据线性增长） |
| `idx_biz_task_due … WHERE completed_at IS NULL AND due_at IS NOT NULL` | 临期/逾期扫描 | 三个定时任务从 0.5 ms 变 200 ms 级，且每小时一次 |
| `idx_biz_task_member_user (user_id, task_id)` | 参与者 id 集合 | 「我参与的」tab 从 12 ms 变全表扫 |
| `idx_biz_task_updated (updated_at DESC)` | 看板增量轮询 `?since=` | 每 30 s 每用户一次全表扫 → 灾难 |
| `uk_biz_task_no` | 编号唯一 + 按编号搜 | 编号可重复（并发/导入时） |

### 5.5 机制验证的实测输出（逐条摘录）

```
① 状态迁移（乐观锁 version）：UPDATE 1
   → id=…004  status_code=DONE  status_semantic=DONE  progress=100  done_at=t  version=2
①b 用过期 version=1 再迁一次：        UPDATE 0        ← 乐观锁生效（第二个用户拖拽被拒）
② 语义迁移矩阵：25 行全部输出（9 条 FALSE 逐条有理由，见 §4.3.1）
③ 循环检测：ancestors(004) = [004(lvl0), 003(lvl1), 001(lvl2), creates_cycle=t]
   → 把 001 挂到 004 下会被拦
④ 深度上限：depth=6 → ERROR: violates check constraint "ck_biz_task_depth"
⑤ 进度加权：auto_progress=64（A:100%×4h + B:40%×6h）/ child_cnt=2 / done_cnt=1
⑤b 回写：progress=64, progress_mode=A, child_count=2, done_child_count=1
⑥ 依赖阻塞：dep_semantic=DOING → blocked=t（前置未完成，禁止进入 DOING）
⑦ 工时回写：actual_hours=4.00（2.5+1.5）/ estimated=6.00 / ratio=0.67
⑧ 负面约束 20 条：20/20 全部被数据库拒绝（清单见 §5.6）
⑨ 真并发（两连接）：A 持锁 3 s → B 阻塞后 UPDATE 0，读到 status=DOING/version=2（无丢失更新）
```

### 5.6 数据库层的 20 条负面约束（实测全部拒绝）

| # | 脏数据 | 被谁拒 |
|---|---|---|
| 1 | `priority = 5` | `ck_biz_task_priority` |
| 2 | `progress = 101` | `ck_biz_task_progress` |
| 3 | `status_semantic='DONE'` 但 `completed_at IS NULL` | `ck_biz_task_completed` |
| 4 | 非 DONE 却给了 `completed_at` | `ck_biz_task_completed` |
| 5 | `status_semantic='PENDING'` | `ck_biz_task_semantic` |
| 6 | `status_code` 不存在 | `fk_biz_task_status` |
| 7 | `parent_id = id` | `ck_biz_task_self_parent` |
| 8 | `done_child_count > child_count` | `ck_biz_task_child_counts` |
| 9 | `task_no` 重复 | `uk_biz_task_no` |
| 10 | 第二个 `is_initial` 列 | `uk_biz_task_status_initial`（部分唯一索引） |
| 11 | `semantic='DONE'` 但 `is_final=false` | `ck_biz_task_status_final` |
| 12 | 删除仍被任务引用的看板列 | `fk_biz_task_status` 的 `ON DELETE RESTRICT` |
| 13 | 依赖指向自己 | `ck_biz_task_dep_self` |
| 14 | 重复依赖 | `uk_biz_task_dep` |
| 15 | 单条工时 25 h | `ck_biz_task_worklog_hours` |
| 16 | 单条工时 0 h | `ck_biz_task_worklog_hours` |
| 17 | 空评论（全空白） | `ck_biz_task_comment_content` |
| 18 | 同一任务重复绑定同一文件 | `uk_biz_task_attachment` |
| 19 | 同一任务重复加同一成员 | `uk_biz_task_member` |
| 20 | 模板 `subtasks` 不是数组 | `ck_biz_task_template_subtasks` |

> 这 20 条是「数据库层最后一道闸」。**状态机合法性不在其中**（DB 不拦 `TODO → DONE`），那是 `model::set_status` 的职责——这一点必须写进实施评审清单，否则误以为 DB 兜住了。

---
## 6. API 契约

> 统一前缀：`/api`（由 `[router].prefix` 注入，宏里只写 `/business/task/...`）。
> 统一响应：`Res<T>` / `Res::page(list, total, page, size)`（与既有模块一致）。
> 所有接口都声明 `#[nik::permission(path=…, method=…, permission=…)]`（例见 `announcement/handler.rs:93`），由 `App::scan()` 自动收集。

### 6.1 端点清单（**78 个**，按域分组；各组标题括号内是该组**表格行数**，实际端点数见本节末的计数表）

#### ① 任务查询与看板（10）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/task/page` | `business:task:list` | 列表分页：`keyword/status_code/priority/assignee_id/project_id/tag_id/due_start/due_end/only_overdue/include_archived/page/page_size` |
| GET | `/business/task/board` | `business:task:list` | **看板一次往返**：N 列 × 每列 50 条 + 列头 total（§4.7.1） |
| GET | `/business/task/board/column` | `business:task:list` | 单列滚动加载（keyset：`status` + `cursor`） |
| GET | `/business/task/board/delta` | `business:task:list` | 增量轮询：`?since=<ISO8601>` → `[{task_id,status_code,version,updated_at}]`（走 `idx_biz_task_updated`） |
| GET | `/business/task/mine` | `business:task:list` | 我的任务：`tab=assigned\|created\|participated\|watched\|from_superior\|mention`、`range=today\|week\|month\|all` |
| GET | `/business/task/{id}` | `business:task:list` | 详情（含成员、附件、依赖、子任务摘要、可见性标记） |
| GET | `/business/task/{id}/children` | `business:task:list` | 子任务列表（扁平，带 `progress/status`） |
| GET | `/business/task/{id}/timeline` | `business:task:list` | 动态时间线（复用 `sys_change_log` 按 `resource='biz_task'` + `resource_id` 反查） |
| GET | `/business/task/{id}/watching` | `business:task:list` | 当前是否关注（前端按钮态） |
| GET | `/business/task/enums` | `business:task:list` | 枚举（优先级/语义/来源/成员角色/可见性）——后端单一事实源（`shared/enums.rs` 范式） |

#### ② 任务写操作（12）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| POST | `/business/task` | `business:task:create` | 新建（`parent_id` 可空；带 `parent_id` 时校验深度/循环，父任务自动 `progress_mode='A'`） |
| PUT | `/business/task/{id}` | `business:task:update` | 编辑字段（`expect_version` 必填） |
| PATCH | `/business/task/{id}/status` | `business:task:status` | **状态流转**（`target_status` / `expect_version` / `force` / `reason`）；→ DONE 时校验验收权 |
| PUT | `/business/task/{id}/progress` | `business:task:update` | 改进度（`progress_mode='A'` → 409 `TASK_PROGRESS_READONLY`） |
| PUT | `/business/task/{id}/assign` | `business:task:assign` | 分派/转派/移交（`assignee_id` / `mode=ASSIGN\|TRANSFER\|HANDOVER` / `keep_as_collaborator`） |
| POST | `/business/task/{id}/decompose` | `business:task:create` | 拆解：批量建子任务（`subtasks[]`，原子） |
| POST | `/business/task/{id}/copy` | `business:task:create` | 复制（可选带子任务/成员/标签） |
| POST | `/business/task/batch` | 按动作逐个校验 | 批量（`action=ASSIGN\|STATUS\|PRIORITY\|TAG\|ARCHIVE`，≤200 条，单事务） |
| DELETE | `/business/task/{id}` | `business:task:delete` | 软删（级联子任务软删？——**不级联**，有子任务时 422 提示先处理） |
| PUT | `/business/task/{id}/restore` | `business:task:restore` | 从回收站恢复 |
| DELETE | `/business/task/{id}/purge` | `business:task:restore` | 彻底删除（物理，仅回收站内） |
| PUT | `/business/task/{id}/archive`、`PUT /business/task/{id}/unarchive` | `business:task:update` | 归档 / 取消归档 |

#### ③ 成员与关注（5）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/task/{id}/member` | `business:task:list` | 成员列表（含角色） |
| POST | `/business/task/{id}/member` | `business:task:update` | 加协作人/关注人（**目标必须能看见该任务**，否则 422 防泄露） |
| DELETE | `/business/task/{id}/member/{user_id}` | `business:task:update` | 移除成员 |
| POST | `/business/task/{id}/watch`、`DELETE /business/task/{id}/watch` | `business:task:list` | 关注 / 取关（自己操作自己，不需要 update 权限） |
| GET | `/business/task/member/candidates` | `business:task:update` | 可选成员（按 scope 裁剪的启用用户，用于选择器） |

#### ④ 评论与 @（6）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/task/{id}/comment` | `business:task:list` | 评论分页（含回复） |
| POST | `/business/task/{id}/comment` | `business:task:comment` | 发表（`content_md` + `mention_ids[]` + `parent_id`；@ 不可见者 → 422） |
| PUT | `/business/task/comment/{comment_id}` | `business:task:comment` | 编辑自己的评论（他人 → 403） |
| DELETE | `/business/task/comment/{comment_id}` | `business:task:comment` / `business:task:comment:delete` | 作者删自己的 / 管理码删他人 |
| GET | `/business/task/mention/me` | `business:task:list` | @我的（GIN 反查 + 可见性过滤） |
| GET | `/business/task/{id}/comment/count` | `business:task:list` | 未读评论数（按 `last_read_at` 比对，前端角标） |

#### ⑤ 附件（4）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/task/{id}/attachment` | `business:task:attachment` | 附件列表 |
| POST | `/business/task/{id}/attachment` | `business:task:attachment` | 绑定已上传的 `file_id`（校验 `category='task'`） |
| DELETE | `/business/task/attachment/{att_id}` | `business:task:attachment` | 解绑（不删 `sys_file`） |
| GET | `/business/task/{id}/attachment/{att_id}/download` | `business:task:attachment` | **鉴权代理下载**（任务可见 → 附件属该任务 → 文件存在，然后流式返回） |

#### ⑥ 依赖（4）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/task/{id}/dependency` | `business:task:list` | 前置 + 后置（双向，标 `blocked`） |
| POST | `/business/task/{id}/dependency` | `business:task:update` | 加前置（校验循环/自依赖/数量上限 20） |
| DELETE | `/business/task/dependency/{dep_id}` | `business:task:update` | 删除依赖 |
| GET | `/business/task/{id}/dependency/check` | `business:task:list` | 能否进入 DOING（返回阻塞列表，前端置灰按钮） |

#### ⑦ 工时（4）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/task/{id}/worklog` | `business:task:worklog` / `business:task:worklog:read` | 明细（看他人需 `worklog:read`） |
| POST | `/business/task/{id}/worklog` | `business:task:worklog` | 登记（`work_date ≤ today`，`0 < hours ≤ 24`，只能记自己） |
| PUT | `/business/task/worklog/{wid}` | `business:task:worklog` | 改自己的（他人 → 403） |
| DELETE | `/business/task/worklog/{wid}` | `business:task:worklog` | 删自己的 |

#### ⑧ 看板列 / 标签（8）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/task/status-def` | `business:task:list` | 列定义（所有人可读，渲染看板用） |
| POST | `/business/task/status-def` | `business:task:status:manage` | 新建列（`code/name/semantic/color/wip_limit`） |
| PUT | `/business/task/status-def/{code}` | `business:task:status:manage` | 改名/配色/WIP（**不可改 semantic**：改语义会让历史状态的统计口径翻转） |
| DELETE | `/business/task/status-def/{code}` | `business:task:status:manage` | 删除（在用 → 409 `TASK_STATUS_DEF_IN_USE`） |
| PUT | `/business/task/status-def/sort` | `business:task:status:manage` | 批量排序 |
| GET | `/business/task/tag` · POST · PUT · DELETE | `list` / `tag:manage` | 标签字典（4 个端点合并计数） |

#### ⑨ 项目（7）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/task/project/page` | `business:task:project:list` | 项目列表（按 scope ∪ 项目成员过滤） |
| GET | `/business/task/project/{id}` | `business:task:project:list` | 详情（含进度汇总） |
| POST | `/business/task/project` | `business:task:project:manage` | 新建 |
| PUT | `/business/task/project/{id}` | `business:task:project:manage` | 编辑/关闭 |
| DELETE | `/business/task/project/{id}` | `business:task:project:manage` | 软删（有任务 → 422） |
| GET/POST/DELETE | `/business/task/project/{id}/member` | `project:list` / `project:manage` | 成员维护（3 个端点合并） |
| GET | `/business/task/project/{id}/board` | `business:task:project:list` | 项目看板（复用看板 SQL，范围换成 `project_id`） |

#### ⑩ 模板 / 统计 / 导入导出（9）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/task/template/page` | `business:task:template:manage` | 模板列表 |
| POST | `/business/task/template` | `business:task:template:manage` | 新建模板 |
| PUT | `/business/task/template/{id}` | `business:task:template:manage` | 编辑 |
| DELETE | `/business/task/template/{id}` | `business:task:template:manage` | 删除 |
| POST | `/business/task/template/{id}/apply` | `business:task:create` | **一键生成**（父任务 + N 子任务，返回 root_id） |
| POST | `/business/task/template/from-task/{task_id}` | `business:task:template:manage` | 存为模板（含子任务快照） |
| GET | `/business/task/stat/personal` | `business:task:stat` | 个人工作量（时段内：任务数/完成数/工时/按时率） |
| GET | `/business/task/stat/dept` | `business:task:stat` | 部门完成率/逾期率/工时（实测 103 ms） |
| GET | `/business/task/stat/trend` | `business:task:stat` | 完成趋势（按日/周/月）+ `GET /stat/workload` 负载（人×进行中任务数） |
| POST | `/business/task/export` | `business:task:export` | 导出（**委托 `business/impexp`**：`biz_type='task'`，异步任务） |
| GET | `/business/task/import/template`、`POST /business/task/import` | `business:task:import` | 导入模板下载 / 提交（复用 impexp，逐行校验） |

> **端点计数（逐组实测口径，含表内以「·」合并的同类端点）**
>
> | 组 | 端点数 | 明细 |
> |---|---|---|
> | ① 任务查询与看板 | 10 | 表内 10 行 = 10 个 |
> | ② 任务写操作 | 13 | 表内 12 行，其中「archive/unarchive」为 2 个 |
> | ③ 成员与关注 | 6 | 5 行，其中「watch/unwatch」为 2 个 |
> | ④ 评论与 @ | 6 | 6 行 |
> | ⑤ 附件 | 4 | 4 行 |
> | ⑥ 依赖 | 4 | 4 行 |
> | ⑦ 工时 | 4 | 4 行 |
> | ⑧ 看板列 / 标签 | 9 | 列 5 个 + 标签 4 个 |
> | ⑨ 项目 | 9 | 6 行，其中成员维护 3 个 |
> | ⑩ 模板 / 统计 / 导入导出 | 13 | 模板 6 + 统计 4（含 workload）+ 导出 1 + 导入 2 |
> | **合计** | **78** | 实施时以 `handler.rs` 里 `#[nik::get/post/put/delete/patch]` 宏的条数为准，本表用于评审对齐 |

### 6.2 权限码（21 个，`V0104` 已种）

| 权限码 | 段 | 授权策略 | 用途 |
|---|---|---|---|
| `business:task:list` | 全员 | 所有未软删角色 | 列表/看板/详情/我的/日历/枚举 |
| `business:task:create` | 全员 | 同上 | 新建/拆解/复制/从模板生成 |
| `business:task:update` | 全员 | 同上 | 编辑字段/成员/依赖/归档 |
| `business:task:delete` | 全员 | 同上 | 软删（只能删自己可见且有权的） |
| `business:task:assign` | 全员 | 同上 | 分派/转派（目标受 scope 约束，§4.2.2） |
| `business:task:status` | 全员 | 同上 | 状态推进 |
| `business:task:comment` | 全员 | 同上 | 评论/编辑自己的 |
| `business:task:attachment` | 全员 | 同上 | 附件增删与下载 |
| `business:task:worklog` | 全员 | 同上 | 登记自己的工时 |
| `business:task:accept` | 全员 | 同上 | 验收（受「必须是分派人/上级」约束） |
| `business:task:project:list` | 全员 | 同上 | 项目列表/项目看板 |
| `business:task:project:manage` | 管理 | 仅 `SUPER_ADMIN` | 项目增删改 + 成员维护 |
| `business:task:status:manage` | 管理 | 仅 `SUPER_ADMIN` | 看板列增删改排序 |
| `business:task:template:manage` | 管理 | 仅 `SUPER_ADMIN` | 模板 CRUD |
| `business:task:tag:manage` | 管理 | 仅 `SUPER_ADMIN` | 标签字典 CRUD |
| `business:task:stat` | 管理 | 仅 `SUPER_ADMIN`（可按需追加给部门主管角色） | 团队统计 |
| `business:task:export` | 管理 | 仅 `SUPER_ADMIN` | 导出 |
| `business:task:import` | 管理 | 仅 `SUPER_ADMIN` | 导入 |
| `business:task:comment:delete` | 管理 | 仅 `SUPER_ADMIN` | 删他人评论 |
| `business:task:restore` | 管理 | 仅 `SUPER_ADMIN` | 回收站恢复 / 彻底删除 |
| `business:task:worklog:read` | 管理 | 仅 `SUPER_ADMIN` | 看他人工时明细 |

> **注意**：数据权限（能看哪些任务）**不由权限码控制**，而由 `scope` 控制。这是批次 B 的既定分工：权限码管「能不能做这个动作」，数据范围管「能对哪些数据做」。任务模块严格照此，不给「看全部任务」这种权限码（那会变成绕过数据范围的后门）。

### 6.3 错误码段 63xxx（新增，`shared/error_code.rs`）

| 码 | 常量 | HTTP | 触发 |
|---|---|---|---|
| 63001 | `TASK_NOT_FOUND` | 404 | 任务不存在 / 已物理删 |
| 63002 | `TASK_NOT_VISIBLE` | 403 | 不可见（范围 ∪ 参与者都不命中） |
| 63003 | `TASK_STATUS_TRANSITION_INVALID` | 422 | 语义迁移矩阵为 FALSE（返回 `from/to` 与允许的目标） |
| 63004 | `TASK_VERSION_CONFLICT` | 409 | 乐观锁冲突（响应体带最新快照） |
| 63005 | `TASK_DEPENDENCY_BLOCKED` | 403 | 前置未完成（响应体带阻塞列表） |
| 63006 | `TASK_DEPENDENCY_CYCLE` | 422 | 依赖/父子成环 |
| 63007 | `TASK_DEPTH_EXCEEDED` | 422 | 深度 > `task.max_depth` |
| 63008 | `TASK_PROGRESS_READONLY` | 409 | 有子任务时手动改进度 |
| 63009 | `TASK_ASSIGN_TARGET_INVALID` | 422 | 目标用户停用/删除/是自己 |
| 63010 | `TASK_ASSIGN_SCOPE_DENIED` | 403 | 目标不在我的数据范围内 |
| 63011 | `TASK_ACCEPT_FORBIDDEN` | 403 | 无验收权（非分派人/上级） |
| 63012 | `TASK_MENTION_FORBIDDEN` | 422 | @ 了不可见者 |
| 63013 | `TASK_MEMBER_DUPLICATE` | 409 | 重复加成员 |
| 63014 | `TASK_MEMBER_NOT_FOUND` | 404 | 成员不存在 |
| 63015 | `TASK_COMMENT_NOT_FOUND` | 404 | 评论不存在/已删 |
| 63016 | `TASK_COMMENT_FORBIDDEN` | 403 | 编辑他人评论 |
| 63017 | `TASK_ATTACHMENT_NOT_FOUND` | 404 | 附件不属于该任务或文件不存在 |
| 63018 | `TASK_STATUS_DEF_IN_USE` | 409 | 删除在用列 |
| 63019 | `TASK_STATUS_DEF_DUPLICATE` | 409 | 列编码/名称重复 |
| 63020 | `TASK_CONTENT_INVALID` | 400 | 标题空/超长、正文超长、评论超 `comment_max_chars` |
| 63021 | `TASK_WORKLOG_INVALID` | 400 | 工时 ≤0 / >24 / 未来日期 |
| 63022 | `TASK_TAG_DUPLICATE` | 409 | 标签名重复 |
| 63023 | `TASK_PROJECT_NOT_FOUND` | 404 | 项目不存在 |
| 63024 | `TASK_PROJECT_CODE_CONFLICT` | 409 | 项目编号重复 |
| 63025 | `TASK_TEMPLATE_NOT_FOUND` | 404 | 模板不存在 |
| 63026 | `TASK_TEMPLATE_CODE_CONFLICT` | 409 | 模板编码重复 |
| 63027 | `TASK_BATCH_LIMIT` | 400 | 批量 > 200 条 |
| 63028 | `TASK_HAS_CHILDREN` | 422 | 有子任务时不能删/不能转派父任务 |
| 63029 | `TASK_ARCHIVED` | 409 | 归档任务不可编辑（需先取消归档） |
| 63030 | `TASK_PARENT_INVALID` | 422 | 父任务不可作为父（已取消/自己/跨项目） |

> 码段 63xxx 在 `error_code.rs` 里**未被占用**（现最大 62024），文件头分段表（`:30-40`）需同步加一行「`6300x` 工作任务管理 · 批次 C+」。

### 6.4 一个端点契约样例（看板）

```jsonc
// GET /api/business/task/board?project_id=&assignee_id=&swimlane=none&show_done=false
{
  "code": 0,
  "data": {
    "columns": [
      {
        "code": "DOING", "name": "进行中", "semantic": "DOING",
        "color": "primary", "wip_limit": null, "total": 137,
        "items": [
          {
            "id": "a1…", "task_no": "TK20261029-000123", "title": "接入对象存储预签名",
            "priority": 0, "progress": 40, "due_at": "2026-10-31T18:00:00+08:00",
            "assignee": { "id": "e1…", "name": "张三", "avatar": null },
            "child": { "done": 2, "total": 5 },
            "overdue": false, "has_comment_unread": true
          }
        ]
      }
    ],
    "participant_truncated": false,   // D7 降级标记
    "server_time": "2026-10-29T12:40:00+08:00",
    "vocab_version": 3                // 列定义变更后前端可据此重取
  }
}
```

---
## 7. 前端设计

### 7.1 技术底座与既有约定（不引入新依赖）

| 项 | 现状（实测） | 本模块怎么做 |
|---|---|---|
| 框架 | Vue 3.5 + TS 5.8 + Vite 6（`frontend/package.json`） | 照用 |
| UI 库 | `tdesign-vue-next ~1.19.1` + `tdesign-icons-vue-next` | 只用 TDesign 组件，不自造基础控件 |
| 状态 | Pinia 3 | 新增 `stores/task.ts`（看板列、筛选、偏好） |
| 图表 | `echarts ~5.4.3` + `components/EChart.vue` | 统计页复用 |
| 日期 | `dayjs ~1.11.13` | 日历视图、相对时间 |
| 拖拽 | **没有任何拖拽库**（`package.json` 里无 sortable/vuedraggable） | **不引入**：用 Pointer Events + `transform` 自实现（§7.3），约 120 行 |
| 路由 | `router/index.ts` 静态路由 + `meta.permission`（如 `permission: 'system:user:list'`） | 增加 6 条静态路由（菜单由后端 `sys_menu` 下发，二者对齐） |
| API 层 | `api/request.ts` 封装 + 每模块一个 `*-api.ts`（如 `api/business/announcement-api.ts`） | 新增 `api/business/task-api.ts` |
| 枚举 | `composables/useModuleEnums.ts`（按模块取一次并缓存） | `useModuleEnums('task')` 取优先级/语义/来源 |
| 配色 | `style/palettes.ts` **22 套色板 × 明暗**，运行时推导 CSS 变量注入 `<html>`；变量名复用 `--td-*` / `--app-*`；`pnpm check:palettes` 是门禁 | **只写变量，不写色值**。看板列头/卡片/进度条全部走 `--td-bg-color-container` / `--td-brand-color` / `--td-text-color-*` / `--app-*` |
| 附件 | `components/FileUploader.vue`（`category` prop → 上传接口 query） | `<FileUploader category="task" @uploaded="bind" />` |
| Markdown | `components/MarkdownViewer.vue` | 任务正文/评论渲染 |

### 7.2 视图清单（6 个页面 + 1 个详情）

| # | 视图 | 路由 / 组件 | 核心交互 | 做/不做 |
|---|---|---|---|---|
| 1 | **任务板（Kanban）** | `/task/board` → `business/task/board/index` | 列＝状态（`status_def` 顺序），卡片拖拽改状态，列头显示「还有 N 条」，泳道切换（负责人/优先级/项目），筛选条（项目/负责人/标签/关键词/含已完成） | **做（P0，最重要）** |
| 2 | 我的任务 | `/task/mine` → `business/task/mine/index` | 5 个 tab（我负责/我创建/我参与/我关注/上级分派）+ 时间范围（今日/本周/本月/全部），列表 + 快速完成 | **做** |
| 3 | 任务列表 | `/task/list` → `business/task/list/index` | 表格（可配列）、批量选择 + 批量操作、保存筛选视图、导出 | **做** |
| 4 | 任务日历 | `/task/calendar` → `business/task/calendar/index` | 月/周视图，按 `due_at` 落格，拖拽改截止日期（复用同一 DnD 内核） | **做** |
| 5 | 项目管理 | `/task/project` → `business/task/project/index` | 项目卡片（进度条 + 任务数）、项目看板（复用看板组件，范围换 `project_id`）、成员维护 | **做** |
| 6 | 任务统计 | `/task/stat` → `business/task/stat/index` | 个人工作量 / 部门完成率与逾期率 / 完成趋势 / 负载分布（4 个 ECharts） | **做** |
| 7 | 任务设置 | `/task/setting` → `business/task/setting/index` | 看板列（增删改排序、WIP、配色）、标签字典、模板管理 | **做（管理码）** |
| 8 | 任务详情 | 抽屉（宽 960px）+ 独立页 `/task/detail/:id` | 见 §7.4 | **做** |
| — | **甘特图** | — | 时间轴 + 依赖箭头 + 拖拽改期 + 关键路径 | **不做（P1）**：依赖只做 FS 软约束、无基线、无关键路径算法，画出来是「假甘特」，用户会按它做承诺——比不画更危险 |

### 7.3 拖拽实现（零依赖，约 120 行）

**为什么不用 HTML5 drag-and-drop API**：iOS Safari 对 `dragstart`/`drop` 支持极差（长按会触发系统文本选择/拖拽图片），而我们要做移动端适配（§2.2-#13）。**统一用 Pointer Events**，桌面与移动同一套代码。

```ts
// composables/useKanbanDrag.ts（实施时新建）
// 1) 卡片：pointerdown → 记录 taskId + 起点（setPointerCapture）
// 2) 移动超过 6px 才进入拖拽（阈值：避免点击被误判为拖拽）
// 3) 拖拽中：卡片设 position:fixed + transform: translate(dx,dy)（脱离文档流，不引起重排）
// 4) pointermove：命中检测 → 判断落在哪个列（列元素矩形 + 中心线），高亮目标列
// 5) pointerup：若目标列 ≠ 原列 → 乐观更新本地状态 → PATCH /status → 失败则回滚
// 6) 触屏：容器 touch-action: none（否则滚动会抢事件）；卡片上 touch-action: pan-y 保留纵向滚动
```

**乐观更新与 409 的处理（这是体验的关键）**：

| 步骤 | 行为 |
|---|---|
| 1 | `pointerup` 立即把卡片移到目标列（不等后端），列头计数本地 ±1 |
| 2 | 发 `PATCH /task/{id}/status { target_status, expect_version }` |
| 3 | 成功 → 用响应体覆盖卡片（拿新 `version`/`progress`/`completed_at`） |
| 4 | **409 `TASK_VERSION_CONFLICT`** → 卡片回滚到原列 + Toast「这条任务刚被别人改过（当前：待验收），已为你刷新」+ 用响应体里的最新快照更新卡片（**不整页刷新**） |
| 5 | **422 状态非法** → 回滚 + Toast 说明原因（「不能从未开始直接到已完成，请先开始处理」） |
| 6 | **403 依赖阻塞** → 回滚 + Toast「前置任务『XXX』未完成」+ 提供「仍要开始」按钮（`force=true`） |

**无障碍（a11y）**：卡片 `tabindex="0"`，`Alt+←/→` 把卡片移到相邻列（键盘用户的等价操作）。这不只是合规——它是**移动端拖拽失败时的兜底路径**。

**列内排序**：不做（§0.3-6）。列内固定 `priority → due_at → id`，拖拽只改状态。若用户确实需要手工排序，P1 时用 `board_seq BIGINT` + 步长 1024 的 lexorank 方案（DDL 加一列可空字段即可，无需改表结构）。

### 7.4 详情页布局

```text
┌──────────────────────────────────────────────────────────────┬─────────────────┐
│ ← 返回   TK20261029-000123   [进行中▾]        ⋯ 更多          │  属性栏（320px）│
├──────────────────────────────────────────────────────────────┤                 │
│ ┌ 标题（可内联编辑，回车保存）───────────────────────────────┐ │ 状态   [进行中▾]│
│ └───────────────────────────────────────────────────────────┘ │ 优先级 [P1 ▾]   │
│                                                              │ 负责人 [张三 ▾] │
│ 描述（Markdown 渲染 ⇄ 编辑双态，复用 MarkdownViewer）          │ 协作人 [+ 2 人] │
│                                                              │ 关注   [👁 3]    │
│ ── 子任务 (2/5) ──────────────────── [+ 添加] ──              │ 分派人 李四     │
│  ☑ 子任务 A  100%  张三   10-28                              │ ─────────────── │
│  ☐ 子任务 B   40%  张三   10-31  ⚠ 逾期                      │ 开始   10-26    │
│                                                              │ 截止   10-31 ⚠  │
│ ── 依赖 ────────────────────────────────────                │ 进度   ▓▓▓▓░░ 64%│
│  前置：子任务 B（进行中）⚠ 会阻塞开始                          │ 预计   10h      │
│  后置：—                                                     │ 实际   6.5h     │
│                                                              │ ─────────────── │
│ ── 附件 (2) ──────────────────── [+ 上传] ──                 │ 项目   云枢 C+  │
│  📄 需求说明.pdf  1.2 MB                                     │ 父任务 父任务名 │
│  📄 接口文档.md   12 KB                                      │ 标签   [线上][客户A]│
│                                                              │ ─────────────── │
│ ── 动态 / 评论 ─────────────────────────────                 │ 工时   [登记]   │
│  [评论框：Markdown + @ 提及选择器]                            │  ─────────────── │
│  张三 10-29 10:12  @李四 这个接口要先做 …                     │ ⏱ 耗时 3 天     │
│  李四 10-29 10:15  收到，今天下午给                           │                 │
├──────────────────────────────────────────────────────────────┴─────────────────┤
│ 右侧「动态」tab：状态变更/负责人变更/截止时间变更（来自 sys_change_log 时间线）  │
└────────────────────────────────────────────────────────────────────────────────┘
```

| 设计点 | 决定 | 理由 |
|---|---|---|
| 左内容右属性 | ✅ | 内容（描述/子任务/附件/评论）是阅读主线；属性是「一次性设置」的，放右侧不打断阅读流 |
| 评论位置 | **底部**（不是右侧） | 任务详情是「先看任务再讨论」，评论在底部符合阅读顺序；也避免右侧双栏挤压 |
| 抽屉 vs 独立页 | **两者都有**：列表/看板点开用抽屉（保留上下文），分享/通知链接用独立页 `/task/detail/:id` | 抽屉里点链接无法分享；独立页刷新会丢列表上下文 |
| 内联编辑 | 标题/属性栏字段直接改（失焦保存），正文用「渲染 ⇄ 编辑」双态 | 任务字段的修改是高频小操作，弹窗表单太重 |
| 子任务 | 折叠列表 + 快速勾选完成 | 「勾完成」是最高频操作，要 1 次点击 |
| 逾期/阻塞提示 | 截止时间旁 ⚠ + 依赖区红字 | 让「为什么不能开始」在详情页一眼可见 |
| 移动端 | 属性栏折叠成「摘要条（状态/负责人/截止）」，点击展开为**底部抽屉**；评论在内容下方 | 小屏放不下三栏 |

### 7.5 与 22 套配色的兼容（实测约定）

| 规则 | 内容 | 检查方式 |
|---|---|---|
| R1 | **只使用 `--td-*` / `--app-*` CSS 变量**，禁止硬编码 hex/rgb | `pnpm check:palettes`（已有门禁，`package.json` 的 `check:palettes` 脚本） |
| R2 | 看板列头用 `--td-bg-color-container` + 1px `--td-component-stroke`；列语义色只用于**列头上的 3px 顶边与徽标**，不用于卡片底色 | 卡片用容器色，保证 22 套配色下卡片文字对比度一致 |
| R3 | 优先级用 `t-tag` 的 `theme`（`default/primary/warning/danger`），不自定义色值 | `shared/enums.rs` 下发的 `color` 字段就是这套主题名（`enums.rs:44-60` 的 `color` 模块） |
| R4 | 进度条用 `t-progress`（主题色自动跟随）；逾期标记用 `--td-error-color` | 深色模式下 TDesign 自动适配 |
| R5 | 泳道分隔线用 `--td-component-border`，不画彩色泳道头 | 22 套色板的主色差异大，彩色泳道头在部分色板下会脏 |

**验证方法**（实施验收项）：在「雾蓝流金 / 玫瑰金 / 石墨橙 / 月白」4 套色板 × 明暗双模式下截图对比，看板/详情/统计三页无「不可读文字」「同色叠影」。

### 7.6 移动端交互（响应式断点 768px / 1200px）

| 断点 | 看板 | 列表 | 详情 |
|---|---|---|---|
| ≥1200（桌面） | 全部列横向铺开，泳道可选 | 表格 10 列 | 左右两栏 |
| 768–1199（平板） | 列宽 260px，横向滚动 | 表格 6 列（其余折叠到「更多」） | 右栏 280px 或折叠 |
| <768（手机） | **单列模式**：顶部列切换 Tab（待开始/进行中/待验收/…），一次一列；长按卡片进入拖拽（Pointer Events）→ 拖到顶部 Tab 上切换列 | 卡片列表（不是表格） | 单栏；属性栏折叠为底部抽屉；评论在底部 |

**手机上的拖拽取舍**：小屏「拖到顶部 Tab」比「拖到右侧列」更符合直觉，且避免横向滚动与拖拽手势冲突（横向滚动用 `touch-action: pan-x`，拖拽用长按 300 ms 后 `preventDefault`）。

---

## 8. 实施拆分（T0–T9 后端 + F1–F4 前端，共 **43 人日**）

> 每一步都是**可独立交付、可独立验收**的。验收项全部是「能跑出来的东西」（SQL/接口/截图），不是「代码写完了」。

### 8.1 后端（T0–T9，28 人日）

| 步 | 交付物 | 验收（可执行） | 人日 |
|---|---|---|---|
| **T0** 底座与迁移 | `V0100`–`V0105` 六个迁移；`business/mod.rs` 加 `pub mod task;`；`error_code.rs` 加 63001–63030；`business/task/{mod,dto,model,handler}.rs` 骨架 + `GET /task/enums` | ① 在生产同构库跑 `sqlx migrate run`（或项目既有迁移方式）到 V0105，`select count(*) from information_schema.tables` = **55**；② `V0104` 的 `DO $$` 自检通过；③ 反顺序 down 全部回滚成功再 up 一次（幂等）；④ `GET /api/business/task/enums` 返回 4 组枚举 | 2 |
| **T1** 任务 CRUD + 变更留痕 | `POST/PUT/GET/DELETE /task`、`GET /task/page`；`changelog::diff::BIZ_TASK` 登记 + `record_in` 调用点 | ① 建/改/删各一条，`sys_change_log` 里 `resource='biz_task'` 有 3 条且 `diff.fields` 只含白名单字段；② **回滚测试**：在 `UPDATE` 后人为 `ROLLBACK`，`sys_change_log` **0 行**（复用批次 C 的 `test_hooks` 思路）；③ `page` 走 `scope::paginate`（单测断言 SQL 含 `{FILTER}` 注入且参数顺序正确） | 4 |
| **T2** 可见性并集 + 我的任务 | 参与者 id 集合查询（含 2000 上限与降级）；`GET /task/mine` 6 个 tab；`GET /task/page` 换成并集形态 | ① 造 3 个用户：同部门同事的任务、跨部门协作任务、无关任务；断言前两者可见、第三者不可见；② 参与者 id 人为灌到 2001 个 → 响应 `participant_truncated=true` 且结果只含范围内任务；③ `EXPLAIN` 断言出现 `BitmapOr`（回归用，防有人把数组改回 `EXISTS`） | 3 |
| **T3** 分派 / 转派 / 拆解 + 层级 | `PUT /task/{id}/assign`、`POST /task/{id}/decompose`、`POST /task/{id}/copy`；上级判定与 scope 校验；深度/循环校验 | ① 主管派给下属 ✅、普通员工派给同事 ❌403、跨部门 ❌403、超管 ✅；② 转派后原负责人出现在 `biz_task_member`；③ 拆 3 个子任务后父任务 `child_count=3, progress_mode='A'`；④ 把父任务挂到孙任务下 → 422 `TASK_DEPENDENCY_CYCLE`；⑤ depth=6 → 422 `TASK_DEPTH_EXCEEDED` | 3 |
| **T4** 状态机 + 进度汇总 | `PATCH /task/{id}/status`、`PUT /task/{id}/progress`；25 组合矩阵 + 验收人判定；父任务汇总与联动 | ① 25 种组合逐条断言（9 条 FALSE 全部 422，16 条 TRUE 全部成功）；② 改进度 `A` 模式 → 409；③ 子任务 A(100%,4h)+B(40%,6h) → 父 `progress=64`（与 §5.5-⑤ 同数）；④ 子任务全 DONE → 父转 `REVIEW` 而非 `DONE` | 3 |
| **T5** 成员 / 评论 / @ | 5 个成员端点 + 6 个评论端点 + `GET /task/mention/me` | ① @ 不可见者 → 422；② @ 可见者 → 收到站内信且 `mention_ids` GIN 反查命中；③ 编辑他人评论 403；④ 删他人评论需 `comment:delete` | 2 |
| **T6** 附件 / 依赖 / 标签 / 工时 | 4+4+4(合并 8)+4 端点 | ① 附件下载先校验可见性（用无权限用户直接打 storage 下载 vs 打 task 下载，后者 403）；② 前置未完成 → 403 且 `force=true` 成功并留痕；③ 工时回写 `actual_hours`；④ 工时 25h → 400 | 3 |
| **T7** 看板 + 并发 | `GET /task/board`、`/board/column`、`/board/delta`；乐观锁 | ① 5 列 × 50 条 P95 < 100 ms（20 万行数据下实测 14 ms）；② 两会话并发拖拽：后者 409 且响应体含最新 `version`；③ `delta?since=` 只返回变化的卡片 | 3 |
| **T8** 项目 / 模板 / 批量 / 回收站 | 项目 7 端点、模板 6 端点、`POST /task/batch`、restore/purge/archive | ① 模板一键生成 → 1 父 + N 子且子任务带 `estimated_hours/due_at` 偏移；② 批量 201 条 → 400；批量中第 5 条非法 → 整体回滚（库里 0 条变更）；③ 删除 → 回收站可见 → 恢复 → 再删 → purge | 3 |
| **T9** 统计 / 导入导出 / 定时任务 | 4 个统计端点、impexp 的 `ExportDefinition`/`ImportDefinition`、4 个定时任务注册 | ① 部门统计 P95 < 300 ms（20 万行实测 103 ms）；② 导出 xlsx 打开后中文不乱码、状态列是中文（`DictTranslation`）；③ 导入带 2 行错误 → 错误明细落到 `sys_import_task_error`；④ 临期任务日志里出现且幂等键不重复（同一天跑两次只发一条） | 3 |

### 8.2 前端（F1–F4，15 人日）

| 步 | 交付物 | 验收 | 人日 |
|---|---|---|---|
| **F1** 骨架与列表 | `api/business/task-api.ts`、`stores/task.ts`、6 条路由、列表页 + 批量操作 + 保存筛选 | 列表能按 6 种筛选组合查询、批量 5 条改负责人成功、筛选存进偏好后刷新仍在 | 3 |
| **F2** 看板 + 拖拽内核 | `useKanbanDrag.ts` + 看板页（泳道/筛选/滚动加载） | ① 拖拽改状态成功；② 构造 409 时卡片回滚并提示；③ 键盘 Alt+←/→ 可移动；④ 手机上长按拖拽可换列 | 4 |
| **F3** 详情页 + 协作 | 详情抽屉/独立页、子任务、附件、评论（@ 选择器）、动态时间线、工时 | @ 选择器只列出任务可见者；评论 Markdown 渲染无 XSS；时间线显示「状态：进行中 → 待验收」 | 4 |
| **F4** 日历 / 项目 / 统计 / 设置 | 日历页、项目页、统计 4 图、设置页（列/标签/模板） | 日历按截止日期落格、拖拽改期生效；4 张图有数据；新增一列后看板立即出现（`vocab_version` 生效） | 4 |

### 8.3 联调与验收（3 人日，含在上表之外的缓冲）

| 项 | 内容 |
|---|---|
| 全链路冒烟 | 「建任务 → 派给下属 → 下属拆 3 子任务 → 逐个完成 → 父转待验收 → 上级验收通过」全流程走通，且 changelog/通知/统计三处数据自洽 |
| 数据权限对抗测试 | 5 种 scope 角色 × 6 类任务（自己/同事/下属/上级/跨部门协作/无关）的 30 格可见性矩阵全对 |
| 性能压测 | 20 万任务下：看板 P95 < 100 ms、列表 P95 < 150 ms、统计 P95 < 300 ms |
| 配色回归 | 4 套色板 × 明暗共 8 种组合下三页截图评审 |

### 8.4 关键依赖与顺序

```text
T0 ──> T1 ──> T2 ──> T3 ──┬──> T4 ──> T7 ──┐
                          ├──> T5          ├──> 联调
                          ├──> T6          │
                          └──> T8          │
                                T9 ────────┘
F1（可与 T1 并行，用 mock）──> F2（依赖 T7）──> F3（依赖 T5/T6）──> F4（依赖 T9）
```

---

## 9. 风险与应对

| # | 风险 | 影响 | 应对 |
|---|---|---|---|
| **R1** | **数据权限与「协作人可见」冲突**：协作人来自别的部门，`scope` 看不到 → 出现「他在我的看板上看不到我派给他的活」的投诉 | 高（跨部门协作是核心场景） | §4.4 的并集 SQL + 2000 上限 + `GET /task/mine?tab=participated` 兜底；**验收项**：跨部门协作任务在协作人的「我参与的」与「我的任务」里必须可见（T2 验收②） |
| **R2** | 参与者 id 数组膨胀导致慢查询（实测 66,733 id 时 >15 s） | 中 | 硬上限 2000 + 降级标记 + `EXPLAIN` 回归断言（防有人把数组改回 `EXISTS`，或把上限调大） |
| **R3** | 父子任务递归：深层树导致汇总 UPDATE 放大、或成环 | 中 | `depth ≤ 5`（DDL CHECK）+ 祖先链检测（≤5 层）+ 汇总只更新「直接父 + 祖先链」（≤5 行）；成环在 API 层 422 |
| **R4** | 看板拖拽并发：两人同时拖同一张卡 | 中 | 乐观锁 `version` + 409 回传最新快照 + 前端局部刷新（不整页刷）；已真两连接实测 |
| **R5** | 统计查询在任务量增长后变慢（当前 20 万 103 ms） | 中 | ① 部门统计加 60 s 缓存；② 数据量 > 100 万时上「每日统计快照表」（`biz_task_stat_daily`，P1）；③ 索引 `idx_biz_task_dept` 已在 |
| **R6** | `status_semantic` 冗余列漂移（有人绕过 `set_status` 直接改状态） | 中 | CI grep 门禁（`biz_task` 的 UPDATE 只能在 `business/task/model.rs`）+ 每日巡检任务修 + 报警 |
| **R7** | 验收权判定错误（企业组织里「分派人」可能已离职/转岗） | 中 | 验收人回落到 `assignee.dept.leader_id`；两者都为空 → 允许负责人自完成（`DOING → DONE`），并在详情页显示「无验收人」提示 |
| **R8** | 通知风暴（批量操作/导入触发上百条） | 中 | `notify_batch_window_seconds=60` 合并 + 批量操作**只发汇总通知**（不逐条发）+ 导入不发通知（导入结果走 impexp 的完成通知） |
| **R9** | `@` 泄露：@ 一个看不到该任务的人 → 通知里带任务标题 | 中（安全） | 服务端校验（422），**且**通知文案只带「任务编号」，标题只对可见者展示 |
| **R10** | 附件下载越权（复用 `business:storage:download` 会变公开） | 高（安全） | 专用代理端点（可见性 → 归属 → 存在三道检查），与公告同款；**不**在响应里暴露 `sys_file.id` 之外的存储路径 |
| **R11** | 迁移与既有数据冲突（`sys_dept` 加 leader 索引、菜单 UUID 撞段） | 低 | 索引用 `IF NOT EXISTS`；菜单段用 `…08xx`（`…07xx` 已被内容管理占用，实测 V0087/V0085/V0088/V0089）；`V0104` 的 `DO $$` 自检会炸在部署期而非上线后 |
| **R12** | 「做了一半」的语义：看板列可自定义但统计口径固定，用户可能建 20 个列 | 低 | 列数上限 12（DTO 校验）+ WIP 上限提示 + 统计只按 5 语义聚合（列多不影响口径） |
| **R13** | 移动端拖拽在 iOS 上失效 | 低 | Pointer Events + `touch-action` 精确设置 + 键盘/按钮兜底（卡片菜单里「移到…」） |
| **R14** | 43 → 55 张表的运维可观测性（备份/监控/SQL 审核） | 低 | 12 张表全部带 `COMMENT ON TABLE`（本设计的 DDL 已写），运维可按 comment 生成表清单；`biz_task` 是唯一大表（预估 3 年 50 万行），单独监控 |

### 9.1 不做清单（含 P1 触发条件）

| 不做 | 触发条件（什么情况下再做） | P1 时的落地方式 |
|---|---|---|
| 周期任务 | 有 ≥3 个部门真实提周期需求 | 建 `biz_task_recur` + 每日生成任务，`instance_key` 唯一约束保幂等 |
| 甘特图 / 里程碑 / 关键路径 | 出现「要拿任务系统做项目排期」的正式诉求 | 加 `is_milestone` + `baseline` 字段，前端独立视图 |
| 自定义字段（EAV） | 出现「必须按自定义字段做数值聚合」 | 建 `biz_task_field_def` + `biz_task_field_val`，索引策略单独设计 |
| 燃尽图 | 需要「迭代」概念（Sprint） | 建 `biz_task_stat_daily` 每日快照 |
| SSE 实时看板 | 看板同时在线人数 > 50 或轮询 QPS 打满 | 给 `notice::sse_hub` 加通用事件通道（底座改动，报批） |
| agent 作为执行者 | 需要「自动分派 + 自动执行」的场景落地 | 引入 `sys_principal` 抽象 + 程序主体令牌 + `executor_type` |
| 列内自由排序 | 用户明确要求手工排序 | `board_seq BIGINT` + lexorank（步长 1024，冲突重排） |
| 逾期升级提醒 | 逾期率成为管理指标 | 加 `task.escalate_days` / `escalate_to` 配置 + 扫描任务扩展 |

---

## 10. 未决项（需老大 / 产品拍板）

| # | 未决项 | 选项 | 建议 |
|---|---|---|---|
| U1 | **数据权限范围与「部门看板」的默认值**：部门主管进任务板，默认看到「本部门及以下」还是「本部门」？ | (a) 本部门 (b) 本部门及以下 | 建议 (a)：看板是「谁在干什么」的现场视图，「及以下」会让大部门的看板一眼几十列上百卡；需要汇总时用统计页 |
| U2 | **谁能建项目**：只有超管（`project:manage`）还是部门主管也能建？ | (a) 仅超管 (b) 主管可建（自建项目自动成为 OWNER） | 建议先 (a)（V0104 现状），跑一个月看是否有阻塞；放权只需改一条角色-菜单授权，不用发版 |
| U3 | **待验收是否强制**：所有任务完成都要验收，还是允许「无需验收」的任务直通完成？ | (a) 强制（`DOING → DONE` 禁止） (b) 可配置 | 建议 (b) + 默认「直通允许」（现状矩阵 TRUE），因为个人待办（如「读一份文档」）验收毫无意义 |
| U4 | **工时是否必须填**：完成任务时是否强制记工时？ | (a) 不强制 (b) 完成时弹窗要求填 | 建议 (a)：强制会让用户乱填，数据质量更差；用「统计页可见谁没记」的软约束 |
| U5 | **归档策略**：完成的任务多久自动归档？ | (a) 从不自动 (b) 完成 30 天后自动归档 | 建议 (b)，用定时任务做（`task.auto_archive_days`），否则看板的「已完成」列会无限增长 |
| U6 | **是否需要「任务可见范围」在任务级覆盖**（有的任务只给特定人看） | (a) 不做（任务可见性 = 范围 ∪ 参与者） (b) 加 `visibility` 字段 | 建议 (a)：加任务级可见性会与「协作人可见」规则打架（协作人看不到自己参与的任务？），且需要一个全新的权限判定层 |

---

## 附录 A：迁移清单与实跑结果

| 文件 | 内容 | 实跑结果（`c_task_v1` 干净库，生产 schema 重建后执行） |
|---|---|---|
| `V0100__create_task_core.up.sql` | `biz_task_status_def` + `biz_task` + `seq_biz_task_no` + **14 个二级索引**（状态表 3 + 任务表 11） | ✅ 通过 |
| `V0101__create_task_member_comment.up.sql` | `biz_task_member` + `biz_task_comment`（GIN 索引） | ✅ 通过 |
| `V0102__create_task_attach_dep_tag.up.sql` | 附件 + 依赖 + 标签 + 标签关联 | ✅ 通过 |
| `V0103__create_task_worklog_template_project.up.sql` | 工时 + 项目 + 项目成员 + 模板 | ✅ 通过 |
| `V0104__seed_task_status_permission.up.sql` | 5 个看板列 + 菜单 29 节点（1 目录 / 7 页面 / 21 按钮）+ 两段授权（全员 17 / 超管 12）+ `DO $$` 自检 | ✅ 通过（列 5 / 配置 8 / 策略 1 / 偏好 4） |
| `V0105__seed_task_config_preference.up.sql` | 8 个 `sys_config` + 1 个存储策略 + 4 个偏好字典 + 自检 | ✅ 通过 |

**全量校验（实测输出）**

```text
# 表数量
select count(*) from information_schema.tables
 where table_schema='public' and table_type='BASE TABLE';        → 55   (43 + 12)
# 新表索引数量
select count(*) from pg_indexes
 where schemaname='public' and tablename like 'biz_task%';       → 49
# 种子
biz_task_status_def = 5 · sys_menu(…08xx) = 29 · sys_config(task.*) = 8
sys_storage_policy(category='task') = 1 · sys_preference_def(task.*) = 4
```

**可重放性（up → down → up 全周期实测，库 `c_task_v3`）**

```text
生产 schema 重建                     → 43 表
跑 V0100–V0105（6 个 up）             → 55 表·0 错误
按 V0105→V0100 反序跑 6 个 down       → 43 表·0 残留（biz_task* 对象数 = 0）
再跑一遍 6 个 up（重放）              → 55 表·种子仍是 5/29/8/4/1（ON CONFLICT 幂等）
```

> 一个实施期注意点（本次实跑踩到）：`sys_config` 的 `INSERT` 列有 13 个（含 `sort_order` 与 `ref_value`），漏一个就报 `INSERT has more target columns than expressions`——迁移写完必须真跑一遍，不能只靠肉眼。

## 附录 B：POC 脚本与复跑方法

| 脚本 | 验证什么 | 关键结论 |
|---|---|---|
| `verify/poc1_visibility.sql` | 三种可见性写法之一：单条 `OR EXISTS` | SelfOnly 197 ms / DeptAndChild 539 ms → 淘汰 |
| `verify/poc2_variants.sql` | UNION 分段写法 + 看板 + 统计首测 | UNION 48–70 ms（可用但需绕开 `paginate`）→ 淘汰 |
| `verify/poc3_idarray.sql` | 参与 id 数组 + BitmapOr（含 `\gset` 取 id 集合） | 40 ms，`BitmapOr(idx_biz_task_dept, biz_task_pkey)` → **采用**（重用户用例会跑很久，慎跑） |
| `verify/poc4_prepared.sql` | 用 `PREPARE` 避免 `EXPLAIN` 回显巨型数组，测 425 / 66,733 两种画像 | 39.6 ms / 超时 → 定 2000 上限 |
| `verify/poc5_board_stat.sql` | 超时上限证据（15 s 打断）+ 看板 + 统计 + 扫描 | 看板 14 ms、统计 103 ms、临期扫描 0.5 ms |
| `verify/poc6_mechanism.sql` | 状态迁移（乐观锁）/ 25 组合矩阵 / 循环检测 / 深度 CHECK / 进度汇总 / 依赖阻塞 / 工时回写 | 逐条结论见 §5.5 |
| `verify/poc7_negative.sql` | 20 条负面约束 | 20/20 被拒（§5.6） |
| `verify/seed_200k.sql` | 20 万任务 + 25 万协作人 + 10 万评论 + 20 万工时 | 种子 ~40 s |
| `verify/prod_schema.sql` | 生产 `pg_dump --schema-only`（43 表） | 用作验证库基线 |

**复跑命令**（本机 PG 18.6）

```bash
# 1) 建验证库并灌生产 schema
sudo -u postgres dropdb --if-exists c_task_v2 ; sudo -u postgres createdb c_task_v2
sudo -u postgres psql -d c_task_v2 < verify/prod_schema.sql
# 2) 跑本批 6 个迁移
for f in migrations/V010*.up.sql; do sudo -u postgres psql -d c_task_v2 < "$f"; done
# 3) 灌 20 万行数据（约 40 s）
sudo -u postgres psql -d c_task_v2 < verify/seed_200k.sql
# 4) 跑 POC（注意 poc3 的重用户用例会跑很久）
sudo -u postgres psql -d c_task_v2 < verify/poc5_board_stat.sql
sudo -u postgres psql -d c_task_v2 < verify/poc6_mechanism.sql
sudo -u postgres psql -d c_task_v2 < verify/poc7_negative.sql
```

> 说明：本机 `psql` 走 `sudo -u postgres`（peer 认证）；生产凭据在 `/etc/cloudpivot/cloudpivot.env`（600），脚本不内嵌任何口令。`TRUNCATE`/无 `WHERE` 的 `DELETE` 在无人值守会话里被安全策略拦截，所以复跑一律「重建库」而不是「清表」。

## 附录 C：事实索引（结论 → 证据）

| 结论 | 证据 |
|---|---|
| `sys_user` 无上级字段 | `migrations/system/V0001__create_sys_user.up.sql`（19 列全文）；`grep -rn "manager\|leader_id\|parent_id" migrations/system/` 仅命中 `sys_menu.parent_id` 与 `sys_dept` |
| `sys_dept.leader_id` / `parent_id` / `dept_path` 存在 | `V0006__create_sys_dept.up.sql:6-8` |
| `sys_dept` 只有 3 个索引（无 leader 索引） | `V0006:38-40`（`uk_sys_dept_code` / `idx_sys_dept_parent_id` / `idx_sys_dept_path`） |
| 五范围 + 分页唯一入口 | `business/shared/scope.rs:70-81` / `:618` / `:651` / `:59` / `:747-761` / `:826-834` |
| 片段只锚定主体表（不能 join 部门） | `scope.rs:144-161`（`ScopeTarget` 字段注释）+ `:152-155` |
| 驱动不支持数组绑定（故用文本字面量 + 双 cast） | `scope.rs:32-43`（含 B2 探针实测输出） |
| `scope::paginate` 的 count 剥 ORDER BY | `scope.rs:794-802` |
| 变更日志签名与事务约束 | `business/changelog/mod.rs:117-122` / `:284-289` / `:242-245`（模块文档） |
| 变更日志资源登记 + 未登记 62012 | `changelog/diff.rs:54-63` / `:157`；`changelog/mod.rs:191-200` |
| 大字段阈值 2 KiB | `changelog/diff.rs:44-47` |
| 写入侧脱敏不看操作人 | `changelog/mask.rs:159-182` |
| `dispatch` 签名与自开事务 | `business/notice/mod.rs:246` / `:242-245` |
| 扇出目标 `Dept` 含子树（递归 CTE） | `notice/fanout.rs:36-45` / `:150-165` |
| SSE 单形态事件（EVENT_NAME="notice"） | `notice/sse_hub.rs:64` / `:90-115` / `:374` |
| 存储上传/预签名/下载端点与权限码 | `business/storage/handler.rs:719` / `:836` / `:1118` |
| 导入导出注册接口 | `business/impexp/registry.rs:39/59/80/172`；`impexp/definition.rs:762/813` |
| 调度器注册点 | `task/mod.rs:233`（`register_builtin`）/ `:304`（`register_cron`）/ `:523` |
| 错误码现最大 62024 | `shared/error_code.rs:417` |
| 枚举下发范式 | `shared/enums.rs:69/93/113/172` |
| Markdown 渲染/净化 | `business/shared/content.rs:125/207/284` |
| 权限宏与路由声明范式 | `business/announcement/handler.rs:93`（`#[nik::permission(...)]` + `#[nik::get(...)]`） |
| 菜单/权限种子范式（页面+按钮+三段授权+自检） | `V0087`（公告读）/ `V0085`（管理码只授超管）/ `V0089`（目录归拢 + 清缓存） |
| 偏好体系（KV + 字典） | `V0090__create_sys_user_preference.up.sql` / `V0091__seed_preference_def.up.sql` |
| 全库零触发器 | 生产 `pg_dump --schema-only` 后 `grep -c "CREATE TRIGGER"` = **0** |
| 生产 43 表 / 迁移到 V0091 | `information_schema` 实测 43；`ls migrations/**/V*.up.sql \| sort -V \| tail` = V0091 |
| 既有 21 个 FK 的形态（子表 CASCADE） | 生产 dump 的 `ADD CONSTRAINT … FOREIGN KEY` 21 条 |
| 前端的 22 套色板与变量约定 | `frontend/src/style/palettes.ts:1-20`；`package.json` 的 `check:palettes` |
| 上传组件 `category` prop | `frontend/src/components/FileUploader.vue:17-24` |
| 前端无拖拽库 | `frontend/package.json` 的 dependencies 全文（无 sortable/draggable） |
| 可见性三种写法性能 | `verify/poc1.log` / `poc2.log` / `poc3.log` / `poc4.log` |
| 看板/统计/扫描性能 | `verify/poc5.log` |
| 机制与负面约束 | `verify/poc6.log` / `poc7.log` |
| 迁移首跑与自检 | `verify/` 下的 `V010*.up.sql` 执行记录（附录 A） |
