# P2-5B · 库存写入口 CI 门禁（加固 + pre-commit）

卡片：`t_5f9f0ca2`　承接：`t_f088a8db`（P2-5A 收口，提交 `8f0a161`）

## 一句话结论

P2-5A 已随收口落地了闸门脚本与 `check.sh` 接线，但**判据本身有 3 类可绕过的写法**（跨行 SQL /
schema 限定 / 加引号标识符）以及 1 类**按文件名子串误豁免**（`"test" in name`）。本卡把判据加固到
扛得住换写法，新增 `--self-test` 红/绿语料（22 条）把判据本身纳入回归，并把闸门接进
`.githooks/pre-commit`（与密钥扫描同一道提交闸）。实测：旧闸门对同一处真实越界写入**放行
（EXIT=0）**，新闸门**拒绝（EXIT=1）**。

## 验收项对照（逐条实测）

| 验收标准 | 结果 | 证据 |
|---|---|---|
| 检查脚本能跑 | ✅ `python3 scripts/verify/inventory_write_gate.py` → 扫 341 个非测试 `.rs`（豁免 61 个测试文件），写入口 22 条写语句，**0 处越界**，`EXIT=0`；连跑两轮 | `gate-round1.txt` / `gate-round2.txt` |
| 故意加个直写，脚本能抓（贴实测） | ✅ 往**真实生产文件** `stockout_model.rs` 塞 4 类直写，**4/4 全部抓红**，还原后 sha256 与基线一致、工作树仍绿 | `neg-probe-real-tree.txt` |
| git commit | ✅ 见提交记录（`.githooks/pre-commit` 生效后第一条真实提交即通过钩子） | 本文件所在提交 |
| 加进 CI 或 pre-commit | ✅ 两者都做：`scripts/check.sh`（CI 入口，新增 `--self-test` 前置）+ `.githooks/pre-commit`（`git config core.hooksPath .githooks`） | `hook-probe.txt`、`scripts/check.sh:305-330` |

## 改了什么（文件 + 关键点）

| 文件 | 变更 |
|---|---|
| `scripts/verify/inventory_write_gate.py` | 判据重写：①`strip_comments()` 字符串感知的注释剥离（保留 SQL 字面量、抹掉 `/* */`）；②整段扫描 + `\s+` 跨行，行号用 `bisect` 从偏移反算；③判据覆盖 schema 限定（`public.` / `"public".` / `{schema}.`）、可选双引号、源码里的 `\"` 转义、`TRUNCATE` / `COPY`；④`_cfg_test_spans()` 按**花括号配对**只跳过 `#[cfg(test)]` 项本身；⑤测试文件豁免改为**明确命名** `TEST_NAME_RE`；⑥新增 `--self-test`（22 条红/绿语料，并校验红例必须给出合法 `表名+行号`）；⑦`--json` 字段不变（`ok/scanned/exempt_files/writer_statements/violations`） |
| `.githooks/pre-commit` | 新增「闸 2 · 库存三本账唯一写入口」，在密钥扫描之后；fail closed（python3/脚本缺失即拒绝）；`SKIP_INVENTORY_GATE=1` 可单独关闭；拒绝时打印改法与文档指针 |
| `scripts/check.sh` | 闸段前新增 `--self-test`（判据被改坏先红），注释同步为新的豁免口径 |
| `docs/后端二开指南.md` §十四 | 闸门三处生效（check.sh / pre-commit / 手动）+ 新增「别换写法绕过闸门」清单，同步新豁免口径 |
| `docs/evidence/20261105-p2-5b-inventory-gate/*` | 本目录证据 |

### 判据口径（现在拦什么）

拦（红）：
- `INSERT INTO` / `UPDATE` / `DELETE FROM` / `TRUNCATE [TABLE]` / `COPY` + 三张表；
- 跨行（关键字与表名不在同一行）；
- `public.biz_inventory`、`{schema}.biz_inventory`、`"biz_inventory"`、源码里的 `\"biz_inventory\"`；
- `#[cfg(test)]` **模块之后**追加的函数（旧实现从这里到文件末尾全豁免）。

放行（绿，故意的）：
- 明确命名的测试文件：`tests.rs` / `tests_support.rs` / `test_*.rs` / `*_test.rs` / `*_tests.rs` / `*_tests_*.rs` / `*_test_support.rs`；
- 文件内 `#[cfg(test)]` 标记项的**花括号体内部**；
- `//` 与 `/* */` 注释里的 SQL；
- 非目标表（`biz_inventory_po` 等）与只读 `SELECT`。

## 负例实测（真实生产文件，不是合成语料）

`neg-probe-real-tree.txt` —— 每次都从原始字节重写、按 sha256 还原：

