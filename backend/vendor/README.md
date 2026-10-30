# vendor/ —— NIK 框架（fork 自 alun 0.4.2）

这 16 个 crate 是 **alun 0.4.2 的 fork + 改名版**，从本目录开始**独立演进**：
上游 `alun` 0.4.2 是不再跟随的基线，我们在这里加自己的修复与能力。

## 一、为什么是 16 个 crate（卡面写的是 8 个）

C1 卡面按「业务代码里出现的 crate 名」数了 8 个（alun / -macros / -db / -db-sql /
-db-core / -plugin / -utils / -web，35,224 行）。实际 `Cargo.lock` 里的 alun 依赖闭包是
**16 个 crate（实测 54,956 行 / 196 个 .rs）**，另外 8 个是被上面 8 个**直接依赖**的地基 crate。
（卡面那 35,224 是前 7 个之和，漏算了 alun-web 的 8,678 行；八者实为 43,902 行。）

```
alun            → alun-cache, alun-config, alun-core, alun-fs, alun-log, alun-macros
alun-db         → alun-db-core, alun-db-sql, alun-db-macro, alun-db-build, alun-db-driver-postgres
alun-web        → alun-cache, alun-config, alun-core, alun-db
alun-utils      → alun-core
alun-plugin     → alun-cache, alun-config, alun-core, alun-utils
```

**只改 8 个会直接编译不过**，两个硬原因：

1. **同一类型会出现两份实例**：`nik-db`（来自 alun-db）要拿 `Value` / `Db` / `Row` 去调
   `alun-db-driver-postgres`。若 driver 仍走 registry 的 `alun-db-core`，而我们 vendored 了
   `nik-db-core`，两边是**两个不同的 crate**，`Value` 是两种不相容的类型 → E0308 满地。
2. **宏生成代码的路径解析会断**：`alun-db-macro`（`resolve.rs`）把生成代码里写死的
   `::alun_db_core::…` 按调用方 `Cargo.toml` 里**实际声明的包名**重写为
   `::alun_db_core` / `::alun_db::__db_core` / `::alun::__db_core` 三选一。
   业务侧只声明门面（原 `alun`，现 `nik`），一旦门面改名，候选表里三个名字全都匹配不上，
   生成代码就退化成裸 `::alun_db_core` —— 业务 crate 根本没有这个依赖 → unresolved import。
   所以 `nik-db-macro` 的候选表必须同步改成 `nik-db-core` / `nik-db` / `nik`。

因此 C1 按**依赖闭包全量**改名（16 个），而不是只改 8 个。业务侧可见的只有 `nik` +
`nik-utils` 两个依赖，其余 14 个是它们的内部依赖。

## 二、改名映射

| 原 crate | 现在 | 说明 |
|---|---|---|
| alun | **nik** | 门面（prelude / re-export / App / AlunRouter） |
| alun-macros | **nik-macros** | 路由注解 / `#[routes]` / `#[plugin]` |
| alun-db | **nik-db** | DB 层（最大，71 文件） |
| alun-db-sql | **nik-db-sql** | SQL 片段构造 |
| alun-db-core | **nik-db-core** | Value / Row / 类型映射 |
| alun-db-macro | **nik-db-macro** | `#[derive(Model)]` / `sql!` / `migrate!` |
| alun-db-build | **nik-db-build** | build.rs 辅助（迁移快照） |
| alun-db-driver-postgres | **nik-db-driver-postgres** | PG 驱动 |
| alun-plugin | **nik-plugin** | 插件设施 + 调度器 |
| alun-utils | **nik-utils** | 工具集（xlsx / xss / 加密 / UA） |
| alun-web | **nik-web** | axum 封装层（App / 中间件 / SSE） |
| alun-cache | **nik-cache** | Redis 缓存 |
| alun-config | **nik-config** | 配置装载 |
| alun-core | **nik-core** | 错误 / 基础 trait |
| alun-fs | **nik-fs** | 文件存储（local / minio） |
| alun-log | **nik-log** | tracing 初始化 |

**没有 vendored 的**（框架 Cargo.toml 里存在、但本项目 feature 从不打开、registry 里也没有解包）：
`alun-kafka` / `alun-task` / `alun-template` / `alun-db-driver-mysql` / `alun-db-driver-sqlite`。
这 5 个在本 fork 里**也一并改名成 `nik-kafka` / `nik-task` / `nik-template` /
`nik-db-driver-mysql` / `nik-db-driver-sqlite`**（保持 optional + registry 依赖、不加 path），
所以 `vendor/` 的 `.rs` / `.toml` 里**一个 `alun` 字样都不剩**。它们永远不会被激活
（本项目 feature 不选它们），因此不需要源码也不需要 manifest。

## 三、改了哪些内容

1. 目录名：`vendor/nik*`（不再是 `alun-x-0.4.2`）。
2. `Cargo.toml`：`[package] name`、`[lib] name`（`alun_db` → `nik_db` 等）、
   所有内部依赖由 `version = "0.4.2"` 改成 `path = "../nik-xxx"`（保留 version，cargo 会校验）；
   每个 crate 加了 `publish = false`（防误发到 crates.io）。
3. 源码里的路径引用：`alun::` → `nik::`、`alun_db::` → `nik_db::`、`alun_web::` → `nik_web::` ……
   （含 `#[alun::get]` 这类属性路径）。
4. **宏生成代码里的路径**（关键）：
   - `nik-macros/src/route.rs` 生成 `::nik::AlunRouter` / `::nik::inventory::submit!` /
     `::nik::PermissionDef` / `::nik::NoAuthDef`；
   - `nik-macros/src/route.rs` 识别属性路径时判定的段名由 `alun` 改成 `nik`
     （否则 `#[nik::get]` 不会被认领）；
   - `nik-db-macro/src/resolve.rs` 的候选包名表 `alun-db-core` / `alun-db` / `alun` → `nik-*`。
