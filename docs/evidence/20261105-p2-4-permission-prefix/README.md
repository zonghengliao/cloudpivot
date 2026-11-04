# P2-4 · 权限码前缀统一（合同 `erp:contract:*` → `business:erp:contract:*`）

卡号：`t_012caced`　执行：backend-developer　日期：2026-11-05

## 一句话结论

合同 5 个权限码 + 1 条菜单 `auth_code` **就地改名**成 `business:erp:contract:*`（与 ERP 其余 313 码同族），
菜单 `id` 不变 ⇒ 已配授权**一行不丢**；旧码 `erp:contract:*` 由 `shared::permission_alias` 在
**装载权限时双向互扩**（新令牌 / `/me` / 前端判权 / 行级 `has_permission` 都同时认新旧码）；
部署瞬间在途的旧令牌由迁移推进 `token_version` 一次性升级（401 + `20008 请重新登录`，不会静默 403）。

## 改动

| 文件 | 改了什么 |
|------|----------|
| `backend/migrations/business/V0241__erp_contract_permission_prefix_unify.{up,down}.sql` | 就地改名 6 条 `sys_menu`（`auth_code` + 按钮 `name`）、兜底补种、持有者 `token_version` +1、5 项自检 |
| `backend/src/shared/permission_alias.rs`（新） | `LEGACY_ALIASES` 对照表 + `aliases_of` / `expand`（双向、幂等）+ 4 条纯单测 |
| `backend/src/shared/mod.rs` | 注册模块 |
| `backend/src/system/auth/model.rs` | `load_permissions_uncached` 末尾 `expand(&mut permissions)`（唯一收口） |
| `backend/src/business/erp/contract_handler.rs` | 5 个 `PERM_*` 常量与 6 处 `#[nik::permission]` 注解换新码、`PERMISSION_PREFIX`、`LEGACY_PERMISSIONS`；单测 ①② 改写 + 新增 ②b（双码互扩）与 ⑩（真库：授权后 `load_permissions` 返回 10 码） |
| `frontend/src/router/index.ts` / `views/business/erp/contract/index.vue` / `api/business/erp/index.ts` | 路由 `meta.permission` 与页面 `PERM` 常量换新码 |
| `scripts/verify/deploya_schema_check.sh` | 合同权限码检查改新码，并新增「旧码残留 = 0」 |
| `scripts/verify/audit_features_20261104/chains_e2e.py` | 「制单员」夹具的 3 个合同码换新码 |
| `docs/architecture/ARCH-2-…md` 附录 B #7 | 权限码一栏更新为新前缀并注明别再改回去 |

## 证据文件

| 文件 | 说明 |
|------|------|
| `01-before-migration-codes.txt` | 迁移**前**（模拟 V0241 之前）：5 条 `erp:contract:*`，每条已有 1 行授权 |
| `02-after-migration-codes.txt` | 迁移**后**：5 条 `business:erp:contract:*`，**菜单 id 不变、授权行数仍为 1**（零丢失） |
| `05-migration-with-token-bump.txt` | 迁移日志：`自检通过：5 个 business:erp:contract:* 就位，旧码 0 残留，授权行 5 → 5（零丢失），在途令牌升级 1 人` |
| `03-me-permissions.txt` | 非超管用户 `p2p4_user` 的 `/api/system/auth/me` → **10 个码（新 5 + 旧 5）** |
| `04-token-version-before.txt` | 升级前 `token_version=0` |
| `06-inflight-token-then-refresh.txt` | 部署前签发的令牌 → `401 + 20008 您的权限已变更，请重新登录` |
| `07-after-relogin.txt` | 重新登录后 `/me` 仍是 10 个码、`GET /api/business/erp/contract/page` = **200** |
| `08-negative-control.txt` | 反例：无合同授权账号 `permissions=[]`，合同分页 **403** |
| `09-ui-contract-page-with-new-codes.png` | 真机（vite dev + 真实后端）：`p2p4_user` 打开 `/business/erp/contract`，侧栏「合同管理」在、「新建合同」按钮在、表格渲染 1 行 |

## 复现方式（隔离库，不碰他人环境）

```bash
# 1) 建库 + 迁移（起一次实例即可，迁移在启动时应用）
runuser -u postgres -- createdb -O alun cloudpivot_pd4_v2
cd backend && NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_pd4_v2 NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun NIK_SERVER_LISTEN=127.0.0.1:18298 NIK_ADMIN_PASSWORD='Admin@123456' \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  ./target/debug/cloudpivot   # 日志里应出现 V0241 自检通过

# 2) 受影响模块用例（连跑两轮）
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_pd4_v2 SYS_DB_URL=$ERP_DB_URL \
  cargo test --lib -- business::erp::contract_handler system::menu::tests::n10 \
  system::menu::permission_audit_tests system::role::tests system::authz_tests shared::permission_alias \
  --test-threads=1   # 51 passed / 0 failed
```

> 注：`system::menu::permission_audit_tests`（N14/N15）会**全表扫描 `sys_menu`**，与
> `system::role::tests` 造的临时菜单（`testc:…`）在**并行**下互相干扰 —— 这是既有测试设计约束
> （仓库脚本一律 `--test-threads=1`），与本次改动无关。并行轮次按「造夹具的组」与「全表扫描的组」
> 分开跑即全绿。
