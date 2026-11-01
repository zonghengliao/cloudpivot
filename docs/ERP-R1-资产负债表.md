# ERP-R1 资产负债表（交付说明 + 运维手册）

> 卡面：`t_17c44185【ERP-R1】资产负债表（补 40 个标准科目 + 报表项映射 + 勾稽校验，12 单测）`
> 代码：`backend/src/business/erp/report.rs`、`backend/migrations/business/V0183__erp_balance_sheet.{up,down}.sql`、
> `frontend/src/views/business/erp/report/balance-sheet/index.vue`、`scripts/verify/erpr1_balance_sheet.sh`

## 一、一句话

把「科目余额」组装成**对外能交的报表**，并当场做勾稽：`资产总计 ≡ 负债合计 + 所有者权益合计`，**差一分钱都报出来**。

## 二、数据模型（V0183）

### 1. 科目补齐（`biz_account`，7 → 56 个）

| 类别 | 新增科目（编码） | 备注 |
|------|------------------|------|
| 资产（24） | 1001 1012 1121 1122 1123 1131 1132 1221 1401 1402 1403 1404 1408 1601 **1602** 1604 1605 1606 1701 **1702** 1801（1405/1901 既有） | 1602 累计折旧 / 1702 累计摊销是**备抵科目**：`ASSET` 类别 + `CREDIT` 方向 |
| 负债（12） | 2101 2203 2211 2221.02 2231 2241 2401 2501 2701（2202/2221/2221.01 既有） | 2221.02 挂在 2221 下（level 2） |
| 权益（5） | 3001 3101 3102 3103 3104 | |
| 成本（2） | 4001 4101 | 报表里并入「存货」（在产品的会计口径） |
| 损益（13） | 5001 5051 5111 5301 5401 5402 5403 5601 5602 5603 5711 5801（6401 既有） | |

**幂等**：全部 `ON CONFLICT (account_code) DO NOTHING` —— 撞 `uk_biz_account_code` 直接跳过，
已存在科目的**名称 / 类别 / 方向 / 层级 / 父级一个都不覆盖**（硬红线 2）。

### 2. 报表项（`biz_report_item`，41 行）

一个报表项 = 一段取数规则 + 一个显示位置，**配置在库里，不写死在代码里**：

| 列 | 语义 |
|----|------|
| `report_code` | 报表编码，目前只有 `BALANCE_SHEET` |
| `item_code` / `item_name` / `item_level` / `parent_code` / `sort_order` | 结构与排版（level 1 大类 / 2 小类 / 3 明细） |
| `side` | `ASSET` / `LIABILITY` / `EQUITY`（`VARCHAR(12)`；卡面写 8 会**溢出**，`LIABILITY` 是 9 个字符） |
| `account_pattern` | 科目匹配模式（见下） |
| `formula` | `+` / `-` 引用其它 `item_code`（只能引用 `sort_order` 靠前的项） |
| `is_deduction` | 减项标记（累计折旧 / 累计摊销） |

**`account_pattern` 语法**（`report::match_pattern`）：

```text
1001,1002            编码清单
2221                 编码本身 + 其全部下级（2221 / 2221.01 / 2221.02）
1*      或  1%        编码前缀
@CATEGORY:PROFIT_LOSS 按科目类别整类匹配（新增损益科目自动进「未分配利润」）
```

## 三、取数口径

```text
opening_net = Σ(借 − 贷)   voucher_date <  period_start     （历史累计，含往年）
period_net  = Σ(借 − 贷)   period_start ≤ voucher_date ≤ period_end
net         = opening_net + period_net                      （借为正）

资产/负债/权益类：金额 = (方向 = DEBIT) ? net : −net        （按正常余额方向取正）
损益类          ：金额 = −net                               （利润对权益的贡献：收入 +，费用 −）
```

* 只算 `status='POSTED'` 且未删除的凭证（与总账 / 科目余额表同一把尺子，草稿不进报表）；
* 红字凭证是**借贷互换**（金额恒为正、净额天然为负），直接累加，无特判；
* 备抵科目取出来是**正数**（贷方余额），由报表项的 `is_deduction` 在小计里减掉。

## 四、勾稽（本卡的灵魂）

### 1. 为什么这个等式必然成立

复式记账下每张 POSTED 凭证 `Σ借 = Σ贷` ⇒ 任意「整张凭证」窗口内 `Σ全部科目(借−贷) = 0`。
把科目按类别归侧（`ASSET+COST` / `LIABILITY` / `EQUITY+PROFIT_LOSS`）后：

```text
资产侧 − 负债侧 − 权益侧 = Σ全部科目(借−贷) = 0
```

所以 `diff ≠ 0` **只可能**是：① 某张凭证的分录被删/改（单头合计不会跟着变，库里的 CHECK 拦不住）；
② 科目 `account_category` 配错；③ 分录指向了已软删科目。

### 2. 未归类差额（`UNMAPPED_*`）

每侧残差 = 该侧类别净额 − 该侧已映射报表项合计，作为 `UNMAPPED_ASSETS / UNMAPPED_LIABILITIES /
UNMAPPED_EQUITY` 三行**显式出现在报表上**（正常恒为 0.00），同时进 `check.unmapped` 与 `check.hints`。
好处：`TOTAL_ASSETS` 恒等于资产侧类别净额 ⇒ `diff` 只剩「借贷不等」一个成因，报表不平 = 真账不平。

### 3. 出参 `check`

