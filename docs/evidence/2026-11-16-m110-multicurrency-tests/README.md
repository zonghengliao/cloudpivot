# M1-10 · 多币种单测（汇率变动 / 期末调汇 / 报表折算）

- 卡：`t_672a468e`（父卡 `t_7f95005f` M1-09 报表取数按币种）
- 日期：2026-11-16　机器：99（开发机）→ PG `172.18.10.145:5432`（一次性克隆库 `cp_m110_t1..t4`，跑完 DROP）
- 结论：**只加测试，不改业务代码、不改表结构、不加迁移**（head 仍是 V0413，下一卡从 V0414 续）。
  新增 `backend/app/src/business/erp/fx_cycle_tests.rs`（5 条用例：1 条纯逻辑 + 4 条真库），
  把「汇率变动 → 期末调汇 → 报表折算」串成一条链，钉住 6 条跨模块恒等式。
  真库用例 5/5 通过；同库同顺序跑 M1-07 / M1-06 / M1-05 / M1-09 的既有用例全部保持原状（见 §5）。

## 1. 为什么这张卡不是「给已有用例补断言」

M1-04..M1-09 每张卡都钉住了自己那一段，但**没有一条用例把一个月走完**。链上任一环的取数口径
漂了（把调汇凭证算进原币 / 折算列回落 rate = 1 / 调汇后重复计提），单卡用例都可能仍然全绿。
本文件的断言目标是**链上的恒等式**：

| 恒等式 | 用例 |
|:--|:--|
| 报表折合列与调汇重估额是**同一把尺子**（同原币同汇率逐位相等） | `report_conversion_and_revaluation_share_one_ruler`（纯逻辑） |
| 汇率变动只动折算列，不动原币、不动已过账凭证的本位币历史与锁定汇率 | `rate_move_changes_only_conversion_not_booked_history` |
| 调汇差额 = 折算 − 账面；试算不落库；落账后差额归零、原币不变；同期重跑幂等 | `revaluation_posts_diff_then_next_month_only_tops_up` |
| 次月汇率再变只补「本期这段」差额（200），不把历史累计（500）重提一遍 | 同上 · 第二段 |
| 报表折合列逐行 = 原币 × 期末牌价；调汇凭证不进外币原币口径；与余额表 `closing_fc` 对拍 | `statement_conversion_follows_rate_and_ignores_revaluation_voucher` |
| 缺牌价：报表留空（含合计）、调汇 400 + `66082`（同一件事的两种正确反应） | `missing_rate_leaves_statement_blank_but_rejects_revaluation` |

## 2. 夹具口径（为什么用 1999 年）

```text
期间：1999-01 / 1999-02 / 1999-03（本文件独占）
  调汇取数是「凭证日期 ≤ 期末的全部已过账分录」，2031/2050/2064 这些年度都被别的卡的夹具占着，
  断言只能退化成增量。全仓最早夹具年度是 2026（生产期间），其次 2031；1999 年及以前没有任何
  用例造数 ⇒ 本文件可以用**绝对值**断言（谁把取数窗口放错就立刻红）。
科目：标准科目（1001 / 1002 / 1122 / 2202 / 5001），biz_report_item 映射能命中真报表行。
牌价：USD→CNY SPOT [1999-01-01,01-31]=7.0 / [1999-02-01,02-28]=7.3 / [1999-03-01,12-31]=7.5
  （必须给有限末日：模板库那条 effective_to IS NULL 的兜底行会与 1999 年区间判重叠）
凭证：M110-C1 1999-01-10 USD 1000 @7.0 借 1001 / 贷 2202
      M110-B2 1999-02-08 CNY   50      借 1001 / 贷 1002   ← 只改账面不改原币，让调汇 net ≠ 0
      M110-C2 1999-02-05 USD  200 @7.3 借 1122 / 贷 5001
```

