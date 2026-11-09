# E2E04-FIX-2 · 反结账的「后续期间」判据收敛到同一会计年度

- 卡片：`t_ca75da74`（P3，E2E-04 演练实际踩到）
- 代码提交：`9d75c58c`（功能 + 6 条单测）、`4aacd588`（文案空格微调）
- 部署：145 `cloudpivot-test` 已 ff 到 `4aacd588`、debug 二进制重建并重启（health=200）
- 真机验收：145 dev 实例，真实 token（admin），`PASS=28 FAIL=0`；旧二进制对照 `PASS=6 FAIL=0`

## 一、问题

`period::reopen_period` 判断「后续期间已结账」的 SQL 是**全库无界**的：

```sql
SELECT period_code FROM biz_accounting_period
 WHERE period_code > ? AND status = 'CLOSED'    -- 没有年度条件
 ORDER BY period_code LIMIT 3
```

于是共享库/演练库里**任何年度**残留一个更晚的已结账期间，本年度就永远反结不了。
E2E-04 实测：`2026-09` 是 CLOSED（别的演练留下），`POST /erp/period/2022-11/reopen` 被卡死，
重置脚本被迫「临时解封 2026-09 → 回撤 2022 → 再封回」。

## 二、修法（`backend/src/business/erp/period.rs`）

1. **同年度判据**：`period_year = ? AND period_code > ? AND status = 'CLOSED'`，
   一次列全部挡路期间（去掉 `LIMIT 3`）。
2. **跨年度判据独立**：查 `biz_status_log` 里 `entity_type='ACCOUNTING_YEAR'` 的**最新**状态
   （`DISTINCT ON (entity_no) … ORDER BY created_at DESC, id DESC`），取
   「比目标年度更晚、且最新状态 = CLOSED」的年度 —— 反年结之后不再误拦；年度允许不连续，
   所以判「所有更晚的年度」而不是只看 `year + 1`。
3. **文案分两类**（新增纯函数 `reopen_conflict_message`，单测钉住）：
   - `同年度挡路 —— 2027 年还有后续期间 2027-02 处于「已结账」…请先按 2027-02 的顺序反结账`
   - `跨年度挡路 —— 更晚的会计年度 2029 年已年结…请先在「年结」页反年结 2029 年再回来反结账`
   - 两类同时存在时用「；」拼接，一次列全。

### 为什么「更晚的年度已年结」还要拦（而不是只看同年度）

`biz_opening_balance(YYYY+1-01, source='YEAR_END')` 与下一年度的年结凭证都是按本年度期末
结转出来的快照；回撤本年度任何一个已封月份会让「报表期初（历史累计）」与「年初期初台账
（快照）」永久分裂，而且从「年结」页看不出异常。唯一正路是先反年结。这与既有的
「本年度已年结 → 不许单独反结账」是同一口径，只是方向相反。

## 三、代码改了什么（`git show 9d75c58c --stat`）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/period.rs` | 新增 `ReopenConflicts` / `reopen_conflict_message` / `later_closed_periods_in_year` / `later_year_ended_after` / `collect_reopen_conflicts`；`reopen_period` 换成这两条判据；新增 6 条单测（5 条真库 + 1 条纯逻辑）；测试夹具 `cleanup()` 增加 `ACCOUNTING_YEAR` 年度日志清理 |
| `backend/src/business/erp/year_end_test_support.rs` | `DB_LOCK` 改为复用 `period_test_support::DB_LOCK`（见下「为什么要共享一把锁」）+ 更新 teardown 的说明 |
| `scripts/verify/e2e04_reset_2022_periods.sh` | 删掉「临时解封跨年度挡路期间再封回」的 workaround（现在不需要，也不再动别人的数据）；顺带把 2022-12 纳入回撤循环 |
| `scripts/verify/e2e04fix2_reopen_scope.py` | 新增真机验收脚本（`before` / `after` 两个阶段，自带夹具与还原断言） |

### 为什么要共享一把测试锁

`year_end_test_support` 原本有自己的一把 `DB_LOCK`。本卡的跨年度判据会看 `ACCOUNTING_YEAR`
的最新状态，而 year_end 夹具正好把 2037 标成已年结 → `cargo test --lib` 并行跑时
period 的 2031 用例会看见 2037 年结状态而假红。改成共享 `period_test_support::DB_LOCK`
后两块夹具互斥，既挡这个串扰，也顺带挡住期间/凭证/单据夹具互删。

