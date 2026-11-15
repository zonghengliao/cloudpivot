# P2-59 `cloudpivot-kernel` 抽 crate 收口（C1 第一张）

> 卡：`t_71c1b4ec`（P2-59【P2-C】共享依赖收敛 → 按 `docs/planning/20261113-新卡清单.md` §P2-C.6 第 289 行**提前成第一张 = C1**）
> 日期：2026-11-15 · 机器：99（开发机）/ 仓：`/opt/cloudpivot` · 基线 `46f4d106`
> 上游依据：`docs/architecture/P2-58-shared抽crate收口.md`（§3 出边逐行 / §3.1 `test_helpers` 硬约束 / §8 施工单）
> 演练库：`cp_p259_kernel` @ **145**（`-T cloudpivot_test`，跑完即 DROP）
> 卡面落点 `cloudpivot-shared` **未采纳**（§P2-C.2 已定名 `cloudpivot-kernel`，见 P2-58 §1.1）

## 1. 做了什么（一句话）

把 `backend/app/src/{shared,utils,business/shared}` 三块**整体搬进独立 crate**
`backend/crates/cloudpivot-kernel/`（三者同一个包），并把内核里 10 处指向业务域 / 平台层的引用
**全部倒转成接缝或上提/下移**，让「域 → 内核」只剩一条向下箭头；余下的 1 254 行跨包 import 是机械替换。

```
cloudpivot-doc (L0，零依赖词表)
      ↑
cloudpivot-kernel (L1，本次新增：37 文件 / 22 332 行)
      ↑
cloudpivot (app，L2/L3：21 个业务域 + 装配层) ← 后续 C2~C15 继续拆
```

| 项 | 数值 | 口径 |
|:--|--:|:--|
| 搬迁文件 | **32** 个 `.rs` | 13 `shared` + 1 `utils` + 18 `business/shared`（与 C1 行逐字相等） |
| 搬迁行数 | **21 826** 行 | 7 119 + 323 + 14 384 |
| 新增文件 | 5 个 | `lib.rs`、`business/mod.rs`、`shared/config_port.rs`、`shared/session_flags_port.rs`、`shared/master_key.rs` |
| 内核现状 | 37 文件 / 22 332 行 | 搬家后 + 接缝/文档 |
| 跨包 import 改写 | **1 359 处 / 511 个文件**（另有集成测试 23 处 / 16 文件） | `crate::{shared,utils,business::shared}` → `cloudpivot_kernel::…`（含文档注释） |
| 路由 / 权限码 / 迁移 / 菜单 | 0 / 0 / 0 / 0 | 内核**零路由**，「拆前 − 0」天然成立 |
| 用例 | 内核 **239 passed**（原 236 + 新增 6 接缝用例 − 搬走 3 跨轴用例） | `cargo test -p cloudpivot-kernel`（真库/假库同绿，零 DB 断言入口） |

## 2. 10 行解环账：逐行怎么处置

P2-58 §3 冻结的 10 行（prod 8 / test 2）。施工顺序按依赖：先接缝、再测试、最后搬文件。

| # | 原位置 | 目标 | 处置 | 落点 |
|--:|:--|:--|:--|:--|
| 1 | `shared/startup_guard.rs:55` | storage | **主密钥解析上提**（AES 加解密留 storage） | `shared/master_key.rs`（新）；`business/storage/cipher.rs` 改 `pub use` 转发 |
| 2·3 | `shared/startup_guard.rs:808,809` | aiagent | **AI 安全红线巡检下移**成域侧函数，启动链调用 | `business/aiagent/safety.rs::warn_missing_safety_block`；`bootstrap::ai_agent_invariant_guard` 调一次 |
| 4 | `shared/idle_session.rs:49` | support | **配置读取接缝**（与 8/10 合并成同一个 port） | `shared/config_port.rs`（新）；实现 `support::config::model::SysConfigReader`，启动期安装 |
| 5 | `shared/session_cache.rs:110` | system | **会话字段读取接缝** | `shared/session_flags_port.rs`（新）；实现 `system::user::model::SessionFlagsReader` |
| 6 | `shared/observability.rs:427` | task | **跨轴一致性用例搬回装配层** | `app/src/kernel_axis_tests.rs`（用例名不变） |
| 7 | `shared/permission_alias.rs:111` | erp | 同上（③④ 两条一起搬） | `app/src/kernel_axis_tests.rs` |
| 8 | `business/shared/cost_switch.rs:47` | support | 同 #4（**一个** `config_port`，不是四个） | `config_port::read_text` |
| 9 | `business/shared/cost_switch.rs:48` | system | 菜单判定**不认 DTO**：抽 `MenuNode` trait，域侧 6 行 impl | `cost_switch::MenuNode` + `system/menu/dto.rs` 的 impl |
| 10 | `business/shared/separation.rs:66` | support | 同 #4 | `config_port` |

