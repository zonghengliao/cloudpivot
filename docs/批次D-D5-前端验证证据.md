# 批次 D · D5 前端验证证据（审批中心 + SVG 流程编辑器）

- 卡片：`t_f9ad26be`「批次 D·D5 前端：流程列表 + SVG 流程编辑器（纯自绘）+ 发起 / 待办 / 详情 5 页面」
- 后端：144 `/opt/cloudpivot` `main @ 7f1d52f`（**D1 定义侧 12 接口 + D2 实例侧 12 接口都已上线**，runtime `127.0.0.1:8023`）
- 前端：`pnpm build` 产物落 `/opt/cloudpivot/frontend/dist`，nginx `root /opt/cloudpivot/frontend/dist` 直接服务
- 浏览器：headless Chromium 1208（CDP 驱动，脚本在 `evidence/d5-shots/scripts/`，可复跑）
- 账号：`admin`（密码只从 `/etc/cloudpivot/cloudpivot.env` 读入内存，未落痕）

## 0. 交付物清单

| 类型 | 路径 | 说明 |
|---|---|---|
| 接口层 | `frontend/src/api/business/flow-api.ts` | 定义侧 12 个 + **实例侧 12 个**函数，字段名与后端 `dto.rs` 逐字对齐 |
| 图引擎 | `frontend/src/views/business/flow/graph.ts` | 几何 / 正交连线 / 网格吸附 / 拓扑分层布局 / 拓扑序（详情页进度排序）/ **命令栈（上限 100）** |
| 画布 | `.../components/FlowCanvas.vue` | 纯 SVG 自绘（`<rect>/<polygon>/<path>/<marker>`），pointer 拖拽 + 连线 + 缩放平移 + 校验高亮 |
| 属性面板 | `.../components/NodeProps.vue` | 按节点类型出字段（审批人来源 10 种 / 会签 / 网关 / 超时 / 抄送人落行） |
| 只读预览 | `.../components/FlowPreview.vue` | 复用同一画布（强制只读），发起页预览用 |
| 进度条 | `.../components/FlowProgress.vue` | 横向步骤条：已过绿 / 当前高亮呼吸 / **未到灰** |
| 页面 | `views/business/flow/def/index.vue` | 流程定义列表（筛选 / 发布 / 停用 / 版本历史 / 删除 / 新建） |
| 页面 | `views/business/flow/def/edit.vue` | **流程编辑器**（三栏 + 撤销重做 + 校验 + 发布 + 另存草稿 + 离开拦截） |
| 页面 | `views/business/flow/start/index.vue` | 发起审批（只列 PUBLISHED + 只读预览图 + 附件 + 提交跳详情） |
| 页面 | `views/business/flow/task/index.vue` | 审批中心四 Tab（待我审批 / 我已审批 / 我发起的 / 抄送我的） |
| 页面 | `views/business/flow/instance/detail.vue` | 审批详情（进度条 + 内容 + 时间线 + 操作区 + 30s 轮询 + 撤回） |
| 路由 | `frontend/src/router/index.ts` | 新增 5 条：`/flow/def`、`/flow/def/:id/edit`、`/flow/start`、`/flow/task`、`/flow/instance/:id` |
| 权限 | `frontend/src/constants/permissions.ts` | 定义侧 5 码（V0121）+ 实例侧 5 码（V0124）全部登记 |
| 状态 | `frontend/src/constants/status-map.ts` | 五套域：`flow-def` / `flow-instance` / `flow-node` / `flow-task` / `flow-cc` |

**零新增依赖**：`package.json` / `pnpm-lock.yaml` 未改（无 bpmn-js / jsPlumb / vue-flow / d3）。

## 1. 编辑器验收（逐条真机）

复跑：`uv run --with websockets python3 evidence/d5-shots/scripts/d5_verify_editor.py`

| # | 验收项 | 结果 | 证据（真机返回值） |
|---|---|---|---|
| 1 | 新建流程 → 拖 4 节点（开始→审批→审批→结束）→ 连线 → 保存 | ✅ | `已保存 v1（4 节点 / 3 连线）`，节点 `n_start / n_1 / n_2 / n_end`，边 `e_1 / e_2 / e_3` |
| 2 | 保存后重新打开，图完整还原（位置 / 连线 / 属性） | ✅ | 刷新后 `nodes=4 edges=[e_1,e_2,e_3] same_positions=true label=主管审批` |
| 3 | 撤销 / 重做 50 步正确 | ✅ | 50 次方向键微调 → `undo=50 redo=50`，坐标 `(130,0) → (180,0) → 回 (130,0) → 重做 (180,0)` |
| 4 | 拖拽不掉帧（无 >50ms 长任务） | ✅ | `PerformanceObserver('longtask')` 抓 60 步连续拖拽：**长任务 0 条**；同段 rAF 帧间隔 avg **16.54 ms**（≈60fps） |
| 5 | 缩放 0.3–2.0、平移正常 | ✅ | 滚轮打满 `max=200% min=30%`；空白拖拽 `translate(187.1,204.0) → (307.1,144.0)` |
| 6 | 自环 / 重复边被前端拦截 | ✅ | `不能连到自己（自环）` / `这两个节点之间已经有连线了`，边数不变（3） |
| 7 | 造无 END 的图 → 校验 → 画布高亮 + 列表原因 | ✅ | 删 END 后校验：`3 条问题`（R2 缺结束节点、R5/R6 审批节点 0 出边）、`has-issue 节点 1 个`；点条目定位选中 `n_1` |
| 8 | 校验全过才允许发布；有错给出原因 | ✅ | 有错时发布按钮 `disabled` + tooltip；修复后 `校验通过（3 节点 / 2 连线）` → `已发布 v1（图 hash d298df91fd76…）` |
| 9 | 发布后状态 PUBLISHED、编辑器只读 | ✅ | 横幅「如需修改请另存草稿」、锚点 **0 个**、保存 `disabled`、徽标「已发布」、只读拖不动；另存草稿后恢复可编辑（锚点 6 个） |

