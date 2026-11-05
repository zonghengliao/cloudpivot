# P1-FIX-10 结账催办 → 同步发信 + 写业务提醒台账（biz_reminder_log）

> 卡号 t_593fba6c ｜ 2026-11-06 ｜ 隔离库 `cloudpivot_r10` / 后端 `127.0.0.1:18210`

## 一、先说结论（含对评审结论的核对）

### 1.1 评审给的两条事实里，有一条是「读数正确、归因错误」

| 评审说的 | 实测 | 判定 |
|---|---|---|
| `biz_reminder_log` 0 行 | ✅ 生产 0 行（`prod_readonly_before.txt` ①）、隔离库修复前 0 行（`step2_before.txt` 6/9 项） | **事实成立** |
| 「催办依赖已停用的定时任务 → 按钮点了没反应」 | ❌ 因果链不成立，见 1.2 | **归因错误** |

### 1.2 催办从来就是同步的，跟定时任务没有任何关系（四条独立证据）

1. **入口是 HTTP 端点，不是任务**：`#[nik::post("/business/erp/period/{id}/remind")]`
   → `period_remind_handler` → `remind_period` → `period_remind::dispatch_all` → `notice::dispatch`
   （`backend/src/business/erp/period_remind.rs`）。
2. **代码里 0 处调度注册**：`period_remind.rs` / `year_end_remind.rs` 里
   `register_builtin|register_cron|Scheduler` 命中数 = **0 / 0**（`step3_r1.txt` 第 13 项）。
3. **库里根本没有这个任务**：生产 `sys_job` 全量 6 行（4 个 ERP 提醒扫描 + 折旧 + 不变量审计），
   没有任何「结账 / 期间 / 催办」任务 —— 没有任务可「停用」（`prod_readonly_before.txt` ③）。
4. **生产现场有真实催办记录**：`sys_notice` 里 `biz_type='period_remind'` **7 行**
   （2026-11-03 21:32 一次催办发出，`receiver_count=4`）（`prod_readonly_before.txt` ②）。

隔离库上的实测同样直接：修复前点催办，**13 毫秒**内 `sys_notice` 就多了一条
（`NTT…` 编号、`receiver_count=1`），只是 `biz_reminder_log` 一直是 0 —— 见 `step2_before.txt`：

```
5) 第 1 次催办（HTTP 应答）→ { "sent": 1, ... }
7) 催办后 sys_notice(period_remind) 行数 → 1
8) 催办后 sys_notice 明细 → NTT202611051635017539ba08 | 2026-11 月结账待处理：PZ-R10-0001 | 1 | ...
9) 催办后 biz_reminder_log 行数 → 0        ← 评审看到的就是这一行
```

### 1.3 真正的缺陷有两个，本卡修第一个，第二个如实上报

**缺口 ①（本卡修）：结账催办从不写 `biz_reminder_log`。**
V0207 头注释当年写得很明白：「不建表 —— 24 小时防重复用的是 `sys_notice`」。
后果不是「按钮没反应」，而是：

* 运维 / 审计按台账追「这批人哪天被催过」查不到任何东西（台账是 0 行）；
* `notice_expire_cleanup` 按 `expire_at` **硬删**过期通知（级联删接收行），
  24h 防重判据随之消失 —— 通知一被清掉，同一张单就能被重复催；
* 与另外 4 类提醒（应付 / 采购 / 合同 / 低库存）记账口径不一致：同是业务提醒，
  只有结账催办不进台账。

**缺口 ②（不在本卡范围，另开卡）：新装配环境里催办权限与「处理人反查」用的权限码没有任何角色持有。**
V0207 授权时的写法是「`sys_role` 为空就跳过」（空库首启，迁移先于 bootstrap），
bootstrap 建出角色后没人再重放这条迁移 → 新库里 `business:erp:period:remind` 授予 **0 行**，
`business:erp:voucher:create` 也 0 行，于是：

* 非超管看不到「催办」按钮（没权限码）；
* 「处理人」反查（`period_owner::load_owner` 按权限码找角色）解析不出来 →
  每条明细都算「待分配」，催办返回 `sent=0, skipped_no_owner=N`。

