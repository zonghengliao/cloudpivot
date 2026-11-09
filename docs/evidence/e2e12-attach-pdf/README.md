# E2E-12 · 附件 + PDF 域演练证据（145 / cloudpivot_dev）

> 卡：`t_ff24670f`（E2E-12 附件+PDF域：上传/预览/下载/权限/必传闸门 + 8 类 PDF 导出）
> 环境：145 测试机 · 后端 `127.0.0.1:8033`（systemd `cloudpivot-test`，profile=dev）
> 库：`cloudpivot_dev`（PG，允许写演练数据）· 账号 `admin`
> 时间：2026-11-09 CST
> **未连 144 生产；未 DROP / TRUNCATE；未改已应用迁移；只操作 `E2E12 / e2e12_` 前缀数据**
> 证据口径：全部真实 HTTP（urllib，无第三方库）+ psql 直查 + pdftotext/pdfinfo/pdffonts/pdftoppm
> 断言总数：**383 PASS / 0 FAIL**（A 段 131 + B 段 252）

## 演练范围

| 段 | 覆盖功能点 |
|---|---|
| S0 环境 | admin 登录、PDF 渲染服务 `/business/pdf/health`（Chromium 版本 / 并发）、规则表 9 条 |
| S1 锚点单据 | 真实 API 建 30 行采购订单（`PO-20261109-000083`，价税合计 16864.62）→ 供 PDF 跨页验收 |
| S2 上传 | 图片 / PDF / Office(docx) / 压缩包 4 类真实 multipart 上传 → 落 `sys_file`+`biz_doc_attachment`；**内容秒传**（同字节复用 `sys_file`、绑定幂等）；**扩展名黑名单**（`.exe` 400）；**20MB 双端闸**（21MB → 400+10001，正好 20MiB 放行）；非法 doc_type / file_role / 单据不存在（400/400/404） |
| S3 列表 | 字段齐全（大小 / MIME / 上传人姓名 / 时间 / sort_order / can_delete）、时间倒序、`sort=manual` 手动排序（PUT 重排 + 回读一致 + 重复 id / 缺行 400）、角色分栏（含空分组，前端据此显示「暂无，点击上传」） |
| S4 下载/预览 | 4 类文件下载 200、Content-Type 精确、`X-Content-Type-Options: nosniff`、RFC5987 `filename*`、中文名可解、**字节与上传完全一致**、`%PDF-` 头；下载留痕 `operation_logs.DOC_ATTACHMENT_DOWNLOAD`（detail 含 doc_type/doc_id/file_id/file_name/file_size） |
| S5 权限 | 自建角色（attachment list/upload + contract list，**故意不给** purchase-order）→ 越权下载 403 + 文案点名单据类型；幽灵 id 404（存在性先于权限，探测面已留档）；越权上传 403；**同一账号对合同附件 200**（非一刀切）；缺 `business:attachment:list` → 路由 403 且点名权限码 |
| S6 删除 | 软删绑定行（行仍在可审计）+ 独占文件随之软删、列表立即消失、重复删除 404、**上传人删自己的不看 delete 码**、删别人附件 403 |
| S7 规则管理 | `/support/attachment-rule`：列表 / options / 新建 / 修改 / 启停 / 软删；**停用规则 → check 立即放行、重启 → 缺口回来（无缓存）**；非超管 403；非法 doc_type / required_at 400；`condition_sql` 注入（`1=1; drop table …`）400 |
| S8 必传闸门 | 8 条规则（INVOICE 电子/纸质两分支、RECEIPT、EXPENSE_REPORT、FIXED_ASSET、STOCK_TAKE、PURCHASE_RETURN、SALES_RETURN、PAYMENT）逐条 `check` 负例（`can_proceed=false` + 缺口白名单 + 人话红字）；**真实动作**：自有盘点单 `DRAFT → PENDING_POST` 被 **400「必须上传盘点表」** 拦下（DB 状态未变）→ 补 PDF → check 放行 → 提交通过 |
| S9 PDF 导出 | **14 个导出入口全部 200**：采购订单 / 销售订单 / 合同 / 入库单 / 出库单 / 调拨单 / 盘点单（空白手写栏）/ 采购退货 / 销售退货 / 审批单 / 派工单 / 资产负债表 / 利润表 / 现金流量表 |
| S10 PDF 权限+审计 | 只有 `purchase-order:list` 的角色导单据 PDF 200、**报表 PDF 403 且点名 `business:erp:report:export`**、合同 PDF 403；`operation_logs` 12 个 `EXPORT_*` 操作码全部落库（username/target_type/target_id 非空） |

