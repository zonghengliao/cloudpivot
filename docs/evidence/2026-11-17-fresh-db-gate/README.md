# 空库首启行为闸（L4）· 验收证据

- 卡：`t_a0099024`（上游：`t_08e1816a` 的 P0 修复 + L0 静态闸）
- 日期：2026-11-17
- 被测提交：`608fa8c0edca0b8cee9f096c745592a0a702fc78`（在此之前工作区未提交本卡改动）
- 库：`172.18.10.145:5432`（用户 `cloudpivot`）；本机 99 经该 PG 直连（不经 127.0.0.1 隧道）

## 做了什么

把「空库首启」从**被动工具**（`scripts/verify/make_full_suite_db.sh` 原先没有任何一层闸会跑它）
变成 `scripts/check.sh` 的**行为闸**：

| 项 | 内容 |
|:--|:--|
| 新层 | `L4 空库首启行为闸`（`--fresh-db` / `--l4` 单独跑；`--full` 含；`--fast` **不含**） |
| 判据实现 | `scripts/verify/fresh_db_gate.sh`（唯一一份，`check.sh` 只做调度） |
| 复用的工具 | `scripts/verify/make_full_suite_db.sh`，新增 `SELFTEST=0`（只建库+起实例+bootstrap，不跑它的 21 项自检）与 `PG_REMOTE=1`（强制走 `-h/-U` 远端路径，给隧道场景用） |
| 临时库 | `cloudpivot_freshgate_<pid>_<YYYYmmddHHMMSS>`，`trap` 里 `dropdb --force`，**绝不碰既有库** |
| 6 条判据 | ① versioned 迁移行数 = 磁盘 `V####__*.up.sql` 条数 ② `max(version)` = 磁盘最大号 ③ `success=false` 行数 = 0 ④ `admin` ↔ `SUPER_ADMIN` 恰好 1 行 ⑤ 日志无 `数据库迁移失败`/`FATAL`（只认日志级别行） ⑥ 迁移完的库**再启动一次**：`/health` 200 且跑满 5s 仍存活 |
| 四种模式 | `auto`（默认：缺库/缺二进制/无建库权 → 跳过 exit 0）/ `block`（强制真跑，缺二进制先 `cargo build`）/ `warn` / `off` |

## 验收结果（逐条对应卡面验收）

| 卡面验收 | 证据文件 | 结果 |
|:--|:--|:--|
| `--dry-run` 能看到新层与说明 | `07-dryrun-full.out` / `08-dryrun-fast-no-l4.out` | ✓ `--full` 打出 `[L4] 空库首启行为闸…`；`--fast` 里 `[L4]` 出现 **0** 次（没把 push 前档变慢） |
| 修复后的树跑新档 **PASS**（附耗时） | `01-pass-fixed-tree.out`（rc 见同名 `.rc`） | ✓ EXIT=0，6 条判据全 ✓，**耗时 14s**（首次启动 7s + 判据⑥ 5s+） |
| 变异反证：V0442 换回旧版 → 闸 **FAIL** 且输出含 `V0442: 祖先目录「系统管理」…` | `02-mutation-old-v0442-fail.out`（rc 见同名 `.rc`） | ✓ EXIT=1，真因行为 `… ERROR nik_web::app: 数据库迁移失败: query failed: : V0442: 祖先目录「系统管理」(1000…0001) 不存在，先应用系统菜单迁移 [P0001]` |
| 换回新版 → PASS | `01-pass-fixed-tree.out`（同一棵树恢复后跑的） | ✓ EXIT=0 |
| 无库环境 → skip 且 exit 0 | `03-skip-nodb-auto.out` | ✓ EXIT=0，打印「ℹ 跳过：连不上库 172.18.10.145:1」 |
| 清场断言 `pg_database like 'cloudpivot_freshgate_%'` = 0 | `09-cleanup-and-baseline.txt` | ✓ = **0**（全部用例跑完之后查的） |