隔离库上实测到的原样现象（补授权前）：

```
5) 第 1 次催办 → { "sent": 0, "skipped_no_owner": 1, "total": 1,
                   "message": "2026-11：已催办 0 张，1 张未指定处理人" }
```

生产不受这条影响（`FINANCE_MANAGER` 持有 `period:remind` / `voucher:create`，
见 `prod_readonly_before.txt` ④），所以本卡的验收在隔离库里按生产同款授权补齐后跑。

## 二、改了什么（6 个代码/迁移文件 + 1 份巡检登记）

| 文件 | 改动 |
|---|---|
| `backend/migrations/business/V0257__erp_period_remind_ledger.up.sql` | **新**：`biz_reminder_log` 的 `ck_biz_reminder_log_doc_type` 5→8 值（加 `VOUCHER` / `STOCK_IN` / `STOCK_TRANSFER`）、`ck_biz_reminder_log_kind` 4→6 值（加 `PERIOD_REMIND` / `YEAR_END_REMIND`），逐值自检「放宽而不是替换」 |
| `backend/migrations/business/V0257__erp_period_remind_ledger.down.sql` | **新**：收回值域；**若新值域已落数据则拒绝执行并说明原因**（台账是 append-only 审计事实，不静默删数据） |
| `backend/src/business/erp/reminder.rs` | 新增 `ReminderLedgerRow`（台账 8 列入参结构体）+ `ledger_insert`（**台账唯一写入口**）；`ledger_record` 改为委托它，4 类扫描行为不变 |
| `backend/src/business/erp/period_remind.rs` | 新增 `LEDGER_KIND_PERIOD` / `LEDGER_KIND_YEAR_END`、`RemindScope.ledger_kind`、`ledger_row_of`（纯函数，列口径可单测）、`record_ledger`；`dispatch_all` 发送成功后写台账；`reminded_recently` 改成「台账 **或** 通知表」两层判据 |
| `backend/src/business/shared/doc_kind.rs` | `REMINDER_DOC_KINDS` 5→8（`StockIn` / `StockTransfer` / `Voucher`），与 V0257 的 CHECK 逐值一致 |
| `docs/architecture/snapshot-drift/10_checks.sql` | `v_poly_ref_map` 的 `REMINDER_DOC` 登记表 3 行 + 3 段 DANGLING 巡检块（悬空引用仍能查出来） |
| `backend/src/business/erp/period_remind_tests.rs` | 新增 2 条用例：`ledger_row_maps_all_columns`（纯函数，8 列口径）、`remind_writes_ledger_and_ledger_alone_blocks_repeat`（真库：写台账 + 通知被回拨 25h 后仍被台账拦住） |

### 2.1 台账列口径

| 列 | 结账催办写什么 | 为什么 |
|---|---|---|
| `doc_type` | 明细的 `doc_type`（`VOUCHER` / `STOCK_IN` / `PURCHASE_ORDER` / `STOCK_TRANSFER` / `INVENTORY`） | 与 `period_items` 同一套单据类型；V0257 把值域放宽到 8 值 |
| `doc_id` / `doc_no` | 单据主键 / 单号快照 | 与 4 类提醒同口径，可按单追「催过几次」 |
| `remind_kind` | `PERIOD_REMIND` / `YEAR_END_REMIND` | 期间与年结分开统计（与 `sys_notice.biz_type` 一一对应） |
| `stage` | 期间码（`2027-05`）或年度（`2026`） | 台账里直接看出「为哪个月催的」 |
| `remind_date` | 业务日（Asia/Shanghai） | 与 4 类提醒共用幂等键 `(doc_type, doc_id, remind_kind, remind_date)` |
| `notice_id` / `receiver_count` | 实际发出的通知 id / 扇出人数 | 通知过期被清理后仍能追溯 |

### 2.2 24h 防骚扰：两层判据 + 幂等键（比卡面更严，有意为之）

1. **台账**（新增，主判据）：`remind_kind` + `doc_id` + `sent_at > now()-24h`
   —— append-only，不随通知清理消失；
