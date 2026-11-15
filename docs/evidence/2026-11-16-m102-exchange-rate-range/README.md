# M1-02 证据：汇率表生效区间 + 区间唯一（V0407）

> 卡：`t_438a4c0d` —— 迁移 `biz_exchange_rate` 加生效区间 + 唯一键（按币种 + 日期唯一，支持历史汇率）。
> 依据：`docs/planning/20261113-新卡清单.md` §4 M1 财务 的 M1-02 行。
> 上游交接：`t_76e55ffd`（M1-01，V0406 `biz_account.currency_code` + `biz_voucher.currency_code/exchange_rate`）。

## 动手前先实测了现状（这决定了本卡到底补什么）

`biz_exchange_rate` 由 V0272（SO-39）建出，11 列，**已经**有：

| 已有 | 说明 |
|------|------|
| `effective_date DATE NOT NULL` | 生效日（含当日），且**不给 DEFAULT**（补录历史汇率不会被静默按今天生效） |
| `uk_biz_exchange_rate (from_currency, to_currency, rate_type, effective_date)` | 同币对 / 同类型 / 同一生效日只能一行；也是 `rate_upsert` 的 `ON CONFLICT` 目标 |

所以卡面「加唯一键」的字面一半**已经在了**；真正缺的是**区间**：只有起点没有终点，
「这条汇率到哪天失效」表达不出来，两条起点不同的汇率可以同时覆盖同一张单据日期，
取数全靠 `ORDER BY effective_date DESC LIMIT 1` 兜 —— 只要顺序/索引变了就换了一个真相，
且没有任何一层能证明「这一天只该用这条」。

## 改了什么

| 文件 | 改动 |
|------|------|
| `backend/app/migrations/business/V0407__exchange_rate_effective_range.up.sql`（新增，355 行） | ① `effective_to DATE`（可空 = 长期有效）；② `ck_biz_exchange_rate_dates`（止不早于起）；③ `ex_biz_exchange_rate_period`：`EXCLUDE USING gist (from_currency, to_currency, rate_type, daterange(effective_date, effective_to, '[]') WITH &&) DEFERRABLE INITIALLY DEFERRED`；④ 存量回填（`lead(effective_date) OVER (PARTITION BY 币对+类型 ORDER BY effective_date) - 1`，末行留 NULL）；⑤ 自检块：4 条结构断言 + 2 条回填断言 + 7 组行为探针（P1/P2/P4 重叠被拒、P3 相邻放行、P5 反向区间被拒、P6 同起点被 uk 拒、P7 区间命中、P8 派生口径对拍），探针自清场 |
| `backend/app/migrations/business/V0407__exchange_rate_effective_range.down.sql`（新增） | 先拆排除约束、再拆 CHECK、最后拆列（顺序不能反：`daterange(effective_date, effective_to)` 是表达式索引，列被它依赖）；全 `IF EXISTS`；**不删 `btree_gist`**（库级对象，V0336 的委托约束也在用）；纯派生列 ⇒ 无数据护栏 |
| `backend/app/src/business/erp/currency_admin_model.rs` | `RateRow.effective_to`；新增 `rederive_group`（派生区间重算，`IS DISTINCT FROM` 幂等）与 `rate_lock`（`SELECT … FOR UPDATE` 取改动前的键）；`rate_insert` / `rate_upsert` / `rate_update` / `rate_delete` 各自包进**一个事务**并在同事务内重算区间（update 时旧组 + 新组都算）；`spot_row` 加区间谓词 |
| `backend/app/src/business/erp/currency.rs` | `spot_rate`（建单汇率锁定的唯一取数实现）加区间谓词 `AND (effective_to IS NULL OR effective_to >= 目标日)` |
| `backend/app/src/business/erp/currency_admin_handler.rs` | `RateRes` 增加 `effective_to: Option<String>`（接口可见，前端无需改动即得到该字段） |
| `backend/app/tests/m1_02_exchange_rate_range_migration.rs`（新增，10 条静态不变量） | 只加一列 / 列型可空 DATE / 反向 CHECK / 排除约束三列 + `'[]'` + DEFERRABLE / 回填用 `lead` 且只动 NULL 行 / **不许拆 V0272 的 uk** / 自检断言齐 + 探针自清场（先删汇率后删币种）/ down 顺序与幂等 / 引擎拆分器逐块验 / 派生口径与写入口逐字对拍；纯文件解析不连库 |
| `scripts/verify/m102_exchange_rate_range_drill.sh`（新增） | 真库往返演练（克隆模板库 → 预置 5 行 → up×2 → 结构/回填逐行对拍 → 4 组行为反证 → down×2 → 重放 → 自删演练库） |

