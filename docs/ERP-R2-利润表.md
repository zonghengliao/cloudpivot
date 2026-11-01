# ERP-R2 利润表（交付说明 + 运维手册）

> 卡面：`t_3e6d3220【ERP-R2】利润表（本期+本年累计，勾稽到权益变动，10 单测）`
> 代码：`backend/src/business/erp/income_statement.rs`、
> `backend/migrations/business/V0184__erp_income_statement.{up,down}.sql`、
> `frontend/src/views/business/erp/report/income-statement/index.vue`、
> `scripts/verify/erpr2_income_statement.sh`
> 前置：ERP-R1（`biz_report_item` + 报表查询框架 + 56 个标准科目）

## 一、一句话

把「损益类科目的**发生额**」组装成利润表（本期 + 本年累计两列），并当场做**两表勾稽**：

```text
净利润 ≡ 营业收入 − 营业成本 − 税金及附加 − 销售费用 − 管理费用 − 财务费用
         + 营业外收入 − 营业外支出 − 所得税费用
净利润（本年累计） ≡ 资产负债表「未分配利润」本年变动
```

第二条不平时，`check.diff` 精确到分、`check.hints` 说出最可能的成因（未归类损益 / 本年利润直接结转 / 上年未结转 / 分录不平）。**绝不四舍五入抹平。**

## 二、数据模型（V0184）

### 1. 报表项（`biz_report_item`，12 行，`report_code = 'INCOME_STATEMENT'`）

| 行 | `item_code` | 名称 | 层级 | 取数 |
|----|-------------|------|------|------|
| 1 | `REVENUE` | 一、营业收入 | 1 | `5001,5051` |
| 2 | `COST_OF_SALES` | 减：营业成本 | 2 | `5401,5402,6401` |
| 3 | `TAX_SURCHARGE` | 减：税金及附加 | 2 | `5403` |
| 4 | `SELLING_EXPENSE` | 减：销售费用 | 2 | `5601` |
| 5 | `ADMIN_EXPENSE` | 减：管理费用 | 2 | `5602` |
| 6 | `FINANCE_EXPENSE` | 减：财务费用 | 2 | `5603` |
| 7 | `OPERATING_PROFIT` | 二、营业利润 | 1 | 公式：`REVENUE - COST_OF_SALES - TAX_SURCHARGE - SELLING_EXPENSE - ADMIN_EXPENSE - FINANCE_EXPENSE` |
| 8 | `NON_OPERATING_INCOME` | 加：营业外收入 | 2 | `5301` |
| 9 | `NON_OPERATING_EXPENSE` | 减：营业外支出 | 2 | `5711` |
| 10 | `TOTAL_PROFIT` | 三、利润总额 | 1 | 公式：`OPERATING_PROFIT + NON_OPERATING_INCOME - NON_OPERATING_EXPENSE` |
| 11 | `INCOME_TAX` | 减：所得税费用 | 2 | `5801` |
| 12 | `NET_PROFIT` | 四、净利润 | 1 | 公式：`TOTAL_PROFIT - INCOME_TAX` |

配置驱动：**改结构 / 加行不用发版**，只改 `biz_report_item`（本模块所有金额都从这张表 + 公式算出来）。

### 2. 三处必须知道的口径（改配置前先读）

**① 营业成本包含 `6401`。** `6401 主营业务成本` 是 V0149 时就有的存量科目，也是
`business/erp/voucher.rs::ACCOUNT_MAIN_COST` —— 序列号出库（借 6401 / 贷 1405）写死的成本科目。
只映射 5401/5402 会让「出库成本」进不了利润表：净利润与「未分配利润变动」当场对不上（**假不平，真漏数**）。

**② `side` 一律填 `'EQUITY'`。** V0183 给 `biz_report_item.side` 定了
`CHECK (side IN ('ASSET','LIABILITY','EQUITY'))`；损益类科目本来就是权益的一部分
（未结账时利润还留在 5xxx/6xxx，见 V0183 的 `UNDISTRIBUTED_PROFIT`）。
**不为一张报表放宽一张共用表的约束** —— 两张报表靠 `report_code` 隔开，`side` 的语义不变。

