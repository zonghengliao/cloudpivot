# P0-FIX-5 证据：新库不自动开账（期间表缺期 → 首笔业务即 F2 WARN）

卡：`t_3b0f5ac3` · 复验脚本：`scripts/verify/p0_fix5_period_seed_check.sh`
机器：`172.18.10.99`（Hermes）· 库：全部为**隔离库**，未连生产 `cloudpivot`

---

## 1. 病：实测基线（先证明评审看到的现象）

评审报告原文（`docs/评审报告-现有功能评分-20261104.md:406`）说「新库迁移完
`biz_accounting_period` 是 **0 行**」。**实测是 1 行，不是 0** —— V0137 §5.4 种了
`2026-10` 一期。差异不影响结论：缺的是「当月」以外的 11 期。

```text
$ psql -d cloudpivot_audit          # 评审用的隔离库（迁移 max=219）
period_code | status          →  只有 2026-10（1 行）
select period, count(*) from biz_voucher where 期间无记录  →  2026-11 | 5 张
```

基线不变量（复制 `cloudpivot_audit` → `cp_p5_before`，**不**应用本卡迁移）：

```text
$ NIK_DATABASE_NAME=cp_p5_before python3 scripts/verify/erp_invariants.py
结果：39/40 通过   🚨 BLOCKER 0   ⚠️ WARN 1
  · [期间] F2 凭证期间必有期间记录  —  5 张凭证的期间无记录
```
完整输出：`before-invariants.txt`（与评审的 `39/40 · WARN 1 · F2` 逐字一致）

**为什么首笔业务就中招**：`voucher::period_of` 按日期区间查期间，查不到就
fail-open 退化成 `yyyy-MM` 字符串 → 凭证照写、`period` 指向一个不存在的期间。

---

## 2. 改了什么

| 文件 | 改动 |
|------|------|
| `backend/migrations/business/V0221__erp_seed_accounting_period_year.up.sql` | **新**：幂等种当年 12 期（`status = OPEN`，自然月区间），末尾 `DO $$` 自检（12 行齐备 / 编码与区间） |
| `backend/migrations/business/V0221__erp_seed_accounting_period_year.down.sql` | **新**：只删本迁移种的行（id 由 `md5('biz_accounting_period:<year>-<mm>')` 确定性生成），已结账 / 有凭证的行保留 |
| `backend/src/business/erp/period.rs` | ＋`ensure_year_periods(ex, year)`（幂等插入 12 期，`ON CONFLICT (period_year, period_month) DO NOTHING`）、＋`ensure_current_year_periods()`（按 CST 当天取年）、＋单测 `p1_ensure_year_periods_seeds_twelve_and_is_idempotent` |
| `backend/src/bootstrap.rs` | ＋启动保底 `ensure_accounting_periods()` 挂进 `tokio::join!`（跨年通道：迁移只跑一次，2027-01-01 起库里又会缺一年） |
| `scripts/verify/p0_fix5_period_seed_check.sh` | **新**：本证据的可复跑脚本（新库 + 存量库两段） |

### 期间状态为什么是 `OPEN`（卡面问题 2 的答案）

卡面问「未开始的期间应该是 `OPEN` 还是 `FUTURE`（按现有语义）」。按现有语义：

1. 库层 `ck_biz_period_status` 只有 **`NOT_OPENED` / `OPEN` / `CLOSED`** 三值
   （V0137:196），**本仓没有 FUTURE**，写 `FUTURE` 直接违反 CHECK；
2. 「未开始」的既有表达是 `NOT_OPENED`，但把它当种子的状态会踩一个坑：
   `batch_open`（批量开账）对**已存在的行一律 skipped**（period.rs
   `Some(row) => skipped`），不会把 `NOT_OPENED` 打开；能把 `NOT_OPENED → OPEN`
   的只有单期开账。种子若写 `NOT_OPENED`，用户在「批量开账」里看到 12 条 skipped、
   一期都没开成 —— 比「没有行」更难收拾；
3. `OPEN` 是唯一「建出来就能用」的状态：写凭证只拦 `CLOSED`（`voucher::post_voucher`），
   录期初要求 `OPEN`（`opening::lock_period`）；
4. 与 V0137 既有种子同形（那一期本来就是 `OPEN`）。

所以：**12 期一律 `OPEN`**；`NOT_OPENED` 保留给「运维手工先建行、之后再点开账」的路径，
本迁移不产出它。

---

## 3. 实测（连跑两轮，脚本同一份，库名/端口不同）

两轮输出完全一致，下面贴 round-2，round-1 见 `round-1/stdout.txt`。

