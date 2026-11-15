# P2-31 解环验证：`shared` 无环 + 全量编译（证据）

> 批次：P2-B（`P2-27 ~ P2-31`，解 `shared → erp`）· 卡：`t_bf023a43` · 日期：2026-11-15
> 口径：**能复核的原始输出**，不写「已验证」三个字了事。每条结论后面跟命令与结果。

## 0. 结论表

| 判据 | 命令 | 实测结果 |
|:--|:--|:--|
| crate 级无环 | `cargo tree -p cloudpivot-doc` | 只有根节点，**零出边**（§1.1） |
| crate 级唯一边 | `cargo tree -p cloudpivot -i cloudpivot-doc` | `cloudpivot-doc ← cloudpivot`，**无第二条边**（§1.2） |
| crate 级解析无环 | `cargo metadata` | 465 个包解析成功；`cloudpivot-doc` 的 `resolve.deps` = `[]`（§1.3） |
| 模块级：`shared → erp` 消失 | `python3 scripts/lint/module_cycle_report.py --edge business::shared business::erp` | **不存在**（§2.2） |
| 模块级：`shared` 不在任何环里 | 同上 `--from business::shared` | 出边 **0 条**，SCC 里不含 `shared`（§2.3） |
| 文字证据 | `grep -rn 'business::erp' backend/src/business/shared/ \| wc -l` | `0` |
| 全量编译 | `cargo check --lib` / `--all-targets` / `test --lib --no-run` | 13.02s / 24.65s / 2m24s，**零 warning**（§4） |
| 受影响模块测试 | `cargo test --lib business::shared::` | **129 passed / 0 failed**（真库，§5.1） |
| 词表 crate 测试 | `cd crates/cloudpivot-doc && cargo test` | **19 passed / 0 failed**（§5.2） |
| 提交门禁 | `bash scripts/check.sh --fast` | L0 + L1 **全绿**（§5.3） |
| 护栏有效性（反证） | 往 `shared/` 塞一条 `business::erp` 引用 | 护栏**判红**并点名到行；删掉即转绿（§3.3） |

**一句话**：`shared → erp` 这条回边在 crate 级（词表已进零依赖 `cloudpivot-doc`）、模块级（出边归零）、
护栏级（19 个业务域全禁区 + 红绿自测）三个层面都已消失，且全量编译与门禁全绿。

---

## 1. crate 级：`cargo tree`

### 1.1 `cloudpivot-doc` 是叶子（零出边）

```console
$ cd backend && cargo tree -p cloudpivot-doc
cloudpivot-doc v0.1.0 (/opt/cloudpivot/backend/crates/cloudpivot-doc)

$ cargo tree -p cloudpivot-doc -e normal
cloudpivot-doc v0.1.0 (/opt/cloudpivot/backend/crates/cloudpivot-doc)
```

输出只有根节点 → `[dependencies]` 为空 → 它不可能指向 `cloudpivot`，环在 crate 级物理上不成立。

### 1.2 反向依赖只有 `cloudpivot` 一条

```console
$ cargo tree -p cloudpivot -i cloudpivot-doc
cloudpivot-doc v0.1.0 (/opt/cloudpivot/backend/crates/cloudpivot-doc)
└── cloudpivot v0.1.0 (/opt/cloudpivot/backend)
```

`-i`（invert）列的是「谁依赖它」：只有 `cloudpivot` 一个。**没有第二条边** —— 这就是卡面要的
「该依赖边已消失」的 crate 级证据：`shared` 不再拥有自己的 crate 指向 `erp`，而词表 crate 的出边为 0。

### 1.3 `cargo metadata` 解析成功（465 包）

```console
$ cargo metadata --format-version 1 >/dev/null; echo $?
0
```

解析结果（脚本读同一次 JSON）：`packages = 465`、`resolve.nodes = 465`；
`cloudpivot-doc` 的 `deps = []`；`cloudpivot` 的直接 `cloudpivot*` 依赖只有 `cloudpivot_doc`。

> **为什么这条也算证据**：cargo 对 crate 级循环依赖是**硬错误**（`cyclic package dependency`），
> `cargo tree` / `cargo metadata` 能跑完就等于「依赖图里没有环」，不需要额外工具。

### 1.4 ⚠ 诚实说明：`cargo tree` 表达不了模块级边

