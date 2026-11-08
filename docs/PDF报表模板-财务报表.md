# 报表 PDF 模板：资产负债表 / 利润表 / 现金流量表（PDF-07）

> 卡：`PDF-07`。单据侧的六批见 [PDF单据模板-采购销售订单.md](./PDF单据模板-采购销售订单.md)（PDF-02）、
> [PDF单据模板-合同与出入库单.md](./PDF单据模板-合同与出入库单.md)（PDF-03）、
> [PDF单据模板-调拨盘点退货.md](./PDF单据模板-调拨盘点退货.md)（PDF-04）、
> [PDF单据模板-审批单.md](./PDF单据模板-审批单.md)（PDF-05）、
> [PDF单据模板-派工单.md](./PDF单据模板-派工单.md)（PDF-06）；
> 底座（Chromium CDP / 字体 / `business/pdf` 分层）见 [PDF导出基础设施.md](./PDF导出基础设施.md)。
>
> 本文件讲**财务报表**：数据不在 `biz_erp_*` 单据表，而在**凭证 + 报表项配置**
> （`biz_report_item` / `biz_cashflow_item`）；版式是「财务报表」而不是「单据」——
> 没有单号、没有明细行合计，只有**科目余额/发生额的纸面排布**与**勾稽校验**。
> 三张报表的取数一行都没有重写，全部复用 R1/R2/R3 的既有实现。

## 一、接口与权限

| 方法 | 路径（`/api` 前缀由 `[router].prefix` 注入） | 权限码 | 版式 | 说明 |
|------|---------------------------------------------|--------|------|------|
| GET | `/business/erp/report/{type}/pdf?period=yyyy-MM` | `business:erp:report:export` | A4 纵向 | `type` = `balance-sheet` / `income-statement` / `cash-flow` |

- **权限复用 xlsx 导出的动作码**（`business:erp:report:export`，V0183 种子 `…1016`）。
  PDF 与 Excel 是**同一份数据的两种纸面**，不是两种能力：另立 `:pdf` 码只会造出
  「能导 Excel、点 PDF 就 403」的哑弹，还要同步迁移 + 角色矩阵 + 前端按钮显隐三处。
  - 验证过了：只挂 `business:erp:report:export` 的普通角色用户 **200**；
    只有 `business:erp:report:balance-sheet`（能看不能导）的用户 **403**。
- `type` 大小写不敏感、下划线写法兼容（`balance_sheet`），**不认识就 400**（绝不猜一个最像的）。
- 响应是二进制（不走 `{code,msg,data}`）：

```
200 application/pdf
Content-Disposition: attachment; filename="资产负债表-BALANCE_SHEET-2026-11.pdf"; filename*=UTF-8''…
Cache-Control: no-store
```

- 失败分支：
  | 场景 | 状态 | 业务码 |
  |------|------|--------|
  | 未登录 | 401 | 10002 |
  | 缺 `business:erp:report:export` | 403 | 10003（`PERMISSION_DENIED`） |
  | `type` 不认识 | 400 | 10001（`msg` 里列出三个合法值） |
  | `period` 缺失 / 不是 `yyyy-MM` | 400 | 10001 |
  | 该期间没开账也没凭证 | 400 | **66010**（与页面同码，不是 500） |
  | 期初不平衡 | 400 | 66012 |
  | `biz_report_item` 配置坏（公式引用不存在的项） | 500 | 66011 |
  | 模板 / 引擎（Chromium 不可用 / 超时） | 422 / 503 / 504 | 67001–67005 |

  66010 / 66011 / 66012 **直接来自既有报表模块的 `snapshot_or_error`**，不另造错误码 ——
  页面报什么、PDF 就报什么。

## 二、代码落点

