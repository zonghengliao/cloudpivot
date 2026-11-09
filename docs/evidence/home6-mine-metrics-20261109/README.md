# HOME-6（我的统计卡片）验收证据 — 工作台「我的指标」四张卡

> 注意：同一天另一张卡（t_15787ddf「权限收口」）也用了 `home6-20261109` 这个目录名，
> 那目录里的内容是**铃铛 / 未读弹窗**的证据，与本卡无关。本卡证据在本目录
> `home6-mine-metrics-20261109/`。

实例：隔离后端 `127.0.0.1:18098`（debug，`NIK_PROFILE=test`）+ 库 `cloudpivot_home6`
（由 `cloudpivot_home5_v4` 克隆，自带真实审批待办）+ Vite dev `127.0.0.1:5398`
（`BACKEND_ORIGIN=http://127.0.0.1:18098`）+ Playwright Chromium（`verify.mjs`，29 条断言）。

账号：`admin / Home5@2026Pass`（SUPER_ADMIN：9 条待办 / 3 条本周已办 / 今日 1 项到期 + 1 项逾期 + 1 项今日已完成）、
`h5viewer / Viewer@2026Pass`（非超管，名下只有 1 项今日任务）。

---

## 一、交付内容

| 层 | 文件 | 说明 |
|----|------|------|
| 后端接口 | `backend/src/business/dashboard/{mod,handler,dto,model}.rs`（新） | `GET /api/business/dashboard/my-metrics`，**登录即可**（不挂权限码），固定返回 4 条 `MetricCardRes` |
| 后端计数 | `backend/src/business/task/task_model_query.rs` | 新增 `count_done_between(me, from, to)`：按 `done_at` 半开区间数「今天已完成」（附加数） |
| 模块注册 | `backend/src/business/mod.rs` | `pub mod dashboard;` + 模块表一行 |
| 前端 API | `frontend/src/api/support/dashboard-api.ts` | `fetchMyMetrics()` → `/api/business/dashboard/my-metrics` |
| 卡片组件 | `frontend/src/views/dashboard/components/MyMetricsRow.vue`（新） | 四张卡（图标 / 大数字+单位 / 标签 / 附加数）；逾期 >0 数字红 + 2px 左红边；hover 上浮；整卡是 button（键盘可达） |
| 工作台容器 | `frontend/src/views/dashboard/components/WorkbenchTab.vue` | 问候语横幅下方、两栏主体上方插入 `<MyMetricsRow />` |
| 落地页 | `frontend/src/views/business/flow/task/index.vue` | 认 `?tab=done`（切 Tab 也写回 URL）→ 工作台「本周已办」直接落「我已审批」 |
| 落地页 | `frontend/src/views/business/task/index.vue` | 认 `?mine=1`（开关与 URL 双向同步）→ 工作台「今日任务 / 逾期」直接落「我的任务」数据源 |

### 接口为什么在 `/business/dashboard`，不是卡面写的 `/support/dashboard`

四张卡的数字全部来自 `biz_flow_task` / `biz_task`，只有 `business/` 里的 flow / task 模块知道正确口径。
放进 `support/`（底座）等于让底座反向依赖二开业务表 —— 专业版 / 企业版复用底座时会立刻编译失败，
也违反 AGENTS「二开业务放 business 容器、不侵入框架底座」。语义上它与 `support/dashboard/overview`
是两套东西：overview 是**系统级**口径（超管在「系统信息」Tab 看），本接口是**身份口径**（每人看自己的）。
overview 一个字段都没动（实测 key 仍是 `user_total … op_log_week`）。

### 口径复用（不写第二套 SQL）

| 卡片 | 取数 | 与谁同口径 |
|------|------|-----------|
| 待办审批 | `flow::model::page_my_tasks(MY_TODO_STATUSES, live_only=true).total` | `/business/flow/task/todo` |
| 本周已办 | `page_my_tasks(MY_DONE_STATUSES, completed_since=本周一 00:00 CST).total` | `/business/flow/task/done?since=` |
| 今日任务 / 逾期 | `task::model::my_tasks(me)` + `bucket_of` 分桶 | `/business/task/my` 的 `counts` |
| 今日已完成（附加数） | `task::model::count_done_between(me, 今天 CST 00:00, 次日 00:00)` | 任务 `done_at` |

