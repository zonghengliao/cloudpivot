# 单据 PDF 模板：采购订单 / 销售订单（PDF-02）

> 底座见 [PDF导出基础设施.md](./PDF导出基础设施.md)（Chromium CDP 常驻服务、字体、错误码、14 项技术验证）。
> 本文只讲**业务单据**这一层：接口、权限、模板结构、金额口径、页码与审计，以及「再加一种单据要改哪几处」。

## 一、接口

| 方法 | 路径（`/api` 前缀由 `[router].prefix` 注入） | 权限码 | 说明 |
|------|---------------------------------------------|--------|------|
| GET | `/business/erp/purchase-order/{id}/pdf` | `business:erp:purchase-order:list` | 采购订单 PDF |
| GET | `/business/erp/sales-order/{id}/pdf` | `business:erp:sale-order:list` | 销售订单 PDF |

**权限复用「单据查看权限」**，不另立 `:export` 码 —— 能看见这张单的人就能打印它；另立码会造出
「列表看得见、点导出 403」的哑弹，还要同步迁移 / 角色矩阵 / 前端按钮三处。同一先例：
`GET /purchase-order/{id}/pending-items` 也挂 `:list`。

响应是**二进制**（不是 `{code,msg,data}`）：

```
200 application/pdf
Content-Disposition: attachment; filename="采购订单-PO-20261108-000001.pdf"; filename*=UTF-8''…
Cache-Control: no-store
X-Content-Type-Options: nosniff
```

前端必须 `responseType: 'blob'`；失败分支按 `content-type` 判断是 JSON 错误体。

## 二、代码落点

| 文件 | 职责 |
|------|------|
| `backend/src/business/pdf/templates/order.html.tera` | A4 版式（`include_str!` 编译进二进制，运行期零文件依赖） |
| `backend/src/business/pdf/template.rs` | `OrderDocKind` / `OrderContext` / `sample_order` / `render_order` / `order_footer` / `order_pdf_opts`；金额格式化（`yuan_to_fen` / `format_quantity`） |
| `backend/src/business/erp/order_pdf.rs` | 两个 GET 入口 + 上下文组装（`purchase_order_context` / `sales_order_context`）+ 审计编排；纯函数 `build_items` / `compose_remark` 可单测 |
| `backend/src/business/erp/order_pdf_model.rs` | 批量补名字（物料 / 单位 / 用户）+ 写 `operation_logs` |

**分层口径**：`business/pdf` 不读业务数据（模板 + 渲染参数）；业务侧自己查库、自己组装上下文，
然后两步 `render_order` → `engine::render_pdf`。加新单据时**只动业务侧 + 新增模板**，不碰引擎。

## 三、版式与打印约束（改模板前必读）

```
页眉：公司名 + 「采 购 订 单」
单据头：单号 / 日期 / 状态 / 供应商(客户) / 仓库 / 经办人   ← 每行 3 组，meta_rows 已切好
明细：# / 物料编码 / 物料名称 / 规格 / 单位 / 数量 / 单价 / 金额 / 备注
合计：行数合计 / 数量合计 / 金额合计 / 税额 / 价税合计 / 币种
大写：人民币…（￥…）
签字栏：制单 / 审核 / 批准 / 仓管 / 客户签收（5 个空白手签位 + 日期线）
页脚：第 X 页 / 共 Y 页（CDP footerTemplate）
```

四条不能动的打印约束（缺一条就踩坑）：

1. `@page { size: A4 portrait; margin: … }` —— 不写按 Chromium 默认 Letter 出纸；
2. `thead { display: table-header-group }` —— 跨页自动重复表头（验收②靠它）；
3. `tr / .summary / .sign { break-inside: avoid }` —— 单行、合计行、签字栏不被腰斩；
4. `.num { font-variant-numeric: tabular-nums; text-align: right }` —— 金额等宽 + 右对齐，小数点逐位对齐。

**列宽**：物料编码列必须 `white-space: nowrap`（实测 `PDF02-MAT-001` 在 62pt 下会折成两行，
逐行核对时看不清 → 已改 82pt + nowrap）。

## 四、页码：为什么走 CDP 的 footerTemplate

`@page` 计数器在 Chromium 的 `printToPDF` 下不生效，HTML 里也拿不到总页数。只有
`printToPDF` 的 `footerTemplate` 认 `.pageNumber` / `.totalPages` 两个内建类，且必须
`displayHeaderFooter = true`。两个硬要求：

