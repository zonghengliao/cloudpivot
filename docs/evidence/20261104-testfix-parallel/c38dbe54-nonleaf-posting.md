# 卡 t_c38dbe54 证据：记账只允许末级科目（FIN-1 5602 变非末级后年结不结平）

日期：2026-11-05 · 分支 HEAD 基线 `11e1ad5`（工作树另有多卡的未提交改动，本卡只提交自己的 5 个文件）
提交：**本卡 6 个文件**（5 个源码 + 本证据；`git log --oneline --grep 't_c38dbe54'` 可定位）
被测库：`cloudpivot_c38dbe54`（`createdb -T cloudpivot_0d7c51f8b`，171 迁移 / max V0232 / 111 表）
提交后复跑确认（本提交的树）：`year_end` **31 passed / 0 failed**（38.12s）、`voucher` **43 passed / 0 failed**（43.81s）

## 1. 结论

| 项 | 结论 |
|---|---|
| 产品口径 | **选 (a)：不允许往非末级（`is_leaf = 0`）科目记账**，记账入口 400 拒绝并点名编码 |
| 三条红（r4_07/08/09） | **已转绿**（`cargo test --lib business::erp::year_end`：31 passed / 0 failed） |
| 年结静默漏结 | 增加 fail-closed 兜底：非末级科目带余额 → 409，不落结转凭证、不写下年期初 |
| 生产数据 | 144 生产库在 V0219（`5602` 仍是末级、无 5602 分录）⇒ **不需要数据迁移** |

## 2. 为什么是 (a)，而不是「年结把父科目余额一并结平」

不是二选一的偏好，而是**文档早已这么定、代码没实现**：

- `docs/architecture/ARCH-6-财务扩展设计.md:2854`：「有人继续往 5602 本级记账 ⇒ 凭证会违反「只能记末级」的会计规则, `voucher::account_required` 已校验 `is_leaf`（§1.3）」；
- `backend/migrations/business/V0226__…up.sql` 头注释：「会计引擎的 `voucher::account_required` 只允许**末级科目**记账（ARCH-6 §1.3 实测）」；
- 实测（本卡）：`load_account_by_code` / `account_required` 的 SQL 只有 `status = 1 AND deleted_at IS NULL`，**没有任何 `is_leaf` 条件**。

即：文档描述的闸从来没落地，V0226 把 `5602`/`2241` 翻成「仅汇总」之后，「能记账但结不平」的中间态就留在生产了。

选 (a) 的会计理由：期初台账（`year_end::account_balances`）、报表（`report::match_account` 的「科目 + 全部下级」前缀匹配）、预算额度三处口径**全部是末级**；父科目可记账 ⇒ 报表父子双计 + 预算漏算 + 年结漏结。

## 3. 改动（5 个文件）

| 文件 | 改动要点 |
|---|---|
| `backend/src/business/erp/voucher.rs` | 模块文档新增「★ 记账科目口径：只允许末级科目」；新增 `non_leaf_codes()`（按编码批查一次，不做 N 次单查）；`post_voucher()` 在借贷平衡校验后、期间闸前加唯一收口校验（`DbError::InvalidValue` → 400，消息点名编码） |
| `backend/src/business/erp/year_end.rs` | 新增 `ensure_no_non_leaf_balance()`（累计到年末口径，409 fail-closed）；`close_year()` 在写任何东西之前调用；`account_balances()` 文档补充；r4_07/08/09 夹具改末级 `5602.01`（新增 `EXPENSE_ACCOUNT` 常量）；新增 `r4_16_posting_to_non_leaf_account_is_rejected`、`r4_17_close_year_refuses_non_leaf_balance` |
| `backend/src/business/erp/report.rs` | `r1_real_full_cycle_is_balanced` 真库夹具 `5602` → `5602.01`；未归类自检清单同步改 `5602.01` |
| `backend/src/business/erp/income_statement.rs` | `r2_real_net_profit_ties_to_balance_sheet_undistributed_profit` 夹具 `5602` → `5602.01` |
| `backend/src/business/erp/cash_flow.rs` | `r3_real_statement_ties_to_balance_sheet_cash` 裸 SQL 夹具 `2221` → `2221.01`（该夹具绕开 `post_voucher`，但不应继续制造父科目分录） |

