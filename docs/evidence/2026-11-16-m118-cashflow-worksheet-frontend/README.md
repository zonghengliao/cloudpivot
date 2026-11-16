# M1-18 现金流底稿：前端页面（底稿录入 + 预览）— 证据

卡：`t_c97db8bd`（M1-18【M1·财务】现金流底稿：前端页面）
上游：M1-13（`V0414` 表）、M1-14（`V0415` 底稿两码 + 三接口）、M1-15（`V0416` 调整项）、
M1-16（`V0417` 出表两码 + 两接口）、M1-17（全链单测）。

## 做了什么（只前端，零后端 diff、零迁移）

`/business/erp/report/cash-flow` 仍是**同一个菜单页**（`sys_menu …001018`），改成三个 tab ——
`V0415` / `V0417` 的迁移注释已经定死这个形状（「前端（M1-18）把底稿做成现金流量表页面里的
一个视图 / 抽屉，路由与页面权限仍归 `/business/erp/report/cash-flow`」，不许新开 `menu_type=2`）：

| tab | 内容 | 需要的权限码 | 文件 |
|-----|------|--------------|------|
| 现金流底稿（默认） | 逐行录入期初 / 三大活动明细 / 期末 + 勾稽结论 + 草稿试算 | `business:erp:cashflow-worksheet:list` / `:save` | `worksheet-panel.vue` |
| 报表预览 | 由底稿汇总的三栏报表（本期 / 上期）+ 导出 xlsx + 勾稽两条等式 | `business:erp:cashflow-statement:list` / `:export` | `statement-panel.vue` |
| 按凭证编制 | 原有 ERP-R3 简化版报表（原地搬家，行为不变） | 页面码 `business:erp:report:cash-flow` | `voucher-panel.vue`（原 `index.vue` 内容） |

新增/改动文件：

```
frontend/src/api/business/erp/cashflow-worksheet.ts          # 5 个接口 + 逐字抄后端的 DTO 类型
frontend/src/views/business/erp/report/cash-flow/worksheet-logic.ts   # 纯逻辑（金额/草稿/提交体/展示口径）
frontend/src/views/business/erp/report/cash-flow/worksheet-panel.vue  # 底稿录入 tab
frontend/src/views/business/erp/report/cash-flow/statement-panel.vue  # 报表预览 tab
frontend/src/views/business/erp/report/cash-flow/voucher-panel.vue    # 按凭证编制 tab（原 index.vue 内容）
frontend/src/views/business/erp/report/cash-flow/index.vue            # tab 容器（按权限决定显示哪几个）
frontend/src/constants/error-messages.ts                     # 补 66085 / 66086 / 66087 三条人话
frontend/tests/unit/erp-cashflow-worksheet.test.ts           # 14 条纯逻辑单测
frontend/tests/unit/pdf08-doc-pdf.test.ts                    # 契约里的文件路径跟着搬家（行为不变）
```

## 五条设计决定（都有理由，写进代码注释里了）

1. **保存允许暂时不平**：底稿是工作稿，页面**不**拿 `check.balanced` 拦保存（拦了会计只能先编
   假数）；只有「出表 / 导出」才要求闭合。保存后如果还没平，给黄色提示 + 差额，而不是红色报错。
2. **只提交改动过的行**：后端 `items` 是非空即 upsert，整表发一遍会把 12 个从没编过的配置项
   写成「已录 0」，`entered` 位被污染 ⇒「本期没编这一行」的信息就没了。`buildSavePayload`
   （有单测）只挑 diff；`opening` / `closing` 各自独立，省略 = 不改。
3. **试算不落库 + 标注来源**：`试算校验` 打 `POST …/worksheet/check`（一个字节都不写），结果标
   「草稿试算（未落库）」；已存底稿的结论标「已存底稿」。两者不混。
4. **`null` ≠ 0**：报表上期列为 `null`（上期没开账 / 上期没编这一行）显示「—」，不显示 0。
5. **导出闸的文案**：后端未闭合回 `400 + 66087`，但 `responseType: 'blob'` 时错误体被 axios
   包成 Blob，全局拦截器读不到 `msg`（只会显示「请求参数不正确」）。所以页面用**已加载的**
   `check` 在前端先拦一次并拼出人话（含精确差额与逐条问题）；后端闸照旧保留。

## 验收（本机 99 + 145 的 PG 克隆库 `cp_m118_front`）