```
backend/src/business/
  erp/report_pdf.rs                     取数编排 + 纯口径（金额格式化 / 两栏对齐 / 行语义）+ HTTP 入口 + 导出审计 + 10 用例
  pdf/templates/report_balance.html.tera  资产负债表版式（左右两栏 × 期末/期初）
  pdf/templates/report_amount.html.tera   利润表 / 现金流量表版式（项目 / 本期金额 / 本年累计）
  pdf/template.rs                       ReportHead / ReportCell / BalanceReportContext / AmountReportContext
                                        + render_balance_report / render_amount_report / report_pdf_opts + 3 用例
frontend/src/
  api/business/erp/index.ts             exportReportPdf(type, period)（blob）
  views/business/erp/report/{balance-sheet,income-statement,cash-flow}/index.vue   「导出 PDF」按钮
```

`erp/report.rs` / `erp/income_statement.rs` / `erp/cash_flow.rs` **只放开三个函数的可见性**
（`validate_period` / `snapshot_or_error` / `company_name` → `pub(crate)`），一行逻辑都没改：
报表 PDF 与页面必须走**同一个快照事务、同一套错误码、同一份编制单位取数**。

## 三、三张报表怎么排

### 3.1 资产负债表（左右两栏）

```
资产          期末余额   期初余额 │ 负债及所有者权益   期末余额   期初余额
流动资产          -         -   │ 流动负债              -         -
货币资金    1,000.00       -   │ 短期借款              -         -
…                              │ …
资产总计    1,500.00       -   │ 负债和所有者权益总计 1,500.00     -
```

- **期初 = 上期期末**（`R1.previous_amount`），不是另查一遍期初余额：这是会计上的等价定义，
  也是 R1 `attach_previous` 已经算好的东西；两处各算一遍，口径迟早漂开。
- **无上期期间时印 `-` 并在附注里明说**（「未取到上期期末余额，不是 0；需要对比请先开账结账上月」）。
- **两个总计必须落在同一行**（[`align_sides`]）：资产侧 20 行、负债权益侧 16 行，
  简单按下标 zip 会让「资产总计」在第 20 行、「负债和所有者权益总计」在第 16 行 ——
  纸面上两个总计一高一低，看起来像「右边少算了 5 行」。所以短的一栏在**自己的总计行之前**
  补透明空行，把两侧总计顶到同一行。
- 六列宽度是按「最宽数字」倒推的：8.5pt 等宽数字下 21mm 放得下 `123,456,789.00`；
  再宽就得牺牲项目名列，中文项目名（「负债和所有者权益总计」9 字）会折行。
  因此左右边距收到 8mm（模板 `@page` 与 `report_pdf_opts` 同值）。

### 3.2 利润表 / 现金流量表（共用一份模板）

两张表的纸面结构是同一张三列表（项目 / 本期金额 / 本年累计），差别只在**行语义**：
`section`（一、经营活动…）/ `line`（明细，按 level 缩进）/ `subtotal`（流入小计）/
`total`（净利润 / 期末余额，加粗底纹）/ `note`（告警，独占一行不印金额）。

现金流量表按经营 / 投资 / 筹资三节，每节「流入明细 → 流入小计 → 流出明细 → 流出小计 → 本节净额」，
末尾「净增加额 → 加：期初 → 期末余额」，再跟勾稽块与简化口径说明。
**「其他（未分类）」只有真有明细或净额非零时才印** —— 全零印一屏 `-` 只会让人以为漏了数据；
真要提示，`warnings` 会进附注。

## 四、金额：两条硬规则

| 规则 | 实现 | 为什么 |
|------|------|--------|
| 千分位 + 两位小数 | `format!("{:.2}", v.round_dp(2))` + 三位分组 | 直接 `{:.2}` 格式化 `Decimal` 是**截断**（12.3456 → 12.34），打印件少一分钱是事故 |
| **空白项与零一律印 `-`** | `money_or_dash`：`None` 或 `round_dp(2)` 后为零 → `-` | 中国会计报表纸面惯例；一行几十个 0.00 会把真正的金额淹掉。校验块里的「差额 0.00」例外，零照印（见下） |

