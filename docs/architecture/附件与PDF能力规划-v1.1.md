# 云枢附件能力与 PDF 导出 · 补全规划

> 版本 **v1.1** · 2026-11-07（据用户拍板更新）
> 状态：**规划中（等 P0 演练 / P2 采购补齐 / P3 工作流引擎 跑完后再派卡）**
>
> v1.1 变更（用户 2026-11-07 拍板）：
> 1. **PDF 必须「点按钮直接下载文件」** → 改为后端生成（不是前端打印）
> 2. **发票附件规则细化**：非电子票必须扫描件/照片；电子票可用 PDF
> 3. **Office 文档不做在线预览**：只提供下载，由用户本地 Office/WPS 打开

---

## 一、现状盘点（实测）

### 1.1 已有能力（后端通用，不用改）

| 项 | 现状 |
|---|---|
| 附件表 | `biz_doc_attachment(doc_type, doc_id, file_id, file_role, remark, uploaded_by)` |
| 文件表 | `sys_file`（含秒传：内容 hash 命中复用同一条记录） |
| 上传接口 | `POST /attachment/upload`（multipart：doc_type / doc_id / file_role / remark / file） |
| 查询接口 | `GET /attachment/list?doc_type=&doc_id=` |
| 类型枚举 | `DocKind` **103 项**，每一项自带 `exists_sql()` 做单据存在性校验 |
| 配额 | `count_by_doc` 限制单单据附件数 |
| 角色分类 | `file_role` 字段（如 `CONTRACT_SCAN` 合同扫描件） |
| 前端组件 | `DocAttachmentPanel.vue`（通用附件面板，已封装） |

**结论：附件后端能力完整，缺的是前端接入 + 业务规则 + 预览 + PDF 导出。**

### 1.2 前端接入现状（43 个模块，只有 5 个接了）

```
✅ contract 合同 / purchase 采购订单 / payable 应付 / stockin 入库 / expense 报销
❌ 其余 38 个模块完全没有附件区
```

### 1.3 缺口清单

| 编号 | 缺口 | 性质 |
|---|---|---|
| G1 | 附件无法在线预览 | 能力缺失 |
| G2 | 「必须传附件」的业务规则不存在 | 规则缺失 |
| G3 | 38 个模块无附件区 | 覆盖缺失 |
| G4 | 单据/审批/工单**无法下载 PDF** | 能力缺失 |

---

## 二、业务判断：哪些单据「必须传附件」

> 判定口径：**没有这份附件，这笔业务在财务/税务/法务上就不成立**。

### 2.1 🔴 必须传（不传不能提交审批，后端硬校验）

| 单据 | 必传附件 | 格式要求 | 依据 |
|---|---|---|---|
| **发票 invoice** | 发票影像 **≥1** | **见 2.1.1 细分规则** | 税务留存 |
| **收款单 receipt** | 银行回单 | pdf / jpg / png | 对账唯一凭据 |
| **付款单 payment** | 银行回单 / 付款凭据 | pdf / jpg / png | 同上 |
| **费用报销 expense** | 票据 **≥1 张** | pdf / jpg / png / ofd | 无票不能报销 |
| **固定资产 asset** | 购置发票 + 验收单 | pdf / jpg / png | 入账凭据 |
| **采购退货 purchase-return** | 退货单签字件或供应商确认 | pdf / jpg / png | 货已出须有据 |
| **销售退货 sales-return** | 客户确认单或退货照片 | pdf / jpg / png | 防虚假冲减收入 |

#### 2.1.1 发票附件的细分规则（**用户 2026-11-07 明确**）

发票要区分**是否电子票**，两种走不同规则：

```
发票类型（invoice_type / 新增字段 is_electronic）
├── 电子发票（数电票 / PDF 版式 / OFD）
│     允许格式：PDF、OFD、XML（数电票原件）
│     要求：必须上传**原始电子文件**，不接受截图
│     理由：电子票的价值在原件（有 XML/OFD 里带签名），截图无效
│
└── 纸质发票（传统增值税专票/普票）
      允许格式：JPG、PNG、PDF（扫描件）
      要求：扫描件或照片，必须**四角完整、字迹清晰可读**
      建议：上传时提示「请确保发票代码、号码、金额、税额清晰可见」
```

**实现方式**：
- `biz_invoice` 加字段 `is_electronic boolean`
- 附件规则表里对 `INVOICE` 建**两条规则**，按 `is_electronic` 分支：
  ```
  规则A：doc_type=INVOICE, when is_electronic=true  → 允许 pdf/ofd/xml，min_count=1
  规则B：doc_type=INVOICE, when is_electronic=false → 允许 jpg/png/pdf，min_count=1
  ```
- 校验时先读单据字段，再匹配规则

### 2.2 🟡 建议传（可保存，提交时提示「缺附件」但不阻断）

