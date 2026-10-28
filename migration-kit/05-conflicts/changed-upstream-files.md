# 我们动过的上游文件清单（`git diff --numstat -M c4e8950..HEAD`）

> 基线 `c4e8950`，对照 `HEAD`（`git rev-parse HEAD`）。
> 共 102 个文件。合并/重放时按本表逐条核对：每个文件「为什么改」，决定它是**必须保留**还是**可以丢掉**。

列说明：`+/-` = 增删行数；`性质` = 改动分类；`涉及提交` = 哪些 commit 碰过它。

## 0. 按性质统计

| 性质 | 文件数 |
|------|-------:|
| 改名 | 52 |
| 安全 | 45 |
| 状态卡功能 | 14 |
| 工程/钩子 | 12 |
| 清理 | 6 |
| 文档 | 6 |
| 配色 | 6 |
| 品牌资源 | 2 |

## 1. 逐文件清单

| # | 文件 | +/- | 性质 | 涉及提交 |
|--:|------|-----|------|----------|
| 1 | `.dockerignore` | +1/-1 | 改名 | df9f658 |
| 2 | `.githooks/post-commit` | +24/-0 | 改名/工程/钩子 | 5850463, df9f658 |
| 3 | `.githooks/pre-commit` | +37/-0 | 改名/安全 | 2a93007, df9f658 |
| 4 | `.github/workflows/secret-scan.yml` | +55/-0 | 改名/安全 | 2a93007, df9f658 |
| 5 | `.gitignore` | +15/-0 | 改名/安全 | 2140b82, df9f658 |
| 6 | `.gitleaks.toml` | +72/-0 | 改名/安全 | 2a93007, 6d93101, df9f658 |
| 7 | `AGENTS.md` | +3/-3 | 改名 | df9f658 |
| 8 | `README.md` | +16/-21 | 改名/安全/清理 | 2140b82, 272e4ba, df9f658 |
| 9 | `backend/Cargo.lock` | +17/-16 | 改名/状态卡功能 | 55d3c5d, df9f658 |
| 10 | `backend/Cargo.toml` | +10/-2 | 改名/状态卡功能 | 55d3c5d, df9f658 |
| 11 | `backend/config/config-dev.example.toml` | +36/-0 | 安全 | 2140b82, 42f7a84 |
| 12 | `backend/config/config-dev.toml` | +0/-24 | 安全 | 2140b82, a37e09c |
| 13 | `backend/config/config-prod.toml` | +7/-1 | 安全/工程/钩子 | 42f7a84, ba84d2a |
| 14 | `backend/config/config-test.example.toml` | +25/-0 | 改名/安全 | 2140b82, 42f7a84, df9f658 |
| 15 | `backend/config/config-test.toml` | +0/-14 | 安全 | 2140b82, 2a93007 |
| 16 | `backend/config/config.toml` | +20/-8 | 改名/安全 | 42f7a84, 6d93101, df9f658 |
| 17 | `backend/migrations/business/V0020__add_biz_monitor_permission.down.sql` | +8/-0 | 状态卡功能 | 55d3c5d |
| 18 | `backend/migrations/business/V0020__add_biz_monitor_permission.up.sql` | +31/-0 | 状态卡功能 | 55d3c5d |
| 19 | `backend/migrations/system/V0022__add_session_security_flags.down.sql` | +7/-0 | 安全 | 2140b82 |
| 20 | `backend/migrations/system/V0022__add_session_security_flags.up.sql` | +24/-0 | 安全 | 2140b82 |
| 21 | `backend/src/bootstrap.rs` | +99/-20 | 安全 | 2140b82 |
| 22 | `backend/src/business/mod.rs` | +10/-0 | 状态卡功能 | 55d3c5d |
| 23 | `backend/src/business/monitor/dto.rs` | +86/-0 | 状态卡功能 | 55d3c5d |
| 24 | `backend/src/business/monitor/handler.rs` | +32/-0 | 状态卡功能 | 55d3c5d |
| 25 | `backend/src/business/monitor/mod.rs` | +22/-0 | 状态卡功能 | 55d3c5d |
| 26 | `backend/src/business/monitor/model.rs` | +502/-0 | 状态卡功能 | 55d3c5d |
| 27 | `backend/src/lib.rs` | +36/-9 | 改名/安全/状态卡功能 | 2140b82, 42f7a84, 55d3c5d, 560bce3, df9f658 |
| 28 | `backend/src/main.rs` | +5/-3 | 改名/安全 | 2140b82, df9f658 |
| 29 | `backend/src/middleware/auth_guard.rs` | +133/-0 | 安全 | 42f7a84 |
| 30 | `backend/src/middleware/mod.rs` | +13/-5 | 安全 | 2140b82, 42f7a84 |
| 31 | `backend/src/middleware/session_guard.rs` | +169/-0 | 安全 | 2140b82 |
| 32 | `backend/src/shared/constants.rs` | +99/-23 | 安全 | 2140b82, 42f7a84, a23c276 |
| 33 | `backend/src/shared/mod.rs` | +2/-0 | 安全 | 560bce3 |
| 34 | `backend/src/shared/startup_guard.rs` | +717/-0 | 改名/安全 | 42f7a84, 560bce3, df9f658 |
| 35 | `backend/src/support/config/model.rs` | +16/-11 | 工程/钩子 | d5a2b61 |
| 36 | `backend/src/support/dashboard/handler.rs` | +13/-14 | 工程/钩子 | d5a2b61 |
| 37 | `backend/src/support/dashboard/model.rs` | +434/-439 | 工程/钩子 | d5a2b61 |
| 38 | `backend/src/support/log/plugin.rs` | +1/-1 | 工程/钩子 | d5a2b61 |
| 39 | `backend/src/support/online/model.rs` | +347/-54 | 安全 | 42f7a84 |
| 40 | `backend/src/system/auth/dto.rs` | +21/-2 | 安全 | 2140b82, 42f7a84 |
| 41 | `backend/src/system/auth/handler.rs` | +541/-16 | 安全 | 2140b82, 42f7a84 |
| 42 | `backend/src/system/auth/model.rs` | +10/-2 | 工程/钩子 | d5a2b61 |
| 43 | `backend/src/system/menu/handler.rs` | +24/-4 | 安全 | 42f7a84 |
| 44 | `backend/src/system/role/handler.rs` | +96/-5 | 安全 | 2140b82, 42f7a84 |
| 45 | `backend/src/system/user/guard.rs` | +414/-0 | 安全 | 179f0b2 |
| 46 | `backend/src/system/user/handler.rs` | +104/-5 | 安全 | 179f0b2, 2140b82, 42f7a84 |
| 47 | `backend/src/system/user/mod.rs` | +2/-0 | 安全 | 179f0b2 |
| 48 | `backend/src/system/user/model.rs` | +143/-1 | 安全 | 179f0b2, 2140b82 |
| 49 | `docker/backend/Dockerfile` | +5/-5 | 改名 | df9f658 |
| 50 | `docker/docker-compose.prod.yml` | +26/-19 | 改名/安全 | 2140b82, 42f7a84, df9f658 |
| 51 | `docker/docker-compose.yml` | +8/-8 | 改名 | df9f658 |
| 52 | `docker/frontend/Dockerfile` | +3/-3 | 改名 | df9f658 |
| 53 | `docker/frontend/nginx.conf` | +1/-1 | 改名 | df9f658 |
| 54 | `docker/init/Dockerfile` | +2/-2 | 改名 | df9f658 |
| 55 | `docker/init/init-db.sql` | +2/-2 | 改名 | df9f658 |
| 56 | `"docs/AI\344\272\214\345\274\200\346\214\207\345\274\225.md"` | +1/-1 | 改名 | df9f658 |
| 57 | `"docs/ERP-\346\211\271\346\254\2411-\344\272\247\345\223\201\351\234\200\346\261\202PRD.md"` | +2186/-0 | 文档 | 26c9040 |
| 58 | `"docs/ERP-\346\211\271\346\254\2411-\345\274\200\345\217\221\350\256\241\345\210\222.md"` | +135/-0 | 文档 | 26c9040 |
| 59 | `"docs/ERP-\346\211\271\346\254\2411-\346\212\200\346\234\257\346\236\266\346\236\204.md"` | +2895/-0 | 文档 | 26c9040 |
| 60 | `docs/README.md` | +7/-3 | 改名/安全/文档 | 2140b82, 26c9040, 42f7a84, df9f658 |
| 61 | `docs/images/wechat-qrcode.jpg` | +-/-- | 清理 | 272e4ba |
| 62 | `"docs/\344\272\214\345\274\200-business-monitor-\346\216\245\345\217\243\345\245\221\347\272\246.md"` | +94/-0 | 改名/状态卡功能/文档 | 55d3c5d, af9310b, df9f658 |
| 63 | `"docs/\344\274\232\350\257\235\345\220\212\351\224\200\347\232\204\350\200\220\344\271\205\346\200\247\350\276\271\347\225\214.md"` | +150/-0 | 改名/安全 | 42f7a84, df9f658 |
| 64 | `"docs/\345\211\215\347\253\257\344\272\214\345\274\200\346\214\207\345\215\227.md"` | +1/-1 | 改名 | df9f658 |
| 65 | `"docs/\347\272\277\344\270\212\344\272\247\347\211\251\350\256\260\345\275\225-20261027-monitor.md"` | +53/-0 | 改名/文档 | af9310b, df9f658 |
| 66 | `"docs/\351\241\271\347\233\256\347\273\223\346\236\204\344\270\216\350\256\276\350\256\241\344\272\256\347\202\271.md"` | +8/-7 | 改名/安全 | 2140b82, df9f658 |
| 67 | `frontend/.env.development` | +1/-1 | 改名 | df9f658 |
| 68 | `frontend/.env.production` | +1/-1 | 改名 | df9f658 |
| 69 | `frontend/.npmrc` | +1/-0 | 工程/钩子 | ba84d2a |
| 70 | `frontend/README.md` | +4/-4 | 改名 | df9f658 |
| 71 | `frontend/deploy/nginx.conf` | +7/-7 | 改名 | df9f658 |
| 72 | `frontend/index.html` | +1/-1 | 改名 | df9f658 |
| 73 | `frontend/package-lock.json` | +2/-2 | 改名 | df9f658 |
| 74 | `frontend/package.json` | +2/-2 | 改名 | df9f658 |
| 75 | `frontend/public/favicon.png` | +-/-- | 品牌资源 | 46bbe9a |
| 76 | `frontend/public/logo.png` | +-/-- | 品牌资源 | 46bbe9a |
| 77 | `frontend/public/wechat-qrcode.jpg` | +-/-- | 清理 | 272e4ba |
| 78 | `frontend/src/api/business/monitor-api.ts` | +85/-0 | 状态卡功能 | 55d3c5d |
| 79 | `frontend/src/api/system/auth-api.ts` | +7/-3 | 安全 | a23c276 |
| 80 | `frontend/src/components/ContactDialog.vue` | +0/-85 | 清理 | 272e4ba |
| 81 | `frontend/src/layouts/BasicLayout.vue` | +9/-6 | 改名/配色 | 04792dc, 2794c40, f616b69, df9f658 |
| 82 | `frontend/src/router/index.ts` | +18/-1 | 改名/安全 | a23c276, df9f658 |
| 83 | `frontend/src/stores/setting.ts` | +1/-1 | 改名 | df9f658 |
| 84 | `frontend/src/stores/user.ts` | +10/-1 | 安全 | a23c276 |
| 85 | `frontend/src/style/index.less` | +28/-4 | 改名/配色 | 04792dc, 2794c40, df9f658 |
| 86 | `frontend/src/style/theme.less` | +136/-72 | 配色（四代） | 04792dc, 2794c40, 1172658/revert(ad55727), f616b69 |
| 87 | `frontend/src/types/components.d.ts` | +2/-2 | 状态卡功能/清理 | 272e4ba, 55d3c5d |
| 88 | `frontend/src/views/dashboard/index.vue` | +295/-189 | 配色/状态卡功能/清理 | 04792dc, 2794c40, f616b69, 272e4ba, 55d3c5d, b08f3a4 |
| 89 | `frontend/src/views/login/index.vue` | +13/-13 | 改名/配色/安全 | 04792dc, 2794c40, f616b69, 2140b82, df9f658 |
| 90 | `frontend/src/views/user/change-password.vue` | +158/-0 | 配色/安全 | 04792dc, 2794c40, f616b69, a23c276 |
| 91 | `frontend/theme-check.mjs` | +1/-1 | 改名 | df9f658 |
| 92 | `frontend/theme-persist.mjs` | +1/-1 | 改名 | df9f658 |
| 93 | `frontend/vite.config.ts` | +4/-2 | 安全/工程/钩子 | a37e09c, ba84d2a |
| 94 | `llms.txt` | +2/-2 | 改名 | df9f658 |
| 95 | `scripts/build.sh` | +2/-2 | 改名 | df9f658 |
| 96 | `scripts/dev-down.sh` | +66/-0 | 改名/工程/钩子 | ba84d2a, df9f658 |
| 97 | `scripts/dev-status.sh` | +52/-0 | 改名/工程/钩子 | ba84d2a, df9f658 |
| 98 | `scripts/dev-up.sh` | +173/-0 | 改名/安全/工程/钩子 | 2140b82, a37e09c, ba84d2a, df9f658 |
| 99 | `scripts/init-db.sh` | +3/-3 | 改名 | df9f658 |
| 100 | `scripts/patch-alun-web.sh` | +382/-0 | 改名/安全 | 2140b82, df9f658, f3fc278 |
| 101 | `scripts/secret-scan.sh` | +111/-0 | 改名/安全 | 2a93007, 6d93101, df9f658 |
| 102 | `skill/SKILL.md` | +5/-5 | 改名/安全 | 2140b82, df9f658 |

## 2. 合并时怎么用这张表

1. **改名类（`refactor(rename)`）**：不要 merge，直接用 `01-rename/rename.sh` 在新基线上重放。
   凡本表标了「改名」的文件，Pro 基线里的同名文件都要重新过一遍脚本。
2. **安全类**：`02-security/patches` 按序 `git am`；打不上的按 `design-notes.md` 逐条手工重做。
   ★ 这类改动**不能丢**，丢了等于把已知漏洞重新引入。
3. **配色/品牌**：`03-brand/apply.sh`（patch）或整文件替换；视觉类，冲突了以新基线的结构为准。
4. **状态卡功能**：`04-modules/business-monitor/copy.sh` + `wiring.md` 的四处接线。
5. **清理类（公众号二维码/关于卡片）**：如果 Pro 又重新加了推广位，需要再删一次 —— 逐个 `grep` 关键字确认。
6. **文档类**：可以最后补，不阻塞编译。