### 3.1 全新隔离库 → 迁移完 12 期

```text
==== ① 全新隔离库 cp_p5_e2 —— 真引擎迁移 ====
  ℹ️  schema_migrations 最大版本 = 227（共享工作区里别人的迁移也在跑，本卡只锚 V0221）
  ✅ V0221 已应用并记录在 schema_migrations（= 1）
  ✅ 当年 2026 期间行数（= 12）
  ✅ 当年期间全部 OPEN（= 12）
  ✅ 12 个月编码齐备（= 0）
  ✅ 区间是自然月（起始 1 号、月末 ≥28）（= 0）
  ✅ 启动保底日志出现：会计期间保底：当年 12 期已齐，无需补建
结果：40/40 通过   🚨 BLOCKER 0   ⚠️ WARN 0
  ✅ 不变量 F2 绿（全新库）
```

真引擎侧的同一条证据（`round-2/fresh-migrate.summary.log`，从 app 日志里抽的真实 NOTICE）：

```text
NOTICE: V0221：当年 2026 年会计期间 —— 新建 11 期，已存在 1 期（共 12 期）
NOTICE: V0221：按 CURRENT_DATE = 2026-11-04 判定，当月 = 11
NOTICE: V0221 自检通过：当年 2026 的 12 期齐备
INFO  nik_web::app: 数据库迁移完成: 应用 168 个迁移
INFO  cloudpivot::bootstrap: 会计期间保底：当年 12 期已齐，无需补建
```
（「新建 11 期 / 已存在 1 期」正是 V0137 已种 `2026-10` 的那 1 行：种子不覆盖它。）

### 3.2 种子迁移 up / down / up 幂等

```text
==== ①b 迁移脚本 up / down / up 幂等 ====
  NOTICE: V0221：当年 2026 年会计期间 —— 新建 0 期，已存在 12 期（共 12 期）   ← 第 2 次 up
  ✅ 第 2 次 up 后行数（幂等）（= 12）
  NOTICE: V0221 down：删除种子期间 11 行，保留（已结账 / 有凭证）0 行
  ✅ down 后只剩 V0137 的 2026-10 种子行（= 1）
  ✅ V0137 字面 id 的种子行没被误删（= 1）
  NOTICE: V0221 down：删除种子期间 0 行，保留（已结账 / 有凭证）0 行             ← 第 2 次 down
  ✅ 第 2 次 down 后行数（幂等）（= 1）
  ✅ 再 up 后回到 12 行（= 12）
```

### 3.3 跨年通道（删一期 → 重启 → 启动保底补回）

```text
  ✅ 人为删掉 2026-06 后行数（= 11）
  ✅ 重启后行数（启动保底补回）（= 12）
  ✅ 重启日志确认补建：会计期间保底：已补齐当年缺失的会计期间（status = OPEN） created=1
```

### 3.4 存量库（评审那份有 5 张无期间凭证的库）转绿

```text
  复制前：期间 1 行 / 凭证 5 张 / 迁移 219
  复制前无期间凭证：5 张
  ✅ 存量库已应用 V0221（= 1）
  ✅ 存量库当年期间行数（= 12）
  ✅ 无期间凭证数（F2 判据）（= 0）
结果：40/40 通过   🚨 BLOCKER 0   ⚠️ WARN 0
  ✅ 不变量 F2 绿（存量库 5 张凭证已挂上期间）
```

> 存量库里 5 张凭证落在 `2026-11`；V0221 种出的 `2026-11`（`OPEN`）正好接住它们，
> **不需要**改凭证日期、也不需要补账。

### 3.5 单测（period 模块，真库 `ERP_DB_URL`）

```text
$ ERP_DB_URL=postgres://alun:…@127.0.0.1:5432/cp_p5_verify \
  cargo test --lib business::erp::period -- --test-threads=1
test business::erp::period::tests::p1_ensure_year_periods_seeds_twelve_and_is_idempotent ... ok
test result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; 2292 filtered out; finished in 42.92s
```
日志：`period-test-round-1.txt`（共享工作树）/ `period-test-round-2.txt`（干净 worktree，见下）

> **第 2 轮为什么在 worktree 里跑**：共享工作树里另一张卡正在写 `budget_*.rs`（未跟踪文件，
> 当时编译不过：`budget_handler.rs` 缺 `use crate::utils`），`cargo test` 会整片失败 ——
> 与本卡无关。第 2 轮改成 `git worktree add /root/p5-wt HEAD`（只含本卡改动）+
> `CARGO_TARGET_DIR=/root/p5-target`：`40 passed; 0 failed; finished in 43.01s`，两轮同结果。

