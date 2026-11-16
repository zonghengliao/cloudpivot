# M3-07 报价单 PDF 模板 · 验收证据

> 卡：`t_53f01475`（M3-07【M3·销售】报价单：PDF 模板）
> 日期：2026-11-17
> 结论：`GET /api/business/erp/quotation/{id}/pdf` 真跑通 —— 真后端 + 真库 + 真 Chromium，
> 演练 52 条断言全绿（`00-drill.out`）。

## 1. 这套东西长什么样

| 层 | 落点 | 说明 |
|----|------|------|
| 模板 | `backend/app/src/business/pdf/templates/quotation.html.tera`（新） | 由 `order.html.tera` 派生；11 列明细（订单 9 列 + **税率 / 税额**）；有效期横幅单独一行；签字位 3 个（制单 / 审核 / 客户确认） |
| 模板上下文 | `pdf/template.rs` ⑨ 段（`QuotationContext` / `QuotationItem` / `render_quotation` / `quotation_pdf_opts` / `sample_quotation` / `format_tax_rate`） | 金额走 `format_money(fen)` + `amount_upper(fen)`（与购销订单同一套）；税率走 `format_tax_rate`（`13` → `13%`，0 也印 `0%`） |
| 取数 + 渲染 + 审计 | `backend/app/src/business/erp/sales/quotation_pdf.rs`（新） | `quotation_context`（单头 + 明细 + 批量补物料/单位/制单人 + 客户主档）、`render_quotation_pdf`、`export_quotation`、HTTP 入口 |
| 路由 | `GET /business/erp/quotation/{id}/pdf`（`/api` 由 `[router].prefix` 注入） | 权限码 **复用 `business:erp:quotation:list`**，不另开 `:export`（与 PDF-02/03 同口径；另开码只会造「详情看得见、点导出 403」的哑弹，还要同步迁移 + 角色矩阵 + 前端三处） |
| 前端 | `frontend/src/views/business/erp/quotation/index.vue` 详情抽屉动作行 | 复用 `DocPdfButton`（带 token 取 blob、用后端给的中文名存盘、403 说清缺哪个码），`testid="qt-detail-pdf"` |

**没有新增迁移**：复用 `:list` 码 = 不新增权限点 = 不需要 `sys_menu` 行（本卡「只追加迁移」这一条自然满足）。

## 2. 跑了什么（`00-drill.out`，52 条断言）

```
库：145 的 cp_m307_drill_1（克隆自 cloudpivot_test，演练后已 dropdb）
后端：145:18023（临时实例，PDF_CDP_URL=http://127.0.0.1:9222，真 Chrome/157）
脚本：scripts/verify/m307_quotation_pdf_drill.py
```

| 组 | 验的是 | 结果 |
|----|--------|------|
| 引擎 | `/api/business/pdf/health` 连上真 Chromium（`Chrome/157.0.8103.38`，CDP `:9222`） | 3/3 |
| 导出 | 200 + `application/pdf` + `%PDF-` 文件头 + RFC 5987 中文下载名 `报价单-QT-…pdf` + >20KB | 6/6 |
| 内容 | 页眉「报价单」、单号、客户、两行物料编码、**税率/税额两列**、`13%`、三项合计 237.04/30.81/267.85、大写「贰佰陆拾柒元捌角伍分」、有效期含当天、签字位 3 个、页脚真页码、行备注 | 14/14 |
| 审计 | `operation_logs` 落下 `EXPORT_QUOTATION_PDF`（导出是 GET，中间件不记），`target_type=biz_quotation`，`detail` 带 doc_no / row_count / total_with_tax | 5/5 |
| 过期 | 已过期单照样能导出，单面印红框「已过有效期，价格仅供参考」，且不再印「有效期内可按本单价格成交」 | 4/4 |
| 跨页 | 60 行 → 2 页；**每页都重复表头**（`thead{display:table-header-group}`）；页脚总页数=真页数；末行没被腰斩 | 5/5 |
| 错误口径 | 不存在 → 404；非法 id → 400 + **66006**；未登录 → 401；**无 `quotation:list` 的用户 → 403**（探针用户能登录，证明 403 来自「没码」） | 6/6 |
| 布局 | PyMuPDF 把第 1 页渲成 PNG（人工视觉核对） | 1/1 |

## 3. 截图

- `01-quotation-2lines.png` —— 2 行报价单（正常单）：11 列全显示、金额右对齐等宽、有效期横幅浅底、三项合计、大写、3 个签字位、页脚「第 1 页 / 共 1 页」。
- `02-quotation-multipage.png` —— 60 行跨页第 1 页：表头重复、序号连续、末行不腰斩、页脚「第 1 页 / 共 2 页」。
- `03-quotation-expired.png` —— 已过期单：有效期横幅变红框 + 「已过有效期，价格仅供参考；如需按此价成交请重新报价。」

## 4. 两个刻意的决定（不是待办）

1. **过期单不拒绝导出**：客户手里那份纸质报价还在，业务员要核对「当时报的是什么价」。悄悄印一份看起来仍然有效的报价，比印一张写明「已过期」的更危险 —— 所以照出 PDF，只把横幅变红、提示语换掉。
2. **11 列仍用 A4 纵向**：靠收窄编码/规格/单位/数量四列 + 明细字号 8.5pt 排下（物料名称仍有 ~130pt），实测不折行不串列（见 `01`）。换来的是装订归档与订单/退货单同一套习惯。退货单那种带库位、序列号长文本的 12 列才必须横向。

## 5. 已知的「不好看但正确」

演示库里有个单位名字面值是 `个(E2E02)`（不是 `个`），30pt 的单位列会折成 3 行 —— 折行而不是截断/改写，是**故意的**：打印件上宁可多一行，也不能把一个单位改成看起来对的值。真实主数据的单位是 1–2 个字，单行放得下（同一模板的订单版 `c-unit` 只有 28pt，PDF-02 已验收）。

## 6. 复核命令

```bash
# 库
PGPASSWORD=$PGPASSWORD createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m307_drill_1
# 后端（必须跑在有 Chromium 的机器上；99 上没有 chromium-pdf.service）
ssh root@172.18.10.145 'cd /opt/cloudpivot/backend && set -a && . /etc/cloudpivot/cloudpivot-dev.env && set +a \
  && NIK_DATABASE_NAME=cp_m307_drill_1 NIK_SERVER_LISTEN=0.0.0.0:18023 PDF_CDP_URL=http://127.0.0.1:9222 \
  && setsid nohup ./target/debug/cloudpivot > /tmp/m307/run.log 2>&1 < /dev/null &'
# 演练
python3 scripts/verify/m307_quotation_pdf_drill.py     # 依赖 pypdf / pymupdf（uv pip install pypdf pymupdf）
# 收尾
PGPASSWORD=$PGPASSWORD dropdb -h 172.18.10.145 -U cloudpivot cp_m307_drill_1
```