卡面之外的补充验证：

| 场景 | 证据 | 结果 |
|:--|:--|:--|
| `FRESH_DB_GATE=block` + 无库 | `04-block-nodb.out` | ✓ EXIT=1（block 不许静默跳过） |
| 缺可执行二进制（auto） | `05a-skip-nobin.out` | ✓ EXIT=0，跳过并说明「先 cargo build」 |
| 建库被拒（auto / block） | `05b-skip-nocreatedb.out` / `05c-block-nocreatedb.out` | ✓ auto EXIT=0 跳过；block EXIT=1 —— 走的是同一个 `createdb 失败` 分支（用 `OWNER=<不存在的角色>` 让 `createdb` 被拒，报错原文照打，不吞） |
| `block` 模式真跑 | `06-block-mode-pass.out` | ✓ EXIT=0，14s |

## 怎么复跑这些用例

```bash
cd /opt/cloudpivot

# 1) PASS（有库环境；想强制真跑就 FRESH_DB_GATE=block）
bash scripts/check.sh --fresh-db

# 2) 变异反证（换回旧版硬护栏 → 必须红 → 换回新版）
F=backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.up.sql
cp "$F" /tmp/v0442_new.sql
git show f06c63c3:"$F" > "$F"          # f06c63c3 = t_08e1816a 修复前的版本
bash scripts/check.sh --fresh-db; echo "EXIT=$?"   # 期望 1，输出含 V0442: 祖先目录「系统管理」…
cp /tmp/v0442_new.sql "$F"             # 一定要换回来（sha1 应为 42617eef6574f717bbb9515e0bce151c9ec85525）
bash scripts/check.sh --fresh-db; echo "EXIT=$?"   # 期望 0

# 3) 无库环境（死端口）
env -u ERP_DB_URL -u NIK_DATABASE_HOST -u NIK_DATABASE_PORT -u NIK_DATABASE_USER \
    -u NIK_DATABASE_PASSWORD -u PGPASSWORD -u PGHOST PGPORT=1 \
    bash scripts/verify/fresh_db_gate.sh auto; echo "EXIT=$?"   # 期望 0（跳过）

# 4) 清场断言
PGPASSWORD=… psql -h 172.18.10.145 -U cloudpivot -d postgres -Atc \
  "select count(*) from pg_database where datname like 'cloudpivot_freshgate_%'"   # 期望 0
```

## 两个实测教训（写进脚本注释了）

1. **判据不能裸 grep `FATAL`**：实例日志里有整份迁移 SQL 的 **DEBUG 转储**，迁移文件正文/注释里
   就写着 `FATAL`、`数据库迁移失败`、`RAISE EXCEPTION 'V0406: …'` ⇒ 裸 grep 会假红。
   现在只认**日志级别行**（`<ts> ERROR|FATAL`）与 `FATAL:`/`ERROR:` 行首，且先剥 ANSI 色码
   （tracing 写文件也上色，不剥连日志级别都匹配不到 —— 本闸第一版就是这么假绿的）。
2. **99 上的 `127.0.0.1:5432` 是到 145 的隧道**：`make_full_suite_db.sh` 原来按 host 是否 loopback
   决定走 `su - postgres`，而 99 的 postgres 系统用户还在（uid=114）、集群早 purge、socket 目录都不存在
   ⇒ `dropdb` 必失败。现在闸用「socket 是否存在」判本机集群并传 `PG_REMOTE=1`。

## 迁移内容是**运行时读盘**，不是编译期内嵌

`backend/config/config.toml` 的 `[database.migration] path = "app/migrations"`（相对 CWD=`backend/`），
所以换迁移文件**不需要重编二进制**（本次变异反证就是直接换文件跑出来的）。
但 **bootstrap 是 Rust 代码**：二进制比源码旧时闸会打印 ℹ 提示（`block` 模式则先 `cargo build`）。
