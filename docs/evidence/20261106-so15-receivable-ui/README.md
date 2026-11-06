# SO-15 应收台账前端页面 —— 证据

卡：`t_3145cf38`【SO-15】应收台账前端页面
父卡：`t_23278334`（SO-14 出库过账 → 应收自动挂账）
本轮工作区：`/opt/cloudpivot`（99 工作仓），隔离库 `cloudpivot_so15ui` / `cloudpivot_so15ui2`（引擎迁到 291）

---

## 一、交付物（改了哪些文件 / 函数）

| 文件 | 类型 | 内容 |
|------|------|------|
| `frontend/src/views/business/erp/receivable/index.vue` | **新增**（1288 行） | 台账（双 Tab：台账列表 / 账龄看板）+ 详情抽屉（含核销记录）+ 坏账核销弹窗 |
| `frontend/src/views/business/erp/receivable/receivable-logic.ts` | **新增**（416 行） | 纯逻辑：筛选→入参、状态域/配色、逾期与账龄算术、核销原因校验、留痕拆行、列定义 |
| `frontend/src/api/business/erp/index.ts` | 改（+246 行，1 个 hunk 在文件尾） | 应收 API 契约：`Receivable` / `ReceivableDetail` / `ReceivableQuery` / `ReceivableAging*` 类型 + `fetchReceivablePage` / `fetchReceivableDetail` / `createReceivable` / `writeOffReceivable` / `fetchReceivableAging` |
| `frontend/src/router/index.ts` | 改（+11 行） | 注册 `business/erp/receivable` → `@/views/business/erp/receivable/index.vue`，`meta.permission = business:erp:receivable:list` |
| `frontend/tests/unit/erp-receivable.test.ts` | **新增**（369 行 / 27 条用例） | 纯逻辑单测（状态域、入参映射、核销校验、逾期算术、留痕拆行、列定义/权限码） |
| `backend/migrations/business/V0291__sales_receivable_menu_page.{up,down}.sql` | **新增** | 把 V0288 故意留空的菜单 `path` / `component` / `is_visible` 补成终态（V0288 注释原文交办给本卡），标题收敛成「应收台账」；down 可回滚且不删权限码 |

**没有**改后端业务代码、没有新增 HTTP 接口、没有新增权限码（沿用 SO-13 的 5 个码）；`backend/src/business/erp/*` 的未提交改动是同期「出库作废红冲」卡的在途工作，本卡未触碰、未提交。

### 页面结构

```
应收台账（Tab 1）
├─ 工具栏：刷新 / 状态 / 客户 / 应收单号(ar_no 模糊) / 来源 / 到期日区间 / 查询 / 重置 + 「已筛选」徽标
├─ 列表：ar_no(固定左) · 客户 · 来源单号(+来源 tag) · 应收金额 · 已收 · 未收 · 到期日(+逾期 tag) · 状态 · 操作(固定右)
│        服务端分页（page/page_size），行内「详情」「坏账核销」
├─ 详情抽屉：客户/来源/金额/已收/未收/已收比例/账期/到期日/期间/币种/发票号/关联凭证/核销时间/备注
│            + 「核销记录」面板（从 remark 的 [坏账核销] / [出库作废冲销] 前缀拆行）
└─ 坏账核销弹窗：不可逆警示 + 核销对象摘要(单号/客户/未收/到期) + 原因（≥4 字，实时校验）

账龄看板（Tab 2）
├─ 基准日（空 = 今天 CST）+ 查询
├─ KPI：未收合计 / 逾期合计 / 逾期占比 / 客户·单据
├─ 客户账龄矩阵：表头**按后端 buckets[] 渲染**（未到期 / 1-30 / 31-60 / 61-90 / 90+），合计行带占比条
└─ 单据明细（ar_no / 客户 / 来源单号 / 未收 / 逾期天数 / 档位 tag）+ 后端 hints
```

---

## 二、照抄口径（逐字对后端，不猜）