| 单据 | 建议附件 | 说明 |
|---|---|---|
| 合同 contract | 合同扫描件 + 补充协议 | 法务要求，允许后补（签署中） |
| 采购订单 purchase | 供应商报价单 / 客户 PO | 采购依据 |
| 盘点单 stock-take | 盘点表签字扫描件 | **确认盘点结果时**转硬校验 |
| 期初余额 opening | 上年审计报告 / 期初依据 | 上线一次性 |
| 调拨单 transfer | 调拨审批单 | 内部管控 |
| 供应商 supplier | 营业执照、开户许可、资质 | 准入资料 |
| 客户 customer | 营业执照、信用资料 | 授信依据 |

### 2.3 🟢 可选（不强制）

```
销售订单 / 发货单 / 出库单 / 预算 / 折旧 / 期间 / 年结 / 报表
单号 seq / 币种 / 汇率 / 单位 / 库位 / 物料（图片走 sys_file）
```

### 2.4 校验时机（关键：不是"新建时必须"）

```
用户明确：「新建时附件不是必须，可以后面上传」

所以校验点分三档：
  CREATE    新建——只有极少数单据（无）
  SUBMIT    提交审批——发票/收款/付款/报销（硬校验）
  CONFIRM   确认（盘点/退货）——硬校验
  ACTIVATE  激活入账（资产）——硬校验
```

**规则做成配置表，不硬编码**（遵循「流程不硬编码」原则）：

```sql
CREATE TABLE biz_attachment_rule (
  id uuid PRIMARY KEY,
  doc_type      varchar(40) NOT NULL,     -- DocKind
  file_role     varchar(40),              -- 指定角色；NULL = 任意角色都算
  required_at   varchar(20) NOT NULL,     -- SUBMIT / CONFIRM / ACTIVATE
  min_count     int NOT NULL DEFAULT 1,
  max_count     int,                      -- NULL = 不限
  allow_exts    varchar(200),             -- 'pdf,jpg,png' 或 'pdf,ofd,xml'
  condition_sql varchar(300),             -- 可选：附加条件（如 is_electronic=true）
  enabled       boolean NOT NULL DEFAULT true,
  remark        varchar(200)
);
```

---

## 三、附件能力补全计划

### 阶段 A：基础能力（5 张卡，约 12-16 小时）

| 卡 | 内容 | 产出 |
|---|---|---|
| **ATT-01** | **在线预览组件**（图片 + PDF） | `AttachmentPreview.vue` |
| **ATT-02** | 下载 + 权限 | 下载按钮 + 复用数据权限 |
| **ATT-03** | 附件列表增强 | 大小/类型/上传人/时间/软删/排序 |
| **ATT-04** | `DocAttachmentPanel` 角色分类 | 扫描件/补充协议/验收单分栏 |
| **ATT-05** | **Office 文档：下载 + 本地打开引导**（见 3.1） | 图标区分 + 提示文案 |

#### 3.1 Office 文档处理（用户拍板：不做在线预览）

**用户要求**：「office 文档提供下载就行，或者下载后直接调用本地 office/wps 打开」

**技术现实**：
```
浏览器安全限制：网页无法直接唤起本地 Word/WPS 打开服务器的文件
（ms-word: / ms-excel: 协议只在 IE 和特定配置的 Edge 下有效，现代浏览器已废弃）

能做到的最佳体验：
  ① 点击 → 触发下载（浏览器"另存为"或直接落到下载目录）
  ② 下载完成后弹出提示：「已下载，请用本地 Office/WPS 打开」
  ③ 可选：提示里带文件名和保存位置
```

**实现**：
```vue
<!-- Office 类型显示不同的图标和操作 -->
<div class="office-file">
  <IconFileWord />   <!-- docx / doc -->
  <span>{{ file.name }}</span>
  <span class="size">{{ file.size }}</span>
  <Button @click="downloadAndHint(file)">下载</Button>
  <!-- 无"预览"按钮 -->
</div>
```

**类型识别与图标**：
```
doc/docx    → Word 图标
xls/xlsx    → Excel 图标
ppt/pptx    → PPT 图标
wps/et/dps  → WPS 图标
pdf         → PDF 图标（可预览）
jpg/png/... → 缩略图（可预览）
zip/rar/7z  → 压缩包图标（只下载）
其他        → 通用文件图标（只下载）
```

**附：文件大小提示**（防止误传超大文件）
```
上传时前端限制：单文件 ≤ 20MB（可在配置文件调）
超出提示：「文件超过 20MB，请压缩或分卷上传」
后端也要校验（防绕过）
```

### 阶段 B：必传校验（4 张卡，约 10-12 小时）

