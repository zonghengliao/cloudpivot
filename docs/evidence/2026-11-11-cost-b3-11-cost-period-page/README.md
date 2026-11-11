# COST-B3-11 前端成本核算页（5 步向导 + 试算确认 + 审批进度 + 红冲）

卡：`t_04c834c7` · 批次 B3 · 依据 `docs/architecture/ARCH-9-成本核算升级设计.md`
§6.1 #5 / §6.2.2（五步向导）/ §4.1（状态机）/ §5.2 #25–#33（接口）/ §5.3（权限码）。
后端接口全部来自 B3-1 / B3-3 / B3-5 / B3-6 / B3-7（本卡**零后端改动**）。

## 交付物

| 文件 | 作用 |
|---|---|
| `frontend/src/api/business/erp/cost-period-kinds.ts` | 词表 + 契约类型（7 状态、5 权限码、高危关键字、列表/详情/计算单出参） |
| `frontend/src/api/business/erp/cost-period.ts` | 9 个请求函数（列表 / 详情 / 建 / 改 / 作废 / 提交 / 撤回 / 结算 / 红冲），`export *` 出词表 |
| `frontend/src/views/business/erp/cost-period/cost-period-logic.ts` | 纯逻辑：状态→步、按钮门禁、关键字闸、原因长度、合计现算、守恒判定、结算/红冲文案 |
| `frontend/src/views/business/erp/cost-period/index.vue` | 页面：批次列表 + 五步向导抽屉 + 提交预览 + 高危确认 + 深链 `?batch_no=` |
| `frontend/src/views/business/erp/cost-period/steps/CalcSheetPanel.vue` | 步③ 成本计算单（对象×料工费×约当×单位成本×完工/在制，**行可展开看明细**，表尾守恒打勾） |
| `frontend/src/views/business/erp/cost-period/steps/RiskConfirmDialog.vue` | 结算/红冲共用高危确认框（先看「将发生什么」→ 关键字 + 原因两道闸） |
| `frontend/src/views/business/erp/cost-period/steps/UncollectedPanel.vue` | 步② 缺口清单（B2-12 已有，本卡接线复用，未改动） |
| `frontend/src/router/index.ts` | 路由 `business/erp/cost-period`（与 V0378 …c005 菜单 path/component 逐字一致） |
| `frontend/src/views/business/erp/shared/source-route.ts` | `COST_PERIOD` 从 `page-missing` 改为 `detail`（凭证来源可现在直达批次，读 `?batch_no=`） |
| `frontend/src/views/business/erp/voucher/voucher-source-link.ts` | `page-missing` 分支改字符串比较（表里已无该态，保住语义不炸编译） |
| `frontend/tests/unit/erp-cost-period.test.ts` | 20 条纯逻辑单测 |
| `frontend/tests/unit/vtrace-source-route.test.ts`、`vtrace-voucher-source-link.test.ts` | 跟随 COST_PERIOD 上线更新断言（原测试自己写着「一旦注册就改」） |

## 验收项对照

| 卡面验收 | 证据 |
|---|---|
| 5 步：选期间 → 料工费确认 → 试算 → 提交审批 → 结算 | `07_step1_period.png` / `06_step2_uncollected.png` / `05_step3_detail_expanded.png` / `09_step4_approval_progress.png` / `08_step5_settle.png`、`15_step5_settled.png`（t-steps 五步全程可前进/后退，步③ 起有批次才放行） |
| 试算结果可看明细 | `05_step3_detail_expanded.png`：展开行分「数量 / 金额 / 单位成本」三组（期初在制、完工数量、在制数量、完工进度、两个约当产量、期初成本、三项投入、三项单位成本、完工入库单价），页脚现算守恒式「完工转出 + 期末在制 = 期初在制 + 本期投入 —— 平」 |
| 审批进度条 | `09_step4_approval_progress.png`：复用审批中心 `FlowProgress`，节点/审批人/时间/意见全来自 `GET /business/flow/instance/{id}`（真实两级审批：财务经理甲 → 财务总监乙，均 2026-11-11 16:43:46 通过） |
| 红冲按钮 + 二次确认 | `12_reverse_dialog.png`（计划文案 + 原因 ≥10 字 + 输入「红冲」）/ `13_reverse_dialog_filled.png` / `14_reverse_real_66078.png`（真实服务端报文原样回显）；列表行也有「红冲」入口（`03_page_list.png` 第 2 行） |
| typecheck + build 通过 | `16_typecheck.out` exit=0、`18_build.out` exit=0、`17_unit.out` **1498 passed / 0 failed** |

## 接口冒烟（真实后端逐条打）

`bash 01_smoke.sh` → `02_smoke.out`（**0 条断言失败**），覆盖：

