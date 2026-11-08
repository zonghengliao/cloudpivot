# 单据 PDF 模板（二）：合同 / 收货入库单 / 出库单

> 卡：`PDF-03`。前一份是 [PDF单据模板-采购销售订单.md](./PDF单据模板-采购销售订单.md)（PDF-02），
> 底座（Chromium / 字体 / `business/pdf` 分层）见 [PDF导出基础设施.md](./PDF导出基础设施.md)。
> 本文件只讲「这一批三种单据怎么落」，以及**加新单据时最容易踩的 7 个坑**。

## 一、接口与权限

| 方法 | 路径 | 权限码 | 说明 |
|------|------|--------|------|
| GET | `/api/business/erp/contract/{id}/pdf` | `business:erp:contract:list` | 合同（条款正文 + 签章位 + 附件清单 + 骑缝章提示） |
| GET | `/api/business/erp/stock-in/{id}/pdf` | `business:erp:stock-in:list` | 收货入库单（批次号 / 库位 / 序列号 / 收货签字栏） |
| GET | `/api/business/erp/stock-out/{id}/pdf` | `business:erp:stock-out:list` | 出库单（来源单据号 / 成本口径 / 发货签字栏） |

- **权限复用单据查看码**，不新种 `:export`：能看这张单就能打印它（否则会造出「详情看得见、点导出 403」的哑弹，
  还要同步迁移 + 角色矩阵 + 前端按钮三处）。
- 响应：`200 application/pdf`，`Content-Disposition: attachment` 中文走 RFC 5987
  （ASCII 回退名把中文换成 `_`，浏览器优先用 `filename*`）；`Cache-Control: no-store`。
- 前端取件必须 `responseType: 'blob'`。

## 二、代码落点

```
backend/src/business/pdf/
  templates/contract.html.tera   合同模板（include_str! 进二进制）
  templates/stock.html.tera      收货入库单 / 出库单共用模板
  template.rs                    ContractContext / StockContext + 渲染 + 页脚 + PdfOpts
backend/src/business/erp/
  contract_pdf.rs                合同：取数 + 渲染 + HTTP 入口 + 审计
  stock_pdf.rs                   出入库单：取数 + 渲染 + HTTP 入口 + 审计
  doc_pdf_model.rs               PDF-03 起的公共数据层：库位/仓库/供应商/客户补名、
                                 序列号清单、导出审计（可复用于后续单据）
```

`business/pdf` **不读业务数据**：业务侧自己查库组装上下文，然后两步
`template::render_*(ctx)` → `engine::render_pdf(html, opts)`。

## 三、合同 PDF 的五个特殊要求怎么落

| 要求 | 落地 |
|------|------|
| 正文（富文本 / 条款）**完整渲染** | `biz_contract.remark` 是系统里唯一的条款正文字段，**全文**进 `.body-text { white-space: pre-wrap }`：不截断、不去换行、逐段保留。本卡不引入富文本列（那要同步 CRUD / 前端编辑器 / 迁移，属另一张卡）；没有正文时印「以双方签章的纸质合同为准」提示，不留白 |
| 甲乙双方信息 | 甲方取 `party_a_name`（为空 = 本组织本体，回落公司名）；乙方取**快照** `party_b_name`（主档改名不回溯历史合同）；主档已改名的，额外标一行「主档现状：新名（签约后已改名）」，主档已删的标「（主档已删除）」 |
| 金额 / 期限 / 付款方式 | 金额「小写 + 大写」出自同一个整数「分」（`yuan_to_fen` → `format_money` / `amount_upper`），逐位自洽；期限由 `start_date` / `end_date` 拼（缺一端写「… 起」/「至 …」，两端都缺写 `—`，**不编日期**）；付款方式从条款正文里识别「付款方式 / 付款条件 / 结算方式 / 支付方式 / 付款安排：…」，识别不到就印「以合同条款约定为准（本系统未单独登记付款方式）」—— `biz_contract` 没有付款方式列，**不许凭空写一个** |
| 签字盖章位 | 「甲方（盖章）」/「乙方（盖章）」两个框 + 各自「日期：____年__月__日」，整块 `break-inside: avoid`（盖章位被切到两页 = 合同作废） |
| 附件清单 | `biz_doc_attachment`（`doc_type='CONTRACT'`）按**手动排序**取，JOIN `sys_file` 出原始文件名 / 大小（人类可读）/ 上传人 / 上传时间（北京时间）；角色标签见 `contract_pdf::file_role_label`（ATT-04 的 8 个值域） |
| 骑缝章提示（多页时） | 签署页固定印一条**条件指令**「多页合同请在第 1 页至末页右侧骑缝处逐页加盖骑缝章…；单页合同在签署页盖章即可。」+ 每页页脚再提示一次。页数只有 Chromium 打完才知道（`totalPages` 拿不回来），所以不按真实页数动态显示——单页合同上出现这句是行业通行写法 |