---

## 4. 没做 / 做不到的（如实记录）

1. **down 是「psql 直跑迁移文件」，不是 `Migrator::rollback()`**：框架的 `rollback()` 回滚的是
   **已应用的最大版本**，而共享工作区里最大版本随时是别人的迁移（本轮实测已到 V0227），
   拿它测 V0221 的 down 只会回滚到别人的迁移上。所以 3.2 的 up/down/up 是把**这两个 .sql 文件
   原样交给 psql**（引擎执行迁移脚本也是同一份 SQL），验的是脚本本身。
   另：down 不是「无条件删除」—— 已结账（`CLOSED`）或已有凭证的种子期间行，down **保留**，
   否则回滚会把 F2 的坑重新挖开。所以「up → down → up」在**有业务数据的库**上是「删能删的、
   再补齐」，在**全新库**上才是干净的 12 → 1 → 12（3.2 验的就是全新库）。
2. **迁移文件里的年份是迁移执行时的时间**（`CURRENT_DATE`）。跨年之后由启动保底补，
   不靠迁移；两者口径由同一段 SQL（`ensure_year_periods`）+ 同一份文档约定锁住。
3. **本轮验证期间踩到共享工作区的一次干扰**：另一张卡（预算）正在写 `V0227`，
   我的第 2 轮首次 boots 时读到半写文件 → `数据库迁移失败`。等对方写完后重跑即全绿
   （`schema_migrations` 最大版本 227，本卡只断言「V0221 已应用」，不断言 max）。
   这是共享 `dir:/opt/cloudpivot` 的固有现象，与本卡改动无关，已写进脚本注释。
4. 未跑全量回归（卡面要求「只跑受影响的模块」）：只跑了 `business::erp::period`
   集群（period + period_items + period_remind，40 条）。

---

## 5. 复跑方法

```bash
# 前置：backend/target/debug/cloudpivot 为当前 HEAD 编译（cargo build --bin cloudpivot）
bash scripts/verify/p0_fix5_period_seed_check.sh cp_p5_verify 18121
# 第 2 轮换库名/端口即可（脚本自己建库、起实例、关实例）
bash scripts/verify/p0_fix5_period_seed_check.sh cp_p5_verify2 18122
```

脚本做的事：建库 → 起实例跑真迁移 → 断言 12 期/F2 → up·down·up → 删一期重启验保底
→ 复制 `cloudpivot_audit`（`SRC_DB` 可覆盖）再跑一遍存量场景 → 汇总 PASS/FAIL。

## 6. 证据文件清单

| 文件 | 内容 |
|------|------|
| `before-invariants.txt` | 迁移前的基线不变量（39/40 · WARN 1 · F2） |
| `round-1/stdout.txt`、`round-2/stdout.txt` | 两轮脚本的完整断言输出（逐条 ✅） |
| `round-{1,2}/fresh-invariants-round.txt`、`f2-invariants-round.txt` | 两轮的全新库 / 存量库不变量原文（40/40） |
| `round-{1,2}/*.summary.txt` | 从 app 日志抽出的关键行（V0221 NOTICE / 迁移完成 / 启动保底）；**完整 DEBUG 原始日志不入库**（每份 1.7MB），留在 `/root/.cloudpivot/p0fix5/raw/` |
| `period-test-round-{1,2}.txt` | period 模块真库单测（40 条）两轮结果（后缀 `.txt`：仓里 `.gitignore` 忽略 `*.log`） |

## 7. 上线影响（144 生产，只读查过，未写）

```text
生产 biz_accounting_period where period_year = 2026  →  2 行：2026-10、2026-11
生产 schema_migrations max(version) = 219
```

部署本卡后生产会**新增 10 行**（`2026-01`…`2026-09`、`2026-12`，`status = OPEN`），
`2026-10 / 2026-11` 原样保留；已有凭证、结账状态、期初余额一律不动（`DO NOTHING`）。

两处可感知变化，都是本卡想要的方向：

1. 「会计期间」页当年从 2 行变 12 行，1–9 月显示「已开启」（原来根本不在列表里，
   属于「查不到 = 不能结账」的黑洞）；
2. 结账页的「未结账期间」会多出 1–9 月 —— 这是把历史月份补回台账，不是新欠账。

**没有**放宽任何闸门：写凭证本来就只拦 `CLOSED`（缺期时 fail-open 照样写），
本卡只是让「期间行存在」这件事实恒成立，F2 从 WARN 变绿。
