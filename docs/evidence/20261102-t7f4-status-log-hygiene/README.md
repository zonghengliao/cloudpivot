# t_7f47221c · `biz_status_log` 的 PAYABLE / VOUCHER 行每轮仍在增长 · 验收证据

> 卡面：`t_7f47221c`（来源 `t_2287c331` §五的残留清单）
> 日期：2026-11-02 · 执行：backend-developer
> 基线代码：`fb19375`（origin/main，136 个迁移，含 V0192）
> 涉及文件：`backend/src/business/erp/{fixture_hygiene,payable_payment,payable_bridge,voucher,voucher_write,opening,batch,serial_handler,pflow3b_tests,mod}.rs`、
> `scripts/verify/t7f4_status_log_twice.sh`

---

## 一、结论先行

| 口径 | 结果 |
|---|---|
| **验收①**：同一个干净库（`cloudpivot_t7f4`，136 迁移）连跑两遍全量 `cargo test --lib -- --test-threads=1` | 第一遍 **1892 passed / 0 failed**（590.40s）· 第二遍 **1892 passed / 0 failed**（589.83s）· `scripts/verify/t7f4_status_log_twice.sh` 判 **PASS** |
| **验收②**：两遍结束后的 `biz_status_log` 计数 | 两遍都恰好是 `ACCOUNTING_PERIOD=39 / ACCOUNTING_YEAR=2 / VOUCHER=2`（**逐行相等，不再增长**）；`PAYABLE=0` 行 |
| **验收③**：孤儿行 | `PAYABLE` / `VOUCHER` 孤儿 = **0**，全库六类孤儿 = **0**（两遍都成立） |
| **验收④**：静态检查 | `cargo fmt -p cloudpivot -- --check` 绿；`cargo clippy --lib -- -D warnings` 绿；`clippy --lib --tests` 仅剩存量红点（`tests/testd/*`、`flow/internal_control.rs`），本卡 9 个文件 **0 条** |
| 修前（`cloudpivot_t2287`，父卡两遍的快照） | `PAYABLE` 18 → 56、`VOUCHER` 61 → 154（孤儿 20 / 33） |
| 修后（本卡两遍） | `PAYABLE` 18 → 0、`VOUCHER` 61 级别的量 → 2；孤儿恒为 0 |

一句话：**每跑一遍全量就线性增长的 `PAYABLE` / `VOUCHER` 状态日志已经归零**，两遍连跑的按类型计数逐行相等。

---

## 二、根因（实测定位，不是猜）

`biz_status_log` **没有外键**（审计留痕表刻意不级联），而 `PAYABLE` / `VOUCHER` 的日志是
**真实业务入口**写进去的（收货过账 / 付款 / 红冲 / 手工凭证 / 序列号变动发凭证 / 期初凭证），
编号落的是**生产号段**（`AP-YYYYMMDD-*` / `PZ-*` / `PZR-*`），所以「按号段前缀清」既不可靠也危险。

逐模块量测（干净库 `cloudpivot_t7f4`，每个模块跑完立即数孤儿，量完即清场再测下一个）：

| 模块 | 每遍新增孤儿（PAYABLE / VOUCHER） | 结构性原因 |
|---|---|---|
| `payable_payment::tests` | **8 / 10** | `setup()` / `d5_setup()` 先删凭证 / 付款 / 应付，**没有先删状态日志** |
| `voucher_write::tests` | 0 / **9** | `setup()` 删 `MANUAL` / `P7V-%` 凭证，没删它们的 `VOUCHER` 日志 |
| `payable_bridge::tests` | 0 / **6** | 同上（P17 付款凭证） |
| `opening::tests` | 0 / **11** | `cleanup()` 删 2034/2035 期初凭证，没删 `VOUCHER` 日志 |
| `batch::tests` | **10 / 10** | `cleanup()` 删凭证 / 应付时，`biz_status_log` 只按 `STOCK_IN` 清 |
| `voucher::tests` | **20 / 20** | `setup()` 清场完全没碰 `biz_status_log`（收货过账会写三类日志） |
| `pflow3b_tests` | 0 孤儿，但**每遍漏 8 张应付单 + 18 行日志** | 夹具走**真实入口** `payable::insert` ⇒ `payable_no` 落的是**生产号段** `AP-YYYYMMDD-000123`，而清场按 `payable_no LIKE 'AP-3B-%'` 删 ⇒ 一张也删不掉，每遍线性累积 |
| `serial_handler::tests` | 0 / **+58（且不是孤儿）** | 序列号变动会发凭证（ADR「两本账 + 凭证」），夹具只清序列号与两本账，**凭证本体从来没删** ⇒ `biz_voucher` 与 `VOUCHER` 日志一起线性增长 |
| `stockin_handler::tests` | 0 / 0 | 父卡 `t_2287c331` 已收口（本轮量测确认为 0） |
| `transfer_handler` / `year_end` / `report` / `cash_flow` / `income_statement` / `ledger` / `payable_query` / `voucher_query` / `purchase_handler` / `handler` | 0 / 0 | 夹具直接 INSERT 或已自带清理，不产生孤儿 |

