# SO-21 销售发票前端页面（证据）

日期：2026-11-06　｜　卡：`t_02a188d2`　｜　后端：SO-20（`t_16ccd7b6`，提交 `b396a09`，`backend/src/business/erp/invoice.rs` 4 接口）

## 1. 交付内容（文件 / 函数）

| 文件 | 性质 | 说明 |
|---|---|---|
| `frontend/src/views/business/erp/invoice/index.vue` | **新增 1441 行** | 列表（9 列 / 服务端分页 / 4 个过滤）+ 开票弹窗（选应收 → 自动带出金额与额度）+ 详情抽屉 + 开票 / 归档 / 作废 |
| `frontend/src/views/business/erp/invoice/invoice-logic.ts` | **新增 461 行** | 纯逻辑：状态域 / 边表 / 额度口径 / 税额拆分 / 表单与原因校验 / 列定义（被单测钉住） |
| `frontend/src/api/business/erp/index.ts` | 改（+165 行） | `InvoiceStatus` / `InvoiceType` / `Invoice` / `InvoiceDetail` / `InvoiceQuery` / `CreateInvoiceReq` + `fetchInvoicePage` / `fetchInvoiceDetail` / `createInvoice` / `changeInvoiceStatus` |
| `frontend/src/router/index.ts` | 改 | 路由 `business/erp/invoice`（name `BusinessErpInvoice`，permission `business:erp:invoice:list`） |
| `frontend/src/views/business/erp/receivable/index.vue` | 改 | 深链 `?ar_no=`（发票页点「关联应收」跳过来即筛出该单）；只读 query，不带参数不动用户筛选 |
| `frontend/tests/unit/erp-invoice.test.ts` | **新增 475 行 / 21 条单测** | 域值 / 边表 / 额度 / 税额拆分 / 表单与原因校验 / 查询参数 / 列宽 / **后端源码逐字契约** |
| `backend/migrations/business/V0297__sales_invoice_menu_page.{up,down}.sql` | 新增 | 把 V0296 故意留空的 `path/component/is_visible` 补成终态（`…2895` 销售发票页开出来），带幂等 + 自检 + 回滚 |

页面能力 ↔ 卡面要求：

| 卡面要求 | 落地 |
|---|---|
| 列表 invoice_no / 客户 / 开票日期 / 金额 / 税额 / 状态 + 分页 | 9 列（另有 关联应收 / 价税合计 / 操作），`page_size=20` 服务端分页 + 状态 / 票种 / 单号模糊 / 开票日期区间 4 个过滤 |
| 开票弹窗：选择应收 → 自动带出金额 | 选应收后发一次 `GET /invoice/page?receivable_id=…`，**用真实发票算「已开票价税合计」**，额度 = 应收金额 − 已开票（排除 VOID），价税合计缺省 = 剩余额度；面板同时显示 应收金额 / 已开票 / 剩余额度，并按税率实时预览「不含税 + 税额」 |
| 作废按钮（带确认） | 行内「作废」→ 弹窗：对象摘要 + 影响提示（ISSUED 说明会红冲收入凭证并撤回应收发票号；DRAFT 说明不动账）+ 原因（≥4 字，前端先拦）；`POSTED` 没有作废边，按钮不出现，草稿/已开票才有 |
| 显示关联应收单号（可点击跳转） | 列表「关联应收」列 + 详情抽屉「应收单」，点击 `router.push('/business/erp/receivable?ar_no=…')`，应收页深链自动填筛选并只列这一张 |
| 路由注册 | 见上表（并配套 V0297 把菜单节点开出来，侧边栏可见） |

## 2. 关键口径与取舍