周起点与自然日边界都在 Rust 侧按 **CST** 算（`week_start_cst` / `cst_day_bounds_utc`）。
不用 PG 的 `date_trunc('week', now())`：容器时区是 UTC，直接用它会在北京时间周一 00:00–08:00
把上周的单子算进「本周」。

---

## 二、验收逐条对照（Playwright `29 passed / 0 failed`，`logs/playwright.txt`）

| 卡面验收 | 结果 | 证据 |
|---|---|---|
| ① 工作台顶部 4 张统计卡 | ✅ | `ui/01-admin-metrics.png`；`.mm__card` 计数 = 4，`data-metric` 依次 `my_todo_count / my_done_week / my_task_today / my_overdue`；单位均为「项」 |
| ② 数字真实（实测核对） | ✅ | 卡片 9 / 3 / 2 / 1 ↔ `/flow/task/todo` total=9、`/flow/task/done?since=2026-11-08T16:00Z` total=3、`/task/my` counts.today=1 + overdue=1；「已完成 1」= 今天 `done_at` 的 1 条（`api/api-evidence.txt`） |
| ③ 逾期 >0 数字红色 | ✅ | 数字计算色 `rgb(193,123,126)` == `--td-error-color`；卡片 `is-danger`、`border-left: 2px rgb(193,123,126)`；**逾期为 0 时数字退回中性色** `rgb(76,82,92)`（`ui/05-viewer-metrics.png`） |
| ④ 点卡片跳对应页面 | ✅ | 待办审批 → `/flow/task`（默认「待我审批」）；本周已办 → `/flow/task?tab=done` 且激活 Tab=「我已审批」（`ui/03-click-done-week.png`）；今日任务 / 逾期 → `/task/list?mine=1` 且「只看我的任务」为开（`ui/04-click-overdue.png`） |
| ⑤ 非超管也能看到（自己的数据） | ✅ | `h5viewer`：待办 0 / 本周 0 / 今日任务 1 / 逾期 0（`ui/05-viewer-metrics.png`）；admin 数字不变 → 身份口径隔离成立 |
| ⑥ 超管「系统信息」仍是系统级指标 | ✅ | `/support/dashboard/overview` key 集合未变（`user_total … op_log_week`）；工作台正文不含「系统概况 / 角色总数」 |
| ⑦ 空数据显示 0、不报错 | ✅ | `h5viewer` 待办/本周/逾期均为 0，卡片照常渲染、无错误条；仅接口失败时出窄提示 + 重试（不把故障伪装成 0） |
| ⑧ cargo test + build + push | ✅ | `logs/cargo-test-dashboard.txt`（4 passed）、`logs/cargo-test-task.txt`（240 passed）、`logs/typecheck.txt` rc=0、`logs/build.txt` rc=0、`logs/palettes.txt`（330 条色阶 0 失败） |

补充断言（同一次运行内）：控制台 0 error；`/api` 意外 4xx/5xx = 0（排除全站既有的
`/business/notice/stream` 401，那是 `EventSource ?token=` 通道的已知缺陷）。

---

## 三、复跑方法

```bash
# 1) 克隆验收库（源库不能有活动连接）
sudo -u postgres createdb -T cloudpivot_home5_v4 -O alun cloudpivot_home6

# 2) 起隔离后端（cwd 需要有 config/ 与 migrations/）
NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18098 \
NIK_DATABASE_NAME=cloudpivot_home6 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
/opt/cloudpivot/backend/target/debug/cloudpivot

# 3) Vite dev（源码即产物）
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18098 ./node_modules/.bin/vite --port 5398 --strictPort

# 4) 造数（今天到期 / 昨天到期 / 今天已完成 三条任务）+ 跑真机脚本
bash fixtures.sh
cd verify && ln -sfn /opt/cloudpivot/frontend/node_modules node_modules && node verify.mjs
```

脚本在任务 scratch（`~/.hermes/profiles/frontend-developer/cache/scratch/home6/`），仓库只留证据。
夹具标题统一 `HOME6-` 前缀，库是隔离库，可整库 drop。
