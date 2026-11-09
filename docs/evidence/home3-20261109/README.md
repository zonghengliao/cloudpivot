# HOME-3 验收证据 · 系统管理 → 服务器状态（2026-11-09）

卡：`t_92150b59`（前端开发工程师）
实例：隔离后端 `127.0.0.1:18093`（debug）+ 空库 `cloudpivot_home3_v1` + Vite dev `:5399` + Playwright Chromium

---

## 一、交付内容

| 层 | 文件 | 说明 |
|----|------|------|
| 页面 | `frontend/src/views/system/server-status/index.vue` | 五段式：4 张指标卡 / 2 张 24h 趋势图 / 服务健康清单 / 环境信息 2×2 / 吸底操作栏 |
| 接口层 | `frontend/src/api/business/monitor-api.ts` | 扩展 `ServerStatusRes`（host / app / business / hermes / DB 延迟与迁移数）+ `downloadDiagnosticBundle()` |
| 路由 | `frontend/src/router/index.ts` | `/system/server-status`（`meta.permission = system:server-status:view`） |
| 后端采集 | `backend/src/business/monitor/{dto,model}.rs` | 主机 / 进程 / 业务量 / 延迟 / 迁移数 / Hermes 端口探测 |
| 后端诊断包 | `backend/src/business/monitor/diagnostic.rs` | zip 组装 + 打码 + `/metrics` 快照 + 日志尾 + 迁移清单 |
| 后端接口 | `backend/src/business/monitor/handler.rs` | `GET /api/system/server-status/diagnostic`（仅超管 + 写 `operation_logs`） |
| 健康重构 | `backend/src/support/health.rs` | 抽出 `probe_deps()`，`/health/deps` 与诊断包共用同一份判定 |
| 构建期元信息 | `backend/build.rs` | 注入 `CLOUDPIVOT_GIT_COMMIT` / `CLOUDPIVOT_RUSTC_VERSION` |
| 菜单与授权 | `backend/migrations/seed/S__menu_data.sql`、`S__role_data.sql` | 菜单行（固定 UUID `…0090`，系统管理下 sort=90，icon `server`）+ 只授 `SUPER_ADMIN` |
| 依赖 | `backend/Cargo.toml` | `zip` 由 dev-dependencies 提升为 dependencies（同版本 8.3 / 同 feature，lock 仅一份） |

> **为什么菜单写进 seed 而不是编号迁移**：`sys_menu.parent_id` 指向「系统管理」目录，
> 而该目录由 `S__menu_data.sql` 建立（seed 在所有编号迁移**之后**执行）。空库首启时
> 编号迁移里拿不到父 id，插进去就是 `parent_id IS NULL` 的孤儿 → 违反
> `menu_invariants.sh` ②。首版按卡面写了 `V0363`，空库首启实测在此处失败
> （`ERROR: V0363：找不到系统管理目录`），随后改为项目既有约定（seed）。

---

## 二、验收对照（全部实测）

| # | 验收项 | 结果 | 证据 |
|---|--------|------|------|
| ① | 系统管理下有「服务器状态」，超管能点进去 | ✅ 侧栏点击 → `URL=/system/server-status`，标题渲染 | `ui/01b-overview-full.png`、`ui/report.json` steps |
| ② | 非超管看不到菜单（菜单查询不返回） | ✅ `server-status` 命中 **0** | `api/03-menus-hit.txt`（超管 1 命中）、`ui/06-viewer-dashboard.png` |
| ③ | 非超管直连 `/system/server-status` → 403 | ✅ 前端守卫重定向 `/result/403`；两个接口直连均 **403** | `ui/07-viewer-403.png`、`api/12-viewer-server-status.txt`、`api/13-viewer-diagnostic.txt` |
| ④ | 4 个指标卡数值真实（与 top/free/df 一致） | ✅ 见下表 | `api/02-server-status.txt` |
| ⑤ | 2 个趋势图有数据（本地采样能画线） | ✅ 2 个采样点 → 2 个 ECharts canvas、0 空态 | `ui/02-trends.png`、`ui/report.json.charts` |
| ⑥ | 服务健康清单 6 项都有状态（Hermes 「未安装」正确） | ✅ 6 行：后端 API / PG / Redis / Hermes / 前端静态 / 磁盘 | `ui/01b-overview-full.png` |
| ⑦ | 环境信息版本 / commit / 迁移数正确 | ✅ `v0.1.0` / `74d8261b` / `295 条` / `PostgreSQL 18.6` | `api/02-server-status.txt`、`ui/01b-overview-full.png` |
| ⑧ | 刷新按钮能刷 | ✅ 点击后重新采集（采样点 +1，倒计时重置） | `ui/report.json` steps |
| ⑨ | 诊断包能下 zip，密码已打码（实测打开看） | ✅ 7 个成员；**真实密钥 / 口令 grep 全 0** | `api/04-diagnostic-members.txt`、`api/05-redaction-check.txt` |
| ⑩ | 视觉过关：环形有动画、卡片 hover、雾蓝流金 | ✅ `transition: stroke-dashoffset 0.8s`；hover `translateY(-2px)`；全量取主题变量 | `ui/report.json.ring` / `.hover`、`ui/01-overview.png` |
| ⑪ | 前端 typecheck + build + push | ✅ 见下 | `logs/typecheck.txt`、`logs/build-frontend.txt` |

