# PDF-04 证据：调拨单 / 盘点表 / 采购退货单 / 销售退货单 PDF 模板（2026-11-08）

> 卡：`t_1849386f`「PDF-04 PDF 模板-3：调拨单 + 盘点单 + 退货单」
> 提交：`5cb3bc8`（主体）→ `b7dbf40`（规格列宽）→ `67c5ffb`（合计行标签 + 序列号列）→ `ce3b894`（th 列宽 class）→ `efbb3d9`（盘点合计币种）
> 环境：145 隔离实例 `127.0.0.1:8078`（库 `cloudpivot_pdf04`，**空库首启迁移 280 条 / 二次启动 0 条**），
> 二进制 md5 `d54c32e5f996e9296692968a46b3b094`（`/root/pdf04-app` @ `efbb3d9`，`:8078` + redis db9）；
> **没有动** 145 的 `cloudpivot-test.service`（8033）、也没碰 144 生产。
> 渲染侧：145 本机 `chromium-pdf.service`（`:9222`）+ PDF-01 装好的静态 glyf Noto Sans SC。

## 一、卡面验收 3 项（逐条实测）

| # | 卡面验收 | 结果 | 关键实测值 | 证据 |
|---|---------|------|-----------|------|
| ① | 盘点单 PDF 有空白手写栏 | ✅ | `PDF04-ST-000001`，**3 页**，209,739 B；表头 `账面数量 / 实盘数量 / 差异 / 备注`，**60/60 行每行只有 1 个数字（账面数量）**，三栏全空（行高 22pt 供手写）；库里已录的实盘数（第 5 行 35→32）与备注（「实物短缺 3 台」）**一个字都没印**；填表说明写明「三栏留空、由盘点人现场手写」；盘盈 2 / 盘亏 4 / 差异金额 74.04 只在合计区 | `sample/stock-take.pdf`、`stock-take-p1-1.png`、`stock-take-p2-2.png`、`stock-take-p3-3.png`、`raw/pdftotext_stock-take.txt` |
| ② | 调拨单有调出 / 调入仓库 | ✅ | `PDF04-TK-000001`，1 页，77,604 B；路线条「调出：PDF04 上海一号仓 ➜ 调入：PDF04 上海二号仓（在途经 PDF04 在途仓）」+ 单据头 `调出仓库/调入仓库/在途仓` 三格；明细 4 行含调出库位 A-01-03 / 调入库位 B-02-04；成本金额 555.60 大写「人民币伍佰伍拾伍元陆角整」；**无价税合计**（原价转移口径） | `sample/stock-transfer.pdf`、`stock-transfer-p1-1.png`、`raw/pdftotext_stock-transfer.txt` |
| ③ | 退货单有原单号、退货原因 | ✅ | 采购 `PDF04-PRT-000001` 1 页 69,639 B：`原单号（入库）=PDF04-IN-000001`、`原单号（采购订单）=PDF04-PO-000001`、退货原因横幅「阀体密封面有砂眼，供应商书面同意退货，运费由供方承担」；销售 `PDF04-SR-000001` 1 页 69,231 B：`原单号（销售订单）=PDF04-SO-000001`、`原单号（发货单）=PDF04-SH-000001`、退货原因「质量（QUALITY）」、序列号 `PDF04-SN-000001、PDF04-SN-000002` 整串一行 | `sample/purchase-return.pdf`、`sample/sales-return.pdf`、两张 PNG、`raw/pdftotext_*.txt` |

E2E 共 **118 项断言，0 失败**（`raw/e2e_run.txt` 末行 `==== 汇总：118 项，失败 0 项 ====`）。

## 二、接口与响应头（真实 token）

```
GET /api/business/erp/stock-transfer/{id}/pdf     → 200 application/pdf（77,604 B）
GET /api/business/erp/stock-take/{id}/pdf         → 200 application/pdf（209,739 B）
GET /api/business/erp/purchase-return/{id}/pdf    → 200 application/pdf（69,639 B）
GET /api/business/erp/sales-return/{id}/pdf       → 200 application/pdf（69,231 B）
```

- 文件名走 RFC 5987（`filename*=UTF-8''…`），`Cache-Control: no-store`。
- 无权限账号（`PDF04_NO_PERM` 角色，不授任何菜单）四个接口全部 **403 且点名权限码**：
  `business:erp:transfer:list` / `business:erp:stock-take:list` / `business:erp:purchase-return:list` / `business:erp:sales-return:list`。
- 不存在 id → 404 且点名单据类型（调拨单 / 盘点单 / 采购退货单 / 销售退货单）；非法 id → 400。

## 三、导出审计（`operation_logs`）

