# COST-B2-13 前端三页（工时台账 / 费用台账 / 分摊规则）实测证据

卡：`t_d8507235` · 批次 B2 · 依据 `docs/architecture/ARCH-9-成本核算升级设计.md`
§3.1 ④⑤⑦、§5.2 #10–#24、§6.1 #2/#3/#4、§6.2.3、§6.2.4。

## 交付物

| 文件 | 作用 |
|---|---|
| `frontend/src/api/business/erp/cost-labor-kinds.ts` / `cost-labor.ts` | 工时台账词表 + 契约类型 / 7 个请求函数（含 multipart 导入、模板下载） |
| `frontend/src/api/business/erp/cost-overhead-kinds.ts` / `cost-overhead.ts` | 费用台账词表 + 类型 / 3 个请求函数 |
| `frontend/src/api/business/erp/cost-alloc-rule-kinds.ts` / `cost-alloc-rule.ts` | 分摊规则词表 + 类型 / 5 个请求函数（含 preview） |
| `frontend/src/views/business/erp/cost-labor/{index.vue,cost-labor-logic.ts}` | 工时台账页 + 纯逻辑（校验 / payload / 分组 / 导入回执） |
| `frontend/src/views/business/erp/cost-overhead/{index.vue,cost-overhead-logic.ts}` | 费用台账页 + 纯逻辑（池 / 直接判定、科目候选、本页汇总） |
| `frontend/src/views/business/erp/cost-alloc-rule/{index.vue,cost-alloc-rule-logic.ts}` | 分摊规则页 + 试算弹窗 + 纯逻辑（守恒判定 / 尾差行 / 版本化 payload） |
| `frontend/src/views/business/erp/shared/decimal.ts` | 定点小数工具（合计 / 相等判定 / 占比），三页共用 |
| `frontend/src/views/business/erp/shared/manufacturing-account.ts` | `4101.*` 判定 + 科目下拉候选（费用台账与分摊规则共用） |
| `frontend/src/views/business/erp/shared/error-text.ts` | 后端 `ApiError` → 人话（三页共用，纯函数） |
| `frontend/src/router/index.ts` | 三条路由（与 V0378 菜单 `…c002/c003/c004` 的 path/component 逐字对应） |
| `frontend/tests/unit/erp-cost-{labor,overhead,alloc-rule}.test.ts` | 39 条纯逻辑单测 |

## 验收项对照

| 卡面验收 | 证据 |
|---|---|
| 三个列表页 | `05/06/07_*.png`（真实例 + 真浏览器，1440×900） |
| 导入按钮（工时） | `05` 页工具栏「导入」→ 弹窗（模板 8 列说明 + 下载模板 + 选文件 + 上传并校验）；`04_http_smoke.out` 第 2 项验证模板接口返回真 xlsx（8610 字节） |
| 分摊规则试算弹窗，显示各对象分摊额 | `08_preview_dialog.png`：逐规则一组，逐对象 基数 / 占比 / 分摊额 / 尾差，表尾「合计 = 池（守恒）」打勾 |
| typecheck + build 通过 | `01_typecheck.out`（exit 0）、`03_build.out`（exit 0） |
| 纯逻辑单测 | `02_unit_tests.out`：全仓 1419 passed / 0 failed（本次新增 39 条） |

## 复现步骤

```bash
# 1) 起隔离实例（空库自动全链迁移，含 4101.* 科目 / 菜单 / 字典种子）
bash 00_up.sh up            # :18031，库 costb213_ui
# 2) 接口冒烟（登录 → 三列表 → 模板 → 工时 66059 / 显式费率 → 费用池行 → 规则 → 试算守恒）
BASE=http://127.0.0.1:18031 bash 04_http_smoke.sh
# 3) 前端（代理到隔离实例）
cd /opt/cloudpivot/frontend && BACKEND_ORIGIN=http://127.0.0.1:18031 pnpm dev --port 5199 --strictPort
# 4) 收场
bash 00_up.sh down
```