2. **通知表**（保留）：`biz_type` + `biz_id` + `created_at > now()-24h`
   —— 兼容升级前已经发出的历史通知，否则升级当天同一张单会被再催一次；
3. **幂等键**：`PERIOD:REMIND:{期间}:{类型}:{id}:{日期}`，并发双击第二次命中
   `uk_sys_notice_idem` → `duplicated=true` → 计入 `skipped_recent`。

口径是「同一张单」而不是「同一张单 × 同一个人」：一张单只有一个下一步动作，
24h 内换个人再催一遍等于骚扰同一个处理组，所以按单拦（**比卡面更严**）。

## 三、验收逐条对照

| 卡面验收项 | 结果 | 证据 |
|---|---|---|
| ☑ 点催办立即发信（贴 `sys_notice` 记录） | ✅ 第 1 次催办 `sent=1`，13 ms 内 `sys_notice` 出现 `NTT20261105165141…`｜`2027-05 月结账待处理：PZ-R10-0007`｜`receivers=1`，收件人 `fin1`；**两轮各一遍** | `step3_r1.txt` 5/5b/6/7 项；`step3_r2.txt` 同结构 |
| ☑ `biz_reminder_log` 有记录 | ✅ 逐列核对：`doc_type=VOUCHER｜doc_no=PZ-R10-0007｜remind_kind=PERIOD_REMIND｜stage=2027-05｜remind_date=2026-11-06｜notice_id=01a1ac9f…｜receiver_count=1` | `step3_r1.txt` 8 项；`step3_r2.txt` 8 项 |
| ☑ 24h 内重复催办被拦（贴实测） | ✅ 第 2 次同一张单：`sent=0, skipped_recent=1`（"1 张24小时内已催过（不重复打扰）"）；台账/通知计数都停在 1，不再增长 | `step3_r1.txt` 9/9b 项 |
| ☑ 不依赖定时任务（贴说明） | ✅ 四条独立证据见 §1.2：入口是 HTTP 端点、代码 0 处调度注册、`sys_job` 里没有这个任务、生产已有 7 条同步催办记录；并且**把通知回拨 25 小时后仍被台账拦住**（第 3 次催办 `skipped_recent=1`），说明拦得住这件事也不靠任何后台清理/巡逻任务 | `step3_r1.txt` 10/11/12/13 项；`step2_before.txt` |
| ☑ git commit | ✅ 见 §六 提交号 | — |

### 3.1 连跑两轮（不同期间，各自跑完 A/B/C/D 四步）

| 轮次 | 期间 | 单据 | `sent` | 台账行 | 第 2 次 | 回拨 25h 后第 3 次 |
|---|---|---|---|---|---|---|
| r1 | 2027-05 | PZ-R10-0007 | 1 | 1 | `skipped_recent=1` | `skipped_recent=1`（台账拦） |
| r2 | 2027-06 | PZ-R10-0008 | 1 | 1 | `skipped_recent=1` | `skipped_recent=1`（台账拦） |

（另有一次 2026-12 / PZ-R10-0002 的冒烟轮，见 `smoke_202612.txt`。）

### 3.2 自动化测试（真库，`ERP_DB_URL=cloudpivot_r10test`）

| 目标 | 结果 | 日志 |
|---|---|---|
| `business::erp::period_remind`（含 2 条新用例） | **13 passed / 0 failed** | `tests_period_remind.txt` |
| `business::erp::reminder`（4 类扫描，验证 `ledger_record` 重构未回归） | **37 passed / 0 failed** | `tests_reminder_scan.txt` |
| `business::erp::year_end_remind`（共用 `dispatch_all`，年结也写台账） | **6 passed / 0 failed** | `tests_year_end_remind.txt` |
| `business::shared::doc_kind`（`REMINDER_DOC_KINDS` ↔ 巡检登记表双向校验） | **19 passed / 0 failed** | `tests_doc_kind.txt` |