`backend` 现在是**单 crate**（workspace 化是 P2-C/P2-40 的活）。`business::shared` 与 `business::erp`
是同 crate 的两个模块，它们之间的依赖**不在** cargo 的图里 —— `cargo tree` 永远看不到那条边。
所以 P2-30 与本卡都没有伪造 `cargo tree` 里的「模块边消失」输出，而是另做了模块级检测（§2）。

---

## 2. 模块级：新增 `scripts/lint/module_cycle_report.py`

### 2.1 它做什么

从 `backend/src/**/*.rs` 全文抽 `crate::a::b::c` 引用（**含注释** —— intra-doc link 是 rustdoc 依赖），
把每个文件归到「业务域」节点（`business/<域>`），用 **Tarjan** 求强连通分量。

- 扫描面硬断言：少于 **500** 个 `.rs` 直接报错退出 —— 防「路径基准变了 → 扫 0 个文件 → 静默无环」
  （写脚本时真踩到过一次：`__file__` 定位到 scratch 目录，扫出 0 个文件却打印「0 个环」）。
- 默认只看 `business::*` 节点；`--all-containers` 才把 `system/support/utils/task/middleware`
  当节点（容器之间的合法分层边会把整张图粘成一个大 SCC，失去诊断价值）。
- 注释命中单独标 `[comment]`：`shared` 里 `/business/erp/cost-switch` 这类 **HTTP 路径**、
  `erp.cost.enabled` 这类**配置键**是字符串常量，不构成依赖；但 `` `crate::business::x::y` `` 这种
  **文档链接**是，必须改。

用法定稿（**只读报告，不进 `check.sh`**）：P2-32~39 还有 4 处环要收，现在挂门禁必红；
等 P2-B 全批收口后再考虑进 L0（那时用 `--fail-on-cycle`）。

### 2.2 `shared → erp`：不存在

```console
$ python3 scripts/lint/module_cycle_report.py --edge business::shared business::erp
源码根：/opt/cloudpivot/backend/src
扫描面：762 个 .rs / 节点 20 个 / 跨域有向边 58 条 （口径：仅 business::*）

边 business::shared → business::erp：**不存在**（该域内无任何 crate:: 引用指向 business::erp）
```

### 2.3 `shared` 出边归零，且不在任何环里

```console
$ python3 scripts/lint/module_cycle_report.py --from business::shared
扫描面：762 个 .rs / 节点 20 个 / 跨域有向边 58 条

business::shared 的出边：0 条

SCC 环（长度 >1）：1 个
  ● 10 个域互为可达：business::aiagent ↔ business::announcement ↔ business::changelog ↔
    business::erp ↔ business::flow ↔ business::hr ↔ business::impexp ↔ business::masking ↔
    business::notice ↔ business::task
```

`shared` 不在这个 SCC 里 —— 它现在是**纯汇点**（只有别人依赖它，它不依赖任何业务域）。

文字层面的同一条证据：

```console
$ grep -rn 'business::erp' backend/src/business/shared/ | wc -l
0
```

---

## 3. 本轮改动（1 处代码注释 + 1 处护栏强化 + 1 个工具）

### 3.1 `backend/src/business/shared/conn.rs`：摘掉最后一条注释回边

P2-30 清完 `state_machine` 后，`shared` 还有**一条注释级回边**（本卡新发现）：

```rust
//! ② 写入路径的签名（[`crate::business::changelog::record`]）一个字都没改，
```

intra-doc link 会被 rustdoc 解析成 `business::changelog` 的引用 —— 与 P2-30 判 `state_machine`
那两处文档链接同一个口径。改成文件路径散文（并已核对 `business/changelog/mod.rs:117` 确有 `pub async fn record`）：

```rust
//! ② 写入路径的签名（`business/changelog/mod.rs::record`）一个字都没改，
```

改完 `--from business::shared` 从「1 条（注释）」变成「0 条」。

### 3.2 `backend/src/business/shared/layering_tests.rs`：禁区从 1 个域扩到**全部**业务域

P2-30 建这条护栏时 `FORBIDDEN_DOMAINS = ["erp"]`（当时只有这一个越界者）。本卡改成
**从 `backend/src/business/` 动态枚举**（排除 `shared` 自己）：

