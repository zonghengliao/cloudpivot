# 单据 PDF 模板（三）：调拨单 / 盘点单 / 采购退货单 / 销售退货单

> 卡：`PDF-04`。前两份是 [PDF单据模板-采购销售订单.md](./PDF单据模板-采购销售订单.md)（PDF-02）与
> [PDF单据模板-合同与出入库单.md](./PDF单据模板-合同与出入库单.md)（PDF-03）；
> 底座（Chromium / 字体 / `business/pdf` 分层）见 [PDF导出基础设施.md](./PDF导出基础设施.md)。
> 本文件只讲「这一批四种单据怎么落」，以及**本批新踩到的坑**（PDF-03 的坑仍适用，不再重复）。

## 一、接口与权限

| 方法 | 路径 | 权限码（复用单据查看码） | 版式 | 说明 |
|------|------|--------------------------|------|------|
| GET | `/api/business/erp/stock-transfer/{id}/pdf` | `business:erp:transfer:list` | A4 纵向 | 调拨单：调出 / 调入仓库 + 双库位 + 成本口径 |
| GET | `/api/business/erp/stock-take/{id}/pdf` | `business:erp:stock-take:list` | A4 纵向 | **盘点表**：实盘 / 差异 / 备注三栏留空手写 |
| GET | `/api/business/erp/purchase-return/{id}/pdf` | `business:erp:purchase-return:list` | **A4 横向** | 采购退货：原入库单 + 采购订单 + 退货原因 |
| GET | `/api/business/erp/sales-return/{id}/pdf` | `business:erp:sales-return:list` | **A4 横向** | 销售退货：原销售订单 + 发货单 + 退货类型 + 序列号 |

- **权限复用单据查看码**，不新种 `:export`（理由同 PDF-03：能看就能打，否则详情可见、导出 403）。
- 响应：`200 application/pdf`，`Content-Disposition: attachment`，中文名走 RFC 5987；
  `Cache-Control: no-store`；前端取件必须 `responseType: 'blob'`。
- 单号不存在 / 已软删 → 404 并**点名单据类型**；`id` 非法 → 400 + `66006`。

## 二、代码落点

```
backend/src/business/pdf/
  templates/transfer.html.tera     调拨单
  templates/stock_take.html.tera   盘点表（★ 唯一带空白手写栏的模板）
  templates/return_doc.html.tera   采购退货 / 销售退货共用（按 kind 开关专有列）
  template.rs                      TransferContext / StockTakeContext / ReturnContext
                                   + 渲染 + 页脚 + PdfOpts + 样本数据
backend/src/business/erp/
  transfer_pdf.rs                  调拨单：取数 + 渲染 + HTTP 入口 + 审计
  stock_take_pdf.rs                盘点表：取数 + 渲染 + HTTP 入口 + 审计
  return_pdf.rs                    两种退货单：取数 + 渲染 + 两个 HTTP 入口 + 审计
  doc_pdf_render.rs                PDF-04 起的新公共件：HTML → 字节 + 文件名 + 行数 + 金额 + 耗时
  doc_pdf_model.rs                 PDF-03 的公共数据层（本卡**新增** `location_dict_labels_of`）
```

分层不变：`business/pdf` 不读业务数据；业务侧查库 → 组上下文 → `render_*` → `engine::render_pdf`。

## 三、盘点表为什么敢「留空」

卡面把「必须留实盘数量 / 差异 / 备注 空白栏」定为验收①。落地方式不是「模板里写三个空 `<td>` 然后祈祷没人填」，
而是**在类型层面堵死这条路**：

```rust
pub struct StockTakeItem {
    pub seq, material_code, material_name, spec, unit, location: String,
    pub book_qty: String,   // ← 只有账面快照
    // ← 没有 actual_qty / diff_qty / remark 三个字段
}
```