合计与卡面现象对得上：孤儿侧 PAYABLE **8 + 10 + 20 = 38**（卡面 18 → 56 正好 +38）；
「单据没删」侧 `pflow3b` +18 行/遍、`serial_handler` +58 行/遍，加上孤儿侧
VOUCHER **10 + 9 + 6 + 11 + 10 + 20 = 56**，正是卡面 61 → 154（+93）的量级。
（逐模块量测是**单独跑一个模块**的口径，全量连跑时模块间会互相清掉一部分，所以是量级吻合而不是逐行相加。）


**一句话根因**：这些模块的清场「先删单据、后（或根本不）删状态日志」——日志没有外键，
单据一删，那些行的 `entity_id` 就永远查不到对应单据，只能变成孤儿；`serial_handler` 更彻底，
连凭证本体都不删，于是每跑一遍全量就线性多出一批凭证与日志。

---

## 三、修法

### 3.1 统一的孤儿判据：`erp::fixture_hygiene`（`#[cfg(test)]`）

新增 `backend/src/business/erp/fixture_hygiene.rs`：

| 项 | 作用 |
|---|---|
| `ENTITY_TABLES: &[(&str, &str)]` | `entity_type` → 单据表（六类：`STOCK_IN` / `PURCHASE_ORDER` / `TRANSFER` / `PAYABLE` / `VOUCHER` / `ACCOUNTING_PERIOD`） |
| `orphans(&[&str]) -> i64` | 按 `entity_type = ? AND entity_id NOT IN (SELECT id FROM <表>)` 数孤儿行 |
| `purge_orphans(&[&str]) -> i64` | 同上判据物理删（失败 panic，不静默吞错） |
| `FINANCE` / `ALL_TYPES` | 本卡口径（凭证 + 应付）/ 六类全量 |

判据是**「引用已失效」而不是号段前缀**：`AP-…` / `PZ-…` 是生产在用的号段，
真实业务入口造的数据就是这些号（技能 §6.40：按 `PM-%` 清场曾把真实付款凭证清空）。

### 3.2 根因修复：各模块清场「删单据之前按 id 删日志」

| 文件 | 改动 |
|---|---|
| `payable_payment.rs` | 抽出 `cleanup_fixtures()`（每个用例开头 + `zz_` 收尾共用），P7 / D5 两条外键链**先删 `VOUCHER` / `PAYABLE` 状态日志**再删凭证 / 付款 / 应付 |
| `voucher_write.rs` | 同上（`MANUAL` / `P7V-%` 口径抽成 `P7V_WHERE` 常量，三条语句共用） |
| `payable_bridge.rs` | 同上（P17 链补 `VOUCHER` 日志删除；`PAYABLE` 那条补上 `entity_type` 限定） |
| `opening.rs` | `cleanup()` 在删 2034/2035 期初凭证前，先删它们的 `VOUCHER` 日志 |
| `batch.rs` | `cleanup()` 补 `PAYABLE` / `VOUCHER` 状态日志删除（原语句只按 `STOCK_IN` 清），并把 `STOCK_IN` 那条补上 `entity_type` 限定 |
| `voucher.rs` | 抽出 `cleanup_fixtures()`；补 `STOCK_IN` / `PAYABLE` / `VOUCHER` 三类日志删除（收货过账一篇写三条） |
| `serial_handler.rs` | 新增 `purge_serial_vouchers()`：按 `source_type IN ('SERIAL_BATCH','SERIAL_OUT','SERIAL_SCRAP')` 删「状态日志 → 分录 → 凭证」，`setup()` 每次先跑 |
| `pflow3b_tests.rs`（原有清场） | 判据从 `payable_no LIKE 'AP-3B-%'` 改成**本模块自己写的 `source_no LIKE 'P3B-%'`**（`payable_with()` 传 `'P3B-IN'`）：真实入口落的是生产号段，前缀判据一张也删不到；补 `VOUCHER` 日志删除 |

