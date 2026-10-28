# 安全补丁序列说明（design-notes.md）

> 15 条补丁 = `c4e8950..d5a2b61` 全部改动，按**时间顺序**编号，用 `02-security/apply.sh` 逐条 `git am`。
> 其中 **9 条是安全整改**（下表标 🔒），其余 6 条是它们的前置/配套（清理、迁移、功能、lint、钩子）。
>
> **为什么不能只挑 9 条打**：`git format-patch` 的补丁带父提交上下文。实测 `0015`（lint）依赖
> `0007`（状态卡）新建的 `backup/src/support/dashboard/model.rs`，`0012` 依赖 `0006` 的
> `startup_guard.rs`。跳着打必然 `error: ... does not apply`。所以序列 = 完整改动序列。

应用到哪一步、期望结果：

| 打完后 | 期望树 == | 说明 |
|--------|-----------|------|
| 全部 15 条 | `d5a2b61` | 改名前的「云枢改动」终态 |

## 总览

| 编号 | 类型 | 提交 | 内容 | 关键文件 |
|------|------|------|------|----------|
| 0001 | 工程 | `ba84d2a` | dev 环境记录：vite BACKEND_ORIGIN 可覆盖、dev 启停/状态脚本、npm 镜像 | `frontend/.npmrc`, `frontend/vite.config.ts`, `scripts/dev-*.sh` |
| 0002 | 清理 | `272e4ba` | 删公众号二维码与「联系我们」推广位 | `frontend/src/components/ContactDialog.vue`(删), `frontend/src/views/dashboard/index.vue`, `*.jpg`(删) |
| 0003 | 清理 | `b08f3a4` | 删 dashboard「关于项目」卡片 | `frontend/src/views/dashboard/index.vue` |
| 0004 | 🔒 P0 | `a37e09c` | dev JWT 密钥环境变量化；Vite / code-server 只监听 127.0.0.1 | `backend/config/config-dev.toml`, `frontend/vite.config.ts`, `scripts/dev-up.sh` |
| 0005 | 🔒 P1 | `2a93007` | config-test 密钥环境变量化 + gitleaks 三级防线（本地/钩子/CI） | `.gitleaks.toml`(新增), `scripts/secret-scan.sh`(新增), `.githooks/pre-commit`(新增), `.github/workflows/secret-scan.yml`(新增) |
| 0006 | 🔒 后端 | `560bce3` | 启动守卫 fail-closed（认证开启但密钥空/短 → 拒绝启动） | `backend/src/shared/startup_guard.rs`(新增 600 行), `backend/src/lib.rs` |
| 0007 | 功能 | `55d3c5d` | 工作台右栏「服务器状态」卡（仅超管） | `backend/src/business/monitor/*`, `frontend/src/api/business/monitor-api.ts`, `migrations/business/V0020` |
| 0008 | 🔒 P3 | `6d93101` | 去掉 `config.toml` 里的 docker 默认库口令 `alun`，撤销扫描器 allowlist | `backend/config/config.toml`, `.gitleaks.toml`, `scripts/secret-scan.sh` |
| 0009 | 钩子 | `5850463` | post-commit 自动同步到本机 bare 仓库 | `.githooks/post-commit`(新增) |
| 0010 | 🔒 P1 | `179f0b2` | 用户管理权限上限：堵死「建号 / 改密 / 授角色」三条提权路径 | `backend/src/system/user/guard.rs`(新增 414 行), `user/handler.rs`, `user/model.rs` |
| 0011 | 文档 | `af9310b` | 接口契约测试基线 + 线上产物指纹（commit/二进制 sha256/dist） | `docs/二开-business-monitor-接口契约.md`, `docs/线上产物记录-*.md` |
| 0012 | 🔒 P1 | `2140b82` | 密钥治理（toml→env 全量）+ 首登改密 + 会话吊销 + 框架 fail-fast + 规则遮蔽自检 | `backend/src/bootstrap.rs`, `middleware/session_guard.rs`(新), `shared/constants.rs`, `system/{auth,role,user}/*`, `config-*.example.toml`, `scripts/patch-alun-web.sh`(新增), `migrations/system/V0022` |
| 0013 | 🔒 P1 | `a23c276` | 首登强制改密**前端闭环**（改密页 + 路由守卫 + 白名单补 me/menus） | `frontend/src/views/user/change-password.vue`(新), `frontend/src/router/index.ts`, `frontend/src/stores/user.ts`, `backend/src/shared/constants.rs` |
| 0014 | 🔒 P1 | `42f7a84` | CODE-16 B：短 TTL + 会话绝对上限 + fail-closed 吊销 + refresh 复用检测 | `backend/src/middleware/auth_guard.rs`(新), `system/auth/handler.rs`, `support/online/model.rs`, `shared/{constants,startup_guard}.rs` |
| 0015 | 工程 | `d5a2b61` | 收掉 §6 检查链的基线 lint 债务（framework 底座最小改动，老大已批准） | `support/dashboard/{model,handler}.rs`, `support/config/model.rs`, `system/auth/model.rs` |

