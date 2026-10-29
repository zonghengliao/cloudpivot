# 批次 D · D4a-follow 前端接线验证证据

- 卡片：`t_55242a3b`「【D4a-follow】前端接线：我发起 Tab + 待办卡片新字段 + 详情 form_data 展示」
- 改动范围：**只改 `frontend/src`**（3 个文件 + 1 个自动生成的类型声明），后端一行未动、零新增依赖
- 后端契约来源：`main @ 27f1fba`（D4a），`backend/src/business/flow/dto.rs` / `handler.rs`

## 0. 本次接线清单

| # | 位置 | 改动 |
|---|---|---|
| 1 | `frontend/src/api/business/flow-api.ts` | 新增 `listMyInstances(query)` → `GET /api/business/flow/instance/my`；`FlowTaskItem` / `FlowCcItem` 补 `flow_name` / `initiator_name` / `current_node_name` / `arrived_at`；`FlowInstance` 补 `form_data?: Record<string, unknown>`；文件头与 DTO 注释里「D2 没下发」的说明全部改写 |
| 2 | `frontend/src/views/business/flow/task/index.vue` | 「我发起的」Tab 改打 `/instance/my`（删掉前端按 `initiator_id` 过滤 + 删掉 `userStore` 依赖）；卡片标题旁显示 `flow_name`、次行显示「发起人：`initiator_name`」与「当前节点：`current_node_name`」（不再显示 `node_key`）；时间口径改 `arrived_at`；文件头「DTO 缺口」整段删除改写 |
| 3 | `frontend/src/views/business/flow/instance/detail.vue` | `formFields` 改读 `detail.instance.form_data`（键名原样、`String(v)`、数组/对象走 JSON 字符串）；「接口未下发 form_data」的 `t-alert` 占位删除；空表单显示「发起时未填写表单内容」 |
| 4 | `frontend/src/types/components.d.ts` | `pnpm build` 自动重生成：删掉已无处引用的 `TSkeleton` 声明（全仓 `t-skeleton` 0 命中） |

**未改**：`frontend/src/api/request.ts`（`git diff` 无此文件）、`package.json`、`pnpm-lock.yaml`、`backend/**`。

## 1. 验证环境（与线上隔离，不动 prod）

| 项 | 值 |
|---|---|
| 后端 | 144 `:8099`，二进制 = `27f1fba` 的 debug 构建（`/root/d3b-build` @ `27f1fba` 编出的 `/root/d3b-target/debug/cloudpivot`，复制到 `/root/d4a-follow/` 后运行，不覆盖他人产物） |
| 库 | `cloudpivot_d4af` = **prod 库 `cloudpivot` 的克隆**（`pg_dump -Fc` → `pg_restore`，15 MB，迁移 0 条待跑）。**没写 prod 库** |
| Redis | `db 9`（`redis://127.0.0.1:6379/9`） |
| 前端 | `/root/work/d5repo`（`27f1fba` + 本卡改动），`vite dev :5209`，`BACKEND_ORIGIN=http://127.0.0.1:18299`（SSH 隧道 → 144 的 `:8099`） |
| 浏览器 | headless Chromium（CDP 驱动）；账号 `admin`，口令只从 144 `/etc/cloudpivot/cloudpivot.env` 读进内存，任何日志都不打印 |
| prod 影响 | **零**：`:8023` 与库 `cloudpivot` 均未触碰；`/opt/cloudpivot` 工作区未改动 |

## 2. 逐条验收

### 2.1 `pnpm typecheck` 零错误、`pnpm build` 通过

```
$ pnpm typecheck   # vue-tsc --noEmit
typecheck exit=0    （日志：evidence/d4a-follow/typecheck.txt）

$ pnpm build
✓ 4698 modules transformed.
✓ built in 17.00s   （日志：evidence/d4a-follow/build.txt）
```

配色门禁（D5 就有的门禁，本卡复跑）：

```
$ node scripts/check-palettes.mjs
色阶算法断言：330 条，失败 0 条
色板数据校验：通过
[check-palettes] 全部通过 ✓
```

改动文件的裸 hex 扫描：`grep -icE '#[0-9a-f]{3,8}\b'` → **0 命中**（新增的流程名 chip 只用
`--td-text-color-secondary` / `--td-bg-color-secondarycontainer` / `--td-component-stroke`，
实测前景 `rgb(66,97,103)` / 背景 `rgb(249,250,251)` → **对比度 6.41**，12px 小字过 AA）。

### 2.2 「我发起的」Tab：条数 = 库里 `initiator_id = 当前用户` 的数量

先看接口层：浏览器 `performance.getEntriesByType('resource')` 里命中的就是新接口，**不再有** `/instance/page`：

