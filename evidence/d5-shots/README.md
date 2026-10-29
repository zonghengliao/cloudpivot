# D5 前端真机截图（批次 D · 审批中心 + SVG 流程编辑器）

全部截图来自 144 生产部署（nginx → `/opt/cloudpivot/frontend/dist`）的真实登录会话（admin），
headless Chromium 1280×633，未经 PS / 未改数据；审批单是脚本真实发起并审批的。

| # | 文件 | 页面 | 看什么 |
|---|---|---|---|
| 1 | `01-流程定义列表.png` | `/flow/def` | 表格列（编码 / 名称 / 状态徽标 / 当前版本 / 图规模 / 业务类型 / 操作）；操作含编辑·发布·停用·版本历史·删除 |
| 2 | `02-发起审批-流程预览.png` | `/flow/start` | 只列 `PUBLISHED` 流程；右侧只读预览图（复用编辑器画布）+ 步骤链「开始 → 审批 → 结束」 |
| 3 | `03-审批中心四Tab.png` | `/flow/task` | 四个 Tab + **真实待办卡片**（标题 / 单据号 / 当前节点 / 到达时间 / 等待时长 / 同意·驳回·转办·加签） |
| 4 | `04-审批详情-进度条.png` | `/flow/instance/{id}` | 头部（单据号 / 流程 / 发起人 / 时间）+ 进度条（**已过绿 / 当前高亮 / 未到灰**）+ 时间线 + 右侧操作区 |
| 5 | `05-流程编辑器.png` | `/flow/def/{id}/edit` | 三栏：左节点面板（7 种节点）/ 中 SVG 画布（开始 → 主管审批 → 结束）/ 右属性面板 |

## 复跑脚本

`scripts/` 下是产生上面结论的 CDP 驱动（需要一台起过 headless Chrome 的机器）：

```bash
uv run --with websockets python3 scripts/d5_verify_editor.py    # 验收 1–6（编辑器：拖拽 / 连线 / 保存还原 / 撤销重做 / 缩放平移 / 自环重复边）
uv run --with websockets python3 scripts/d5_verify_publish.py   # 验收 7–9（校验高亮 / 发布 / 只读 / 另存草稿）
uv run --with websockets python3 scripts/d5_verify_pages.py     # 验收 10–11 + 17（列表筛选停用 / 发起页预览 / 部署指纹）
uv run --with websockets python3 scripts/d5_verify_instance.py  # 验收 12–13（真实审批单：发起 → 待办 → 同意 → 已通过）
uv run --with websockets python3 scripts/d5_verify_perf.py      # 验收 4（长任务 / 帧率）
```

`d5_cdp.py` 是公共部分：定位 `DevToolsActivePort` → 连 CDP → 用
`Page.bringToFront` + `Page.setWebLifecycleState('active')` 规避隐藏标签页导致的
Transition / 路由假死；登录密码只从 `/etc/cloudpivot/cloudpivot.env` 读进内存，不打印。

`d5_verify_instance.py` 会先在编辑器里把审批节点的「审批人来源」改成「发起人本人」并把
「自审策略」设为「允许自审」（走属性面板 UI，不是改库），这样单子会停在第 2 步等我审 ——
正好用来验「3 节点流程走到第 2 步」的进度条位置。
