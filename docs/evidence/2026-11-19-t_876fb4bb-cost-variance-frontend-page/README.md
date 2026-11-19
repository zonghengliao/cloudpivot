# M5-19 差异分析：前端页 — 交付证据

> 卡：`t_876fb4bb`（M5 成本批第 19 张）· 日期：2026-11-19 · 主机：99（开发机，PG 在 145）
> 演练库：`cp_m519_smoke`（145 上由 `cloudpivot_test` 克隆 → 326 条已应用迁移 → 本卡新二进制首启补到 **V0466**；跑完已清场）
> 上游：M5-11（`GET /business/erp/report/cost-variance` 逐物料差异报表）· M5-18（标准成本页 + 统一话术）

## 〇、一句话

把 M5-11 交的**只读差异报表**接成真页面：一屏内 **三类差异（材料 / 人工 / 制造费用）→ 整表合计 → 逐物料明细**，
页面**不重算任何公式**（连合计也不加总，直接吃后端 `totals`），并把四处「会被误读成 bug」的口径写在明面上
（期间必填 / 无批次≠差异为 0 / 差异率 `null` 不是 0% / 不可比行不进差异合计）。

## 一、改动清单

| 文件 | 类型 | 说明 |
|---|---|---|
| `frontend/src/views/business/erp/cost-variance/index.vue` | 新增 | 页面：筛选条（期间 / 关键字 / 状态四档）→ 汇总 6 卡 → 三类差异表 → 逐物料表（服务端分页 + 展开行）→ 页脚口径 |
| `frontend/src/views/business/erp/cost-variance/cost-variance-logic.ts` | 新增 | 纯逻辑（零 DOM / 零请求 / 零公式）：入参拼装 `buildQuery`、金额·单价·差异率·数量的显示口径、三要素行装配、汇总卡、四处话术、展开行事实块 |
| `frontend/src/api/business/erp/cost-variance.ts` | 新增 | `fetchCostVariance`（一条 GET，`/api/business/erp/report/cost-variance`） |
| `frontend/src/api/business/erp/cost-variance-kinds.ts` | 新增 | 与 `cost_standard_compare_dto.rs` 逐字段对齐的类型 + 词表（状态 / 方向 / 要素 / 四档筛选） |
| `frontend/src/router/index.ts` | 改（+14 行） | 手工注册 `business/erp/cost-variance` 路由（本仓没有 views 自动扫描，漏了就是点菜单 404） |
| `frontend/tests/unit/erp-cost-variance.test.ts` | 新增 | **28 条**纯单测（入参 / 差异率 / 金额口径 / 三要素顺序与补位 / 汇总 / 四处话术 / 展开行事实 / 词表） |
| `backend/app/migrations/business/V0466__cost_variance_menu.{up,down}.sql` | 新增 | 只碰 `sys_menu` / `sys_role_menu`：页面行 `/business/erp/cost-variance`（sort 74，读码复用 `cost-method:list`）+ 派生授权 + **祖先闭包** |
| `docs/evidence/2026-11-19-t_876fb4bb-cost-variance-frontend-page/` | 新增 | 本目录（README + 5 张截图 + DOM 取证 + 冒烟/迁移/门禁输出 + `smoke_variance.py` / `seed_demo.py`） |

**零后端代码改动**（`git diff --stat backend/app/src` 为空）、**零新路由**（后端）、**零新权限码**、**1 条迁移（只种菜单）**。

## 二、口径与决策（都是决定，不是待办）