## 结果总览

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| S1 | `POST /business/erp/purchase-order`（30 行） | 200 | `biz_purchase_order` + 30 行明细 | 通畅 |
| S2 | 上传 pdf/png/docx/zip | 200×4 | `sys_file` 4 行 + 绑定 4 行，`uploader_name` 快照非空 | 通畅 |
| S2 | 同字节换角色再传 | 200（`duplicated=true`，绑定行 id 相同） | 未新增绑定行 | 通畅（响应角色回显见 P4-1） |
| S2 | `.exe` 上传 | **400**（硬禁止黑名单） | 未落库 | 通畅 |
| S2 | 21MB / 正好 20MiB | **400+10001** / 200 | 超限未落库、边界落库 20971520 B | 通畅 |
| S3 | `GET /attachment/list` | 200 | 5 条、时间倒序、roles 含空分组 | 通畅 |
| S3 | `PUT /attachment/sort` | 200 | `sort_order 0..n-1`，`sort=manual` 回读一致 | 通畅 |
| S4 | `GET /attachment/download/{id}` ×4 | 200 | 下载留痕 18 行（含 S5/S6 复测） | 通畅 |
| S5 | 越权下载 / 越权上传 | **403 / 403** | 无写入 | 通畅 |
| S5 | 幽灵 id 下载 | **404** | — | 通畅（与 403 文案不同，见 P4-2） |
| S6 | 删除 + 重删 + 上传人自删 | 200 / **404** / 200 | 绑定行 `deleted_at` 有值、独占文件同步软删 | 通畅 |
| S7 | 新建/改/停用/启用/删除规则 | 200×6 | `biz_attachment_rule` 落库、软删留痕 | 通畅 |
| S7 | 停用规则后 check | 200 | `can_proceed=true`（**无缓存，立即生效**） | 通畅 |
| S8 | 8 条规则负例 check | 200×7（SALES_RETURN 库内仅 1 单已挂附件→skip） | — | 通畅 |
| S8 | 盘点 `DRAFT→PENDING_POST`（无附件） | **400**「必须上传盘点表或盈亏说明」 | 状态仍 `DRAFT` | 通畅（后端闸门） |
| S8 | 补 PDF 后提交 | 200 | `PENDING_POST` + 附件 1 | 通畅 |
| S9 | 14 个 PDF 入口 | 200×14 | `operation_logs` 12 个 `EXPORT_*` | 通畅 |
| S10 | 报表 PDF（无 export 码） | **403** | 无导出留痕 | 通畅 |

## 勾稽验证

| # | 勾稽项 | 结果 |
|---|---|---|
| 1 | 采购单明细行数 | API 建 30 行 → DB `biz_purchase_order_item` = 30 ✅ |
| 2 | 价税合计三处一致 | API `total_with_tax=16864.6200`；PDF 文本层大写「壹万陆仟捌佰陆拾肆元陆角贰分」= 16864.62 ✅（脚本里 float 逐行累加得 16864.63，是 Python 舍入口径差异，非数据差异） |
| 3 | 绑定行 ↔ `sys_file` 悬空 | `dangling_binding=0` ✅ |
| 4 | 软删附件与其物理文件状态 | 软删绑定 1 行、其文件仍活 0 行（独占文件同步软删）✅ |
| 5 | 分栏计数 vs 列表 | CONTRACT_SCAN=1 / SIGN_RECEIPT=1 / SUPPLEMENT=1 / DEFAULT=2，合计 5 = 列表 total ✅ |
| 6 | `uploader_name` 快照（AUD-01） | 5 行附件 `uploader_name` 空值 0 ✅ |
| 7 | 留痕三类操作码 | UPLOAD=37 / DOWNLOAD=18 / DELETE=6（含本轮复测） ✅ |
| 8 | PDF 导出审计 | 12 种操作码 × 目标表正确（`EXPORT_PURCHASE_ORDER_PDF→biz_purchase_order` 等），15 行全部有 username/created_at ✅ |
| 9 | 闸门与状态机 | 无附件提交 400 且 `biz_stock_take.status` 仍 `DRAFT`（未半截写入）✅ |
| 10 | 发票两分支规则 | 表内 `is_electronic IS TRUE`（pdf,ofd,xml）/ `IS FALSE`（pdf,jpg,png）两条并存；电子票单据 check 命中 `ofd` ✅ |

