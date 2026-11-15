# P2-40：建 Cargo workspace 骨架（`backend/` 变 workspace 根，主包搬进 `backend/app/`）

> 结论先说：`backend/` 现在是虚拟 workspace 根（`members = ["app", "crates/*"]`），主 binary crate
> 在 `backend/app/`（包名仍是 `cloudpivot`，产物仍是 `backend/target/debug|release/cloudpivot`）。
> `cargo build --workspace` 3m02s 绿；`cargo test --lib` **5681 passed / 0 failed**（应用 crate，与拆前
> 一字不差）**+ 19 passed**（词表 crate，P2-40 起它也是 workspace 成员）；`scripts/check.sh --fast` 全绿。
> 仓外表面（脚本/钩子/前端用例/Docker/文档）同步了 437 个文件 2063 处路径引用。

## 1. 为什么这件事单开一张卡、独占窗口

拆 workspace 不是「改一行 Cargo.toml」：`backend/src` 这个路径在仓库里被 **2000+ 处**引用
（脚本 596、前端 197、文档 1261、Docker 1、根文件 3）。搬目录之后**绝大多数引用不会报错，而是静默失效**：

- `grep -rn 'FROM t' backend/src/...` → 0 行命中，闸判绿；
- `cargo test --lib business::erp::x` → `0 passed`，退出码 0；
- `include_str!("../../../../migrations/…")` → 编译期红（唯一会主动报错的一类）。

所以本卡的工作量其实是「搬 + 把每一处静默失效改回硬失败/真路径」。同窗口跑其他卡会互相踩
（共同的 `target/` 锁、共同的 `git` 工作区），因此排他执行。

## 2. 布局（before → after）

```
before                          after
backend/                        backend/                     ← workspace 根（虚拟清单，无 [package]）
  Cargo.toml      (package)       Cargo.toml                 ← [workspace] members/exclude/resolver
  Cargo.lock                      Cargo.lock                 ← 不变（全 workspace 一份）
  target/                         target/                    ← 不变（产物路径不变）
  rustfmt.toml                    rustfmt.toml               ← 不变（crates/* 也吃这份，向上查找命中）
  config/                         config/                    ← 不变（运行时输入，cwd 相对）
  uploads/ downloads/             uploads/ downloads/        ← 不变（cwd 相对）
  vendor/  (16 个 path 依赖)      vendor/                    ← 不变，但**显式 exclude**（不是成员）
  crates/cloudpivot-doc/          crates/cloudpivot-doc/     ← 不变，但现在**是**成员
  src/                            app/src/
  tests/                          app/tests/
  examples/                       app/examples/
  build.rs                        app/build.rs
  migrations/                     app/migrations/
```

`app/Cargo.toml` 的 path 依赖随之改成 `../vendor/nik`、`../vendor/nik-utils`、`../crates/cloudpivot-doc`。

## 3. 三个决定（都有代价，写清为什么）

### 3.1 `migrations/` 跟着 `src/` 一起进 `app/`（不是留在 `backend/`）

因为源码里有 **89 处**「按源码相对路径 / `CARGO_MANIFEST_DIR` 定位迁移」的用法
（`include_str!` 60 处 + `join("migrations")` / `concat!` 29 处）：

```rust
// src/business/erp/cost/cost_settle_finish.rs（搬迁后仍是同一行，一个字都没改）
const V0376_UP: &str = include_str!("../../../../migrations/business/V0376__stock_in_production.up.sql");
// src/business/erp/doc_reader_impl_tests.rs
let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
```

`src` 与 `migrations` 一起平移 ⇒ 相对深度不变 ⇒ **这 89 处零改动**。
只搬 `src` 就得把每条 `include_str!` 的 `../` 数量 +1、把 `join("migrations")` 改成 `join("../migrations")`：
改动量更大，且每条都是一次「编译过了、运行期才错」的机会。

代价：运行时迁移目录变了，所以 `config/config.toml` 的 `[database.migration] path` 由 `"migrations"`
改成 `"app/migrations"`（**cwd 相对**），Dockerfile 的 `COPY backend/migrations/` 改成
`COPY backend/app/migrations/`。其余运行时路径（`config/`、`uploads/`、`downloads/`）坐标未变。