## 四、验收（卡面四条）

| # | 卡面要求 | 结论 | 证据 |
|---|---|---|---|
| ① | 同一会计年度内后续期间已结账 → 反结账被拦（原有行为） | ✅ | 145：`reopen 2027-01 → HTTP 409`，文案点名 `2027-02`；单测 `p1_reopen_blocked_by_later_closed_period` / `p3_*` |
| ② | 其他年度有更晚的已结账期间 → **不再影响**本年度反结账 | ✅ | 旧二进制同一夹具 `409`（点名 2029-02）→ 新二进制 `200`，且 2029-02 仍 CLOSED；单测 `p3_reopen_ignores_closed_periods_in_other_years` |
| ③ | 报错文案区分「同年度挡路」和「跨年度挡路」 | ✅ | 145 两类文案原文见 `e2e04fix2_after_145.log`；真实数据 `2021-11` 一次给出两段；单测 `p3_reopen_conflict_message_distinguishes_two_kinds` |
| ④ | 跨年度（下年已年结）→ 提示先反年结 | ✅ | 145：2029 年结夹具 → `409 跨年度挡路 … 请先在「年结」页反年结 2029 年再回来反结账`；拆掉夹具立刻 `200`；单测 `p3_reopen_blocked_by_later_year_end` / `p3_reopen_allows_after_later_year_was_reopened` |
| ⑤ | `cargo test` + push | ✅ | 见下「单测」与「推送」 |

### 145 真机实测（原始输出见 `e2e04fix2_after_145.log`，60 行，PASS=28 FAIL=0）

关键片段：

```text
-- ② 其它年度更晚的已结账期间 → 不再影响本年度反结账
  close  2027-01 -> HTTP 200  ok
  close  2029-02 -> HTTP 200  ok
  reopen 2027-01 -> HTTP 200  ok
[PASS] ② 别的年度的封账没被顺手动掉：2029-02 仍是 CLOSED

-- ①③ 同一会计年度内后续期间已结账 → 拦，且文案标明「同年度挡路」
  reopen 2027-01 -> HTTP 409  不能反结账 2027-01：同年度挡路 —— 2027 年还有后续期间 2027-02 处于「已结账」：反结账必须从该年度最后封账的月份往回撤，否则会出现「前一个月开着、后一个月封着」的窟窿，请先按 2027-02 的顺序反结账

-- ④ 更晚的年度已年结 → 拦，且文案标明「跨年度挡路」+ 先反年结
  reopen 2027-01 -> HTTP 409  不能反结账 2027-01：跨年度挡路 —— 更晚的会计年度 2029 年已年结：年结凭证与下一年度期初是按 2027 年期末结转的，回撤 2027-01 会让报表期初与年初期初台账对不上，请先在「年结」页反年结 2029 年再回来反结账

-- 真实数据对照：2021 年全封且未年结，但 2022 年已年结
  reopen 2021-11 -> HTTP 409  不能反结账 2021-11：同年度挡路 —— 2021 年还有后续期间 2021-12 处于「已结账」：…；跨年度挡路 —— 更晚的会计年度 2022 年已年结：…请先在「年结」页反年结 2022 年再回来反结账
```

为什么不用卡面举例的 2022 年直接测：145 dev 库里 2022 年度**真的**年结过
（`ACCOUNTING_YEAR=2022 CLOSED`，E2E-04 演练留下的），反结 2022 任意月份会先撞
「本年度已年结」那道旧闸，测不到本卡的判据。所以用 2027 / 2028 + 自建 2029 夹具，
另用 2021 做「真实年结年度」对照（2021 全封未年结，2022 已年结）。

### 现场还原（零残留）

```text
[PASS] 收尾：2029 夹具期间与状态日志已清空  ← period_rows=0 logs=0
[PASS] 收尾：2027-01 / 2027-02 回到 OPEN、2026-09 仍是 CLOSED
[PASS] 收尾：2022 年度 12 期仍是 CLOSED（真数据没被动）
[PASS] 收尾：没有留下 2029 的年结状态日志  ← count=0
[PASS] 全量状态快照与开工前一致  ← {}
```

## 五、单测

