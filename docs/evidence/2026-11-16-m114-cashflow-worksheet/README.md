# M1-14 · 现金流底稿：录入 + 勾稽校验（直接法）· 证据

- **卡**：`t_9e14a327`（M1-14【M1·财务】现金流底稿：录入 + 勾稽校验（直接法））
- **上游**：`t_31c91cd5`（M1-13 建表 `biz_cashflow_worksheet`，迁移 V0414）
- **拆卡依据**：`docs/planning/20261113-新卡清单.md` §4 M1 财务 M1-14 行
- **日期**：2026-11-16
- **本卡迁移**：`backend/app/migrations/business/V0415__cashflow_worksheet_perm.{up,down}.sql`（只追加；V0414 未改动一个字节）
- **本卡代码**：`backend/app/src/business/erp/cashflow_worksheet.rs`（新文件）+ `shared/error_code.rs` 三个新码 + `erp/mod.rs` 一行登记

## 一、交付了什么

| # | 交付物 | 路径 |
|:--:|---|---|
| ① | 交互层：3 个接口 + 纯逻辑（勾稽 / 期间 / 金额 / 快照补齐）+ 12 条单测 | `backend/app/src/business/erp/cashflow_worksheet.rs`（1 800 行） |
| ② | 错误码 `66085` / `66086` / `66087`（三段各指一条修改路径） | `backend/crates/cloudpivot-kernel/src/shared/error_code.rs` |
| ③ | 迁移 V0415：两个按钮权限码 + 授权（**只追加**，幂等，down 带护栏） | `backend/app/migrations/business/V0415__cashflow_worksheet_perm.{up,down}.sql` |
| ④ | 迁移往返演练（21 断言） | `scripts/verify/m114_cashflow_worksheet_perm_drill.sh` |
| ⑤ | 真引擎实测（14 断言：升级 / 指纹 / 重启幂等 / down 护栏） | `scripts/verify/m114_engine_upgrade.sh` |
| ⑥ | 真 HTTP 实测（51 断言：鉴权 / 三接口 / 三道闸 / JSON 契约） | `scripts/verify/m114_worksheet_http_e2e.sh` |
| ⑦ | 证据目录（本 README + 7 份原始输出） | `docs/evidence/2026-11-16-m114-cashflow-worksheet/` |

**没有**前端改动（前端页面是 M1-18）、没有种子数据、没有新增表、没有改任何已应用迁移、没有碰凭证 / 科目 / 期间状态。

## 二、接口（三张，前缀 `/api` 由 `[router].prefix` 注入）

| 方法 | 路径 | 权限码 | 说明 |
|:--|:--|:--|:--|
| GET | `/business/erp/report/cash-flow/worksheet?period=2026-11` | `business:erp:cashflow-worksheet:list` | 读整张底稿（配置项 ∪ 已录入行）+ 三大活动小计 + 勾稽结果；`period` 缺省 = 当前自然月 |
| PUT | `/business/erp/report/cash-flow/worksheet` | `business:erp:cashflow-worksheet:save` | 录入：`opening` / `items[]` / `closing` 任选（全空 400），同一事务 upsert，返回保存后的整张底稿 |
| POST | `/business/erp/report/cash-flow/worksheet/check` | `business:erp:cashflow-worksheet:list` | 勾稽校验（**不落库**）：草稿叠加到已存底稿上算一遍；`require_balanced=true` 时不平即 `400 + 66087` |

### 入参 / 出参形状（前端 M1-18 直接照抄）

```jsonc
// PUT 请求
{ "period": "2026-11",
  "opening": { "amount": "500.0000", "remark": "上月结转" },
  "closing": { "amount": "1000.0000" },
  "items": [ { "item_code": "OPER_SALES_RECEIPTS", "amount": "1200.0000", "remark": "11 月回款" } ] }

// GET / PUT 响应（data 段）
{ "period": "2026-11", "period_status": "OPEN", "editable": true,
  "rows": [ { "line_type": "FLOW", "item_code": "OPER_SALES_RECEIPTS",
              "item_name": "销售商品、提供劳务收到的现金", "activity": "OPERATING",
              "direction": "INFLOW", "amount": "1200.0000", "sort_order": 101,
              "remark": "11 月回款", "entered": true, "item_active": true } ],
  "opening": { "line_type": "OPENING", "item_name": "期初现金及现金等价物余额", "amount": "500.0000", "remark": "上月结转" },
  "closing": { "line_type": "CLOSING", "item_name": "期末现金及现金等价物余额", "amount": "1000.0000", "remark": null },
  "activities": [ { "code": "OPERATING", "name": "一、经营活动产生的现金流量",
                    "inflow": "1200.0000", "outflow": "700.0000", "net": "500.0000", "entered_rows": 2 } ],
  "totals": { "inflow": "1200.0000", "outflow": "700.0000", "net": "500.0000" },
  "check": { "balanced": true, "opening": "500.0000", "inflow": "1200.0000", "outflow": "700.0000",
             "expected_closing": "1000.0000", "closing": "1000.0000", "diff": "0.0000",
             "message": "勾稽闭合：期初 500.0000 + 流入 1200.0000 − 流出 700.0000 = 期末 1000.0000",
             "problems": [] } }
```

