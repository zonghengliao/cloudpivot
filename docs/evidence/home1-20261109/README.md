# HOME-1 首页「服务器状态卡 → 公告与消息卡」验收证据

- 卡片：`t_a340af6d`（HOME-1）
- 时间：2026-11-09（CST）
- 改动文件：`frontend/src/views/dashboard/index.vue`（唯一改动）
- 验证环境：本机隔离实例 —— `backend/target/debug/cloudpivot`（真后端）+ 隔离库 `cloudpivot_home1`，
  前端用 `vite build` 产物（`dist-home1/`，入包入口 `assets/index-*.js` 见 `build-summary.txt`）
  由一次性静态服务 + `/api` 反代挂在 `127.0.0.1:5301`，Playwright（Chromium，1600×1000）真浏览器打开 `/dashboard`。
  **没有覆盖 `/opt/cloudpivot/frontend/dist`（生产 nginx 目录），也没有写生产库。**

## 结论对照（验收 ①–⑧）

| # | 验收项 | 实测 |
|---|---|---|
| ① | 右栏显示「公告与消息」卡 | `cardHeader = "公告与消息"`，右上角两个入口「公告全部 / 消息全部」 |
| ② | 公告 ≤5、消息 ≤10（不多不少） | 库里 7 条公告 / 19 条消息 → DOM `annCount=5`、`msgCount=10`；请求参数 `page_size=5` / `page_size=10` |
| ③ | 未读消息有红点 | `unreadDots=10`、`unreadRows=10`（10 条全未读时） |
| ④ | 点公告进详情、点消息进详情并标已读 | 公告：`GET /api/business/announcement/{id}` 200 + `POST …/read` 200；消息：`PUT /api/business/notice/read/{receiver_id}` 200，红点 10→9、统计条「未读 10」→「未读 9」 |
| ⑤ | 空数据显示空态、不报错 | 真机：非超管 `h1viewer2` 消息区显示「暂无消息」；拦截层（两条接口都返回空）显示「暂无公告」「暂无消息」，统计条「公告 0 条 · 消息 0 条」 |
| ⑥ | 非超管也能看到这张卡 | `h1viewer2`（仅 `DEPT_LEADER` 角色，非超管）首页卡片可见、公告 5 条、无 403 |
| ⑦ | 首页其他位置不受影响 | `dashboard__metric` 8 个、快捷入口在、页面全文不再出现「服务器状态」 |
| ⑧ | typecheck + build | `typecheck.txt`（exit 0）、`build-summary.txt`（`✓ built in …` + 入包 hash）、`palettes.txt`（22 套配色全过） |

布局：`.dashboard__aside` 实测 `width=360`、`align-self=flex-start`；卡片内容高度实测 **608px**（5 公告 + 10 消息，随内容自适应，不设固定高度）。

骨架屏：拦截两条接口延迟 1.5s 返回，首屏渲染出 12 行 `.dashboard__feed-skeleton-row`（`shot-skeleton.png`），数据到达后骨架移除。

## 文件说明

- `api-evidence.txt` — curl 真实回执（公告/消息分页、未读数、非超管可见性、已读幂等）
- `verify-result.json` — Playwright 断言原始输出（含抓到的每个 `/api` 请求 URL 与状态码）
- `verify-run.txt` — 同一次的完整控制台日志
- `shot-admin-dashboard.png` — 超管首页（右栏新卡）
- `shot-announcement-drawer.png` — 公告详情抽屉（Markdown 渲染）
- `shot-notice-drawer.png` — 消息详情抽屉
- `shot-viewer-dashboard.png` — 非超管首页（卡片可见 + 消息空态）
- `shot-skeleton.png` / `shot-empty.png` — 骨架屏 / 双空态
- `typecheck.txt` / `palettes.txt` / `build.txt` / `build-summary.txt` — 门禁
- `seed.sh` / `newann.sh` / `create-viewer2.sh` / `verify.mjs` — 造数与断言脚本（可复跑）

## 复跑方式（隔离环境）

```bash
# 1) 后端（隔离库 cloudpivot_home1，debug 二进制）
NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18023 NIK_DATABASE_NAME=cloudpivot_home1 \
NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=<64hex> CLOUDPIVOT_STORAGE_MASTER_KEY=<64hex> \
NIK_ADMIN_PASSWORD=Admin@123456 ./backend/target/debug/cloudpivot

# 2) 前端产物 + 静态服务（不碰生产 dist）
cd frontend && ./node_modules/.bin/vite build --outDir <scratch>/dist-home1 --emptyOutDir
node <scratch>/serve.mjs ./dist-home1 5301 127.0.0.1:18023

# 3) 造数 + 断言
bash <scratch>/seed.sh && bash <scratch>/newann.sh && bash <scratch>/create-viewer2.sh
node <scratch>/verify.mjs
```

## 与卡面写法的差异（重要）

- 卡面写「全部 → `/system/announcement` / `/system/message`」，**这两个路由不存在**。
  实际公告中心是 `/announcement`、消息中心是 `/business/notice/message`（`router/index.ts`），
  卡片入口按实际路由落地，并已在真机点通（`verify-result.json.navigation`）。
- 底部统计条的「未读」取**本卡展示的 10 条里未读的条数**（不是全量未读），否则会出现
  「消息 10 条（未读 17）」这种自相矛盾的文案；全量未读由顶栏铃铛负责，站内信未读数的加减仍同步 notice store。
- 空态/骨架/两个「全部」入口都在真机上验过；其中「两区都空」这一条是**拦截层**（接口返回空 body）验证，
  真机上用非超管账号验的是「消息区为空」这一半。
