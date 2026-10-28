# 验收清单（06-verify/acceptance.md）

> 分两层：
> **A 静态断言**（`bash 06-verify/verify.sh --no-build` 全自动，读代码判定，几十秒）
> **B 运行时验证**（必须在部署机上真发请求，静态断言证明不了「真的挡住了」）
>
> 历史事实：A 层 20 项 + B 层 6 项在 2026-10-27 的部署验收里全部通过
> （伪造超管令牌 401、旧 refresh 复用 401、首登 403 等均有实测记录）。

---

## A. 安全断言（14 项，脚本自动判定）

| # | 断言 | 判定命令（verify.sh 内） | 为什么重要 |
|--:|------|--------------------------|-----------|
| 1 | 配置文件无明文 `jwt_secret` | `! grep -rnE '^\s*jwt_secret\s*=\s*"[^"]+"' backend/config/*.toml` | 密钥进 git = 任何人可伪造令牌 |
| 2 | 启动守卫存在 | `test -f backend/src/shared/startup_guard.rs` | 配置不自洽必须拒绝启动 |
| 2b | 启动守卫 fail-closed | grep `拒绝启动`/`process::exit` | fail-open 等于没有守卫 |
| 2c | 启动链真的调用守卫 | `grep startup_guard backend/src/lib.rs` | 写了但没接 = 摆设 |
| 3 | `config.toml` 无 docker 默认口令 | `! grep -E '^\s*password\s*=\s*"alun"'` | 默认口令=可直接连库 |
| 3b | 库口令为空串（走 env） | `grep -E '^\s*password\s*=\s*""'` | 同 1 |
| 4 | gitleaks 三级防线 4 件套 | `.gitleaks.toml` / `secret-scan.sh` / `pre-commit` / CI yml | 提交时 + CI + 历史回扫 |
| 5 | 规则 3 同时匹配新旧前缀 | `grep '(?:cloudpivot\|cailu)-' .gitleaks.toml` | 少一半等于把历史泄露藏进扫描器 |
| 6 | 用户管理提权拦截 | `user/guard.rs` 存在 + `handler.rs`/`role/handler.rs` 调用 | 三条提权路径（建号/改密/授角色） |
| 7 | 首登强制改密（后端） | `must_change_password` 进 constants + session_guard | 初始口令不改就能长期用 |
| 7b | 未改密用户只放行改密/登出 | session_guard 里有改密路径判断 | 否则改密页也进不去（死锁） |
| 8 | 首登改密前端闭环 | `change-password.vue` + router 守卫 | 只有后端 403 → 前端满屏报错 |
| 9 | 会话吊销中间件 | `middleware/session_guard.rs` | 停用/改密后旧令牌必须立刻失效 |
| 9b | `token_version` 迁移在位 | V0022 up.sql 含 `token_version` | 令牌版本比对的数据基础 |
| 10 | 短 TTL + 绝对上限 | `access=900` / `refresh=86400` | 令牌被偷的时间窗 |
| 10b | 同上（refresh） | 见 10 | refresh 轮换不滑动 |
| 11 | refresh 复用检测 | `grep reuse_detected auth/handler.rs` | 令牌被复制使用要能发现并全撤 |
| 11b | 认证层做版本比对 | `auth_guard.rs` 有 `token_version`/`ver` | 复用检测的另一半 |
| 12 | Vite 默认只听 127.0.0.1 | `grep VITE_HOST frontend/vite.config.ts` | dev 服务器+code-server 不暴露公网 |
| 13 | prod compose 密钥是必需 env | `ALUN_MIDDLEWARE_AUTH_JWT_SECRET:?` | 缺失即启动失败，不静默用默认值 |
| 13b | prod compose 无明文口令 | 同上正则取反 | 同 3 |
| 14 | patch-alun-web 脚本在位 | `test -f scripts/patch-alun-web.sh` | 框架层 fail-open 补丁的入口 |
| 14b | 判重按补丁代码签名 | `grep 'let mut best'` | 改名后不会重复插补丁 |
| 14c | registry 里补丁在位 | `scripts/patch-alun-web.sh --check` → `PATCH-STATE: OK` | 脚本在≠补丁打了 |