### 3.2 `vendor/` 必须逐条 `exclude`，**不能用 `vendor/*`**

Cargo 的默认规则是「workspace 目录内的 path 依赖自动成为成员」。不排除的后果不是编译错，
而是 `cargo fmt --check` / `cargo clippy --workspace -D warnings` 去格式化和 lint **第三方源码**
（16 个 nik 包），L1 立刻红，且红在别人的代码上。

实测（cargo 1.94.1）：`exclude` 的 glob **不生效** ——

```bash
$ cargo metadata --no-deps --format-version 1 | jq -r '.workspace_members[]' | grep -c vendor
# exclude = ["vendor/*"]  → 16      ← 照样是成员
# exclude = ["vendor/nik", …16 条…] → 0   ← 只有逐条列名才排干净
```

所以 `backend/Cargo.toml` 里逐条列了 16 个。**新增/删除 vendor 包时要同步改那张表**，漏掉的会静默变成员
（注释里已写明这条纪律）。

### 3.3 `Cargo.lock` / `target/` / `config/` 留在 `backend/`（兼容面，刻意不搬）

- `Cargo.lock` 的位置由 workspace 根决定 = `backend/Cargo.lock`（不变）⇒ `--locked`、Docker、
  deploy 的「锁文件变了要重编译」判据全部不用改；
- `target/` 默认落在 workspace 根 = `backend/target`（不变）⇒ 所有脚本里
  `backend/target/debug/cloudpivot`、`target/release/cloudpivot` 这类路径**一个字都不用改**；
- `config/` 是**部署输入**不是包源码，且 `config.toml` 里的相对路径（`uploads`/`downloads`）都按
  进程 cwd 解析。进程 cwd 约定仍是 `backend/`（systemd / docker / scripts 都先 `cd backend`）⇒ 不动。

反过来说，这也是为什么没有把 workspace 根提到仓库根：那样 `Cargo.lock` 与 `target/` 会各上移一级，
几百处 `backend/target/...` 与 Docker 的 `COPY --from=builder /app/backend/target/...` 全部要改。

## 4. Rust 侧真正改了的地方（10 处，全是「越出包目录」的引用）

| 文件 | 改法 | 为什么 |
|---|---|---|
| `app/build.rs` | `cargo:rerun-if-changed=../.git/HEAD` → `../../.git/HEAD` | build script 的相对路径按包根（`app/`）解析 |
| `app/src/business/hr/hr_invariants_tests.rs` | `repo_root()`：`.parent()` → `.ancestors().nth(2)`；2 处 `"backend/migrations/…"` → `"backend/app/migrations/…"` | 仓库根从「包根上一级」变成上两级 |
| `app/src/business/hr/hr_e2e_contract_tests.rs` | 同上（`repo_root()`） | 同上 |
| `app/src/business/hr/release_drill_contract_tests.rs` | 同上 | 同上 |
| `app/src/business/attachment/tests.rs` | 同上 | 同上 |
| `app/tests/invariant_registry.rs` | 同上 | 同上 |
| `app/tests/hr_04_menu_perms_migration.rs` | 同上 | 同上 |
| `app/src/business/shared/user_name.rs` | `include_str!(concat!(MANIFEST, "/../scripts/…"))` → `/../../scripts/…` | 回填脚本在仓库根 |
| `app/src/system/menu/tests.rs` | `frontend_root()`：`../frontend` → `../../frontend` | 前端仓库根是包根的兄弟的兄弟 |
| `app/src/business/erp/linkage/tests_presets_coverage.rs`、`app/src/business/hr/perm_matrix_tests.rs` | `include_str!` 的 `../` +1 级（5→6、4→5） | 引用仓库根的脚本（`scripts/verify/…`） |

另有两处**不是**编译期能抓的，属运行期口径，本卡一并修：

- `app/src/shared/startup_guard.rs` 的 `merge_semantics_or_base_and_profile`：
  `cargo test` 的 cwd 是**包根**（现在是 `backend/app/`），`DEFAULT_CONFIG_DIR = "config"` 会指向
  `app/config`（不存在）→ 用例假红。改为按 `CARGO_MANIFEST_DIR/../config` 解析；**生产不动**
  （进程 cwd 仍是 `backend/`）。
