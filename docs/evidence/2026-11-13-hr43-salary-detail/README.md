# HR-43 薪资单详情页（四段式 + 明细展开 + 凭证卡）— 实测证据

卡：`t_dc3985e8` · 设计：`docs/architecture/ARCH-9-人力资源模块设计.md` §6.1 / §6.2.5 · 依赖：HR-36（已完成）

## 1. 交付内容

| 文件 | 说明 |
|---|---|
| `frontend/src/api/business/hr/salary-kinds.ts` | **纯逻辑**：状态词表（6 档）、动作矩阵（按状态算）、权限码、上限常量、明细查询串、部门汇总求和 / 与单头对拍、凭证卡构造、三张表的列字典。不 import 请求层，可被 `tsx --test` 直接加载 |
| `frontend/src/api/business/hr/salary.ts` | HTTP 层：`GET /salary/{id}` · `GET /salary/{id}/detail/page` · `PUT /salary/{id}/detail/{detail_id}` · `POST /submit` · `POST /post` · `POST /reverse` · `GET /export`（blob） |
| `frontend/src/views/business/hr/salary/detail.vue` | 四段式页面：单头卡（含吸顶 `DocActionBar`）→ 按部门汇总（含合计行 + 明细项按类型合计）→ 员工明细（**可展开**成薪资项表，可改单项金额）→ 审批轨迹（`FlowProgress`）→ 凭证卡（计提 / 发放 / 红冲）→ 附件（`DocAttachmentPanel`） |
| `frontend/src/router/index.ts` | 注册页面级路由 `business/hr/salary/:id`（不进菜单，权限 `business:hr:salary:detail`） |
| `frontend/tests/unit/hr-salary-detail.test.ts` | 19 条单测（读 Rust 源码对拍词表 / 权限码 / 路由 / 上限 / 锁表） |

## 2. 静态门禁（原始输出）

```
$ cd frontend && pnpm typecheck
> vue-tsc --noEmit
（无输出，退出码 0）

$ cd frontend && pnpm build
✓ built in 51.95s
dist/assets/detail-DP9GGIka.js  54.77 kB │ gzip: 18.70 kB

$ cd frontend && npx tsx --test tests/unit/hr-salary-detail.test.ts
1..19
# tests 19
# pass 19
# fail 0
```

## 3. 浏览器实测（本地 `dist` + mock 后端，1440×1000）

`dist` 是真构建产物；`/api/*` 由本机临时 mock 服务（约 300 行 Node stdlib）返回固定 JSON，
**不连任何库、不改生产 dist**（mock 脚本不进仓库）。三张截图分别是三种状态。

| 截图 | 状态 | 实测 DOM 断言（原文） |
|---|---|---|
| `00-head-and-summary.png` | `POSTED`（已过账） | 单号 `SAL-202611-000001`、状态 `已过账`、应发 `9,154.03`、实发 `7,354.03`；部门表 2 行 + **合计行**（`dept-total-row`：`2 / 9,154.03 / 1,680.00 / 960.00 / 7,354.03`）；明细项按类型 `Σ 收入 9,889.66 · Σ 代扣 1,800.00 · Σ 单位成本 2,640.00 · 明细项 30 条` |
| `01-detail-expanded.png` | `POSTED`，点「展开全部」 | `hr-salary-items-table` 从 0 → 1 张；展开项 10 行，逐行有 `金额 / 算法 / 说明`：`BASE 基本工资 收入 固定值 8,000.00 标准月薪 8000`、`OT_WD 平时加班费 收入 公式 689.66 8000/21.75*2`、`ATT_DEDUCT 考勤扣款 应发前扣减 735.63 8000/21.75*2（无薪假 2 天）`；`POSTED` 下操作列全是 `—`（只读，符合状态机锁表） |
| `02-inconsistent.png` | `POSTED` + 后端回 `consistent=false` | 红条原文：`单头与明细不一致（2 处）—— 这张单的数不能直接发` + 逐字段 `应发合计 单头 9154.03 · 明细 9254.03` / `实发合计 单头 7354.03 · 明细 7454.03`（**页面照常打开，不 5xx**） |
| `03-vouchers-and-approval.png` | `POSTED` | 凭证卡三张：`计提凭证 已生成 PZ-20261130-000012 11,794.03`、`发放凭证 已生成 PZ-20261205-000003 7,354.03`、`红冲凭证 未生成 0.00`；审批轨迹 `人事复核 已通过 → 财务经理审批 已通过`，逐条审批人 / 时间 / 意见齐全 |

另外实测了 `CALCULATED`（已计算）状态（`MOCK_STATUS=CALCULATED`）：

| 断言 | 实测 |
|---|---|
| 操作栏 | `hr-salary-action-submit`（提交审批）+ `hr-salary-action-export`（导出工资表）；**没有**「试算 / 删除 / 作废」 |
| 审批卡 | 走空态 `未发起审批，暂无审批过程`（无实例 id 不打 flow 接口） |
| 凭证卡 | 两张都是 `未生成` + 说明「过账后生成（借 费用/成本，贷 应付职工薪酬）」 |
| 明细展开 | 每个薪资项右侧出现「改金额」→ 弹窗回显 `员工 张三（E0001）` / `薪资项 基本工资 BASE` / `原金额 8,000.00` / 新金额 + 原因（≥5 字） |
| 红冲弹窗（`POSTED`） | 影响预览列出将红冲的两张凭证号；空原因点确认 → 前端先拦：`红冲原因必填（至少 5 个字，会落到单据与状态日志上）`（不打注定 400 的请求） |

## 4. 设计取舍（已在本卡上报）

1. **「试算 / 删除 / 作废」不出按钮**：设计 §6.2.5 的按钮态表写了这三个，但 §5.2 的接口表里**没有对应路由**
   （`POST /salary/calc` 是「按期间试算」不是「按单重算」；状态机 6 档里**没有 `CANCELLED`**，
   V0383 的 `ck_biz_sal_sheet_status` 已定稿）。所以动作矩阵只列**真接口**支撑的
   `submit / post / reverse / export` —— 不画点了必 404 的假按钮。单测把这条钉住了。
2. **`SUBMITTED` 只读**：撤回 / 驳回由审批中心推进（`SUBMITTED → CALCULATED` 是驳回 / 撤回边），
   薪资单这边没有撤回路由，页面上给只读 + 状态提示，不给假按钮。
3. **红冲卡不给深链**：单头只下发 `reversal_voucher_id`（没有 `reversal_voucher_no`），
   与其猜一个点了打不开的单号，不如让用户去凭证页按来源单号反查（`GET /erp/voucher/by-source`）。
4. **薪资列表页缺失**：本卡只做详情页；`/business/hr/salary` 列表页目前**没有前端卡片**，
   所以「返回」在没有历史时回工作台（不指向 404）。**已上报**，需要单独开卡。
5. **`CancelImpactDialog` 未接**：设计 §6.3 建议红冲用它的「影响预览」，但该组件的
   `DOC_KIND_ROUTES` 只有 7 种 ERP 单据（无 HR 类型），接了也打不开。本卡用自带的确认弹窗
   列出「将红冲哪两张凭证」达到同一目的。
6. **附件面板**：`biz_salary_sheet` 已在 `DOC_KIND` / `DEFAULT_ROLE_BY_DOC` 里（`DEFAULT` 角色），
   直接复用 `DocAttachmentPanel`，未改公共组件。