**③ `5111 投资收益` 刻意不映射。** 卡面 §一 的小企业结构没有这一行。硬塞进现有行会让数字含义失真；
不映射时若真的发生投资收益，报表会多出一行 `UNMAPPED_PROFIT_LOSS`（金额 = 对利润的影响）、
`check.hints` 点名科目编码、`check.diff` 精确报差额。**要显示它，加一个报表项即可**（配置驱动）。

## 三、取数口径（与资产负债表最大的差异：这里是**发生额**，不是余额）

```text
本期     = Σ(借 − 贷)   period_start ≤ voucher_date ≤ period_end
本年累计 = Σ(借 − 贷)   本年 1 月 1 日 ≤ voucher_date ≤ period_end

收入类（balance_direction = CREDIT，如 5001）：报表金额 = 贷 − 借 = −Σ(借−贷)
成本费用类（DEBIT，如 5401 / 5602）：       报表金额 = 借 − 贷 =  Σ(借−贷)
```

* 两种都是**正数表示该行正常方向的发生额**，加减关系交给 `formula`（`减：` 的行参与减法）；
* 损益类没有期初余额（年终结转后为 0），所以**只取发生额**，不取余额；
* 红字凭证是**借贷互换**（`is_red = 1`，金额恒为正），净额天然为负 → 红冲自动抵减，无特判
  （单测 R2-⑤ / 端到端 0004 号红字凭证：本期收入 5,000 − 500 = 4,500）；
* 只算 `status='POSTED'` 且未删除的凭证（与总账 / 科目余额表同一把尺子，草稿不进报表）。

**两段窗口怎么取**：复用 R1 的 `report::load_accounts` 查两次，不另写 SQL ——

| 窗口 | `net_before` | `net_period` |
|------|--------------|--------------|
| `[期间开始日, 期间结束日]` | 期间之前累计 | **本期发生额** |
| `[本年 1 月 1 日, 期间结束日]` | **年初之前累计** | **本年累计发生额** |

## 四、勾稽（本卡的灵魂）

### 1. 为什么第二条恒等式必然成立

复式记账下每张 POSTED 凭证 `Σ借 = Σ贷`，所以 `[年初, 期末]` 窗口内：

```text
Σ全部科目 (借−贷) = 0
⇒ 利润 = −Σ损益类 (借−贷) = Σ非损益类 (借−贷)
```

资产负债表的「未分配利润」= `3103 本年利润 + 3104 利润分配 + 全部损益类科目的利润贡献`
（V0183 的 `UNDISTRIBUTED_PROFIT` 报表项），它的本年变动 = 损益贡献变动 + 3103/3104 变动。
因此两条相等 **当且仅当**：①利润表把每个损益类科目都归了行；②没有手工往 3103/3104 直接记账。

### 2. 「未分配利润」口径只认一处配置

勾稽右式**不写死科目清单**：运行时从 `biz_report_item` 读
`report_code='BALANCE_SHEET' AND item_code='UNDISTRIBUTED_PROFIT'` 的 `account_pattern`
（取不到时回退 `@CATEGORY:PROFIT_LOSS,3103,3104`）。
改了资产负债表的映射，利润表的勾稽跟着变 —— 不会出现两张报表各算各的、都自称平。
单测 R2-⑧c 用**真库**钉住这一点：利润表 `undistributed_profit_end` 必须与资产负债表
`UNDISTRIBUTED_PROFIT` 逐分一致。

### 3. 不平的四类成因（`check.hints` 按这个清单给）

| 现象 | `check.hints` 会说什么 |
|------|------------------------|
| 某损益科目没被任何行匹配 | 「有 N 个损益类科目没有归入利润表任何一行：5111(50.00)…」+ 补映射的指引 |
| 3103 / 3104 本年有直接发生额 | 「本年利润 / 利润分配本年直接发生额 X：结转损益、利润分配、以前年度损益调整」 |
| 损益类年初有余额 | 「上年没做年终结转，差额里含这块上年利润」 |
| 分录借贷不等 / 单头不符 | 「Σ借 X ≠ Σ贷 Y」+ 点名具体凭证号 |

### 4. 未归类损益行（`UNMAPPED_PROFIT_LOSS`）

