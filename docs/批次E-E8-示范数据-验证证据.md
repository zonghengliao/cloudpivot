# 批次 E · E8 示范数据（帮助文档 + 项目）验证证据

- 看板卡：`t_c4be09f0`「【E8·P1】填充示范数据：帮助文档4-5篇 + 项目2个（种子迁移）」
- 提交：`80ccc2b`（4 文件，+817 行，仅 SQL，未动 Rust 代码）
- 复跑脚本：`/root/e8/step_a.sh`（空库首启）、`/root/e8/step_b.sh`（up→down→up 往返）、
  `/root/e8/e8_api.py` + `/root/e8/step_c.sh|step_f.sh`（接口验收）、`/root/e8/step_i.sh`（§6.2）
- 本卡**不改 Rust 代码**，全部是「读表结构 → 写 SQL → 空库往返 → 生产应用 → 接口实测」。

## 一、交付物

| 文件 | 行数 | 作用 |
|---|---|---|
| `backend/migrations/business/V0131__seed_help_docs.up.sql` | 536 | 2 个分类 + 5 篇已发布文档 + 5 条版本快照 |
| `backend/migrations/business/V0131__seed_help_docs.down.sql` | 51 | 版本 → 文档 → 分类，逆序零残留 |
| `backend/migrations/business/V0132__seed_projects.up.sql` | 188 | 2 个 ACTIVE 项目 + 6 条示范任务 |
| `backend/migrations/business/V0132__seed_projects.down.sql` | 42 | 任务 → 摘关联 → 项目，逆序零残留 |

## 二、帮助中心内容（5 篇，57–77 行 Markdown）

| slug | 标题 | 分类 | 行数 | 字节 |
|---|---|---|---|---|
| `what-is-cloudpivot` | 云枢是什么 | 快速开始（共用 V0086） | 57 | 2612 |
| `create-first-task` | 5 分钟建第一个任务 | 快速开始（共用 V0086） | 64 | 2685 |
| `task-status-and-progress` | 任务状态与进度规则 | 任务管理（新建） | 73 | 3424 |
| `subtask-and-dependency` | 子任务与依赖 | 任务管理（新建） | 62 | 2743 |
| `draw-first-approval-flow` | 画第一个审批流程 | 审批流（新建） | 77 | 3174 |

- **分类决策**：V0086 已种 3 个默认分类（快速开始 / 常见问题 / 功能说明）。本卡**不重复造
  「快速上手」**，而是复用「快速开始」放 2 篇上手文档，另新建 2 个分类（`task-management` /
  `approval-flow`）承载专业内容 —— 满足卡面「2 个分类」且帮助中心不出现两个同义分类。
- **内容准确性**：状态机 5 态 / 10 条合法流转、进度两种模式、`63010/63011/63012`、
  子任务 2 层限制 `63005`、依赖环检测 `63101`、甘特 `63105/63110–63112`、
  审批节点 `START/APPROVAL/COUNTERSIGN/CONDITION/END`、门闸 `65030` —— 全部对着
  `src/business/task/state.rs`、`src/shared/error_code.rs`、`src/business/flow/graph.rs` 核过一遍才写。
- `content_html` 固定写 `''`：红线要求它只能由 `shared/content` 生成；读路径
  `help/handler.rs::detail_html` 对空快照有兜底（按 `content_md` 现渲 + 过闸 3 + 生成 TOC），
  实测详情接口返回 1866–3454 字节 HTML。

## 三、项目示范

| 项目 | 编码 | 状态 | 负责人 | 挂的任务 |
|---|---|---|---|---|
| 云枢平台迭代 | `CP-ITER` | ACTIVE | admin（超级管理员） | TSK-202610-000201 帮助中心：用户侧文档检索（DOING 40%）／000202 审批流：超时自动提醒（TODO）／000203 任务看板：三视图切换体验优化（TODO） |
| 示例：电商后台 | `DEMO-EC` | ACTIVE | admin（超级管理员） | TSK-202610-000204 商品列表页改版（TODO）／000205 订单退款审批流程（DOING 30%）／000206 库存预警通知（TODO） |

- 任务编号刻意落在 `TSK-202610-000201`–`000206`（既有任务只到 `000148`），不撞号、也不插队；
  标题都是真实可交付的工作项，没有「测试1/测试2」。
- `owner_id` 不写死：按 `admin` → `laoliu_test` → NULL 解析。空库首启（迁移早于
  `S__role_data.sql`，一条用户都没有）时项目先以 `owner_id = NULL` 落库并**整体跳过任务种子**
  （`biz_task.owner_id/creator_id` 是 NOT NULL + FK），不造半条脏数据；用户就位后重放本迁移
  会自动补负责人并种任务（实测见 §四 B 段）。

