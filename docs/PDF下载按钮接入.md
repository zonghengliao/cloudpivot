# 单据「下载 PDF」按钮（PDF-08 统一接入）

> 后端接口与版式见同目录的 `PDF单据模板-*.md` / `PDF报表模板-财务报表.md`。
> 本文只讲**前端这一层**：按钮放在哪、为什么只有一份实现、加新入口要改什么、踩过的坑。
> 实测证据：`docs/evidence/20261108-pdf08/`（145 真 token curl 165 断言 + 真 Chromium 71 断言）。

## 1. 一句话

所有单据 / 审批 / 任务 / 报表的「下载 PDF」都走同一个组件 `DocPdfButton`，
它内部只有一条链路：**fetch 带 `Authorization: Bearer` → blob → 从 `Content-Disposition`
取中文名 → 造临时链下载 → 释放 URL**，失败按状态码翻成人话（403 = 无权限）。

```vue
<DocPdfButton
  :path="`/api/business/erp/purchase-order/${detail.id}/pdf`"
  :filename="`采购订单-${detail.order_no}.pdf`"
  testid="purchase-detail-pdf"
/>
```

## 2. 为什么不用 `window.open` / `<a href>` / axios

| 写法 | 结果 |
|------|------|
| `window.open(url)` / `<a href>` 直开 | **带不上 `Authorization` 头** → 后端必 401；把 token 拼进 query 会进浏览器历史 / 代理日志 / Referer |
| axios（`responseType: 'blob'`） | 能用，但错误体是 Blob，要自己按 `content-type` 判断再解 JSON，且 `loading` / 文件名 / `revokeObjectURL` 会在每页重写一遍 |
| **`fetch` + Bearer（本仓做法）** | 与后端二进制接口语义一一对应：`res.ok` 决定拿 blob 还是读错误体；`Content-Disposition` 直接给中文名 |

## 3. 三个文件（改一处，13 个入口一起生效）

| 文件 | 职责 |
|------|------|
| `frontend/src/utils/doc-pdf.ts` | 纯函数：`filenameFromContentDisposition`（只信 `filename*=`）、`sanitizeFileName`、`docPdfErrorMessage` |
| `frontend/src/composables/useDocPdfDownload.ts` | `downloading` + `downloadDocPdf(path, fallbackName)`：fetch、错误体解析、`saveBlob` |
| `frontend/src/components/DocPdfButton.vue` | 按钮外观 + loading + 成功/失败 `MessagePlugin` |

### 3.1 文件名为什么只认 `filename*=`

后端 `pdf::handler::content_disposition` 恒发：

```
attachment; filename="____-PO-1.pdf"; filename*=UTF-8''%E9%87%87%E8%B4%AD%E8%AE%A2%E5%8D%95-PO-1.pdf
```

ASCII 回退名把每个中文字换成了 `_`（`采购订单-PO-1.pdf` → `____-PO-1.pdf`）。
所以解析顺序是：**先 `filename*=`（RFC 5987 百分号编码 decode）**；
朴素 `filename=` 只在**不含 `_`** 时可信（纯英文名那类）。取不到就用调用方给的
`filename` 回退名 —— 宁可名字朴素，也不能让用户存成 `____-PO-1.pdf`。

### 3.2 错误文案

| HTTP | 文案 |
|------|------|
| 401 | 登录已过期，请重新登录后再导出 |
| 403 | 没有导出该单据 PDF 的权限：<后端 msg>（无 msg 时「请联系管理员分配」） |
| 404 | 单据不存在或已被删除 |
| 5xx | PDF 渲染服务暂时不可用，请稍后重试 |
| 其它 | 后端 msg，没有就「导出失败（HTTP xxx）」 |
| 网络异常 | 下载失败：<原因>，请重试 |

按钮在失败后自动恢复可点，**用户再点一次就是重试**（提示里也写了「可重试」）。
403 **不隐藏按钮**：藏起来用户只会以为功能没做，看不到「缺哪个权限码」。

## 4. 13 个入口与它们的路径 / 权限