**口径要点（留给后面 14 张卡照抄）**

1. **一个 port 合并四行**：`read_text` / `read_i64` / `parse_bool` 三件事原本散在 4 行里，
   现在只有 `config_port` 一份；`support::config::model::parse_bool` 改成**转发**
   （`pub use cloudpivot_kernel::shared::config_port::parse_bool`），调用点一行没改。
2. **读接缝与写接缝的失败语义不同**：配置未安装 → `None` + 一次 `warn`（所有调用点都有兜底）；
   会话字段未安装 → `DbError::InvalidValue`（鉴权字段静默降级 = 只验签，绝不允许）。
3. **测试专用符号**用「trait 反转 + 域侧 impl」而不是复制 DTO：`MenuNode` 9 行。
4. **跨轴用例归装配层**：内核与域的「登记表 ⇄ 实现」断言放 `app/src/kernel_axis_tests.rs`，
   放内核里就是 test 配置下的包级环。

## 3. `test_helpers` / `for_test` / `stats`：三处 `cfg(test)` 特性化

抽包后**依赖方**（域包）编译内核时 `cfg(test)` 为假，这些符号根本不存在。四处一律改成
`#[cfg(any(test, feature = "test-support"))]`：

| 符号 | 谁在用 | 说明 |
|:--|:--|:--|
| `shared::test_helpers`（整个模块） | hr 92 行 / erp 40 行 / 全仓 132 行、39 文件 | 真库用例前置体检 |
| `ScopedPrincipal::for_test` | cost / hr / expense 等真库用例（构造任意数据范围） | 「闸 1」仍不许生产构造 |
| `user_name::stats` | AUD-02 的 SQL 计数断言 | thread-local 计数器 |
| `InvalidationGate::mark_dirty_for_test` | 缓存欠账用例 | |

打开方式：`backend/app/Cargo.toml` 的 `[dev-dependencies] cloudpivot-kernel = { …, features = ["test-support"] }`。
`test-support` 顺带引入 `dep:sha2`（夹具口令摘要），**生产构建里 sha2 与这些符号都不存在**。

> `shared/test_helpers.rs` 因此按 `scope_gate` 口径 v2 属「可能进生产」⇒ 照判，
> 文件头写了 `SCOPE-EXEMPT`（夹具必须看得见全库，数据范围对它无意义）。

## 4. 路径 / 门禁跟改（搬包会失效的部分）

| 位置 | 改了什么 | 为什么 |
|:--|:--|:--|
| `crates/cloudpivot-kernel/src/business/shared/cost_switch.rs:485/497` | `include_str!` 改 `concat!(env!("CARGO_MANIFEST_DIR"), "/../../app/migrations/…")` | 迁移仍在 `backend/app/migrations/`（运行时路径由 `config.toml` 决定，不动） |
| `.../business/shared/user_name.rs:324` | 回填脚本路径 `../` → `../../../` | 目录深了一层 |
| `.../business/shared/state_machine.rs:5830` | `join("../../app/migrations/…")` | 同上 |
| `.../shared/startup_guard.rs:1113` | `join("../../config")` | `config/` 仍在 `backend/` |
| `.../business/shared/layering_tests.rs` | 禁区域名仍从 **app** 的 `src/business/` 枚举 | 域都还在 app 里；读内核自己的 `src/business` 会让禁区变空 ⇒ 静默变绿 |
| `scripts/lint/scope_gate.py` | **双根**：app 根 + 内核根（各自的 `SCAN_DIRS` / `REQUIRED_PRODUCTION` / `SKIP_MANIFEST`）；`--self-test` 全绿 | 不扫内核 = 判定面少一半（`scope.rs` 自己就在内核里） |
| `scripts/lint/path_refs_gate.py` | `REF_RE` 认 `backend/crates/<crate>/src/…`；30 处失效引用改到新路径 | 不认新形态 = 这些引用**静默不被检查**，且旧形态②把尾巴误判成 `backend/app/src/…`（假红） |
| `scripts/check.sh` | 闸① 扫描根加内核 `src`，白名单改仓库相对全路径 | 否则「扫不到 scope.rs」⇒ 闸① 静默变绿 |
| `scripts/verify/crate_split_readiness.py` | 认内核根 + `cloudpivot_kernel::…` 两类边 | 否则 `--plan-order` 报「未知节点」、`--domain shared` 看不见出边 |
| 前端 2 个单测（`erp-shipment` / `erp-sales-return`） | `backendSource()` 认 `backend/…` 全路径 | 读源码做断言的用例要能找到内核文件 |