- 页脚模板**必须显式写 `font-size`**（Chromium 页脚默认字号 0，不写就是一条看不见的页脚）
  与 `font-family`（中文字体别指望继承正文）；
- 下边距要留够（本模板 `bottom_mm = 16`），否则页脚会压到正文。

`order_pdf_opts()` 已经把这几项封好，业务侧只需要 `order_pdf_opts(kind, doc_no, Some(filename))`。

## 五、金额口径（与库里的 `NUMERIC(18,4)` 对上）

| 写法 | 实现 | 说明 |
|------|------|------|
| 千分位两位小数 | `format_money(yuan_to_fen(x))` | 元 → 分用 `RoundingStrategy::MidpointAwayFromZero`（**四舍五入**，不是 Decimal 默认的银行家舍入） |
| 数量 / 单价 | `format_quantity(x)` | 最多 4 位小数、去尾零、千分位；**不许**用 `format_int`（0.5 米会被打成 0 米） |
| 人民币大写 | `amount_upper(fen)` | 与对账单共用同一份实现；大写与小数两种写法出自**同一次** `yuan_to_fen`，逐位自洽 |
| 外币 | `currency_cn = ""`、`currency_symbol = "USD "` | 别把美元写成「人民币」 |

单头 `total_*` 是**服务端算好落库的权威值**（逐行先舍入再汇总），本模块只读不重算。

## 六、审计（`operation_logs`）

中间件的操作日志只记 POST/PUT/PATCH/DELETE，导出是 GET —— 不显式写就一点痕迹都没有。
导出成功后补一行（**异步**，写失败只记 error 日志、不回滚已生成的 PDF）：

```
operation   = EXPORT_PURCHASE_ORDER_PDF / EXPORT_SALES_ORDER_PDF
target_type = biz_purchase_order / biz_sales_order
target_id   = 单据主键
detail      = {doc_type, doc_id, doc_no, row_count, total_with_tax, filename, bytes, duration_ms, source:"PDF-02"}
request_method = 'GET'，status_code = 200
```

## 七、再加一种单据（例如入库单 / 发票）要改哪几处

1. `pdf/templates/<doc>.html.tera`：复制 `order.html.tera`，**只改 `.sheet` 内部结构**，四条打印约束不动；
2. `pdf/template.rs`：加 `include_str!` 常量 + 注册名（`.html` 后缀才会启用 HTML 自动转义）+ `render_<doc>()`
   + 上下文结构体（金额字段一律是**已格式化的字符串**，模板不做计算）；
3. 业务模块新建 `xxx_pdf.rs`：`pub async fn export_xxx(id) -> Result<RenderedOrder, ApiError>`（组装上下文 → 渲染），
   再套一层 `#[nik::permission(…, permission = "business:erp:<模块>:list")]` 的 GET handler；
4. 复用 `pdf::handler::pdf_response`（四个响应头 + `%PDF-` 校验）与 `business/pdf` 的 `engine::render_pdf`；
5. 审计：复用 `order_pdf_model::write_export_log`（`OrderDocKind` 加一个枚举值 + 表名 + 操作码）；
6. 真库用例照 `erp/order_pdf.rs::db_tests` 的写法（`ERP_DB_URL` 开关，夹具用独占 UUID 命名空间）。

## 八、坑（已踩过，别重复）

- **导出接口是 GET，操作日志中间件不会记录** —— 必须显式写审计行；
- 绑参：`Value::Text` 绑 `?::numeric` / `?::inet` / int 列会报 `error serializing parameter`
  或 `cannot bind parameter to column type`，一律写 `?::text::numeric` / `?::text::inet` / `?::text::int4`；
  `uuid` 列直接 `Value::from(&Uuid)` + `?::uuid`；回查 `inet` 列要 `ip::text`；
- `pdftotext` 抽取时 `letter-spacing` 与相邻 span 会变成多个空格（「第 1 页 / 共 3 页」抽成「第1页/共3页」），
  断言要按**去空白**形态比对，否则假红；
- 145 上 admin **首登强制改密**，且 `must_change_password` 有 60s 会话缓存：改密后 60s 内所有业务接口
  都返回 403「首次登录必须先修改初始密码」，看起来像权限问题；
- 起隔离实例时 **cwd 必须是含 `config-dev.toml` 的目录**（如 `/opt/cloudpivot/backend`），否则 DB 段读不到，
  启动会在 `nik-web/src/resources.rs` 以「数据库未初始化」panic。
