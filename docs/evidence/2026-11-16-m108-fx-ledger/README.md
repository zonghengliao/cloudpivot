# M1-08 外币明细账（V0413）· 施工与实测证据

- 卡：`t_e2797b07`（M1-08【M1·财务】多币种：外币明细账）
- 日期：2026-11-16
- 迁移：`V0413__fx_ledger_report`（只种一个页面权限码 + 菜单页面行 + 授权 + 自检；**不动表结构**）
- 接口：`GET /business/erp/report/fx-ledger?currency=USD[&period_from=2026-11&period_to=2026-11][&account_code=1001][&page=1&page_size=20]`
- 权限码：`business:erp:report:fx-ledger`（页面 `menu_type=2`，父=财务管理 `…0e02`，`sort=51`）

## 一、口径（唯一真相，与 M1-04 / M1-05 / M1-07 逐字对齐）

```text
范围  biz_account.is_foreign_currency = 1（V0408）且未删除
      × biz_voucher.status='POSTED' 且未软删、**凭证币种 = 请求币种**
      × 可选日期区间 [period_from, period_to]（缺省 = 全部历史）

原币  fc = COALESCE(fc_debit_amount, debit_amount)   ← 与外币余额表（M1-07）同一表达式
      某科目期初 = Σ(fc 借 − fc 贷)          for voucher_date <  区间首日
      本期借/贷  = Σ fc 借 / Σ fc 贷          for voucher_date >= 区间首日
      期末      = 期初 + 本期借 − 本期贷       （「借方为正」的有符号口径）
      逐行余额  = 该科目期初 + 按流水顺序累计到本行的净额（**按科目分别累计**）
本位币       = 该币种原币行的 debit_amount / credit_amount（**不含历次调汇**）
```

五条不能改的判据（写错任意一条，账会静默失真）：

1. **币种必填且不接受本位币**：本位币分录没有原币口径（`fc_*` 为 NULL），查本位币流水应当
   用「总账 / 科目余额表」的 `ledger/page`。缺失 / 非三位大写 / 等于账套本位币 ⇒ `400 + 10001`，
   报文点名去哪个页面查。**不**回落成「查全部币种」（用户以为筛了 USD 却看到混合流水）。
2. **原币只认「凭证币种 = 请求币种」的分录**：期末调汇（M1-05）落的是**本位币**凭证、
   金额恰是上期调汇差额，并进外币明细账会让原币余额被自己的调汇额污染。代价写在页面与
   `hints` 里：本表「本位币」列不含调汇，含调汇的账面余额看外币余额表。
3. **逐行余额必须按科目分别累计**：明细账的余额是「某科目的余额」，全局游标累加会让 A 科目
   的余额串到 B 科目头上且看不出来。翻页用「分页取行 + 同序前缀和子查询（按科目分组）+
   Rust `Decimal` 累加」实现，排序键 `a.account_code, v.voucher_date, v.voucher_word,
   v.voucher_no, e.line_no` 三处逐字一致。
4. **数据脏只提示不拦**：外币分录落在**未标记**科目、或 `fc_*` 为空的历史脏数据，都进
   `warnings` 并说明影响（本表按 `is_foreign_currency=1` 取数，与余额表同口径）。报表不该
   因为数据脏就打不开，但必须让人看见脏在哪。
5. **与外币余额表同口径同数字**：同一 `WHERE` + 同一 `COALESCE`，某 `(科目, 币种)` 的
   `closing_fc` 必须相等。真库用例 `fx_ledger_closing_matches_fx_balance` 与演练脚本
   各有一道对拍断言盯着这件事。

## 二、改动清单

后端：

| 文件 | 内容 |
|------|------|
| `backend/app/src/business/erp/fx_ledger.rs` | 新增（1611 行）：模块文档 + 纯逻辑 + 取数 SQL + handler + 11 个单测（5 纯逻辑 / 6 真库） |
| `backend/app/src/business/erp/mod.rs` | 注册 `pub mod fx_ledger;`（+ 与余额表同口径的模块注释） |
| `backend/app/migrations/business/V0413__fx_ledger_report.up.sql` | 权限码 + 菜单页面行 + 两角色授权 + 自检块 |
| `backend/app/migrations/business/V0413__fx_ledger_report.down.sql` | 回收授权与菜单行（带 down 自检） |

前端：

| 文件 | 内容 |
|------|------|
| `frontend/src/api/business/erp/fx-ledger.ts` | 接口层（类型逐字抄后端 + `fetchFxLedger`） |
| `frontend/src/views/business/erp/report/fx-ledger/index.vue` | 报表页（币种 / 区间 / 科目 / KPI / 按科目汇总 / 明细分页 / warnings / hints） |
| `frontend/src/views/business/erp/report/fx-ledger/fx-ledger-logic.ts` | 纯逻辑（金额格式化、借贷方向、区间文字、空表原因、口径说明） |
| `frontend/src/router/index.ts` | 手工注册路由（漏了 = 点菜单 404） |
| `frontend/tests/unit/erp-fx-ledger.test.ts` | 9 条纯逻辑单测 |

验收脚本：`scripts/verify/m108_fx_ledger_drill.sh`

## 三、实测结果

### 3.1 真引擎 + 真 HTTP（`bash scripts/verify/m108_fx_ledger_drill.sh 172.18.10.145`）

