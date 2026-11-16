# M1-09 · 多币种：报表取数按币种（资产负债表 / 利润表支持币种切换）

- 卡：`t_7f95005f`（父卡 `t_e2797b07` M1-08 外币明细账）
- 日期：2026-11-16
- 结论：**资产负债表 / 利润表新增 `currency` 参数**（可选），不传 / 传本位币 = 与改动前逐字一致的
  本位币报表；传外币 = 只取该币种凭证的**原币**金额出表，并逐行给出「折合本位币」列
  （缺期末牌价留空 + `rate_missing`，绝不按 1 折算）。**没有新表、没有新迁移**（head 仍是 V0413）。

## 1. 接口变化（只加可选参数，老调用零影响）

| 方法 | 路径 | 变化 |
|:--|:--|:--|
| GET | `/business/erp/report/balance-sheet?period=2026-10[&currency=USD]` | 新增可选 `currency`；出参新增 `currency` 块与逐行 `converted_amount` / `previous_converted_amount` |
| GET | `/business/erp/report/income-statement?period=2026-10[&currency=USD]` | 同上；出参新增 `currency` 块与逐行 `current_base` / `ytd_base`、`notes` |
| GET | `/business/erp/report/balance-sheet/export?period=…[&currency=USD]` | 外币口径多两列「折合本位币」；文件名带 `_USD` |
| GET | `/business/erp/report/income-statement/export?period=…[&currency=USD]` | 外币口径多两列「本期折合 / 累计折合」 |

`currency` 块（`report::fx_report::ReportCurrencyRes`）：

```json
{ "mode": "FOREIGN", "currency": "USD", "currency_name": "美元", "base_currency": "CNY",
  "rate": "7.10000000", "rate_date": "2065-10-31", "rate_missing": false, "entry_count": 4 }
```

`mode` = `BASE`（本位币，与改动前逐字一致）/ `FOREIGN`（外币原币）。

## 2. 取数口径（唯一真相）

```
不传 / 传本位币  ⇒ BASE：与 M1-09 之前完全一致（本位币分录 + 期末调汇一起算）
传外币（USD）    ⇒ FOREIGN：
   只取 v.currency_code = 'USD' 的已过账分录
   金额 = COALESCE(fc_debit_amount, debit_amount) − COALESCE(fc_credit_amount, credit_amount)
   报表金额 = 原币；折合列 = round(原币 × 期间末日 SPOT 汇率, 4)；缺牌价 ⇒ null + rate_missing
```

- 与 M1-07 外币余额表 / M1-08 外币明细账**逐字同口径**（同一 WHERE + 同一 `COALESCE`），
  真库用例 `fx_report_closing_matches_fx_balance` 拿 (1001, USD) 的 `closing_fc` 与报表取数对拍。
- 本位币分录（含 M1-05 期末调汇凭证，`rate = 1`、不带原币）**不进**外币口径。
- 勾稽恒等式在 FOREIGN 口径内仍成立（外币凭证原币借贷同时平衡），`check` / `diff` / `unmapped`
  全部按该币种口径给出；`unbalanced_vouchers` 在 FOREIGN 口径判的是「原币借 = 原币贷」
  （单头合计是本位币，与原始分录比只会条条报假红）。

## 3. 入参四条分支

| 入参 | 结果 |
|:--|:--|
| 不传 / 空串 | BASE 200 |
| 本位币 `CNY` | BASE 200（切回本位币不是错误） |
| 格式非法 `usd$` | 400 + `10001` |
| 格式合法但主数据没有 `XYZ` | 400 + **`66084`**（新增错误码，指路「币种 / 汇率」） |

## 4. 改了哪些文件

```
backend/app/src/business/erp/report/fx_report.rs          新增（币种口径 + 折算 + 出参拼装，纯逻辑单测 5 条）
backend/app/src/business/erp/report/fx_report_tests.rs    新增（真库用例 6 条）
backend/app/src/business/erp/report/mod.rs                资产负债表：load_accounts_of / ledger / unbalanced / snapshot 的币种口径 + handler + 导出
backend/app/src/business/erp/income_statement.rs          利润表：同上
backend/app/src/business/erp/report/report_pdf.rs         测试夹具补 2 个新字段（PDF 仍只做本位币口径）
backend/crates/cloudpivot-kernel/src/shared/error_code.rs 新增 ERP_REPORT_CURRENCY_INVALID = 66084
backend/app/src/test_seed.rs                              修好 ensure_accounting_period（原先 status 写成 1、日期走文本参数会报错，一直没人用）
                                                          —— 真库用例改用统一夹具，避免撞 test_seed_gate 的主数据预算
frontend/src/api/business/erp/index.ts                    类型 + 4 个函数加可选 currency
frontend/src/views/business/erp/report/currency-basis-logic.ts  新增（口径文案 / 折合表头 / 导出后缀 / 下拉，两页共用）
frontend/src/views/business/erp/report/balance-sheet/index.vue  币种下拉 + 折合列 + 附注 + PDF 禁用
frontend/src/views/business/erp/report/income-statement/index.vue 同上
frontend/src/constants/error-messages.ts                  66084 的人话文案
frontend/tests/unit/erp-report-currency.test.ts           新增（7 条纯逻辑断言）
scripts/verify/m109_report_currency_drill.sh              新增（真引擎 + 真 HTTP 演练）
```