## PDF 质量（真实文本层 + 字体嵌入 + 视觉取证）

- **中文不乱码**：14 份 PDF 的 `pdffonts` 均显示 `NotoSansSC CID TrueType … yes yes yes`（字体已嵌入）、`pdftotext` 文本层含中文。
- **跨页表头重复**：30 行采购订单 2 页，`pdftotext -f 2 -l 2` 第 2 页仍印「物料编码 物料名称 规格 单位 数量 单价 金额 备注」。
- **金额大写**：`壹万陆仟捌佰陆拾肆元陆角贰分`（= 16864.62）。
- **签字栏 / 页脚**：文本层含「制单」「共 N 页」；首页 `pdftoppm -r 100` 出图 `raw/purchase-order-p1-1.png`。
- **盘点单留空手写栏**：文本层含「实盘」「差异」列名（数量列留空，模板结构上不填）。
- **审批单**：含审批轨迹文本（`FLW-20261109-…`，1 页）。
- 样例文件全部在 `raw/`（14 份 PDF + 各份 `*.txt` 文本层 + 1 张 PNG）。

## 问题清单

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|---|---|---|---|---|
| P4-1 | **内容秒传命中时，上传响应回显「本次请求」的 file_role，而库内绑定的仍是首行角色** | P4 | 同一份字节先以 `CONTRACT_SCAN` 上传，再以 `ACCEPTANCE` 上传 → 响应 `file_role=ACCEPTANCE`，`biz_doc_attachment.file_role` 仍是 `CONTRACT_SCAN` | 前端若用响应体就地刷新列表，会短暂显示错误分栏；重查列表即正确。审计行也记的是请求角色 | `attachment/handler.rs` 在 `insert_with_uploader` 后回读绑定行（`model::find_active(binding_id)`），响应用**实际落库角色**；顺带让 `upload_log` 同源。1 文件小改 |
| P4-2 | 附件下载统一 `Content-Disposition: attachment`，服务端没有 inline 预览口径 | P4 | `GET /business/attachment/download/{id}` 看响应头 | 不影响功能：前端预览走 fetch blob + pdf.js / 图片组件；但「预览 vs 下载」完全由前端决定，接口契约没体现 | 若要服务端支持：给可预览类型（image/pdf）加 `?disposition=inline`，或另开 `/preview/{id}`。注意别改默认值（下载语义） |
| P4-3 | 幽灵附件 id 与「存在但无权」的响应不同（404 vs 403） | P4 | 用随机 uuid 下载 → 404；用真实 id 下载（无读码）→ 403 | 理论上可探测「某 id 是否是一条附件绑定行」。UUID v7 枚举不可行，风险可接受 | 已在代码注释留档（400→404→403 顺序）；如需彻底一致可对无权者一律 404 |
| P4-4 | 演练环境样本限制：`SALES_RETURN` 库内仅 1 张单且已挂附件，无法用「零附件真实单据」验负例 | P4 | `check?doc_type=SALES_RETURN` 找不到零附件样本 | 该条负例改由「规则表存在 + 规则停用/启用立即生效」间接覆盖；不是产品缺陷 | 需要时由测试卡自己建一张销退单（本卡未越界造别家单据） |
| 说明 | 规则 `PAYMENT` 的 `required_at` 是 `ACTIVATE` 而非卡面 `SUBMIT` | — | 迁移 V0347 头注释 | 付款记录在提交动作内部创建，SUBMIT 时无单据可挂附件，挂 SUBMIT 会把付款永久锁死 | 设计取舍，已在迁移头留档，本卡确认行为正确（`check?at=ACTIVATE` 命中） |