## 四、空库跑迁移：up → down → up（库 `cloudpivot_e8`，实例 127.0.0.1:18081）

首启日志（空库，89 个迁移一次到位）：

```
NOTICE: V0131 自检通过：2 个分类 + 5 篇已发布文档 + 5 条版本快照
NOTICE: V0132：当前库没有任何用户（空库首启），已跳过任务种子；项目已建但暂挂 0 条任务（应用启动并建出 admin 后重跑本迁移即可）
数据库迁移完成: 应用 89 个迁移
cloudpivot 启动完成, 耗时 396ms
```

A. 空库首启后：

```
sys_user 行数            : 1
新增分类(…04/…05)        : 2
全部可见分类             : getting-started,faq,features,task-management,approval-flow
示范文档                 : 5
版本快照                 : 5
项目(编号/负责人空?/状态) : CP-ITER/NULL/ACTIVE | DEMO-EC/NULL/ACTIVE
示范任务                 : 0
```

B. 用户就位后重放 V0132（补负责人 + 种任务）：

```
NOTICE:  V0132 自检通过：2 个 ACTIVE 项目（CP-ITER 3 条任务 / DEMO-EC 3 条任务），负责人 01a18a9f-…
项目(编号/负责人名/状态) : CP-ITER/admin/ACTIVE | DEMO-EC/admin/ACTIVE
CP-ITER 任务数           : 3
DEMO-EC 任务数           : 3
```

C. down（V0132 → V0131）零残留：

```
新增分类残留             : 0
示范文档残留(按 slug)    : 0
示范文档残留(按 id)      : 0
版本快照残留             : 0
项目残留                 : 0
示范任务残留             : 0
V0086 默认分类仍在(应=3) : 3
```

D. 再 up（内容全回来）：

```
NOTICE:  V0131 自检通过：2 个分类 + 5 篇已发布文档 + 5 条版本快照
NOTICE:  V0132 自检通过：2 个 ACTIVE 项目（CP-ITER 3 条任务 / DEMO-EC 3 条任务）
文档 5 / 版本快照 5 / 项目 2 / 任务 6 ；schema_migrations 最大版本 = 132
```

E. up 再跑一遍（真幂等）：

```
文档(仍应为 5)           : 5
任务(仍应为 6)           : 6
```

## 五、生产库迁移（cloudpivot / 8023）

```
重启前 schema_migrations 最大版本 : 130
systemctl restart cloudpivot → active，health http 200
NOTICE: V0131 自检通过：2 个分类 + 5 篇已发布文档 + 5 条版本快照
NOTICE: V0132 自检通过：2 个 ACTIVE 项目（CP-ITER 3 条任务 / DEMO-EC 3 条任务），负责人 01a17bb9-7d99-76c3-b362-58d3a284c270
数据库迁移完成: 应用 2 个迁移
cloudpivot 启动完成, 耗时 212ms

130 task_menu_group
131 seed_help_docs
132 seed_projects
```

## 六、真实登录后的接口验收（生产 8023，`e8_api.py`）

```
== 登录 ==  HTTP 200 :: {"code":0,"msg":"ok","data":{"access_token":"eyJ0eX…}}
== 分类树 == HTTP 200 :: 快速开始(doc_count=2) / 任务管理(doc_count=2) / 审批流(doc_count=1)
[PASS] 帮助中心分类树可读 / 新分类 task-management 在 / approval-flow 在
== 文档列表 == HTTP 200 :: total=5
   - 画第一个审批流程 | slug=draw-first-approval-flow | cat=审批流 | status=PUBLISHED | v1
   - 任务状态与进度规则 | slug=task-status-and-progress | cat=任务管理 | status=PUBLISHED | v1
   - 云枢是什么 | slug=what-is-cloudpivot | cat=快速开始 | status=PUBLISHED | v1
   - 子任务与依赖 | slug=subtask-and-dependency | cat=任务管理 | status=PUBLISHED | v1
   - 5 分钟建第一个任务 | slug=create-first-task | cat=快速开始 | status=PUBLISHED | v1
== 详情 what-is-cloudpivot == HTTP 200 :: title=云枢是什么 status=PUBLISHED v1
   content_md 1157 字节 / content_html 1866 字节 / toc 1 项
   html 开头: <h2 id="h-2-1">云枢是什么</h2> <p>云枢（CloudPivot）是团队内部的一体化协作平台…
   <h3 id="h-3-1">一、四大能力</h3><table><thead><tr><th…
[PASS] 文档详情可打开（slug 当 id）
[PASS] Markdown 渲染成 HTML（含 <h2 标题层级）
[PASS] 表格渲染成 <table>
[PASS] TOC 非空且锚点与 HTML 一致 —— toc ids=['h-2-1']
[PASS] 无 <script> 残留（净化生效）
[PASS] create-first-task 渲染含 <pre（代码块）   HTTP 200 / 2194 字节
[PASS] task-status-and-progress 渲染含 <table   HTTP 200 / 3454 字节
== 项目列表 == HTTP 200 :: total=2
   - 示例：电商后台 | code=DEMO-EC | status=ACTIVE | tasks=3 | owner=超级管理员
   - 云枢平台迭代 | code=CP-ITER | status=ACTIVE | tasks=3 | owner=超级管理员
== DEMO-EC 的任务 == HTTP 200 :: total=3  (000205 DOING 30% / 000204 TODO / 000206 TODO)
== CP-ITER 的任务 == HTTP 200 :: total=3  (000201 DOING 40% / 000202 TODO / 000203 TODO)
[PASS] 项目列表有 2 个项目 / DEMO-EC 挂 2-3 条真实任务 / CP-ITER 挂 2-3 条真实任务
===== 汇总：PASS 14 / FAIL 0 =====
```