### ④ 指标真实性核对（同一台机，前后 1 分钟内）

| 指标 | 页面 / 接口 | 系统命令 | 结论 |
|------|-------------|----------|------|
| 核心数 | `4` | `nproc` → `4` | ✅ |
| 内存总量 | `15.1 GB`（16183201792 B） | `free -b` total 16183201792 | ✅ 逐字节一致 |
| 内存已用 | `3.2 GB`（本次 21.4%） | `free -b` total − available（采样时刻不同，量级一致） | ✅ 同口径（total − available） |
| 磁盘总量 | `252.8 GB`（271465345024 B） | `df -k /` 265102876 KB = 271465345024 B | ✅ 逐字节一致 |
| 磁盘已用 | `94.3%` / `238.4 GB` | `df -k /` 93–94%（构建任务持续写入，本机磁盘确实在 92%+） | ✅ 同口径 |
| 负载 | `1.70 / 2.75 / 2.86` | `/proc/loadavg` 同量级 | ✅ 时刻差 |
| 主机运行 | `29 天 14 小时` | `/proc/uptime` 2559849 s（29.6 天） | ✅ |
| CPU | `30.5%`（200 ms 窗口） | `top` 1 s 窗口 `18.8 us + 8.9 sy` | ⚠️ 采样窗口/时刻不同，量级一致（见下「偏差」） |

### ⑥ 服务健康实测

| 服务 | 状态 | 响应时间 | 说明 |
|------|------|----------|------|
| 后端 API | 正常 | 20 ms | `GET /health → HTTP 200`（浏览器直接计时） |
| PostgreSQL | 正常 | 3.0 ms | `PostgreSQL 18.6 · 48 个连接` |
| Redis | 未启用 | — | 测试 profile `[redis] enabled = false`（生产开启后为「正常」） |
| Hermes Agent | **未安装** | — | `127.0.0.1:8100 无服务监听`（当前部署形态正常，非故障） |
| 前端静态资源 | 正常 | 19 ms | `HEAD index.html → 200` |
| 磁盘空间 | **异常** | 94.3% | ≥80% → 红（本机磁盘确实 93–94%，不是造出来的告警） |

### ⑨ 诊断包（实测打开）

```
info.json  health.json  server-status.json  metrics.txt
config-redacted.txt  migrations.txt  logs-tail.txt     （7 个文件；压缩后 24 KB）
```

打码实测（`api/05-redaction-check.txt`）：把**已知明文**逐个 grep 包内 7 个文件 ——
`NIK_MIDDLEWARE_AUTH_JWT_SECRET`（64 hex）/ `CLOUDPIVOT_STORAGE_MASTER_KEY`（64 hex）/
admin 口令 —— **命中数全部为 0**；40 位以上十六进制串 0 个文件。
`ARK_API_KEY=***REDACTED***` 这类真实第三方密钥也被打码。

---

## 三、门禁