| 卡 | 内容 |
|---|---|
| **ATT-06** | `biz_attachment_rule` 表 + 后端校验框架（含 `allow_exts` / `condition_sql`） |
| **ATT-07** | 7 个「必须」单据接入硬校验（含发票电子/纸质分支） |
| **ATT-08** | 前端校验提示（缺附件时提交按钮禁用 + 明确说缺什么、允许什么格式） |
| **ATT-09** | 规则管理界面（`/support/attachment-rule`，让管理员能改规则不写代码） |

### 阶段 C：批量接入附件区（2 张卡，约 8-12 小时）

| 卡 | 内容 |
|---|---|
| **ATT-10** | 🔴 7 个必传单据加附件区 |
| **ATT-11** | 🟡 7 个建议单据加附件区 |

**每张卡的模板**（已接入的 5 个就是范例）：
```vue
<DocAttachmentPanel
  :doc-type="DOC_KIND.INVOICE"
  :doc-id="detail?.id"
  :required-roles="['INVOICE_SCAN']"
  :required-at="'SUBMIT'"
  testid-prefix="invoice-attach"
/>
```

---

## 四、PDF 导出计划（**后端生成，直接下载文件**）

> 用户 2026-11-07 拍板：**「要『点按钮直接下载 PDF 文件』」**
> 所以不用前端打印方案，必须后端生成 PDF 流。

### 4.1 要导出的对象

| 对象 | 用途 | 优先级 |
|---|---|---|
| 业务单据（采购订单/销售订单/合同/入库单/出库单/调拨单/退货单） | 打印签字、发客户/供应商、归档 | P1 |
| 审批单（含审批轨迹、每级审批人/时间/意见） | 打印留存、审计取证 | P1 |
| 工单 / 任务单 | 派工单打印 | P1 |
| 报表（资产负债表/利润表/现金流量表） | 财务归档、报送 | P2 |

### 4.2 技术选型（三个方案，我推荐 typst）

| 方案 | 原理 | 优点 | 缺点 | 评价 |
|---|---|---|---|---|
| **A. typst**（推荐） | Rust 原生库，编译进二进制，标记语言排版 | 无外部依赖、单文件部署、中文字体可控、排版能力强、生成快 | 要学 typst 语法、字体文件要打包 | ⭐ **首选** |
| **B. printpdf** | Rust 底层 PDF 库，手写坐标 | 纯 Rust、完全可控 | 表格分页要手算坐标，开发量极大 | 不推荐（太原始） |
| **C. headless Chromium** | HTML → PDF（`--print-to-pdf`） | 模板就是 HTML/CSS，前端能复用、渲染最准 | 要装 Chromium（~300MB）、内存吃、启动慢（每个请求 1-2s） | 备选（若要极致排版） |

**我的建议：typst**。

理由：
```
① 编译进二进制，144/145 不用额外装东西（运维简单）
② 中文支持：嵌入思源黑体/宋体（字体文件打进 repo，约 10-20MB）
③ 生成速度快（毫秒级，Chromium 要 1-2 秒）
④ 模板可版本化（.typ 文件跟着代码走，改样式就是改文本）
⑤ 表格自动分页、页码、页眉页脚都是内置能力
```

**依赖与验证**（派卡时要先做技术验证）：
```toml
[dependencies]
typst = "0.13"          # 或者用 typst-as-lib
typst-pdf = "0.13"
```
```
验证点：
· 中文字体能否正确嵌入（不能显示方框）
· 表格跨页表头是否重复
· 生成的 PDF 能否在 Windows/Mac 预览正常
· 单页生成耗时（目标 < 200ms）
```

### 4.3 接口设计

```
统一模式：GET /api/{module}/{id}/pdf

例：
  GET /api/business/erp/purchase-order/{id}/pdf
  GET /api/business/erp/sales-order/{id}/pdf
  GET /api/business/erp/contract/{id}/pdf
  GET /api/business/flow/instance/{id}/pdf        审批单
  GET /api/business/task/{id}/pdf                 工单
  GET /api/business/erp/report/balance-sheet/pdf  报表

响应：
  Content-Type: application/pdf
  Content-Disposition: attachment; filename="采购订单-PO-20261107-000001.pdf"

权限：复用该单据的「查看」权限（能看才能导）
审计：导出记录写 operation_log（谁在什么时候导了哪张单）
```

**前端按钮**：
```vue
<Button @click="downloadPdf(row.id, 'purchase-order')">
  <IconDownload /> 下载 PDF
</Button>

// 实现（必须带 token，不能直接 window.open）
async function downloadPdf(id, module) {
  const res = await fetch(`/api/business/erp/${module}/${id}/pdf`, {
    headers: { Authorization: `Bearer ${token}` }
  });
  if (!res.ok) { Message.error('导出失败'); return; }
  const blob = await res.blob();
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `${module}-${id}.pdf`;
  a.click();
  URL.revokeObjectURL(url);
}
```