1. **前端一个公式都不算**。差异额 / 差异率 / 三要素拆分 / 尾差 / 整表合计全由后端 `calc_of` / `totals_of` 出（M5-11 有 43 条后端测试 + 6 条真库用例钉着）。前端再算一遍只会造出两份口径 —— 「页面 3,299.99、接口合计 3,300.00」这种差异查起来要人命。
2. **合计吃后端 `totals`，不加总当前页**。报表是服务端分页的：拿当前页加出来的合计翻页就变，而财务会拿它跟凭证对。冒烟第 ⑦ 组专门钉了「翻页后合计不变」。
3. **不可比行不画 0**。`variance = null` 的行（缺标准 / 缺实际）差异列一律「—」：写 0 会被读成「没有差异」，真相是「缺一侧、根本没比」。三要素列同理。
4. **差异率 `null` 显示「—」+ tooltip**。后端 `rate_of` 在标准额为 0 时返回 `null`（数学上不成立），画成 `0.00%` 会被读成「完全达标」。
5. **`cost_ready=false` 是 warning 条 + 「去成本核算」按钮**，不是「差异为 0」。后端此时仍列标准侧（全部「缺实际」），页面必须说清原因与下一步。
6. **合计卡只在有行时画**（`total > 0`）。0 行时一排 `0.00` 只是噪声；空态文案与引导条已经把「为什么」说清了（见 `04_no_cost_batch_hint.png`）。
7. **三要素永远三行（料 / 工 / 费）**，后端漏一行就补占位并显示「—」，**不补 0**：补 0 会让「漏了一行」看起来像「没有差异」（单测钉住）。
8. **筛选条不由 `ListPage` 渲染**（`:filters="[]"` + `:exportable="false"` ⇒ 组件筛选卡整块不画）：本页要「先看合计、再看明细」，合计卡必须落在筛选条与明细表之间；查询 / 重置直接驱动 `listRef.search()`，入参统一由 `buildQuery` 拼（有单测）。
9. **页面不打第二个请求**。行内展开用的是同一条响应里已带回来的 `variance.elements` / `standard` / `actual` —— 钻取（某物料某批的六类差异）要等 `biz_cost_variance` 落表后另开子路径（M5-11 模块头已写明「不改本路径出参结构」）。
10. **权限码复用 `business:erp:cost-method:list`**（零新增）：与 M5-11 的接口闸逐字一致；新开 `cost-variance:list` 属「页面码 / 接口码解耦」（PERM-01 只在 HR 试点），是独立的一张卡。
11. **菜单必须先有页面才种**（V0466）：先插一条指向不存在组件的菜单行，侧栏点开就是 404（`system::menu::tests::n08` 真库档当场红）—— 与 V0464 的理由逐字相同。

### 本卡顺带修掉的一个真 bug（祖先闭包）

V0390 建「成本核算」目录时用 `gen_random_uuid()` 且**没给任何角色授权**；而 `/system/auth/menus` 用 `utils::build_tree` 组装树，
**父节点不在结果集里的节点会被整块丢掉**。实测后果：既有 6 个成本页面（成本对象 / 工时 / 费用 / 分摊规则 / 成本核算 / 批次成本 / 标准成本）
的行都授出去了，但因为 `/business/erp/cost` 一行授权都没有，非超管登录后**整棵成本子树在侧栏里静默消失**（不是灰、不是 403）。

V0466 用递归 CTE 沿 `parent_id` 上溯把祖先补授（只补给「拿到本页的角色」），并在自检 §3.7 里断言
「授了页面没授祖先 = 0 处」。真机复核：`e2e01_appr` 的菜单树在修复前 `含 cost-variance=False`，修复后 `=True`；
只持 EMPLOYEE 的 `e2e06_emp_only` 仍然是 `False`（他不该看到）。**不新增任何页面 / 按钮**。

## 三、验收（全部真跑）