| 负例 | 注入位置 | 期望 | 实测 |
|---|---|---|---|
| 1 · 朴素直写 | `stockout_model.rs` 追加 `UPDATE biz_inventory SET qty = qty` | 红 | `✗ 1 处越界 · ...:1009 [biz_inventory]` `EXIT=1` |
| 2 · 跨行 + `public.` + 双引号 | 同上，`UPDATE public.\"biz_inventory\"\n SET qty = qty` | 红 | `✗ ... [biz_inventory] UPDATE public. "biz_inventory` `EXIT=1` |
| 3 · `TRUNCATE`（旧判据没这个关键字） | 同上，`TRUNCATE TABLE biz_inventory_txn` | 红 | `✗ ... [biz_inventory_txn]` `EXIT=1` |
| 4 · 文件名只是含 `test` 子串 | 新建 `erp/latest_stock_probe.rs` | 红 | `✗ backend/src/business/erp/latest_stock_probe.rs:3 [biz_inventory]` `EXIT=1` |
| 还原后 | — | 绿 | `★ 零越界` `EXIT=0`，sha256 前后一致：`65e2979f…046a` |

## 旧闸门 vs 新闸门（同一处越界，`old-vs-new.txt`）

把 `git show 8f0a161:scripts/verify/inventory_write_gate.py` 取出来，和加固版跑**同一处**注入：

```
--- 旧闸门（git 8f0a161）---   ★ 零越界 …            OLD_EXIT=0   ← 放行了真实越界写入
--- 新闸门（本卡加固后）---     ✗ 有 1 处越界写入 …    NEW_EXIT=1   ← 抓住
```

同一批 22 条语料喂给旧实现：**15 条红例里漏 8 条**（schema 限定 / schema 占位符 / 双引号 /
跨行 ×2 / 转义引号 / TRUNCATE / COPY）。

## pre-commit 实测（`hook-probe.txt`）

```
core.hooksPath = .githooks
干净工作树直接跑钩子        → EXIT=0（放行）
暂存一处直写后 git commit   → EXIT=1（被拒），报错含文件:行号:表名 + 改法 + 文档指针
清理后                      → 无残留条目，EXIT=0
```

注：该钩子同时保留原有的密钥扫描；本机未装 `gitleaks`，扫描器只告警不失败（`secret-scan.sh`
原有行为，未改）。`core.hooksPath` 是**本机 .git/config 设置，不随提交分发**，每个克隆需执行一次
`git config core.hooksPath .githooks`。

## 连跑两轮

`gate-round1.txt` / `gate-round2.txt`：两轮均为 `--self-test 22/22` + 扫描 0 越界 +
`--json ok=true`，退出码全 0。单次耗时约 **1.4s**（341 个 `.rs`，纯静态、无数据库依赖），
放在 pre-commit 完全可接受。

## 复现命令

```bash
cd /opt/cloudpivot
python3 scripts/verify/inventory_write_gate.py --self-test   # 判据自检 22/22
python3 scripts/verify/inventory_write_gate.py               # 扫全树
python3 scripts/verify/inventory_write_gate.py --json        # CI 解析
git config core.hooksPath .githooks                          # 启用提交闸（每个克隆一次）
```

负例复现：把下面任一语句塞进任意**非测试** `.rs`，闸门必须 exit 1，删掉后必须 exit 0。

```rust
fn __probe() { let _p = "UPDATE public.\"biz_inventory\"\n        SET qty = qty"; }
```

## 边界 / 没做的事

- **没跑全量回归**：本卡只改静态检查脚本 + shell 钩子 + 文档，不碰 Rust 业务代码，
  按卡面「不跑无关的全量回归」要求未跑 `cargo test`；闸门本身无数据库依赖。
- **没有独立 CI workflow 文件**：本仓 CI 入口就是 `scripts/check.sh`（仓库内无
  `.github/workflows` 的业务 CI，仅 `secret-scan.yml`），所以「加进 CI」= 接进 `check.sh`。
- **生产库 144 无需执行**：无迁移、无 schema 变更。
- 判据仍是**静态文本匹配**，不解析 SQL AST：能挡住「换写法绕过」的常见形态（见 §判据口径），
  但理论上无法覆盖运行期拼 SQL（如把表名从变量拼出来）。这类只能靠 code review；
  若真出现，建议在 `inventory_writer` 上加「唯一入口」的编译期约束而不是继续加正则。
- 钩子失败的绕过口：`git commit --no-verify`（关全部闸）、`SKIP_INVENTORY_GATE=1`
  （只关这一道，打 stderr）。保留是为了紧急情况，和既有 `SKIP_SECRET_SCAN` 一致。
