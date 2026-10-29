# 批次 D · D1「定义侧数据与校验」验证证据

- 卡面：`t_a92e9d5e`【批次D·D1】定义侧：V0115(5表)+环检测(应用层DFS)+12条发布校验+11接口
- 依据：`docs/批次D-工作流审批-技术架构.md` §0.1（D1–D7、D20）、§3、§3.7、§6.1、§8、§10.1.1、§10.3
- 交付提交：`19e511e`（HEAD == origin/main == bare main == `19e511e7c66ea495478debe0b0e6f2744fdf8b2f`，主仓工作区 0 改动）
- 验收环境：144（ubuntu-svr）/ 库 `cloudpivot_d1` / 端口 8028（独立 target 编译，未污染生产构建）
- 生产：144 systemd `cloudpivot.service`（8023 / 库 `cloudpivot`），release 二进制 sha256 `a78abe6f…5af20`

---

## 0. 交付清单（只改卡面允许的路径）

| 路径 | 内容 |
|------|------|
| `backend/migrations/business/V0120__flow_def_tables.{up,down}.sql` | 定义侧 5 张表 + 索引 + 约束 + 外键 + 结构自检 |
| `backend/migrations/business/V0121__flow_def_permissions.{up,down}.sql` | 5 个权限节点 + 1 个页面节点 + 自检 |
| `backend/migrations/seed/S__workflow_permissions.sql` | SUPER_ADMIN 授权（种子，排在 `S__role_data` 之后） |
| `backend/src/business/flow/{mod,graph,dto,model,handler}.rs` | 图算法 / DTO / 数据访问 / 12 个接口 |
| `backend/src/business/mod.rs` | 挂载 `pub mod flow;` |
| `backend/src/shared/error_code.rs` | 新增 `65xxx` 段 22 个码（架构 §10.3 整体契约） |
| `scripts/check.sh` | 闸② 登记 5 张 `biz_flow_*` 表 + 豁免口径说明 |

未改：`business/task/`、`business/aiagent/`、`shared/scope.rs`（卡面「不改」段）。

---

## 1. 三处与文档/卡面的偏差（必须先看，都是有意的）

### 1.1 迁移编号：V0115/V0118 → **V0120/V0121**

文档 §6 排的是 V0115（定义侧）/ V0116（运行侧）/ V0117（索引）/ V0118（权限种子）/ V0119（定时任务），
但 **V0115 已被 T7c「项目分组」占用**（`V0115__project_grouping.{up,down}.sql` 已在 main 上）。
卡面写的 `V0115(5表)` + `V0118 权限种子` 已不可执行。本卡整体后移到 **V0120（表）/ V0121（权限节点）**，
把文档排给 D2–D6 的 V0116–V0119 原样留给它们（编号不冲突是并行批次唯一的硬约束）。

### 1.2 权限码：按**卡面**的 5 个码，不按文档 §10.2 的 10 个码

卡面「5. V0118 权限种子」明确要求 `business:flow:read/create/update/delete/publish`；
文档 §10.2 给的是 `business:flow:def:read/write/publish` + 实例侧 7 个。本卡按卡面落地，理由：

- D1 只交付定义侧，实例侧（`instance:read` / `approve` / `withdraw` / `admin` / `stats:read`）在 D2/D3；
- 5 个码与 `business:project:*` / `business:task:*` 的既有命名段一致，前端角色树不用为流程记两套前缀。

码 → 接口映射写在 `V0121` 文件头与 `handler.rs` 的模块文档表里。D2/D3 落地实例侧时**追加**码，不改这 5 个。

### 1.3 三处「往更严/更实用方向」的增量

| 增量 | 为什么 |
|------|--------|
| `biz_flow_def_version.is_current` + 部分唯一索引 `uk_biz_flow_def_version_current` | 卡面「版本号自增、`is_current` 切换」要求；让「每个 def 至多一个当前版本」成为库级事实 |
| `ck_biz_flow_node_ratio`（RATIO 必须有阈值） | 架构 V10 的库侧兜底；应用层先给中文 400，库层再挡脏数据（实测：只靠库拦会让用户拿到 500） |
| 第 12 个接口 `POST /def/{id}/draft`（另存为新草稿） | 卡面「范围·3」要求「改流程 = 复制当前版本成 draft」；没有它，「编辑已发布版本」只能靠前端猜版本号（违背架构 §3.6.3） |