运行时补出来的一行（**不落库**），插在「四、净利润」之前，金额 = 未归类科目对利润的影响
（收入为正、费用为负）。它**不计入净利润**（净利润严格等于卡面的加减关系），
所以只要它不为 0，`check.balanced` 必定 false —— 报表漏了钱就不能显示「平」。

## 五、API

```http
GET /business/erp/report/income-statement?period=2026-10        # 权限 business:erp:report:income-statement
GET /business/erp/report/income-statement/export?period=2026-10 # 权限 business:erp:report:export（复用 R1 的导出码）
```

响应（节选，真机实测值见 §七）：

```json
{
  "code": 0,
  "data": {
    "report_code": "INCOME_STATEMENT",
    "report_name": "利润表",
    "period": "2026-10",
    "period_name": "2026年10月",
    "period_status": "OPEN",
    "ytd_start": "2026-01-01",
    "current_label": "本期金额",
    "ytd_label": "本年累计",
    "items": [
      {"item_code":"REVENUE","item_name":"一、营业收入","level":1,"current":"4500.0000","ytd":"7500.0000","is_total":false,
       "detail":"5001,5051","accounts":["5001","5051"]},
      {"item_code":"COST_OF_SALES","item_name":"减：营业成本","level":2,"current":"2000.0000","ytd":"2000.0000","is_total":false,
       "detail":"5401,5402,6401","accounts":["5401","5402","6401"]},
      {"item_code":"OPERATING_PROFIT","item_name":"二、营业利润","level":1,"current":"1700.0000","ytd":"4700.0000","is_total":true},
      {"item_code":"NET_PROFIT","item_name":"四、净利润","level":1,"current":"1400.0000","ytd":"4400.0000","is_total":true}
    ],
    "check": {
      "balanced": true,
      "net_profit_ytd": "4400.0000",
      "equity_change": "4400.0000",
      "diff": "0",
      "diff_text": "0.00",
      "message": "勾稽一致：净利润（本年累计）4400.0000 = 资产负债表「未分配利润」本年变动 4400.0000",
      "undistributed_profit_end": "4400.0000",
      "undistributed_profit_year_start": "0",
      "unmapped_profit_loss_ytd": "0",
      "unmapped": [],
      "ledger_debit_total": "…", "ledger_credit_total": "…",
      "unbalanced_vouchers": [],
      "hints": []
    }
  }
}
```

错误码：

| HTTP | code | 场景 |
|------|------|------|
| 400 | `10001` | `period` 缺失 / 不是 `yyyy-MM`（如 `2026-13`） |
| 400 | `66010` | 期间没开账**且**该月没有任何已记账凭证（报表不替空月份编造报告期） |
| 500 | `66011` | `biz_report_item` 配置坏了（公式引用了不存在的项） |

期间口径与资产负债表**完全一致**（复用 `report::resolve_period`）：有期间行就用它（`OPEN` / `CLOSED` 都出表）；
库里没期间行但该月有账 → 按自然月推算、状态 `DERIVED`、`hints` 里提示尽快开账。

## 六、前端

`frontend/src/views/business/erp/report/income-statement/index.vue`

* 两列：**本期金额 / 本年累计**（列名由后端给，前端不写死）；
* 层级缩进 + 「减：/ 加：」前缀（前缀已在 `item_name` 里，前端**不重复拼**）；
* 合计行加粗 + 底色（营业利润 / 利润总额 / 净利润）；
* 底部勾稽条：绿框「勾稽一致」/ 红框「不平 + 差额」，并列出未分配利润期末 / 年初、未归类科目、不平凭证；
* 导出 Excel（同一份报表，`scripts/verify/erpr2_income_statement.sh` 断言 PK 魔数与中文文件名）。

页面**不做任何金额计算**：`numeric(20,4) → rust_decimal → JSON 字符串`，前端只做十进制字符串的千分位与四舍五入到分
（`Number()` 的二进制浮点误差会让报表少一分钱）。

路由手工注册在 `frontend/src/router/index.ts`（`BusinessErpIncomeStatement`，`sys_menu` 种子 = V0184 `…1017`）。

## 七、验收

### 1. 单测（`cargo test --lib business::erp::income_statement`，19 条）