- 新增业务域**自动**进禁区，不用回来改常量（否则新域一建，护栏就漏）；
- 扫描面两道硬断言：`MIN_DOMAINS = 18`（实测 19 个域）+ `REQUIRED_FILES` 5 个必扫文件；
- 扫描改成递归（现在 `shared/` 是平的，将来分子目录也不漏）；
- 判据串仍用 `format!("business::{domain}")` 拼，护栏文件自身不会成为命中点。

红字报错里会列出全部 19 个禁区域，定位到「文件:行:原文」。

### 3.3 护栏红绿自测（反证：它不是装饰）

往 `shared/` 临时塞一个文件，只写一行 `business::erp::period::ensure_period_writable`：

```console
$ cargo test --lib business::shared::layering_tests -- --test-threads=1
test business::shared::layering_tests::shared_never_references_business_domains ... FAILED

business/shared 是跨域基座，不许依赖任何业务域（禁区 19 个：business::aiagent / … / business::task）：
/opt/cloudpivot/backend/src/business/shared/zz_redproof_probe.rs:3: //! 引用 business::erp::period::ensure_period_writable —— 这是模块路径 token，不是 HTTP 路径。
test result: FAILED. 0 passed; 1 failed
```

删掉该文件后：

```console
$ cargo test --lib business::shared::layering_tests -- --test-threads=1
test business::shared::layering_tests::shared_never_references_business_domains ... ok
test result: ok. 1 passed; 0 failed; 5586 filtered out
```

### 3.4 `scripts/lint/module_cycle_report.py`（新增）

见 §2.1。`path_refs_gate` 已把它算进扫描面（脚本总数 897 → 898，0 处失效路径），说明新脚本里的
路径引用都是真路径。

---

## 4. 全量编译（`cargo check`）

```console
$ cd backend && cargo check --lib
    Checking cloudpivot v0.1.0 (/opt/cloudpivot/backend)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 13.02s

$ cargo check --lib --all-targets
    Checking cloudpivot v0.1.0 (/opt/cloudpivot/backend)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 24.65s

$ cargo test --lib --no-run
   Compiling cloudpivot v0.1.0 (/opt/cloudpivot/backend)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2m 24s
  Executable unittests src/lib.rs (target/debug/deps/cloudpivot-3d829db500975524)
```

三次都**零 warning**（`-D warnings` 由 `check.sh --fast` 的 clippy 那一档兜底，见 §5.3）。
`--all-targets` + `--no-run` 证明的不只是 lib：**5587 条 lib 单测的测试代码也全部编译通过**。

---

## 5. 测试与门禁

### 5.1 `business::shared::`（受影响模块 · 真库）

库：`172.18.10.145:5432` 上从模板库 `cloudpivot_test` 克隆的 `cp_t_bf023a43_verify`（本次已删）。

```console
$ ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_t_bf023a43_verify" \
    cargo test --lib business::shared:: -- --test-threads=1
test result: ok. 129 passed; 0 failed; 0 ignored; 0 measured; 5458 filtered out; finished in 4.29s
```

129 与 P2-30 的 `shared` 计数一致（含本卡扩写后的 `layering_tests`，用例数未变）。

### 5.2 `cloudpivot-doc` crate

```console
$ cd backend/crates/cloudpivot-doc && cargo test
running 19 tests
test result: ok. 19 passed; 0 failed
   Doc-tests cloudpivot_doc: running 0 tests ... ok
```

### 5.3 `bash scripts/check.sh --fast`（L0 + L1，pre-push 同款）

```console
==> 库存三本账唯一写入口闸：扫描 614 个非测试 .rs（豁免 147）… ★ 零越界
==> 路径引用门禁 --self-test：13/13 全绿（判据可用）
==> 路径引用门禁：PASS（扫描 898 个脚本，白名单 10 条，0 处失效路径）
==> 闸⑧ 迁移授权段角色码：自检 39/39；324 个 .up.sql 无未知角色码
==> L0 /metrics 自检：本机无部署证据 → 跳过（开发机正常）
==> 全部检查通过（L0 L1）
```

> L1 档含 `cargo fmt --check` / `clippy --all-targets -D warnings` / `cargo check` /
> `crates/cloudpivot-doc` 的 fmt+clippy / 前端 `typecheck`，全绿即「零 warning」的正式证据。