### 3.3 模块级 teardown：7 条 `zz_`（libtest 按名排序必然最后）

| 用例 | 形态 |
|---|---|
| `payable_payment::tests::zz_teardown_purges_finance_status_logs` | 应付单 → 真实全额付款（PAYABLE 流转 + 付款凭证 VOUCHER）→ `cleanup_fixtures()` → 按 id 断言 0 → 兜底扫孤儿 |
| `voucher_write::tests::zz_teardown_purges_voucher_status_logs` | 手工凭证（真实 `post_voucher`）→ `cleanup_fixtures()` → 按 id 断言 0 |
| `payable_bridge::tests::zz_teardown_purges_payment_status_logs` | 降级直付（真实付款 + 凭证）→ `cleanup_fixtures()` → 按 id 断言 0 |
| `opening::tests::zz_teardown_purges_opening_voucher_status_logs` | 期初凭证 → `cleanup()` → 按 id 断言 0 |
| `batch::tests::zz_teardown_purges_finance_status_logs` | 草稿 → 真实批量过账（应付 + 凭证）→ `cleanup()` → 按 id 断言 0 |
| `voucher::tests::zz_teardown_purges_finance_status_logs` | 收货过账（三类日志）→ `cleanup_fixtures()` → 按 id 断言 0 |
| `serial_handler::tests::zz_teardown_purges_serial_vouchers` | 批量生成序列号（发凭证）→ `purge_serial_vouchers()` → 断言本模块凭证 0 |
| `pflow3b_tests::pflow3b_zz_teardown_purges_fixtures`（原有，本卡加断言） | 追加「六类实体整库孤儿 = 0」 |

每条 teardown 都先证「清场前确实有日志」（`assert!(n > 0, "…否则这条用例证明不了什么")`），
再断言清场后按 id 为 0，最后 `fixture_hygiene::purge_orphans(FINANCE)` + `orphans == 0` 兜底。

---

## 四、验收原始输出

### 4.1 验收①/②/③：同一个干净库连跑两遍（脚本原文）

库：`cloudpivot_t7f4`（`DROP/CREATE` + 实例启动跑迁移 → `数据库迁移完成: 应用 136 个迁移`）

```
$ ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_t7f4' LOG_DIR=/tmp/t7f4_twice3 \
    bash scripts/verify/t7f4_status_log_twice.sh
==> 第 1 遍：cargo test --lib -- --test-threads=1（日志 /tmp/t7f4_twice3/pass1.log）
    test result: ok. 1892 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 590.40s
    --- biz_status_log（entity_type|count）
    ACCOUNTING_PERIOD|39
    ACCOUNTING_YEAR|2
    VOUCHER|2
    PAYABLE/VOUCHER 孤儿 = 0，全库孤儿（六类）= 0
==> 第 2 遍：cargo test --lib -- --test-threads=1（日志 /tmp/t7f4_twice3/pass2.log）
    test result: ok. 1892 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 589.83s
    --- biz_status_log（entity_type|count）
    ACCOUNTING_PERIOD|39
    ACCOUNTING_YEAR|2
    VOUCHER|2
    PAYABLE/VOUCHER 孤儿 = 0，全库孤儿（六类）= 0
    ✓ 两遍结束时按类型计数逐行相等（不再增长）
==> PASS：同一库连跑两遍全量都全绿；PAYABLE/VOUCHER 孤儿 = 0；按类型计数不再增长
```

改动前同口径（父卡 `t2287` 两遍）：`PAYABLE` 18 → **56**、`VOUCHER` 61 → **154**（孤儿 20 / 33）。
改动后：`PAYABLE` 18 → **0**（`pflow3b` 的 18 行也被收走）、`VOUCHER` **2**，两遍逐行相等。

