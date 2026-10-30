# C1 技术债 / fork 记录：alun → nik

日期：2026-10-30　分支：`feat/nik-fork`（基于 `7c43e7f`）
执行者：backend-developer（Kanban t_7d9adc6d）

---

## 1. 一句话

把框架从「crates.io 上的 alun 0.4.2」变成「仓库里自己的 `backend/vendor/nik*`」：
16 个 crate 全部改名 + 内部依赖改 path 依赖 + 宏生成代码路径同步，业务侧 190 个文件
（882 行命中 / 5,069 处标识符）机械替换，`cargo check` 0 error，
`cargo test --lib` 874 passed / 0 failed（基线同数，见 §6），release 构建通过。

## 2. 为什么 vendor 全量 16 个而不是卡面的 8 个

`Cargo.lock` 里 alun 闭包是 16 个 crate（54,956 行 / 196 .rs）。卡面 8 个里
`alun-db` / `alun-web` / `alun-plugin` / `alun-utils` 都直接依赖那 8 个「地基」
（alun-core / -config / -cache / -fs / -log / -db-core / -db-sql / -db-macro / -db-build /
-db-driver-postgres）。只改上面 8 个会当场炸两类：

1. **类型分裂**：`nik-db-core::Value` 与 registry `alun-db-core::Value` 是两个 crate 的两个类型，
   `nik-db` 把前者传给 `alun-db-driver-postgres`（后者）→ E0308；
2. **宏路径解析断链**：`alun-db-macro/src/resolve.rs` 把生成代码里写死的 `::alun_db_core::…`
   按**调用方 Cargo.toml 实际声明的包名**重写成 `alun-db-core` / `alun-db` / `alun` 三选一；
   业务只声明门面，门面一改名三个候选全不中，生成代码退化成裸 `::alun_db_core` → unresolved import。

所以按闭包全量改。业务侧 Cargo.toml 依旧只有两个框架依赖：`nik` + `nik-utils`。

## 3. 改了哪些地方（可复核）

| 位置 | 改动 | 为什么 |
|---|---|---|
| `backend/vendor/nik*`（16 个目录） | 从 `~/.cargo/registry/src/rsproxy.cn-*/alun-*-0.4.2` 复制，去掉 `Cargo.toml.orig/.cargo-ok/.cargo_vcs_info.json/Cargo.lock` | vendor 源码入库，从此独立演进 |
| 每个 `vendor/nik*/Cargo.toml` | `[package] name`、`[lib] name`（`alun_db`→`nik_db`…）、内部依赖加 `path = "../nik-x"`（保留 `version` 让 cargo 校验）、加 `publish = false` | 包名 + 源码 crate 名 + 依赖解析三处必须同时改 |
| `vendor/nik-macros/src/route.rs` | `quote!` 里的 `::alun::AlunRouter` / `::alun::inventory::submit!` / `::alun::PermissionDef` / `::alun::NoAuthDef` → `::nik::…`；**属性段名判定** `s == "alun"` → `"nik"` | 生成代码路径 + `#[nik::get]` 能否被认领 |
| `vendor/nik-db-macro/src/resolve.rs` | 候选包名表 `alun-db-core`/`alun-db`/`alun` → `nik-*`，占位标识 `alun_db_core`/`alun_db` → `nik_*` | 见 §2 第 2 条 |
| `vendor/*` 全部 `.rs` / `.md` | `alun::`→`nik::`、`alun_db::`→`nik_db::`、`alun_web::`→`nik_web::` 等（含 crate 内自带的 tests/benches/examples） | 路径一致性 |
| `backend/Cargo.toml` | `alun = { version="0.4.2", features=[…] }` → `nik = { path="vendor/nik", features=[…] }`；`alun-utils = { version="0.4.2" }` → `nik-utils = { path="vendor/nik-utils" }` | 只保留两个框架入口依赖 |
| `backend/{src,tests,examples}` 169 个 `.rs` | 同样的标识符替换（`\balun\b` 词边界，不影响 `ALUN_*` 环境变量） | 业务侧 882 行命中 |
| `backend/vendor/README.md` | 新增：fork 来源、16 个映射表、安全补丁内联清单、构建方式、演进约定 | 技术债落地 |

**没有 `.cargo/config.toml`**：cargo 的 source replacement 只能整体替换 crates.io，
做不到「只替换这 16 个、其余走 registry」；path 依赖等价且不会让仓库膨胀（vendor 仅 2.6 MB）。

## 4. 顺手保住的安全补丁（重要）

`scripts/patch-alun-web.sh` 早前对 registry 里的 alun-web 0.4.2 打过 6 处安全补丁
（CODE-07 fail-open、CODE-09 / 09b 权限规则遮蔽、CODE-10 `?token=`、CODE-11 / 11b 模板段放行）。
vendor 时**复制的是打过补丁的源码**（复制前 `--check` 显示 `PATCH-STATE: OK`），
所以这些修复现在直接编译进 `nik-web`。`patch-alun-web.sh` 从此对构建无效（registry 里的
alun-* 不再参与编译），保留作为补丁内容的可读文档。

## 5. 明确不改（避免破坏契约）

1. **环境变量前缀 `ALUN_`**（`ALUN_DATABASE_*` / `ALUN_PROFILE` / `ALUN_AUTH_QUERY_TOKEN_PATHS` …）
   与 `config.toml` 的键、DB 账号 `alun`：这是部署契约，改要同步 `/etc/cloudpivot/*.env`、
   systemd、`docker/`。留给 C2。
2. **公开类型名** `AlunRouter` / `AlunError` / `AlunPlugin` 等：C1 只改 crate 名与 use 路径，
   改类型名会让 190 个业务文件全部产生无收益 diff。
3. `docs/` / `migrations/` / `frontend/src` 注释里仍写 `#[alun::permission]`（纯文案）。
4. 框架自带的 3 条编译警告（`nik-web` deprecated `ApiBody`、`nik-db` 两个 unused fn）未动。

## 6. 验证证据（全部真跑）

| 项 | 命令 | 结果 |
|---|---|---|
| 改名完整性 | `grep -rn 'alun' backend/src/ \| wc -l` | `0` |
| 编译 | `cargo check`（isolated `CARGO_TARGET_DIR=/root/nik-target`） | **0 error**，1m00s |
| lint | `cargo clippy --all-targets -- -D warnings` | **exit 0** |
| 格式 | `cargo fmt -- --check` | 干净（改名后 import 顺序变化已 fmt 收口） |
| 单测 | `cargo test --lib -j 1` | **874 passed / 0 failed** |
| 基线对照 | 同版本 main（`7c43e7f`，clone 到 `/root/main-base`）跑同一条命令 | **874 passed / 0 failed** → 一条不少 |

> 卡面写的是 857。实测基线（main `7c43e7f`）已经是 **874**：857 是更早一次测的，
> E11（任务模板 / 周期任务 / 燃尽图）等卡又加了用例。判据按「改名前后同数」执行：874 → 874。
