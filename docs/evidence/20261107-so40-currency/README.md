# SO-40 销售单据双币字段 + 汇率锁定 —— 证据包

卡：`t_f98963a3`（ARCH-7 §15.6.3 / §15.6.4 ①②④）· 上游 SO-39（V0272 币种 + 汇率表，commit 9294b07）

## 1. 交付清单

| 类别 | 文件 | 说明 |
|---|---|---|
| 迁移 | `backend/migrations/business/V0314__sales_dual_currency.up.sql` / `.down.sql` | 4 张单据表各加 5 列 + 12 条 CHECK + 存量回填 + 末尾自检 DO 块 |
| 新模块 | `backend/src/business/erp/currency.rs` | 本位币取法 / SPOT 取数 / **锁定决策** / 原币→本位币换算 + `#[cfg(test)] mod tests`（14 例） |
| 建单接入 | `sales_order_handler.rs` + `sales_order_model.rs` | 建单/改单锁定汇率并落 5 列；详情/列表出参带 `currency_code` / `exchange_rate` / `base_*` |
| 收款 | `receipt.rs` | `POST /business/erp/receipt` 接受 `currency_code`，按收款日锁定；Row/列表列/出参带 5 列 |
| 应收 | `receivable.rs` + `receivable_bridge.rs` | 手工挂账按挂账日锁定；**出库自动挂账继承来源销售订单的币种与已锁定汇率** |
| 发票 | `invoice.rs` | 开票按开票日锁定；从应收开票时继承应收**币种**（汇率仍按开票日） |
| 证据 | `scripts/verify/so40_dual_currency_evidence.sh`、`scripts/verify/so40_constraint_probes.sql`、本目录 | 真实 token curl 实测 + 库层约束探针 |

## 2. 迁移 APPLY（空库引擎实跑）

```
ERP_DB=cloudpivot_so40_clean RECREATE=1 BASH scripts/verify/erp_clean_db.sh
→ INFO nik_web::app: 数据库迁移完成: 应用 246 个迁移     （245 + 本卡 1）
→ schema_migrations: 314 | versioned | executed | success=t
→ NOTICE 'SO-40 自检通过：4 表 × 5 列（varchar(8) / 18,8 / 18,4×3）、12 条 CHECK 在位、存量回填一致'
```

`V0314__sales_dual_currency.up.sql` 连跑两遍 = no-op（`ADD COLUMN IF NOT EXISTS` + 约束先查 `pg_constraint`），
down → up 往返后自检仍通过（在 `cloudpivot_so40`（克隆 V0313 迭代库）上演练）。

## 3. 关键决策（与稿面的偏离，均写在迁移文件头）

1. **`base_*` 用 `numeric(18,4)` 而不是卡面/ARCH 的 `numeric(18,2)`**
   §15.6.1 自己写着「金额精度 numeric(18,4)」，四张表的原币列也全是 18,4。用 18,2 会有两个硬伤：
   ① CNY 单回填 `base_* = 原币` 时把 4 位小数的存量金额四舍五入（改历史账）；
   ② 逐项按 2 位舍入后 `base_with_tax = base_amount + base_tax` **不成立**，只能放弃库层兜底。
   取 18,4 后恒等式精确成立，由 CHECK 兜底（`ck_*_base_amount`）。
2. **尾差归含税列**：`base_amount = round(amount × rate, 4)`、`base_tax = round(tax × rate, 4)`、
   `base_with_tax = base_amount + base_tax`（不单独 `round(with_tax × rate)`）。
3. **`exchange_rate > 0` 无条件收紧**：ARCH ④ 只要求「外币时 > 0」，本位币行 rate 恒 1 天然满足；
   无条件约束才能拦住最危险的「外币单 + rate 0 ⇒ base 全 0（金额凭空消失）」。
4. **`biz_receivable.currency_code` 从 `CHAR(3)` 放宽为 `varchar(8)`**：V0287 当年就建了这一列，
   与另外三张表、与 `biz_currency.code` 同型才不会有定长空格语义 / 隐式转换；放宽 lossless。
   down 不回收（收窄会截断，且回退到类型不一致状态）。
5. **取汇率的 SQL 只有一处**（`currency::spot_rate`）：`rate_type='SPOT'` + `effective_date <= 单据日期`
   + `ORDER BY effective_date DESC LIMIT 1`，命中 `uk_biz_exchange_rate` 最左前缀。
   纯逻辑版 `latest_on_or_before` 单测覆盖「未来汇率不算数 / 同日取最新 / 早于全部牌价 ⇒ None」。
6. **外币取不到汇率 ⇒ 400**，不默认 1（`LockOutcome::into_locked` 给出可照做的文案）。
7. **改单的汇率语义**：传 `currency_code` ⇒ 按新的 `order_date` 重新锁定；不传/传空串 ⇒ **沿用已锁定值**
   （不按日期重取，否则「改个交期」就把汇率漂了，§15.6.4 ① 等于没做）。
