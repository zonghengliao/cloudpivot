# SO-14 配套：客户档案页「账期天数 credit_days」UI（前端）

卡片：`t_df9ec5e1`（父卡 `t_23278334` SO-14 出库过账 → 应收挂账）
日期：2026-11-06

## 一、做了什么

在**已有**客户档案页上补 `credit_days`（账期天数，`0` = 现结），不新建模块：

| 文件 | 改动 |
|---|---|
| `frontend/src/views/business/erp/customer/customer-query.ts` | `CustomerFormState` 加 `credit_days: number`；`emptyCustomerForm()` 默认 `0`（**不是 30**）；`customerFormFromRow` 回填；`formatCreditDays` / `isCashOnDelivery` / `creditDaysError` / `creditDaysValue` / `CUSTOMER_CREDIT_DAYS_MAX`；列表列在「信用额度」**前**插 `{colKey:'credit_days', title:'账期(天)', width:110, align:'right'}`；新建请求体恒带 `credit_days`，编辑请求体按值比较（只发真改过的） |
| `frontend/src/views/business/erp/customer/index.vue` | 表单「信用额度」下加 `t-input-number`（`name="credit_days"`、`min=0`、`step=1`、`decimal-places=0`、`suffix="天"`、tips「0 = 现结（到期日 = 出库过账日）」）；列表列插槽 `0` 灰显（`muted`）显示「现结」，`>0` 显示「N 天」 |
| `frontend/src/api/business/erp/index.ts` | `Customer.credit_days: number`；`CreateCustomerReq` / `UpdateCustomerReq` 加 `credit_days?: number \| null` |
| `frontend/tests/unit/erp-customer.test.ts` | 列顺序断言加 `credit_days`；新建默认 0；`formatCreditDays` / `isCashOnDelivery` / `creditDaysError` / `creditDaysValue` / 负数额度同款校验 / 编辑「45→0 要发、0→0 不发」共 6 条新用例 |

**没有**详情抽屉：客户页本来就只有编辑弹窗（见 `index.vue` 文件头「为什么没有查看抽屉」），弹窗即详情，账期在里面同步可看可改。

### 文案口径（逐字照后端）
`backend/src/business/erp/customer_handler.rs::require_credit_days`：

```
credit_days 不能为负数（当前 -1）；0 表示现结（到期日 = 过账日）
```

前端 `creditDaysError()` 用同一条文案，另外多拦两件后端会 400 但不说人话的：
非整数（「账期天数必须是整数（按自然日计，0 = 现结）」）、超 `i32`（「账期天数超出可存范围（最大 2147483647）」）。

### 编辑为何是「按值比较」而不是「恒带」
本页的编辑提交一直是**最小变更**（`buildCustomerUpdatePayload` 只发真改过的字段，缺席 = 不改），
这样别人刚改的字段不会被我这台机器的旧值覆盖。账期沿用同一规则：`45 → 0` 会发 `{credit_days: 0}`（`0` 是合法值，不能因假值跳过），
一个字没改就不发 PUT。新建则**恒带** `credit_days`（显式 `0`，免得以后后端默认值变了偷偷改语义）。

## 二、门禁（真跑，输出见同目录）

| 检查 | 命令 | 结果 | 原文 |
|---|---|---|---|
| typecheck | `pnpm typecheck` | **rc=0** | `typecheck.txt` |
| build | `NODE_OPTIONS=--max-old-space-size=4096 pnpm build` | **rc=0**（vue-tsc + vite 全部产物落盘） | `build.txt` |
| 前端单测 | `pnpm test:unit` | **909 passed / 0 failed**（本卡新增 6 条） | `unit-tests.txt` |

## 三、真接口 + 真页面验证（不是 mock）

环境：
```
后端  backend/target/debug/cloudpivot，NIK_PROFILE=test，库 cloudpivot_so14（引擎已到 V0289，biz_customer.credit_days 存在）
      NIK_SERVER_LISTEN=127.0.0.1:18117   /health = ok
前端  pnpm dev（Vite 5173，BACKEND_ORIGIN=http://127.0.0.1:18117，/api 反代真后端）
驱动  headless Chrome（CDP），admin 真账号登录
```

逐条实测（截图见同目录）：

| # | 操作 | 实测 |
|---|---|---|
| 1 | 列表首屏 | 列头出现「账期(天)」，位于「信用额度」之前；老客户（`credit_days=0`）全部显示**灰字**「现结」（`shot-list-cash-muted.png`，1680 宽截全列） |
| 2 | 新建客户 `FE-CUS-CREDIT-1`，账期填 **45** → 创建 | 总数 14 → 15；列表该行「账期(天)」= **45 天**（`class="mono"`，不灰）；库里 `credit_days=45` |
| 3 | 编辑该客户：45 → **0** → 保存 | 列表变 **现结**，单元格 `class="mono muted"`（灰显，与 `shot-list-cash-muted.png` 同款写法）；库里 `credit_days=0` |
| 4 | **整页重载** + 过滤该编码 | 仍显示「现结」→ 不是前端缓存，是真接口读回 |
| 5 | 编辑 0 → **30** → 保存 | 列表「30 天」；库里 30（`shot-list-30days.png`） |
| 6 | 编辑账期填 **-5** → 保存 | 弹窗不关、字段红框 + 字段级错误「credit_days 不能为负数（当前 -5）；0 表示现结（到期日 = 过账日）」、**没发请求**；库里仍是 30（`shot-negative-guard.png`） |
| 7 | 编辑 30 → **45** → 保存 → **整页重载** | 列表「45 天」，`class="mono"`（不灰）；`GET /api/business/erp/customer/page?code=FE-CUS-CREDIT-1` 返回 `credit_days: 45`（`shot-reload-45days.png` / `api-page-credit45.json`） |
| 8 | 清场 | `DELETE /customer/{id}` → 软删成功；库里 `deleted_at` 非空，夹具不污染后续卡 |

后端日志 `ERROR` 行数 **0**（`grep -c ERROR` 结果 0），全程无 5xx。

## 四、复跑

```bash
# 后端（库已在 V0289；如实例已停）
cd backend && NIK_PROFILE=test NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_DATABASE_NAME=cloudpivot_so14 NIK_SERVER_LISTEN=127.0.0.1:18117 \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=<64 hex> CLOUDPIVOT_STORAGE_MASTER_KEY=<64 hex> \
  ./target/debug/cloudpivot

# 前端
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18117 pnpm dev --port 5173
# 登录 admin（该库口令为 SO-14 卡改过的 Admin@123456_so14）→ /business/erp/customer
```

## 五、边界
- 只补「档案维护」；账期参与算 `due_date` 是后端 SO-14（`receivable_bridge`）的事，本卡不动。
- 账期与已有应收无关：已挂应收的 `due_date` 是挂账时快照，改客户账期不回溯（后端决策，见父卡 handoff）。
- 本卡未新增接口 / 权限码，也不需要迁移。
- 仓库里 `frontend/src/api/business/erp/index.ts` 同时有并行卡（应收作废/红冲 `t_b33adc45`）的改动；本卡提交时只 stage 了自己那 3 个 hunk（`credit_days` 相关 9 行），另一卡的改动留在工作区未提交。