- `app/src/support/invariant/runner.rs` 的 `resolve_script()`：补一条 `../../scripts/verify/erp_invariants.py`
  候选（cwd=包根时的口径），避免巡检脚本在 `cargo test` 口径下静默 `None` → 落 ERROR 行。

## 5. 仓库外围的同步（这部分才是本卡的量大头）

| 面 | 文件 | 替换处 | 说明 |
|---|--:|--:|---|
| `scripts/**`、`.githooks/**`、`.github/**` | 107 | 596 | 含 `os.path.join(root,"backend","src")`、`repo/"backend"/"src"` 两种拼法 |
| `frontend/**`（含 `frontend/tests`，会被 path_refs_gate 扫） | 129 | 197 | 多为注释里的出处；`frontend/tests` 里是 `readFileSync(new URL(...))` 真读 |
| `docker/` | 1 | 1 | `COPY backend/migrations/` → `COPY backend/app/migrations/` |
| `docs/**`（不含 `docs/evidence`、`docs/review`、`docs/patches`） | 197 | 1261 | 见 §7「冻结区」 |
| `skill/SKILL.md` | 1 | 5 | AI 速查 |
| 根文件（`llms.txt` 等） | 2 | 3 | — |

### 5.1 顺手补掉的两个「静默假绿」（不改就会变成空闸）

1. `scripts/verify/clean_checkout_gate.sh`：`cargo test --lib --no-run` 在 workspace 根会输出**两个**
   `Executable unittests src/lib.rs (…)`，脚本用 `tail -1` 取二进制 → 会取到**词表 crate 的 19 条**
   而不是应用 crate 的 5681 条。闸还是绿的，全量单测却没跑。已改成 `cargo test -p cloudpivot --lib --no-run`。
2. `scripts/deploy/deploy.sh`：`BE_CHANGED` 判据里 `grep -vE '^backend/(migrations|tests)/'` 与
   `git diff … backend/Cargo.toml backend/Cargo.lock` —— 新布局下「只改了 `app/Cargo.toml` 的依赖」
   会被判成「后端无改动」而不重编译。已改为 `^backend/app/(migrations|tests)/`，并把
   `backend/app/Cargo.toml`、`backend/crates` 加进清单文件监视。

同类还需要人看一眼的：`scripts/lint/path_refs_gate.py` 的 `REF_RE` / `NORM_RULES` 与
`SCAN_ROOTS`（后端的测试面从 `backend/tests` 变成 `backend/app/tests`）已同步，判据自检 `--self-test` 全绿。

## 6. 验证（真实执行输出）

```bash
# ① 验收命令 1：workspace 全量构建
$ cd backend && cargo build --workspace
   Compiling cloudpivot v0.1.0 (/opt/cloudpivot/backend/app)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3m 02s

# ② 验收命令 2：全量测试面编译（含 tests/ 集成测试与 examples）—— 退出码 0
$ cd backend && cargo test --workspace --no-run
   Finished `test` profile [unoptimized + debuginfo] target(s) in … 
   Executable unittests src/lib.rs (target/debug/deps/cloudpivot-…)
   Executable unittests src/lib.rs (target/debug/deps/cloudpivot_doc-…)
   Executable tests/hr_04_menu_perms_migration.rs (…)   # 以及其余 tests/*

# ③ 单测条数与拆前一致（离线口径：真库用例自行 SKIP）
$ cd backend && cargo test --lib -- --test-threads=1
test result: ok. 5681 passed; 0 failed; 0 ignored     # 应用 crate（拆前同为 5681）
test result: ok.   19 passed; 0 failed; 0 ignored     # 词表 crate（P2-40 起也是成员）

# ④ 格式 / 成员面
$ cd backend && cargo fmt -- --check && echo FMT-OK
FMT-OK
$ cargo metadata --no-deps --format-version 1 | jq -r '.workspace_members[]'
path+file:///opt/cloudpivot/backend/app#cloudpivot@0.1.0
path+file:///opt/cloudpivot/backend/crates/cloudpivot-doc#0.1.0      # vendor 16 个均不在列
$ cargo metadata --no-deps --format-version 1 | jq -r .target_directory
/opt/cloudpivot/backend/target                                       # 产物路径未变

# ⑤ 静态闸（L0）—— 搬家后最容易静默失效的那批
$ python3 scripts/lint/path_refs_gate.py
结论：PASS（扫描 898 个脚本，白名单 10 条，0 处失效路径）
$ python3 scripts/lint/path_refs_gate.py --self-test
自检结论：全绿（判据可用）
$ python3 scripts/lint/scope_gate.py
闸② …扫描生产面 630 个文件 / 跳过测试面 160 个文件 … PASS：0 处
$ python3 scripts/lint/changelog_in_tx.py
G5 PASS：检查了 33 个调用点（扫了 790 个文件）
$ python3 scripts/lint/sys_change_log_gate.py
★ 生产面 0 命中：sys_change_log 只有 business/changelog/ 在碰。
$ python3 scripts/lint/palette_def_sync.py
PASS：99 套色板与 V0399__preference_palette_ids_gallery77.up.sql 的 ui.palette oneof 逐字一致
$ python3 scripts/lint/migration_version_unique.py
✓ 迁移版本号跨目录唯一：660 个文件 / 324 对 up-down / 404 个版本，最大 V0404
$ python3 scripts/verify/role_code_gate.py
✓ 无**新增/改动**的未知角色码（历史基线全部按指纹豁免）
$ python3 scripts/verify/scope_skip_manifest_audit.py
登记 160 条；未登记但被跳过 0 条；登记了但未跳过 0 条

# ⑥ 一键门禁
$ bash scripts/check.sh --fast
全部检查通过（L0 L1）
```