---

## 0004 🔒 P0 · dev JWT 密钥环境变量化 + 监听收口

**改了什么**
- `backend/config/config-dev.toml`：`jwt_secret` 从明文改成「注释说明 + 空值」，由 `ALUN_MIDDLEWARE_AUTH_JWT_SECRET` 注入。
- `frontend/vite.config.ts`：`host: process.env.VITE_HOST || '127.0.0.1'`（原来监听 `0.0.0.0`）。
- `scripts/dev-up.sh`：code-server 与 vite 都绑定 `127.0.0.1`，远程访问走 SSH 隧道。

**为什么**：dev 环境原来把 JWT 密钥写在配置文件里（且同一把密钥在多个环境复用）——
拿到仓库就等于拿到签发权，可以伪造任意用户（包括超管）的令牌。监听 `0.0.0.0`
则把 dev 服务器和 code-server（等于远程 shell）暴露到局域网上。

**apply 失败怎么手工重做**
1. `config-dev.toml` 搜 `jwt_secret`，把值改成空串，上面加一行注释说明由
   `ALUN_MIDDLEWARE_AUTH_JWT_SECRET` 注入（env 文件在 `/etc/cloudpivot/cloudpivot-dev.env`，600）。
2. `vite.config.ts` 的 `server.host` 改为 `process.env.VITE_HOST || '127.0.0.1'`。
3. `dev-up.sh` 里所有 `--host 0.0.0.0` / `--bind-addr 0.0.0.0` 改成 `127.0.0.1`。
4. 验证：`grep -rn "0.0.0.0" scripts/dev-up.sh frontend/vite.config.ts` 应无输出（除注释）。

## 0005 🔒 P1 · gitleaks 三级防线

**改了什么**：新增 `.gitleaks.toml`（3 条自定义规则）、`scripts/secret-scan.sh`
（`--staged/--tree/--history` 三种模式）、`.githooks/pre-commit`（**fail-closed**：
扫描器缺失就拒绝提交，而不是静默放行）、`.github/workflows/secret-scan.yml`。
`config-test.toml` 的 jwt_secret 同时改成环境变量注入。

**为什么**：`config-*.toml` 里的凭据是这类仓库最常见的泄露源，且历史里已经真实泄露过两次。
三级 = 提交时（钩子）/ CI / 历史回扫。

**apply 失败怎么手工重做**
1. 先 `git checkout` 拿 5 个新文件（`.gitleaks.toml`、`scripts/secret-scan.sh`、
   `.githooks/pre-commit`、`.github/workflows/secret-scan.yml`），再把 `config-test.toml` 的
   `jwt_secret` 置空。
2. `git config core.hooksPath .githooks`（每个克隆一次）。
3. 验证：`bash scripts/secret-scan.sh --tree` 应 `no leaks`；故意在
   `backend/config/config.toml` 写一把 `password = "x"` 应被拦。

