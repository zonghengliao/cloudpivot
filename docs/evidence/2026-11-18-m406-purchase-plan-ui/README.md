# M4-06 采购计划 · 前端页面（录入 + 执行率）取证

卡：`t_62aa0328`（M4-06【M4·采购】采购计划：前端页面）
日期：2026-11-18　机器：99 开发机（后端实例）+ 145 的 PG（演练库）

## 一、交付了什么

| 文件 | 作用 |
|:--|:--|
| `frontend/src/api/business/erp/purchase-plan.ts` | 接口层：9 条契约（6 条录入 + 2 条执行率只读 + 1 条转申请），**只有类型与取数函数** |
| `frontend/src/views/business/erp/purchase-plan/purchase-plan-logic.ts` | 页面纯逻辑：筛选入参 / 表单校验 / 金额预览 / 执行率展示 / 转申请校验（无 Vue 依赖，可被 Node 单测直跑） |
| `frontend/src/views/business/erp/purchase-plan/index.vue` | 页面：两个 Tab（采购计划 / 执行率）+ 建改弹窗 + 详情抽屉 + 转申请弹窗 + 执行率下钻抽屉 |
| `frontend/src/router/index.ts` | 注册 `/business/erp/purchase-plan` → `meta.permission = business:erp:purchase-plan:page`（页面显形码，后端零引用） |
| `frontend/tests/unit/erp-purchase-plan.test.ts` | 25 条纯逻辑单测 |
| `frontend/e2e/m406-purchase-plan.spec.ts` | 6 条真浏览器用例（真点真断言） |
| `scripts/verify/m406_purchase_plan_ui_drill.sh` | 一键演练：空库首启 → 夹具 → 真接口造单链 → 起前端 → Playwright |

## 二、口径（每条都能指回后端一行代码）

1. **金额/数量一律字符串提交**；单头三个 `total_*` 是派生列，payload 里干脆不送。
2. **类型 × 月份自洽**（ANNUAL 不带月 / MONTHLY 1–12）、年度 1990–2200 —— 与 `check_month_for_type` / `ck_biz_pp_period` 同口径。
3. **粒度 = 物料 × 仓库**：重复格在**请求前**拦住，报文与 `check_cells_unique` 逐字一致（「第 3 行与第 1 行…」）。
4. **月度计划的父单只能是同年度年度计划**（`check_parent_period`）；父单下拉只列同年度 ANNUAL。
5. **只有草稿能改/删/提交**：判据用后端给的 `editable`，前端不自己推状态机。
6. **只有已批准能转申请**（`APPROVED`），转出量 ≤ 计划量 − 已执行量，计划行单价为 0 时必须覆盖 `est_price > 0`。
7. **执行率状态不按 pct 反推**：读 `exec_state` / `over_plan`；允许「100.00% 仍为部分下单」；进度条封顶 100（真实倍数在文案里）。
8. **一仓一单**：转申请结果按仓库列出 N 张申请单（单号 + 仓库）。

## 三、验收证据（全部可复跑）

```bash
# 1) 类型检查（src 与 tests/unit 两套）
cd frontend && pnpm typecheck && pnpm typecheck:unit          # 均 rc=0
# 2) 纯逻辑单测（本卡新增 25 条）
cd frontend && npx tsx --test tests/unit/erp-purchase-plan.test.ts
# 3) 全量单测（回归：没踩到别人）
cd frontend && pnpm test:unit                                  # 2287 passed / 0 failed
# 4) 生产构建
cd frontend && pnpm build                                      # rc=0，chunk 里含本页
# 5) 真栈演练（建库 → 起后端 → 造单链 → 起前端 → 6 条浏览器用例）
bash scripts/verify/m406_purchase_plan_ui_drill.sh             # RESULT: PASS=23 FAIL=0
```

| 证据文件 | 内容 |
|:--|:--|
| `00_drill_full.out` | 演练全量输出（23 条 PASS） |
| `10_playwright.out` | Playwright 6/6 通过 |
| `01-plan-list.png` | 录入 Tab：3 张计划（月度草稿 / 已批准已下满 / UI 新建） |
| `02-execution-overview.png` | 执行率总览：100.00% + 「已下满」+ 行状态计数 |
| `03-execution-drill.png` | 下钻抽屉：KPI（计划 14 / 已申请 14 / 已下单 14 / 剩余 0 / 100.00%） |
| `04-execution-sources.png` | 展开来源单据：申请单号 + 采购单号（`counted=true` 原因「已下单」） |
| `m406_typecheck.out` / `m406_typecheck_unit.out` | 两套 typecheck rc=0 |

## 四、演练里真跑出来的东西（不是 mock）

- **空库首启**：`cp_m406_ui` 从 0 表起，实例自己跑完全部迁移（含 V0446–V0449），菜单页行落位。
- **单链**：建计划（2 行 × 2 仓）→ 提交 → 审批人批 → `APPROVED` → 转申请（一仓一单 2 张）→ 两张申请各自审批 → 合并转采购单 → 执行率 `DONE / 100.00%`。
- **页面侧**：通过表单真建一张计划（真 POST，列表立刻出现）；同一「物料 × 仓库」写两行被前端拦住并按后端文案点出行号；执行率 Tab 读到后端视图算出来的 100.00%；下钻能看到采购单号。

## 五、踩到并修掉的三个坑（都是浏览器才暴露的）

1. **`t-table` 的单元格插槽名必须等于 `colKey`**：初版按 `#lineMaterial` / `#cQty` 命名，槽子根本不生效 —— 明细行渲染成裸值（物料列空、计划量显示 `1`）。typecheck 发现不了。
   > 顺带发现 `purchase-requisition/index.vue` 的建单明细用的是同一套错误命名（`#lineMaterial`），属于**存量缺陷**，本卡按「不夹带」未改，已记录待后续卡。
2. **TDesign `t-tabs` 没有 `role="tab"`**：页签是 `.t-tabs__nav-item` 的 div，`getByRole('tab')` 永远等不到。
3. **下拉浮层 DOM 不会销毁**：上一个 select 的选项留在 body 里（hidden），`getByText(...).first()` 会命中那个 hidden 的旧选项 → 必须限定 `.t-select-option:visible`。

另外，空库演练环境里还有两个「挡点击」的浮层（`setup-wizard-dialog`、`login-unread-dialog`），e2e 里用 localStorage/sessionStorage 标记 + 兜底点掉，见 spec 注释。

## 六、没做的事（明确划界）

- **后端零改动**：本卡只做前端页面；`cargo fmt` / `cargo clippy --lib` 不涉及（无 diff）。
- **不接 `_catalog.ts` 的 52 页套件**：那套要求全站 56 页都有 probe 现场文件，本卡只验本页，单列一份 spec。
- **`purchase-requisition` 的槽名缺陷**：只记录不修（不夹带）。