页面节点 `…0780` 以 `is_visible = FALSE` 登记：前端页面 D6 才建，**指向不存在路由的可见菜单是坏链接**；
D6 建好 `business/flow/def/index` 后用一条 UPDATE 放出来（V0121 文件尾留了这条 SQL）。

---

## 2. 迁移：空库 up → down → up（144 / 库 `cloudpivot_d1`）

### 2.1 空库启动即跑通（V0120 + V0121 + 种子）

```
① 空库启动后（迁移器跑 up + 种子已跑）
tables=5
schema_rows=120/versioned,121/versioned
flow_indexes=17
flow_menus=6
flow_grants_read=1        ← 全部 5 个角色中，SUPER_ADMIN 之外还有 1 个角色（验收库只有 1 个角色）
flow_grants_write=4       ← SUPER_ADMIN 独占 create/update/delete/publish
```

启动日志（应用首启即自检通过，失败会 `exit(1)` 卡住启动）：

```
NOTICE: V0120 自检通过：5 张表 + 4 条关键索引 + node 约束 + 4 条外键
NOTICE: V0121 自检通过：1 个页面节点 + 5 个权限按钮（business:flow:read/create/update/delete/publish）
```

### 2.2 外键（库里的真实定义）

```
biz_flow_def_version · biz_flow_def_version_def_id_fkey      · FOREIGN KEY (def_id)      REFERENCES biz_flow_def(id)         ON DELETE CASCADE
biz_flow_node        · biz_flow_node_version_id_fkey         · FOREIGN KEY (version_id)  REFERENCES biz_flow_def_version(id) ON DELETE CASCADE
biz_flow_node_assignee · biz_flow_node_assignee_node_id_fkey · FOREIGN KEY (node_id)     REFERENCES biz_flow_node(id)        ON DELETE CASCADE
biz_flow_edge        · biz_flow_edge_version_id_fkey         · FOREIGN KEY (version_id)  REFERENCES biz_flow_def_version(id) ON DELETE CASCADE
biz_flow_edge        · biz_flow_edge_from_node_id_fkey       · FOREIGN KEY (from_node_id) REFERENCES biz_flow_node(id)       ON DELETE CASCADE
biz_flow_edge        · biz_flow_edge_to_node_id_fkey         · FOREIGN KEY (to_node_id)  REFERENCES biz_flow_node(id)        ON DELETE CASCADE
```

> 卡面只要求 4 条（version→def / node→version / edge→version / assignee→node）；边两端各一条是**多出来的**，
> 因为 `from_node_id` / `to_node_id` 是真实引用，不建外键就等于允许「连线指向已删节点」。

### 2.3 down 零残留 → 再 up 可重放

```
---- 先撤 V0121（权限节点 + 授权）----
NOTICE:  V0121 回滚自检通过：6 个菜单节点与全部授权已清零
after V0121 down → menus=0
after V0121 down → grants=0
---- 再撤 V0120（5 张表）----
NOTICE:  V0120 回滚自检通过：5 张表 / 索引 / 约束全清零
after V0120 down → tables=0
after V0120 down → indexes=0
after V0120 down → constraints=0
---- 再 up（V0120 → V0121 → 种子）----
NOTICE:  V0120 自检通过：5 张表 + 4 条关键索引 + node 约束 + 4 条外键
NOTICE:  V0121 自检通过：1 个页面节点 + 5 个权限按钮（business:flow:read/create/update/delete/publish）
after re-up → tables=5 / indexes=17 / menus=6 / grants(read)=1 / grants(write)=4
```

两个 down 脚本都带 `DO $$ … RAISE EXCEPTION … $$` 自检：表 / 索引 / 约束 / 菜单 / 授权**任一残留即回滚失败**。