一条值得记住的会计恒等式（本卡在夹具设计时踩到的）：**同一张外币凭证借贷两侧用同一个锁定汇率，
逐科目调汇差额必然互相抵消** —— 所以「只有外币凭证」的账上调汇 `net` 恒为 0，凭证里不会出现
汇兑损益行。要让净额 ≠ 0，必须有一张**只动一侧账面**的分录（本位币凭证 / 只涉及未标外币核算
科目的分录）。这条既是夹具要求，也是排障线索：调汇凭证里出现 5603 行 ⇒ 该期间一定有这样一笔。

## 3. 真库跑法与实测输出

```bash
# ① 克隆模板库（模板停在 V0404，缺 effective_to / fc_* / is_foreign_currency）
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m110_t1
# ② 用编译好的后端二进制真启动一次，走引擎迁移器补齐 V0405..V0413（应用 9 个迁移 → head 413）
# ③ 跑用例
ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m110_t1" \
  cargo test --lib business::erp::fx_cycle_tests -- --test-threads=1
```

```text
running 5 tests
test ...::missing_rate_leaves_statement_blank_but_rejects_revaluation ... ok
test ...::rate_move_changes_only_conversion_not_booked_history ... ok
test ...::report_conversion_and_revaluation_share_one_ruler ... ok
test ...::revaluation_posts_diff_then_next_month_only_tops_up ... ok
test ...::statement_conversion_follows_rate_and_ignores_revaluation_voucher ... ok
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 5537 filtered out; finished in 0.43s
```

> SKIP 不算通过：4 条真库用例的每条断言都要查库（余额表 / 调汇凭证分录 / 报表取数），
> 没连上库时 `setup()` 返回 `None` 会打印 SKIP，卡面要求「真跑」⇒ 汇报里贴的是上面这段 pass 数。
> 交叉核对过库侧痕迹：跑完 `biz_voucher` 里能看到用例造的 `FX_REVAL_ACCOUNT|1999-02|1.00000000|300.0000`。

## 4. 夹具卫生：为什么必须「收场」，不只是「setup 清场」

第一版只在 `setup()` 里清场，验证时发现本文件留在 1999 年的凭证会被排在后面的
`fx_balance` / `fx_ledger` 用例算进「期初」，让它们多出几行而红。改成每条用例末尾
`teardown()`（清凭证 / 清牌价 / 清期间 + 把 `is_foreign_currency` 还原成模板默认 0）后：

```text
① fx_balance 单独                     → 15 passed
② 本文件在同一个库上再跑一轮           → 5 passed
③ 本文件跑完，fx_balance 再跑一轮      → 15 passed（库被还回去了）
```

**夹具不止要自己跑得对，还要把库还给下一个人。**

## 5. 既有失败：与本体无关，另行报告

在一个**全新克隆库**上跑 `--lib business::erp::fx`（含本文件）：

```text
test result: FAILED. 51 passed; 8 failed   ← fx_ledger 2 + fx_revaluation 6
```

同样 8 条在**没有本文件参与**的全新克隆库上逐条复现（`fx_ledger` / `fx_revaluation` 单独跑也红），
根因是 M1-07 余额表用例留下的 `M107FX01` 夹具（它 setup 时清自己、teardown 不清）撞上
`fx_ledger` 的「两个外币科目」行数断言：

```text
assertion `left == right` failed: 两个外币科目：[M107FX01 …, M108FX01 …, M108FX02 …]  left: 3  right: 2
```

⇒ 属 M1-07 / M1-08 之间的夹具卫生问题（给 `fx_balance` 补一个 `teardown()` 即可），
本卡按「只做这一件事，不夹带」未改，已写进卡内评语与 metadata 供编排另开卡。

## 6. 检查项

| 检查 | 结果 |
|:--|:--|
| `cargo clippy --lib --all-targets -p cloudpivot -- -D warnings` | 0 warning（含 rustfmt --check） |
| `rustfmt --edition 2024 --check` 两个改动文件 | 干净 |
| `bash scripts/verify/test_seed_gate.sh` | 全过（主数据手写 INSERT 109 = 预算） |
| 前端 | 本卡无前端改动（未跑 pnpm） |
| 迁移 | 无新增（head 仍 V0413） |