### 5.4 未做的事（如实说明）

- **没跑全量 5587 条 lib 单测**：按 `docs/测试卫生-真库单测分库跑法.md`，全库串跑会 60+ 条假红，
  需要按模块分库分进程；本卡改动面（`shared` 注释 + 护栏 + 一个新脚本）不触及其他域。
  已用 `cargo test --lib --no-run` 保证**全部测试代码编译通过**，用 §5.1 保证受影响模块真绿。
- **没跑 L2/L3**（真库闸 + 干净检出闸）：属发版前 `check.sh --full`，本卡只做 push 前门禁。

---

## 6. 还没收口的环（后续卡的活，不是本卡的遗漏）

`--mutual` 口径：**8 对双向边**（P2-27~31 的 `shared` 已不在其中）：

| 双向边 | 代码处数 | 归属卡 |
|:--|:--|:--|
| `erp ⇄ flow` | 132 / 131 | P2-32 ~ P2-34（flow 定义 `DocReader` trait，erp 实现注入） |
| `erp ⇄ hr` | 0 / 6（erp 侧只有 1 处注释） | P2-35 ~ P2-36 |
| `flow ⇄ task` | 11 / 13 | **已完成（P2-37 + P2-38）**：`task → flow` = 0（P2-37），`flow → task` = 0（P2-38）；写法 `flow::task_port` + `task::approval_port` 双接缝，实现在中立接线域 `business::taskflow`。见 `P2-37-task去掉对flow的直调.md` / `P2-38-flow去掉对task的直连.md` |
| `impexp ⇄ masking` | 8 / 9 | **已完成（P2-39）**：`masking → impexp` = 0（导出定义搬去 `impexp/builtin/user_export.rs`、导出 e2e 搬去 `impexp/export_mask_tests.rs`）；反方向保留且只认接缝 `masking::masker::Masker`（`format_export_rows` 收 `&dyn Masker`）。见 `P2-39-impexp与masking解环.md` |
| `flow ⇄ hr` | 10 / 5 | 随 P2-32~36 一起收（`hr` 侧调 `flow` 的建流程 + 回调） |
| `hr ⇄ impexp` | 18 / 11 | **未排卡**（P2-39 只收 `impexp ⇄ masking`）：HR 侧导入走通用 impexp、导出走自己的 `/business/hr/*/export`，两边形状不同，需单卡设计（见 `P2-39-impexp与masking解环.md` §6） |
| `erp ⇄ changelog` | 6 / 0（changelog 侧只有 1 处注释） | 随 P2-32~34 收（`erp` 写变更日志，注释回指 erp） |
| `announcement ⇄ notice` | 5 / 0（notice 侧只有 1 处注释） | 未排卡，体量极小（注释回边） |

> 上表只做**基线登记**：P2-32~39 收口后应只剩 0 对，届时 `--mutual` 即可当门禁用。

---

## 7. 复核清单（照抄能跑）

```bash
cd /opt/cloudpivot

# ① crate 级：词表 crate 零出边、只有一条反向边
cd backend && cargo tree -p cloudpivot-doc && cargo tree -p cloudpivot -i cloudpivot-doc && cd ..

# ② 模块级：shared 出边为 0、不在任何环里
python3 scripts/lint/module_cycle_report.py --from business::shared
python3 scripts/lint/module_cycle_report.py --edge business::shared business::erp

# ③ 文字层面
grep -rn 'business::erp' backend/src/business/shared/ | wc -l      # 期望 0

# ④ 全量编译
cd backend && cargo check --lib --all-targets && cargo test --lib --no-run

# ⑤ 受影响模块 + 护栏（真库：99 上连 145，145 上连 loopback）
PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4 createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_p231_verify
ERP_DB_URL="postgres://cloudpivot:fbb1263691ff75c31ec99eaf258138b4@172.18.10.145:5432/cp_p231_verify" \
  cargo test --lib business::shared:: -- --test-threads=1
PGPASSWORD=fbb1263691ff75c31ec99eaf258138b4 dropdb -h 172.18.10.145 -U cloudpivot cp_p231_verify

# ⑥ 词表 crate + 提交门禁
cd crates/cloudpivot-doc && cargo test && cd /opt/cloudpivot
bash scripts/check.sh --fast
```