```
/api/business/flow/task/todo?page=1&page_size=50
/api/business/flow/instance/my?page=1&page_size=50     ← 新增接线命中
```

再看页面与库的对照（同一时刻）：

| 来源 | 结果 |
|---|---|
| 页面「我发起的」Tab | **5 张卡片** |
| 库 `cloudpivot_d4af`：`select count(*) from biz_flow_instance i join sys_user u on u.id=i.initiator_id where u.username='admin'` | **5** |

库里这 5 条（按 `started_at` 倒序，与页面顺序一致）：

```
FLW-20261029-B225EE25 | D4a 接线验收：无表单内容的单                 | initiator=admin | PENDING  | form_data={}
FLW-20261029-959A027F | D4a 接线验收：差旅报销 1280 元                 | initiator=admin | PENDING  | form_data={"content":"上海出差 2 天，交通 780 + 住宿 500，发票 3 张。","attachments":[]}
FLW-20261029-E0FCFC1F | D5 真机：客户餐叙报销 3200 元                  | initiator=admin | PENDING  | form_data={"content":"4 人餐叙，发票 2 张。","attachments":[]}
FLW-20261029-1DFCB8B4 | D5 真机：报销 6800 元（第 2 步待审）            | initiator=admin | APPROVED | form_data={...}
FLW-20261029-CE9AA370 | D5 真机：报销 6800 元（第 2 步待审）            | initiator=admin | APPROVED | form_data={...}
```

口径正确性（**不是** 0、**不是**全量、**不是**可见性并集）：

- 「抄送我的」Tab 同一时刻是空态「没有抄送给你的单子」，而「我发起的」有 5 条 —— 说明没有混进抄送单；
- 5 条卡片「发起人」全部是 `超级管理员`（= admin 本人），没有出现别人发起的单；
- 先测时库里是 4 条 → 页面 4 张；随后通过「发起审批」页再造 1 张带 `form_data` 的单 → 库里 5 条 → 页面刷新为 5 张（条数跟着库走，没有写死、没有演示数据）。

页面实测卡片（`我发起的`）：

```
D4a 接线验收：无表单内容的单   [D5 校验发布演示（v4）]  FLW-20261029-B225EE25 · 发起人：超级管理员 · 待我处理   审批中
D4a 接线验收：差旅报销 1280 元 [D5 校验发布演示（v4）]  FLW-20261029-959A027F · 发起人：超级管理员 · 待我处理   审批中
D5 真机：客户餐叙报销 3200 元  [D5 校验发布演示（v4）]  FLW-20261029-E0FCFC1F · 发起人：超级管理员 · 待我处理   审批中
D5 真机：报销 6800 元（第 2 步待审）[D5 校验发布演示（v4）] FLW-20261029-1DFCB8B4 · 发起人：超级管理员        已通过
D5 真机：报销 6800 元（第 2 步待审）[D5 校验发布演示（v3）] FLW-20261029-CE9AA370 · 发起人：超级管理员        已通过
```

> `InstanceRes` 里没有节点中文名，所以「我发起的」卡片**不编造节点名**，只把「待我处理」放进附注
> （`my_task_id` 非空时）。这一栏对 `my` Tab 不显示「当前节点」，避免出现「当前节点：待我处理」这种假节点名。

### 2.3 待办卡片：流程名 / 发起人姓名 / 当前节点中文名

接口层先确认后端真的下发了 4 个字段（同一台 8099 后端、同一个 admin token）：

```
first item keys: [arrived_at, assignee_id, assignee_name, assignee_source, comment, completed_at,
  created_at, current_node_name, flow_name, id, initiator_name, instance_id, instance_no,
  is_added_sign, is_auto, is_self_approval, node_key, round, status, task_kind, title, transferred_from]
关键字段: {"instance_no":"FLW-20261029-E0FCFC1F","title":"D5 真机：客户餐叙报销 3200 元",
  "flow_name":"D5 校验发布演示","initiator_name":"超级管理员","current_node_name":"审批",
  "arrived_at":"2026-10-29T14:04:48.382448Z","node_key":"n_1"}
```

页面「待我审批」实测（3 张，DOM 取文本）：

```
D4a 接线验收：无表单内容的单   [D5 校验发布演示]  FLW-20261029-B225EE25 · 发起人：超级管理员 · 当前节点：审批   10-29 22:38  待处理
D4a 接线验收：差旅报销 1280 元 [D5 校验发布演示]  FLW-20261029-959A027F · 发起人：超级管理员 · 当前节点：审批   10-29 22:36  待处理
D5 真机：客户餐叙报销 3200 元  [D5 校验发布演示]  FLW-20261029-E0FCFC1F · 发起人：超级管理员 · 当前节点：审批   10-29 22:04  待处理
```