> 注：P4-1 未在本卡改代码（会动共享文件 `attachment/handler.rs` 且需重编 8033 二进制；本卡零业务改动）。建议下张卡带「响应与库内角色一致性」单测一起修。

## 修复记录

本卡**未修改业务源码**（发现的问题全为 P4 语义/契约级，均已在上表给出 1 文件级修法）。
交付物为演练脚本 + 证据：

```
scripts/verify/e2e12_lib.py           公共库（HTTP / multipart / psql / 造文件 / PDF 取证）
scripts/verify/e2e12_a.py             A 段 S0–S6（附件全链）
scripts/verify/e2e12_b.py             B 段 S7–S10（规则 / 闸门 / PDF / 权限审计）
scripts/verify/e2e12_db_evidence.sh   DB 层证据 + 10 组勾稽
scripts/verify/e2e12_cleanup.sh       按外键顺序只清 E2E12 前缀
docs/evidence/e2e12-attach-pdf/       断言日志 / 原始响应 / 14 份 PDF / 文本层 / PNG
```

## 历史问题回归（ATT / PDF / AUD）

| 编号 | 回归点 | 结果 |
|---|---|---|
| ATT-01 存储策略白名单 | `attachment` 分类 27 种扩展名，`.exe` 硬禁止 | 通过（400 文案点名黑名单） |
| ATT-02 读权限 + 下载留痕 | 越权 403 / 留痕字段完整 | 通过 |
| ATT-03 手动排序 | `PUT /sort` + `sort=manual` 回读、非法入参 400 | 通过 |
| ATT-04 上传/删除写路径收口 | 越权上传 403；上传人自删 200；删他人 403 | 通过 |
| ATT-05 20MB 双端闸 + Office 只下载 | 21MB→400+10001、20MiB 放行、docx 下载字节一致 | 通过 |
| ATT-06/07 必传规则 + 发票分支 | 8 条规则 check、电子/纸质两分支条件 | 通过 |
| ATT-08 新单据类型联动 | 12 类 doc_type 可选可存（规则 options + check） | 通过 |
| ATT-09 规则管理界面 | CRUD + 启停 + 软删 + 非超管 403 | 通过 |
| ATT-10 闸门四件套 | 自有盘点单真实 400 → 补附件放行 | 通过 |
| ATT-11 面板分栏 | roles 含空分组、count 与列表一致 | 通过 |
| PDF-01 渲染底座 | `/business/pdf/health` 200、Chromium 可读 | 通过 |
| PDF-02 订单 | 采购/销售订单 200，合计/大写/表头/签字 | 通过 |
| PDF-03 合同+出入库 | 合同/入库/出库 200，字体嵌入 | 通过 |
| PDF-04 调拨/盘点/退货 | 调拨、盘点（空白手写栏）、采退、销退 200 | 通过 |
| PDF-05 审批单 | 审批轨迹文本、签字栏 | 通过 |
| PDF-06 派工单 | 任务单 200、中文正常 | 通过 |
| PDF-07 三大报表 | 资产负债表/利润表/现金流量表 200（`period=<voucher max>`） | 通过 |
| PDF-08 下载按钮契约 | 服务端 `filename*=UTF-8''` + 中文名可解 | 通过（前端契约见 `pdf-download-button-cards.md`，本卡不做浏览器） |
| AUD-01 uploader_name 快照 | 5 行附件 `uploader_name` 空值 0 | 通过 |

## 结论

**全部通畅**（383 断言 0 失败）：附件全链（上传/秒传/闸门/列表/排序/分栏/下载/预览字节/权限/软删）与 PDF 全链（14 个入口、中文不乱码、跨页表头、金额大写、签字栏、权限 403、12 类导出审计）均按卡面通过；发现 4 条 P4（1 条秒传角色回显、1 条 inline 预览契约、1 条 404/403 探测面、1 条环境样本限制），无 P0–P3。

演练数据自清：`po=0 st=0 att=0 file=0 role=0 user=0 rule=0 po_item孤儿=0`（`cleanup_after.out`）。
