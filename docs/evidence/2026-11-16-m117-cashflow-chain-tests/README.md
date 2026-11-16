# M1-17 现金流底稿单测（含勾稽闭合）· 证据包

> 卡：`t_7e9df7bf`（M1-17【M1·财务】现金流底稿：单测（含勾稽闭合））
> 上游：M1-13（`V0414` 底稿表）/ M1-14（录入 + 勾稽）/ M1-15（间接法调整项）/ M1-16（由底稿出表）
> 下游：M1-18（前端底稿页）、M1-24（收口 E2E）
> 生成时间：2026-11-16（CST）

## 1. 本卡交付了什么

一张**不需要新业务代码**的卡：M1-14 / M1-16 已各自带了内联用例，缺的是**把两段连起来**
的那一层。所以本卡交付两件东西：

| 交付物 | 路径 | 说明 |
|:--|:--|:--|
| 单测套件 | `backend/app/src/business/erp/cashflow_chain_tests.rs` | 11 条：2 条纯逻辑 + 9 条真库；闭合正例 2 条 + 不闭合反例 4 条 + 上期口径/历史可出 3 条 |
| 可复现脚本 | `scripts/verify/m117_cashflow_chain_tests.sh` | 克隆临时库 → 补前置迁移 → 跑本卡 + 全家族 → 无库 SKIP 反证 → **三次代码反证**（改坏代码必须转红）→ 自清场 |

零业务代码 diff（`git diff` 只有 `mod.rs` 的模块注册一行 + 两个新文件），不新增迁移、不新增权限码、
不动前端。**为什么不需要业务改动**：本卡断的是「已有的两条路径要给出同一个结论」，若断言发现
不一致，那是 M1-14/M1-16 的 bug —— 本次 11 条用例第一次跑就全绿，说明这两段此前确实对得上。

## 2. 11 条用例逐条说明（断言的都是绝对值）

夹具期间 `1912-11`（上期）/ `1912-12`（本期）/ `1911-01`（本期，上期 `1910-12` 故意不开账）。
金额写死在用例里（不用增量），取数窗口写错、上期错配、小计漏项都会立刻红。

底稿夹具（两期都闭合）：

```text
1912-11  期初    0   OPER_SALES_RECEIPTS 900(流入)  OPER_PURCHASE_PAYMENTS 400(流出)        期末  500
1912-12  期初  500   OPER_SALES_RECEIPTS 1200(流入) FIN_CAPITAL_IN 300(流入)
                     OPER_PURCHASE_PAYMENTS 700(流出) INVEST_PURCHASES 250(流出)          期末 1050
          ⇒ 流入 1500 / 流出 950 / 净增加额 550 = 期末 1050 − 期初 500
```