| 项 | 口径 | 出处 |
|----|------|------|
| 列表行字段 | `ar_no` / `customer_name` / `source_no` / `amount` / `received_amount` / `unsettled_amount` / `due_date` / `status` / `status_label` / `overdue` | `receivable_query::ReceivableRes` |
| 分页过滤键 | `ar_no`（模糊，同时匹配来源单号）、`customer_id`、`status`、`source_type`、`due_date_from/to` | `receivable_query::ReceivablePageQuery` |
| **没有** `keyword` | 传 `keyword` 会被后端静默忽略 → 返回全表（SO-14 curl 踩过），前端一律不发 | 同上 |
| 状态域 | `OPEN / PARTIAL / SETTLED / WRITTEN_OFF / CANCELLED` | `receivable::STATUSES` / `ck_biz_ar_status` / V0290 |
| **不是**应付的域 | 传 `PARTIAL_PAID` → **400**（实测，见 curl §3），绝不静默 | `ReceivablePageQuery::validate` 白名单 |
| 账龄分档 | **表头按响应 `buckets[]` 渲染**，前端不写死 30/60/90（后端加档位自动跟上） | `receivable_aging::ReceivableAgingRes`，与 `payable_aging` 同源 |
| 账龄格金额字段名 | `unpaid`（**不是** `unsettled`）—— 与应付共用同一个结构体 | `payable_aging::AgingBucketAmountRes` |
| 核销原因 | trim 后按**字符数** 4..=500 | `receivable::REASON_MIN_CHARS/MAX_CHARS` |
| 核销留痕 | 后端把原因 **同时**写 `biz_status_log.reason` 与单据 `remark`，一行一条，前缀 `[坏账核销]` / `[出库作废冲销]` | `receivable::write_off` / `cancel_for_stock_out` |
| 逾期判据 | `due_date < 今天` **且** 仍有未收 **且** 状态 OPEN/PARTIAL；到期当天不算逾期 | `receivable_query` + 账龄同口径 |

### 三个刻意的取舍

1. **页面不算账**：账龄分档、汇总、逾期占比、最长逾期天数全部由后端算，前端只格式化（金额走 `shared/money` 的整数分转换，不用浮点累加）。
2. **核销记录按 `remark` 前缀拆行**：`biz_status_log` 没有对外接口，前缀是后端硬契约（`WRITE_OFF_REMARK_PREFIX` / `CANCEL_REMARK_PREFIX`）。SO-17 收款核销明细未上线 ⇒ 面板**据实**显示「收款单 / 核销明细模块尚未上线」，`RECEIPT_ALLOCATION_READY = false` 开关留着，**不伪造收款单号**。
3. **列宽压到 ~1055px**：`ar_no` 固定左 + 操作固定右，中间列超宽会被固定列盖住。实测 1500 视口 `overflow=0`、`statusRight == opLeft == 1277`、`covered=0`（见 §四）。

---

## 三、门禁

```
pnpm typecheck        → rc=0
pnpm test:unit        → 936 tests / 936 pass / 0 fail（本卡新增 27 条）
pnpm build            → rc=0（NODE_OPTIONS=--max-old-space-size=6144，见 §六 坑 1）
裸 hex 检查            → src/views/business/erp/receivable/index.vue 0 命中（全走 var(--td-*)）
:deep() 编译产物       → .ar-tabs[data-v-29f5276d] .t-tabs__content{...}（单冒号生效，无 ::deep 残留）
```

---

## 四、真机 UI 验收（浏览器实测，非打桩）

实例：后端 `backend/target/debug/cloudpivot` @ `127.0.0.1:18120`（库 `cloudpivot_so15ui2`，全新库自动迁移到 V0291）
前端：`BACKEND_ORIGIN=http://127.0.0.1:18120 pnpm dev --port 5298`（源码即产物）
账号：`admin`（首登改密后 `Admin@123456_so15`）；视口 1500×950

| 图 | 验到了什么 |
|----|-----------|
| `shot-1-list.png` | 9 列全部可见（应收单号/客户/来源单号/应收金额/已收/未收/到期日/状态/操作）；5 条真数据；状态中文 tag（未收=蓝、坏账核销=红）；到期日旁「逾期 5 天 / 逾期 45 天」tag；来源列 MANUAL + 「手工挂账」tag；金额千分位；分页「共 5 条数据」；侧边栏短标题「应收台账」 |
| `shot-2-aging.png` | 账龄看板：KPI（未收 5,290.00 / 逾期 3,810.00 / 逾期占比 72.02% / 2 家 4 张）；5 档表头由后端下发；合计行带占比条；客户行；单据明细 4 行含「逾期 45 → 31-60 天」「未到期 20 → 未到期」；后端 hints 原文 |
| `shot-3-detail.png` | 详情抽屉：全部字段 + **核销记录 = 1 条**（`11-06 / 坏账核销 / SO-15 演示：客户已注销，经理批准核销`）+ 收款核销空态文案；「坏账核销」按钮在 WRITTEN_OFF 下置灰 |
| `shot-4-writeoff.png` | 核销弹窗：不可逆警示 + **核销对象摘要**（AR-20261106-000005 / 华东精密制造有限公司 / 未收 1,130.00 / 到期 2026-09-22）+ 原因为空时的红字校验 |

