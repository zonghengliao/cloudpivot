# HOME-4 验收证据 — 首页架构：超管双 Tab（系统信息 / 工作台）+ 非超管单工作台

实例：隔离后端 `127.0.0.1:18094`（debug，profile=test）+ 空库 `cloudpivot_home4_v1`（首启自动迁移 + seed）
+ Vite dev `127.0.0.1:5400`（`BACKEND_ORIGIN` 指向 18094）+ Playwright Chromium。

账号：`admin / Home4@2026Pass`（SUPER_ADMIN）、`h4viewer / Viewer@2026Pass`（EMPLOYEE，非超管）。

---

## 一、交付内容

| 层 | 文件 | 说明 |
|----|------|------|
| 首页容器 | `frontend/src/views/dashboard/index.vue` | 超管 → 两个 Tab（默认「工作台」）；非超管 → 直接 `<WorkbenchTab />`。整页从 1799 行降到 90 行 |
| 工作台 Tab | `frontend/src/views/dashboard/components/WorkbenchTab.vue`（新） | ① 问候语条（副标题带真实任务计数）② 工作任务卡 ③ 公告与消息卡 ④ 快捷入口 + 两个详情抽屉 |
| 系统信息 Tab | `frontend/src/views/dashboard/components/SystemInfoTab.vue`（新） | ① 健康摘要 ② 系统概况 8 指标 ③ 登录趋势 / 登录耗时趋势 |
| 健康摘要卡 | `frontend/src/views/dashboard/components/HealthSummaryCard.vue`（新） | CPU / 内存 / 磁盘 / 服务 4 指标 + 阈值配色 + 「查看详情 →」跳 `/system/server-status` |
| 角色判定 | `frontend/src/stores/user.ts` | 新增 `isSuperAdmin`（`roles.includes('SUPER_ADMIN')`，与后端 `is_super_admin` 同口径） |

> HOME-1（公告与消息卡）与 HOME-2（工作任务卡）原本写在 `index.vue` 里，本次整体搬进
> `WorkbenchTab.vue`（**不是重写**：模板、样式、交互、注释一律照搬，只在类名前缀上从
> `dashboard__feed-*` 改为 `wb__feed-*`，scoped 样式语义不变）。

---

## 二、验收逐条对照（自动化：`43 passed / 0 failed`）

| 卡面验收 | 结果 | 证据 |
|---|---|---|
| ① 非超管只有工作台（问候语 / 工作任务 / 公告与消息 / 快捷入口），**看不到趋势图** | ✅ | `ui/06-viewer-workbench.png`；断言 `canvas=0`、`.health__grid`/`.si__metric` 均为 0 |
| ② 超管两个 Tab，**默认「工作台」** | ✅ | `.t-tabs__nav-item` = `["系统信息","工作台"]`，`.t-is-active` 文本含「工作台」 |
| ③ 超管「系统信息」= 健康摘要 4 指标 + 登录趋势 + 延迟趋势 + 系统概况 | ✅ | `ui/02-admin-system-info.png`；4 个 `.health__item`、8 张 `.si__metric`、2 个 ECharts canvas |
| ④ 超管 Tab2 与非超管首页**完全一致（同一组件）** | ✅ | 两处 `.wb` 的 banner/tasks/feed 结构 + 卡片标题序列逐一相等；代码上就是同一个 `<WorkbenchTab />` |
| ⑤ 健康摘要「查看详情」→ `/system/server-status` | ✅ | `ui/03-server-status-from-health.png`；URL 落到 `/system/server-status` 且 `.ss-hero__title` 出现 |
| ⑥ Tab 切换不重复请求（懒加载） | ✅ | 首屏 0 次 overview / server-status；首次切到「系统信息」各 +1；再切走切回**不再增加** |
| ⑦ 非超管控制台无 403（根本没请求超管接口） | ✅ | 非超管 403/401 响应 = `[]`，console error = 0；请求清单见 `api/report.json` |
| ⑧ typecheck + build + push | ✅ | `logs/typecheck.txt`、`logs/build.txt`、`logs/palettes.txt` 全部 exit 0 |

---

## 三、复跑方法

```bash
# 1) 建库 + 起隔离后端（18094）
sudo -u postgres createdb -O alun cloudpivot_home4_v1
cd backend && NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18094 \
  NIK_DATABASE_NAME=cloudpivot_home4_v1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_ADMIN_PASSWORD='Admin@123456' NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) ./target/debug/cloudpivot

# 2) 改超管口令（首登强制改密）+ 建非超管账号 h4viewer
bash docs/evidence/home4-20261109/scripts/setup-accounts.sh
# 3) 演示数据：3 条任务（逾期/今天/下周）+ 2 条公告（发布即扇出站内信）
bash docs/evidence/home4-20261109/scripts/seed-demo.sh

# 4) 前端 dev 指向隔离后端
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18094 pnpm dev --port 5400 --strictPort
# 5) UI 验收（7 张截图 + 43 条断言）
node docs/evidence/home4-20261109/scripts/verify-ui.mjs
```