复跑：`d5_verify_publish.py` → 8/8 PASS（校验 / 发布 / 只读 / 另存草稿）
复跑：`d5_verify_perf.py` → 长任务 0 条

## 2. 实例侧页面验收（D2 交付后**真实数据**跑通全链路）

复跑：`uv run --with websockets python3 evidence/d5-shots/scripts/d5_verify_instance.py`

脚本先在编辑器里把审批节点改成「审批人来源 = 发起人本人 + 自审策略 = 允许自审」（走属性面板 UI），
保存 → 校验 → 发布，然后发起一张真实审批单，走完 **发起 → 待办 → 同意 → 已通过**：

| # | 验收项 | 结果 | 证据 |
|---|---|---|---|
| 12 | 待办四 Tab 切换 + 真实数据 | ✅ | 四 Tab 渲染正常；待办 Tab 出现真实卡片 `D5 真机：报销 6800 元 FLW-20261029-1DFCB8B4 · 当前节点：n_1 · 待处理 · [同意/驳回/转办/加签]`；同意后卡片消失、toast「已同意，审批单已通过」；「我发起的」2 张、「我已审批」2 张、「抄送我的」空态 |
| 13 | 详情页进度条位置正确（3 节点走到第 2 步） | ✅ | 步骤条 `开始 已通过 / 2 审批 审批中 / 3 结束 未到达`，`activeIndex=1`（第 2 步高亮）、`doneCount=1`；操作区可见（`my_task` 存在） |
| 13+ | 审完后状态收敛 | ✅ | 徽标「已通过」、三步全 `is-done`、操作区消失、时间线有「同意（n_1 第 1 轮）」 |

> 进度条为什么能画出「未到达」：后端 `nodes[]` **只返回已到达的节点行**，所以详情页按
> `flow_code + version_no` 把实例绑定的版本整图拉回来（`/def/page` → `/def/{id}/versions` → `/version/{vid}`），
> 用 `topoOrder` 拓扑排序后与后端状态合并，图上没有状态的节点显示 `PENDING`（灰）。图拉不到时自动退化为「只用后端节点」。

## 3. 其他页面 + 部署

复跑：`uv run --with websockets python3 evidence/d5-shots/scripts/d5_verify_pages.py`

| # | 验收项 | 结果 | 证据 |
|---|---|---|---|
| 10 | 流程列表能筛选、能发布 / 停用 | ✅ | 列表 `d5_282760 │ D5 校验发布演示 │ 已发布 │ v1 │ 3 节点 / 2 连线 │ EXPENSE`；关键词 `D5` → 2 行；状态筛选「已停用」→ 只剩已停用行；发布 / 停用按钮实测生效（停用后徽标「已停用」，toast「已停用（在跑审批单不受影响）」） |
| 11 | 发起页选流程 → 流程预览正确 | ✅ | 选 `D5 校验发布演示（d5_282760）` → 预览画布 `nodes=3 edges=2`，步骤链「1 开始 → 2 审批 → 3 结束」 |
| 14 | `pnpm typecheck && pnpm build` 通过 | ✅ | 本地与 144 双跑：`vue-tsc --noEmit` 0 错误；`✓ built in 16.32s` |
| 15 | 22 套配色门禁通过 | ✅ | `node scripts/check-palettes.mjs` → `色阶算法断言 330 条，失败 0 条` / `全部通过 ✓` |
| 16 | 颜色全部主题变量（无裸 hex） | ✅ | `grep -riE '#[0-9a-f]{3,8}'` 扫 `views/business/flow/**`、`flow-api.ts`、两处 constants、`router/index.ts`：**0 命中** |
| 17 | §6.2 部署验收 | ✅ | `dist/index.html` 由 `pnpm build` 重新产出；nginx 服务 `http://172.18.10.144/`，`assets/index-B2zxzify.js` 200（104 360 B）；`assets/edit-*.js` / `FlowCanvas-*.js` 均 200 且含 `flow-editor__bar` / `flow-node__port`；真实登录 admin 后流程列表可见、编辑器可打开（见截图） |

## 4. 截图