8. **出库自动挂账继承订单汇率**（不重取 SPOT）：订单上锁的就是这张单的最终汇率，出库晚几天不该让应收漂移。
9. **从应收开票继承币种、汇率按开票日重锁**：发票是独立凭证，「锁的是开票时点」。

## 4. 库层约束探针（`scripts/verify/so40_constraint_probes.sql`，事务内跑完 ROLLBACK）

```
P1 PASS: exchange_rate = 0 被 ck_biz_sales_order_exchange_rate 拒绝
P2 PASS: 小写币种码被 ck_biz_sales_order_currency_code 拒绝
P3 PASS: base_with_tax <> base_amount + base_tax 被 ck_biz_sales_order_base_amount 拒绝
P4a PASS: 收款单 exchange_rate = 0 被拒
P4b SKIP: 库里没有发票行（没跑发票夹具）
P5 PASS: 迁移记账 version=314 kind=versioned applied_via=executed success=t
P6 PASS: 四张单据表各 5 个双币列
```

## 5. 真实 token curl 实测（`api_evidence.txt`，起实例 / `:18099` / `cloudpivot_so40_clean`）

汇率种子（本卡无汇率维护 API，用 SQL 模拟财务维护）：`USD→CNY 7.10000000 @2026-11-01`、`USD→CNY 7.90000000 @2026-11-08`。

| 场景 | 请求 | 结果 |
|---|---|---|
| USD 单 2026-11-07（汇率变动前） | `POST /api/business/erp/sales-order` `{"order_date":"2026-11-07","currency_code":"USD", items: 10 × 100, 税 13%}` | `exchange_rate=7.10000000`、`base_total_amount=7100.0000`、`base_total_tax=923.0000`、`base_total_with_tax=8023.0000`（= 1130 × 7.1） |
| USD 单 2026-11-08（汇率已改 7.9） | 同上，日期换 11-08 | `exchange_rate=7.90000000`、base 7900 / 1027 / 8927 —— **新单按新汇率** |
| **回读老单**（11-07 那张） | `GET /api/business/erp/sales-order/{id}` | `exchange_rate` 仍是 `7.10000000`、base 仍是 7100/923/8023 —— **次日改汇率不影响已建单据** |
| EUR 单（无任何 EUR 汇率） | `POST ... "currency_code":"EUR"` | `HTTP 400` + `10001`「销售订单币种 EUR 在 2026-11-07（含）之前没有可用的 SPOT 汇率（本位币 CNY）…外币单不允许按汇率 1 入账」 |
| USD 收款单 2026-11-07 | `POST /api/business/erp/receipt` | `currency_code=USD`、`exchange_rate=7.10000000`、`base_total_amount=7100.0000`、`base_total_tax=0` |

## 6. 单测

```
cargo test --lib -j 1 -- erp::currency     → 14 passed; 0 failed
cargo test --lib -j 1 -- erp::sales_order  → 28 passed; 0 failed   （含新增 create_order_locks_foreign_currency_and_persists_base_amounts）
cargo test --lib -j 1 -- erp::sales_approval → 9 passed
cargo test --lib -j 1 -- erp::receipt      → 30 passed
cargo test --lib -j 1 -- erp::invoice      → 14 passed
cargo test --lib -j 1 -- erp::receivable   → 43 passed; 2 failed（批量同 filter 时夹具互删；两条用例单独跑均 PASS，见 §8）
```

## 7. 已知遗留（不在本卡范围）

- **汇率维护 API（币种 / 汇率 CRUD + 按日取汇率接口）没有做**：SO-39 只交付了两张表，本卡只做「取数 + 锁定」。
  本卡证据里的汇率是直接写表种进去的。前端「币种 / 汇率」页面同样未做。
- `biz_receivable` 的**列表读模型**（`receivable_query::ReceivableRow`）未加双币列（详情/建单回执已带）。
- 凭证（`biz_voucher`）仍按原币金额记账：本卡只保证「本位币金额已落库」，
  **凭证改按 `base_*` 入账属汇兑/多币种记账那组卡**（ARCH-7 §15.6.4 ② 月末重估同理）。
- 退货（`biz_sales_return`）/ 采购链未纳入（卡面只点名 4 张表）。

## 8. 关于批量 filter 下的 2 条 aging/write_off 用例失败

`cargo test --lib -j 1 -- erp::receivable` 一次性跑 45 条时，
`db_write_off_respects_state_domain` 与 `db_period_cutoff_excludes_later_documents` 会红
（404 / 计数多 1：彼此把对方的夹具删了或加了自己的夹具）。这两个用例包含**跨模块**的
`DELETE FROM biz_receivable`（无 WHERE）与按期间计数，**单独跑均 PASS**（证据见执行记录），
属历史遗留的夹具隔离问题，与本卡改动无关（本卡没有删除/新增任何 AR 行）。