模板想印系统实盘数也拿不到数据。库里已有的 `actual_qty`（`PENDING_POST` / `POSTED` 的单子）只在**合计区**
以「盘盈 / 盘亏 / 差异金额」出现，且**仅当确实有差异时**才打印 —— 全 0 的「盘盈 0 / 盘亏 0」会让人误以为
这张表已经盘过了。

单测的硬判据（`template.rs::renders_sample_stock_take_with_blank_columns`）：

```rust
html.matches("<td class=\"w-actual blank\"></td>").count() == 行数
html.matches("<td class=\"w-diff blank\"></td>").count()   == 行数
html.matches("<td class=\"w-note blank\"></td>").count()   == 行数
```

再加一条「空白格后面不许紧跟数字」，防止有人后端塞值。列高 22pt（现场笔迹写得下），表头底色用浅橙区分。

## 四、调拨单：三个容易搞错的字段

| 字段 | 坑 | 本卡口径 |
|------|-----|----------|
| 调出 / 调入仓库 | 卡面验收②点名，但只写在单据头容易漏看 | 单据头两个字段 **+** 顶部路线条「调出：X ➜ 调入：Y（在途经 在途仓）」；草稿（无在途仓）不印「在途经」 |
| **单位** | `biz_stock_transfer_item` **没有** `unit_id` 列（V0140 只到物料 / 库位 / 数量 / 成本） | 取物料主档 `biz_material.unit_id` → `biz_unit.unit_name`；明细连表带出的编码 / 名称优先 |
| 金额 | 调拨是**原价转移**，没有税额三档 | 只印「成本金额合计」+ 人民币大写；模板里不出现「价税合计」字样（单测钉死） |

库位：明细 `from/to_location_id` 指向**库位账行**（`biz_inventory_location.id`），`None` 显示「未分配」（真实语义）。

## 五、退货单：两种口径**不同**，别合并取数逻辑

| 项 | 采购退货单 | 销售退货单 |
|----|-----------|-----------|
| 原单号（验收③） | `stock_in_no`（原入库单）+ `purchase_order_no`（原采购订单） | `biz_sales_order.so_no` + `biz_shipment.ship_no` |
| 退货原因（验收③） | `reason`（NOT NULL，库层拦空白） | `return_type`（QUALITY / WRONG_ITEM / OTHER）+ 备注 |
| 明细 `location_id` 指向 | **库位账行** `biz_inventory_location.id`（与出入库明细同口径） | **库位字典** `biz_location.id`（`NULL` = 整仓） |
| 行级价税合计 | 有（`with_tax_amount`） | **没有**（只存 `amount` / `tax_amount`）→ 打 `—`，合计区仍给准确三档 |
| 序列号 | 无 | `serial_nos`（一物一码，顿号分隔） |
| 版式 | A4 **横向** | A4 **横向** |

`doc_pdf_model` 里两个函数专治这个差别：

```rust
location_labels_of(&ids)        // 入参 = 库位账行 id（biz_inventory_location.id）
location_dict_labels_of(&ids)   // 入参 = 库位字典 id（biz_location.id）← 本卡新增
```

混用会把「A-01-03」印成别的库位（或「已删除的库位账行」），是**静默错数据**，不是显式报错。

## 六、审计

`GET` 不被操作日志中间件记录，导出成功后显式补一行（失败只 `tracing::error`，不改响应）：

| operation | target_type | detail 关键字段 |
|-----------|-------------|-----------------|
| `EXPORT_STOCK_TRANSFER_PDF` | `biz_stock_transfer` | `doc_type=STOCK_TRANSFER`, `row_count`, `amount`, `render_ms` |
| `EXPORT_STOCK_TAKE_PDF` | `biz_stock_take` | `doc_type=STOCK_TAKE`, `row_count`, **`diff_amount`**（盘点单的「金额」是差异） |
| `EXPORT_PURCHASE_RETURN_PDF` | `biz_purchase_return` | `doc_type=PURCHASE_RETURN`, `row_count`, `amount` |
| `EXPORT_SALES_RETURN_PDF` | `biz_sales_return` | `doc_type=SALES_RETURN`, `row_count`, `amount` |