导航入口（菜单）在库：

```
20000000-…-740 | 帮助中心 | 菜单 | system:help:read  | /help    | is_visible=t status=1
20000000-…-775 | 项目     | 菜单 | business:project:read | /project | is_visible=t status=1
```

## 七、§6.2 部署验收

```
1) 二进制指纹（本卡无 Rust 改动，指纹与部署时一致）
   6e15bc9d39ec5daf03aa59c84cbc2076e67aaa8e7dd7725b953e4d31ae925ceb  36,628,496 B  2026-10-30_10:19:05
2) systemctl is-active: active     MainPID: 1666634     启动时刻: 2026-10-30 10:25:13
3) 迁移：数据库迁移完成: 应用 2 个迁移
4) /health：HTTP=200 {"code":0,"msg":"ok","data":{"status":"ok","app":"cloudpivot","profile":"prod",…}}
5) 无令牌访问（401 = 路由在位，不是 404）：
   /api/business/help/doc/page        HTTP=401 {"code":401,"msg":"未授权访问，请先登录"}
   /api/business/help/category/tree   HTTP=401 {"code":401,"msg":"未授权访问，请先登录"}
   /api/business/project/page         HTTP=401 {"code":401,"msg":"未授权访问，请先登录"}
6) 前端：GET / → HTTP=200
7) schema_migrations：130 task_menu_group / 131 seed_help_docs / 132 seed_projects
```

## 八、测试不回归（`cargo test -j 1`）

```
16 个测试目标，全部 ok
汇总：passed=874 failed=0 ignored=7（7 个 ignored 是 doctest 标记）
其中 lib 单测：825 passed; 0 failed
```

## 九、已知边界（诚实清单）

1. **空库首启时项目负责人为 NULL**：迁移早于用户种子，`biz_task.owner_id` 不可空 →
   任务种子整体跳过（日志 NOTICE 明说）。应用 bootstrap 建出 admin 后重放一次 V0132
   即补齐负责人与 6 条任务（已实测，见 §四 B）。生产库因为已有用户，一次到位。
2. **`content_html` 源数据里是空串**：首次被读时才由 `shared/content` 渲染并返回（不落库）。
   代价是每次读都渲染一遍（实测 1–3 KB 文档，接口整体 < 20 ms），收益是渲染口径永远跟代码一致。
3. **TOC 目前只有一级（h2）**：`Recipe::Help` 把 `#` 偏移成 `<h2>`，TOC 只收该层
   （实测 `toc ids=['h-2-1']`）。文档内部用 `##`/`###` 分节，正文层级正常，只是侧栏目录不展开 h3。
4. **截图位是占位块**（`> 📷 截图位 1/2/3`），没放真图片：仓库里没有对应的产品截图资产，
   放 Markdown 图片会得到裂图，故用可读占位并在文档里说明由运营补图。
5. **「常见问题」「功能说明」两个分类仍是空的**：它们是 V0086 种的默认分类，本卡只负责
   示范内容，未强行塞凑数文档。
6. **审批演示流程未新增**：卡面写「`task_accept` 已存在就不用动」——库里
   `biz_flow_def.flow_code = 'task_accept'`（`任务验收`，TASK_ACCEPT，PUBLISHED，is_default=t，
   未删）确实存在，故按卡面跳过。
7. **两条迁移在 `migrations/business/`**（不是 `migrations/seed/`）：`S__*.sql` 是「每次启动
   都重放」的种子，而本卡要求「空库 up→down→up、down 零残留」，必须是**可回滚的版本化迁移**。