> ⚠ 规则 3 的正则**保留旧前缀 cailu**，改名脚本会专门护住它（见 `01-rename/special-cases.md` 特例 1）。
> 与 Pro 合并时若 Pro 也自带 gitleaks 配置，**以我们这份为准**（它的历史泄露形态是我们知道的）。

## 0006 🔒 · 启动守卫 fail-closed

**改了什么**：新增 `backend/src/shared/startup_guard.rs`（600 行）。核心是「配置不自洽就拒绝启动」：
认证开启但 `jwt_secret` 为空/过短、prod 用 `local` 缓存而不是 redis、CORS 通配 + credentials 等
组合一律 `exit(1)`，日志打印**判定依据**但不打印密钥。

**为什么**：原实现是 fail-open —— 密钥为空时认证层「什么也不挂」，`AuthClaims` 提取器
用空 HMAC 密钥验签，攻击者可以自签任意 claims 直接登进来。这是整批整改里最严重的一条。

**apply 失败怎么手工重做**
1. 把 `backend/src/shared/mod.rs`、`lib.rs` 的 `main` 启动链按补丁接入
   `startup_guard::check(&cfg)`（在 `App::new` 之前调用）。
2. 判定规则（按需裁剪）：
   - `middleware.auth.enabled && jwt_secret.len() < 32` → 拒绝
   - `profile == prod && cache.type != redis` → 拒绝（令牌黑名单重启即丢）
   - `cors.allow_origins` 含 `*` 且 `allow_credentials` → 拒绝
3. 验证：`ALUN_MIDDLEWARE_AUTH_JWT_SECRET=`（空）启动应打印「拒绝启动」并退出码 1。

## 0007 · 服务器状态卡（功能，非安全）

见 `04-modules/business-monitor/wiring.md`（文件清单 + 4 处接线 + 权限模型）。
**注意**：本补丁修改了 `frontend/src/views/dashboard/index.vue`（307 行改动）和
`backend/Cargo.toml`（显式 redis 依赖），后续 `0015` 的 lint 清理依赖它新增的
`support/dashboard/model.rs`。

## 0008 🔒 P3 · 去掉 docker 默认库口令

**改了什么**：`config.toml` 的 `[database] password` 从 `"alun"` 改成空串（由
`ALUN_DATABASE_PASSWORD` 注入），并删掉 `.gitleaks.toml` / `secret-scan.sh` 里为了放过
这个假口令而加的 allowlist。

**为什么**：`alun` 是 docker 示例库口令，跟 `docker/docker-compose.yml` 里的
`POSTGRES_PASSWORD` 一致 —— 谁拿到仓库就能连上按默认口令起的库。allowlist 更是
「为了让扫描通过而改扫描器」，属于安全反模式。

**手工重做**：把 `password = ""`、`max_connections` 保持原值；确认
`grep -n 'password' backend/config/config.toml` 只有注释和空串；`alun` 只在
`backend/config/config.toml` 的 `[database] user`（**账号名不是凭据**）里出现，
若 Pro 换成别的用户名，扫描器 allowlist 不要重新加回来。

## 0009 · post-commit 同步 bare

**改了什么**：`.githooks/post-commit`，只推当前分支、只认 `/opt/git/*.git`、10s 超时、失败静默。
**为什么**：本地 bare 是唯一「远端」（不再向 GitHub 推），同步失效会让备份悄悄断掉。
**手工重做**：注意改名脚本会把白名单改成通配 `/opt/git/*.git`（见 special-cases 特例 2）。

## 0010 🔒 P1 · 用户管理提权三连

**改了什么**：新增 `backend/src/system/user/guard.rs`（414 行）+ `user/handler.rs`/`model.rs` 接入。
三条路径逐个堵：
1. **建号**：非超管不能创建比自己权限高的角色（防止造一个超管账号）；
2. **改密**：非超管不能改超管的口令（防止接管超管）；不能改自己的角色；
3. **授角色**：授权集合必须是「当前操作者权限的子集」，且不能授予 `SUPER_ADMIN`。

**为什么**：原来只校验「有 user:update 权限」，于是任何有用户管理权限的普通管理员都能
把别人（或自己）提成超管 —— 权限体系形同虚设。