---

## 3. 每条 CHECK / 唯一索引 / 外键实测生效（35 条全 PASS）

`bash /root/d1-run/migrate_check.sh` 汇总：**PASS=35 FAIL=0**。逐条摘录（节选错误原文）：

| # | 约束 | 违反示例 → PG 的回答 |
|---|------|----------------------|
| 1 | `uk_biz_flow_def_code`（软删过滤） | 同编码再插 → `duplicate key value violates unique constraint "uk_biz_flow_def_code"`；**软删后同编码可复用**（PASS） |
| 2 | `uk_biz_flow_def_default` | 同 `biz_type` 第二条默认 → `duplicate key value violates unique constraint "uk_biz_flow_def_default"` |
| 3 | `ck_biz_flow_def_code` | `D1_Bad` → `violates check constraint "ck_biz_flow_def_code"`；`d1-bad` 同样被拒 |
| 4 | `ck_biz_flow_def_published_has_version` | `status='PUBLISHED'` 且无版本 → `violates check constraint "ck_biz_flow_def_published_has_version"` |
| 5 | `biz_flow_def_status_check` | `status='ARCHIVED'` → `violates check constraint "biz_flow_def_status_check"` |
| 6 | `uk_biz_flow_def_version` | 同 `(def_id, version_no)` → `duplicate key … "uk_biz_flow_def_version"` |
| 7 | `ck_biz_flow_def_version_published` | `PUBLISHED` 无 hash/快照/时间 → `violates check constraint "ck_biz_flow_def_version_published"` |
| 8 | `ck_biz_flow_def_version_current_published` | DRAFT 挂 `is_current` → `violates check constraint "ck_biz_flow_def_version_current_published"` |
| 9 | `uk_biz_flow_def_version_current` | 同 def 两个当前版本 → `duplicate key … "uk_biz_flow_def_version_current"` |
| 10 | `biz_flow_def_version_version_no_check` | `version_no = 0` → `violates check constraint "biz_flow_def_version_version_no_check"` |
| 11 | `biz_flow_node_node_type_check` | `node_type='GATEWAY'` → `violates check constraint "biz_flow_node_node_type_check"` |
| 12 | `biz_flow_node_assignee_type_check` | `assignee_type='NOTIFY'` → 拒绝 |
| 13 | `biz_flow_node_approval_mode_check` | `approval_mode='MAJORITY'` → 拒绝 |
| 14 | `biz_flow_node_approve_ratio_check` | `approve_ratio = 1.5` → 拒绝 |
| 15 | `ck_biz_flow_node_ratio` | `RATIO` 不填阈值 → `violates check constraint "ck_biz_flow_node_ratio"` |
| 16 | `biz_flow_node_fork_mode_check` | `fork_mode='XOR'` → 拒绝（本仓只有 AND/OR） |
| 17 | `biz_flow_node_join_mode_check` | `join_mode='BOTH'` → 拒绝 |
| 18 | `reject_veto` NOT NULL | 显式 `NULL` → `null value in column "reject_veto" … violates not-null constraint` |
| 19 | `ck_biz_flow_node_key` | `1bad` → `violates check constraint "ck_biz_flow_node_key"` |
| 20 | `uk_biz_flow_node` | 同 `(version_id, node_key)` → 拒绝 |
| 21 | `biz_flow_node_assignee_assignee_type_check` | `INITIATOR_SUPERIOR` 落行 → 拒绝（架构 N22：只有 USER/ROLE/DEPT_LEADER 是可枚举来源） |
| 22 | `biz_flow_node_assignee_pkey` | 同 `(node_id, type, ref_id)` → 拒绝 |
| 23 | `ck_biz_flow_edge_not_self` | 自环 → `violates check constraint "ck_biz_flow_edge_not_self"` |
| 24 | `ck_biz_flow_edge_default_no_cond` | 兜底边带条件 → `violates check constraint "ck_biz_flow_edge_default_no_cond"` |
| 25 | `uk_biz_flow_edge` | 同 `(version_id, edge_key)` → 拒绝 |
| 26–29 | 4 条外键 | 不存在的 def / version / version / node → `violates foreign key constraint "…_fkey"` |
| 30 | CASCADE | 删 version 后 `nodes=0 edges=0 assignees=0` |