| # | 用例 | 钉住什么 |
|---|------|----------|
| ① | 只有一笔收入 | 营业收入 = 净利润 = X |
| ② | 收入 + 成本 | 营业利润 = 收入 − 成本；**6401 也算成本** |
| ③ | 期间费用 | 营业利润再减管理费用 / 财务费用 |
| ④ | 所得税 / 营业外收支 | 净利润 = 利润总额 − 所得税 |
| ⑤ | 红冲收入 | 收入减少；冲成负数如实显示（不夹 0） |
| ⑥ | 本期 / 累计两段窗口 | 本期 200 / 累计 500（纯逻辑） |
| ⑥b | 真库跨月（9 月 + 10 月） | 累计含上月、本期只含本月 |
| ⑦ | 空期间 | 全 0（含未归类行） |
| ⑦b | 真库空期间 2000-12 | 全 0 且勾稽平 |
| ⑧ | 勾稽平 | 净利润 300 = 权益变动 300 |
| ⑧b | 未归类 5111 | 不平 −50.00 且点名科目 |
| ⑧c | 真库：收入+成本+费用+税 | 净利润 218 = 权益变动 218 = **资产负债表未分配利润变动 218** |
| ⑨ | period 校验 | 必填 / `yyyy-MM` / 累计起点 1 月 1 日 |
| ⑨b | 真库：期间不存在 | 400 + 66010（消息含期间） |
| ⑩ | 人为改数 | 不平 −400.00 且指出 3103/3104 成因 |
| ⑪ | 0.0001 差额 | 不被抹平（保留 4 位） |
| ⑫ | 公式引用未知项 | 显式报错，不静默 0 |
| ⑬ | 真库配置一致性 | 12 项 / 公式闭合 / 标准损益科目各匹配一行 |
| ⑭ | 真库生产期间 | 有账就出表、勾稽平、导出表头正确 |

### 2. 端到端（`bash scripts/verify/erpr2_income_statement.sh`，隔离库 + 真 HTTP）

夹具（`ERPR2V`，4 期间 / 7 凭证 / 14 分录，含一张**真红字凭证** `is_red=1` + `reverse_of`）：

```text
2026-09  资本 10,000；收入 3,000
2026-10  收入 5,000；红字冲销 500；成本 2,000；管理费用 800；所得税 300
⇒ 本期：收入 4,500 / 营业利润 1,700 / 净利润 1,400
⇒ 累计：收入 7,500 / 营业利润 4,700 / 净利润 4,400
⇒ 资产负债表：未分配利润 4,400、资产总计 14,700、负债+权益 14,700
```

覆盖：两列金额逐行对账、勾稽 balanced/diff、**两表互证**（同一份数据打资产负债表）、
空期间全 0、无发生额月份「本期 0 / 累计不变」、非法期间 400+10001、空月份 400+66010、
无令牌 401、未归类损益 → 红 + 点名 5111 + diff −250、删掉后回绿、导出 xlsx（PK + 中文文件名）。

### 3. 真机（生产库 172.18.10.144）

见 §八。生产 2026-10 只有采购/应付类凭证（无损益发生额），因此净利润 0.00、未分配利润变动 0.00 ——
**勾稽仍是「一致」**（0 = 0，不是"算不出"）。有损益数据的场景由 §七.2 的隔离库脚本证明。

## 八、回滚

```bash
psql -f backend/migrations/business/V0184__erp_income_statement.down.sql
```

只删本迁移种下的 12 个报表项 + 权限节点 `…1017` 及其授权；资产负债表配置（V0183）、
科目、凭证 / 分录 / 期间**一行不动**（利润表是只读报表，本迁移也不写业务表）。

## 九、给下一次改这张报表的人

1. **加一行**：往 `biz_report_item` 插一行（`report_code='INCOME_STATEMENT'`、`side='EQUITY'`、
   `sort_order` 落在合适位置、`formula` 只能引用**排在前面**的编码）。不用改代码、不用发版。
2. **不要**在代码里写死科目清单：科目匹配、公式求值、未分配利润口径都从 `biz_report_item` 读。
3. 改完跑 `cargo test --lib business::erp::income_statement`（⑬ 会抓「同一科目被两行匹配」「公式引用不存在」）
   和 `bash scripts/verify/erpr2_income_statement.sh`。
4. `check.diff` 的比较是**精确比较**：不要为了"好看"加容差，0.0001 的不平也是不平（单测 R2-⑪ 钉住）。