| 文件 | 内容 |
|---|---|
| `01-流程定义列表.png` | 列表：编码 / 名称 / 状态徽标 / 当前版本 / 图规模 / 业务类型 / 操作 |
| `02-发起审批-流程预览.png` | 发起页：只列已发布流程 + 右侧只读预览图 + 步骤链 |
| `03-审批中心四Tab.png` | 四个 Tab + 真实待办卡片（含同意 / 驳回 / 转办 / 加签） |
| `04-审批详情-进度条.png` | 详情页：头部（单据号 / 流程 / 发起人 / 时间）+ 进度条（已过绿 / 当前高亮 / 未到灰）+ 时间线 + 操作区 |
| `05-流程编辑器.png` | 三栏编辑器：左节点面板 / 中 SVG 画布 / 右属性面板 |

## 5. 联调发现（D2 的 DTO 缺口，建议 D2/D6 补）

> **已闭环（2026-10-29）**：D4a 后端（`27f1fba`）补齐了下面 3 项，前端接线见
> `docs/批次D-D4a-前端接线-验证证据.md`（我发起 Tab 改打 `/instance/my`、卡片直取 4 个新字段、详情读 `form_data`）。
> 本节保留作历史记录。

1. **详情 DTO 没有 `form_data`**：`biz_flow_instance.form_data` 里存着发起时填的内容，但
   `InstanceDetailRes` 只返回 `instance / nodes / timeline / cc / my_task`。
   详情页的「审批内容」区因此改为显示「接口未下发」的显式说明（**不假装内容为空、不编内容**）。
   补字段后前端只需把 `hasFormData` 改成读 `instance.form_data`。
2. **待办 / 已办列表 DTO 没有 `flow_name` / `initiator_name` / `node_name`**：
   `TaskRes` 只有 `instance_no` + `title`。卡片上「当前节点」只能显示 `node_key`（`n_1`），
   「流程名 / 发起人」在待办 Tab 显示 `—`；点进详情才有中文节点名。
3. **没有「我发起的」专用接口**：`instance/page` 是「可见的单子」，前端按 `initiator_id` 精确过滤
   （不猜、不从别处拼数据）；等有 `/instance/my` 后换成服务端过滤更省一次全量拉取。

## 6. 与卡面口径的偏差（都在代码注释里写明原因）

1. **停用状态字面量**：卡面写 `OFFLINE`，D1 后端 `dto::DEF_STATUSES` 是 **`DISABLED`**，前端以后端为准。
2. **保存 / 校验 / 发布的路径**：文档 §10.1.1 写 `/def/{id}/graph`，D1 实际落 `PUT /version/{vid}/graph`
   + `POST /version/{vid}/validate`；另存草稿是 `POST /def/{id}/draft`。前端按 D1 实现。
3. **抄送我的路径**：架构 §10.1.4 写 `/cc/my`，D2 实际落 **`GET /business/flow/task/cc`**。前端按 D2 实现。
4. **详情出参是嵌套结构**：`{instance, nodes, timeline, cc, my_task}`，不是架构 §10.4.1 的扁平样例
   （样例里 `flow / initiator / biz / form_data / actions` 都跟实现不一致）。
5. **菜单可见性**：V0121 把 `business:flow:*` 页面节点种成 `is_visible = FALSE`；本卡按要求**不改后端迁移**，
   5 条路由已注册、可按 URL 直达，放进侧栏留给 D6 的一条 UPDATE。
6. **新建流程默认图坐标是 (0,0)**：后端 `NodeSpec::new` 默认 0，两个节点会叠成一个点。编辑器在
   「所有节点都在 (0,0)」时做一次**纯展示层**的拓扑分层排版（不写库、不进撤销栈、不标脏）。
7. **实例侧权限码只种不授**：V0124 种了 `business:flow:instance:create/read/withdraw`、`approve`、`admin`，
   但**不替任何角色授权**。非超管角色需要在角色管理里勾选后才看得到 `/flow/start`、`/flow/task`、
   `/flow/instance/:id`（路由守卫用的就是这几个码）。

## 7. 红线自查

| 红线 | 结论 |
|---|---|
| 不引重型图库 | ✅ `package.json` / `pnpm-lock.yaml` 零改动；画布是 `<rect>/<polygon>/<path>/<marker>` 自绘 |
| 颜色只用主题变量 | ✅ 裸 hex grep 0 命中；节点语义色走 `--td-success-color / --td-brand-color / --app-brand-secondary / --td-warning-color / color-mix(brand, danger)` |
| 发布后编辑器只读 | ✅ 服务端 `version.editable=false` 驱动：画布 `readonly`（不画锚点 / 拖不动 / 删不掉）+ 保存·校验·发布禁用 + 「另存草稿」出口 |
| 实例侧未就绪不许造假数据 | ✅ D2 未上线时提交如实报错、列表显示「暂无数据」；D2 上线后全部走真接口，仓库内无任何 mock 数据文件 |
| 只改 `frontend/` | ✅ 改动全部在 `frontend/`（+ 本证据文档与截图）；未碰 `backend/` |