（表格只列了 30 行，实际 35 条断言：其余 5 条是「合法值必须被接受」的正例，
例如「`is_current` 挂在补齐三件套的 PUBLISHED 上可以通过」）

---

## 4. 环检测：应用层 O(V+E) DFS（架构 §8 的证据复现）

### 4.1 单测实测（`cargo test --lib flow::`）

```
[R3] 纯链 200 节点 → None，耗时 613.624µs（613 µs）        ← 断言 < 50ms
[R3] 加回边后闭合路径（196 跳）：n5 → n6 → … → n199 → n5
自环 A→A → 检出 ["b","b"]
菱形分支（条件 → 两审批 → 汇合）→ None（不是环）
拓扑排序：链 n0..n4 → 拓扑序正确；有环时 Kahn 返回 None 且列出环上节点
```

> `cargo test` 实际输出：`test result: ok. 41 passed; 0 failed`（`business::flow::*`），
> 其中环检测 4 条 + 拓扑排序 2 条 + 15 条校验的正/反例 + DSL 6 条 + 快照/hash 4 条 + 其它。

### 4.2 接口级复现（真实 HTTP，200 节点图）

```
保存 200 节点 + 258 边的压力图 → 200
  nodes=200 edges=258
200 节点压力图 → 环检测不报 R3（HTTP 往返 77ms；其余不通过项=['R6','R8']）
  ← 跳跃边=隐式分叉，按 D7 本就该被 R6/R8 拦；本项只验「无环判定快且正确」
加回边 c198→c5 → R3 返回闭合路径（首尾相同）
  文案=检测到环：c5 → c6 → c7 → … → c198 → c5（流程必须无环；…）
自环在保存期被拦 → 400 code=65005 msg=连线 e9 是自环（n_a → n_a），流程不允许自环
```

### 4.3 严禁递归 CTE（红线）

```
$ grep -rniE 'recursive' backend/src/business/flow/
mod.rs:37:  //! **全模块 `grep RECURSIVE` 必须 0 命中**（验收 6）。
graph.rs:12: //! | `WITH RECURSIVE … CYCLE`（路径枚举） | **300 秒未结束**…
graph.rs:13: //! | `WITH RECURSIVE … UNION`（去重） | …**有环也返回 200 行 ⇒ 判不出环** |
```

**3 处命中全部是解释「为什么不用它」的注释，代码 0 命中**。这条不只靠人工 grep：
`graph.rs` 里常驻一条单测 `module_sources_have_no_cte_cycle`，用 `include_str!` 把本模块四个源文件
编进来做文本断言（非注释行出现关键词即红），随 `cargo test` 每次执行：

```
[环检测] 4 个源文件里 RECURSIVE 只出现在注释中（非注释命中 0）
```

---

## 5. 发布校验：15 条（卡面 12 + 架构 V7/V9/V12），每条都有正例 + 反例

### 5.1 卡面 12 条 ↔ 架构 §3.7 V 编号映射