5. crate 内自带的 tests / benches / examples 同步改名（`autotests = false` 时不参与构建，
   但源码保留、路径一致，将来打开就能用）。
6. **可观测性与内部名字也一起改干净**（fork 里不留 `alun` 字样）：
   - Prometheus 指标名前缀 `alun_http_*` / `alun_db_*` / `alun_cache_*` / `alun_raw_sql_*` /
     `alun_tenant_scope_*` → `nik_*`（`nik-web/src/metrics.rs`、`nik-db/src/metrics.rs`、
     `nik-cache/src/metrics.rs`）。**注意：本机没有 Prometheus / Grafana 抓这些指标**，
     所以改名没有大盘影响；将来若接了监控，schema 见 `nik-*/src/metrics.rs` 的模块文档表。
   - 迁移咨询锁键 `"alunmigr"`（ASCII `0x616C756E6D696772`）→ `"nikmigr\0"`
     （`nik-db/src/migrate/lock.rs`，只影响多实例并发迁移的互斥键，单实例部署无影响）。
   - 宏生成的内部变量名 `__alun_sql` / `__alun_params` / `__alun_app` → `__nik_*`
     （`nik-db-macro/src/sql_macro.rs`、`nik-macros/src/routes.rs`：只在宏展开里出现，
     业务代码看不到）。
   - benches / tests 里的零散名字（`new_alun_db`、`alundb_migrate_*` 临时目录、
     `/api/alun-body-limit-echo` 测试路由、文档里的示例应用名 `alun-ledger`）同样改 nik。

### 没改、也不该改的

- **公开类型名**：`AlunRouter`、`AlunError`、`AlunPlugin` 等保持原名。
  业务侧只换了 crate 前缀（`nik::AlunRouter`），如果连类型名一起改，会有一次无收益的大面积
  业务改动（C1 红线：只改 use 路径和 crate 名）。
- **环境变量前缀 `ALUN_`**（`ALUN_DATABASE_*` / `ALUN_PROFILE` / `ALUN_AUTH_QUERY_TOKEN_PATHS` …）
  与配置文件里的键：这是**部署契约**，改名前缀要同步改 `/etc/cloudpivot/*.env`、systemd unit、
  `docker/*` 与所有环境。留到后续卡（C2）统一处理。
- 数据库账号 `alun`（`config.toml` 的 `user`）：是 DB 事实，改名要动数据库。

## 四、安全补丁已内联（重要）

`scripts/patch-alun-web.sh` 早前对 **registry 里的 alun-web 0.4.2 源码**打了 6 处安全补丁。
本次 vendor 时**复制的是打过补丁的源码**，所以下面这些修复现在直接长在 `nik-web` 里，
不依赖任何 post-fetch 脚本：

| 标记 | 位置 | 修的是什么 |
|---|---|---|
| CODE-07 | `nik-web/src/app.rs` | `auth.enabled` 但 `jwt_secret` 为空时 fail-open → 改 fail-fast（panic 拒绝启动） |
| CODE-09 | `nik-web/src/middleware/permission_check.rs` | 免认证路径被权限规则拦成 403；匹配由「第一条命中」改为「完全相等 > 静态 > 模板」打分 |
| CODE-09b | 同上 | 规则遮蔽自检收敛到「同一路径不同权限」，不再把兄弟路由误报冲突 |
| CODE-10 | `nik-web/src/middleware/auth.rs` | 支持 `?token=` 认证（仅白名单路径，给浏览器 EventSource 用） |
| CODE-11 | 同上 | 忽略列表支持模板段 `{id}` |
| CODE-11b | 同上 | 模板段只匹配「像 id」的段（`page`/`my`/`gantt` 不再被放行） |

`scripts/patch-alun-web.sh` 因此**对构建不再有意义**（registry 里的 alun-* 已经不参与编译），
保留它只作为补丁内容的可读文档；`scripts/patch-alun-web.sh --check` 现在只反映 registry 副本的状态。

## 五、构建方式

用 **path 依赖**，不需要 `.cargo/config.toml` 的 source replacement：

```toml
nik = { path = "vendor/nik", features = [...] }
nik-utils = { path = "vendor/nik-utils" }
```

（cargo 的 `source replace` 只能整体替换 crates.io，做不到「只替换某几个包」；
path 依赖正好表达「只有这 16 个走本地源码、其余仍走 registry」，也不会让仓库变成几十 MB。）

## 六、验收口径（可逐条复核）

```bash
cd backend
ls vendor | grep -c '^nik'                                              # 16
grep -rn 'alun' vendor --include='*.rs' --include='*.toml' | wc -l      # 0
grep -rn 'alun' src | wc -l                                            # 0
cargo tree -p nik | grep -c alun                                       # 0
cargo tree | grep -c alun                                              # 0（整棵依赖树）
cargo test --lib -j 1 | grep 'test result'                             # 874 passed / 0 failed
```

> `vendor/*.md`（本文件与 `FORK-NOTES.md`）里仍然会出现 `alun` —— 那是 fork 来源说明，
> 不参与编译；如果把这两个文件也算进去会得到 42 处，属预期。

## 七、独立演进约定

- 上游是 0.4.2，**不会**再 `cargo update` 跟上：升级 = 手工比对上游 diff 后移植。
- 在这里修框架 bug 就**直接改**，并在 `/tmp/cpx-fork-notes.md`（或后续 docs）留一行
  「症状 → 原因 → 修法」，业务侧不用再加 `patch-*.sh`。
- 改 `nik-*` 的公开 API 时要顺带跑业务侧 `cargo test --lib`（874 条）。