1. **状态域照后端 `invoice::STATUSES`（DRAFT / ISSUED / POSTED / VOID）**，不是应收的 `OPEN/PARTIAL`，也不是收款的 `CANCELLED` —— 抄错不是编译错而是接口 400，单测因此把域值 + 后端源码逐字钉住。
2. **边表 4 条**（`INVOICE_EDGES`）：`DRAFT→ISSUED`（开票，生成收入凭证 + 回写应收）/ `ISSUED→POSTED`（归档）/ `DRAFT/ISSUED→VOID`（作废，必给原因）。**没有 POSTED→VOID，也没有回 DRAFT** —— 所以 `POSTED` 行不渲染作废按钮，前端也不提供「改回草稿」。
3. **「金额自动带出」不硬编成应收全额**：同一张应收常要分次开票（或已部分红冲），直接填全额第二次必然 409。所以填的是**剩余可开票额度**，且用真实发票列表算出（与后端 `invoiced_total` 同口径：`status <> 'VOID'`）。
4. **提交只发 `total_with_tax`**，不含税 / 税额由后端 `split_tax` 定标 4 位算出；前端那份「不含税 + 税额」只是**预览**，并在面板上写明「权威金额以后端 4 位定标结果为准」。整数分运算保证预览里 `不含税 + 税额 ≡ 合计`。
5. **「创建后立即开票」默认勾选**：开票的业务动作是 DRAFT → ISSUED（凭证 + 回写），只建草稿等于让用户再点一次。若第二步失败，草稿已落库 → 提示「已建草稿，但开票没成功：<原因>，可在列表里重试」，而不是报「失败」让用户重开一张。
6. **列表的「关联应收」单号在前端解析**：`InvoiceRes`（列表行）只下发 `receivable_id`，单号在 `InvoiceDetailRes` 才有。为不改 SO-20 已 curl 验证的 4 接口契约，本页按当前页 distinct 的 `receivable_id` 调 `GET /receivable/{id}` 补单号（一页 ≤20 个，结果缓存跨页复用；`undefined` = 解析中显示 `…`、`null` = 应收已删）。**若后端愿意在列表 DTO 里补 `receivable_no`，这段可以整块删掉**（已在代码注释里写明）。
7. **时间按北京时间显示**：后端 `created_at` 是 `DateTime<Utc>`，直接切字符串会早 8 小时（实测踩到），详情抽屉里统一 +08:00 后再格式化。

## 3. 门禁实测

### 3.1 前端

```
node v22.23.4 / pnpm 10.5.0
pnpm typecheck            → 通过（vue-tsc --noEmit 无输出）
pnpm build                → exit 0（见 frontend-build.log）
pnpm test:unit            → tests 985 / pass 985 / fail 0（新增 21 条全绿）
pnpm check:palettes       → 全部通过 ✓（22 套方案 × 明暗）
```

### 3.2 真机（真后端 + 真 dist + 真库，两轮连跑）

环境：隔离实例 `127.0.0.1:18123`（debug 二进制 2026-11-06 20:59，库 `cloudpivot_so20`），
`/opt/cloudpivot/frontend/dist` 由 `serve-dist.mjs`（静态 + `/api` 反代 18123）托到 `:5299`，
Playwright Chromium 1600×1000，令牌由 `POST /api/system/auth/login` 现取。

```
round-1-verify.log → PASS=53 FAIL=0
round-2-verify.log → PASS=53 FAIL=0   （同一库连跑第二轮）
```

覆盖面（每条都是 DOM / 接口回执断言，不靠截图）：
① `/business/erp/invoice` 直达 + 9 列表头 + 「共 N 条」；
② 选 AR20-001（应收 1130 / 已开票 0）→ 已开票 `0.00`、额度 `1,130.00`、价税合计自动 `1130`、预览 `1,000.00 + 130.00 = 1,130.00`；「创建后立即开票」开关在首屏可见；
③ 选已开满的 AR20-003 → 额度 `0.00`，提交被前端拦住并说「这张应收已经没有可开票额度了」；
④ 提交 → `POST /invoice` 200（DRAFT、`IV-20261106-…`）→ `PATCH status=ISSUED` 200（凭证号 `PZ-…`、不含税 1000 / 税额 130 / 合计 1130、应收单号 AR20-001）→ 抽屉自动打开显示「已开票 + 凭证号 + 额度 0.00」；
⑤ 作废：弹窗点名单号/客户/金额，ISSUED 提示「红字凭证 + 撤回应收发票号」；原因填 2 字被拦；填 ≥4 字 → `PATCH status=VOID` 200 → 列表行变「已作废」、应收链接仍在；
⑥ 点「关联应收」→ 跳到 `/business/erp/receivable?ar_no=AR20-001`，筛选条预填、**列表只剩 1 张且不含 AR20-002**；
⑦ DRAFT 路径：取消「立即开票」→ 只建草稿（DRAFT + 「开票」按钮）→ 草稿作废提示「账上没动、不产生凭证」→ `DRAFT→VOID` 200；
⑧ 除全站既有 `notice/stream` 401 外 0 个 4xx/5xx，0 条 `console.error`。