**apply 失败怎么手工重做**
1. 新增 `user/guard.rs`，核心三个断言函数：`assert_can_create_role`、`assert_can_reset_password`、
   `assert_role_grant_subset`（都返回 `AlunError::Forbidden`）。
2. 在 `user/handler.rs` 的 create / reset_password / update 三处、`role/handler.rs` 的
   assign_users 处各插一行调用。
3. 验证（重要，别只信编译过）：
   - 用普通管理员令牌 `POST /api/system/user` 带 `role_codes=["SUPER_ADMIN"]` → 403
   - 用普通管理员令牌重置超管口令 → 403
   - 超管执行同样操作 → 200

## 0012 🔒 P1 · 密钥治理 + 首登改密 + 会话吊销 + 框架 fail-fast

**改了什么**（最大的一条，26 文件 / +1092 行）
1. `config-dev.toml` / `config-test.toml` 从仓库删除，改为 `*.example.toml` 模板；
   `.gitignore` 忽略真实配置文件。密钥/口令全部走 `/etc/cloudpivot/*.env`（600）。
2. `middleware/session_guard.rs`（169 行）+ 迁移 `system/V0022`：
   `must_change_password`（TRUE 时除改密/登出外全部 403）、`token_version`（自增即令该用户
   全部已签发令牌失效）。
3. `shared/constants.rs`：改密/登出的白名单路径集中管理。
4. `bootstrap.rs`：启动期 fail-fast（配置缺失即报错，不吞）。
5. `scripts/patch-alun-web.sh`（371 行，新增）：给 `alun-web 0.4.2` 打两个补丁 ——
   CODE-07 认证层 fail-open→fail-fast；CODE-09 权限规则「静态段优先」+ 免认证路径跳过
   + 规则遮蔽自检。
6. `system/{auth,role,user}/handler.rs`：改密、重置口令、改角色、停用时自增 `token_version`。

**为什么**：密钥进 git（第 1 点）、会话永不过期（第 2 点）、改密后旧令牌还能用（第 6 点）
三件事在真实审计里是连续踩的；框架层的 fail-open（第 5 点）会让上面所有业务层校验失效。

**apply 失败怎么手工重做**
1. 迁移先做：`backend/migrations/system/V0022__add_session_security_flags.{up,down}.sql`
   必须**幂等**（`ADD COLUMN IF NOT EXISTS`），因为同批次可能还有别的迁移加同名列。
2. `session_guard.rs`：读 `sys_user.must_change_password` / `token_version`（启动期缓存 60s 亦可），
   逐请求比对 claim `ver`；不一致返回 401「会话已失效，请重新登录」。
3. `patch-alun-web.sh`：**不要手改 registry 源码**，跑脚本；`cargo update` / `cargo clean`
   之后必须重跑（脚本幂等，判重按「补丁代码是否在位」）。
   验证：`bash scripts/patch-alun-web.sh --check` → `PATCH-STATE: OK`。
4. 验证：登入拿到 token → 管理员改该用户口令 → 旧 token 立刻 401。

## 0013 🔒 P1 · 首登强制改密前端闭环

**改了什么**：`frontend/src/views/user/change-password.vue`（新，158 行）+
`router/index.ts` 全局守卫（`user.mustChangePassword` 为真时只允许进 `/user/change-password`）+
`stores/user.ts` 状态 + `api/system/auth-api.ts` 调改密接口；
后端 `constants.rs` 的白名单补上 `system/auth/me`、`system/auth/menus`
（否则被强制改密时前端连用户信息都拉不到，页面白屏）。

**为什么**：只有后端 403 的白名单不够 —— 前端得知道「为什么被拦」并把人送到改密页，
否则用户体验是「登录成功后满屏 403」。

**手工重做**：改密页 → 守卫 → store → 后端白名单四件套，缺一个都会白屏或死循环。
验证：把某用户 `must_change_password` 置 TRUE → 登录 → 应直接跳改密页，
除改密/登出外任何请求 403；改完密 → 正常进工作台。

