# FIN-3 缺陷回归：撤回后重新提交报销单 500（卡 t_b9c992d1）

日期：2026-11-05 ｜ 执行：backend-developer ｜ 提交：见 `git log -1 --stat`

---

## 一、现象（FE 真机验收发现，前端行为正确）

`POST /api/business/erp/expense/{id}/submit`：`提交 → 撤回（SUBMITTED→DRAFT）→ 再提交`
第二步返回 **HTTP 500**，单据停在 `DRAFT`，日志：

```
ERROR cloudpivot::utils: 数据库操作失败 context="写预算占用" error=query failed: : db error
ERROR nik_web::api: 请求处理异常 status=500 code=10004 detail=写预算占用: query failed: : db error
```

原始日志节选见 `08-redproof-old-binary-server.txt`（旧二进制，修复前）：两条
`INSERT INTO biz_budget_usage …  'OCCUPIED'` 之后立刻 `ERROR … 写预算占用` + `ROLLBACK`。

## 二、根因（已在库上复核，两条独立证据）

唯一索引把**已释放**的历史行也纳入唯一性：

```sql
CREATE UNIQUE INDEX uk_biz_budget_usage_source ON public.biz_budget_usage
  USING btree (source_type, source_id, dept_id, account_id, fiscal_year)
  WHERE (deleted_at IS NULL);          -- ← 谓词里没有 status
```

撤回 / 驳回 / 作废只把行置 `RELEASED`（**不删行**，`deleted_at` 仍为 `NULL`）；再次提交走纯
`INSERT` 又插一条 `OCCUPIED` → 撞唯一键。

`05-redproof-db-uk.txt` 是同一份索引语义的最小复现（事务内构造、末尾 `ROLLBACK`，零残留）：

```
① 首次占用插入成功           rows=1
② 模拟撤回 → RELEASED        deleted_at=(NULL)
③ 旧实现的再插 OCCUPIED → ERROR: duplicate key value violates unique constraint "uk_biz_budget_usage_source"
   → 后续语句: ERROR: current transaction is aborted, commands ignored until end of transaction block
```

第三条错误码 `25P02` 正是 500 的真实成因：PG 里第一条语句报错就把**整个事务**打成 aborted，
旧实现「撞 23505 当幂等成功」只能吞掉第一条明细的错，第二条明细立刻 `25P02`
（`DbError::Query` → 文案 `query failed: : db error`），提交事务整体 ROLLBACK。

## 三、修法：复用行（卡面建议 1），唯一索引不动

`budget_guard::occupy()` 改为「**先复用、后插入**」：

1. `UPDATE biz_budget_usage SET status='OCCUPIED', period_code, amount, amount_net, source_no,
   budget_id, over_budget, over_amount, released_at=NULL, release_reason=NULL, consumed_at=NULL,
   voucher_id=NULL, updated_at=now() WHERE <五元组> AND status='RELEASED' AND deleted_at IS NULL
   RETURNING id`
2. 命中 → 返回同一个 `id`（不再 INSERT）；没命中 → 才走原来的 `INSERT`。

为什么这么做，而不是卡面建议 2（把 `RELEASED` 排除出唯一键）：

- **唯一键语义不变**：一张单 × 一个预算槽天生一行，状态在 `OCCUPIED ↔ RELEASED` 之间往返；
  索引定义与巡检 SQL 一个字都不用改（`07-db-snapshot.txt` 里 `indexdef` 与 V0227 完全一致）；
- **并发仍安全**：`UPDATE … WHERE status='RELEASED' … RETURNING` 会重新求值谓词并持行锁，
  两个并发「再提交」只有一个能把行拉回，另一个落到 `INSERT` → 23505 → 与「双击提交」同一条幂等路径；
- **历史可追溯**：行 `id` / `created_at` 不变，释放原因留在状态日志与审批轨迹里；
- 若改索引，释放/重占的巡检 SQL、并发锁、以及「重复提交当幂等」这条既有设计都要重审
  —— 收益为零，风险明显更高。

`ck_biz_budget_usage_done CHECK ((status='RELEASED') = (released_at IS NOT NULL))` 要求状态与
`released_at` 同进同退，所以复用时会一并清空 `released_at / release_reason / consumed_at / voucher_id`。

