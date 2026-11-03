# 【NOTIFY-POP】新通知自动弹（在线右上角浮层 + 登录未读汇总）验收证据

日期：2026-11-03 · 卡片：`t_39cef414` · 后端：本机 `127.0.0.1:8023`（`NIK_PROFILE=test`，库 `cloudpivot_test`）
前端：`frontend` 源码 `vite build` 产物 + 一次性静态服务 + `/api` 反代（`127.0.0.1:5290`），**没有动 `/opt/cloudpivot/frontend/dist`**
浏览器：Playwright 1.64.1 / Chromium 1248（真登录、真接口、真 SSE，零拦截）

> 为什么不用仓库里跑着的 5173 dev server：那台 dev server 的模块缓存是**旧的**
> （`curl /src/layouts/BasicLayout.vue?vue&type=template` 里没有新组件标签，页面里也没有顶栏用户下拉），
> 拿它验收会验到半旧代码。故改用 `vite build` + 静态服务。

---

## 一、在线：右上角自动滑出消息列表 —— 逐条对照

| 验收项 | 实测结果 | 证据 |
|---|---|---|
| 造一条通知 | `POST /api/business/notice`（真接口，`target_type=USER`，回执 `receiver_count=1`） | `results-toast.json` → `stages.a1.sent` |
| 真机自动滑出 | 浮层在推送后 **7 ms** 可见（`pushDelayMs=7`） | `10-toast-online-single.png` |
| 列表显示标题+时间+图标 | 首行文本 `[NP] 采购单 PO-20261103-000011 审批通过 待办 刚刚`，图标 `t-icon-task`（`notice_type=TODO`）+ `--success`（`level=SUCCESS`） | `stages.a1.firstItemText/iconClass` |
| 位置不遮顶栏 | `box = {x:1124, y:64, width:300, height:170}`；视口 1440×900 ⇒ `right=16px`、`top=64px`，顶栏高 56px ⇒ **不压铃铛/头像** | `stages.a1.box` |
| z-index | `2000`（TDesign `t-dialog` 2500 之上不可能遮住模态框；普通内容之上） | `stages.a1.zIndex` |
| 200ms 滑入过渡 | `.notice-toast-enter-active{transition:transform .2s ease,opacity .2s ease}` | `23-toast-css.txt` |
| 8 秒自动收起 | `visibleMs=7981`（另一轮 7973），随后 `hidden=true` | `11-toast-auto-closed.png` + `stages.a2` |
| 悬停不收起 | 鼠标移入浮层中心 → 等 **10 s** 仍可见；移开鼠标 → 收起 | `12-toast-hover-pause.png` + `stages.a3` |
| 点条目跳转带 focus | `/business/notice/message?focus=32dba996-3c70-4a4a-a62d-5fc405bc2d03`，浮层已收起 | `14-toast-click-item-focus.png` + `stages.a5` |
| 点「查看全部」 | `/business/notice/message`（`focus=null`），浮层已收起 | `stages.a6` |
| 点 [×] | 收起 | `stages.a7` |
| 连发 3 条合并 | 浮层实例数 **1**、标题 **「新消息 3 条」**、3 行，图标分别是 `error-circle / task / notification`（ALERT/TODO/CIRCULAR） | `13-toast-merged-3.png` + `stages.a4` |
| 铃铛不受影响 | 点铃铛 → 面板可见、5 条未读、角标 7 | `15-bell-still-works.png` + `stages.a8` |

悬停验证方法（可复现）：`page.mouse.move(浮层中心)` → `waitForTimeout(10000)` → `isVisible()` 仍为 `true`；
再 `page.mouse.move(20,700)` 移出 → `hidden`。计时器内部用的是「剩余毫秒 + 暂停/续跑」，不是重建计时。

---

## 二、登录：屏幕中间弹未读汇总 —— 逐条对照

