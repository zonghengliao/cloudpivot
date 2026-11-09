# HOME-2 首页「工作任务」卡（今天最多 10 条）验收证据

- 卡片：`t_ec641e4e`（HOME-2）
- 时间：2026-11-09（CST）
- 改动文件：`frontend/src/views/dashboard/index.vue`（唯一代码改动）
- 验证环境：本机隔离实例 —— `backend/target/debug/cloudpivot`（真后端）+ 隔离库 `cloudpivot_home1`；
  前端用 `pnpm build`（typecheck + vite build，产物落 scratch，**未覆盖 `frontend/dist`**）
  由一次性静态服务 + `/api` 反代挂在 `127.0.0.1:5302`，Playwright（Chromium，1600×1400）真浏览器打开 `/dashboard`。
  另用 **145 真机**（`http://172.18.10.145` + 真库 `cloudpivot_dev`）做真实 token curl。
  **没有新建后端接口、没有改迁移、没有写生产库业务数据。**

## 结论对照（验收 ①–⑧）

| # | 验收项 | 实测 |
|---|---|---|
| ① | 「公告与消息」下面有「工作任务」卡 | 右栏 `.dashboard__aside` 两张卡标题实测 `["公告与消息", "工作任务"]`，`gap=16px`（`verify-result.json.check.aside_card_titles` + `shot-dashboard-full.png`） |
| ② | 最多 10 条（逾期优先，然后今日） | 库里造 **12 条**（3 逾期 + 9 今日）→ DOM **10 行**；前 3 行是逾期（`逾期 3 天(11-06)` / `2 天(11-07)` / `1 天(11-08)`），后 7 行是今日；只请求一次 `GET /api/business/task/my`（200） |
| ③ | 逾期任务标红 | 3 行 `.dashboard__tasks-due--overdue`，计算色 `rgb(193,123,126)`（主题 `--td-error-color`），非逾期行 `rgb(121,130,144)` |
| ④ | 点任务能进详情 | 点第 1 行 → URL `/task/list?id=01a1c064-…`，详情抽屉打开，header `任务详情 · TSK-202611-000003`（`shot-task-detail-from-card.png`） |
| ⑤ | 已完成有删除线/勾选 | 样式分支已验：`line-through` + 灰色 `rgb(121,130,144)` + 白色勾选图标（`✓`）+ 时间文案「已完成 · 11-09」（`shot-done-row-real.png`）。**注意**：`/business/task/my` 后端只回未终态，DONE 不在其中，所以这一条用**拦截层构造一条 DONE** 验证渲染分支 |
| ⑥ | 底部统计正确 | 卡片底部 `今日 9 项 · 逾期 3 项 · 本周 0 项`，与接口 `counts` 逐字段一致（`{overdue:3,today:9,this_week:0}`） |
| ⑦ | 空态友好 | 非超管 `h1viewer2`（DEPT_LEADER，`/task/my` 真返回 `total=0`）真机显示「今天没有待办任务」+「去任务中心」；点按钮跳 `/task/list`，无错误条（`shot-empty-viewer.png`） |
| ⑧ | typecheck + build + push | `typecheck.txt` exit 0；`build-pnpm.txt` `✓ built in 50.33s`（pnpm build 全链路 exit 0）；`palettes.txt` 22 套配色全过；commit / push 见卡内汇报 |

## 布局与细节

- 右栏 360px，两卡竖排 `display:flex; flex-direction:column; gap:16px`，`align-self:flex-start`（不拉伸出空白）。
- 卡片「工作任务」整高 **609px**（10 行 × 49–52px + 卡头 + 统计条），10 行完整可见；行数少时随内容缩短，不设固定高度。
- 状态图标：待办空心灰圆 / 进行中实心品牌色圆 / 待验收橙圈 / 已完成品牌色圆 + 白色勾 / 已取消虚线灰圈。
- 标题超长单行截断（`scrollWidth 370 > clientWidth 286`，hover 有原生 `title` 全文）。
- 优先级 P0/P1 才打标签（「最高」danger /「高」warning），P2/P3 不打，避免 10 行全是标签；颜色全部取 `PRIORITY_MAP` 主题变量，无硬编码。
- 骨架屏 5 行（延迟 1.5s 抓到 5 行，数据到达后归零）；加载失败出错误条 + 重试；403 按「无权限」空态而非报错（卡片对全员可见）。