> **金额一律是字符串**（`Decimal` 走 `serde` 的定点序列化，`"1200.0000"` 4 位小数）。
> 前端不要 `Number(x)` 后参与累加再比较 —— 那会重新引入二进制尾差、把「差 0.0001」抹平。

### `check.problems[].code` 四个机读码

| code | 含义 | 页面该做什么 |
|:--|:--|:--|
| `OPENING_MISSING` | 未录入期初余额 | 提示填期初（或去「期初余额」页确认上期已结转） |
| `CLOSING_MISSING` | 未录入期末余额 | 提示填期末 |
| `IMBALANCED` | 期初 + 流入 − 流出 ≠ 期末 | 原样显示报文（带 4 位小数的精确差额） |
| `EMPTY` | 一行明细都没录 | 提示先录明细行 |

## 三、关键决定（都写进了代码 / 迁移的注释里）

1. **保存允许不平，校验（出表）才要求闭合**。底稿是逐行编制的工作稿：先录流入、期末最后才对得上。
   保存就强制平，用户只能先编个假数把单存进去 —— 那才是真账务风险。所以 PUT 永远 200 + 把差额写清楚；
   `POST …/check` 只在 `require_balanced=true`（出表前复核）时 `400 + 66087`。
2. **三块快照由服务端从配置取**（`item_name` / `activity` / `direction` / `sort_order`），请求体只收
   `item_code` + `amount` + `remark`。若信客户端给的方向，改一下就能把流出改成流入、**自己把不平「调平」**。
3. **期间闸 66085**：只有 `OPEN` 可写。`NOT_OPENED` 时底稿与凭证无任何对应；`CLOSED` 时账已定稿，
   底稿还能改就会出现「报表说 A、底稿说 B」。读不受限（返回 `editable:false`，历史底稿照样能看）。
4. **项目闸 66086**：项目必须存在且启用。编码是弱引用（V0414 不建外键），项目不存在时该行没有方向、
   勾稽算不进流入 / 流出 —— 金额凭空消失而勾稽**反而是平的**，所以必须写入口拦。
   已停用项目的历史底稿行**仍然显示**（`item_active:false`），只是不能再写。
5. **金额精度 4 位小数入口硬校验**：`NUMERIC(18,4)` 会把 `1.23456` 静默四舍五入成 `1.2346`，
   造出「底稿合计 ≠ 凭证合计」的尾差且不报错 —— 入口先拒（`10001`），让人自己决定舍去哪里。
6. **upsert 幂等**：两条部分唯一索引就是冲突键（`(period_code,item_code) WHERE item_code IS NOT NULL`、
   `(period_code,line_type) WHERE line_type <> 'FLOW'`）。重复提交同一张底稿不攒第二行；HTTP 实测
   「重复 PUT 后库里仍是 4 行」。
7. **整批一个事务**：期初 / 明细 / 期末在同一事务，任一行失败整批回滚 —— 不留「期初写进去、明细没写」
   的半张底稿（半张底稿的差额会指向错误原因）。§约定 11 的「提交后不许留死单」在纯 DB 事务内天然满足
   （本卡 commit 之后没有任何可能失败的后续动作）。
8. **权限码分成读 / 写两个**：看底稿是看报表依据，改底稿是改报表依据（错一分钱报表就错一分钱）。
   校验接口**复用读码**（一个字节都不写库）。两个按钮挂在既有「现金流量表」页面 `…001018` 下 ——
   底稿不是独立报表，它的产出物就是那张表，种一个 `menu_type=2` 页面行会让侧边栏多一项指向 404 的入口。
9. **刻意不给 FINANCE_DIRECTOR 授权**：它没有现金流量表页面码，给了按钮就是**孤儿按钮**
   （`orphan_button_gate.py` 判红：不可达父页），界面上根本点不进去。要放开得先给报表页码 —— 另一张卡的事。