| # | 用例 | 类型 | 断的是什么 |
|:--:|:--|:--:|:--|
| 1 | `previous_period_is_previous_accounting_period_across_years` | 纯逻辑 | 「上期」= 上一会计期间**且跨年减年**（`1913-01`→`1912-12`）；非法期间不推导；12 月 / 闰年 2 月的月范围 |
| 2 | `statement_gate_needs_arithmetic_equality_not_only_worksheet_reconcile` | 纯逻辑 | 出表闸的**第二条等式**真在闸上：把「流入 + 流出」当净增加额喂进 `build_check`，底稿那侧仍闭合而报表转不平（差 1900）；余额缺一端 ⇒ 不平（不默认放过）；投资区净额 `−250` 不并正 |
| 3 | `closed_pair_yields_balanced_statement_and_real_xlsx` | 真库 · 正例 | 两期都闭合 ⇒ 报表 `balanced` / `arithmetic_ok` / `previous_balanced` 全真；两栏金额、三活动小计与净额、净增加额、期初期末逐位对；报表合计 = 底稿合计；备注跟着上报表；导出三栏文本（`1,050.00` / `-250.00`）；**真 xlsx 出得来**（PK 魔数 + >2KB） |
| 4 | `resave_and_rebuild_are_idempotent` | 真库 · 正例 | 同一批再存一遍：库里仍是 6 行（4 明细 + 期初 + 期末），重出的报表**逐行相同**（upsert 幂等 + 合并不重复计数） |
| 5 | `wrong_closing_keeps_page_open_but_blocks_export` | 真库 · **反例 A**（期末录错 1150） | ① 保存允许但 `check.diff = −100`；② GET 级仍 200（页面能开去补，带 `problems`）；③ POST 复核闸 400+66087；④ 导出闸 400+66087 且点名 `1150.0000` / `-100.00`；⑤ **改回 1050 立刻能出表**（用户自愈，不用改库） |
| 6 | `missing_closing_row_is_named_and_blocks_export` | 真库 · **反例 B**（期末行不存在） | 缺行 ≠ 0：`closing = null`、`diff = null`、`problems` 点名 `CLOSING_MISSING`（且**不**混成 `IMBALANCED`）；导出报文带「待补 + 期末现金及现金等价物余额 + 请到「现金流底稿」页」；补录后恢复闭合 |
| 7 | `sub_cent_tail_is_not_rounded_away` | 真库 · **反例 C**（尾差 0.0001） | 底稿与报表两侧的差额都精确是 `−0.0001`（**绝不四舍五入抹平**），导出报文带 `0.0001` |
| 8 | `previous_unbalanced_warns_but_does_not_block_current` | 真库 · **反例 D**（上期不闭合） | 上期错只吼不拦：`previous_balanced = false` + `previous` 勾稽一起返回 + 告警「上期底稿未闭合」；本期仍闭合、仍可出表（上期数照出，标为不可信） |
| 9 | `previous_period_not_opened_leaves_previous_column_null` | 真库 · 上期口径 | 上期没开账（跨年 `1910-12`）：上期金额**整列 `null`**（余额 / 净增加额 / 小计 / 明细行四处逐个断）；告警点名 `1910-12`；导出件该列是**空串**而不是 `0.00` |
| 10 | `previous_only_row_survives_after_config_deleted` | 真库 · 历史不丢数 | 上期录过、配置随后被删的项目：本期报表仍列示（本期 0 / 上期 30 / `item_active = false` / 上期小计含它）+ 告警「只在上期底稿里存在」；本期仍闭合可出表，导出件 `0.00 / 30.00` |
| 11 | `closed_period_statement_is_readable_and_exportable_but_not_writable` | 真库 · 期间闸 | 已结账：写被 66085 拒（报文点名期间）；底稿仍可读（`editable=false`）；报表仍闭合、仍能导出真 xlsx（**历史留档**） |

## 3. 三次「反证」（这套用例的效力证明）

全绿只能说明「跑通了」，**能红**才说明「它在守着东西」。脚本每次把一处关键实现改坏、跑一遍、
断言指定用例转红，然后用 `git checkout` 还原（只还原本脚本自己改过的文件，还原后 `git diff --quiet` 复核）。

| 反证 | 改坏哪一处 | 结果 | 转红的用例 |
|:--:|:--|:--|:--|
| A | `reconcile` 的差额 `round_dp(2)`（尾差被抹平） | 10 passed / **1 failed** | `sub_cent_tail_is_not_rounded_away` |
| B | `ensure_exportable` 无条件放行（出表闸失效） | 8 passed / **3 failed** | `missing_closing_row…` / `sub_cent_tail…` / `wrong_closing…` |
| C | `previous_period_of` 只减月份不减年（跨年给 `1913-00`） | 9 passed / **2 failed** | `previous_period_is_previous_accounting_period_across_years` / `previous_period_not_opened…` |

诚实说明：反证只覆盖了上表三条「要命的等式 / 口径」。用例 4（幂等）、10（上期独有行）、11（已结账）
只有正例断言、**没有做反证** —— 它们的失效方式（合并重复计数、历史行静默丢弃、结账后报表消失）
本次没有单独改坏验证过。

## 4. 执行记录（真机、真库，全部可复现）