改动文件：

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/budget_guard.rs` | `occupy()` 增加条件 UPDATE 复用分支 + 模块文档写明「为什么不能靠撞 23505 当幂等」 |
| `backend/src/business/erp/expense_model_tests.rs` | 新增真库回归用例 `resubmit_after_release_reuses_same_usage_row`（第 16 条） |
| `scripts/verify/fin3_resubmit_e2e.py` | 新增：本缺陷的 46 项真机回归（3 轮撤回重提 / 审批到 APPROVED / 作废 409 / 换槽重提 / 全库唯一性） |
| `scripts/verify/fin3fix_dbsetup.sh` | 新增：一次性干净库（模板 `cloudpivot_fin2asset`，实例启动自动补 V0239–V0246） |

唯一的 `INSERT INTO biz_budget_usage` 就在 `budget_guard.rs` 里（`grep -rn` 全仓仅此一处），
所以这一处收口即覆盖报销与采购两条占用来源。

## 四、验收矩阵（全部为真实执行输出）

| 卡面验收项 | 证据 | 结果 |
|---|---|---|
| 建单→提交→撤回→再提交→APPROVED，usage 仍只有一份（占用中） | `01-accept-resubmit-46.txt` ⒶⒷ | 46/46 通过；EXP-000001 走完 3 轮撤回重提后 `usage` 仍 2 行（3180+1060），审批后转 `CONSUMED` |
| 连续「撤回→再提交」3 轮不产生重复 usage | 同上（每轮都断言 `usage` 行 id 不变、行数不涨） | 3 轮全部复用同一批行 id |
| L1–L8 不变量全绿 | `02-invariants-63.txt` | 63/63 通过，BLOCKER 0 / WARN 0（含 L8「占用合计 = 受控明细价税合计」） |
| 作废后再也不能提交（409），修法不放宽 | `01-…` Ⓒ（DRAFT→CANCELLED）、Ⓓ（SUBMITTED→CANCELLED） | 两次均 409 code=66041，且作废单的 usage 仍是 2 行 RELEASED（没有被偷偷复活） |
| `cargo clippy -D warnings` 干净 | `04-clippy-and-fmt.txt` | `cargo clippy --all-targets -- -D warnings` RC=0（rustc 1.94.1）；`rustfmt --check` 两个改动文件 RC=0 |
| 单测覆盖 | `03-unit-tests-16.txt` | `expense_model_tests` 16/16 通过（真库，11.12s；同一库连跑两轮均绿） |
| 回归（FIN-3 全链路未被改坏） | `06-finse2e-regression-67.txt` | 父卡 `fin3_e2e.py` 67/67 通过（全新库） |

补充证据：`07-db-snapshot.txt`（跑完 3 轮 + 审批 + 换槽重提后的库快照：五元组重复行 0 行、
`EXP-000001` 两行 CONSUMED、`EXP-000004` 一行 RELEASED(WITHDRAWN) + 一行 OCCUPIED）。

## 五、复现 / 复验命令（144 上实测）

```bash
# ① 干净库（模板 cloudpivot_fin2asset → 实例启动时自动补 V0239–V0246）
bash scripts/verify/fin3fix_dbsetup.sh cloudpivot_fin3fix3

# ② 隔离实例（独立库 + 独立端口，收尾务必 stop）
FIN3_DB=cloudpivot_fin3fix3 FIN3_PORT=18034 bash scripts/verify/fin3_instance.sh start

# ③ 缺陷回归（本卡）：46 项
NIK_DATABASE_NAME=cloudpivot_fin3fix3 python3 scripts/verify/fin3_resubmit_e2e.py http://127.0.0.1:18034

# ④ 不变量（L1–L8 在内）
NIK_DATABASE_NAME=cloudpivot_fin3fix3 python3 scripts/verify/erp_invariants.py

# ⑤ 真库单测（不设 ERP_DB_URL 会静默 SKIP = 假绿）
ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_fin3fixut" NIK_PROFILE=test \
  cargo test --lib business::erp::expense_model::expense_model_tests -- --test-threads=1

# ⑥ 门禁
cargo clippy --all-targets -- -D warnings
```

## 六、边界与已知取舍

- **换槽重提**（撤回后把明细改到另一个预算科目）：五元组不同 → 新槽插一条 `OCCUPIED`，
  旧槽那行留 `RELEASED` 作历史痕（`01-…` Ⓕ 用例钉住：`usage` = 旧槽 RELEASED + 新槽 OCCUPIED，
  有效占用合计 = 改后明细金额，L8 仍成立）。
- **同一单据同一槽的多行明细**：在提交前的 `check_for_expense` 里已按科目合并成一个槽
  （`ExpenseBudget.checks` 逐槽唯一），所以不会出现「同单同槽两次 occupy」。
- `release()` 的既有语义（`OCCUPIED` **和** `CONSUMED` 都会被释放）本次未改动；
  已入账单据的回冲走凭证红冲，不依赖 usage 状态。
- 本卡没有改迁移：V0227 的唯一索引保持原样（这也是「复用行」相对「改索引」的优势之一，
  生产库不需要下发 DDL）。