### 3.3 迁移 / 库层（`boot-and-migration.txt` / `db-effects.txt`）

```
versioned | 297 | sales_invoice_menu_page | executed | success=t | 12ms
sys_menu …2895 → path=/business/erp/invoice  component=business/erp/invoice/index  is_visible=t  title=销售发票
页面下 3 个按钮仍在；schema_migrations 共 225 行
```

真机两轮在库里留下的痕迹：

```
PZ-20261106-000009  收入凭证（3 行分录：1 借 1122 应收账款 1130 / 2 贷 5001 主营业务收入 1000 / 3 贷 2221.02 应交税费-销项税 130）
PZR-20261106-000003…12  每次 UI 作废各生成一张红字凭证（3 行分录）
借贷不平的凭证数 = 0
应收 AR20-001 / AR20-002 的 invoice_no / invoice_date 作废后回到 NULL（AR20-003 仍是 000008）
额度恢复：AR20-001 1,130 / AR20-002 565 / AR20-003 0（已开满）
biz_status_log(INVOICE) = 51 条
```

`GET /health ×2` → 200（两轮）。

### 3.4 n08 同规则自查（`menu-router-check.txt`）

不跑 cargo 的等价检查：`sys_menu(menu_type=2)` 81 条 path 全部能在 `frontend/src/router/index.ts` 里解析，
`MISSING = 0`；本卡的 `/business/erp/invoice` 库侧 / 路由侧都 ✓。

## 4. 复跑方法

```bash
# 1) 起隔离后端（库 cloudpivot_so20 保留着 SO-20 的夹具：3 张应收 + 演示客户）
cd /opt/cloudpivot/backend && NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_so20 \
  NIK_SERVER_LISTEN=127.0.0.1:18123 NIK_MIDDLEWARE_AUTH_JWT_SECRET=<64hex> \
  CLOUDPIVOT_STORAGE_MASTER_KEY=<64hex> ./target/debug/cloudpivot &

# 2) dist + 反代
cd docs/evidence/20261106-so21-invoice-ui && node serve-dist.mjs /opt/cloudpivot/frontend/dist 5299 18123

# 3) 真机两轮（令牌现取）
ln -sfn /opt/cloudpivot/frontend/node_modules node_modules   # ESM 按脚本所在目录解析包
node verify-ui.mjs     # 期望 PASS=53 FAIL=0（ADMIN_PASS=... 可覆盖口令）
```

## 5. 已知边界

- 两轮都是对 `cloudpivot_so20`（一次性隔离库）跑，跑完留下的 `biz_invoice` 全是 `VOID`（每轮自己开、自己作废）；
  夹具应收的额度会因作废而恢复，所以能反复跑。
- 列表补 AR 单号是**按页 N+1**（≤20 次 `GET /receivable/{id}`，带缓存）。当前种子规模无感；
  应收台账上千后可让后端把 `receivable_no` 补进列表 DTO 后删掉这段（见 §2.6）。
- 手工开票（无应收）本页只做**展示 + 跳转**，不提供入口：卡面要求「选择应收 → 自动带出金额」，
  手工开票的入口在应收页「手工挂账」之后（后端 `CreateInvoiceReq` 支持 `customer_id`，接口已封装好，页面按需可加）。