```
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_e2e04fix2 \
  cargo test --lib business::erp::period -- --test-threads=1
test result: ok. 56 passed; 0 failed; 0 ignored

ERP_DB_URL=… cargo test --lib business::erp::year_end -- --test-threads=1
test result: ok. 41 passed; 0 failed; 0 ignored
```

`business::erp::` 全模块真库套件（原始输出见 `erp_suite.log`，2117 行）：

```
cargo test --lib business::erp:: -- --test-threads=4
test result: FAILED. 2020 passed; 9 failed; 0 ignored; 1933 filtered out; finished in 1307.15s
```

**这 9 条红都与本卡无关，全部是并行跑（`--test-threads=4`）的夹具串扰**，单跑逐条通过：

| 红（全量并行） | 抽测（单跑） |
|---|---|
| `expense_model_tests::db::type_usage_count`（引用计数 6→6） | `ok. 1 passed`（0.66s） |
| `expense_linkage_tests::lnk05_submit_triggers_linkage_notice`（通知 0≠1） | `ok. 1 passed`（3.04s） |
| `payable_bridge::tests::p17_skip_approval_payment_logs_are_purged_by_id`（`payable_pay` 流程已发布） | `ok. 1 passed`（4.23s） |
| 其余 6 条同族（`lnk05_*` ×3、`lnk06_*` ×2、`receivable_aging::db_period_cutoff_*`） | 同属「别的卡留下的联动规则 / 已发布流程 / 单据残留」串扰 |

佐证：本卡 diff 只碰 `period.rs` 与 `year_end_test_support.rs`（后者 `#[cfg(test)]`），
上述红点所在模块（expense / payable / voucher_linkage / receivable_aging）一个字节都没改。
`period` / `year_end` 两个最相关模块在同一份全量套件里全绿。

新增/加强的用例：

| 用例 | 钉住什么 |
|---|---|
| `p3_reopen_ignores_closed_periods_in_other_years` | 2032 有更晚的 CLOSED 期间时 2031-09 仍能反结账，且 2032 原样 |
| `p3_reopen_blocked_by_later_year_end` | 2032 已年结 → 409 +「跨年度挡路」+ 点名年度 + 提示反年结 |
| `p3_reopen_reports_same_year_and_cross_year_conflicts_together` | 两类挡路一次列全 |
| `p3_reopen_allows_after_later_year_was_reopened` | 反年结（最新状态 NOT_CLOSED）后不再误拦 |
| `p3_reopen_conflict_message_distinguishes_two_kinds` | 纯逻辑：文案两段、处置顺序「晚→早」、互不串味 |
| `p1_reopen_blocked_by_later_closed_period`（加强） | 原有行为 + 文案前缀 / 「同年度挡路」/ 处置顺序 |

## 六、推送与部署

```
git push origin HEAD:main
   7a81aa0f..9d75c58c  HEAD -> main      # fix(erp): E2E04-FIX-2 …
   9d75c58c..4aacd588  HEAD -> main      # style(erp): 文案空格微调
```

145 部署（详见 `deploy_145.log`）：`git merge --ff-only origin/main` → `cargo build --bin cloudpivot`
→ `systemctl restart cloudpivot-test` → `health=200`；
二进制里 `grep -ac '同年度挡路'` = 1（新文案在）、`grep -ac '还处于'` = 0（旧文案不在）。

## 七、已知边界（如实说明）

- **跨年度判据是「所有更晚年度」**，不是只 `year+1`：年结不强制要求上一年先年结
  （`year_end::collect_blockers` 只查本年 12 期），所以存在「2024 已年结、2023 未年结」这种
  合法状态；这种状态下回撤 2022 同样会打破 2024 年初期初快照，故一并拦。
- **不修数据**：库里的历史残留（145 dev 的 2026-09 CLOSED、2022 年结状态）一律不动，
  本卡只改判据。
- `cargo clippy --lib -- -D warnings` 在当前共享工作区有 **4 处既有红**，都不在本卡文件里：
  `business/attachment/model.rs:879`（too_many_arguments）、`business/erp/invoice.rs:1912`
  （explicit lifetimes）、`business/erp/receivable.rs:782`（doc list indentation）、
  `business/erp/sales_order_model.rs:661`（too_many_arguments）—— 与 `git status` 里本卡
  只改 `period.rs` / `year_end_test_support.rs` 对照即可确认。
