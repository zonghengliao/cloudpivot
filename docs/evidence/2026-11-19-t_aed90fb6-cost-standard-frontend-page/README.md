# M5-18 标准成本：前端页（维护 + 发布）— 交付证据

> 卡：`t_aed90fb6`（M5 成本批第 18 张）· 日期：2026-11-19 · 主机：99（开发机，PG 在 145）
> 演练库：`cp_m518_smoke`（145 上由 `cloudpivot_test` 克隆 → 本卡新二进制首启把迁移补到 **V0465**；跑完已 drop）
> 上游：M5-09（`t_99fe85c8` 维护页 + V0464 菜单）· M5-10（`t_a7d7a2d5` 发布轴 + V0465）· M5-08（版本表 8 条接口）

## 〇、先说明「本卡补的是什么」

标准成本页的**维护屏（版本 CRUD + 批量导入）** 是 M5-09 交的、**发布屏（提交 / 撤回）** 是 M5-10 交的。
M5-18 落地前，M5-08 交的 8 条接口里有 **2 条没有任何前端消费者**（`period-sheet` / `effective`），
而 M5-08 的交接明确写着「页面第一屏就是物料 × 期间矩阵（`period-sheet`）」。

本卡把这一页补成 M5-08 设计的样子：

| 屏 | 内容 | 数据源 |
|---|---|---|
| **版本维护**（tab 1） | 一版一行：CRUD / 生效失效 / 提交发布 / 撤回 / 批量导入（M5-09 + M5-10 既有，本卡未改行为） | `cost-standard`（列表）+ 详情 / 写接口 |
| **期间视图**（tab 2，**本卡新增**） | 一个物料一行：这一期按哪一版算 + 换版标记 + 提交发布 / 撤回 / 编辑 | `GET /cost-standard/period-sheet`（此前**无消费者**） |
| **查生效版本**（对话框，**本卡新增**） | 某物料某期间（或某天）命中哪一版：金额、标准 BOM、期间内其它版本、口径说明 | `GET /cost-standard/effective`（此前**无消费者**） |

## 一、改动清单

| 文件 | 说明 |
|---|---|
| `frontend/src/views/business/erp/cost-standard/index.vue` | ListPage 收进 `t-tabs`（`destroy-on-hide=false`，切走不丢状态）；新增「期间视图」面板（期间 / 关键字 / 查询 / 查生效版本 / 矩阵表 / 分页 / 空屏 / 口径说明）；新增「查生效版本」对话框（物料 + 期间⇄日期 + 结果区：摘要 / 换版告警 / 金额 / BOM / 期内其它版本）；`reload()` 顺带刷矩阵 |
| `frontend/src/views/business/erp/cost-standard/cost-standard-logic.ts` | 纯逻辑 +4 组：`sheetStats` / `sheetSummary` / `SHEET_SCOPE_HINT`（矩阵口径说明）、`switchNotice`（换版提示，顺手去掉后端 note 的 `**`）、`effectiveLookup`（期间⇄日期二选一，别让后端判 400）、`effectiveSummary` / `EFFECTIVE_NOT_PUBLISHED_HINT` |
| `frontend/src/api/business/erp/cost-standard-kinds.ts` | 新增常量 `COST_STANDARD_NOT_FOUND_CODE = 10002`（`effective` 的 404 用它判「是不是还没发布」） |
| `frontend/src/constants/error-messages.ts` | **顺带修一条既存红灯**：M5-16 加的 `66112`（差异凭证重复）没有前端人话，`error-messages.test.ts` 在本卡之前就红；补一行文案 |
| `frontend/tests/unit/erp-cost-standard.test.ts` | 25 → **35 条**（+10）：期间视图统计 / 话术 / 口径说明、换版提示（含去 `**`）、生效版本查询的取数条件（只发一个条件）、形状校验、摘要话术、404 引导 |

**零迁移、零后端改动、零新路由、零新权限码**（发布按钮沿用 `cost-method:update`，与 M5-10 一致）。

## 二、口径（决定，不是待办）