| 门禁 | 结果 | 证据 |
|------|------|------|
| `cargo test --lib business::monitor` | **28 passed / 0 failed** | `logs/backend-test-monitor.txt` |
| `cargo fmt`（本人文件） | `rustfmt --check` 全绿 | — |
| `cargo clippy --lib` | 我的文件 **0 warning**；仓库既有 4 条 warning（`erp/invoice.rs`、`erp/receivable.rs`、`erp/sales_order_model.rs`、`attachment/model.rs`）与本卡无关，未改动 | `logs/clippy.txt` |
| `pnpm typecheck` | exit 0 | `logs/typecheck.txt` |
| `pnpm build` | exit 0（构建在 worktree，未覆盖仓库 `frontend/dist`） | `logs/build-frontend.txt` |
| `pnpm check:palettes` | 22 套配色全过（330 条断言，失败 0） | `logs/palettes.txt` |
| `scripts/verify/menu_invariants.sh` | 8 条断言全绿（菜单总数 433，icon `server` 合法） | `logs/menu-invariants.txt` |

---

## 四、复跑命令

```bash
# 1) 后端（隔离库 + 独立端口）
bash /var/tmp/home3-run/start.sh          # 需要 /var/tmp/home3-target/debug/cloudpivot
curl -s http://127.0.0.1:18093/health

# 2) 接口验收（真实 token）
bash docs/evidence/home3-20261109/scripts/verify-api.sh
# 3) 权限验收（建非超管账号 → 菜单 0 命中 / 接口 403 / 审计留痕）
bash docs/evidence/home3-20261109/scripts/verify-permission.sh
# 4) 前端：dev server 指向隔离后端
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18093 pnpm dev --port 5399
# 5) UI 验收（截图 + 断言 + 下载）
node docs/evidence/home3-20261109/scripts/verify-ui.mjs
# 6) 深色模式截图
node docs/evidence/home3-20261109/scripts/verify-dark.mjs
```

---

## 五、偏差与取舍（明写，不藏）

1. **菜单落 seed 而不是 `V03XX` 迁移**：原因见第一节末。首版 `V0363` 在空库首启实测失败（父目录尚不存在），改 seed 后空库/升级库都正确（临时改 seed 会重跑，幂等 `ON CONFLICT`）。
2. **状态灯用主题语义色，不是设计稿的 #22c55e/#f59e0b/#ef4444**：项目「雾蓝流金」七色板里没有绿，
   `--td-success-color` 就是晨雾蓝（主题注释明确「项目内正常/通过与品牌同色系」）。
   按卡面「不硬编码新颜色」的硬约束取 `--td-success/warning/error-color`，深色模式自动跟随。
3. **CPU 数值与 `top` 不完全相同**：后端是 200 ms 窗口的 `/proc/stat` 差值（既有实现），
   `top` 是 1 s 窗口；两者采样时刻也不同（本机 4 核常驻构建任务，负载本身在抖）。
   内存 / 磁盘 / 负载 / 运行时长是逐字节可对上的。
4. **趋势图是浏览器本地采样**（localStorage，24h / 30s 一个点）：后端没有历史指标表，
   为一张运维页建时序表 + 写放大不划算。页面明确写「打开本页后开始采样」，不假装是服务端历史。
   验收时用「刷新两次、间隔 16 s」拿到 2 个真实采样点画出折线（页面 15 s 去抖，不会打毛刺）。
5. **「今日单据」→「今日操作」**：跨 30+ 张业务表统计「今日单据」口径不确定且代价高，
   改用真实的 `operation_logs` 今日计数（同一区块「用户 / 在线会话 / 今日登录」也都来自真实表）。
6. **Redis 在测试 profile 未启用**：健康清单显示「未启用」（灰色），不是「异常」——语义上正确。
   生产开启 Redis 后自动变「正常」，代码无需改。
7. **诊断包的 `metrics.txt`**：`[metrics] enabled = true` 时走本机回环真抓 `/metrics`（本次即真实
   Prometheus 文本）；未启用时降级为「等价格式快照」并在文件头写明原因，不冒充 `/metrics` 原文。
8. **`commit` 字段**：来自构建期注入（`build.rs`，可用 `CLOUDPIVOT_GIT_COMMIT` 覆盖）。
   本证据里的 `74d8261b` **就是本卡的提交**——验收用的二进制是从该提交的 worktree
   重新构建的（`build.rs` 监听 `.git/HEAD`，checkout 后重新构建即自动更新），
   所以页面显示的 commit 与仓库 HEAD 一致，不是写死的值。
9. **卡片 hover / 环形动画在 `prefers-reduced-motion: reduce` 下自动关闭**（无障碍）；
   Playwright 默认不声明该偏好，故证据里能看到 `translateY(-2px)` 与 0.8 s 过渡。