1. 期间下拉（12 个未结账期间）；
2. 两个成本对象夹具；
3. `POST /cost-period` 建 DRAFT（`CS-202610-0001`）；
4. 同期间再建 → **409 + 66072**（报文点名已有批次号 —— 步① 提示的判据）；
5. 用 SQL 灌两行计算单（算例 A 口径：19000/4000/4000 → 完工 22311.1104 + 在制 4688.8896 = 27000）；
6. `GET /cost-period/{id}`：`items` 2 行、`items_summary.total_cost = 27700.0000`（**现算**）、单头 `total_*` 仍是 `0.0000`（DRAFT 未结算，页面只用现算值）；
7. `GET /cost-period/uncollected?period_code=` → 200 + `gap_count`（不阻断）；
8. `POST /{id}/submit` → `SUBMITTED` + `approval_instance_id`；
9. `GET /business/flow/instance/{id}` → 节点（`s` 已通过 / `n_fm` 处理中）；
10. 财务经理甲、财务总监乙各自用自己的令牌逐级同意 → 批次 `APPROVED`；
11. `POST /{id}/settle` → **409 + 66077**（C-COST-3 断言不平，整笔回滚），批次仍 `APPROVED`（**不留死单**）；
12. `POST /{id}/reverse`（APPROVED）→ 409 + 66073（状态闸）；原因太短 → 400；
13. 造一张 SETTLED 夹具供页面红冲入口 → 红冲 → 409 + 66078（无蓝字凭证可红，报文给了修法）；
14. 列表出参形状 + `PUT` 改 SETTLED → 409。

## 复现步骤

```bash
bash 00_up.sh up                  # :18041，库 costb311_ui（含 V0381 审批流种子）
bash 01_smoke.sh                  # 接口冒烟（0 失败）
cd /opt/cloudpivot/frontend
BACKEND_ORIGIN=http://127.0.0.1:18041 pnpm dev --port 5199 --strictPort
# 浏览器：token 注入 localStorage（x_access_token / x_refresh_token）→ /business/erp/cost-period
bash 00_up.sh down                # 收场（dropdb）
```

## 已知偏差 / 交接

1. **`POST /cost-period/calc`（设计 §5.2 #27 的六段计算编排）尚未落地**，所以步③ 的「重算」按钮是
   **禁用 + tooltip 说明**，不伪造本地试算：口径在后端，前端算出的数没人认。要看到真实计算单，
   当前只能由后端计算流程写入（本卡证据里的 2 行是用 SQL 按算例 A 口径灌的，用于验证渲染与守恒）。
   编排卡落地后只需在 `cost-period.ts` 加 `calcCostPeriod()`、在 `handleRecalc` 里调用，其它不动。
2. **结算/红冲的 happy path 无法在页面端演示**：没有 calc 就产不出合规归集与三张蓝字凭证，
   结算的 6 条事务内断言必红（实测 66077，报文本卡原样回显，且整笔回滚、批次仍 APPROVED）。
   红冲同理（66078「无蓝字可红」）。这两个动作的**成功分支**证据在 B3-6 / B3-7 的真库用例里，
   页面侧验证到：按钮门禁、二次确认、真实报文回显、失败后单据状态不变。
3. **列表里的「投入 / 完工转出 / 期末在制」是落库值**（后端 `CostPeriodListRes` 的口径：结算时回写），
   所以未结算批次列显示 `0.00`，明细合计在详情 `items_summary`（步③ 表尾）。这是后端 DTO 定死的
   两套数，本卡不改后端；若产品要求列表也显示现算值，属后端另开卡。
4. **红冲关键字「红冲」是本卡口径**（设计只写了结算输入「结算」二字）：红冲同样动已入账凭证与库存，
   所以沿用同一套高危确认，并要求原因 ≥10 字（后端本来就强制）。
5. **深链 `?batch_no=`**：借 VTRACE 的凭证明细「来源=成本核算批次」点进来直接打开该批次；为此把
   `source-route.ts` 的 `COST_PERIOD` 从 `page-missing` 改成 `detail`，并同步改了两条 vtrace 单测
   （原测试注释就写着「一旦路由注册就该改成能跳」）。这是本卡唯一改到别人模块的地方，已在此报备。
6. **无迁移、无 Rust 改动**（`git status` 里只有 `frontend/` 与 `docs/evidence/`）：卡面「迁移幂等 +
   `.down.sql`」「cargo test」不适用；后端单测未跑（没有后端改动）。
7. 单卡耗时约 3h（超 1.5h）：大头是真例 + 真浏览器把「建批次 → 提交 → 两级审批 → 结算/红冲报文」
   跑通（含造两个审批人账号绕过职责分离、修 token 被遮蔽器改写导致的 401、按 1440 视口重排列宽）。