1. **期间视图只显示「参与取数」的行** —— 后端 `period_sheet` 的 WHERE 是
   `status = 1 AND publish_state = 'PUBLISHED' AND 覆盖期末`。所以：
   - 草稿 / 审批中的卡**不在矩阵里**（差异分析也吃不到它）；
   - 只覆盖期初到中旬、没人覆盖期末的版本**也不在**（期末口径）。
   页面必须把这件事写在明面上（`SHEET_SCOPE_HINT`），否则「维护页 5 张卡 → 矩阵 3 行」会被当成丢数据。
   **不做**「把未发布卡也塞进矩阵」：那会让矩阵同时承担两种语义（口径 vs 清单），而清单已经有「版本维护」的发布状态筛选。
2. **换版 = 期末那一版 + 显式标记**：一期内两版（12-01~12-15 / 12-16~12-31）时，矩阵一物料仍只有一行（v2），
   `switch_in_period = true` 打「期间内换版」标；差异分析按整期一套口径（分段属方法切换策略卡，本卡不含）。
3. **「查生效版本」只发一个取数条件**：`period_code` 与 `on_date` 同时发会被后端判「互相矛盾」400（`10001`）。
   两选一的判断收在 `effectiveLookup`，不散在模板里。
4. **`effective` 的 404 要补一句话**：它只返回已发布版本，所以「我明明建过卡」与「查不到」会同时成立
   （卡还是草稿 / 审批中）。只念后端报文会把人推去重建一张卡 —— 那会撞 66108 重叠。页面补
   `EFFECTIVE_NOT_PUBLISHED_HINT`（是不是还没发布 → 去版本维护提交发布）。
5. **矩阵表「操作」列不 fixed**：实测 1600 视口下固定列会把「换版」整列压住（列头都看不见），
   只有横向滚动才露出来。矩阵宽度贴着容器，去掉 fixed + 收窄列宽即可全列可见（`04_dom_checks.out` 里有列头坐标）。
6. **后端 note 里的 markdown 强调记号在 UI 去掉**：`note` 是给人读的（`本期按**期末 2026-11-30** 取数`），
   但页面那个位置是纯文本 alert —— 不去 `**` 会在界面上留星号（口径原文照旧展示，只去记号）。
7. **两屏共用一份发布闸**：矩阵的「提交发布 / 撤回」直接调 M5-10 的接口 + `publishGate` 判可点，
   不在矩阵里另写一套判据（否则列表说能点、矩阵说不能点）。

## 三、验收（全部真跑）

