# C1 技术债 / fork 记录：nik → nik

日期：2026-10-30　分支：`feat/nik-fork`（基于 `7c43e7f`）
执行者：backend-developer（Kanban t_7d9adc6d）

---

## 1. 一句话

把框架从「crates.io 上的 nik 0.4.2」变成「仓库里自己的 `backend/vendor/nik*`」：
16 个 crate 全部改名 + 内部依赖改 path 依赖 + 宏生成代码路径同步，业务侧 190 个文件
（882 行命中 / 5,069 处标识符）机械替换，`cargo check` 0 error，
`cargo test --lib` 874 passed / 0 failed（基线同数，见 §6），release 构建通过。

## 2. 为什么 vendor 全量 16 个而不是卡面的 8 个

`Cargo.lock` 里 nik 闭包是 16 个 crate（54,956 行 / 196 .rs）。卡面 8 个里
`nik-db` / `nik-web` / `nik-plugin` / `nik-utils` 都直接依赖那 8 个「地基」
（nik-core / -config / -cache / -fs / -log / -db-core / -db-sql / -db-macro / -db-build /
-db-driver-postgres）。只改上面 8 个会当场炸两类：

1. **类型分裂**：`nik-db-core::Value` 与 registry `nik-db-core::Value` 是两个 crate 的两个类型，
   `nik-db` 把前者传给 `nik-db-driver-postgres`（后者）→ E0308；
2. **宏路径解析断链**：`nik-db-macro/src/resolve.rs` 把生成代码里写死的 `::nik_db_core::…`
   按**调用方 Cargo.toml 实际声明的包名**重写成 `nik-db-core` / `nik-db` / `nik` 三选一；
   业务只声明门面，门面一改名三个候选全不中，生成代码退化成裸 `::nik_db_core` → unresolved import。

所以按闭包全量改。业务侧 Cargo.toml 依旧只有两个框架依赖：`nik` + `nik-utils`。

## 3. 改了哪些地方（可复核）

| 位置 | 改动 | 为什么 |
|---|---|---|
| `backend/vendor/nik*`（16 个目录） | 从 `~/.cargo/registry/src/rsproxy.cn-*/nik-*-0.4.2` 复制，去掉 `Cargo.toml.orig/.cargo-ok/.cargo_vcs_info.json/Cargo.lock` | vendor 源码入库，从此独立演进 |
| 每个 `vendor/nik*/Cargo.toml` | `[package] name`、`[lib] name`（`nik_db`→`nik_db`…）、内部依赖加 `path = "../nik-x"`（保留 `version` 让 cargo 校验）、加 `publish = false` | 包名 + 源码 crate 名 + 依赖解析三处必须同时改 |
| `vendor/nik-macros/src/route.rs` | `quote!` 里的 `::nik::NikRouter` / `::nik::inventory::submit!` / `::nik::PermissionDef` / `::nik::NoAuthDef` → `::nik::…`；**属性段名判定** `s == "nik"` → `"nik"` | 生成代码路径 + `#[nik::get]` 能否被认领 |
| `vendor/nik-db-macro/src/resolve.rs` | 候选包名表 `nik-db-core`/`nik-db`/`nik` → `nik-*`，占位标识 `nik_db_core`/`nik_db` → `nik_*` | 见 §2 第 2 条 |
| `vendor/*` 全部 `.rs` / `.md` | `nik::`→`nik::`、`nik_db::`→`nik_db::`、`nik_web::`→`nik_web::` 等（含 crate 内自带的 tests/benches/examples） | 路径一致性 |
| `backend/Cargo.toml` | `nik = { version="0.4.2", features=[…] }` → `nik = { path="vendor/nik", features=[…] }`；`nik-utils = { version="0.4.2" }` → `nik-utils = { path="vendor/nik-utils" }` | 只保留两个框架入口依赖 |
| `backend/{src,tests,examples}` 169 个 `.rs` | 同样的标识符替换（`\balun\b` 词边界，不影响 `NIK_*` 环境变量） | 业务侧 882 行命中 |
| `backend/vendor/README.md` | 新增：fork 来源、16 个映射表、安全补丁内联清单、构建方式、演进约定 | 技术债落地 |

**没有 `.cargo/config.toml`**：cargo 的 source replacement 只能整体替换 crates.io，
做不到「只替换这 16 个、其余走 registry」；path 依赖等价且不会让仓库膨胀（vendor 仅 2.6 MB）。

## 4. 顺手保住的安全补丁（重要）

`scripts/patch-nik-web.sh` 早前对 registry 里的 nik-web 0.4.2 打过 6 处安全补丁
（CODE-07 fail-open、CODE-09 / 09b 权限规则遮蔽、CODE-10 `?token=`、CODE-11 / 11b 模板段放行）。
vendor 时**复制的是打过补丁的源码**（复制前 `--check` 显示 `PATCH-STATE: OK`），
所以这些修复现在直接编译进 `nik-web`。`patch-nik-web.sh` 从此对构建无效（registry 里的
nik-* 不再参与编译），保留作为补丁内容的可读文档。

## 5. 明确不改（避免破坏契约）

1. **环境变量前缀 `NIK_`**（`NIK_DATABASE_*` / `NIK_PROFILE` / `NIK_AUTH_QUERY_TOKEN_PATHS` …）
   与 `config.toml` 的键、DB 账号 `nik`：这是部署契约，改要同步 `/etc/cloudpivot/*.env`、
   systemd、`docker/`。留给 C2。