> 严格计数：`verify.sh` 里按「一行一个断言」实现，共 26 条 check（含 2b/2c/3b/... 这些子项），
> 编号沿用需求里的「14 项安全整改」，子项是每项的落地证据。

## B. 运行时验证（部署机上人工执行，静态断言替代不了）

```bash
API=https://<host>/api ; TOKEN=<超管登录后的 access_token>

# B1 伪造超管令牌（用已轮换的旧 dev 密钥自签）→ 必须 401
#    历史实测：旧密钥伪造的令牌返回 401（可利用性归零）
curl -s -o /dev/null -w "%{http_code}\n" -H "Authorization: Bearer $FORGED" $API/system/user/page
#   期望 401

# B2 首登强制改密：must_change_password=TRUE 的用户登录后
psql -d <db> -c "UPDATE sys_user SET must_change_password=TRUE WHERE username='<u>';"
curl -s -o /dev/null -w "%{http_code}\n" -H "Authorization: Bearer $USER_TOKEN" $API/system/user/page
#   期望 403；改密接口 200；改完再访问 200

# B3 会话吊销：管理员重置该用户口令后，旧令牌 → 401
# B4 refresh 复用：登录 → refresh(A)→B → 再用 A refresh → 401，且该用户全部会话失效
# B5 提权拦截：普通管理员 POST /system/user 带 role_codes=["SUPER_ADMIN"] → 403
#    （guard.rs 单测已覆盖同样逻辑，B5 是端到端复验）

# B6 缓存不可用即拒绝（fail-closed）：stop redis 后任何带令牌请求 → 401，而不是放行
systemctl stop redis-server && curl -s -o /dev/null -w "%{http_code}\n" -H "Authorization: Bearer $TOKEN" $API/system/auth/me
systemctl start redis-server
```

## C. 功能断言（脚本自动）

| # | 断言 | 说明 |
|--:|------|------|
| F1 | `business/monitor` 四件套完整 | 后端模块不残缺 |
| F2 | 接口权限码 == 迁移里的 `auth_code` | 不一致 → 超管也看不到状态卡 |
| F3 | 所有迁移 `up`/`down` 成对 | 回滚能力 |
| F4 | 状态卡仅超管渲染（`isSuperAdmin`） | 非超管不发请求、看不到主机指标 |
| F5 | 莫兰迪主色 `#51697C` 生效 | 视觉一致 |
| F6 | logo/favicon 已替换 | 品牌一致 |

## D. 改名残留（脚本自动，`01-rename/verify.sh --strict`）

- 只允许两类残留：`.gitleaks.toml` 规则 3（1~3 行，**有意保留**）、`docs/ERP-批次1-*.md`（历史文档）
- 文件名不得含 `cailu`（`find -iname '*cailu*'` 为空）
- `rename.sh --dry-run` 二跑必须 0 改动（幂等）
- 外部上游 URL `wild1024/cailu` 若出现必须原样保留

## E. 构建

| # | 断言 | 通过标准 |
|--:|------|----------|
| E1 | `cargo test --all-targets` | 45 passed / 0 failed（新增 8 个 monitor 单测 + 守卫/提权单测在内） |
| E2 | `pnpm typecheck`（`vue-tsc --noEmit`） | 零错误 |

## F. 环境准备（容易漏，漏了 E1 必挂）

`backend/config/config-dev.toml` 与 `config-test.toml` 是 **`.gitignore` 掉的**，
从 `*.example.toml` 生成（`bash prepare-config.sh`）。干净 checkout 里没有这两个文件时，
`shared/startup_guard.rs::merge_semantics_or_base_and_profile` 会 FAILED（本机实测 44 passed / 1 failed）。
**这不是准备包的 bug，是「未入库配置」设计的必然步骤** —— 但它同时也是个真实缺陷：
这条测试依赖未跟踪文件，纯 CI 环境（不带本地配置）会红。建议后续把它改成「读内嵌的配置文本」而不是读磁盘。

## 一句话结论模板

> 静态 26 项 + 功能 6 项 + 改名残留 4 项 + 构建 2 项全通过；运行时 B1~B6 在部署机实测通过。
> 唯一的已知缺口：`merge_semantics_or_base_and_profile` 依赖未入库配置文件（F 节）。