| 卡面 | 规则 | 架构 §3.7 | 错误码 | 单测 |
|------|------|-----------|--------|------|
| 1 | 恰好 1 个 `START` | V1 | 65010 | `r1_rejects_zero_or_two_starts` |
| 2 | 至少 1 个 `END` | V2（前半） | 65010 | `r2_rejects_missing_end` |
| 3 | 无环（DFS 返回闭合路径） | V3 | **65011** | `r3_rejects_cycle_and_names_every_node` |
| 4 | `START` 入度=0、`END` 出度=0 | V5/V6 的度数族 | 65012 | `r4_rejects_start_with_inbound_and_end_with_outbound` |
| 5 | 除 `END` 外每个节点 ≥1 条出边 | V2/V5 | 65012 | `r5_rejects_node_without_out_edge` |
| 6 | `APPROVAL`/`COUNTERSIGN`/`CC` 出度 = 1（D7） | V5 | 65012 | `r6_rejects_approval_with_two_out_edges` |
| 7 | `CONDITION`/`PARALLEL` 出度 ≥ 2 | V6 | 65012 | `r7_rejects_condition_with_single_out_edge` |
| 8 | 禁止混流汇聚（D6） | V8 | 65014 | `r8_rejects_xor_branches_joining_with_and` + `r8_rejects_mixed_sources` |
| 9 | `approve_ratio ∈ (0,1]` + 会签模式必填（D3） | V10 | 65015 | `r9_rejects_bad_ratio_and_missing_mode` |
| 10 | 条件 DSL 白名单 / 深度≤8 / 节点≤64 | V11 | 65016 | `r10_rejects_unknown_field_and_deep_nesting` |
| 11 | 所有节点从 `START` 可达 | V4 | 65010 | `r11_rejects_unreachable_node` |
| 12 | 所有节点都能到达某个 `END` | V2（后半） | 65010 | `r12_rejects_node_that_cannot_reach_end` |
| 附 | 条件节点恰 1 条 `is_default` + 其余带条件 | **V7** | 65013 | `r13_rejects_missing_default_edge` |
| 附 | 审批类节点审批人配置（`USER/ROLE/DEPT_LEADER` 至少 1 行；`FORM_VAR` 要变量名） | **V9** | 65015 | `r14_rejects_empty_assignee` |
| 附 | 规模上限：节点 ≤ 200 / 边 ≤ 400 | **V12** | 65017 | `r15_rejects_oversized_graph` |

卡面的 12 条与文档的 V1–V12 是同一套语义的两种切法（卡面把「度数」拆成 4 条、把「可达性」拆成 2 条），
文档的 V7/V9/V12 卡面没单列但**必须实现**（否则「条件分支缺兜底」「没有审批人」会漏到运行时），
所以实际实现是 **R1–R15 十五条，一条不少**。

### 5.2 接口级反例（每条的 validate 文案 + publish 错误码都是真机返回）

| 规则 | validate 命中文案 | publish |
|------|------------------|---------|
| R1 | 流程必须有且仅有一个开始节点（当前 0 个） | 400 / **65010** |
| R2 | 流程至少要有一个结束节点 | 400 / **65010** |
| R3 | 检测到环：n_a → n_end → n_a（流程必须无环；分支请用「条件」节点，并让所有分支最终汇合） | 400 / **65011** |
| R4 | 结束节点 n_end 有 1 条出边（结束节点必须没有出边） | 400 / **65012** |
| R5 | 节点 n_lost（孤儿审批）没有出边（除结束节点外，每个节点都必须继续往下走） | 400 / **65012** |
| R6 | 审批节点 n_a（A 审批）有 2 条出边（审批节点只能有一条出边，分支请用条件网关） | 400 / **65012** |
| R7 | 条件节点 n_cond（条件）只有 1 条出边（条件 / 并行节点至少要有 2 条出边…） | 400 / **65012** |
| R8 | 汇聚节点 n_merge（汇合）的入边来自 n_cond 的条件分支（XOR），必须用 OR 汇聚（当前 AND）—— 用 AND 汇聚会有一条分支永远不激活 → 审批单永久卡死 | 400 / **65014** |
| R9 | 会签节点 n_cs（会签）未设置会签方式（全员同意 / 按比例 / 或签，必须选一个） | 400 / **65015** |
| R10 | 边 e2（n_cond → n_a）的条件表达式非法：字段 secret.amount 不在白名单内 | 400 / **65016** |
| R11 | 节点 n_end2（结束2）从开始节点走不到（悬空节点，请连上或删掉） | 400 / **65010** |
| R12 | 节点 n_a（A）走不到结束节点（分支没有汇合，流程会在这里走丢） | （同图另有环，首条为 65011） |
| R13 | 条件节点 n_cond（条件）有 0 条兜底分支（必须恰有 1 条兜底分支…） | 400 / **65013** |
| R14 | 节点 n_a（A 审批）未配置审批人（请选择审批人来源：指定用户 / 角色 / 发起人 / 主管 …） | 400 / **65015** |
| R15 | 流程节点数 202 超过上限 200（请拆成多个流程） | 400 / **65017** |

