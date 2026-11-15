# M1-07 外币余额表（V0412）· 施工与实测证据

- 卡：`t_c456af9e`（M1-07【M1·财务】多币种：外币余额表）
- 日期：2026-11-16
- 迁移：`V0412__fx_balance_report`（只种一个页面权限码 + 菜单页面行 + 授权 + 自检；**不动表结构**）
- 接口：`GET /business/erp/report/fx-balance?period=yyyy-MM[&currency=USD]`
- 权限码：`business:erp:report:fx-balance`（页面 `menu_type=2`，父=财务管理 `…0e02`，`sort=50`）

## 一、口径（唯一真相，与 M1-04 / M1-05 逐字对齐）

```text
范围  biz_account.is_foreign_currency = 1（V0408）且未删除
      × biz_voucher.status='POSTED' 且未软删、voucher_date <= 期末日
分组  (科目, 分录所在**凭证**的币种)；本位币凭证（currency_code = 账套本位币）不成行

原币  fc   期初 = Σ(fc_debit − fc_credit)  FILTER (voucher_date <  期间首日)
           本期借/贷 = Σ fc_debit / Σ fc_credit FILTER (voucher_date >= 期间首日)
           期末 = 期初 + 本期借 − 本期贷        （一律「借方为正」的有符号口径）
账面  book 同上三式，但取 debit_amount / credit_amount（**全部**已过账分录）
折算  rate = spot_rate(币种, 本位币, 期间末日)   即期 SPOT，最近一条（V0407 区间口径）
           conv = round(期末原币 × rate, 4)
           diff = conv − 期末账面              （尚未入账的汇兑差额；正 = 借方调增）
```

三条不能改的判据（写错任意一条，报表数字会静默失真）：

1. **原币只取外币凭证** —— `COALESCE(fc_debit, debit)` 无脑求和会把**调汇凭证自己**
   算进原币（本位币凭证金额恰是上期调汇差额），原币余额越滚越大。
2. **账面必须含本位币分录（含历次调汇）**，且**一个科目一个账面**：本位币分录没有币种
   归属，按币种切分会整段丢掉。单币种科目用「科目合计」，同一科目挂多币种时退到
   「该币种账面」并在 `warnings` 点名（与 `period_fx_reval::FxAccountBalance::book_balance()`
   同一个判断，报表差额必须等于调汇凭证会调的金额）。
3. **缺牌价不按 rate = 1 折算**：`close_rate / converted_base / reval_diff` 三项 `null`、
   `rate_missing = true`、币种进 `missing_rates`、`hints` 指名去补牌价；`Σconverted` 与
   `Σdiff` 只在**全部**币种都有牌价时给值（半套折算的合计会被当成真数抄走）。

## 二、改动清单

后端：

| 文件 | 内容 |
|------|------|
| `backend/app/src/business/erp/fx_balance.rs` | 新增（约 1750 行）：模块文档 + 纯逻辑 + 取数 SQL + handler + 15 个单测（9 纯逻辑 / 6 真库） |
| `backend/app/src/business/erp/mod.rs` | 注册 `pub mod fx_balance;`（+ 模块表注释） |
| `backend/app/migrations/business/V0412__fx_balance_report.up.sql` | 权限码 + 菜单页面行 + 两角色授权 + 自检块 |
| `backend/app/migrations/business/V0412__fx_balance_report.down.sql` | 回收授权与菜单行 |

前端：

| 文件 | 内容 |
|------|------|
| `frontend/src/api/business/erp/fx-balance.ts` | 接口层（类型逐字抄后端 + `fetchFxBalance`） |
| `frontend/src/views/business/erp/report/fx-balance/index.vue` | 报表页（期间 / 币种 / 汇总段 / 明细段 / warnings / hints） |
| `frontend/src/views/business/erp/report/fx-balance/fx-balance-logic.ts` | 纯逻辑（金额格式化、借贷方向、缺牌价占位、口径标签） |
| `frontend/src/router/index.ts` | 手工注册路由（漏了 = 点菜单 404） |
| `frontend/tests/unit/erp-fx-balance.test.ts` | 10 条纯逻辑单测 |

验收脚本：`scripts/verify/m107_fx_balance_drill.sh`

## 三、实测结果

### 3.1 真引擎 + 真 HTTP（`bash scripts/verify/m107_fx_balance_drill.sh 172.18.10.145`）

