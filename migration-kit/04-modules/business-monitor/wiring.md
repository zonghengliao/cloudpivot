# business-monitor 模块：文件清单与接线点

> 模块 = 工作台右栏「服务器状态」卡（CPU / 内存 / 磁盘 / PostgreSQL / Redis），**仅超管可见**。
> 后端四件套 + 前端 API + 迁移 V0020，共 8 个文件；不做 service 层（遵循底座约定）。

## 1. 文件清单（本目录内相对路径 = 目标树相对路径）

| 文件 | 作用 |
|------|------|
| `backend/src/business/mod.rs` | 业务模块容器，声明 `pub mod monitor;` |
| `backend/src/business/monitor/mod.rs` | 模块出口，注册路由 |
| `backend/src/business/monitor/dto.rs` | 出参 DTO（cpu/mem/disk/db/redis/uptime） |
| `backend/src/business/monitor/model.rs` | 采集实现：`/proc/stat` 差值、`/proc/meminfo`、`df -P`、PG `SELECT 1`、Redis `INFO`；单项失败降级为 `state=down`，**接口永不 5xx** |
| `backend/src/business/monitor/handler.rs` | `GET /business/monitor/server-status`，`#[alun::permission("business:monitor:server-status")]` |
| `frontend/src/api/business/monitor-api.ts` | 前端 API 封装 |
| `backend/migrations/business/V0020__add_biz_monitor_permission.{up,down}.sql` | 权限点 `business:monitor:server-status` + SUPER_ADMIN 授权（幂等 UUID + `ON CONFLICT DO NOTHING`） |

模块单测 8 个，全部在 `model.rs` 的 `#[cfg(test)]` 中（解析 / 差值 / 除零 / 格式化边界）。

## 2. 四处接线点（copy.sh 会自动自检）

1. **`backend/src/lib.rs`** —— 加一行（放在 `pub mod support;` 附近）
   ```rust
   pub mod business;
   ```
2. **`backend/src/business/mod.rs`** —— 声明子模块
   ```rust
   pub mod monitor;
   ```
3. **`backend/Cargo.toml`** —— 显式声明 redis 依赖（框架的 `Cache` 门面只有 KV 语义，读不到 `INFO`）
   ```toml
   redis = { version = "1.7", features = ["tokio-comp", "connection-manager"] }
   ```
   版本/features 与 `alun-cache 0.4.2` 内部声明完全一致 —— **不引入依赖树里不存在的新 crate**，
   只是把既有的间接依赖显式化。若 Pro 的 alun 版本不同，先 `cargo tree -i redis` 对齐版本再填。
4. **`frontend/src/types/components.d.ts`** —— 注册 `TProgress`（进度条）
   ```ts
   TProgress: typeof import('tdesign-vue-next')['Progress']
   ```

## 3. 前端页面（不在拷贝范围内）

`frontend/src/views/dashboard/index.vue` 的右栏改造同时混有「配色」「推广位删除」等其它改动，
整文件覆盖会把别的改动一起回退。**逐条核对**见 `05-conflicts/changed-upstream-files.md`，
要点：

- `v-if="isSuperAdmin"` 才渲染状态卡，且**非超管不发请求**（省掉无权限的 401 噪音）
- 20s 轮询，`document.hidden` 时暂停
- 三项进度条（CPU/内存/磁盘）+ DB/Redis 明细（`state` 用语义色：up=绿 / down=红）
- 保留原有「服务状态 / 应用名 / profile / 时间」

## 4. 权限模型（Pro 若也做权限改造，这里是冲突点）

- 权限码：`business:monitor:server-status`，**必须**与 `sys_menu.auth_code` 逐字一致
- 只有 SUPER_ADMIN 可读；超管判定走 `role_code` 短路（`system/auth/model.rs::is_super_admin`，
  权限列表直接下发 `*:*:*`），V0020 里写 `sys_role_menu` 只是为了让「角色授权」页的权限树能展示它，
  **不是鉴权所必需** —— 这一点在与 Pro 的 RBAC 实现合并时尤其要注意：若 Pro 改成纯菜单树鉴权，
  超管短路逻辑要跟着改，否则状态卡会对超管 403。