## 四、出入库单模板

| 卡面字段 | 数据来源 |
|---|---|
| 仓库 | `biz_stock_in/out.warehouse_id` → `biz_warehouse.warehouse_name` |
| 库位 | 明细 `location_id` 指向**库位账行** `biz_inventory_location.id`（不是 `biz_location.id`！）→ JOIN `biz_location.location_code`；`NULL` 打「未分配」。单据头再给一个「库位」汇总：≤3 个全列，>3 个写「多库位（N 个，见明细）」 |
| 经办人 | `create_by` → `sys_user`（`real_name` 优先；离职员工照实打印当时的人） |
| 供应商 / 客户 | 入库单：`supplier_id` → `biz_supplier.supplier_name`（无源入库打 `—`）；出库单：`source_type='SALES'` 时取来源发货单的客户，手工建单打 `—`（**不编客户**） |
| 来源单据号 | 入库单：`source_order_id` → `biz_purchase_order.order_no`；出库单：`source_type='SALES' AND source_id` → `biz_shipment.ship_no`；没有来源就 `—` |
| 明细批次号 | `lot_no`（入库单 / 出库单都有列），空打 `—` |
| 序列号 | 一物一码：`biz_material_serial.source_id = 收货单 id`（**不按 `source_type` 过滤**：无源入库那条路的 `source_type` 是 NULL），按物料分组列「序列号清单」。出库单不登记序列号（出库是改状态），整段不出现 |
| 收货 / 发货签字栏 | 入库：收货人 / 质检 / 仓管 / 供应商送货人 / 制单；出库：发货人 / 领用人／收货人 / 仓管 / 审核 / 制单 |

**金额口径只读不重算**：入库是「不含税 / 税额 / 价税合计」三档，出库是**成本结转金额**（过账时账上真实差额，
不是 `qty × avg_cost`）。出库单因此**没有**税额行与「价税合计」字样，大写取成本金额。

## 五、导出审计（GET 不会被中间件记录）

`middleware::operation_log` 只记 POST/PUT/PATCH/DELETE（`constants::AUDITED_METHODS`），导出是 GET ——
渲染成功后由 `doc_pdf_model::write_export_log` 显式补一行：

| operation | target_type | detail（jsonb） |
|---|---|---|
| `EXPORT_CONTRACT_PDF` | `biz_contract` | `doc_type=CONTRACT` / `attachment_count` / `render_ms` |
| `EXPORT_STOCK_IN_PDF` | `biz_stock_in` | `doc_type=STOCK_IN` / `row_count` / `amount` / `render_ms` |
| `EXPORT_STOCK_OUT_PDF` | `biz_stock_out` | `doc_type=STOCK_OUT` / `row_count` / `amount` / `render_ms` |