## 四、验收结果（原始输出见同目录 `.out` 文件）

| 层 | 命令 | 结果 |
|:--|:--|:--|
| fmt | `cargo fmt --check` | **0 diff**（`00_fmt_check.out`） |
| clippy | `cargo clippy -p cloudpivot --lib -- -D warnings` | **0 警告**（`01_clippy.out`） |
| 单测（含 4 条真库） | `ERP_DB_URL=…145/cp_m114_unit cargo test --lib business::erp::cashflow_worksheet -- --test-threads=1` | **12 passed / 0 failed**（`02_unit_tests.out`） |
| 迁移往返（真库） | `bash scripts/verify/m114_cashflow_worksheet_perm_drill.sh` | **PASS=21 FAIL=0**（`03_migration_drill.out`） |
| 真引擎升级 | `bash scripts/verify/m114_engine_upgrade.sh` | **PASS=14 FAIL=0**（`04_engine_upgrade.out`） |
| 真 HTTP 三接口 | `bash scripts/verify/m114_worksheet_http_e2e.sh` | **PASS=51 FAIL=0**（`05_http_e2e.out`） |
| 门禁 | migration_version_unique / role_code / perm_menu / menu_invariants / orphan_button | 见 `06_gates.out`（下面逐条解释） |

### 关键读数

- **真引擎**：克隆模板库（versioned 最大 404）→ 真启动 → **应用 11 个迁移（含 V0414 / V0415）**，
  日志里出现 `V0415 自检通过` NOTICE；`schema_migrations` 登记
  `415/true/app/migrations/business/V0415__cashflow_worksheet_perm.up.sql`；**重启应用 0 个迁移**（指纹幂等）。
- **真 HTTP**：`401`（无 token）→ 登录 → GET 12 行未录入 / `balanced:false` 且点名缺期初缺期末 →
  PUT 期初 500 + 流入 1200 + 流出 700 + 期末 1000 → **`balanced:true, diff:0.0000`** → 回读闭合、
  库里 4 行（2 明细 + 期初 + 期末）→ 重复 PUT 仍是 4 行 → POST check 不平 `400 + 66087`、
  闭合 `200` 且**没写库** → 已结账期间 `66085`、不存在项目 `66086`、负数 `10001`、空请求 `10001`，
  且被拒请求一行都没写。
- **门禁**：`migration_version_unique` ✓（682 文件 / 415 版本 / 最大 V0415，下一卡从 **V0416** 起）；
  `role_code_gate` ✓ 无新增未知角色码；`perm_menu_gate` **missing_in_menu = 0**（注解码 408 个，
  在跑满迁移的库 `cp_m114_full_drill` 上实测）；`menu_invariants` **8/8 全绿**（菜单总数 508 ∈ 400–520）。

### 两条**存量**红项（不是本卡造成，如实记录）

1. `orphan_button_gate`：`孤儿 (角色,码) = 3` —— `DEV_MONITOR_VIEWER` / `TEST_UADMIN` 各挂
   `business:notice:mine`、`TEST_UADMIN` 挂 `system:role-mutex:conflict`。**基线对照**：同一个闸对
   未应用本卡 V0415 的 `cloudpivot_dev` 也是这 3 条（`06_gates.out` 末段），且**不含本卡任何码**。
   本卡最初的 `FINANCE_DIRECTOR` 授权会造出第 4 条孤儿（已在本卡内改掉，见决定 ⑨）。
2. `scope_gate`（闸②）：`fx_cycle_tests.rs` / `report/fx_report_tests.rs` / `test_seed.rs`
   未登记 `SKIP_MANIFEST` —— 来自 M1-10 等前序卡，本卡新增文件**零命中**（脚本输出里 grep `cashflow` 无结果）。
   `scripts/check.sh` 头注释已写明闸②在施工期故意报错，不在 pre-push 里。

## 五、夹具与清场

- 单测用**死年份期间** `1913-01`（OPEN）/ `1913-02`（CLOSED）+ 临时分类配置 `M114_SCRATCH_ITEM`，
  每个用例开头 `reset()` 清场；演练 / 引擎 / HTTP 三个脚本都**自建库、自 DROP**（库名均含 drill/e2e）。
- 单测连的库：`cp_m114_unit`（模板库 + V0414 + V0415），跑完已 DROP。
- 模板库 `cloudpivot_test` 未被修改；`cloudpivot_dev` 只做了只读 SELECT（做基线对照）。