**交互实测（真点，非只读）**：在 `AR-20261106-000006`（未收 500）上打开核销弹窗 → 填原因 → 点「确认核销」→ 列表该行状态即时变「坏账核销」，弹窗关闭（第一轮实例 `cloudpivot_so15ui` 上执行，日志见 §五 同链路接口断言）。

**几何断言**（`overflow = scrollWidth - clientWidth`）：

```
overflow=0  statusRight=1277  opLeft=1277  covered=0     ← 固定操作列没压住状态列
行高 49px；操作列两按钮同一行（sameLine=true）
```

---

## 五、接口实测（真实 token curl，连跑两轮）

脚本：`curl-e2e.sh`（58 条断言，自带清场，可重放）

```bash
BASE=http://127.0.0.1:18120/api DB=cloudpivot_so15ui2 bash docs/evidence/20261106-so15-receivable-ui/curl-e2e.sh
```

```
第一轮：==> 汇总：PASS=58 FAIL=0  （rc=0）   → curl-round1.txt
第二轮：==> 汇总：PASS=58 FAIL=0  （rc=0）   → curl-round2.txt
```

逐段覆盖（摘录第一轮真回执）：

```
登录 admin                                    → access_token
POST /customer {credit_days:30}               → 客户 id
POST /receivable ×2（1130 到期 -30 天 / 500 到期 +10 天）→ AR-…000005 / AR-…000006
GET  /receivable/page?customer_id=…           → total=2 / page=1 / page_size=20
       行字段齐全：ar_no · customer_name · source_no · amount · received_amount
                  · unsettled_amount · due_date · status · status_label · overdue
       customer_name 回填「SO-15 台账演示客户」（LEFT JOIN 生效）
       source_type=MANUAL · source_no=MANUAL（uk_biz_ar_source 的唯一例外）
       status_label=未收（中文由后端下发，前端只兜底）
       30 天前到期 → overdue=true；10 天后到期 → overdue=false
GET  …/page?status=PARTIAL_PAID               → HTTP 400（应付状态域传进来立刻暴露）
GET  …/page?due_date_from=…&due_date_to=…     → total=1（到期日区间过滤）
GET  /receivable/{id}                         → amount=1130 / received=0 / unsettled=1130
                                               voucher_no 键存在且为 null（收入确认凭证属 SO-22）
GET  /receivable/aging?as_of=今天              → as_of_source=QUERY / buckets=5
                                               keys=NOT_DUE,D0_30,D31_60,D61_90,OVER_90
                                               summary 与 buckets 同序同长
                                               D0_30=1130（逾期 30 天落 1-30 档）· NOT_DUE=500
                                               detail_count=2 · bucket_label=1-30 天
PATCH /receivable/{id}/write-off {reason:短}   → 10001（原因过短被拒，与前端校验同口径）
PATCH /receivable/{id}/write-off {reason:…}    → status=WRITTEN_OFF / status_label=坏账核销
                                               settled_at 落值 · amount 不变（只改状态不冲金额）
PATCH 再来一次                                  → 10003（重复核销业务错误，非 200）
GET  /receivable/{id}                         → remark 含 [坏账核销] + 原因（前端面板的数据源）
GET  /receivable/aging?as_of=今天（核销后）      → D0_30=0 · detail_count=1（不再计入应收资产）
库层：biz_receivable 该客户 2 张（1 张已核销）· biz_status_log to_status=WRITTEN_OFF 1 条
库层：sys_menu …2880 path/component/is_visible 已终态 · schema_migrations v291 versioned/executed/true
```

### 迁移（V0291）双向验证

全新库 `cloudpivot_so15ui2` 由后端**自动迁移**到 291，`schema_migrations` 记录：

```
versioned|288|sales_receivable_permissions|executed|true
versioned|289|sales_customer_credit_days|executed|true
versioned|290|sales_receivable_cancelled|executed|true
versioned|291|sales_receivable_menu_page|executed|true
```

`sys_menu …2880` → `path=/business/erp/receivable | component=business/erp/receivable/index | is_visible=true | title=应收台账`，4 个按钮（2881~2884）父级完好。

down / up 各跑一次（`psql -v ON_ERROR_STOP=1 -f`，在 `cloudpivot_so15ui` 上）：