「为什么校验放在 `post_voucher` 而不是 `account_required`」写进了 `voucher.rs` 模块文档：手工凭证按 `account_id` 提交（`account_by_id`），只在编码解析函数上校验会被 id 路径绕过；而自动凭证 / 手工凭证全部经过 `post_voucher`。

## 4. 其他模块排查（卡面第 4 项）

```
grep -rn '"5602"|"5601"|"5603"|"2221"|"2241"' backend/src
```

除上表三处真库夹具外，其余命中全部是**纯内存**配置/断言（`income_statement::expense_account()`、`cash_flow::item()/two_line()`、`report::match_account` 断言、`aux_tests` 的规则常量），不碰真库，无需改。

库侧非末级科目实测只有 3 个：

```
account_code | account_name | is_leaf | account_category
2221  | 应交税费     | 0 | LIABILITY
2241  | 其他应付款   | 0 | LIABILITY
5602  | 管理费用     | 0 | PROFIT_LOSS
```

144 生产库 `cloudpivot`（`schema_migrations` max = 219）：分录只落在 `1405 / 2202 / 2221.01 / 6401`，**没有任何父科目分录** ⇒ 本卡改动上线无需数据清洗。

## 5. 回归（全部在 `cloudpivot_c38dbe54` 上真库执行）

```
export ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_c38dbe54
cargo test --lib business::erp::year_end
cargo test --lib business::erp::voucher
cargo test --lib business::erp::report
cargo test --lib business::erp::income_statement
cargo test --lib business::erp::cash_flow
```

| 模块 | 结果 |
|---|---|
| `business::erp::year_end`（含 `item_tests`、`year_end_remind`） | **ok. 31 passed; 0 failed**（42.99s） |
| `business::erp::voucher` | **ok. 43 passed; 0 failed**（40.39s） |
| `business::erp::report` | 13 passed **2 failed** —— 见下，均为他卡在案红 |
| `business::erp::income_statement` | **ok. 20 passed; 0 failed**（2.16s） |
| `business::erp::cash_flow` | **ok. 20 passed; 0 failed**（1.53s） |

`report` 的两条红是卡 t_20c290c5（V0221 `bc37b27` 自动开账种出 2026 全年期间）的已知红，**与本卡无关**（本卡改的 `r1_real_full_cycle_is_balanced` 通过）：

```
r1_real_missing_period_is_rejected_but_derivable  report.rs:2340  缺期间行但有账 → DERIVED  left: "OPEN"      right: "DERIVED"
r1_real_previous_period_comparison                report.rs:2408  2026-12 没建期间 → 不出上期对比 left: Some("2026-12") right: None
```

## 6. 新增用例做了什么

- `r4_16_posting_to_non_leaf_account_is_rejected`：往 `5602`（非末级）记 400 元费用 → 断言 `post_voucher` 返回错误、消息点名 `5602` 且含「非末级」、且**凭证没有落库**（`biz_voucher` 计数 0）。库里的 `5602` 若仍是末级（未应用 V0226 的老库）用例自动 SKIP。
- `r4_17_close_year_refuses_non_leaf_balance`：用裸 SQL 造一条落在 `5602` 的历史分录（绕开记账入口），再跑年结 → 断言 `status = 409` + `code = RESOURCE_CONFLICT`、消息点名 `5602`/「非末级」、**没有结转凭证**、**没有下年期初行**。

## 7. 未做 / 风险

- 前端科目选择器需带 `is_leaf=1`（`GET /business/erp/account/page` 已支持该过滤位）；本卡只改后端，前端若仍列父科目，用户会吃 400 而不是选不到 —— 属 FIN 前端卡范围（t_539dab1d 同类），已在此登记。
- `year_end` 的 fail-closed 闸会对「历史上真有父科目余额」的库报 409，这是设计行为；复验请用新建干净库，老库残留（如 `cloudpivot_0d7c51f8b` 里 r4_09 旧夹具留下的 `5602` 150 元）会被判红，不是回归。