> 用例基线 1885 → **1892** = 本卡新增 7 条 `zz_` teardown（`pflow3b` 那条是给已有 teardown 加断言，不增数）。

### 4.2 逐模块量测（定位证据，改前代码、干净库）

每个模块单独跑一遍后立即数孤儿，量完即清场再测下一个（脚本见 §七）：

```
$ bash /tmp/t7f4_measure.sh cloudpivot_t7f4 <filter>
=== [business::erp::payable_payment] before: PAYABLE=0/orph=0|VOUCHER=0/orph=0
=== [business::erp::payable_payment] ok. 27 passed … after: PAYABLE=8/orph=8|VOUCHER=10/orph=10
=== [business::erp::voucher_write]     ok.  9 passed … after: PAYABLE=0/orph=0|VOUCHER=11/orph=9
=== [business::erp::payable_bridge]    ok. 22 passed … after: PAYABLE=18/orph=0|VOUCHER=8/orph=6
=== [business::erp::opening]           ok. 15 passed … after: PAYABLE=18/orph=0|VOUCHER=14/orph=11
=== [business::erp::batch]             ok.  8 passed … after: PAYABLE=28/orph=10|VOUCHER=16/orph=10
=== [business::erp::voucher::]         ok. 20 passed … after: PAYABLE=38/orph=20|VOUCHER=84/orph=20
=== [business::erp::serial_handler]    ok. 46 passed … VOUCHER=6 → 64（orph=0，凭证本体没删）
=== [business::erp::stockin_handler]   ok. 102 passed … after: 0 新孤儿（父卡已收口）
=== [business::erp::transfer_handler]  ok. 54 passed … 0 新孤儿
=== [business::erp::year_end]          ok. 15 passed … 0 新孤儿
```

修后同一批模块（含新 `zz_` 用例，全绿）：

```
=== [payable_payment]  ok. 28 passed … after: PAYABLE=0/orph=0|VOUCHER=0/orph=0   （原 8/10）
=== [voucher_write]    ok. 10 passed … after: VOUCHER 孤儿 0                      （原 9）
=== [batch]            ok.  9 passed … after: PAYABLE=0/orph=0|VOUCHER=0/orph=0   （原 10/10）
=== [voucher::]        ok. 21 passed … after: PAYABLE=0/orph=0|VOUCHER=0/orph=0   （原 20/20）
=== [opening]          ok. 16 passed … after: VOUCHER 孤儿 0                      （原 11）
=== [payable_bridge]   ok. 23 passed … after: VOUCHER 孤儿 0                      （原 6）
=== [serial_handler]   ok. 47 passed … after: VOUCHER=1/orph=0（175 → 1，自造的 174 条被收干）
=== [pflow3b] 连跑两遍: P=0 docs=6 orphan=0 两遍一致（原 每遍 +8 张应付单 +18 行日志）
```

### 4.3 静态检查（原样）

```
$ cargo fmt -p cloudpivot -- --check
（无输出）→ FMT-OK

$ cargo clippy --lib -- -D warnings
    Checking cloudpivot v0.1.0 (/opt/cloudpivot/backend)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 13.59s

$ cargo clippy --lib --tests -- -D warnings   # 红，但红点全在存量文件
    error: returning the result of a `let` binding from a block   → tests/testd/impexp.rs:123
    error: useless use of `vec!`                                  → tests/testd/impexp.rs ×6
    error: deref on an immutable reference                        → tests/testd/{linkage,mutex_help_ai,project_log}.rs ×6
    error: using `contains()` instead of `iter().any()`            → tests/testd/project_log.rs:393
    error: this operation has no effect / will always return zero   → src/business/flow/internal_control.rs ×6
    error: useless use of `vec!`                                  → src/business/flow/internal_control.rs:482
```

本卡 9 个文件在 `--lib --tests` 下 **0 条**（`grep -E '^\s+--> src/business/erp/(fixture_hygiene|payable_payment|payable_bridge|voucher|voucher_write|opening|batch|serial_handler|pflow3b_tests)'` 无输出）。
第一版曾有一条 `useless_format`（`payable_payment.rs:2711` 的无插值 `format!`），已改成 `.to_string()` 并复跑该模块（`ok. 28 passed`）。

---

