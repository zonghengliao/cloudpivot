# PDF-03 证据：合同 / 收货入库单 / 出库单 PDF 模板（2026-11-08）

> 卡：`t_6bb3d852`「PDF-03 PDF 模板-2：合同 + 入库单 + 出库单」
> 提交：`69b0cd6`（主体）→ `9248630`（明细列宽收口）
> 环境：145 隔离实例 `127.0.0.1:8078`（库 `cloudpivot_pdf03`，**空库首启迁移 278 条 / 二次启动 0 条**），
> 二进制 md5 `2e4ec1fd97281aa00a31b1a34451ec52`（**99 干净 worktree `/root/pdf03-wt` @ `9248630` 构建后 scp 过去**，
> 不含任何他人未提交的 WIP）；**没有动** 145 的 `cloudpivot-test.service`（8033），也没碰 144 生产。
> 渲染侧：145 本机 `chromium-pdf.service`（`:9222`）+ PDF-01 装好的静态 glyf Noto Sans SC。

## 一、验收 4 项（逐条实测）

| # | 卡面验收 | 结果 | 关键实测值 | 证据 |
|---|---------|------|-----------|------|
| ① | 合同导出含签字盖章位和附件清单 | ✅ | `PDF03-HT-000001`，**3 页**，78,872 B；甲方/乙方盖章位 + 两个「日期：____年__月__日」+ 骑缝章提示；附件清单 2 份（`PDF03-合同扫描件-第1页.pdf` 合同扫描件 262.5 KB / `PDF03-用印授权书.pdf` 补充协议 100.0 KB）；60 条条款正文全文（第 1 条 ↔ 第 60 条都在），付款方式从正文识别 | `sample/contract.pdf`、`sample/contract-p1-1.png`、`sample/contract-p3-3.png`、`raw/pdftotext_contract.txt` |
| ② | 入库单含批次号 | ✅ | `PDF03-IN-000001`，1 页，71,291 B；批次号列 + `LOT-20261108-A` / `LOT-20261108-B` **整串一行**；库位 `A-01-03`；来源单据号 `PDF03-PO-000001`；序列号清单 3 个（`PDF03-SN-000001..3`）；价税合计 422.00 大写「肆佰贰拾贰元整」 | `sample/stock-in.pdf`、`sample/stockin-p1-1.png`、`raw/pdftotext_stock-in.txt` |
| ③ | 出库单含来源单据号 | ✅ | `PDF03-OUT-000001`，**2 页**，220,191 B；来源单据号 `PDF03-SH-000001`（发货单）、客户 `PDF03 测试客户`、出库事由；成本口径（**无税额 / 价税合计字样**），大写「人民币陆佰壹拾柒元整（￥617.00）」 | `sample/stock-out.pdf`、`sample/stockout-p2-2.png`、`raw/pdftotext_stock-out.txt` |
| ④ | 跨页正常 | ✅ | 合同 3 页：每页页脚 `第 X 页 / 共 3 页`，条款跨页不断行、签章位整块落在末页；出库单 2 页：**表头 2/2 页重复**、序号 34→50 连续（不是从 1 重来）、50 行一行不少 | 同上两张 PNG + `raw/pdftotext_*.txt` |

E2E 共 **67 项断言，0 失败**（`raw/e2e_run.txt` 末行 `==== 汇总：67 项，失败 0 项 ====`）。

## 二、接口与响应头（真实 token）

```
GET /api/business/erp/contract/{id}/pdf    → 200 application/pdf  （78,872 B）
GET /api/business/erp/stock-in/{id}/pdf    → 200 application/pdf  （71,291 B）
GET /api/business/erp/stock-out/{id}/pdf   → 200 application/pdf  （220,191 B）
```

合同实测响应头：

```
Content-Type: application/pdf
Content-Disposition: attachment; filename="__-PDF03-HT-000001.pdf";
                     filename*=UTF-8''%E5%90%88%E5%90%8C-PDF03-HT-000001.pdf
Cache-Control: no-store
```

无权限账号（`PDF03_NO_PERM` 角色，不授任何菜单）三个接口全部 **403 且点名权限码**：