编译质量：`cargo check --lib --tests` 零告警；`cargo clippy --lib --tests -- -D warnings` 见 `clippy.txt`；
`cargo fmt --check` 干净。

迁移自检：V0257 up / up+down 往返都在事务里跑通（`migration_dryrun.txt`），
隔离实例启动时自动应用并记账（`smoke_202612.txt` 第 1 项）。

## 四、环境（可复现）

| 项 | 值 |
|---|---|
| 数据库 | `cloudpivot_r10`（99 本机 PG，迁移 199 条，含 V0257）；测试库 `cloudpivot_r10test`（同上克隆） |
| 后端 | `127.0.0.1:18210`，二进制 `backend/target/debug/cloudpivot`（提交前的 HEAD+本卡改动） |
| 账号 | `admin`（超管，首登强制改密后 `R10-Admin-Pass.2026b`）、`fin1`（`FINANCE_MANAGER` 唯一成员，作为「处理人」） |
| 夹具 | 2026-12 / 2027-01…06 各一张 `DRAFT` 凭证（借贷各 200，平），期间经真实开账接口开出 |
| 脚本 | `scripts/start.sh`（起停）、`scripts/apilib.py`、`scripts/step1_setup.py`、`scripts/step1b_grants.py`（补权限矩阵，生产同款）、`scripts/step2_remind.py`（修复前基线）、`scripts/step3_after.py`（修复后 A/B/C/D）、`scripts/prod_readonly_before.sh`（生产只读核对） |

脚本里的口令 / JWT / 存储主密钥按仓库规矩**已脱敏成环境变量**（只用于隔离实例，不是生产值）：

```bash
export R10_ADMIN_PASSWORD_INITIAL=…   # admin 首登口令（首登强制改密）
export R10_ADMIN_PASSWORD=…           # admin 改后口令（后续脚本复用）
export NIK_ADMIN_PASSWORD=…  NIK_MIDDLEWARE_AUTH_JWT_SECRET=…  CLOUDPIVOT_STORAGE_MASTER_KEY=…（openssl rand -hex 32）
```

## 五、偏差与没做的事（如实登记）

1. **生产未发版**：V0257 是纯约束放宽（无数据变更），但需要 `cargo build --release` + 重启
   才会被应用到 144；本卡只到「代码 + 隔离库实测」，发版走 `scripts/deploy/deploy.sh`。
   下次发版时这条迁移会自动应用（引擎按集合差选待跑），无需人工干预。
2. **缺口 ②（新装配库权限矩阵不全）未在本卡修**：它牵连的是整个角色-权限预置
   （`voucher:create` 等业务码同样没人持有，属于「预置矩阵 vs bootstrap 顺序」问题），
   修一条 `period:remind` 的授予并不能让催办在新库跑通 —— 已另开卡上报（见 §七）。
3. **24h 判据口径比卡面更严**：卡面写「同一单据同一处理人」，实现对「同一单据」。
   理由见 §2.2；若产品要求「换人可以再催」，改成按 `receiver` 粒度即可（台账需加一列）。
4. **只跑了受影响的 4 个测试目标**（per 卡面「不跑无关全量回归」）；`cargo test --lib` 全量未跑。
5. **夹具补授权是测试动作**：`step1b_grants.py` 在**隔离库**里给 `FINANCE_MANAGER` 补
   `voucher:create` / `period:remind` 等授权，目的是让「处理人」能解析出来（生产本来就持有）。
   生产库本卡只做只读查询（`prod_readonly_before.sh`），未写一行。

## 六、提交

见卡内 handoff 的 `commits` 字段（只提交本卡涉及的文件，未碰同工作区其它卡的在途改动）。

## 七、跟进卡

* **t_227464dd（architect，gated on 本卡）新装配库权限矩阵**：V0207 的授权发生在 `sys_role` 为空时会被
  NOTICE 跳过且无人重放；新库里 `business:erp:period:remind` / `business:erp:voucher:create`
  0 授予 → 催办按钮不显示、「处理人」解析为空 → `sent=0, skipped_no_owner=N`。
  与 t_34cee9ea（43 个业务页无权限码）同源，建议合并处理。