| 证据文件 | 内容 | 结果 |
|---|---|---|
| `05_smoke_sheet.out` | 真实例 `127.0.0.1:8023` + 真库 `cp_m518_smoke` 跑 `smoke_sheet.py`（39 条断言） | **39 PASS / 0 FAIL** ✅ |
| `06_typecheck.out` | `pnpm typecheck`（src/**）· `pnpm typecheck:unit`（tests/unit/**） | rc=0 / rc=0 ✅ |
| `07_unit.out` | `pnpm test:unit` | **2425 passed / 0 failed**（本卡 +10；本卡之前 2424/1 FAIL —— 那条 66112 红灯由本卡补文案修掉） ✅ |
| `08_build.out` | `pnpm build`（typecheck + vite build） | rc=0 ✅ |
| `01_maintenance_tab.png` | 真浏览器：维护屏（3 条夹具，切回 tab 后列表状态仍在） | ✅ |
| `02_period_sheet.png` | 真浏览器：期间视图（2 行 · 换版标记 · 金额列 · 分页「共 2 条数据」） | ✅ |
| `03_effective_dialog.png` | 真浏览器：查生效版本（摘要 / 换版告警 / 金额 / BOM / 期内其它版本） | ✅ |
| `04_dom_checks.out` | 页面 DOM 取证（tabs、列头、行内容、按钮可点性、换版列坐标、分页文案） | ✅ |

### 39 条 HTTP 冒烟覆盖了什么（`smoke_sheet.py`）

| 组 | 断言要点 |
|---|---|
| ① 期间视图 | 期间必填 400/10001；草稿卡不出现；审批中不出现；**只覆盖期初到中旬（期末没人覆盖）⇒ 仍 0 行**；期内换版 ⇒ 一行 = 期末那版 v2 + `switch_in_period`；换版行金额取 v2（工 70 / 总 97）；整期单版本 ⇒ `switch_in_period=false`；关键字命中 / 乱码 0 行；`page_size=1` ⇒ 1 行 + total=2 |
| ② 查生效版本 | 两条件都不给 400/10001；按期间 ⇒ `on_date` = 期末；命中已发布 v2；`switch_in_period` + `other_versions=[v1]`；`note` 非空；详情带 BOM（`std_amount=15.000000`）；按日期 12-01 ⇒ v1；期间与日期矛盾 400；无版本 404 + **10002** + 报文带建卡引导 |
| ③ 发布轴 | 草稿 → 提交 = PENDING；重复提交 409/66111；审批中改口径 409/66111；撤回 → DRAFT；再撤回 409/66111；撤回后可继续改 |
| ④ 清场 | 四张卡软删（still-alive = 0、期间视图清零）；夹具硬删，零残留 |

### 页面 DOM 取证要点（`04_dom_checks.out`）

```
tabs          : ["版本维护", "期间视图"]
sheet_meta    : 本期 2 个物料按标准成本取数；1 个物料期间内换过版（整期按期末那一版取数）
sheet_headers : 物料 / 版本 / 生效期 / 标准材料 / 标准人工 / 制造费用 / 标准总成本 / 换版 / 操作
row1          : DEMO-M001 v2 · 2026-11-16 ~ 2026-11-30 · 15 / 70 / 12 / 97 · 期间内换版
row1_actions  : cs-sheet-publish:disabled / cs-sheet-withdraw:disabled / cs-sheet-edit:on / cs-sheet-effective-row:on
                （已发布 ⇒ 提交与撤回都不可点，与 publishGate 一致）
switch_header : x=1203 right=1307（在表内，未被固定列压住）
```

浏览器验收**没有在页面上输入口令**：先在同一实例上用 `POST /system/auth/login` 取到 access/refresh
token，再 `localStorage.setItem('x_access_token' | 'x_refresh_token')` 注入后打开页面（避免凭据走浏览器输入通道）。

## 四、给下游卡的接缝

- **M5-19（差异分析页）**：可直接复用 `cost-standard-kinds.ts` 的
  `publishStateLabel / publishStateTheme / statusLabel / statusTheme` 与 `logic.ts` 的
  `amountsOf / windowText / materialLabel / sheetStats / switchNotice`。
  「未发布不参与取数」这条口径已由本卡的三处文案统一（矩阵口径说明 / 生效版本 404 提示 / 摘要），
  差异分析页照抄这三句即可，别另造说法。
- **发布流程相关（M6）**：本卡只做「发起 / 撤回」，审批通过后的落地由 M5-10 的桥完成；页面**不跳审批中心**
  （`publish_instance_id` 已在行数据里，将来做「看审批轨迹」直接用它，不必改接口）。
- **M5-34（标准成本 E2E）**：`seed_demo.py` 就是一套可复用的浏览器夹具（两版换版 + 整期单版本），
  Playwright 用例可直接照它的三条造数。

## 五、已知边界（本卡刻意不做）

1. **矩阵不含未发布卡**：口径见 §二.1；未发布清单走「版本维护」的发布状态筛选（`publish_state=DRAFT/PENDING`）。
2. **不做页面内审批**：通过 / 驳回在审批中心，页面只有提交与撤回。
3. **不做浏览器 E2E 入库**：Playwright 用例属 M5-34；本卡的真浏览器验收 = 截图 + DOM 取证（一次性脚本，未进 `frontend/e2e`）。
4. **备份重现步骤**：`createdb -T cloudpivot_test cp_m518_smoke` → 用当前 HEAD 的二进制首启（自动补迁移到 V0465）→
   `update sys_config set config_value='true' where config_key='erp.cost.enabled'` → **重启进程**（开关有缓存）→
   跑 `smoke_sheet.py` / `seed_demo.py`。演练库已 drop。