| 验收项 | 实测结果 | 证据 |
|---|---|---|
| 有未读登录就弹 | `admin` 登录 → 标题「你有 21 条待处理」，四行：站内信 **11 条** / 公告 **1 条** / 工作任务 **1 条** / 审批待办 **8 条** | `01-login-dialog-admin.png` + `results-login-dialog.json` |
| 居中、480px | `dialogWidth=480`，`x+width/2 = 720` = 视口半宽 ⇒ 水平居中 | `results-login-dialog.json` → `stages.A_dialogShown` |
| 每行 [查看] 能跳 | 站内信 → `/business/notice/message`；公告 → `/announcement`；工作任务 → `/task/list`；审批待办 → `/flow/task`（4 条全命中） | `02-login-dialog-view-notice.png` + `stages.B_nav_*` |
| 点「知道了」关闭 | 关闭后 `sessionStorage.login-unread-shown = '1'` | `stages.C_closeAndReload` |
| 刷新不再弹 | `reload()` 后 `visibleAfterReload=false` | `03-login-dialog-after-reload.png` |
| 退出登录清标记 → 再登录又弹 | 退出后标记 `null`，`np_admin2` 重新登录 → 又弹（1/0/0/0） | `04-login-dialog-admin2.png`、`05-login-dialog-after-relogin.png` + `verify-login-dialog-2` 结果 |
| 全部已读/全空不弹 | 新账号 `np_fresh`（四类实测 0/0/0/0）登录 → **不弹**，且标记已写（刷新也不会再打 4 个接口） | `06-login-dialog-empty-skip.png` + `stages.e` |

---

## 三、代码质量

| 项 | 结果 | 证据 |
|---|---|---|
| `pnpm typecheck`（`vue-tsc --noEmit`） | 0 错误（输出 0 字节） | `21-typecheck.txt` |
| `vite build` | 成功（exit=0） | `22-build.txt` |
| 新增单测 | **22 条全绿**（累积/上限/去重、脏数据、自动收起、悬停暂停、重计时、cancel、SSE 帧跨 chunk、心跳注释、时间水位过滤、4 类汇总、单类失败按 0、标记生命周期…） | `20-unit-tests.txt` |
| 全量前端单测 | **575 条全绿**（无 skip/ignore） | `25-full-unit-tests.txt` |
| 新依赖 | 无（只用现有 TDesign 组件 + 原生 `fetch/ReadableStream`） | `git diff package.json` 为空 |

---

## 四、实测中发现的问题（都已在本卡内处理或记录）

1. **SSE 的 `?token=` 通道恒 401（全局既有缺陷）**
   `GET /api/business/notice/stream?token=<有效令牌>` → `401 未授权访问，请先登录`（认证层 `NIK_AUTH_QUERY_TOKEN_PATHS` 默认空）。
   ⇒ 浏览器 `EventSource` 永远连不上，只有 30 秒轮询，"自动弹"最慢要等半分钟。
   **处理**：`EventSource` 一行未改（仍是首选通道），追加**带 `Authorization` 头**的 `fetch` + `ReadableStream` 通道
   （后端 handler 文档里点名的推荐姿势）。实测该通道 `200`，推送 **7 ms** 到达。
   证据：`results-toast.json` → `streamResponses`（`401 …/stream?…` 与 `200 …/stream` 各 3 条，一一对应三次页面加载）。
   建议另开卡让后端把该路径加进查询令牌白名单，前端这条兜底即可退役。

2. **卡面写的 `GET /business/flow/task/page?status=PENDING` 不存在**（实测 `404 请求的资源不存在`）
   ⇒ 审批待办改用 `GET /business/flow/task/todo?page=1&page_size=1` 取 `total`（后端语义就是「只返回 PENDING 的 APPROVAL+RESUBMIT」）。

3. **未读接口返回结构**：`notice/unread-count` → `{unread_count}`；`announcement/mine/unread-count` → `{announcement_unread}`（**不是** `unread_count`）。