`detail.source` 固定 `PDF-03`（PDF 导出**子系统**的标记，不是卡号；PDF-04 沿用同一通道，日志页一筛即得）。

## 七、再加一种单据要改哪几处

1. `pdf/templates/<doc>.html.tera` + `template.rs` 顶部 `include_str!` 常量 + `tera()` 里注册；
2. `template.rs`：`XxxContext` / `XxxItem` / `XxxContextArgs`（**展示字符串**，模板不做计算）+ `render_xxx` + `xxx_footer` + `xxx_pdf_opts` + `sample_xxx`；
3. `erp/<doc>_pdf.rs`：取数（批量补名）→ 上下文 → `render_xxx` → `doc_pdf_render::render_document` → handler（`#[nik::permission]` + `#[nik::get]`）→ 审计；
4. `erp/mod.rs` 注册模块（`rustfmt` 会重排位置）；
5. 单测：结构面（关键文字 / 列 / 空白格 / 页脚 / 金额同源）+ 真库用例（`ERP_DB_URL` 未设则 SKIP）；
6. 证据：145 隔离实例 + 真实 token + `pdftotext / pdfinfo / pdffonts` + 视觉取证。

## 八、本批新踩到的坑

1. **夹具 UUID 命名空间必须避开别的卡**。本卡最初用 `0f05xx`，与 PDF-03 的 `seed145.sql` 撞车 →
   `biz_material_serial.location_id` 外键把两边的夹具串在一起（清理时互相 FK 阻塞）。
   现用 `0f41`（调拨）/ `0f42`（盘点）/ `0f43`（采购退货）/ `0f44`（销售退货），
   业务码也带模块段（`PDF04-TK-*` / `PDF04-ST-*` / `PDF04-RT-*` / `WH-PDF04-*`）。
2. **`purge()` 要按业务码清，不能只按 id 清**。命名空间一旦改过，老行还在库里：
   `uk_biz_unit_code` / `uk_biz_material_code` 这类唯一约束会拦住重新种入。
   现在的 purge 对主数据用 `LIKE 'PDF04-TK-%'` 等前缀清，对库位（通用码）才按 id 清。
3. **`purge()` 连主数据一起删**：一个测试里「造数 → 清 → 再造」必须**重新 `seed_master()`**，否则第二次 INSERT 撞外键。
4. **删库位 / 仓库 / 物料之前先删 `biz_material_serial`**：序列号行同时引用 `material_id` / `location_id` / `warehouse_id`（都有硬外键）。
5. **`PdfOpts.landscape` 才是横竖向的唯一开关**：`prefer_css_page_size` 默认 false，模板里的 `@page { size: A4 landscape }`
   不会覆盖后端参数 —— 横版必须在 `*_pdf_opts` 里设 `landscape = true`（模板 `@page` 只是浏览器直接打印时的兜底）。
6. **明细没有单位列**的单据（调拨 / 销售退货）单位只能从物料主档补，查不到打 `—`（别留空串）。
7. **测试模块里声明了不用的常量会触发 `dead_code` 告警**（clippy `-D warnings` 会红）。夹具 id 只在 SQL 字面量里用时，
   写注释列出命名空间即可，不要逐个声明常量。
8. **`table-layout: fixed` 只认首行（`thead th`）的宽度**：给 `td` 写 `width` 在 auto 布局下会被内容重算，
   在 fixed 布局下则**完全无效**（列宽均分）。要么给 `<th>` 也加同一套 `.c-*` 类（本卡做法），
   要么用 `<colgroup>`；否则「加宽了序列号列」是假的，`pdftotext` 里号码照样被劈开。
9. **tera 仍会转义半角 `/` 和引号**（PDF-03 的坑继续适用）：文案里用全角「／」，单测断言按转义结果写。