---

## 四、偏差与取舍（明写，不藏）

1. **`isSuperAdmin` 是新增的 store 字段**。卡面写「现有 `userStore.isSuperAdmin`（见原 624 行附近）」，
   但实测仓库里**没有**这个字段（`grep -rn isSuperAdmin frontend/src` 零命中）。
   按「从 roles 判断」的指示补在 store 里，只认后端下发的 `SUPER_ADMIN` 角色码，
   与后端 `model::is_super_admin` 逐字同口径，没有引入第二套判定。
2. **原首页的 8 张指标卡整体搬进「系统信息」Tab（标题「系统概况」）**。卡面 ④ 写
   「今日系统概况（4 个数字）」；但 `overview` 下发的 12 个指标**全是全系统口径**，
   其中「今日登录 / 在线用户 / 登录失败率 / 启用任务」已经包含在原有 8 张卡里。
   若再单独渲染 4 个数字，同一屏会出现两遍同样的值。故按「现有字段能取到什么显示什么」，
   用 8 张卡承载（是卡面 4 项的超集），并在小节标题写了口径说明。
3. **「服务 4/5」实为 `x/3`**。后端 `ServerStatusRes` 只有 3 个可判定探针：
   数据库 / Redis / Hermes Agent。凭空补 2 项就是假数据，故按真实探针渲染；
   `Hermes not_installed` 是正常状态（未部署），只在脚注写「未安装」并降级为中性色，不判红。
   实测本机 profile=test 未启 Redis → 显示 `1/3` + 脚注「Redis 不可用 · Hermes Agent 未安装」（红），
   这是**真实**状态，不是渲染错误。
4. **TDesign 1.19 的 `tab-panel` 默认 `destroyOnHide: true`**（切走即销毁 → 切回重新请求）。
   首版只加了 `lazy`，实测「切回系统信息」会再发一轮请求（overview 2→3）。
   已显式 `:destroy-on-hide="false"`：首次切到才挂载，之后常驻。
5. **公告 / 站内信按权限发请求**。EMPLOYEE 角色没有 `business:announcement:read` /
   `business:notice:mine` 时，原实现会硬发 4 个注定 403 的请求（控制台一片红）。
   现改为「没权限就不发」，空态写「当前账号没有公告/消息查看权限」。
   `api/report-viewer-noperm.json` 是**未授权前**那一轮的留证：
   首页接口 0 个 403（公告/消息接口已不再请求），仅剩顶栏铃铛的
   `/notice/stream`、`/notice/unread-count`、`/announcement/unread-count` 三条 —— 那是
   `BasicLayout` / `login-unread.ts` 的既有行为，不在本卡范围（已另开卡跟进）。
6. **快捷入口新增 4 个业务入口**（任务中心 / 审批中心 / 项目 / 我的报销）。
   卡面说「快捷入口（现有的，保留）」——原有的 10 个入口全是 `system:*` / `support:*`，
   实测 EMPLOYEE 角色一个都没有 ⇒ 非超管首页的「快捷入口」是**整卡空态**（截图见首轮报告）。
   补 4 条业务动线（权限码与 `router` meta / `sys_menu.auth_code` 逐字一致）后全员都有入口。
   列数改为 `min(5, 可用入口数)`：4 个入口按 5 列排会在右边空一格。
7. **工作台布局是「左宽栏 + 右 360px 栏」**（左：工作任务 → 快捷入口；右：公告与消息），
   不是四条上下堆叠。原因：公告卡 5+10 行本来就矮而长，占满通栏会非常难看；
   且 HOME-5「待我审批」每行三个按钮需要宽栏，已在 `WorkbenchTab.vue` 里标好插入点注释。
8. **Tab 不做 localStorage 记忆**（卡面明确要求每次默认工作台）。

---

## 五、文件清单

```
ui/01-admin-workbench.png              超管默认「工作台」1680×1050
ui/02-admin-system-info.png            超管「系统信息」（健康摘要 / 系统概况 / 两图）
ui/03-server-status-from-health.png    「查看详情 →」落到服务器状态页
ui/04-admin-workbench-1360.png         1360 宽
ui/05-admin-workbench-960.png          960 宽（Tab 横排、内容单列）
ui/06-viewer-workbench.png             非超管首页（无 Tab、无趋势图）
ui/07-viewer-960.png                   非超管 960 宽
api/report.json                        43 条断言 + 请求清单 + 健康摘要实测值
api/report-viewer-noperm.json          未授权版非超管（证明「没权限就不发请求」）
api/admin-me.json / viewer-me.json     /auth/me 回执（roles / permissions 对照）
api/my-tasks.json                      /business/task/my 回执
logs/{typecheck,build,palettes,verify-ui}.txt
scripts/{setup-accounts.sh,seed-demo.sh,verify-ui.mjs}
```