```bash
# 起后端（迁移自动跑 V0414~V0417 到克隆库）
cd backend && cargo build -p cloudpivot
NIK_MIDDLEWARE_AUTH_JWT_SECRET=… CLOUDPIVOT_STORAGE_MASTER_KEY=… X_CONFIG_SECRET_KEY=… \
NIK_DATABASE_HOST=172.18.10.145 NIK_DATABASE_NAME=cp_m118_front NIK_DATABASE_USER=cloudpivot \
NIK_DATABASE_PASSWORD=… ./target/debug/cloudpivot        # :8023
# 前端 dev（vite 代理 /api → :8023）
cd frontend && pnpm dev                                  # :5173
# 真接口 34 条断言（脚本在 api_check.py，直接打 :8023）
python3 docs/evidence/2026-11-16-m118-cashflow-worksheet-frontend/api_check.py
```

| 项 | 结果 | 证据 |
|----|------|------|
| 真接口 34 条断言（读空底稿 / 保存 / 试算不落库 / 出表两栏 / 真 xlsx / 出表闸 66087 / 期间闸 66085 / 项目闸 66086 / 金额闸 10001） | 全部 PASS | `00_api_check.out` |
| 底稿页（闭合，含勾稽结论） | 见截图 | `01-worksheet-balanced.png` |
| 报表预览（三栏、上期未开账整列「—」） | 见截图 | `02-statement-preview-balanced.png` |
| 草稿试算不闭合（差 100 精确显示，「草稿试算·未落库」标注） | 见截图 | `03-worksheet-draft-unbalanced.png` |
| 未闭合时报表页可开、导出被拦（人话含差额） | 见截图 | `04-statement-unbalanced-export-blocked.png` |
| 后端（本卡零 diff，仍跑门禁） | `cargo fmt -- --check` exit 0、`cargo clippy --lib -- -D warnings` exit 0 | `05_backend_fmt.out` / `06_backend_clippy.out` |
| 本卡单测 14 条 | 14/14 PASS | `07_unit_cashflow_worksheet.out` |
| `pnpm typecheck`（vue-tsc） | exit 0 | `08_typecheck_vue.out` |
| `pnpm typecheck:unit` | exit 0 | `09_typecheck_unit.out` |
| `pnpm test:unit` 全量 | 1947/1947 PASS | `10_unit_all_tail.out` |
| `pnpm build` | exit 0（含 typecheck），`✓ built in 50.70s` | `11_build_tail.out` |

### `00_api_check.out` 覆盖的九组（真库，无 SKIP）

1. 读空底稿：12 个配置项全 `entered=false`、期初/期末 `null`、`problems` 点名缺期初+缺期末；
2. 保存：期初 5000 + 流入 12000 − 流出 7000 = 期末 10000 → `balanced=true`，小计/净额逐位对；
3. 草稿试算：把期末改 9900 → `diff=100`、`IMBALANCED`，且**再读已存底稿仍是 10000**（证明不落库）；
4. 出表：`report_code=CASH_FLOW_WORKSHEET`、两栏列名、上期期间 2023-01、明细本期 12000 / 上期 0 且
   `previous_entered=false`、净增加额 5000、期初期末；
5. 导出：`200` + `PK` 魔数的真 xlsx（6856 字节，content-type 正确）；
6. 出表闸：期末改 9900 能保存（工作稿）→ GET 报表仍 200 但 `balanced=false` → 导出 400 + `66087`
   且文案带「差 100」→ 改回 10000 导出恢复 200；
7. 期间闸：已结账期间（2021-01）写入 400 + `66085`，读 200 + `editable=false`；
8. 项目闸：不存在的 `item_code` → 400 + `66086`；
9. 金额闸：5 位小数 → 400 + `10001`。

### 复原动作

- 克隆库 `cp_m118_front` **已删除**；`cloudpivot_test` 模板库只读未写（本卡只在克隆库里造数）。
- 本机临时后端（:8023）与 vite（:5173）在验收后停掉。
- 克隆库里为了能登录，把 `admin` 的口令哈希改成 `Admin@123456`（只影响这个已删除的克隆库；
  模板库与生产库未动）。

## 诚实缺口

- **权限自适应只在前端做了 tab 过滤**：只授 `worksheet:*` 的人看不到「报表预览」tab，但**接口**
  仍是各自的码把关（后端 403）——这是设计（V0417 注释：底稿与报表是两个风险面），不是缺口。
- **`pdf` / 打印**：底稿与企业版报表的 PDF 版式不在本卡（`cashflow_statement` 只有 xlsx 导出口径）。
- **间接法调整项页面**（`V0416` 的表 + `adjust-item` 五个接口）不在本卡：卡面范围只有「底稿录入 +
  预览」，调整项配置页是另一张卡。
- **e2e**：本卡没加 Playwright 用例（`e2e/_catalog.ts` 的 56 页清单按路由登记，本卡未新增路由）；
  浏览器验证用的是 dev server + 真后端的人工脚本（截图在上）。