### 4.4 PDF 模板设计要点（每张单据通用）

```
① A4 纵向，页边距 15mm，页眉显示公司名 + 单据类型，页脚显示页码
② 单据头：公司名 / 单据标题 / 单号 / 日期 / 状态（草稿/已审批/已完成）
③ 主表信息：客户/供应商、仓库、经办人、备注
④ 明细表：序号 / 物料编码 / 物料名称 / 规格 / 单位 / 数量 / 单价 / 金额 / 备注
   - 跨页时表头必须重复
   - 金额右对齐，数量右对齐
⑤ 合计行：数量合计 / 金额合计 / 税额 / 价税合计
⑥ 金额大写：人民币「壹万贰仟叁佰肆拾伍元陆角柒分」
⑦ 签字栏：制单 / 审核 / 批准 / 仓管 / 客户签收（留空白手签位）
⑧ 二维码（可选）：扫码可查单据
⑨ 水印：状态为「作废」时加斜向灰色水印
⑩ 附件清单附录（可选）：单据的附件文件名列表
```

### 4.5 卡片拆分（7 张，约 20-26 小时）

| 卡 | 内容 | 估算 |
|---|---|---|
| **PDF-01** | **技术验证 + 基础设施**：typst 接入、中文字体嵌入、通用布局库（页眉页脚/表格跨页/金额大写）、技术验证报告 | 3-4h |
| **PDF-02** | 单据 PDF-1：采购订单 + 销售订单（高频单据，验证模板通用性） | 3-4h |
| **PDF-03** | 单据 PDF-2：合同 + 入库单 + 出库单 | 3-4h |
| **PDF-04** | 单据 PDF-3：调拨单 + 盘点单 + 退货单 | 3-4h |
| **PDF-05** | 审批单 PDF（含审批轨迹时间轴、每级意见、审批人签名位） | 2-3h |
| **PDF-06** | 工单/任务单 PDF（派工单格式） | 2-3h |
| **PDF-07** | 报表 PDF（三大报表，报表布局与单据不同，要单独设计） | 2-3h |
| **PDF-08** | 前端下载按钮统一接入 + 导出审计日志 | 1-2h |

### 4.6 PDF-01 必须先做的技术验证（关键风险）

```
本卡第一步是「可行性验证」，结论决定后续 7 张卡怎么做：

1. typst 0.13 能否在 Rust 1.99 下编译通过？
2. 嵌入中文字体（思源黑体/宋体）后，中文是否正常显示（不是方框）？
3. 表格 500 行跨页，表头是否自动重复？
4. 生成一份 A4 单页 PDF 耗时？（目标 < 200ms）
5. PDF 在 Windows（Adobe/Edge）、Mac（预览）、手机 上是否都能正常打开？
6. 生成的文件大小？（目标 < 200KB/页，否则邮件附件太大）

如果 typst 有硬伤 → 降级到 headless Chromium 方案（要改设计）
```

---

## 五、整体依赖与排期

```
现在：P0-01~06 演练 → P2-01~03 采购补齐 → P3-01 工作流引擎
                    ↓（全部完成后）
        附件与 PDF 阶段
                    ↓
┌─────────────────────────────┴──────────────────────────────┐
│                                                             │
附件（11 张，30-40h）                                    PDF（8 张，20-26h）
  阶段A ATT-01~05  基础（12-16h）                      PDF-01 技术验证（3-4h）
       ↓                                                    ↓
  阶段B ATT-06~09  必传校验（10-12h）                  PDF-02~04 单据（9-12h）
       ↓                                                    ↓
  阶段C ATT-10~11  批量接入（8-12h）                   PDF-05~08 审批/工单/报表/前端（7-10h）
```

**串行理由**：ATT 都改前端共同组件；PDF 都改后端模板库，并发会冲突。

**总计估算：**
- 附件：11 张卡，30-40 小时
- PDF：8 张卡，20-26 小时
- **合计 19 张卡，50-66 小时**

---

## 六、用户已拍板（2026-11-07）

| # | 决策 | 拍板 |
|---|---|---|
| 1 | PDF 导出方式 | **要「点按钮直接下载 PDF 文件」** → 后端生成 |
| 2 | 发票附件 | **非电子票传扫描件/照片；电子票可用 PDF** |
| 3 | Office 文档 | **只提供下载**，由用户本地 Office/WPS 打开（不做在线预览） |

---

## 七、待补充（等 P0 演练暴露后再定）

```
· P0 演练中如果发现「某单据缺了关键附件导致链路走不通」→ 提到必传清单
· P2 采购退货/询比价/采购申请 → 各自附件需求（报价单、退货凭据）
· P3 工作流引擎 → 审批人上传意见附件是否支持
· PDF 模板是否需要随单据状态变化（草稿/终版 watermark）
```