**每条失败后都回查了库**：`失败未留下 PUBLISHED 版本 → status=DRAFT`（发布是全或无，不会半发布）。

正例：最简图（START → END）与「条件分支 + 汇合」的菱形图都是 `passed=true / 0 issues`，能正常发布。

---

## 6. 图操作（验收 8–11）

### 6.1 保存整图的计数与库一致（验收 8）

```
PUT /business/flow/version/{v2}/graph → 200 nodes=6 edges=6 warnings=0
库里：db_nodes=6 db_edges=6 db_assignees=1        ← 与回执逐字一致
```

### 6.2 PUBLISHED 版本的行不可改（验收 9，红线）

```
PUT /business/flow/version/{v1}/graph → 409 code=65022
msg=该版本已发布，不能直接编辑（请先「另存为新草稿」再改）
```

### 6.3 发布后快照有内容、hash 稳定（验收 10）

```
publish v1 → 200 PUBLISHED hash=c492bd027ef8e14e…
库里：snapshot_len=1157  hash_equal=t  status=PUBLISHED is_current=t
def.current_version_id 指向 v1 且 status=PUBLISHED
复制 v2 后原样发布 v3 → hash 与 v2 完全相同（ed2d6bf8e2db…）
```

> 生产冒烟里同一张最简图发布的 hash 同样是 `c492bd027ef8e14e…`（不同库、不同环境、不同时间
> → 同一个 hash），这是 canonical JSON 的实证：节点/边按 key 排序、对象键递归升序、
> 审批人按 `(type, ref_id)` 排序、比例用 `0.600` 字符串保精度。

新增版本时旧版本自动降级：

```
旧版本 v1 → ARCHIVED / v2 → PUBLISHED+current
```

### 6.4 另存草稿：改流程不动已发布版本（验收 11）

```
POST /def/{id}/draft → 200 version_no=2（DRAFT），复制 2 节点 / 1 边
PUT  graph（改成条件分支 6 节点 6 边）→ 200
回查 v1：['PUBLISHED', 't', 'c492bd02…', '2']   ← hash / 节点数 / 状态一字未变
```

`copy_version_to_draft` 是 3 条纯 SQL（节点 → 审批人 → 边，按 `node_key` 映射新 id），
200 节点的图也只有 3 次往返（不把图读进内存再写回）。

---

## 7. 单测 / 门禁

```
$ cargo test --lib                → test result: ok. 734 passed; 0 failed
                                    （基线 692 + 本卡新增 42 = 41 条 flow + 1 条 65xxx 码表自检）
$ cargo test --lib flow::         → test result: ok. 41 passed; 0 failed
$ cargo clippy --all-targets -- -D warnings → 0 warning
$ cargo fmt -- --check            → 干净
$ bash scripts/check.sh           → 全部检查通过
   闸①  0 处
   闸②  0 处（5 张 biz_flow_* 已登记，flow/model.rs 写 SCOPE-EXEMPT）
   C7 G1–G6 全 PASS
   偏好字典与色板 22 套一致
```

**闸② 豁免口径**：流程定义是**全局配置**（不是谁的业务数据），按部门范围裁剪会导致
「A 部门的人看不到自己的单据要走哪个流程」；写权限由 `business:flow:create/update/delete/publish` 隔离。
同口径先例：`biz_task_status_def`（全局字典）。

---

## 8. §6.2 部署验收（生产 8023 / 库 `cloudpivot`）