`GET` 不会被操作日志中间件记录，导出成功后由 `doc_pdf_model::write_export_log` 显式补一行（`raw/operation_logs.txt`）：

| operation | target_type | 单号 | 行数 | 金额 | 字节 | render_ms |
|-----------|-------------|------|------|------|------|-----------|
| `EXPORT_STOCK_TRANSFER_PDF` | `biz_stock_transfer` | PDF04-TK-000001 | 4 | 555.60 | 77,604 | 171 |
| `EXPORT_STOCK_TAKE_PDF` | `biz_stock_take` | PDF04-ST-000001 | 60 | 差异 74.04 | 209,739 | 322 |
| `EXPORT_PURCHASE_RETURN_PDF` | `biz_purchase_return` | PDF04-PRT-000001 | 2 | 97.70 | 69,639 | 166 |
| `EXPORT_SALES_RETURN_PDF` | `biz_sales_return` | PDF04-SR-000001 | 2 | 226.00 | 69,231 | 163 |

## 四、单测（99 上真库跑，`ERP_DB_URL` 指向 cloudpivot_pdf03）

| 模块 | 条数 | 说明 |
|------|------|------|
| `business::pdf::template` | 43 | 新增调拨 / 盘点 / 退货结构面用例（三列空白格计数、路由条、专有列、页脚、转义） |
| `business::erp::transfer_pdf` | 10 | 5 纯逻辑（序号 / 双库位 / 兜底 / 金额同源）+ 5 真库（调出调入仓库、在途仓随状态、审计行、404、400） |
| `business::erp::stock_take_pdf` | 9 | 4 纯逻辑 + 5 真库（空白三栏、差异区块、审计、404、400） |
| `business::erp::return_pdf` | 10 | 5 纯逻辑 + 5 真库（采购原单号/原因、销售原单号/类型、审计、404、400） |
| `business::erp::doc_pdf_render` | 1 | 文件名净化 |
| `business::erp::doc_pdf_model` | 6 | 含新增 `location_dict_labels_of` 空列表短路 |
| 合计 | **79** | 全绿（`cargo test --lib -- <6 个模块> --test-threads=1`） |

全量 `cargo test --lib`：**3718 passed / 3 failed**，3 条全为**既有红**
（`business::flow::linkage` 的 LNK-12 触发点白名单：`PURCHASE_RETURN` 已接线但预期清单未更新，属别的卡的活，与本卡改动无关）。

本卡文件 clippy 0 告警、`rustfmt +1.94.1 --check` 无 diff。

## 五、启动 / 迁移幂等

- 空库首启（`instance_first_start.log`）：`数据库迁移完成: 应用 280 个迁移`；
- 二次启动（`instance_restart_*.log`）：`数据库迁移完成: 应用 0 个迁移`；
- `schema_migrations` 行数 280；本卡**无新增迁移**（四张单据表都是既有表）。

## 六、文件清单

```
sample/  stock-transfer.pdf + p1 PNG / stock-take.pdf + p1·p2·p3 PNG /
         purchase-return.pdf + p1 PNG / sales-return.pdf + p1 PNG
raw/     e2e_run.txt（118 项断言）· pdftotext_* · pdfinfo_* · pdffonts_*（字体 CID TrueType emb=yes）
         operation_logs.txt · instance_first_start.log · instance_restart_final.log · instance_final_run.log
         build_first.log / build_width_fix.log / build_style_fix.log / build_final.log
         pdf04_build_145.sh · pdf04_start_145.sh · binary_md5.txt
```

`seed145.sql`（本目录）是四张单据的夹具（命名空间 `0f51…0f55`，幂等：先删后插）。

## 七、本批修掉的 4 个真问题（都是 145 实测发现，先看 PNG 才发现）

1. **规格列 56pt 把 `DN100/PN16` 折成两行** → 66pt（三个模板统一），`pdftotext` 抽出来才是一整串。
2. **合计行标签折行**（「成本金额合计（CNY）」「金额合计（CNY）」「价税合计（大写）」「差异金额合计（CNY）」）
   → 标签去币种后缀 / 改用「价税合计大写」+ `white-space: nowrap`，币种移到数值格。
3. **销售退货序列号列 auto 布局被挤到 100pt**，号码从中间劈开（`PDF04-SN-` + `000001、`）
   → 明细改 `table-layout: fixed`，序列号 170pt。
4. **fixed 布局只认首行**：只给 `td` 写宽度无效（列宽被均分）。
   → `<th>` 也带 `.c-*` 列宽 class（CSS 选择器 `td.c-x` 改为 `.c-x`），宽度才真正生效。