**没有**改前端、没有改已应用的迁移、没有动 `uk_biz_exchange_rate`。

## 七个刻意的决定

| 决定 | 为什么不选另一种 |
|------|------------------|
| **不动 `uk_biz_exchange_rate`**，另加一条区间排除约束 | 四列 uk 是 upsert 的 `ON CONFLICT` 目标（V0272 头注释 ②.5），改列序等于重写写入口 + 汇率维护页的 409 判重文案；而「起点不重复」本身没错。两张网各管一件事：uk 管**起点唯一**，ex 管**区间不重叠**（任一日期一个真相）——这才配得上卡面「按币种 + 日期唯一」 |
| **`effective_to` 是派生列**（恒等于同组下一条生效日 - 1），由写入口重算 | 汇率是时间序列，不存在「这条作废但仍保留」的业务语义；要停用就补一条新的，或删掉这条（删完前后两段自动接上）。若让人工填，就会与「取最近一条」的口径打架，且多一条人工维护路径 |
| 约束 **`DEFERRABLE INITIALLY DEFERRED`** | 写入口是「先写行、后重算整组区间」，新行落地那一瞬间必然与旧邻居重叠；延迟到 commit 校验正好放过中间态。代价：别的迁移/脚本想立刻看到冲突要 `SET CONSTRAINTS ALL IMMEDIATE`（自检探针就是这么做的） |
| 区间 **`'[]'` 左闭右闭**、端点相接不算重叠 | 汇率录入的常态是「月末最后一天失效、次日生效」（全年无休）。`'[)'` 会让 12-31 与 01-01 判成重叠 |
| `from_currency / to_currency / rate_type` **三列**都进排除键 | 少 `rate_type` 会让 SPOT 与 MONTH_END 互相排斥（月末重估的汇率根本录不进去）；多 `id` 等于不约束 |
| `btree_gist` 建不出来时**降级**并 RAISE WARNING | 与 V0336 同口径：不能让一个可选扩展把服务卡在启动阶段。降级后只剩 ① ② + 应用层重算（存在毫秒级并发窗口），迁移会明确告警 |
| 取汇率**加区间谓词**（建单锁定 + 界面预览两处同口径） | 连续分区下它与「取 <= 目标日的最新一条」结果逐行相同（零回归）；但万一区间被改出空洞，会返回 `MISSING` 让建单 4xx 拒绝，而**不是**静默沿用一条已经失效的牌价（外币单按过期汇率入账 = 账直接错） |

**为什么不给 down 加数据护栏**（对比 V0406 加了）：`effective_to` 完全是 `effective_date` 的函数，
重放 up 能逐行重建，丢它不丢真相；V0406 那三列（币种 / 汇率）是人工录的业务信息，回滚就真丢了。

## 验证结果

### 1) 真库往返（145 的 PG，模板库 `cloudpivot_test` 克隆；全量输出见 `01_drill.out`）

```
bash scripts/verify/m102_exchange_rate_range_drill.sh
== 汇总：PASS=29 FAIL=0 ==
```

覆盖：up 首次成功（自检块通过）→ `effective_to` 是可空 DATE → 反向 CHECK + 排除约束（含
`daterange` / `&&` / `DEFERRABLE`）+ `btree_gist` 在位 → **V0272 的 uk 仍在** →
预置 5 行（USD/SPOT 3 行、USD/MONTH_END 1 行、EUR/SPOT 1 行）**逐行对拍**回填结果
（11-01→11-14 / 11-15→11-30 / 12-01→NULL / 单行组→NULL）→ 全表零重叠对 →
up 重复执行后整表指纹不变 → 4 组行为反证（重叠插入被 ex 拒 / 反向区间被 ck 拒 /
同起点被 uk 拒 / 相邻 12-31→01-01 放行）→ down×2 幂等且 5 行业务数据一行不少 →
回滚后重放 up，整表指纹与第一次**完全一致**。

### 2) 克隆库升级（**真引擎**，不是 psql 手动执行；全量输出见 `02_engine_upgrade.out`）

模板库 `cloudpivot_test`（versioned 326 条，最大 V0404）克隆成 `cp_m102_upgrade`，
用 `backend/target/debug/cloudpivot` 真启动一次（profile=test，PG 指 145）：

