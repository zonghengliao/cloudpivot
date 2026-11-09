# E2E06-FIX-3 留证：历史占用行回填 + 凭证/占用期间口径明示

卡：`t_1cf59295`（E2E06-FIX-3【P4×2】，父卡 `t_24707809`）
提交：见本目录同批 commit（`fix(erp-budget): 历史占用行回填…`）
环境：145 测试机（`cloudpivot_dev`，后端 `127.0.0.1:8033`，前端 nginx `:80` 静态 `frontend/dist`）

---

## ① 历史占用行回填：「压根没编预算」不再标成超支

### 现场数字（与卡面描述有差异，以库为准）

卡面写「3 行」（`over_amount = 3500 / 2000 / 2500`），145 实测是 **5 行占用行 + 5 张报销单**：

| 单号 | 占用金额 | over_amount（回填前） | 占用状态 |
|---|---|---|---|
| EXP-20261107-000002 | 3500.00 | 3500.00 | CONSUMED |
| EXP-20261107-000003 | 2000.00 | 2000.00 | CONSUMED |
| EXP-20261107-000004 | 3000.00 | 3000.00 | RELEASED |
| EXP-20261107-000013 | 500.00 | 500.00 | OCCUPIED |
| EXP-20261107-000014 | 800.00 | 800.00 | OCCUPIED |

全部 `budget_id IS NULL`（写入时没命中任何预算版本 = 压根没编预算），却带着
`over_budget=1 / over_amount=全额` —— 正是 FIX-5（V0325）之前 `missing_budget` 与 `over`
同时为真的历史残留。原始输出：`before_145.txt` §A / §C / §D。

### 回填

迁移 `backend/migrations/business/V0358__budget_over_flag_backfill.{up,down}.sql`

* 只改 `over_budget = 1 AND budget_id IS NULL AND deleted_at IS NULL` 的行 → `over_budget=0 / over_amount=NULL`；
* **真超支（有 `budget_id`）一行不动**；
* 单据头 `biz_expense_report` 同口径回填（该单所有有效占用行都没绑预算才改）→
  `over_budget=0 / over_amount=NULL / missing_budget=1`，否则报销列表页仍是一排「超预算」红标；
* 回填前把**整行**快照进通用留档表 `biz_data_fix_backup`（`fix_code='E2E06-FIX-3'`，
  `row_data = to_jsonb(整行)`）：可审计「改前长什么样」，`down` 可按快照精确还原；
* 幂等：UPDATE 的谓词就是脏标记本身（重复执行 0 行）；末尾自检两类脏行必须为 0，否则迁移失败。

### 结果（145 实跑，服务重启自动迁移）

迁移日志（`journalctl -u cloudpivot-test`）：

```
NOTICE: V0358 回填完成：占用行 快照 5 / 回填 5；报销单 快照 5 / 回填 5
```

库内验证（`after_145.txt`）：

```
① 脏占用行（over=1 且 budget_id IS NULL） = 0        ← 验收 ①
② 真超支占用行仍在（有 budget_id，over=1）：2 行      ← 验收 ②
   EXP-20261107-000012  2026-11  over=1  over_amount=2500.00  budget_id=01a1b604-…
   EXP-20261109-000062  2027-02  over=1  over_amount=500.00   budget_id=01a1bf08-…
③ 回填前原值已留档：10 行（5 占用 + 5 单头）          ← 验收 ③
```

回填后 5 张单全部 `over_budget=0 / over_amount=NULL / missing_budget=1`
（`after_145.txt` §C/§D），详情接口也随之不再显示「超支」（见下方 19 条探针）。

---

## ② 凭证期间（审批日）≠ 预算占用期间（费用发生日）—— 只明示，不改逻辑

记账口径是设计决定（`docs/architecture/ARCH-6-财务扩展设计.md` §4.5.4），本卡**不动逻辑**：

| 期间 | 取值 | 谁按它算 |
|---|---|---|
| 费用归属期间 | `period_of(expense_date)` | 预算占用（额度闸 / 执行报表「已用·可用」） |
| 记账期间 | `period_of(审批通过日)` | 凭证（`biz_expense_report.period_code` 审批时被重算） |

改动（全部是**呈现层**）：

