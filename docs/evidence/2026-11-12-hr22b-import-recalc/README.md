# t_de6e8d41 复核：通用 impexp 导入是否重算考勤期间汇总（HR-22 缺口）

> 卡面现象（HR-29 真机，2026-11-12）：考勤台账页走通用 impexp 三段导入，明细落 2 行，
> 但期间头 `employee_count / should_days_total / actual_days_total / ot_hours_total` 仍是 0。
> 卡面要求二选一修：落地 `POST /business/hr/attendance/import`，或把重算接到确认入库钩子。

## 结论

**在 `5e4a6655`（含 HR-22 的 `e9c8f43e`）上，通用 impexp 路径已经会重算期间头，卡面现象不可复现。**

- 重算不是挂在「尚未注册的 HR 专属路由」上，而是长在
  `business/impexp/builtin/hr.rs::AttendanceImport::persist` 里，
  **与记录写入同一个事务**（`db().transaction(|tx| persist(...))`）；
  通用三段（`upload` / `template` / `{id}/confirm`）的确认入库正是逐批调 `persist`，
  所以任何入口（页面、脚本、其他前端、后续批量导入卡）都会重算。
- 空期间 → 上传 2 行 → `confirm` → **不调任何 recalc 接口** 直接读
  `GET /business/hr/attendance/period/page`，即得
  `employee_count=2 / should_days_total=44.00 / actual_days_total=40.00`（与卡面验收逐字一致）。
  见 [`01_acceptance_flow.txt`](./01_acceptance_flow.txt)。
- 锁定期间再导被入库预检拦下（`60014` + Excel 行号），期间头与记录都不动；解锁后再导，汇总跟着变
  （44.00 / 41.00）。见 [`01_acceptance_flow.txt`](./01_acceptance_flow.txt) 第 7 步与
  [`02_unlock_then_reimport.txt`](./02_unlock_then_reimport.txt)。

### 那 HR-29 当时为什么看到 0

时序上唯一能解释的是**当时跑着的后端二进制早于 `e9c8f43e`**：那次实测用的是本机
`:8023` 上一份早已起好的实例（HR-22 落地前后没人重启过），旧代码的 `persist` 只有
`INSERT … ON CONFLICT`，没有任何重算。代码现状与 DB 现状都与卡面描述不符。

## 本次仍做的改动（护栏 + 回归钉）

复核没有停在「不可复现」，因为旧实现确实留了一条**能让同样的现象悄悄回来**的缝：

| 缝 | 现在的处理 |
|---|---|
| `recalc_periods` 丢掉 `recalc_totals` 的返回值 | `affected != 1` ⇒ `DbError` + 点名考勤月（「整批回滚」），不再把「更新 0 行」当成功 |
| 锁定期间与「该重算却更新 0 行」混在一起分不清 | 先读期间状态：`LOCKED` ⇒ 计入 `RecalcReport::skipped_locked`（定稿冻结是设计意图）；其余期间更新 0 行 ⇒ 报错 |
| 文件里的期间码在库里查不到 | 计入 `RecalcReport::missing` 并记 debug 日志（正常应为空） |
| 没有用例钉住「确认入库完成即正确」 | 新增 3 条真库用例（含复刻 handler 两段循环的那条），见下 |

真库用例（`import_post_db_tests.rs`，跑在 145 的 `hr22b_drill`）：

- `confirm_path_leaves_totals_correct_without_any_client_recalc`：复刻
  `handler::import_confirm` 的「校验 → 按 `batch_size` 切批 → 每批一个事务 `persist`」，
  断言期间头瞬间为 2 / 44.00 / 42.00 / 20.50，**中间没有任何 recalc 调用**。
  谁把重算从 `persist` 摘出去（改钩子或删掉），这条立刻红。
- `recalc_skips_locked_period_and_freezes_the_header`：锁定期间跳过重算并如实回报，期间头原样冻住。
- `stale_totals_are_an_error_not_a_silent_success`：把「库里其实 LOCKED 的期间」包成
  `status=DRAFT` 的 target（等价于「解析时 DRAFT、更新时被锁」的并发时序），
  单期间重算必须报错并点名考勤月。

## 验收对照

| 卡面验收项 | 结果 | 证据 |
|---|---|---|
| 空期间 → 上传 2 行 → 确认入库 → 直接读期间分页：`employee_count=2 / 44.00 / 40.00`，不需要前端 recalc | ✅ | `01_acceptance_flow.txt` 第 5/6 步 |
| 期间 `LOCKED` 时导入被预检拦下（现有行为不变） | ✅（拦在确认入库的入库预检，报文带 Excel 行号） | `01_acceptance_flow.txt` 第 7 步 |
| 带 `#[cfg(test)] mod tests` | ✅ 单测 + 3 条真库用例 | `03_lib_db_tests.log`（42 passed / 0 failed） |
| 真库用例跑在 145 演练库并清理 | ✅ `hr22b_drill`（172.18.10.145，用完 drop） | 同上 |

## 环境

- 后端：本仓 `5e4a6655` + 本卡改动，`:18243`，`NIK_PROFILE=test`
- 库：145 `hr22b_drill`（`scripts/verify/make_full_suite_db.sh` 建库：迁移 393 + 演示组织）
- 客户端：`curl`（不用页面，避免任何前端补偿干扰）

## 复现命令

```bash
# 1) 建演练库（在 99 上跑，PG 指 145）
KEEP_DB=1 ERP_DB=b_drill PGHOST=172.18.10.145 DB_USER=cloudpivot DB_PASS=*** \
  OWNER=cloudpivot BIN=/opt/cloudpivot/backend/target/debug/cloudpivot PORT=18242 \
  bash scripts/verify/make_full_suite_db.sh

# 2) 起实例（三把密钥必给）
cd backend && NIK_PROFILE=test NIK_DATABASE_NAME=b_drill NIK_DATABASE_HOST=172.18.10.145 \
  NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD=*** NIK_SERVER_LISTEN=127.0.0.1:18243 \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) X_CONFIG_SECRET_KEY=$(openssl rand -hex 16) \
  ./target/debug/cloudpivot

# 3) 走通用三段：upload → confirm → period/page（不放任何 recalc）
```

真库用例：

```bash
cd backend
export ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/hr22b_drill'
export HR_DB_URL="$ERP_DB_URL"          # ← 必须单独 export：`export A=… B="$A"` 里 B 拿到的是旧值
cargo test --lib business::impexp::builtin::hr -- --test-threads=1 --nocapture
```

> 坑：`export ERP_DB_URL=… HR_DB_URL="$ERP_DB_URL"` 写在同一行时，bash 先展开再赋值，
> `HR_DB_URL` 拿到空串 ⇒ `DatabaseConfig::from_url("")` 退回 **sqlite** 方言 ⇒
> 全部真库用例静默 SKIP（打印「unsupported feature `driver` for dialect `sqlite`」）。分两行 export。