```
A) 克隆库升级（模板库停在 V0404）
   应用 9 个迁移（V0405..V0413）｜V0413 自检 NOTICE 打印｜schema_migrations 413/true
   菜单页面行 2/…0e02/51/1/外币明细账//business/erp/report/fx-ledger｜授权 2 角色
   重启：应用 0 个迁移｜versioned 326 → 335（+9）
B) 空库首启：345 个迁移，versioned 333 条，V0413 自检通过
C) 真 HTTP（100 项断言）：
   外币核算标记走 M1-03 接口（PUT /account/{id}/foreign-currency）→ 库里落成 4 个
   凭证全部走「建单(admin) → 审核(checker) → 记账」真实路径（内控开关保持开启）
   主用例（USD / 2063-10）：
     FXL01 期初 200 / 本期借 100 / 期末 300；本位币借 720；方向 DEBIT
     FXL02 期初 −200 / 本期贷 100 / 期末 −300；本位币贷 720（**不含**那笔纯本位币 50）
     合计：期初 0 / 借 100 / 贷 100 / 期末 0（平）/ 本位币净额 0
     明细 2 行：第 1 行原币借 100 / 本位币 720 / 汇率 7.2 / 逐行余额 **300**（含期初 200）
                第 2 行逐行余额 **−300**（从自己的 −200 起算，不串第 1 行）
     草稿 999 USD 不进表；干净夹具 warnings 为空；二次查询逐字段相同
   分页（全区间 4 行 / page_size=2）：第 1 页 200、300；第 2 页 −200、−300（前缀和接得上）
                                    越界页返回空 list（不报错）
   与外币余额表对拍：FXL01/FXL02 的期初、期末原币全部相等
   币种过滤 EUR ⇒ 2 科目且无别币种混入；科目过滤 FXL01 ⇒ 汇总与明细同步收窄、回显正确
   本位币 CNY ⇒ 400+10001 且报文指路「总账」；非法码 US / 缺 currency / 区间反了 / 非法日期 ⇒ 400
   不存在科目 ⇒ 404；非法 account_id ⇒ 400
   只有别的报表码的账号 ⇒ 403；同一账号访问资产负债表 ⇒ 200（证明 403 来自码）；无 token ⇒ 401
汇总：PASS=100 FAIL=0
```

### 3.2 真库单测（克隆库升级到 V0413 的 `cp_m108_t1`，`--test-threads=1`）

```
ERP_DB_URL=postgres://…/cp_m108_t1 cargo test --lib business::erp::fx_ledger
test result: ok. 11 passed; 0 failed
```

覆盖：明细行/汇总/KPI 三层数字、逐行余额按科目累计且含期初、本位币分录与草稿不进表、
翻页余额连续（前缀和）、与外币余额表逐科目对拍、未标记科目的外币分录报警（含过滤到该科目
时的解释）、币种/区间/科目/日期入参校验（400/404）。

### 3.3 静态检查

```
cd backend && cargo fmt && cargo clippy --lib -- -D warnings        → 0 warning
cargo test --lib system::menu::permission_audit_tests               → 8 passed（N14..N21）
bash scripts/verify/menu_invariants.sh cp_m108_t1                   → 8/8 全绿（菜单总数 506）
python3 scripts/verify/perm_menu_gate.py（NIK_DATABASE_* 指 cp_m108_t1）→ PASS（missing_in_menu=0）
```

### 3.4 前端

```
pnpm typecheck       → 通过
pnpm typecheck:unit  → 通过
pnpm test:unit       → 1899 条全通过（本卡新增 9 条：tests/unit/erp-fx-ledger.test.ts；本卡前基线 1890 条）
pnpm build           → 通过（vite build 无错）
```

## 四、踩到的一个坑（写下来给下一个动这批表的卡）

`biz_voucher.is_red` 是 **`SMALLINT`**，不是 `bool`。`row.get_typed::<bool>("is_red")`
在运行期报 `type mismatch: expected Bool, got I64` → 整个接口 500（第一次演练 56 项 FAIL 就是它）。
口径与 `ledger.rs` 一致：SQL 里取 `v.is_red`，Rust 侧读 `i16`，序列化前转 `bool`。
**编译期抓不到**（列类型不参与类型检查），只有真 HTTP 打一发才看得见 —— 这也是本卡坚持用
真引擎 + 真 HTTP 演练而不是只跑纯逻辑单测的原因。

## 五、未做的事（刻意，不夹带）

- **导出 xlsx**：卡面范围只有「按币种查明细」，导出不在本卡；将来做导出复用既有报表导出码
  `business:erp:report:export`，不新增码。
- **不建台账表 / 不落快照**：明细账是凭证的派生视图，落表 = 双真相（红冲后快照不改、
  库里无法跨表断言）。历史版本靠「已结账期间只读 + 凭证留痕」保证。
- **不改 `ledger.rs`**：总账的「一个科目 × 本位币流水」与本表的「一个币种 × 跨科目原币流水」
  是两种语义，合并会让两个报表互相污染 `WHERE`（详见模块文档末节）。
- **汇率折算不在本表**：本表只出原币与凭证汇率；期末折算与汇兑差额是外币余额表（M1-07）的职责。