公共字段 `doc_id` / `doc_no` / `filename` / `bytes` / `duration_ms` / `source="PDF-03"` 由
`write_export_log` 统一补齐。写日志失败**只记 error，不改响应**（用户要的 PDF 已经生成好了），走 `tokio::spawn`。

## 六、再加一种单据要改哪几处

1. `pdf/template.rs`：加模板常量 + 上下文结构（金额一律走 `yuan_to_fen` / `format_money` / `amount_upper`）+ `render_*` + `*_footer`（页脚页码 + 需要的话骑缝章位）+ 样本构造函数；
2. `pdf/templates/<x>.html.tera`：照抄 `stock.html.tera` 的四条打印铁律（`@page` / `thead{display:table-header-group}` / `break-inside:avoid` / `.num` 等宽右对齐）；
3. `erp/<x>_pdf.rs`：取数（批量补名字）+ 渲染 + `#[nik::permission(permission="…:list")]` + `#[nik::get("…/{id}/pdf")]` + 审计；
4. `erp/mod.rs`：注册模块（`rustfmt` 会按字母重排，别手工插错位置）；
5. 单测：模板结构面（关键文字全在）、转义、页脚、纯逻辑（金额 / 标签），真库用例（取数口径 + 审计行）。

## 七、7 个已踩过的坑

1. **tera 会把 `/` 转义成 `&#x2F;`**（`.html` 模板启用 HTML 自动转义）。所以：文案里尽量别用半角 `/`（本卡把「领用人 / 收货人」改成全角「／」）；单测断言也必须按转义后的串写，否则假红。
2. **CSS 类名不能撞**：明细表 `td.c-seq` 用来数行数，序列号清单也写 `c-seq` 会数出 4 行（3 行明细 + 1 行序列号）。序列号清单用 `s-*` 前缀。
3. **批次号 / 规格列宽要实测**：`LOT-20261108-A` 在 62pt 列里会被折成 `LOT-` / `20261108-A` 两行（`pdftotext` 抽出来也不连续），逐行核对批次直接失效。批次号 78pt、规格 62pt 才一行放得下。
4. **附件角色有个值域演进史**：V0212 是 `CONTRACT_SCAN / SIGN_RECEIPT / INVOICE / OTHER`，V0345 变成 8 值（`OTHER`→`DEFAULT`，新增 `SUPPLEMENT` / `ACCEPTANCE` / `INVOICE_SCAN` / `BANK_RECEIPT`）。老库插 `SUPPLEMENT` 会撞 `ck_biz_doc_attachment_role`；夹具想在新旧库都能跑，就选 `CONTRACT_SCAN` / `SIGN_RECEIPT`。
5. **`operation_logs.target_id` 是 varchar**（不是 uuid）：`DELETE … WHERE target_id IN ('…'::uuid)` 直接报 db error；按文本比。
6. **`biz_stock_in` 有 `ck_biz_si_sourceless_pair`**（`is_sourceless = (source_order_id IS NULL)`）：造无源入库夹具必须显式写 `is_sourceless = TRUE`，否则 CHECK 失败。
7. **首登强制改密（CODE-06）会拦掉一切业务接口**：新建/初始 admin 的 `must_change_password` 为真，`PUT /api/system/auth/password` 改密后还有 **60s 会话缓存窗口**（`uv:{user_id}`，Redis 60s TTL）；E2E 脚本要么等 70s，要么把「403 且点名权限码」写进断言以区分。

## 八、证据与回滚

- 证据目录：`docs/evidence/20261108-pdf03/`（145 隔离实例 8078 + 库 `cloudpivot_pdf03`，
  样例 PDF / 渲染图 / `pdftotext` 全文 / `pdffonts` / `pdfinfo` / 67 项 E2E 断言输出 / 审计行）。
- 本卡**无迁移**（`biz_contract` / `biz_stock_in/out` / `biz_doc_attachment` 都是既有表）。
- 回滚：删三个 handler 与模块注册即可（无 schema 变更、无权限码变更）。