* 后端 `ReportRes` 新增 `expense_period_code` / `accounting_period_code`（未入账为 `null`），
  取值唯一处 `expense_compute::expense_period_code / accounting_period_code`（3 条单测钉住）；
* 报销详情页（`ExpenseDetailDrawer.vue`）并排显示「费用归属期间 / 记账期间」，
  两者不同时给 `expense-detail-period-mismatch` 说明；
* 预算执行报表（`budget/report.vue`）加 `budget-report-period-note` 口径注脚；
* `ARCH-6` 新增 §4.5.4.1（两个期间的取值 / 落库位置 / 为什么不能统一）。

145 实测（真 token，`e2e06_fix3_period_probe.sh`，PASS=19 FAIL=0）：

```
样本 EXP-20261109-000062：费用 2027-02 / 记账 2026-11 / 占用 2027-02
未入账单 accounting_period_code = null（前端显示「未记账」）
```

跨期样本（库内 3 张）：`EXP-20261109-000062`（APPROVED）、`-000059`（PAID）、`-000060`（CANCELLED）
—— 都是 2027-02 的费用在 2026-11 审批。

---

## 界面实测（145 :80，admin 登录，真实数据）

| 图 | 内容 | 结论 |
|---|---|---|
| `ui_expense_detail_periods_145.png` | 报销详情（`EXP-20261109-000062`，2027-02 的费用在 2026-11 审批） | 「费用归属期间 2027-02 / 记账期间 2026-11」并排显示，下面一条蓝色提示解释两个口径 |
| `ui_expense_detail_unbudgeted_145.png` | 历史脏单详情（`EXP-20261107-000002`，回填前 `over=1 / over_amount=3500`） | 变「提交时未编制预算」蓝条，**没有**超预算红条；列表预算列也从「超预算」变「未编预算」 |
| `ui_budget_report_period_note_145.png` | 预算执行报表（FY2027） | 表头下出现「期间口径」注脚（费用发生期间归集 / 审批期间记账） |

## 145 部署与回归

```
git pull --ff-only origin main                  # HEAD 含 0b86ea2f（另有他人后续提交）
cargo build（debug，1m10s） → systemctl restart cloudpivot-test → health=200
启动迁移日志：V0358 回填完成：占用行 快照 5 / 回填 5；报销单 快照 5 / 回填 5
前端：pnpm exec vite build --outDir dist-fix3 → 双 rename（旧 dist 留 dist-prev-*）
      entry_chunk = assets/index-BYe9Wf1D.js，与 :80 实际返回一致 → DIST_OK，page=200
真 token 探针：scripts/verify/e2e06_fix3_period_probe.sh → PASS=19 FAIL=0（probe_145.out）
E2E-06 全量演练（脚本造完新夹具后再跑一遍，确认没有回归）：
      bash scripts/verify/e2e06_run_all.sh → 汇总 PASS=228 FAIL=0（其中勾稽失败 0）
      勾稽 13 条 0 差异，含「未生效预算却标了超支（本卡数据） = 0」
      演练后复跑只读探针：dirty_usage=0 / dirty_report=0 / true_over_usage=2 / backup_rows=10
      原始断言：run_all_145_asserts.log
```

## 复跑方式

```bash
# 145 上（真 token + 真库）
bash scripts/verify/e2e06_fix3_period_probe.sh          # PASS=19 FAIL=0

# 只读探针（任意带业务的库）
psql "$DATABASE_URL" -P pager=off -At -F '|' -f scripts/verify/e2e06_fix3_probe.sql
#   dirty_usage=0 / dirty_report=0 / true_over_usage>=1 / backup_rows>=1 / cross_period>=0
```

## 本地检查

```
cargo test --lib                 → 3900 passed / 0 failed（新增 3 条期间口径单测）
pnpm typecheck                   → 0 error
pnpm build                       → 成功
pnpm exec tsx --test tests/unit/e2e06-fix3-expense-periods.test.ts → 4/4
```

## 迁移往返（145 事务内预演，未落库）

```
BEGIN; \i V0358.up.sql   → usage 快照5/回填5；report 快照5/回填5；重复执行 0/0
      dirty=0；真超支 2 行不变
      \i V0358.down.sql  → UPDATE 5 + UPDATE 5，脏行 5/5 复原（over_amount 原值逐行一致），留档清空
ROLLBACK
```