## 5. 验证（全部实跑，命令 + 结果）

```bash
$ cd backend && cargo build --workspace --offline
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2m 54s        # exit 0

$ cargo check --workspace --offline --all-targets                               # exit 0，零 warning

$ PGPASSWORD=… createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_p259_kernel
$ ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_p259_kernel" \
  cargo test -p cloudpivot-kernel --offline -- --test-threads=1
test result: ok. 239 passed; 0 failed; 0 ignored            # 4.27s
   Doc-tests cloudpivot_kernel: 5 ignored（全是 `ignore` 示例）

$ ERP_DB_URL=… cargo test -p cloudpivot --lib --offline -- --test-threads=1 \
    kernel_axis_tests business::hr::attendance_export_db_tests business::erp::expense::expense_model_tests::
test result: ok. 6 passed; 0 failed                          # 16.31s（含 3 条跨轴用例 + 3 条真库用例）

$ python3 scripts/lint/scope_gate.py --root .            # 双根 PASS（app 158 条跳过清单一致 / 内核 2 条一致）
$ python3 scripts/lint/scope_gate.py --self-test         # 全部通过（⑪⑫⑬ 负例仍红）
$ python3 scripts/lint/path_refs_gate.py                 # PASS（899 脚本 / 白名单 10 条 / 0 处失效）
$ python3 scripts/lint/path_refs_gate.py --self-test      # 自检结论：全绿
$ python3 scripts/verify/crate_split_readiness.py --plan-order docs/planning/20261113-新卡清单.md
    判定：绿 —— 卡序与实测边一致，每一步抽包后包图无环        # C1 = 0/10（解环账已付清）
$ python3 scripts/verify/crate_split_readiness.py --domain shared --layer-check   # exit 0（出边 0）
$ PGPASSWORD=… dropdb -h 172.18.10.145 -U cloudpivot cp_p259_kernel
```

`--plan-order` 里 C1 现在显示 `实际只 0 行 < 承诺 10（可下调承诺值）`——这是**付清**的证据：
10 行解环账已在施工时切完，卡序文档冻结的是**开工时**的值，工具按下调提示而不是判红。

## 6. 这次没做的事（边界）

- **没有**抽 C2（`cloudpivot-server`/bin）：`middleware` + 独立 bin 仍留在 `cloudpivot` 包里，
  所以现在 `cloudpivot` 同时是「内核的依赖者」和「域的宿主」——这在 C2 之前是**允许的中间态**
  （包图仍然无环：域还没单独成包）。C2 是其余 13 张卡的硬前置。
- **没有**动任何业务域：`erp` / `hr` / `system` 等源码只改了 import 前缀（1 359 处）与
  `ScopedPrincipal::for_test` 等符号的可见性，业务逻辑零改动。
- **没有**改 `AGENTS.md`（受保护）：它第 22/199/200 行仍写着 `src/shared/…`，补偿 diff 见
  `docs/patches/AGENTS-P2-59.diff`（人工 `git apply`）。
- **没有**跑全量 `cargo test --workspace`（5 681 条真库用例，需完整种子库）：改成
  `--all-targets` 编译 + 内核 239 条 + 抽样真库用例（见 §5）。全量回归留给 C2 之后的整体验收卡。

## 7. 下一张卡（C2）开工前必须知道的

1. `bootstrap::register_kernel_ports()` 是**新接缝的安装点**（`config_port` / `session_flags_port`），
   C2 把 `bootstrap.rs` 搬进 bin 包时，这两行要跟着走，否则运行期读参数一律 `None`。
2. `crates/cloudpivot-kernel` 的 `[features] test-support` 是**域包 `[dev-dependencies]` 的固定写法**，
   后面每抽一个域包都要照抄（C11 `hr` 的 92 行夹具引用最多）。
3. 内核的 `pub(crate)` 已全部改成 `pub`（跨包必需）。**不要**把这条当风格去「收敛可见性」：
   域包要用的符号必须 `pub`，只在内核内部用的请写 `pub(crate)`，编译器会替你抓漏。
4. `scope_gate` 的双根结构：下一个域 crate 抽出时，把它的 `src` 加进 `roots` 列表即可，
   别只改 `SCAN_DIRS`（那会漏扫新根）。
