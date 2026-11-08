# PDF-07 财务报表 PDF · 145 实测证据

> 卡：`PDF-07`（`GET /api/business/erp/report/{type}/pdf?period=yyyy-MM`）
> 接口与版式说明：[../../PDF报表模板-财务报表.md](../../PDF报表模板-财务报表.md)

## 1. 环境

| 项 | 值 |
|----|----|
| 主机 | `172.18.10.145`（测试环境，**不是 144 生产**） |
| 实例 | `127.0.0.1:8082`，代码/库目录 `/root/pdf07-app`，env `/root/pdf07.env` |
| 数据库 | `cloudpivot_pdf07`（**空库首启**：本卡未新增未修改任何迁移，日志「数据库迁移完成: 应用 283 个迁移」） |
| Redis | `127.0.0.1:6379/12` |
| 渲染引擎 | `chromium-pdf.service`（CDP `127.0.0.1:9222`），与 145 上其它实例共用 |
| 二进制 | `md5 a588d7478a5ed4f52e8a454d7ee5fe43`（**与提交的代码同一次构建**；下面 154 项断言就是用它跑的） |
| 起停脚本 | `/root/pdf07_instance.sh`（pgrep 串 `target/debug/cloudpivot$` 锚定 + `/proc/<pid>/cwd` 二次校验，避免误杀同机其它实例） |

## 2. 夹具（决定「借贷平衡」的具体数字）

```text
期间：2026-11（OPEN）  ／ 2026-10（CLOSED，无凭证 → 期初整列必然是「-」）

凭证 1  2026-11-05  借 1001 库存现金 1,000.00 / 贷 3001 实收资本 1,000.00
凭证 2  2026-11-06  借 1122 应收账款   500.00 / 贷 5001 主营业务收入 500.00

⇒ 资产总计 = 1,500.00   负债和所有者权益总计 = 1,500.00（差额 0.00）
⇒ 利润表 营业收入/净利润 = 500.00（本期 = 本年累计）
⇒ 现金流量表 筹资活动 吸收投资 1,000.00 → 期末现金 1,000.00 ≡ 资产负债表货币资金 1,000.00
```

夹具由 [`e2e145.py`](./e2e145.py) 里的 `FIXTURE_SQL` 建（幂等：先按 `voucher_word='PDF07'` 删再插，
期间行 `ON CONFLICT DO NOTHING`），并**自检**「2 张 POSTED 已入库」才算通过。

## 3. 结果：**154 项断言 / 0 失败**

```
$ cd /root/pdf07-evidence && BASE=http://127.0.0.1:8082 DB=cloudpivot_pdf07 \
    ENVF=/root/pdf07.env EVID=/root/pdf07-evidence python3 e2e145.py
...
PASS=154 FAIL=0
```

全量输出：[`raw_e2e.txt`](./raw_e2e.txt)。

### 卡面四条验收

| 验收 | 断言落点 | 结果 |
|------|----------|------|
| ① 三张报表各导一份 PDF | 三个 `type` 各 `200 application/pdf` + `%PDF-` + `Content-Length` 自洽 + `attachment; filename*=UTF-8''…` + `no-store` + `Pages>=1`；样本落在 `sample/` | PASS |
| ② 借贷平衡（资产负债表左右相等） | 按表头列位切片读「资产总计」= `1,500.00`、`负债和所有者权益总计` = `1,500.00`，**两行在同一行**；勾稽块「差额 0.00 —— 借贷平衡」 | PASS |
| ③ 金额格式（千分位、两位小数） | 文本层每个金额 token 匹配 `^-?\d{1,3}(,\d{3})*\.\d{2}$`；≥1000 的金额都带逗号（`1,000.00` / `1,500.00`） | PASS |
| ④ 空白项显示「-」 | 逐行逐列切片：`应收票据/预付账款/其他应收款/存货`（左）与 `短期借款/应付账款/预收账款/应付职工薪酬/应交税费/盈余公积`（右）的期末与期初列**都正好是 `-`**；报表主体里没有任何金额恰为 `0.00`；空白格 ≥20 个 | PASS |

### 表头 / 表尾（卡面版式要求）

- 页眉：公司名（云枢）+ 报表名称（`资 产 负 债 表` / `利 润 表` / `现 金 流 量 表`）
- 表头行：`编制单位：` / `期间：2026年11月（未结账）` / `日期：2026-11-30` / `单位：元`
- 表尾：`制表人：` `审核人：` `日期：`（三格留空手写）+ 页脚「第 X 页 / 共 Y 页」

### 负面分支

| 场景 | 结果 |
|------|------|
| 不带 token | 401 |
| 有 `business:erp:report:balance-sheet` 但**没有** `business:erp:report:export` | **403**，`msg` 点名权限码 |
| 只有 `business:erp:report:export` 的普通用户（非 SUPER_ADMIN） | **200**，且拿到合法 PDF |
| `type=profit` / `type=balance-sheetx` | 400 + `10001`，`msg` 列出三个合法值 |
| `period` 缺失 / `2026-13` / `2026-11-01` | 400 + `10001` |
| `period=1998-07`（没开账也没凭证） | 400 + **`66010`**（与报表页面同码，不是 500） |

### 审计

```
operation_logs: EXPORT_REPORT_PDF | biz_report | 10 行
detail.report_type: balance-sheet, cash-flow, income-statement（三种齐全）
detail.source: PDF-07（10 条）
```

## 4. 文件

```
sample/
  report-balance-sheet.pdf   (+ -p1-1.png)   左右两栏、期末/期初、两总计同行、勾稽块
  report-income-statement.pdf (+ -p1-1.png)  项目/本期金额/本年累计、合计行加粗
  report-cash-flow.pdf       (+ -p1-1.png)   三大活动分节 + 小计/净额 + 期末现金 + 附注
raw/
  pdftotext_report-*.pdf.txt   文本层（-layout，逐列可判）
  pdfinfo_report-*.pdf.txt     页数/纸张
  pdffonts_report-*.pdf.txt    字体嵌入（CID，非 Type3）
  raw_e2e.txt                  154 项断言全量输出
e2e145.py                      实测脚本（stdlib + urllib，无第三方依赖）
```

## 5. 复跑

```bash
# 145 上
bash /root/pdf07_instance.sh status          # 应 health=200
bash /root/pdf07_instance.sh start           # 或重启（换二进制必须先 stop）
cd /root/pdf07-evidence && \
  BASE=http://127.0.0.1:8082 DB=cloudpivot_pdf07 ENVF=/root/pdf07.env \
  EVID=/root/pdf07-evidence python3 e2e145.py
```