- 流程名：标题右侧中性 chip，`D5 校验发布演示`（后端 `flow_name`）
- 发起人：`发起人：超级管理员`（后端 `initiator_name`）
- 当前节点：`审批`（后端 `current_node_name`，**不再是** `n_1` 这种 `node_key`）
- 时间/「已等 X」：`arrived_at`（`10-29 22:04` / `已等 32 分钟`）
- 布局自检：每张卡 `task-card__title` / `task-card__meta` 的 `scrollWidth <= clientWidth`（**无截断**），
  chip 实测 104×19 px、`border-radius: 3px`

### 2.4 详情页 form_data（先发起一张带 form_data 的单）

走「发起审批」页真流程新建：`D4a 接线验收：差旅报销 1280 元`（内容「上海出差 2 天，交通 780 + 住宿 500，发票 3 张。」），
落库 `form_data = {"content":"…","attachments":[]}`，详情页：

```
D4a 接线验收：差旅报销 1280 元   [审批中]   FLW-20261029-959A027F · D5 校验发布演示 v4 · 发起人 超级管理员 · 2026-10-29 22:36
审批内容
  attachments   []
  content       上海出差 2 天，交通 780 + 住宿 500，发票 3 张。
```

- 键名原样（`content` / `attachments`），值 `String(v)`，数组走 JSON 字符串 → `[]`
- 「接口未下发 form_data」那条 `t-alert` 已消失：`document.querySelectorAll('.content .t-alert').length == 0`
- 空表单分支也验了：另造一张不带 `form_data` 的单（后端存 `{}`，实例 `FLW-20261029-B225EE25`），
  详情显示 `发起时未填写表单内容`，字段行数 0、无 alert（不假装有内容、也不报错）

### 2.5 无新依赖、不改 `request.ts`

```
$ git diff --stat
 frontend/src/api/business/flow-api.ts              |  38 ++++++-
 frontend/src/types/components.d.ts                 |   2 -
 frontend/src/views/business/flow/instance/detail.vue |  40 ++++---
 frontend/src/views/business/flow/task/index.vue    | 124 +++++++++++----------
 4 files changed, 123 insertions(+), 81 deletions(-)
```

`request.ts` / `package.json` / `pnpm-lock.yaml` 未出现在 diff 中 → 零新增依赖、封装未动。

## 3. 发版顺序（重要，别踩）

**本卡没有把 `dist` 部署到 prod nginx**，原因：prod 的 `:8023` 还是 **D4a 之前**的构建 —— 实测 `admin` token 打
`http://127.0.0.1:8023/api/business/flow/instance/my` 拿不到数据，`/task/todo` 的 item 里也**没有**那 4 个新字段：

```
# prod :8023（预 D4a）
first item keys: [assignee_id, …, instance_no, is_added_sign, is_auto, is_self_approval, node_key,
  round, status, task_kind, title, transferred_from]      ← 无 flow_name / initiator_name / current_node_name / arrived_at
/instance/my → total = None（接口不存在）

# 验证 :8099（27f1fba）
first item keys: 含 flow_name / initiator_name / current_node_name / arrived_at
/instance/my → total = 5
```

所以**必须后端 D4a 先上线，再发前端 dist**；否则「我发起的」Tab 会直接 404 报错、待办卡片字段全空。
发版后复跑本文件第 2 节的对照 SQL 即可复核。

## 4. 复跑方式

```bash
# 1) 144 上准备验证库（克隆 prod，不写 prod）
/root/d4a_db.sh clone

# 2) 144 上起 D4a debug 后端 :8099（独立库 + redis db9）
/root/d4a_backend.sh

# 3) 本机起前端（隧道 18299 → 144:8099）
ssh -N -L 18299:127.0.0.1:8099 root@172.18.10.144 &
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18299 pnpm dev --port 5209 --strictPort

# 4) 浏览器打开 http://127.0.0.1:5209/flow/task（admin 登录）
#    对照 SQL：
/root/d4a_db.sh count        # = initiator_id=admin 的实例数，应与「我发起的」卡片数一致
```

## 5. 红线自查

| 红线 | 结论 |
|---|---|
| 不许前端造假数据 | ✅ 四个字段全部直取后端；`InstanceRes` 没有节点名就不显示节点名；空表单显示「未填写」，接口失败走错误态/空态，仓库无任何 mock |
| 只改 `frontend/src` | ✅ diff 只有 4 个前端文件；后端、迁移、部署配置零改动 |
| 不改 `request.ts` 封装 | ✅ 未出现在 diff |
| 不加依赖 | ✅ `package.json` / `pnpm-lock.yaml` 零改动 |