| 证据文件 | 内容 | 结果 |
|---|---|---|
| `07_smoke_variance.out` | 真实例 `127.0.0.1:8023` + 真库 `cp_m519_smoke` 跑 `smoke_variance.py` | **46 PASS / 0 FAIL** ✅ |
| `08_typecheck.out` | `pnpm typecheck`（src/**）· `pnpm typecheck:unit`（tests/unit/**） | rc=0 / rc=0 ✅ |
| `09_unit_and_build.out` | `pnpm test:unit` · `pnpm build` | **2453 passed / 0 failed**（本卡 +28；本卡之前 2425）· build rc=0 ✅ |
| `10_migration_and_menu.out` | V0466 应用记录 / 菜单行形态 / 派生授权 + 祖先闭包 / **幂等重放** / `menu_invariants.sh` | 全绿（8/8），重放后页面行仍 =1、授权行仍 =3 ✅ |
| `01_overview.png` | 真浏览器：一屏四段（筛选 → 汇总 6 卡 → 三类差异 → 明细 3 行），表 `scrollWidth == clientWidth`（无横向截断） | ✅ |
| `02_expanded_row.png` | 真浏览器：展开可比行 —— 左标准（版本 / 三要素单位成本 / 工时·工资率）、右实际（行数 / 完工 / 三要素 / 投入 / 在制）、行内三要素表、单位口径 + 尾差 | ✅ |
| `03_filter_keyword.png` | 真浏览器：关键字 `E2E02-M1` + 查询 → 共 1 条（重置回 3 条） | ✅ |
| `04_no_cost_batch_hint.png` | 真浏览器：切到 2026-10（无成本批次）→ warning 条 + 「去成本核算」+ 合计卡隐藏 + 空态原因 | ✅ |
| `05_filter_no_standard.png` | 真浏览器：状态下拉切「缺标准」→ 共 1 条，汇总卡同步（可比 0 / 缺标准 1 / 缺实际 0） | ✅ |
| `06_dom_checks.out` | DOM 取证：面包屑 / 汇总 6 卡文本 / 范围行 / 口径 / 三类差异表 / 明细表（表头 + 3 行）/ 展开行 / 「共 N 行」/ 页脚口径 | ✅ |
| `scripts/check.sh --fast` | L0 + L1 提交前门禁 | 全部通过 ✅ |

### 46 条 HTTP 冒烟覆盖了什么（`07_smoke_variance.out`）

| 组 | 断言要点 |
|---|---|
| ① 期间必填 | 缺期间 → 400 + `10001`；`2026-13` → 400（页面据此「不发请求」） |
| ② 无批次期间 | `cost_ready=false`、`batch=null`、`note` 指向 `/cost-period/calc` |
| ③ 可比行 | 标准 19.5×800=**15600**、实际 **16320**、差异 **720**（不利）、率 **0.046154**（比值不是百分数）、单位差异 0.9、尾差 0；三要素 400/320/0 且**相加 = 720**；标准侧带版本号与 `switch_in_period` |
| ④ 缺标准行 | `NO_STANDARD` + `standard=null` + `variance=null` + 实际 300 + hint 指向「标准成本」页「发布」 |
| ⑤ 缺实际行 | `NO_ACTUAL` + `actual=null` + hint 指向成本核算 |
| ⑥ 合计 | rows 3 / 可比 1 / 缺标准 1 / 缺实际 1；金额只算可比行；缺标准的实际额**单列** 300；note 写符号约定与「不入账」 |
| ⑦ 筛选 / 关键字 / 分页 | comparable（**可比且有差异**）/ no_standard / no_actual 各命中 1 行；非法 state → 400；关键字命中 1 / 无命中 0；`page_size=1` 翻页 total 恒定 3、两页不同行、**合计不随页变**、可比行排第一 |
| ⑧ 权限 | 只持 EMPLOYEE 的账号 → **403**（菜单不给他，接口也拦） |
| ⑨ 清场 | 夹具删净后当月 0 行 + `cost_ready=false` + 三张表零残留 |

### 浏览器验收怎么做的（不往页面输口令）

HTTP 登录取 token（`/system/auth/login`）→ 注入 `localStorage`（`x_access_token` / `x_refresh_token`）→ 打开
`http://127.0.0.1:5173/business/erp/cost-variance`（vite dev 代理到后端 8023）。UI 侧验证了四条链路：
**期间切换**（点月份面板 → 2026-10/2026-11）、**关键字 + 查询**（共 3 条 → 1 条）、**重置**（→ 3 条）、
**状态下拉**（缺标准 → 1 条，汇总同步）。首登的「待处理」与「新功能引导」两个弹窗先关掉再截图。

## 四、边界（明确不做，属别的卡）

- **导出 xlsx**：设计 §5.4 的「差异报表 xlsx（二期）」；本页只有 `ListPage` 默认的「导出当前页 CSV」，服务端导出另开卡。
  另：本页没有后端导出路由，`:exportable="false"` 关掉了默认导出（前端手里只有一页，假装能导全量就是骗人）。
- **钻取到「某物料某批的六类差异」**：要等 `biz_cost_variance` 落表后另开 `/report/cost-variance/{...}` 子路径（M5-11 已留好扩展位）。
- **期间控件不可清空**：月份选择器 + 默认本月。页面里 `!periodReady` 的引导条与 `buildQuery → null` 都是**防守分支**
  （控件被换掉 / 传了非法值时页面仍说人话，而不是直接打 400），正常操作下期间永远有值。
- **Playwright 入库用例**：属 M5-34（标准成本 E2E）。`seed_demo.py` 的三条造数（可比 / 缺实际 / 缺标准）可直接作夹具。
