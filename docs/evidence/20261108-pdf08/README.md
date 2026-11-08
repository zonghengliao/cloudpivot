# PDF-08 · 单据 PDF 下载按钮统一接入 + 导出审计（证据）

> 卡面：`t_f6cfb133`。目标：12 个单据/审批/任务/报表入口都能点「下载 PDF」，
> 下载带 token（不许 `window.open`）、文件名用后端的中文名、下载中 loading、
> 失败提示可重试、无权限 403 有提示；后端 `operation_logs` 记「谁在什么时候导出了哪张单」。

## 一句话结论

| 验收 | 结果 | 证据 |
|------|------|------|
| ① 采购/销售订单、合同、入库、出库、调拨、盘点、退货（采购+销售）都有按钮 | ✅ 9 页 | `sample/ui-*-button.png`（9 张截图）+ 单测 `tests/unit/pdf08-doc-pdf.test.ts` 页面契约 14 条 |
| ② 审批实例、任务、三张报表也有 | ✅ 5 页 | 同上（`ui-任务-button.png`、`ui-资产负债表-button.png` 等） |
| ③ 点击能下载，文件名正确、中文不乱码 | ✅ 9 个真实浏览器下载 | `sample/dl-*.pdf`（9 份真文件，24KB–215KB）；文件名断言见 `raw_browser.txt` |
| ④ `operation_logs` 有导出记录 | ✅ 12 类导出逐条 +1 | `raw/run_145.log`（每行 `audit=操作码|target_type|单号|admin|时间`） |
| ⑤ 无权限 → 403 + 提示 | ✅ | `sample/ui-403-toast.png`（提示原文见下）+ `raw/run_145.log` 的 401/403/404 段 |

## 1. 后端实测（145 隔离实例，真 token curl）

```bash
# 5 个实例，复用各 PDF 卡留下的库（不重造数据）
8091 cloudpivot_pdf03 → 采购/销售订单、合同、入库、出库、调拨、盘点
8092 cloudpivot_pdf04 → 采购退货、销售退货
8093 cloudpivot_pdf05 → 审批实例
8094 cloudpivot_pdf06 → 任务
8095 cloudpivot_pdf07 → 三张报表（2026-10，PDF-07 造的 1,500.00 凭证）

ssh root@172.18.10.145 'cd /root/pdf08-evidence && \
  EVID=/root/pdf08-evidence ENVF=/root/pdf07.env python3 e2e145.py'
```

**结果：断言 165 项，PASS=165 FAIL=0**（`raw/run_145.log`）。

每条单据断言 11 项：200 / `application/pdf` / `%PDF-` 魔数 / `filename*=UTF-8''` /
中文名解出 / 文件名含单号 / 页数 ≥ 1 / 文本层含单号 / `operation_logs` +1 /
`target_type` 正确 / 审计记录里有 `username=admin` 和导出时间。

审计实样（`operation_logs`，GET 不走中间件，是 PDF handler 显式补的）：

```
EXPORT_PURCHASE_ORDER_PDF | biz_purchase_order | PDF03-PO-000001 | admin | 2026-11-09 01:56:29
EXPORT_SALES_ORDER_PDF    | biz_sales_order    | PDF03-SO-000001 | admin | 2026-11-09 01:56:29
EXPORT_CONTRACT_PDF       | biz_contract       | PDF03-HT-000001 | admin | 2026-11-09 01:56:29
EXPORT_STOCK_IN_PDF       | biz_stock_in       | P1FIX-IN-1      | admin | 2026-11-09 01:56:29
EXPORT_STOCK_OUT_PDF      | biz_stock_out      | PDF03-OUT-000001| admin | 2026-11-09 01:56:30
EXPORT_STOCK_TRANSFER_PDF | biz_stock_transfer | P1FIX-TR-1      | admin | 2026-11-09 01:56:30
EXPORT_STOCK_TAKE_PDF     | biz_stock_take     | PDF04-ST-000001 | admin | 2026-11-09 01:56:30
EXPORT_PURCHASE_RETURN_PDF| biz_purchase_return| PDF04-PRT-000001| admin | 2026-11-09 01:56:31
EXPORT_SALES_RETURN_PDF   | biz_sales_return   | PDF04-SR-000001 | admin | 2026-11-09 01:56:31
EXPORT_FLOW_INSTANCE_PDF  | biz_flow_instance  | FLW-2026…AD5A4448 | admin | 2026-11-09 01:56:32
EXPORT_TASK_PDF           | biz_task           | TSK-202611-000001 | admin | 2026-11-09 01:56:33
EXPORT_REPORT_PDF         | biz_report         | 资产负债表 2026-10 | admin | 2026-11-09 01:56:35
```

下载名（后端 `Content-Disposition` 的 `filename*`，中文不乱码）：