| 页面 | 路径 | 权限码 |
|------|------|--------|
| 采购订单详情 | `GET /api/business/erp/purchase-order/{id}/pdf` | `business:erp:purchase-order:list` |
| 销售订单详情 | `GET /api/business/erp/sales-order/{id}/pdf` | `business:erp:sale-order:list` |
| 合同详情 | `GET /api/business/erp/contract/{id}/pdf` | `business:erp:contract:list` |
| 收货入库详情 | `GET /api/business/erp/stock-in/{id}/pdf` | `business:erp:stock-in:list` |
| 出库单详情 | `GET /api/business/erp/stock-out/{id}/pdf` | `business:erp:stock-out:list` |
| 调拨单详情 | `GET /api/business/erp/stock-transfer/{id}/pdf` | `business:erp:transfer:list` |
| 盘点单详情 | `GET /api/business/erp/stock-take/{id}/pdf` | `business:erp:stock-take:list` |
| 采购退货详情 | `GET /api/business/erp/purchase-return/{id}/pdf` | `business:erp:purchase-return:list` |
| 销售退货详情 | `GET /api/business/erp/sales-return/{id}/pdf` | `business:erp:sales-return:list` |
| 审批实例详情 | `GET /api/business/flow/instance/{id}/pdf` | `business:flow:instance:read` |
| 任务详情（派工单） | `GET /api/business/task/{id}/pdf` | `business:task:read` |
| 资产负债表 / 利润表 / 现金流量表 | `GET /api/business/erp/report/{type}/pdf?period=yyyy-MM` | `business:erp:report:export`（与 xlsx 导出同码） |

> **单据类 PDF 与「查看」同码**（能看见单据就一定能导出，不另立 `:pdf` 权限点）；
> 报表是例外：页面码 `business:erp:report:{balance-sheet|income-statement|cash-flow}`、
> 导出码 `business:erp:report:export` —— 这才有「看得见按钮但点下去 403」的真实场景，
> 也是 403 提示的证据来源（`sample/ui-403-toast.png`）。

## 5. 加一个新入口要改哪几处

1. 后端先有 `GET …/pdf`（见 `PDF导出基础设施.md` §8 与各 `PDF单据模板-*.md`）；
2. 详情页模板里放一个 `<DocPdfButton :path="…" :filename="…" testid="…-detail-pdf" />`
   （`path` **必须带 `/api` 前缀**，漏了会命中 SPA fallback 拿回 HTML）；
3. `frontend/tests/unit/pdf08-doc-pdf.test.ts` 的 `PAGES` 数组加一行（页面契约：
   组件被引入、路径片段、testid 都在）—— 忘写这行时单测不会提醒，但下一张卡会；
4. 若该单据是 GET 导出，**后端务必显式写 `operation_logs`**（中间件只记
   POST/PUT/PATCH/DELETE），否则「谁导出了哪张单」查不到。

## 6. 坑（都踩过）

| 现象 | 原因 | 处理 |
|------|------|------|
| 下载按钮点了没反应 / 控制台 200 但文件是 HTML | `path` 漏 `/api` 前缀，命中 SPA fallback | 路径写全 `/api/business/...` |
| 存成 `____-PO-1.pdf` | 用了 `filename=`（ASCII 回退名） | 只信 `filename*=`，取不到用回退名 |
| Safari 下偶尔空文件 | `URL.revokeObjectURL` 立即执行 | `saveBlob` 里延后 1s |
| 明明有权限却 401 | 用了 `window.open`，带不上头 | 统一走 `DocPdfButton` |
| 报表页按钮灰着点不动 | 期间没选中（`:disabled="!sheet"`） | 先选期间等重算 |
| E2E 点不到「查看」 | TDesign `fixed` 列把操作列 DOM 复制了一份 | 逐个点，直到详情里的按钮出现（`browser_check.mjs` 的 `openDetailUntil`） |
| E2E 页面被遮罩挡住 | 「首次启动向导」「新功能引导」 | `localStorage`：`cloudpivot.setup-wizard.skipped:<用户>=1`、`cp-onboarding-done=1` |