| 字段 | 说明 |
|------|------|
| `balanced` | `diff == 0`（**精确比较，不按 2 位小数**） |
| `assets_total` / `liability_equity_total` | 两式总额 |
| `diff` / `diff_text` | 差额与「人话」；`|diff| < 0.005` 时按 4 位小数显示，绝不被舍成 `0.00` |
| `message` | 「平衡：资产 X = 负债和所有者权益 Y」/「不平：… 差 Z 元」 |
| `ledger_debit_total` / `ledger_credit_total` | 窗口内分录借贷合计（诊断） |
| `unbalanced_vouchers` | 分录合计 ≠ 单头的凭证（最多 20 张，点名+四个金额） |
| `hints` | 成因清单（借贷不等 / 未归类科目 / 科目类别配错 / 期间未开账） |

## 五、API

| 方法 | 路径 | 权限码 | 说明 |
|------|------|--------|------|
| GET | `/business/erp/report/balance-sheet?period=2026-10` | `business:erp:report:balance-sheet` | 编制报表（含勾稽、上期对比、编制时间） |
| GET | `/business/erp/report/balance-sheet/export?period=2026-10` | `business:erp:report:export` | 导出 xlsx（复用 `impexp::engine::write_sheet`） |

**隔离级别**：整个报表在**一个 REPEATABLE READ 事务**里算完（科目余额 / 报表项 / 上期 / 借贷合计 /
异常凭证 5 条 SQL 同一快照）。`SET TRANSACTION ISOLATION LEVEL REPEATABLE READ` 必须是 `BEGIN`
后的第一条语句（PG 规定，否则 25001），见 `report::snapshot_or_error`。

**错误码**：

| 场景 | HTTP | code |
|------|------|------|
| `period` 缺失 / 不是 `yyyy-MM` | 400 | `10001` |
| 期间没建且该月没有任何凭证 | 400 | `66010` |
| `biz_report_item` 配坏（公式引用不存在的项 / 一项都没配） | 500 | `66011` |

### 期间口径（**与卡面 ⑧ 的差异，必读**）

```text
有 biz_accounting_period 行                    → 用它（OPEN / CLOSED 都能出表）
无期间行 + 该月有已记账凭证                     → 按自然月推算，period_status = "DERIVED"，check.hints 要求尽快开账
无期间行 + 该月一张凭证都没有                    → 400 + 66010（不替空月份编造报告期）
```

生产库实测（2026-11-01）：`biz_voucher` 里有 2 张 **2026-11** 的凭证，而 `biz_accounting_period`
只有 2026-10。严格 400 会让「有账的月份出不了表」，用户会以为账丢了 —— 这比「没开账就出表」危害大。
反过来给**空月份**编造报告期才是真正的静默错误，所以空月份仍然 400（卡面 ⑧ 的场景完整保留）。

## 六、前端

`frontend/src/views/business/erp/report/balance-sheet/index.vue`：期间下拉（只列已建立期间）→ 查询 →
左资产 / 右负债和权益两栏（含上期余额列）→ 平衡绿色横幅 / 不平红色告警（差额、凭证清单、成因）→ 导出 Excel。
金额字符串来自后端定点数，前端只做千分位显示，**不做任何金额计算**。

## 七、验收（真机证据）

```bash
# ① 单测（15 条：4 匹配/取数 + 3 勾稽 + 3 边界 + 2 对比 + 3 配置/公式）
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/<db> cargo test --lib business::erp::report
# ② 端到端（70 项断言：真 HTTP + 真库 + 删分录制造不平）
bash scripts/verify/erpr1_balance_sheet.sh
# ③ 不变量套件不退化
NIK_DATABASE_NAME=<db> python3 scripts/verify/erp_invariants.py    # 31/32, BLOCKER 0
```

生产数据副本（11 张凭证）实测：

| 期间 | 状态 | 资产总计 | 负债+权益 | diff | 说明 |
|------|------|----------|-----------|------|------|
| 2026-10 | OPEN | 435.0000 | 435.0000 | 0.00 | 平衡；Σ借 = Σ贷 = 692.17 |
| 2026-11 | DERIVED | 835.0000 | 835.0000 | 0.00 | 有账无期间 → 推算 + 提示开账；带 2026-10 上期对比 |
| 2026-12 | — | — | — | — | 400 + 66010（空月份，不编造） |

> 卡面示例里的「资产总计 1144.17」其实是**本期借贷发生额**（11 张凭证 Σ借=Σ贷=1144.17），
> 不是资产余额；以库里真实余额为准（435.00），勾稽成立的判据是「两侧相等」，不是某个固定数字。

## 八、回滚

```bash
psql -d <db> -f backend/migrations/business/V0183__erp_balance_sheet.down.sql
```

删报表项表 + 权限码 + **本迁移新种的 49 个科目**（仅当既无下级、也无任何分录引用；有引用一律保留，
保留行优先于删账）。既有 7 个科目一行不动。

## 九、给下一次改这张报表的人

1. **加报表项不用改代码**：往 `biz_report_item` 插一行（`sort_order` 决定显示顺序与公式求值顺序），
   公式只能引用 `sort_order` 更小的项；引用不存在的 item_code → 接口 500 + `66011`（故意炸，不静默出 0）。
2. **一个科目只能被一个报表项匹配**：匹配到两个 = 同一笔钱算两遍。单测 `r1_real_config_is_consistent`
   会把这种配置立刻按红（`同一科目被多个报表项匹配`）。
3. **新增科目忘了映射**不会让报表变平：它会落进 `UNMAPPED_*` 三行并在 `check.hints` 里点名，
   报表仍然平衡但一眼能看出没归类 —— 这是"宁可显式难看，不许静默错账"的取舍。
4. **别用 psql 手工预演 V0183 再让迁移器跑**（实测：手工跑过之后再启动实例，迁移器报
   `数据库迁移失败: query failed: : db error` 且不给 SQL 细节）。要么全程让迁移器跑，
   要么手工跑完自己往 `schema_migrations` 补记录。
