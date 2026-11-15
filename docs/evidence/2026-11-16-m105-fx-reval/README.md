# M1-05 期末调汇（按科目余额生成汇兑损益凭证）· 证据

卡：`t_02c8407a`（M1-05）。落库提交：`6927f92a`（本卡代码被同批次 B6 提交一并带走，见文末「提交说明」）。

## 一、做了什么

| 项 | 内容 |
|---|---|
| 接口 | `POST /business/erp/period/{id}/fx-reval`（`{ "dry_run": true/false }`；`true` = 只试算） |
| 权限码 | `business:erp:period:fx-reval`（V0410：菜单按钮行 父=会计期间 / `sort=6` / title=期末调汇；授 SUPER_ADMIN + FINANCE_MANAGER） |
| 凭证来源 | `biz_voucher.source_type = 'FX_REVAL_ACCOUNT'`、`source_no = 期间码`（与 SO-41 的 `FX_REVALUATION` 分开，两者不会互相判「已提过」） |
| 错误码 | `66082 ERP_FX_REVAL_RATE_MISSING`（期末取不到 SPOT 牌价 → 400，点名币种/期末日/科目） |
| 前端 | 会计期间页行内「期末调汇」按钮 + 试算弹窗（逐科目：原币余额 / 账面 / 期末汇率 / 重估后 / 差额 → 确认落账 → 回显凭证号） |

## 二、会计口径（唯一真相）

```text
原币余额 fc    = Σ(CASE WHEN v.currency_code <> 本位币 THEN COALESCE(fc_debit, e.debit) ELSE 0 END
                 − 同式贷方)                         —— 只取**外币凭证**的分录
账面余额 book  = Σ(e.debit − e.credit)               —— **科目合计**（含本位币分录 / 历次调汇）
期末汇率 rate  = currency::spot_rate(币种, 本位币, period.end_date)   —— 走 currency 模块，不另写 SQL
重估后 reval   = round(fc × rate, 4)（五入）
差额 diff      = reval − book                        —— 正数借该科目，负数贷该科目
净额 net       = Σdiff ≠ 0 时，对侧 5603 财务费用-汇兑损益（net>0 贷方收益 / net<0 借方损失）
```

三条防错口径（都写在 `period_fx_reval.rs` 文件头）：

1. **原币余额只取外币凭证的分录** —— 调汇凭证自己也是落在该科目上的本位币凭证，若用
   `SUM(COALESCE(fc, 本位币))` 会把上期的调汇额当成原币金额再算一遍（差额越滚越大）；
2. **账面必须含本位币分录** —— 否则每期都把历史累计差额重提一次；
3. **账面是「一个科目一个数」** —— 本位币分录没有币种归属，按币种切分会整段丢掉
   （第一版按 `(科目,币种)` 分组算账面，实测把 30 元本位币调整丢了，见「三、踩到的坑」）。

多币种科目（同一科目被记了 USD 与 EUR）：退到「该币种自身账面」并在响应 `warnings` 里点名，
不猜分摊比例；治理办法是把不同币种拆到不同科目。

## 三、踩到的坑（实测，已修）

| 坑 | 现象 | 处置 |
|---|---|---|
| 权限节点 id 复用 `…001022` | 引擎启动 `数据库迁移失败`（该 id 已是「期初余额」页面，主键冲突） | 改用 `…001730`（V0408 开的 0017xx 小段） |
| 同父 `sort_order=5` 已被催办占用 | 侧边栏顺序会抖（约定 12） | 取 6，并在迁移里加「同父 sort 唯一」自检 |
| 账面按 `(科目,币种)` 分组 | 一笔 30 元本位币分录被整段丢掉（对方科目账面 -660 被算成 -700，差额从 -130 变 -80） | 窗口函数 `SUM(...) OVER (PARTITION BY a.id)` 同时取「科目合计」与「分币种」，单币种科目用合计 |
| V0407 汇率区间排除约束 | 脚本直接插两条 USD 牌价 → `ex_biz_exchange_rate_period` 冲突（第二行 `effective_to` 为 NULL 覆盖到无穷） | 脚本显式给 `effective_to`，测试夹具按对清场 |
| 前端 `error-messages` 单测 | 「error_code.rs 里 66xxx 每一条常量，前端表里都有」判红 | 补 `66082` 文案 + 区间注释改到 66082 |

## 四、实测结果（全部真跑）

| 项 | 命令 | 结果 |
|---|---|---|
| 迁移 psql 往返 | `bash scripts/verify/m105_fx_reval_drill.sh` | **PASS=18 FAIL=0**（up×2 幂等 / 结构 6 / down→up / down 护栏反证 4） |
| 真引擎 + 真 HTTP | `bash scripts/verify/m105_fx_reval_engine_and_api.sh` | **PASS=39 FAIL=0**（克隆库升级 +6 迁移、重启 0 个、空库首启 330 条、C1–C19） |
| 真库单测 | `ERP_DB_URL=… cargo test --lib business::erp::period::period_fx_reval -- --test-threads=1` | **11 passed / 0 failed**（5 纯逻辑 + 3 真库 + 3 纯逻辑） |
| 死权限码闸 | `scripts/verify/perm_menu_gate.py`（连真库） | **PASS** |
| 后端静态 | `cargo fmt` + `cargo clippy -p cloudpivot --lib -- -D warnings` | **0 警告** |
| 前端 | `pnpm typecheck && pnpm typecheck:unit && pnpm test:unit && pnpm build` | typecheck 全绿 / **1871 单测全通过** / build 成功 |

真 HTTP 的九条路径：试算（不落库）→ 落账（本位币凭证、rate=1、3 行借贷各 80）→ 逐科目差额
（甲借 50 / 乙贷 80 / 5603 借 30）→ 再跑 skipped=true → 已结账 409 → 未开账 409 → 非法 UUID 400 →
缺牌价 400+66082（点名 HKD）→ 无权限 403；另断言「调汇凭证不污染原币余额（仍 100 USD）」。

## 五、提交说明

本卡的文件在收尾时被同批次的 B6 前端提交（`6927f92a`，`git add -A`）一并带走，
因此本卡没有独立的代码提交号；代码内容以 `origin/main = 6927f92a` 为准：

```text
backend/app/migrations/business/V0410__period_fx_revaluation.{up,down}.sql
backend/app/src/business/erp/period/period_fx_reval.rs
backend/app/src/business/erp/period/mod.rs
backend/crates/cloudpivot-kernel/src/shared/error_code.rs（66082）
frontend/src/api/business/erp/index.ts（fxRevalPeriod / FxRevalResult）
frontend/src/views/business/erp/period/{index.vue,period-logic.ts}
frontend/src/constants/error-messages.ts（66082）
frontend/tests/unit/erp-period-actions.test.ts
scripts/verify/m105_fx_reval_{drill,engine_and_api}.sh
```