```
A) 克隆库升级（模板库停在 V0404）
   应用 8 个迁移（V0405..V0412）｜V0412 自检 NOTICE 打印｜schema_migrations 412/true
   菜单页面行 2/…0e02/50/1/外币余额表//business/erp/report/fx-balance｜授权 2 角色
   重启：应用 0 个迁移｜versioned 326 → 334（+8）
B) 空库首启：344 个迁移，versioned 332 条，V0412 自检通过
C) 真 HTTP（82 项断言）：
   外币核算标记走 M1-03 接口（PUT /account/{id}/foreign-currency）→ 库里落成 4 个
   凭证全部走「建单(admin) → 审核(checker) → 记账」真实路径（内控开关保持开启）
   主用例（2061-10）：
     USD 资产  期初 200 / 本期借 100 / 期末 300；账面 1400 + 720 = 2120
               期末牌价 7.9（不是更晚的 8.5）⇒ 折算 2370、差额 250
     USD 对方  期末 −300；账面含那笔本位币借 50 ⇒ −2070（按币种切分会丢这 50）
               ⇒ 折算 −2370、差额 −300
     EUR 资产/对方 期末 ±100，账面 ±800；期末无 EUR 牌价 ⇒ 三列 null + rate_missing
     合计：账面 50；折算合计 null；差额合计 null；missing_rates ['EUR']
     干净夹具 warnings 为空；草稿 999 USD 不进表；二次查询逐字段相同
   币种过滤（小写 usd 也认）/ 筛本位币返回空表 + 解释 / 非法码 400+10001
   非法月份 400+10001 / period 缺失 400+10001 / 未建账月份 200 + 自然月 + hints
   只有别的报表码的账号 ⇒ 403；同一账号访问资产负债表 ⇒ 200（证明 403 来自码）
   无 token ⇒ 401
汇总：PASS=82 FAIL=0
```

### 3.2 真库单测（空库首启库 `cp_m107_t1`，`--test-threads=1`）

```
ERP_DB_URL=postgres://…/cp_m107_t1 cargo test --lib business::erp::fx_balance
test result: ok. 15 passed; 0 failed
```

覆盖：期初/本期/期末三层数字、账面含本位币分录、多币种科目退化 + warning、
缺牌价三列 null、草稿与期末之后凭证不进表、未标记科目的外币分录报警、币种/月份入参校验、
未建账月份按自然月（含余额结转）。

### 3.3 静态检查

```
cd backend && cargo fmt && cargo clippy --lib -- -D warnings     → 0 warning
cd frontend && pnpm typecheck / typecheck:unit / test:unit / build → 见 §3.4
bash scripts/verify/menu_invariants.sh cp_m107_t1                → 8/8 全绿（菜单总数 505）
python3 scripts/verify/perm_menu_gate.py（NIK_DATABASE_* 指 cp_m107_t1）→ PASS（missing_in_menu=0）
```

### 3.4 前端

```
pnpm typecheck       → 通过
pnpm typecheck:unit  → 通过
pnpm test:unit       → 1890 条全通过（本卡新增 10 条：tests/unit/erp-fx-balance.test.ts）
pnpm build           → 通过
```

## 四、踩到的两个坑（写下来给下一个用同一批表的卡）

1. **汇率表区间排除约束**（V0407）会让「直接 INSERT 多条历史牌价」全部冲突：
   `effective_to` 默认 NULL = 无限，先插早行就把后面的行全挡住。夹具里必须按写入口
   的口径**派生** `effective_to`（= 同组下一条生效日 − 1，末行 NULL）。单测 `seed_rate`
   与演练脚本 `seed_rate` 都已按此实现（并各留了一段注释解释为什么）。
2. **缺牌价用例必须让汇率行在期末前失效**（`effective_to` 写成期末之前），否则
   `effective_to IS NULL` 会被取数谓词认为「一直有效到未来」，用例永远绿得没有意义。

## 五、未做的事（刻意，不夹带）

- **导出 xlsx**：卡面范围只有「按币种列示余额 + 本位币折算」，导出不在本卡。将来做导出时
  复用既有报表导出码 `business:erp:report:export`，不新增码。
- **不建台账表 / 不落快照**：报表是凭证的派生视图，落表 = 双真相（红冲后快照不改、库里
  无法跨表断言）。历史版本靠「已结账期间只读 + 凭证留痕」保证。
- **外币明细账（M1-08）** 不在本卡；本表的取数 SQL 已按「按科目扫外币分录」的形态写，
  明细账可以直接复用同一套 `fc_*` 口径。