| 检查 | 结果 |
|------|------|
| 迁移日志 | `应用 3 个迁移`（V0405 / V0406 / **V0407**） |
| `schema_migrations` | `versioned / exchange_rate_effective_range / 407 / da4a72d9f2662c97… / executed / t`，`source = app/migrations/business/V0407__exchange_rate_effective_range.up.sql` |
| 升级后结构 | `effective_to date, nullable=YES`；`ck_biz_exchange_rate_dates`；`ex_biz_exchange_rate_period … daterange(…,'[]') WITH && DEFERRABLE INITIALLY DEFERRED` |
| 存量回填 | 3 行首尾相接：`11-01→11-14`、`11-15→11-30`、`12-01→NULL` |
| `/health` / `/health/deps` | 200 / `db.ok=true cache.ok=true` |
| 重启一次 | `应用 0 个迁移`（引擎级幂等） |

### 3) 空库首启 + 写入口区间维护（真引擎 + 真 HTTP；全量输出见 `03_write_path_api.out`）

空库 `cp_m102_e2e` → 真启动：`应用 339 个迁移`（versioned **327** 条，V0407 登记
`407/true/da4a72d9f2662c97`），迁移自检本身打印
`M1-02 自检通过：effective_to 区间 + 反向护栏 + 区间排除约束 + 存量回填连续 + 7 组行为探针`。

随后用 `admin` 真登录、真调接口（每一步都回读库里的区间）：

| 步骤 | 调用 | 库里的区间（USD→CNY SPOT） |
|------|------|---------------------------|
| A | `POST /api/business/erp/exchange-rate` 2026-11-01 | `11-01 → NULL` |
| B | `POST` 2026-12-01 | `11-01 → 11-30`、`12-01 → NULL`（**前一条被自动收口**） |
| C | `POST …/batch-month` 2027-01 | `11-01→11-30`、`12-01→2027-01-30`、`2027-01-31→NULL` |
| D | `DELETE` 末行 2027-01-31 | `11-01→11-30`、`12-01→NULL`（**放开回长期有效**） |
| E | `PUT` 把 12-01 改成 12-20 | `11-01→12-19`、`12-20→NULL`（**两侧重算**） |
| F | `POST` 挂在中间 12-10 | `11-01→12-09`、`12-10→12-19`、`12-20→NULL` |
| G | `GET …/quote?date=` | `11-20→7.10`、`12-15→7.15`、`12-25→7.25`（区间命中，不是「取最新」碰运气） |
| H | 手工把末行关到 12-31 后问 `2027-03-01` | 返回 `source=MISSING, effective_date=null`（**不沿用 7.25**；建单入口按 `LockOutcome::Missing` 拒绝） |
| I | 直接 `INSERT` 重叠区间 | `ERROR: conflicting key value violates exclusion constraint "ex_biz_exchange_rate_period"` |
| J | 重启 | `应用 0 个迁移`、`/health 200` |

> A–F 是这套改动**最关键**的回归面：V0272 的写入口原本「插了就完事」，加了区间唯一之后
> 若不重算，插第二条就会被排除约束在 commit 时拒掉（汇率维护页整页 500）。实测六条路径
> （新建 / 批量 upsert / 删除 / 改生效日 / 中间插入）全部自动维持连续。

### 4) 静态门禁

```
cargo fmt -- --check                                  → 通过
cargo clippy --lib -- -D warnings                     → exit 0（零警告）
cargo test --test m1_02_exchange_rate_range_migration  → 10 passed
cargo test --test m1_01_currency_columns_migration     → 10 passed（回归，未受影响）
cargo test --test flow_all_role_multiversion_migration → 7 passed（版本号唯一/4 位补零/up 必有 down）
```

**前端**：本卡零前端改动（`RateRes` 只多返回一个字段，前端页面无需改）。工作区里当时有
其他 card 未提交的前端改动，跑 `pnpm typecheck` 反映的是别人的半成品，故未跑——
与 M1-01 同口径。

## 下游卡要照着的几件事

- **M1-04 凭证录入**：取汇率只许走 `currency::lock` / `spot_rate`（已带区间谓词）；不要自己写
  `ORDER BY effective_date DESC LIMIT 1`。
- **M1-05 期末调汇 / M1-09 报表折算**：要「某月月末的 MONTH_END 汇率」时按 `effective_date <= 月末日
  AND (effective_to IS NULL OR effective_to >= 月末日)` 取——区间语义从本卡起是可靠的。
- **谁都不许人工写 `effective_to`**：它是派生列；写入口 `rederive_group` 是唯一维护点，
  手改要么被排除约束拒（重叠），要么改出空洞让建单 4xx。
- 汇总卡（M1-24）：多币种链路的 E2E 里把「汇率区间连续」当一条断言（库里任意两行同币对同类型
  区间不重叠）。

## 演练库清理

`cp_m102_upgrade` / `cp_m102_e2e` / 探针库 `cp_m102_probe*` / `cp_m102_sql` 全部已 DROP，
`select count(*) from pg_database where datname like 'cp_m102%'` = **0**。