**没有迁移**：不动表结构、不加权限码（`currency` 是既有两个报表接口的可选参数）。

## 5. 验收证据（全部真跑）

| 项 | 命令 | 结果 |
|:--|:--|:--|
| 后端编译/静态检查 | `cargo fmt --all`（只保留自己的文件）+ `cargo clippy --lib -p cloudpivot -- -D warnings` | 0 warning |
| 报表域真库单测 | `ERP_DB_URL=…/cp_m109_t1 cargo test --lib business::erp::report -- --test-threads=1` | **39 passed / 0 failed**（含新增 6 条真库 + 5 条纯逻辑） |
| 真引擎 + 真 HTTP 演练 | `bash scripts/verify/m109_report_currency_drill.sh` | 见下方汇总 |
| 前端类型 + 单测 + 构建 | `pnpm typecheck:unit && pnpm test:unit && pnpm build` | typecheck 通过；**1906 passed / 0 failed**；build 通过 |
| 菜单/权限门禁 | 本卡不动菜单与权限码（见 §6） | — |

演练脚本 `m109_report_currency_drill.sh`（99 → 145 的 PG，端口 18097，库名含 `upgrade`/`e2e`，脚本自己 DROP）：

- A) 克隆库升级：模板 `cloudpivot_test`（V0404）→ 真启动补 9 个迁移到 V0413 → 重启 0 迁移（幂等）
- B) 空库首启：全量 333 个 versioned 迁移 + `/health` 200
- C) 真 HTTP（建单 → 审核 → 记账，内控开启、制单 ≠ 审核）：
  - 本位币口径 1001 = 2056（1400 − 213 + 792 + 77）
  - USD 口径 1001 = 170（上期 200 − 本期 30；草稿 999 / EUR 99 / HKD 70 / 本位币 50 都不进）
  - USD 折合 170 × 7.1；`entry_count = 4`；勾稽平（资产 270 ≡ 负债 200 + 权益 70）
  - EUR 口径 = 99（币种隔离）；HKD 口径缺期末牌价 ⇒ `rate = null` + `rate_missing` + 折合列留空 + 附注点名
  - 利润表 USD：收入 100 / 成本 30 / 净利润 70，勾稽（净利润 70 ≡ 未分配利润变动 70）；本位币口径 710 / 213 / 497
  - 入参：CNY ⇒ BASE 200、小写 `usd` 规范化、`usd$` ⇒ 400+10001、`XYZ` ⇒ 400+66084
  - 与外币余额表对拍：(1001, USD) `closing_fc = 170` 与报表取数相等
  - 导出：外币口径 xlsx 200 + 真 zip 魔数 + 体积 > 3KB；无码账号 ⇒ 403

## 6. 刻意没做的事（避免夹带）

- **PDF 不走外币口径**：`/report/{type}/pdf` 保持本位币（PDF 版式列固定，塞折合列会挤爆版面）。
  外币口径下前端把 PDF 按钮**禁掉并提示**，否则导出的 PDF 数字看起来像原币、实际是本位币。
- **不加新权限码**：`currency` 是既有 `business:erp:report:balance-sheet` /
  `:income-statement` / `:export` 三个码下的可选参数；切换币种不构成越权（同一份账、同一套码）。
- **不做多币种合计 / 折算合计**：一张报表只按一个币种出（合计相加不同币种是错的）。
- **不做平均汇率折算**：库里有 SPOT / MONTH_END 两种牌价，没有日均价序列；折算统一用期间末日 SPOT，
  并在附注里写明（要平均汇率得先有数据源，属于另一张卡）。

## 7. 已知环境坑（写给下一张卡）

1. **`biz_exchange_rate` 的排除约束在本机 PG 18.6 上对「∞ 上界 + 远期日期」会误判重叠**：
   `daterange('2064-09-01','2064-10-14','[]') && daterange('2026-11-15', NULL, '[]')` 纯函数算返回 **true**。
   ⇒ 夹具有两种解法：① 插的行给**有限** `effective_to`，且先清掉同币对的既有行；② 用全新的币种码。
   本卡的夹具/演练都是「先清 USD/EUR/HKD→CNY 再种有限区间」。
2. **`NULL` 写进 `INSERT ... SELECT` 的 numeric 列会报 `is of type text`**：必须写 `NULL::numeric`。
3. **5602 系管理费用科目要部门辅助核算**，且 5602 本身非末级 ⇒ 建外币凭证改用 `5401 主营业务成本`。
4. **共享库断言一律用增量**：别的报表用例也在 1001 / 1122 / 2202 上记凭证，绝对额会假红。
5. `cargo fmt --all` 会把 `backend/vendor/**` 一起格式化（152 个文件）——**提交前务必 `git checkout -- backend/vendor`**。