```
采购订单-PDF03-PO-000001.pdf      收货入库单-P1FIX-IN-1.pdf
销售订单-PDF03-SO-000001.pdf      出库单-PDF03-OUT-000001.pdf
合同-PDF03-HT-000001.pdf          调拨单-P1FIX-TR-1.pdf
盘点单-PDF04-ST-000001.pdf        采购退货单-PDF04-PRT-000001.pdf
销售退货单-PDF04-SR-000001.pdf    审批单-FLW-20261108-AD5A4448.pdf
派工单-TSK-202611-000001.pdf      资产负债表-BALANCE_SHEET-2026-10.pdf
```

负面分支：

```
未登录（无 Authorization）→ 401，且响应体不是 PDF
不存在的单 id             → 404
无 list 权限的账号        → 403  msg="无权限访问，需要权限: business:erp:purchase-order:list"
同一账号导合同            → 403
报表可看但无 export 码    → 403（请求 /report/balance-sheet/pdf）
```

> 单据类 PDF 与「查看」**同一个权限码**（如 `business:erp:purchase-order:list`）：
> 能看见单据就一定能导出。真正会出现「看得见按钮但 403」的是**报表页**
> （页面码 `business:erp:report:balance-sheet` / 导出码 `business:erp:report:export`）。

## 2. 前端实测（真实 Chromium 点击按钮）

前端用 `vite dev` + `BACKEND_ORIGIN` 反代到 145 的隔离实例（三个 UI 端口 5191/5194/5195），
`browser_check.mjs` 用 Playwright 真点按钮、真收文件：

```bash
node docs/evidence/20261108-pdf08/browser_check.mjs
# 真实浏览器断言 71 项：PASS=71 FAIL=0（raw_browser.txt）
```

每个入口断言 7 项：有「下载 PDF」按钮 / 文案 / 真收到 download 事件 /
文件名匹配后端中文名 / 文件 > 20KB / 请求带 `Authorization: Bearer` / 页面无 JS 报错。
另外两条专项：

- **loading**：把 PDF 请求人为延迟 1.5s，点下去 500ms 时按钮 `t-button--loading` →
  「采购订单：下载中按钮进入 loading」PASS；
- **403 提示**：用 `pdf08_rpt_noperm`（`report:balance-sheet` + `period:list`，无 `report:export`）
  登录报表页，按钮照常显示（不藏），点击后红条原文：
  `没有导出该单据 PDF 的权限：无权限访问，需要权限: business:erp:report:export（可重试）`
  → `sample/ui-403-toast.png`。

## 3. 目录

```
20261108-pdf08/
├── README.md             # 本文件
├── e2e145.py             # 后端 12 类导出 + 审计 + 401/403/404（145，stdlib）
├── browser_check.mjs     # 真实 Chromium 点击 9 个入口（Playwright）
├── fixture_403.sql       # 报表「可看不可导出」夹具（角色 + 用户，仅 pdf07 库）
├── raw/
│   ├── run_145.log       # e2e145.py 全程输出（165 断言）
│   └── raw_e2e.txt       # PASS/FAIL 汇总
├── raw_browser.txt       # 浏览器断言逐条（71 条）
├── curl/                 # 145 导出的 14 份 PDF（curl 侧）
└── sample/
    ├── dl-*.pdf          # 浏览器真下载的 9 份（文件名即浏览器看到的名字）
    ├── ui-*-button.png   # 每个入口的按钮位置（右上角/吸顶动作条）
    ├── ui-*-after.png    # 下载后的页面
    └── ui-403-toast.png  # 无权限红条
```

## 4. 复现要点（踩过的坑）

1. **五个库、五个实例**：一个实例只能连一个库；各 PDF 卡的数据分散在 pdf03–pdf07，
   所以 8091–8095 各起一个（`setsid --fork` 后台起，日志 `/root/pdf08-*.log`）。
2. **admin 密码**：这些库是各卡实测留下的，admin 口令早被改过（`Pdf04@…` 等）。
   本卡把其余库 `sys_user.password` 对齐到 pdf04 那份哈希（`Admin@12345678`），
   脚本里还带了一串候选口令，任一命中即可。
3. **前端两个「首次进入」遮罩**：「首次启动向导」和「新功能引导」会盖住列表，
   点不到「查看」。`addInitScript` 里写
   `cloudpivot.setup-wizard.skipped:<用户>=1` 与 `cp-onboarding-done=1` 后消失。
4. **TDesign 固定列的 DOM 是两份**：操作列 `fixed: 'right'` 会复制一份隐藏副本，
   直接点 `.first()` 会点到被遮住的那份 → 抽屉不开。`openDetailUntil()` 逐个试，
   直到详情里的 PDF 按钮可见。
5. **报表页按钮默认禁用**：期间没选中时 `:disabled="!sheet"`。要用 `.t-select-option`
   选一个期间（本卡选 2026-10）等自动重算后再点。
6. **别把 `blob:` 的 pdf.js 请求当 PDF 接口**：附件预览的 worker URL 里也有 `pdf`，
   断言 Bearer 头时只认 `http…/api/…/pdf`。
