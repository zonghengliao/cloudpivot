# HR-23 考勤「锁定 / 解锁 + 锁定后写保护」落地证据

卡面：`t_8aa9f8ed` — 考勤锁定/解锁 + 状态机接线 + 锁定后写保护（批 3 考勤请假，依赖 HR-19）。
设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md`
§4.2.3（状态机 `DRAFT→SUBMITTED→LOCKED→DRAFT`、`LOCKED` 之后记录不许增删改、解锁要原因 + 权限 + `biz_status_log`）、
§5.2 #20（`POST /business/hr/attendance/period/{id}/status`，权限 `business:hr:attendance:lock`）。

## 交付了什么

| 层 | 文件 | 内容 |
|---|---|---|
| 库层守卫 | `backend/migrations/business/V0392__attendance_lock_guard.{up,down}.sql` | `biz_attendance_record` 上的 `BEFORE INSERT OR UPDATE` 触发器：期间 `LOCKED` → 抛 `CP001`（含 HINT）；逃生门 `SET LOCAL cloudpivot.attendance_lock_guard='off'`；幂等（`CREATE OR REPLACE` + `DROP TRIGGER IF EXISTS`）；迁移内自检（触发器唯一 + 函数体含 `LOCKED`/`CP001`） |
| 唯一守卫面 | `backend/src/business/hr/attendance_lock.rs` | `WRITABLE_STATUSES` / `is_writable` / `assert_writable` / `locked_error`（`409 + 68010` 三段式报文）/ `lock_for_write`（先 `FOR UPDATE` 锁期间再判）/ `is_lock_violation` + `error_chain_text` + `map_write_error`（`CP001 → 409 + 68010`）+ 12 条单测 |
| 真库用例 | `backend/src/business/hr/attendance_lock_db_tests.rs` | 4 条：① 锁定后 INSERT/UPDATE/软删全被拒且**逐列未变**；② 解锁缺原因/过短 400、给原因 200、日志带原因与操作人；③ `lock_for_write` 草稿放行 / 锁定 409 / 不存在 404 / 回滚不残留锁；④ 逃生门事务级（提交后自动恢复） |
| 接线 | `backend/src/business/hr/mod.rs` | 挂上 `pub mod attendance_lock;` + `#[cfg(test)] mod attendance_lock_db_tests;` |
| 接线 | `backend/src/business/hr/attendance_record_handler.rs` | 单行修正的「锁期间 + 判 LOCKED」改为调 `attendance_lock::lock_for_write`（判据收成一份，报文统一）；删掉本文件里重复的 `rule_blocked` |
| 接线 | `backend/src/business/hr/attendance_record_model.rs` | `write_error` 优先认库层守卫指纹 → `409 + 68010`（导入 upsert / 并发锁定这类不经过 handler 闸的写路径也有正确出口）；+1 条单测 |
| 冒烟 | `scripts/verify/hr/attendance_lock_http_smoke.sh` | 真实例 + 真库的 REST 断言：401 / 锁定后 409+68010 / 无权限账号 403 / 缺原因 400 / 给原因 200 / `biz_status_log` 直查 |

## 三层同一条规则（为什么不是三套）

| 层 | 落点 | 拦得住 | 拦不住 |
|---|---|---|---|
| 接口层 | `#[nik::permission(... business:hr:attendance:lock)]`（HR-19 落的状态路由） | 没有锁定权限的人调接口 | 有权限的人 + 别的写路径 |
| handler 层 | `attendance_lock::lock_for_write`（HR-20 单行修正已改用它） | 忘了自己判状态的写路径 | 绕过 handler 的裸 SQL |
| 库层 | `V0392` 触发器 | 任何路径：导入 upsert / 请假 bridge / 手工脚本 | 硬 `DELETE`（**刻意不拦**：业务删除是软删，硬删只出现在清场与 `ON DELETE CASCADE`，拦了会让演练库与回滚收不回数据） |

判据只有一份：`is_writable(status)` 的白名单与 `state_machine::ATTENDANCE_PERIOD_LOCKED` 由单测
`writable_statuses_are_complement_of_locked` 对拍（改一个忘另一个就红）。

## 验收实测