## 0014 🔒 P1 · CODE-16 B（短 TTL / 绝对上限 / fail-closed 吊销 / 复用检测）

**改了什么**
1. `config.toml`：`access_token_expire_secs = 900`、`refresh_token_expire_secs = 86400`（**绝对上限**，
   refresh 的 exp 固定为「登录时刻 + 86400」，轮换不滑动）。
2. `middleware/auth_guard.rs`（新，133 行）：每请求校验令牌黑名单与 `ver`，**缓存不可用即拒绝**
   （fail-closed，而不是当作「没吊销」）。
3. `system/auth/handler.rs`：refresh 轮换写血缘标记，命中「已作废的旧 refresh」→ 判定复用
   → 该用户全部会话失效（`reuse_detected`）。
4. `support/online/model.rs`：在线会话登记/吊销（401 行改动）。
5. `startup_guard.rs`：prod 必须是 redis 缓存，否则拒绝启动（B6）。

**为什么**：refresh 复用检测的前提是「记录不会丢」。用进程内 LRU 存黑名单，重启即清空，
吊销静默失效 —— 所以要 fail-closed + 强制 redis。

**手工重做**：按上面 5 点逐条落。验证顺序建议：
1. `access_token` TTL 15 分钟（改 `exp` 观察 401）；
2. 登录 → 记下 refresh A → 刷新得到 B → **再用 A 刷新** → 应 401 且该用户所有会话失效；
3. 停掉 redis → 任何带令牌的请求都应 401（fail-closed），而不是放行。

## 0015 · lint 基线债务清理

纯可读性/工具链改动（`clippy`/`fmt` 基线归零），**无语义变化**，但动了
`support/dashboard/model.rs` 873 行。若 `git am` 在这条上失败（Pro 改过 dashboard），
**直接跳过本补丁**即可 —— 它不影响任何安全与功能断言，只是让 CI 的 lint 门禁过。

---

## 已知事项：补丁文件里含有「已轮换的旧密钥字面量」

`0004` / `0005` 这两条补丁的**删除行**（`-` 开头）里，保留了上游基线里原本写在
`backend/config/config-dev.toml`、`config-test.toml` 中的 dev/test JWT 密钥原文。
这是 **`git am` 的硬性要求**（上下文必须逐字节匹配），不能改写/打码，否则补丁打不上。

为什么可以接受：
- 这两把密钥在 2026-10-27 的 P0 整改里**已经轮换**，旧值伪造的令牌实测返回 401；
- 该字面量本来就在**上游基线 commit `c4e8950` 的仓库历史里**（这也是 `.gitleaks.toml` 规则 3
  要保留 `cailu` 旧前缀的原因），本准备包没有产生新的暴露面。

因此提交本准备包时 pre-commit 的密钥扫描会命中它 —— 这是**已知且有意**的例外，
用 `git commit --no-verify` 提交，并在 commit message 里写明理由。
**不要把这两条补丁再往任何公开仓库推**。

## 打不上时的通用处理流程

```bash
# 1) 看是哪条失败、冲突在哪
bash 02-security/apply.sh            # 会自动停在失败处并打印冲突文件
git status; git diff

# 2) 先试 3way 手工合并（多数冲突只是上下文漂移）
#    改到「补丁想表达的终态」后：
git add -A && git am --continue

# 3) 改动太大：放弃该条，手工重做
git am --abort                       # 回到序列起点
git apply --reject 02-security/patches/00NN-*.patch   # 生成 .rej，逐个 hunk 手工处理
# 或：git show <原提交> -- <文件> 对照着抄

# 4) 每条补丁必须能这样验证（否则等于没打）
bash 06-verify/verify.sh --no-build
```

> 判断「这条补丁在 Pro 上还有没有意义」：看它的**为什么**那一段。
> 安全类的「为什么」如果在新基线里已经不成立（比如 Pro 已经把密钥全部环境变量化），
> 就以 `verify.sh` 的断言为准 —— 断言绿了就说明目标达成，不必强行把补丁打进去。