## 浏览器实测记录（2026-11-11）

1. **工时台账**（`05`）：1 行数据（CB-20261111-000001 / 张三 / 总部 / 8.00 工时 / 2.00 机器工时 /
   50.000000 费率 / 400.00 金额 / 已确认 / 手工录入）。
2. **新增工时 → 工资率留空**：填齐对象 / 姓名 / 工作中心 / 工时后提交，
   界面原样展示后端报文「工资率取数失败：…（请求 ID …）」+ 页面补的「三级都没配 ⇒ 422 + 66059，
   **不会按 0 计**…请到费率维护里配一条生效中的工资率」——即 66059 的"怎么修"落到界面。
3. **分组视图**：切换开关 → 按成本对象汇总（人数 / 行数 / 人工工时 8.00 / 机器工时 2.00 /
   金额 400.00 / 确认进度 1 已确认 0 草稿），页脚写明「本页 1 行 → 1 个成本对象」。
4. **导入弹窗**：8 列模板说明 + 「下载导入模板」（接口实测 200 + xlsx）+ 期间 + 选文件 + 上传并校验。
5. **费用台账**（`06`）：池行显示「车间池 待分摊」标签，借方 4101.09 / 对方 1002 / 金额 1,000.00；
   页脚汇总「车间池 1,000.00（1 行）· 直接计入 0.00（0 行）· 合计 1,000.00」。
   新增表单的服务端校验：借方科目下拉只列 `4101.01/02/03/04/09`（末级 + 启用）。
6. **分摊规则**（`07`）：列表显示 UI_SMOKE_UTILITY / 总部 / 4101.02 制造费用-水电费 /
   按人工工时分摊 / 2026-11-01 起（长期）/ 启用；新增表单四种分配标准带一句解释。
7. **试算**（`08`）：`period_code=2026-11` → 摘要「池 0.0000 / 摊 0.0000 · 守恒 ✓ · 未分摊 0 组」，
   组内逐对象 基数 8.0000 / 占比 100.00% / 分摊额 0.00 / 尾差 0.0000，
   表尾「合计 = 池（守恒）」。

> 池为 0 的说明：池金额来自 `biz_cost_collect` 的费池行，只有 `calc` 第 3 段（B2-8）跑过才有；
> 本期编排队列卡（#27）尚未落地，无 HTTP 入口。这与 B2-5 试算的设计口径一致（试算读池，不读台账）。

## 已知偏差 / 交接

1. **成本对象下拉复用 `cost-object` 列表接口**（`cost-object:list`）。财务角色由 V0378 同时授予两页，
   不存在 403；若将来出现"只录工时"的角色，需要像出库单那样加一个最小投影接口
   （`cost-object-options`，权限复用调用方），不要给下拉单开新码（`perm_menu_gate` 会红）。
2. **规则"已参与分摊"在列表里看不出来**：`CostAllocRuleListRes` 没有该字段，页面只能靠 PUT 的
   409 报文 + 补的「新建生效期更晚版本」引导（`RULE_LOCKED_HINT`）。要事前灰掉字段需要后端加
   `allocated_periods` 出参（另立卡，本卡不改后端）。
3. **费率维护没有入口**（B2-3 已报）：新环境费率表为空时，工时录入必报 66059。页面把这条写在表单脚注里。
4. **费用台账没有编辑/导入**：设计 §5.2 只给 list/create/delete；§5.4 明确费用导入二期。
5. **表宽**：三页列表按 1440 视口（侧栏展开，内容区约 1128px）校过列宽，实测量
   `scrollWidth == clientWidth`（工时 1128/1128、分摊规则 1136/1128，8px 内不产生横向滚动条）；
   第一版 1260/1292px 会把「状态」和「操作」列盖住 —— 已在代码注释里写死这个数。
6. **无迁移**：本卡是纯前端卡，未碰任何迁移（`git status` 里只有 `frontend/` 与 `docs/evidence/`）。