```
① 部署前指纹
HEAD           = 19e511e7c66ea495478debe0b0e6f2744fdf8b2f
release sha256 = a78abe6f131e6cd1f02ff62dcd18a13fd4763bd182e04f0523df51d69eb5af20
binary mtime   = 2026-10-29 21:21:48

② systemd cloudpivot.service → active（Main PID 1273824）
   /health → {"code":0,…,"profile":"prod"}

③ 迁移证据（生产库）
schema_rows=120/versioned/flow_def_tables,121/versioned/flow_def_permissions
seed_workflow=workflow_permissions/true
flow_tables=5   flow_indexes=17   flow_menus=6
grants_read=5（全部角色）   grants_write=4（SUPER_ADMIN）

④ 启动日志
NOTICE: V0120 自检通过：5 张表 + 4 条关键索引 + node 约束 + 4 条外键
NOTICE: V0121 自检通过：1 个页面节点 + 5 个权限按钮（business:flow:read/create/update/delete/publish）
数据权限自检通过：11 张登记表的部门列/归属列均存在，SUPER_ADMIN 角色存在
```

### 8.1 接口 401 + 真实登录 200

```
GET /business/flow/def/page              未登录 → 401 {"code":401,"msg":"未授权访问，请先登录"}
PUT /business/flow/version/{vid}/graph   未登录 → 401
GET /business/flow/def/page              真实登录 → 200 code=0 total=0 page=1 page_size=10
```

（`total=0` 是正常的：生产库里还没有任何流程定义 —— D1 只提供「定义」能力，D6 前端页面建好后才有数据。）

### 8.2 生产写路径冒烟（建 → 发布 → 查 → 软删 → 收尾硬清）

```
POST /business/flow/def        → 200 {"version_no":1,"node_count":2,"edge_count":1}
POST /def/{id}/publish         → 200 {"status":"PUBLISHED","graph_hash":"c492bd027ef8e14e…","node_count":2,"edge_count":1}
库里版本行                      → 1 | PUBLISHED | t | 2 | 1 | 1157 | hash_ok=t
GET  /def/{id}/versions        → 200（含 published_by_name）
GET  /def/page?keyword=…       → 200（列表里能查到）
DELETE /def/{id}               → 200 {"deleted":true}
收尾（本卡不在生产留演示数据）    → left_defs=0 / left_versions=0 / left_nodes=0
```

---

## 9. 交接给 D2–D6 的三条注意事项

1. **`model::has_running_instance(def_id)` 目前恒返回 `false`**：本卡范围内 `biz_flow_instance` 还不存在。
   D2 建表后**只改这一个函数**（文件里已写好要换的 SQL），`DELETE /def/{id}` 的 409/65021 就自动生效 ——
   刻意写成函数而不是注释，是为了让 D2 不会漏掉「删流程前先查引用」。
2. **L3 运行期内存图缓存不在本卡**：`graph::parse_snapshot(&str)` 已经能反序列化快照（带
   `schema_version` 校验），D2 的缓存直接从这里进。
3. **节点上的运行期策略字段已进快照**：`empty_assignee` / `self_approval` / `allow_robot` /
   `timeout_hours` / `timeout_action` / `remind_before_hours` / `max_remind_times` / `cc_on_approved`
   都会随发布写进 `graph_snapshot`（单测 `snapshot_carries_runtime_policy_fields` 守着）。
   D2 的引擎只读快照，**不要**再回表读 `biz_flow_node`（那会重新引入「行可变 → 在跑单子变道」的风险）。

---

## 10. 复跑命令

```bash
# 验收环境（144）
bash /root/d1-run/accept_setup.sh                  # 建库 cloudpivot_d1 + 独立 target 编译
bash /root/d1-run/svc.sh start                     # 起 8028（空库启动即跑全量迁移）
bash /root/d1-run/migrate_check.sh                 # 结构 + 35 条约束实测 + down/up
python3 /root/d1-run/d1_accept.py verify           # 84 条接口断言
# 生产
bash /root/d1-run/prod_deploy.sh                   # 构建产物已在位，重启 + 迁移证据
bash /root/d1-run/prod_api_check.sh                # 401 + 登录 200 + 权限节点
bash /root/d1-run/prod_smoke.sh                    # 生产写路径冒烟（自动收尾）
```