`money_plain` 专门给勾稽块用：**零也照印 `0.00`** —— 差额到底是不是 0，必须一眼看见。

## 五、勾稽校验块（三张报表各有）

| 报表 | 校验内容 | 一致性时 |
|------|----------|----------|
| 资产负债表 | 资产总计 ≡ 负债合计 + 所有者权益合计 | 差额 0.00 —— 借贷平衡（资产 ≡ 负债 + 所有者权益） |
| 利润表 | 净利润（本年累计） ≡ 资产负债表「未分配利润」本年变动 | 差额 0.00 —— 两表勾稽一致 |
| 现金流量表 | 期末现金及现金等价物余额 ≡ 资产负债表「货币资金」 | 差额 0.00 —— 与资产负债表货币资金一致 |

不平的时候：块体变红（`.check.is-bad`），逐条打印两侧金额与**精确差额**，并在附注里带
R1/R2/R3 给的成因提示（未归类科目 / 分录不平 / 未分配利润直接记账…）。
**打印件不能瞒**：不平的报表也照印，只是看不到「借贷平衡」四个字。

## 六、导出审计

导出是 GET，`middleware::operation_log` 只记写方法，所以渲染成功后显式补一行：

```
operation   = EXPORT_REPORT_PDF
target_type = biz_report
detail      = {"report_type":"balance-sheet","report_code":"BALANCE_SHEET",
               "source":"PDF-07","row_count":28,"total_amount":"1500.0000","render_ms":412}
doc_no      = BALANCE_SHEET-2026-11
```

报表不是单据、没有 `biz_*` 主键，所以 `target_id` 存**本次导出事件的 v7 id**，
业务定位靠 `detail.report_type + doc_no`。写日志失败只记 error，不改已生成的响应。

## 七、踩过的坑（本卡新增）

1. **`pdftotext -layout` 把左右两栏印在同一行**：按行找金额会把左栏「货币资金 1,000.00」
   算到右栏「短期借款」头上（首版 E2E 就是这么假红的）。必须**按表头各列起始位置切片**逐列判。
2. **`1,000.00` 里含 `0.00` 子串**：用 `"0.00" in text` 判「有没有零」会大面积假阳；
   要判「单元格金额恰为 0.00」。
3. **行名互相包含**：利润表的「未归类损益（未计入净利润，映射缺失）」本身含「净利润」三个字，
   按名字 `find` 会抓到错行（`is_total=false`）。`AmountReportRow` 因此带 `code`
   （`NET_PROFIT` / …），模板不打印、单测与审计靠它定位。
4. **`biz_report_item.is_active` 是 `smallint`（0/1）而不是 `boolean`**，且该表没有 `deleted_at`；
   写 `is_active = TRUE` 直接 `db error`。
5. **`CASH_FLOW` 不在 `biz_report_item` 里**（它在 `biz_cashflow_item`）：报表项自检按报表分开查。
6. **角色授权表是 `sys_role_menu(role_id, menu_id)`**，不是 `sys_role_permission`；
   写错表 → 用户没有任何权限码 → 明明该 200 的用例拿 403（首版 E2E 假红）。
7. **换二进制要先停实例**：运行中的进程占着可执行文件，`scp` 会以 `Failure` 失败（ETXTBSY）。
   停 → 拷 → 启，并核对 `md5sum` 一致再看 `health=ok`。
8. **别对共享工作区的文件整体跑 rustfmt**：`erp/report.rs` 里有别人未提交的改动，
   整体格式化会把无关 hunk 卷进自己的提交；只对**自己的新文件**跑格式化，
   对既有文件只改需要的那几行。

## 八、实测（145 隔离环境）

见 [docs/evidence/20261108-pdf07/README.md](./evidence/20261108-pdf07/README.md)：
154 项断言 / 0 失败，三份 PDF 样本在 `sample/`，文本层与 `pdfinfo`/`pdffonts` 在 `raw/`。