```bash
bash scripts/verify/m117_cashflow_chain_tests.sh        # 99 → 145:5432（默认）
PG_HOST=127.0.0.1 bash scripts/verify/m117_cashflow_chain_tests.sh   # 在 145 上跑
```

实测（99 上跑，连 `172.18.10.145:5432`）：

| 步骤 | 结果 | 证据文件 |
|:--|:--|:--|
| 建临时库 `cp_m117_chain_<pid>` + 补前置 `V0414` / `V0416` | 4 张前置表齐 | `00_script_run.out` |
| 本卡单测 11 条（真库） | **11 passed / 0 failed**，无一条 SKIP | `03_chain_tests_11.out` |
| 现金流全家族（M1-14 12 + M1-15 12 + M1-16 18 + 本卡 11） | **53 passed / 0 failed** | `04_family_regression_53.out` |
| 无 `ERP_DB_URL`（`--nocapture`） | 打印 `SKIP: 未设置 ERP_DB_URL` 1 次（`OnceCell`），纯逻辑 2 条照跑、真库 9 条空跑 | `05_skip_without_db.out` |
| 反证 A / B / C | 三次都转红、都还原 | `06/07/08_reverse_proof_*.out` |
| 脚本汇总 | **PASS=24 FAIL=0** | `00_script_run.out` 末行 |
| `cargo fmt -- --check` | exit=0（0 diff） | `01_fmt_check.out` |
| `cargo clippy --lib -- -D warnings` / `--lib --tests -- -D warnings` | 两次 exit=0（0 警告） | `02_clippy.out` |
| `bash scripts/verify/test_seed_gate.sh` | 全过（主数据手写 INSERT 109 处 = 预算 109，本卡 0 处） | `09_test_seed_gate.out` |

前置条件提醒（写给下一个人）：

* 模板库 `cloudpivot_test` **还没跑到 `V0414` / `V0416`**（本次核实：`biz_cashflow_worksheet`、
  `biz_cashflow_adjust_item` 都不存在，最老期间是 2030-02）。直接在克隆库上跑 `cargo test` 会报
  「表不存在」而不是「用例失败」，所以脚本会先补这两版 migration。
* 夹具期间 `1912-xx` / `1911-01` 是本文件独占：`1913-xx` 属 M1-14/M1-16 内联用例、`1999-xx` 属 M1-10、
  `2026-xx` 起是生产期间与别卡夹具。清场只按自己的期间码与自己造的临时配置项收，**不按年份通配**。
* 期间行**不手写 INSERT**：走 `crate::test_seed::ensure_accounting_period(year, month)`（它对期间表
  只负责「这一行存在且 `OPEN`」，别的状态由用例自己 `UPDATE`）。这是 P2-62 门禁
  （`scripts/verify/test_seed_gate.sh` ③）的要求 —— 第一版本卡手写了一条被门禁拦在 push 前，已改。
* 用例 3 断言 `warnings.len() == 1`（唯一一条是「模板里还有 N 个配置项本期没录」）。若以后有人往
  `biz_cashflow_item` 加模板行，这条断言只会影响告警条数、不影响金额断言。

## 5. 清场与未动的东西

```text
临时库：cp_m117_chain_<pid>     脚本 trap 自动 DROP（本次两次运行都删掉了）
手动库：cp_m117_unit            验证阶段手工准备，已删
复核： select datname from pg_database where datname like 'cp_m117%'  → （0 行）
模板库 cloudpivot_test          未写（V0414/V0416 表仍不存在、无 191x 期间）；12 行现金流量项目模板未改
反证残留：git status 里只有本卡的两个文件（mod.rs 一行注册 + 新用例文件），三个被改坏的实现文件已还原
```

不碰的东西：不新增/修改迁移（`V0414`..`V0417` 原样不动）、不动 `biz_cashflow_item` 模板行、
不改 `ROUTE_CONTRACT` / 权限码、不新增错误码（用现成的 66085 / 66087 / 66010）、前端零 diff。