2. **公开类型名** `NikRouter` / `NikError` / `NikPlugin` 等：C1 只改 crate 名与 use 路径，
   改类型名会让 190 个业务文件全部产生无收益 diff。
3. `docs/` / `migrations/` / `frontend/src` 注释里仍写 `#[nik::permission]`（纯文案）。
4. 框架自带的 3 条编译警告（`nik-web` deprecated `ApiBody`、`nik-db` 两个 unused fn）未动。

## 6. 验证证据（全部真跑）

| 项 | 命令 | 结果 |
|---|---|---|
| 改名完整性 | `grep -rn 'nik' backend/src/ \| wc -l` | `0` |
| vendor 目录数 | `ls backend/vendor \| grep -c '^nik'` | **16** |
| vendor 源码残留 | `grep -rn 'nik' backend/vendor --include='*.rs' --include='*.toml' \| wc -l` | **0** |
| 依赖树残留 | `cargo tree -p nik \| grep -c nik` / `cargo tree \| grep -c nik` | **0 / 0** |
| 编译 | `cargo check`（isolated `CARGO_TARGET_DIR=/root/nik-target`） | **0 error**，1m00s |
| lint | `cargo clippy --all-targets -- -D warnings` | **exit 0** |
| 格式 | `cargo fmt -- --check` | 干净（改名后 import 顺序变化已 fmt 收口） |
| 单测 | `cargo test --lib -j 1` | **874 passed / 0 failed** |
| 基线对照 | 同版本 main（`7c43e7f`，clone 到 `/root/main-base`）跑同一条命令 | **874 passed / 0 failed** → 一条不少 |
| 闸门链 | `bash scripts/check.sh` | **全绿**：fmt/clippy/check/typecheck + 闸①=0 闸②=0 + C7 G1–G6 + 色彩一致性 |
| release | `cargo build --release` | 成功（3m35s → 二次 1m49s/1m54s） |
| 部署 | 备份旧二进制 → 装新 → `systemctl restart cloudpivot` | `/health` 200；日志 `target` 已是 `nik_web::app` / `nik_plugin::scheduler` / `nik_core::plugin` |
| 真机冒烟 | `python3 /root/c1_smoke.py` | **6/6 PASS**（health / login / auth-me / 权限层 / 任务列表 / 审批待办） |

> 卡面写的是 857。实测基线（main `7c43e7f`）已经是 **874**：857 是更早一次测的，
> E11（任务模板 / 周期任务 / 燃尽图）等卡又加了用例。判据按「改名前后同数」执行：874 → 874。

## 7. 运营补充后的三遍补扫（16 个 crate 那一条之外）

liuzi 中途补了要求：「不止 8 个，16 个一起改；vendor 里彻底无 nik；依赖树里无 nik」。
第一遍已经按 16 个做（见 §2），另补了三遍**收口**：

| 遍 | 改了什么 | 影响面 |
|---|---|---|
| 第二遍 | vendor 里残余的 `alun_*` 标识符：Prometheus 指标名前缀（`nik-web/src/metrics.rs`、`nik-db/src/metrics.rs`、`nik-cache/src/metrics.rs`）+ bench 函数名 + 测试库名 | 运行时唯一可见的是指标名；本机无 Prometheus/Grafana，无大盘依赖 |
| 第三遍 | 5 个「从不激活」的可选 crate 名（kafka/task/template/mysql/sqlite 驱动）也改名 `nik-*`（仍 optional + registry，不加 path，因此不需要源码）；迁移咨询锁键 `"alunmigr"` → `"nikmigr\0"`；测试路由 `/api/nik-body-limit-echo`、`alundb_migrate_*` 临时目录、文档示例名 `nik-ledger` | 咨询锁键只影响多实例并发迁移的互斥（单实例部署无影响，已在代码注释里写明） |
| 第四遍 | 宏生成的内部变量名 `__alun_sql` / `__alun_params` / `__alun_app` → `__nik_*`（`nik-db-macro/src/sql_macro.rs`、`nik-macros/src/routes.rs`）+ `new_nik_db` bench 函数名 | 宏展开内部可见，业务代码看不到 |

只剩 `vendor/README.md` / `FORK-NOTES.md` 两个**说明文档**里还有 `nik`（42 处，讲 fork 来源），
不参与编译，也过 `--include='*.rs' --include='*.toml'` 那条验收。

## 8. 运行期行为修正（vendor 侧 bug fix）

fork 之后对框架**语义**的每一次改动都记在这里（与 §4 的「安全补丁」分开：那些是复制时就带上的）。

| 日期 | 文件 | 改动 | 为什么 / 证据 |
|---|---|---|---|
| 2026-11-04 | `nik-cache/src/lib.rs` · `LocalCache::incr` | 自增前先判 `expires_at`：**过期条目按不存在处理**（先删再从 0 加），并对齐 Redis `INCR` 语义 | 旧实现只做加法、不看 TTL，`TTL 已到但尚未被清理`的计数键会继续累加，而 `expires_at` 早在过去 ⇒ 计数永久停在阈值之上。登录限流因此在无 Redis（进程内缓存）下 10 次登录后永久 429，只能重启进程（卡 `t_91a8d427`，已在 18150 实例实机复现并修复后复测）。单测：`test_incr_after_expiry_restarts_from_zero` / `test_incr_keeps_window_ttl` |

约定：改完 vendor 行为必须同时补 crate 内单测（`cargo test -p nik-cache`），
并在业务侧留一条能复现原故障的回归用例——否则下次 vendor 同步会悄悄退回旧语义。
