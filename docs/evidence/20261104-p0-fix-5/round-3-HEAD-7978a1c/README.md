# P0-FIX-5 第 3 轮复验（HEAD `7978a1c`，干净工作树 + 干净编译）

本目录是 **恢复调度后**（run 600）对已交付实现的**独立复验**，不是重做实现。
实现提交仍是 `bc37b27`（V0221 种子迁移 + `period::ensure_year_periods` + 启动保底），
证据补正在 `f002247` / `d1d0fdf`，上级证据见 `../README.md`。

## 为什么这一轮要单建干净工作树

`/opt/cloudpivot` 是多 worker 共用的工作区，复验时它的 `backend/src` 有 **14 个文件、
784 行未提交改动**（别的卡的 WIP），`backend/migrations/business/` 下还有未跟踪的
`V0233__flow_seed_is_system_fix2.*`。直接用共享树里的二进制 + 迁移目录做验证，
等于让别人的半成品参与本卡的验收（半写 SQL 会让整库迁移 `db error` 半路中止）。

所以本轮改成：

```bash
git worktree add --detach /root/p5-wt2 HEAD        # HEAD = 7978a1c
cd /root/p5-wt2/backend && cargo build --bin cloudpivot   # 2m 09s，冷 target
```

`migrations` 的路径来自 `backend/config/config.toml`（`path = "migrations"`，相对 cwd），
不是编译期内嵌，所以**必须用工作树的迁移目录**才干净 —— 复验脚本的 `ROOT` 因此被
sed 成 `/root/p5-wt2`（`scripts/verify/p0_fix5_local.sh`，不提交，纯执行副本）。
`scripts/verify/erp_invariants.py` 同样取 HEAD 版本，不吃共享树里 151 行的 WIP 改动。

## 两轮实跑（库名 / 端口不同，脚本同一份）

```bash
LOG_DIR=/root/.cloudpivot/p0fix5-v1 bash scripts/verify/p0_fix5_local.sh cp_p5_v1 18161
LOG_DIR=/root/.cloudpivot/p0fix5-v2 bash scripts/verify/p0_fix5_local.sh cp_p5_v2 18162
```

两轮输出 `diff`（抹掉库名/端口/日志目录）**逐字一致**：

```text
① 全新隔离库 —— 真引擎迁移（234 个迁移，含 V0221）
  ✅ V0221 已应用并记录在 schema_migrations（= 1）
  ✅ 当年 2026 期间行数（= 12）        ✅ 当年期间全部 OPEN（= 12）
  ✅ 12 个月编码齐备（= 0）            ✅ 区间是自然月（起始 1 号、月末 ≥28）（= 0）
  ✅ 启动保底日志：会计期间保底：当年 12 期已齐，无需补建
  结果：51/51 通过   BLOCKER 0   WARN 0            ← F2 绿
①b up / down / up 幂等（psql 直跑迁移文件）
  第 2 次 up → 新建 0 期（= 12 行）
  down       → 删 11 行，剩 V0137 的 2026-10（= 1 行，未误删）
  第 2 次 down → 删 0 行（= 1 行）
  再 up      → 新建 11 期（= 12 行）
①c 跨年通道：删 2026-06 → 11 行 → 重启 → created=1 → 12 行
② 存量库（cloudpivot_audit 副本 cp_p5_v1_f2 / cp_p5_v2_f2）
  复制前：期间 1 行 / 凭证 5 张 / 迁移 219 / 无期间凭证 5 张
  迁移后：期间 12 行 / 无期间凭证 0 张
  结果：51/51 通过   BLOCKER 0   WARN 0            ← F2 从 WARN 转绿
RESULT: PASS（0 项失败）
```

原始输出：`round-1.out.txt` / `round-2.out.txt`；冷编译日志：`clean-build.log.txt`。

### 不变量为什么是 51/51 而不是卡面的 40/40

卡面基线 `39/40 · WARN 1` 是 2026-11-04 23:3x 的套件规模。此后别卡往
`erp_invariants.py` 加了断言（P2-6a 的 H5 防回归、判定式口径调整等），HEAD 上套件
已扩到 **51 项**。51/51 是 40/40 的**超集**，F2 仍在其中且为 ✅ —— 验收口径按当前
套件全绿执行，未放宽任何一条。

## 单测（同 HEAD、同干净工作树，连跑两轮）

```bash
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cp_p5_v1' \
  cargo test --lib p1_ensure_year_periods -- --test-threads=1
# test business::erp::period::tests::p1_ensure_year_periods_seeds_twelve_and_is_idempotent ... ok
# test result: ok. 1 passed; 0 failed       （两轮均 ok）
```

日志：`unit-test.log.txt`。该用例钉住三件事：12 期齐建、重跑 0 新建（幂等）、
`ON CONFLICT DO NOTHING` 不改已有行状态/备注（先放一条 `NOT_OPENED` 手工行验证保留）。

## 与卡面验收的逐条对照

| 卡面验收项 | 本轮实测 |
|---|---|
| 种子迁移 up/down/up 幂等 | 12→12→1→1→12（两轮一致） |
| 全新隔离库迁移后期间表 12 行 | `cp_p5_v1` / `cp_p5_v2` 均 12 行，`period_year=2026` 全部 `OPEN`、自然月区间 |
| 不变量全绿（F2 不再 WARN） | 两轮均 `51/51 · BLOCKER 0 · WARN 0`，F2 ✅ |
| 5 张无期间凭证补期后 F2 转绿 | `cloudpivot_audit` 副本：无期间凭证 5 → 0，F2 ✅，未改任何凭证 |
| git commit | `bc37b27` + 证据 `f002247`/`d1d0fdf`，本轮证据随附独立提交 |

注：卡面写「全新隔离库（`cloudpivot_test`）」。`cloudpivot_test` 是本机长期模板库
（迁移 max=208、已有 68 期 / 107 张凭证），**不是**全新库，用它验证不符合语义；
本轮用两个真正从零建的库（`cp_p5_v1` / `cp_p5_v2`，`createdb -O alun` 后由真引擎跑完 234 个迁移）。