| 验收项 | 证据 | 结果 |
|---|---|---|
| 锁定后改数据被拒 | `01_db_tests.out` ① + `03_http_smoke.out` | 库层：锁定后 `INSERT` / `UPDATE` / 软删（`deleted_at`）全部被拒（`CP001`→`409 + 68010`），记录值**逐列未变**、条数未变；接口层：锁定后 `PUT /period/{id}`、`POST /period/{id}/recalc` → `409 + 68010`，报文点名期间并指出「先解锁再改」，psql 直查确认库里 `remark` 未被改写（拒绝 ≠ 改了再报错）。冒烟 37 条断言全 PASS |
| 解锁要权限 + 审计 | `01_db_tests.out` ② + `03_http_smoke.out` | 权限：`EMPLOYEE` 角色账号调状态接口 → **403「无权限访问，需要权限: business:hr:attendance:lock」**（与「首次登录必须先修改初始密码」的 403 显式区分，后者靠 SQL 夹具准备排除）；审计：缺原因 400、原因 2 字 400、给原因 200；`biz_status_log` 直查 4 行（建单/提交/锁定/解锁），解锁行带原因且 `operator_id` 非空 |
| 兜底路径同样被拒 | `01_db_tests.out` ①③④ | `CP001` 能被 `is_lock_violation` 认出（**只认 `Display` 认不出来**，真库实测后改为沿 `source()` 链查 `Debug`），翻成 `409 + 68010` 而不是 500；`lock_for_write` 草稿放行 / 锁定 409 / 不存在 404 / 回滚不残留锁；逃生门 `SET LOCAL` 事务内放行、提交后恢复 |
| 门禁 | `02_gates.out` | `rustfmt --check`（本卡 4 个文件）clean；`cargo clippy --all-targets -D warnings` 绿；`cargo test --lib` **5212 passed / 2 failed** —— 两条失败与本卡无关（见下「偏差与报告」第 3 条：另一位 worker 未提交的 `shared/scope.rs` 把 `BIZ_ATTENDANCE_RECORD` 加进 `SCOPED_TABLES`（17 张）而没同步注册表计数常量（16 张）） |
| 真库用例 | `01_db_tests.out` | `business::hr::attendance_lock*` **16 passed / 0 failed**（10 条单测 + 4 条真库用例 + 2 条指纹单测；跑在 145 演练库上） |
| 残留 | `04_residue.out` | 145 上 `hr23%` 库 0 个；18096 无监听；临时 `backend/config/config-hr23.toml` 已删 |

## 关键实现取舍（代码注释里都写全了）

1. **库层守卫必拦 `UPDATE`（含软删），刻意不拦硬 `DELETE`**：业务删除是软删（`deleted_at`），
   硬删只出现在演练清场、`V0383.down.sql` 收表、`ON DELETE CASCADE`；拦了硬删会让「期间一旦锁过就删不掉」，
   演练库只能 drop database，换来的是挡不住任何真实业务路径的「保护」。
2. **逃生门只许事务级**：`SET cloudpivot.attendance_lock_guard='off'`（会话级）会留在连接池里、
   等于把守卫永久关掉且无人知晓；`SET LOCAL` 跟着事务结束自动恢复，默认值 = 守卫开启（忘了设 = 多拦一次）。
3. **迁移里带自检**：`DO $$` 断言「触发器有且仅有 1 个」+「函数体里真有 `LOCKED` 判据」+「真抛 `CP001`」，
   防止有人用空函数把守卫顶掉却以为还开着。
4. **`CP001` 的认法**：`nik-db-core` 的 `from_raw_driver_error` 认不出自定义 SQLSTATE，落到
   `DbError::Query { sql: "", source }`，其 `Display` 是 `query failed: : db error`
   （tokio-postgres 的 `Display` 只写 `"db error"`）。**只认 `Display` 在真库上是永远认不出来**的
   ——第一版就是这么写的，4 条真库用例红在「拒绝必须能认出来」；改成沿 `source()` 链查 `Display`+`Debug` 后全绿。
5. **`locked_at / locked_by` 解锁不清空**（HR-19 的口径）：`locked` 出参表示「**现在**是不是锁定态」，
   解锁后为 `false`；最近一次锁定的审计信息保留，流转轨迹在 `biz_status_log`。真库用例按这个语义断言。

## 偏差与报告（未在卡内自行处理）

- **迁移号段**：设计 §7.1 把 `V0391/V0392` 预留给 `hr_salary` / 调薪·离职**流程**（设计原号 V0385/V0386，
  后来整体顺延）。本卡按「实际最大号 +1」取 **V0392**（此前 business 目录最大是 V0391），
  **HR-38 落地时请从 V0393 起**，已在迁移文件头写明告警。
