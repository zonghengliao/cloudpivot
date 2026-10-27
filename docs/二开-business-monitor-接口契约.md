# 二开模块 `business/monitor` —— 服务器状态接口契约

> 引入版本：迁移 `V0020`（二开号段，基础版封顶 V0017）
> 代码位置：`backend/src/business/monitor/`（`mod.rs` / `handler.rs` / `dto.rs` / `model.rs`）
> 前端位置：`frontend/src/api/business/monitor-api.ts` + `frontend/src/views/dashboard/index.vue`
> 这是本项目**第一个 `business/` 业务模块**，同时完成了「`business/` 容器首次登记」：`src/business/mod.rs` + `lib.rs` 的 `pub mod business;`

## 1. 接口

| 项 | 值 |
|---|---|
| 方法 / 路径 | `GET /api/business/monitor/server-status` |
| 后端宏路径 | `/business/monitor/server-status`（**不含** `/api`，前缀由 `[router] prefix` 注入） |
| 权限码 | `business:monitor:server-status` |
| 授权范围 | 仅 `SUPER_ADMIN`（种子授权 + 迁移 V0020 写入 `sys_menu` / `sys_role_menu`） |
| 鉴权失败 | 401（无令牌） / 403（非超管：`{"code":403,"msg":"无权限访问，需要权限: business:monitor:server-status"}`） |
| 响应 | 统一 `Res<ServerStatusRes>`，`code=0`；**恒 200，不因单项采集失败返回 5xx** |

### 响应结构（字段名与前端 TS 类型严格一致）

```json
{
  "code": 0,
  "msg": "ok",
  "data": {
    "cpu": { "percent": 1.2, "cores": 4 },
    "memory": { "percent": 19.6, "used_bytes": 1375494144, "total_bytes": 7756943360,
                "used_text": "1.3 GB", "total_text": "7.2 GB" },
    "disk":   { "percent": 8.8, "used_bytes": 23880863744, "total_bytes": 271465345024,
                "used_text": "22.2 GB", "total_text": "252.8 GB" },
    "database": { "state": "up", "name": "cailu", "size_bytes": 9639615, "size_text": "9 MB",
                  "connections": 25, "version": "PostgreSQL 18.6 (Ubuntu 18.6-0ubuntu0.26" },
    "redis":  { "state": "up", "version": "8.0.5", "used_memory_text": "1.08M",
                "used_memory_bytes": 1131856, "connected_clients": 4, "uptime_seconds": 17363,
                "keyspace_hits": 199, "keyspace_misses": 5281 },
    "collected_at": "2026-10-27T09:40:25.051869938+00:00"
  }
}
```

## 2. 采集口径

| 指标 | 实现 | 失败时 |
|---|---|---|
| CPU | `/proc/stat` 首行，间隔 200ms 采两次，`(busyΔ / totalΔ) × 100`，busy = total − idle − iowait，结果 clamp 0-100 | `percent=0.0` |
| 内存 | `/proc/meminfo`：`MemTotal` − `MemAvailable`；`MemAvailable` 缺失时退化为 `MemFree+Buffers+Cached` | 全 0 |
| 磁盘 | `df -P -k /` 解析（未引入 `libc`，避免新增系统依赖） | 全 0 |
| 数据库 | 复用 `db()` 连接：`current_database()` / `pg_database_size()` / `pg_stat_activity` 计数 / `version()`（截 40 字符） | `state="down"` |
| Redis | `redis::cmd("INFO")`（依赖已在依赖树中，见 `Cargo.toml` 注释）取版本、`used_memory*`、`connected_clients`、`uptime_in_seconds`、`keyspace_hits/misses` | `state="down"` |

**安全边界**：响应只含聚合数值 / 版本号；**不含**数据库密码、连接串、Redis 密码、内部路径、主机地址。

## 3. 前端行为

- 卡片位于工作台右栏（360px，`dashboard__card` 体系），标题「服务器状态」。
- **仅超管渲染**：`v-if="isSuperAdmin"`（`userStore.roles.includes('SUPER_ADMIN')`）。非超管时整卡不进 DOM，且**不发请求**（实测 `performance` 中 `server-status` 请求数为 0）。
  - 不用 `hasPermission('business:monitor:server-status')` 作为唯一判据：超管持有 `*:*:*` 通配码，若该权限点将来授予普通角色，`hasPermission` 会放行，与「仅超管可见」相悖。
- 保留原「服务状态 / 应用名称 / 运行 profile / 服务端时间」（数据仍来自免鉴权 `/health`）。
- 新增：CPU / 内存 / 磁盘三条 `t-progress`（≥85% 红、≥70% 橙、其余绿）+ 容量文本；数据库（状态 / 名称 / 大小 / 连接数）；Redis（状态 / 内存 / 客户端数 / 运行时长）。
- 刷新按钮 + 20s 轮询；`document.visibilityState !== 'visible'` 时暂停轮询，标签页回到前台立即补一次并重新计时；`onUnmounted` 清理定时器与监听。

## 4. 迁移与权限种子

```sql
-- backend/migrations/business/V0020__add_biz_monitor_permission.up.sql
INSERT INTO sys_menu (...) VALUES ('20000000-0000-0000-0000-000000000001', 'business:monitor:server-status', ...)
ON CONFLICT (id) DO NOTHING;
INSERT INTO sys_role_menu (...) SELECT ... WHERE r.role_code = 'SUPER_ADMIN' ON CONFLICT DO NOTHING;
```

- 超管鉴权本身由 `role_code` 短路（`is_super_admin` → 权限列表下发 `*:*:*`），`sys_role_menu` 的写入是为了「角色授权」页权限树完整展示。
- `.down.sql` 先删授权行再删菜单行。
- 幂等：固定 UUID + `ON CONFLICT DO NOTHING`，重复执行不报错、不覆盖人工改名。

## 5. 测试基线

```bash
cd /opt/cailu/backend  && cargo test        # 22 passed（新增 8 个：/proc/stat 解析、差值/除零、meminfo 正常+回退、df 正常+异常、redis INFO、format_bytes、truncate_chars）
cd /opt/cailu/frontend && pnpm typecheck    # rc=0
```

实测（dev `cailu_dev` / prod `cailu` 双端）：

| 场景 | 结果 |
|---|---|
| 超管 token `GET /api/business/monitor/server-status` | 200，`code=0`，字段齐全有真实数值 |
| 普通角色 token 同接口 | **403**，`msg` 明确给出所需权限码 |
| 超管工作台（浏览器） | 卡片渲染，CPU/内存/磁盘/DB 名+大小/Redis 状态均有真实值，原 4 项信息保留 |
| 非超管工作台（浏览器） | 卡片不在 DOM，`server-status` 请求数 0 |
| 20s 轮询 | 「更新于」自动前进（visible 时）；页面隐藏时按设计暂停 |

## 6. 后续二开参考

新增 `business/<模块>` 时按同一套路走：四件套 + `pub mod` 登记（首次需建 `business/mod.rs` 并在 `lib.rs` 声明）+ `migrations/business/V00xx__*.{up,down}.sql`（从 V0020 起）+ `sys_menu` 权限点 + 前端 `api/business/<模块>-api.ts` + 单测（正常 + 1 个边界）+ `cargo test` 全绿。