```
403 无权限访问，需要权限: business:erp:contract:list
403 无权限访问，需要权限: business:erp:stock-in:list
403 无权限访问，需要权限: business:erp:stock-out:list
```

## 三、导出审计（`operation_logs`）

`GET` 不会被操作日志中间件记录，导出成功后由 `doc_pdf_model::write_export_log` 显式补一行（`raw/operation_logs.txt`）：

| operation | target_type | 单号 | 行数 | 附件 | 金额 | 字节 |
|---|---|---|---|---|---|---|
| EXPORT_CONTRACT_PDF | biz_contract | PDF03-HT-000001 | — | 2 | — | 78,871 |
| EXPORT_STOCK_IN_PDF | biz_stock_in | PDF03-IN-000001 | 3 | — | 422.00 | 71,291 |
| EXPORT_STOCK_OUT_PDF | biz_stock_out | PDF03-OUT-000001 | 50 | — | 617.00 | 220,191 |

`request_method='GET'`、`status_code=200`、`detail.source="PDF-03"`；`ip` 为空（本机 curl 直连无 `X-Real-IP`，与中间件同场景一致）。

## 四、门禁

| 项 | 命令 | 结果 |
|---|---|---|
| 模板单测 | `cargo test --lib business::pdf::` | **60 passed / 0 failed**（`raw/unit_pdf.txt`） |
| 合同（纯逻辑 + 真库） | `ERP_DB_URL=… cargo test --lib business::erp::contract_pdf -- --test-threads=1` | **7 passed / 0 failed**（`raw/unit_contract_pdf.txt`） |
| 出入库单（纯逻辑 + 真库） | `ERP_DB_URL=… cargo test --lib business::erp::stock_pdf -- --test-threads=1` | **13 passed / 0 failed**（`raw/unit_stock_pdf.txt`） |
| 公共数据层 | `ERP_DB_URL=… cargo test --lib business::erp::doc_pdf_model` | **6 passed / 0 failed**（`raw/unit_doc_pdf_model.txt`） |
| clippy | `cargo clippy --lib --all-targets` | 本卡 4 个文件 **0 告警**（`raw/clippy.txt`；全 crate 剩余告警均为他人既有） |
| rustfmt | `rustfmt +1.94.1 --edition 2024 --check <本卡 4 个 .rs>` | 无 diff |
| 全量 lib 套件 | `cargo test --lib` | **3658 passed / 3 failed**（`raw/full_lib_test.txt`）—— 3 条全是**既有红**：`business::flow::linkage::*` 的 LNK-12 触发点白名单（差异来源是别的卡的 `PURCHASE_RETURN` 触发点），**与本卡文件无关** |
| 迁移 | 空库首启 / 二次启动 | **278 / 0**（本卡**无新增迁移**） |

## 五、产物

| 文件 | 说明 |
|------|------|
| `sample/contract.pdf` / `sample/contract-p1-1.png` / `sample/contract-p3-3.png` | 验收①：3 页合同（第 1 页版式、第 3 页签章位） |
| `sample/stock-in.pdf` / `sample/stockin-p1-1.png` | 验收②：1 页入库单（批次号 / 库位 / 序列号清单 / 收货签字栏） |
| `sample/stock-out.pdf` / `sample/stockout-p2-2.png` | 验收③④：2 页出库单（表头重复、序号 34→50、页脚页码） |
| `raw/e2e_run.txt` | 67 项断言完整输出 |
| `raw/result.json` | 同一轮结构化结果 |
| `raw/pdftotext_*.txt` | 三份 PDF 全文（中文可抽取 = 不乱码） |
| `raw/pdffonts_*.txt` / `raw/pdfinfo_*.txt` | 字体嵌入（`NotoSansSC emb=yes sub=yes uni=yes`）/ 文档属性 |
| `raw/operation_logs.txt` | 三条导出审计行 |
| `raw/instance.log` | 145 隔离实例启动日志（迁移 278 / 复用 Redis db8） |
| `raw/seed145.sql` / `raw/e2e145.py` | 夹具与实测脚本（可复跑） |