## 7. 没做的事 / 需要人跟一行

1. **`AGENTS.md` 没改**（受保护文件，需人工批准）。改动已落成补丁：
   `docs/patches/AGENTS-workspace.diff`（`git apply --check` 已过）。它包含目录树、常用命令、
   依赖铁律三条的更新。注意它与仍未 apply 的 `docs/patches/AGENTS-doc-crate.diff` 在「目录」块重叠，
   **两份都要时以本补丁的目录块为准**（它已经包含 crates/ 一段）。
2. **`docs/evidence/**`、`docs/review/**`、`docs/patches/**` 按设计冻结**：那是某个时间点的证据快照与
   待 apply 的补丁，改路径等于篡改历史。里面仍会出现 `backend/src/...`，读的时候按「当时的布局」理解。
3. **`migration-kit/**`** 是历史迁移工具包（含 `*.patch` 与设计笔记），同上，不动。
4. **仓库外的验证脚本**：`/root/.hermes/profiles/laoliu/scripts/deploy_check_v3.sh`（99 上的闭环验证）
   仍写死 `backend/migrations/business/*.up.sql`、`backend/src/`、`backend/target/release`。
   它不在本仓、不归本卡改，但**它会静默查 0 条迁移**。改法 3 行：`backend/migrations` → `backend/app/migrations`、
   `backend/src/` → `backend/app/src/`（`backend/target/release` 不用改）。
5. `llms.txt` 里除路径外的其它内容已经陈旧（还写着 `alun` 框架、迁移封顶 `V0017`），本卡只修了路径，
   框架改名与版本号另属别的账。
6. `scripts/deploy/selftest_migrate_plan.sh` 需要 `/etc/cloudpivot/cloudpivot.env`（只在 144 有），
   99 上跑不了；上线前在 144 跑一次。

## 8. 给 P2-41 ~ P2-58（逐域抽 crate）的约定

- 新 crate 一律放 `backend/crates/cloudpivot-<域>/`，`members = ["crates/*"]` 已覆盖，**不用改根清单**；
- 应用 crate 的依赖声明在 **`backend/app/Cargo.toml`**（不是 `backend/Cargo.toml`）；
- 抽走的域若自带迁移/CSS 等静态资源，放**新 crate 自己目录**，别把 `app/migrations` 当共享盘；
- 抽 crate 会改变源码根：**凡是靠 `CARGO_MANIFEST_DIR` 或源码相对路径读文件**（`include_str!`、`repo_root()`、
  `path_refs_gate` 的扫描面）都要跟着改 —— 本卡踩过的 10 处就是范本；
- 抽完之后 `cargo tree -p <crate>` 才第一次能表达「域间依赖方向」，那才是 P2-B 想要的判据（模块级
  `module_cycle_report` 到时候与 crate 级互为交叉验证）。