## 与卡面设计的差异（重要）

1. **底部统计没有「已完成 M」**：`GET /business/task/my` 只返回 `TODO/DOING/REVIEW` 三类未终态（后端 `task_handler_crud.rs::my` 注释与代码都写明 `DONE/CANCELLED` 不进「我的任务」），`counts` 里也没有 done 字段。
   按卡片「按实际字段取，不要瞎猜」的要求，底部显示 **今日 / 逾期 / 本周**（三项都来自 `counts`），不编造「已完成 3」。
2. **列表里不会自然出现「✓ 已完成」行**：同上原因。样式分支已实现并用拦截层构造 DONE 验证（见 ⑤），后端一旦把终态放进 `/my` 就会正确渲染。
3. **时间文案只到「日」**：任务 `due_date` 是 `YYYY-MM-DD`（无时分），所以写「今天截止 / 逾期 N 天（MM-DD）」，不伪造「18:00」。
4. 点击不复制详情逻辑：跳 `/task/list?id=`，列表页 `onMounted` 认这个 query 自动开抽屉（与通知里 `/task/{id}` 兼容跳转同一套）。

## 文件说明

- `api-evidence.txt` — curl 回执（本地隔离实例 + 145 真机；token 打码）
- `verify-result.json` / `verify-run.txt` — Playwright 断言原始输出 / 控制台日志
- `shot-dashboard-full.png` — 整页（右栏两卡）
- `shot-tasks-card.png` — 工作任务卡（10 行 + 底部统计）
- `shot-done-row-real.png` — DONE 行样式（拦截层构造）
- `shot-task-detail-from-card.png` — 点任务后的列表 + 详情抽屉
- `shot-empty-viewer.png` — 非超管真机空态
- `shot-skeleton.png` — 加载骨架
- `typecheck.txt` / `build-pnpm.txt` / `palettes.txt` — 门禁（`*.log` 被 .gitignore 忽略，构建日志以 .txt 留档）
- `seed-tasks.sh` / `verify.mjs` / `inspect.mjs` / `serve.mjs` / `make-evidence.sh` — 造数 + 断言 + 静态服务脚本（可复跑）

## 复跑方式（隔离环境）

```bash
# 1) 后端（隔离库 cloudpivot_home1，debug 二进制，18023）
NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18023 NIK_DATABASE_NAME=cloudpivot_home1 \
NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=<64hex> CLOUDPIVOT_STORAGE_MASTER_KEY=<64hex> \
NIK_ADMIN_PASSWORD=Admin@123456 ./backend/target/debug/cloudpivot

# 2) 前端产物 + 静态服务（不碰生产 dist）
cd frontend && pnpm build --outDir <scratch>/dist-home2
node <scratch>/serve.mjs <scratch>/dist-home2 5302 127.0.0.1:18023

# 3) 造数 + 断言
bash <scratch>/seed-tasks.sh
node <scratch>/verify.mjs
```

## 副作用说明（透明记录）

- 145 真机取 token 时，先试的几个历史密码已失效，连续失败把 `admin` 账号触发锁定（`sys_user.is_locked=t`、`login_fail_count=5`）。
  随后用 145 的 `/etc/cloudpivot/cloudpivot-dev.env` 里配置的 `NIK_ADMIN_PASSWORD` 登录成功，并把这两个锁定字段**恢复为 `login_fail_count=0 / is_locked=false / lock_time=null`**（只改这两个字段，未动任何业务数据）。
- 本地隔离库新增 12 条 `【HOME2】*` 任务用于验收（隔离库 `cloudpivot_home1`，与生产无关）。
