# E2E（Playwright 真实交互测试）

5 条关键流程，**全部打真实后端**（不 mock、不打桩），浏览器里真点、真拖、真连线。

| 文件 | 流程 | 关键交互 |
| --- | --- | --- |
| `01-login-task-list.spec.ts` | 登录 → 任务列表（+ 工作任务目录三视图） | 填表登录、点侧边栏目录「工作任务」下的子菜单「任务列表」、目录标题只展开不导航、三个子菜单各进对应视图（`/task/list` `/task/board` `/task/gantt`）、刷新后仍在线 |
| `02-task-crud.spec.ts` | 建任务 → 编辑 → 删除 | 新建弹窗（标题 / 优先级下拉 / 日期面板）、行内编辑、删除确认 |
| `03-board-drag.spec.ts` | 看板拖拽 | 五列断言、HTML5 原生拖拽 TODO→DOING（计数 + 刷新后仍在）、TODO→DONE 跳级**回弹 + 错误文案** |
| `04-flow-editor.spec.ts` | SVG 流程编辑器 | 面板 pointer 拖 3 节点进画布、拖锚点连线、保存、重开整图还原、校验、发布 |
| `05-approval.spec.ts` | 审批待办 → 同意 | 待办卡片进详情、填审批意见、同意、实例状态 APPROVED |

## 跑法

```bash
pnpm add -D @playwright/test        # 已入库，无需重复
pnpm exec playwright install chromium

pnpm test:e2e                                   # 默认打 http://172.18.10.144
E2E_BASE_URL=http://127.0.0.1:5199 pnpm test:e2e   # 打本地 vite（BACKEND_ORIGIN 指向真后端）
E2E_USERNAME=xxx E2E_PASSWORD=yyy pnpm test:e2e    # 换账号
```

- `workers: 1`：用例读的是同一份线上数据（看板列计数、待办列表），串行才稳定；
- 失败留证据：`screenshot: only-on-failure` + `trace: retain-on-failure`，报告在 `e2e-report/`；
- CI：`CI=1` 时自动重试 1 次并额外产 JUnit（`e2e-report/junit.xml`）。

## 数据与清理

- 名字统一带 `RUN`（每次运行唯一的 `e2e<base36>` 戳）与 `[E2E]` 前缀，便于检索；
- 预置数据走真接口（`support.ts` 的 `newApi()`，就是测试账号的登录态），不是插库；
- 用例结束 `afterAll` 自己清：任务 `DELETE /business/task/{id}`；流程先撤回在跑实例
  （`/instance/{id}/withdraw`，否则后端拒绝删定义）再 `DELETE /business/flow/def/{id}`；
- 审批实例**没有删除接口**，只能走到终态（APPROVED/REJECTED/WITHDRAWN）+ 流程软删；
  需要彻底清库时按 `title like '[E2E]%'` / `flow_code like 'e2e_%'` 归零。

## 已知前提

- `business:flow:*` 的菜单在生产上是 `is_visible=false`（D5 交付时按卡面用 URL 直达），
  所以流程编辑器 / 审批中心的用例**按 URL 进入**，其余交互仍是真实点击；
- `flow/def` 新建的默认图是「开始 → 结束」，用例在其上扩展；落点可能与默认节点重叠，
  故拖完节点先点一次「自动布局」再连线（否则锚点被别的节点盖住点不到）。