```
down → NOTICE: V0291 down 完成：应收台账页已关闭（path/component=NULL，is_visible=FALSE），4 个按钮与 5 个权限码保留
       查询 → NULL|NULL|false|应收账款（销售链台账：…）
up   → NOTICE: V0291 完成：应收台账页已开（path=/business/erp/receivable component=business/erp/receivable/index），本次改 path 1 行；4 个按钮父级完好
       查询 → /business/erp/receivable|business/erp/receivable/index|true|应收台账
```

---

## 六、踩坑（下一张镜像卡直接用）

1. **`pnpm build` 默认堆 1.5G 会 OOM**：`FATAL ERROR: Reached heap limit`（`vite build` 阶段，不是 ts 类型错）。
   `NODE_OPTIONS=--max-old-space-size=6144 pnpm build` → rc=0。机器上 concurrently 跑 cargo 时必现。
2. **两端固定列（`fixed:'left'` + `fixed:'right'`）会把中间列压在滚动区外**：首版列宽合计 1390，1500 视口下「状态」整列被操作列盖住，截图看着像没这一列。
   判据只能用 `holder.scrollWidth - holder.clientWidth` + `statusRight vs opLeft`，肉眼在带横向滚动的表里看不出。
   修法是把 9 列压到 ~1055（`ar_no` 150 / 客户 112 / 来源 142 / 金额 95 / 已收 85 / 未收 95 / 到期 160 / 状态 92 / 操作 150）。
3. **`t-alert` 的 `#description` 插槽在 `message` 也给了时不渲染**：核销弹窗原来把「核销对象摘要」放在 alert 的 description 里，真机上整块不出现（不可逆动作看不到「核销哪一张」）。
   改成 alert 下面独立的摘要块（`writeoff__target`）才是稳的。
4. **`t-textarea` 带 `maxlength` 会自带右下角计数**：自己再画一个「N / 500」就是双份。留组件自带的即可。
5. **`sys_menu.title` 就是侧边栏显示名**（不是 `name`）：V0288 里那句长说明会让侧边栏被截成「应收台账（销售链应收…」。本卡 V0291 收敛成「应收台账」，说明文案放列表卡右上角 `.panel-head__tip`（与 V0263 客户档案同口径）。
6. **卡面写「照搬 payable/index.vue 的账龄组件」与实际不符**：`payable/index.vue` 里**没有**账龄组件，账龄在独立页 `views/business/erp/report/payable-aging/index.vue`。本卡按那个页的矩阵/KPI/明细三段式做进 Tab，而不是硬找 payable 页。
7. **curl 连跑多轮会踩登录限流 `10005`**：脚本已内建 25s 退避重试（抄 SO-14 脚本）。
8. **`0.0000` 与 boolean 的断言写法**：`received_amount` 是 `"0.0000"`，`jq -r tonumber` 仍输出 `0.0000`（要 `| floor`）；`psql -Atc` 拼 `success` 布尔得到 `true` 而不是 `t`。两条都曾让脚本假红。

---

## 七、复跑方法

```bash
# 1) 干净库（owner 必须是 alun）
runuser -u postgres -- psql -qc "CREATE DATABASE cloudpivot_so15ui2 OWNER alun"

# 2) 后端（自动迁移到 291）
cd /opt/cloudpivot/backend
NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_so15ui2 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_SERVER_LISTEN=127.0.0.1:18120 NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  setsid ./target/debug/cloudpivot > /tmp/so15ui2_server.log 2>&1 &

# 3) 接口两轮
BASE=http://127.0.0.1:18120/api DB=cloudpivot_so15ui2 \
  bash docs/evidence/20261106-so15-receivable-ui/curl-e2e.sh

# 4) 前端（源码即产物，不碰 dist）
cd /opt/cloudpivot/frontend
BACKEND_ORIGIN=http://127.0.0.1:18120 pnpm dev --port 5298 --host 127.0.0.1
# admin / Admin@123456_so15（脚本首登改密后的口令）→ http://127.0.0.1:5298/business/erp/receivable
```

## 八、边界（未做，已明确）

- **收款单核销明细**（`biz_receipt_allocation`）属 SO-16 / SO-17，未上线 ⇒ 详情的「核销记录」只列**状态留痕**（坏账核销 / 出库作废冲销），收款核销据实空态。SO-17 落地后把 `RECEIPT_ALLOCATION_READY` 翻 true 并接明细接口。
- **手工挂账入口**（`POST /receivable`，权限 `:create`）卡面没要求，本卡只保留 api 层函数与类型，页面未放入口（挂账主路径是出库过账自动挂）。
- 出库作废 → 应收红冲的**冲销记录**已能显示（前端按 `[出库作废冲销]` 前缀拆），其写入链路由并行卡 `t_b33adc45` 负责。