4. **工作任务没有独立 count 接口**，用 `task/page?status=TODO&owner_id=<我>&page_size=1` 的 `total`。
   `owner_id` 不能省：超管数据范围是全量，不带它会把别人的 TODO 也算进「我的工作任务」。

5. **第一轮真机验出「新消息 5 条」却全是旧未读**（本轮实测记录在 `stages.a1` 的旧版输出里）：
   「直接入队」与「拉最新 5 条」两条通道叠加时，后者把 5 条历史未读也当成新消息。
   **处理**：拉取侧加时间水位（只收 `created_at >= 上次刷新 - 5s`），直接推送成功时把未读基线同步 +1。
   已加两条单测锁死（`filterArrivedSince`）。修后同一场景标题为 **「新消息 1 条」**。

---

## 五、改动清单

```
frontend/src/utils/notice-popup.ts        新增：NoticeAccumulator（累积/上限5/去重）、AutoCloseTimer（悬停暂停）、
                                                 formatRelativeTime、noticeTypeMeta、filterArrivedSince
frontend/src/utils/login-unread.ts        新增：4 类未读采集（allSettled 容错）、汇总、sessionStorage 标记
frontend/src/utils/notice-stream.ts       新增：带 Authorization 头的 SSE 读取器 + 跨 chunk 帧解析
frontend/src/stores/notice.ts             latestItems/newBatchCount/popupSeq/pushNotice/pullLatest/resetPopupBatch；
                                                 SSE 补 addEventListener('notice')（原来只写 onmessage，自定义事件收不到）；
                                                 轮询兜底按「未读上涨 + 时间水位」触发浮层；带头流通道 + 重连
frontend/src/layouts/components/NoticeToast.vue      新增：Teleport 到 body 的右上角浮层
frontend/src/layouts/components/LoginUnreadDialog.vue 新增：屏幕居中 480px 未读汇总（4 行 + 知道了）
frontend/src/layouts/BasicLayout.vue      挂载两个组件（全站一次），onMounted 触发登录汇总检查
frontend/src/stores/user.ts               logout 清 `login-unread-shown` 标记
frontend/src/api/business/notice/index.ts 新增 buildNoticeStreamUrl()（不带令牌的地址）
frontend/tests/unit/notice-popup.test.ts  新增 14 条
frontend/tests/unit/login-unread.test.ts  新增 8 条
```

`NoticeBell.vue` **未改动**；SSE 连接/降级的主体逻辑未重写；没有用 `window.open`、没有用浏览器原生 Notification。

---

## 六、造数与清理

- 造数走真接口：`POST /api/business/notice`、`POST /api/business/announcement` + `/publish`、`POST /api/business/task`、`POST /api/system/user`（两个临时账号）。
- 收尾全部软删并复核（`is_deleted / deleted_at` 置位，对外不可见）：

```
 notice_total 17 | notice_visible 0 | receiver_visible 0
 task_visible 0  | ann_visible 0     | user_visible 0
```

（`sys_notice` 的软删会同时把 `sys_notice_receiver.is_deleted` 置位，所以未读数不会被残留行污染。）

---

## 七、限制与未做

- 视觉模型配额耗尽（`vision_analyze` 429），截图**没有**用人眼描述，改用 DOM 硬断言 + PNG 像素统计（`24-pixel-probe.txt`：
  浮层区域边缘密度 6.09%~8.21%，收起后同区域 1.35%，与「画了/没画」一致）。
- 登录接口有 10 次/60s 限流（`10005`，窗口内不会自解封），验收中途被限流一次；改用两个**全新账号**（`np_admin2` / `np_fresh`）
  完成「退出登录→重新登录又弹」与「全空不弹」两轮，未重启共享后端。
- 四类中「0 条」的行保留展示但置灰、「查看」置灰（避免点进空页面）；本卡验收用真实数据凑齐 4 类非 0（11/1/1/8）。
- 浮层不做「点外部关闭」：卡面只要求 [×] / 8 秒 / 点条目三种收起路径，避免和铃铛面板争抢点击。