## 五、与卡面的差异

| # | 卡面写法 | 实际落地 | 原因 |
|---|---|---|---|
| 1 | 「各模块补 `zz_` 模块级 teardown（`transfer_handler` 结算、`voucher*`、`payable_*`、`pflow3b_tests` 等）」 | 量测后确认 `transfer_handler` 零孤儿（它自己早已收口），真正产孤儿的是 `payable_payment` / `voucher_write` / `payable_bridge` / `opening` / `batch` / `voucher` / `serial_handler` 七个模块 | 按**实测来源**补，不按猜测补（`transfer_handler` 补了也只是空跑） |
| 2 | 只说「凭证 / 应付 / 调拨链路的夹具单据删了但状态日志没删」 | 多一类：`serial_handler` 是**凭证本体根本没删**（每遍 +58 张），不是孤儿 | 卡面「每轮仍在增长」的验收口径同样要求它归零，否则按类型计数第二遍必然不等 |
| 3 | 「若某张单据表有 `PENDING_* ⇒ approval_instance_id 非空` 的库级约束……解开弱引用时要排除待审状态」 | 本卡的修法**不碰弱引用**：只删 `biz_status_log`，因此 `ck_biz_si_pending_cancel` 类约束完全不涉及 | 状态日志没有外键，删它不会触碰单据行的 check 约束；那条教训属于父卡（解实例弱引用）的场景 |
| 4 | 卡面只点名 `transfer_handler` 结算 / `voucher*` / `payable_*` / `pflow3b_tests` | 实际还修了 `opening` / `batch` / `voucher`（收货过账）/ `serial_handler` | 逐模块量测出的真实来源，含两类「不删单据」的形态（`serial_handler` 凭证本体没删、`pflow3b` 按生产号段清不掉） |
| 5 | 「同一个干净库连跑两遍」 | 一共跑了 **三次** 两遍口径：`/tmp/t7f4_twice`（发现脚本计数 SQL 笔误）、`/tmp/t7f4_twice2`（发现 `pflow3b` 生产号段判据问题，PAYABLE 18 → 36 被脚本抓住）、`/tmp/t7f4_twice3`（**最终 PASS**） | 前两次都是脚本**判 FAIL**、按失败原因修完再跑；最终证据是第三次。验收脚本的 `ERROR` 兜底断言也在这轮加上，防止「psql 报错被当成相等」 |

---

## 六、部署口径

本卡全部改动都在 `#[cfg(test)]` 里（含新增的 `fixture_hygiene` 模块、各 `cleanup_fixtures()` /
`purge_serial_vouchers()` / `zz_` 用例），**不进 release 二进制路径**：

| 改动 | 是否需要发版 |
|---|---|
| 夹具清场 / teardown / 验收脚本 | 否（只跑在测试进程里） |
| 运行时代码 | **零改动**（`erp/mod.rs` 只多了一行 `#[cfg(test)] pub(crate) mod fixture_hygiene;`） |

因此本卡**未部署 144 生产**，也不需要迁移、不需要开关。下次发版不带本卡的任何运行时差异。

---

## 七、复现方式

```bash
# 1) 干净库（owner 必须是应用账号 alun；migrations 由实例启动时跑）
runuser -u postgres -- psql -qc "DROP DATABASE IF EXISTS cloudpivot_t7f4"
runuser -u postgres -- psql -qc "CREATE DATABASE cloudpivot_t7f4 OWNER alun"
cd /opt/cloudpivot/backend
NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_t7f4 NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun NIK_SERVER_LISTEN=127.0.0.1:18188 NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > /tmp/t7f4_server.log 2>&1 < /dev/null &
# 等日志出现「数据库迁移完成: 应用 136 个迁移」+ /health 200 + SUPER_ADMIN 绑定 admin 后再停实例

# 2) 两遍验收（脚本自带四条断言）
cd /opt/cloudpivot
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_t7f4' \
  bash scripts/verify/t7f4_status_log_twice.sh

# 3) 逐模块量测（定位用，卡面残留数字就是它给的）
psql … -c "select entity_type, count(*) from biz_status_log group by 1 order by 1"
psql … -c "select count(*) from biz_status_log s where s.entity_type='VOUCHER'
             and s.entity_id not in (select id from biz_voucher)"
```