- **导入路径（HR-21/22）不用改代码也有保护**：`V0392` 在库层兜住 `INSERT/UPDATE`，
  导入 upsert 撞锁定期间会拿到 `CP001`；若导入实现要给出 `409 + 68010` 的友好报文，
  调 `attendance_lock::map_write_error` 即可（`attendance_record_model::write_error` 已接好）。
  本次未改 `impexp/builtin/hr.rs`：该文件当时是**另一位 worker 的未提交工作**，按「只做本卡」的纪律不动。
- **闸②（`scripts/lint/scope_gate.py`）前后都红**：`SKIP_MANIFEST` 存量漏登记 40+ 条
  （含 `impexp/builtin/hr/db_tests.rs` 等在飞文件），与本卡改动无关；本卡的
  `attendance_lock_db_tests.rs` 已登记（前一张卡已补），本卡未新增测试面文件以外的生产文件。
- `state_machine::ATTENDANCE_PERIOD_EDGES` 没有 `SUBMITTED → DRAFT`（HR-08 按设计图落的），
  提交后发现建错月份只能「锁定 → 解锁（带原因）」回草稿或删掉重来 —— 属 HR-08 的边表 + 设计问题，另一张卡。

### 3. 与本卡无关但必须上报的三件（都不是本卡改的，别记到本卡头上）

1. **`PUT /system/auth/password` 恒 500**：该路由**没挂** `#[nik::permission]`，框架认证层因此不往
   extensions 注入 `AuthClaims`，处理器 `Extension(AuthClaims)` 取不到 → axum 报
   `Missing request extension`。空库首启（`must_change_password=true`）时没人能改密 ⇒ 业务接口永久 403。
   复现：空库起实例 → 登录 → `PUT /api/system/auth/password`（本卡冒烟脚本第一版就撞在这里）。
2. **起实例必须用带 `[middleware.auth] enabled = true` 的 profile**：基础 `config/config.toml` 里
   `enabled = false`（认证由 dev/prod profile 打开）。用自定义 profile 忘了写这一行，会看到
   「带 `*:*:*` 的 admin 也被 403」，日志里是 `AuthClaims 未注入 (auth 中间件未执行或配置有误)`
   —— 很容易被误判成「权限码没种」，排查一次 20 分钟。
3. **`cargo test --lib` 有 2 条存量红**（`permission::scope_model::d8_coverage_matches_registry`、
   `shared::guard::preflight_uses_the_registry`）：`backend/src/business/shared/scope.rs` 里
   **未提交**的改动把 `ScopeTarget::BIZ_ATTENDANCE_RECORD` 加进了 `SCOPED_TABLES`（17 张），
   而两处计数常量还是 16。该文件不在本卡提交里（本卡不碰数据权限），归属是那条加登记表的卡。

## 复现命令

```bash
# ① 真库用例（需要一个已迁移的库；本卡用 145 的演练库，跑完已 DROP）
HR_DB_URL=postgres://cloudpivot:***@127.0.0.1:15432/<db> \
  cargo test --lib business::hr::attendance_lock -- --test-threads=1 --nocapture

# ② HTTP 冒烟（实例已起、库已迁移；库连接走 libpq 环境变量供审计直查）
#    PREP_ADMIN_FLAG=1 只用于**演练库**：用 SQL 清 must_change_password（理由见「偏差与报告」第 1 条）
BASE=http://127.0.0.1:18096 ADMIN_PASSWORD='<admin 初始口令>' PREP_ADMIN_FLAG=1 \
PGHOST=127.0.0.1 PGPORT=15432 PGUSER=cloudpivot PGPASSWORD=*** PGDATABASE=<db> \
  bash scripts/verify/hr/attendance_lock_http_smoke.sh

# ③ 起实例（145 的 PG 只监听 127.0.0.1 → 从别的机器要 ssh 隧道；profile 里必须开认证）
ssh -f -N -L 15432:127.0.0.1:5432 root@172.18.10.145
NIK_PROFILE=hr23 NIK_SERVER_LISTEN=18096 NIK_DATABASE_NAME=<db> \
NIK_DATABASE_PASSWORD=*** NIK_ADMIN_PASSWORD=*** \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=*** CLOUDPIVOT_STORAGE_MASTER_KEY=*** ./target/debug/cloudpivot
```
