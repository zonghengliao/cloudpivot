# FIX-02：5 条 E2E 红归因（只查不改）

> **任务**：t_edb61eaa ｜ **执行**：test-enginner ｜ **性质**：定性报告，**未改任何业务代码 / 未改任何 spec**
> **来源**：测试E 全量一轮 398 通过 / 5 失败 / 5 跳过（t_4c9d4e71，2026-10-31）
> **本次复跑**：HEAD = `881931d`（2026-11-01 01:29），隔离实例实跑，5 条**全部原样复现**，无一条自愈

## 结论速览

| # | Spec（卡面行号 → HEAD 实测行号） | 归因 | 一句话证据 | 处置 |
|---|---|---|---|---|
| 1 | `08-t8b-task-tree.spec.ts:94` → **95/102** | **A 断言过时**（两处） | 箭头已按老大要求从标题列(第1列)挪回**任务号列(第0列)**：`dcda130`；且父行底色改成 `color-mix(品牌 6%)`，断言还在比 `--td-brand-color-light` | 改断言：`td[0]` + 比 `color-mix 6%`（见 §1） |
| 2 | `08-t8b-task-tree.spec.ts:132` → **133/137** | **A 断言过时**（一处） | 同上：`.t-table__tree-op-icon` 在 `td[0]`，断言点在 `td[1]` → 0 个元素，click 15s 超时 | 改断言：`titleCellOf` 换成第 0 列（见 §2） |
| 3 | `09-t8c-root-only.spec.ts:66` | **C 夹具/数据漂移** | 实数 31 顶层 / 48 子 / 79 存活（spec 写死 28/15/43）；第 1 页 10 行**今天一个父任务都没有** | 改相对断言 + 自造夹具（见 §3） |
| 4 | `09-t8c-root-only.spec.ts:169` | **C 夹具/数据漂移** | 看板子任务行实测 33（spec 写死 `43-28=15`）；**"不传 root_only" 这半条是过的** | 同上（见 §4） |
| 5 | `10-erp-warehouse.spec.ts:118` → **120/130** | **A 断言过时** | `V0156__remove_erp_top_menu`：ERP 目录已删，"仓库管理"提到**根级**；页面本身完好（WH01/WH02/WH-TRANSIT 正常渲染） | 改菜单路径（见 §5） |

**没有一条 B（真回归）→ 本卡不另建修复卡。** 需要改的是 3 条断言（第 1/2/5）和 2 条数据口径（第 3/4），建议另开一张「FIX-03 更新 5 条 E2E 断言」卡统一收口（本卡按"只查不改"不落代码）。

---

## 一、复跑环境（可复现）

| 项 | 值 |
|---|---|
| 代码版本 | `881931d`（HEAD，工作区干净） |
| 副本库 | `cloudpivot_e2e_iso2`（生产 2026-10-31 12:44 快照，`/tmp/e2e-iso/backend2.env`） |
| 后端 | `backend/target/debug/cloudpivot`，profile=test，`127.0.0.1:18089`（启动时自动补迁 V0160/V0171 乱序迁移，V0174 已 applied） |
| 前端 | `vite --port 5189`，`BACKEND_ORIGIN=http://127.0.0.1:18089` |
| 判据 | `playwright.config.ts` 硬判据通过：`[e2e-target] OK 隔离实例：http://127.0.0.1:5189（回环 + 隔离端口 5189）` |
| 跑法 | `E2E_BASE_URL=http://127.0.0.1:5189 npx playwright test e2e/08-t8b-task-tree.spec.ts --reporter=line`（08 整文件）/ 单条 `:66`・`:169` / `10-erp-warehouse.spec.ts` |
| 日志 | `/tmp/e2e-iso/fix02-run1.log`、`fix02-08.log`、`fix02-10.log`；现场 `frontend/test-results/…`（screenshot + trace + error-context.md，最后一跑已另存 `/tmp/e2e-iso/test-results.head-fix02/`） |
| 数据无污染 | 复跑前后 `biz_task` 存活行数恒为 **79**，`[T8B-` 残留 **0**（08 spec 的 afterAll 清理有效） |

> **行号偏移说明**：卡面记的 `94:3 / 132:3 / 118:1` 是报告那一轮的；HEAD 上因 spec 有 +1 行改动，实测为 `95:3 / 133:3 / 120:1`。同一批用例，不是两条红。

---

## 二、逐条归因

### ① `08-t8b-task-tree.spec.ts:95` › 箭头在标题列、父行 4px 竖条 + 浅底 + 加粗、进度列 1/3 小进度条

**真实报错（HEAD 复跑原文）**

```
Error: expect(locator).toHaveCount(expected) failed

Locator:  locator('.t-table__body > tr').filter({ has: locator('.cell-title__text').filter({ hasText: '[T8B-t8bmvwoiwxi]根任务' }) }).locator('td').nth(1).locator('.t-table__tree-op-icon svg')
Expected: 1
Received: 0
Timeout:  15000ms

Call log:
  - waiting for locator('…').locator('td').nth(1).locator('.t-table__tree-op-icon svg')
    34 × locator resolved to 0 elements
       - unexpected value "0"

    > 102 |     await expect(titleCell.locator('.t-table__tree-op-icon svg')).toHaveCount(1);
```

**归因：A 断言过时（两处，第 1 处在第 102 行就红了）**

依据 1 — 箭头列被老大改了，代码里有明确记录：

```
$ git log --oneline -3 -- frontend/src/views/business/task/components/tree-layout.ts
dcda130 style(task): 树形箭头从标题列移到任务号列（老大：放任务号前面更好）
609a366 feat(task): 任务列表树形改造（箭头移标题列 + 父行竖条 + 子行连接线 + 子任务进度条）

$ git show --stat --format='%h %ci %s' dcda130
dcda130 2026-10-30 19:44:27 +0800 style(task): 树形箭头从标题列移到任务号列（老大：放任务号前面更好）
  tree-layout.ts
    titleColumnIndex（找 title，第 1 列） → treeArrowColumnIndex（找 task_no，第 0 列）
  真机验证：父行 箭头列=0 有真箭头（第0列是 TSK-xxx）
```

`tree-layout.ts` 现在的实现与注释（`frontend/src/views/business/task/components/tree-layout.ts:80-107`）：

```ts
// **2026-10-30 老大改主意**：T8-B 原本把箭头挂在标题列…但实际用起来觉得放任务号前面更顺手
// 所以改回任务号列（现在是第 0 列）。
export function treeArrowColumnIndex(colKeys) {
  const index = colKeys.findIndex((key) => key === 'task_no');
  return index >= 0 ? index : 0;
}
```
`index.vue:433-434` 的列顺序确认：`{colKey:'task_no'}` 是第 0 列，`{colKey:'title'}` 是第 1 列。**spec 的 `titleCellOf = td.nth(1)` 是旧口径。**

依据 2 — 现场直采（真机 DOM，只读探针；父任务「ERP 严谨性治理（五条铁律 + 三层检查）」，6 个子任务）：

| 断言点 | spec 期望 | 真机实测 | 判定 |
|---|---|---|---|
| 箭头 svg 在第 1 列 | 1 | **0**（`td[1]` 只有 `.cell-title`，没有树形容器） | 断言过时 |
| 箭头 svg 在第 0 列 | — | **1**（`span.t-table__tree-op-icon > svg.t-icon.t-icon-add-rectangle`，16×16） | 功能正常 |
| 行 class | `/task-row--parent/` | `t-table-tr--level-0 task-row--parent` ✅ | 通过 |
| 首列 4px 竖条颜色 | `4px` + `--td-brand-color` | `border-left: 4px solid rgb(125,82,102)`，token `--td-brand-color=#7d5266=rgb(125,82,102)` ✅ | 通过 |
| 标题加粗 | ≥600 | `600` ✅ | 通过 |
| 父行浅底 | `= --td-brand-color-light`(rgb(205,187,195)) | **`color(srgb 0.943608 0.929804 0.93451)`** ＝ `color-mix(品牌 6% + 容器色)`，**不等于** brand-light | 断言过时（第 2 处） |
| 子任务进度 | `1/3` + `33%` | 该父行 `0/6` + `0%`（`.sub-progress` + `width:0%`，结构一致） | 功能正常（数字取决于夹具，不是缺陷） |
| 10 列 + 4 个视图 Tab | 10 / 4 | `thead th` = `任务号,标题,状态,优先级,进度,负责人,项目,截止日期,来源,操作` = 10；视图 Tab 4 个 | 通过 |

依据 3 — 浅底为什么变成 color-mix（`index.vue:1002-1015`）：

```css
:deep(.task-row--parent > td) {
  /* 父行底色：品牌色 6% 混白（老大定稿：4% 太淡，6% 刚好）。
     原来用 --td-brand-color-light（≈10% 品牌色），视觉上偏重、和子行的灰底拉不开层次。 */
  background-color: var(--td-brand-color-light);                  /* 老浏览器回退，被下一行覆盖 */
  background-color: color-mix(in srgb, var(--td-brand-color) 6%, var(--td-bg-color-container));
}
```
token 实测：`--td-brand-color=#7d5266`、`--td-brand-color-light=#cdbbc3`、`--td-bg-color-container=#f8f7f7`；真机把同一句 color-mix 打到探针元素上得到的值与行上取到的值**完全一致**（`color(srgb 0.943608 0.929804 0.93451)`）→ CSS 生效、断言口径旧。

**判断依据小结**：功能（箭头/竖条/加粗/进度条/列数）**全部在**，只是箭头所在列与底色实现按需求变了 → A，不是 B。

**处置（交给 FIX-03 卡）**：`titleCellOf` 改成第 0 列（或用 `colKey` 反查），箭头/连接线/缩进断言全部移到 `td[0]`；浅底断言改成与同一句 `color-mix` 探针值比较（或直接放宽为"底色 ≠ 容器色且与 color-mix 一致"）。

---

### ② `08-t8b-task-tree.spec.ts:133` › 逐节点折叠展开：展开出 3 条子任务（连接线 + 缩进 24px），收起后兄弟不受影响

**真实报错（HEAD 复跑原文）**

```
TimeoutError: locator.click: Timeout 15000ms exceeded.
Call log:
  - waiting for locator('.t-table__body > tr').filter({ has: locator('.cell-title__text').filter({ hasText: '[T8B-t8bmvwojah9]根任务' }) }).locator('td').nth(1).locator('.t-table__tree-op-icon')

    > 137 |     await titleCellOf(rowOf(page, '根任务')).locator('.t-table__tree-op-icon').click();
```

**归因：A 断言过时（与①同根，一处）**

真机把同一套断言**换到第 0 列**后逐条实测（同一父任务，6 个子任务）：

| 断言点（spec 原文口径） | 真机实测（第 0 列） | 判定 |
|---|---|---|
| 点箭头能展开出子行 | 点 `td[0] .t-table__tree-op-icon` → 立刻出现 **6 行** `.task-row--child` | 功能正常 |
| 子行 class | `t-table-tr--level-1 task-row--child` ✅ | 通过 |
| 子行连接线 `::before` | `border-left: 1px`、`width: 12px`、`height: 12px` ✅ | 通过 |
| 子行缩进 | `padding-left: 24px`（在 `td[0]`，`td[1]` 无） ✅ | 通过 |
| 子行底色 | `rgb(244,243,242)` ＝ `--td-bg-color-secondarycontainer`(#f4f3f2) ✅ | 通过 |
| 再次点击收起 | 子行数 → **0**（同级行不受影响） ✅ | 通过 |

**判断依据小结**：spec 断言的每一项行为（展开/子行 class/连接线/缩进/收起）**都在且数值与 spec 完全一致**，唯一失配的是"在哪个 td 里找箭头"。→ A。

**处置**：`titleCellOf` 改成第 0 列即可，其余断言可原样保留（数值都是对的）。

---

### ③ `09-t8c-root-only.spec.ts:66` › 列表第 1 页 = 5 个带真箭头的父任务；展开出 3 个子行；total 28

**真实报错（HEAD 复跑原文）**

```
Error: 工具条计数应显示顶层数 28：全部可见任务 · 共 31 条

expect(received).toContain(expected) // indexOf
Expected substring: "共 28 条"
Received string:    "全部可见任务 · 共 31 条"

    > 73 |     expect(countText, `工具条计数应显示顶层数 28：${countText}`).toContain('共 28 条');
```

**归因：C 夹具 / 数据漂移（绝对数字写死 + "第 1 页必是 5 个父任务"这个数据假设没了）**

依据 1 — 库内实数（副本库直查，只读）：

```
alive_total|79      roots|31      children|48
（spec 写死：43 = 5 父 + 15 子 + 23 独立 ⇒ roots 28 / children 15）
```

依据 2 — 31 与 28 的差**恰好是别人留下的夹具残留**：

```
普通标题      roots 28 / children 41
[E2E]-前缀    roots  2 / children  7     ← spec 06 那批 "[E2E]详情-e2emvvv*" 没清掉
[自检]-前缀   roots  1 / children  0     ← "[自检] 父B"
```
28 + 2 + 1 = **31**（与工具条"共 31 条"逐字对上）；而 children 41 ≠ 15 则是**数据增长**（推断：副本库是生产 12:44 快照，比 spec 写数那一刻更新；无法回放 spec 写数时刻的库，故此条按"数据增长"解释，未做进一步考古）。→ 两类漂移叠加，都归 C。

依据 3 — 即使把 28 改成 31，**后面半条仍然会红**：真机第 1 页实测

```
listPage1.rows      = 10
listPage1.parentRows= 0          ← spec 期望 5 个父任务
listPage1.realArrows= 0
第 1 页标题 = 整理季度绩效材料 / T13 / T12 / T11 / [自检] 父B / T10 / T9-B / T9-A / T8-B / T8-A
```
这 10 条今天**全是叶子任务**（0 个有子任务），所以"点第 1 个父任务展开 3 个子行"这步也无从谈起。这是数据/排序变化，不是树形功能坏。

依据 4 — 这条用例真正要验的东西是**过的**：`GET /api/business/task/page?page=1&page_size=10&root_only=true` 确实带 `root_only=true`，且 `total=31` 是顶层计数（不是 79）。

**判断依据小结**：spec 头注释自己写着"数据：不造数、不清理（只读），数据源是…只读副本库（43 = 5 父 + 15 子 + 23 独立）"——它把一份**会变的共享库**当成了常量。库一变（别的 spec 残留 3 条 + 生产增量），断言必红。→ C。

**处置（FIX-03）**：两类改法二选一
1. **自造夹具**（推荐）：beforeAll 用 API 造"根 + 3 子"，按 keyword 过滤后再断言（像 08 spec 那样），断言写相对量：`total >= 父任务数`、`第 1 页所有行 parent_id 为空`；
2. **保留只读**但改成相对断言：`expect(total).toBe(父任务真实数)` 由接口自己算，或断言"分页 total < 全量 total（79）"且"列表请求带 root_only=true"，并删掉"第 1 页 5 个父任务 / 每个 3 子"这类写死假设。

---

### ④ `09-t8c-root-only.spec.ts:169` › 看板 / 甘特 / 燃尽图不传 root_only，数据量不变（全部 43 条）

**真实报错（HEAD 复跑原文）**

```
Error: expect(locator).toHaveCount(expected) failed

Locator:  locator('.board .subtasks__row')
Expected: 15
Received: 33
Timeout:  30000ms

  - waiting for locator('.board .subtasks__row')
    63 × locator resolved to 33 elements
       - unexpected value "33"

    > 186 |     await expect(page.locator('.board .subtasks__row')).toHaveCount(EXPECT_TOTAL_ALL - EXPECT_TOTAL_ROOTS, {
```

**归因：C 夹具 / 数据漂移（`EXPECT_TOTAL_ALL - EXPECT_TOTAL_ROOTS` = 43-28 = 15 是写死的）**

依据 — 真机实测（同一套网络监听，只读）：

| 断言点 | spec 期望 | 真机实测 | 判定 |
|---|---|---|---|
| 看板不带 root_only | `[]` | `[]` ✅（5 条 `/task/page?…&status=XXX` 全无 root_only，另加 15 条 `/task/{id}/subtasks`） | **通过** |
| 甘特不带 root_only | `[]` | `[]` ✅（`/task/gantt?scope=all`） | **通过** |
| 燃尽不带 root_only | `[]` | `[]` ✅（`/task/burndown?board=all`） | **通过** |
| 看板子任务行数 | 15 | **33**（15 个有子任务的卡片各自懒加载，逐父合并 33 行） | **红（数据口径）** |
| 看板工具条 | "共 43 条" | **"共 79 条"**（= 31 顶层 + 48 子，与库内实数一致） | 红（数据口径） |

**判断依据小结**：这条用例想守的"三个视图不传 root_only、一条不少"**是全部通过的**；唯一的红是最后那句把总任务数写死成 43（其中 28/15 的拆分同样过时）。→ C，不是接口/前端回归。

**处置（FIX-03）**：把 `EXPECT_TOTAL_ALL/ROOTS` 换成运行时从接口拿的真值（例如先 `GET /task/page?page_size=1` 读 `total`，再看板断言 `subtasks__row > 0` 且 `sum(各卡 child_count) === subtasks__row`），或改为"看板/甘特/燃尽三处请求都不含 root_only"这一条硬断言（这才是本用例的核心）。

---

### ⑤ `10-erp-warehouse.spec.ts:120` › W4a：仓库树 + 库位页 › ① 菜单进入「ERP 主数据 → 仓库管理」

**真实报错（HEAD 复跑原文）**

```
TimeoutError: locator.click: Timeout 15000ms exceeded.
Call log:
  - waiting for getByText('ERP 主数据').first()

    > 130 |     await page.getByText('ERP 主数据').first().click();
```

**归因：A 断言过时（菜单重组，ERP 目录已被整层删掉）**

依据 1 — 迁移里写得很清楚（`backend/migrations/business/V0156__remove_erp_top_menu.up.sql`）：

```sql
-- 老板要求（2026-10-31 08:30）：
--   「不需要ERP那个菜单目录。把采购管理、仓库管理、财务管理、基础数据放到根目录」
-- 改前：ERP > {采购管理, 仓库管理, 财务管理, 基础数据}
-- 改后：{采购管理, 仓库管理, 财务管理, 基础数据} 直接挂根级
UPDATE sys_menu SET parent_id = NULL, sort_order = 66 … WHERE id = '…e01';  -- 仓库管理
DELETE FROM sys_menu WHERE id = '…940' …;                                   -- 删掉 ERP 目录
```

依据 2 — 库内实况（副本库 `sys_menu`，只读）：顶级菜单里只剩 `采购管理 / 仓库管理 / 财务管理 / 基础数据`，**`ERP`（原 `ERP 主数据`，id …940）整行已不存在**：

```
顶级菜单：工作台, system, notice-center, support, 审批流, 工作任务, 采购管理, 项目, 仓库管理, content, 财务管理, 通用底座, 基础数据, 帮助中心, 外观设置
```

依据 3 — 真机侧边栏：`getByText('ERP 主数据').count() = 0`；根级「仓库管理」展开后子项为 `仓库管理 / 库位管理 / 序列号管理 / 即时库存 / 库存调拨`。

依据 4 — **页面本身没坏**（证明不是 B）：按新路径真实点进去

```
根级 .t-submenu__title「仓库管理」→ 子项 .t-submenu__item「仓库管理」点击
finalUrl = http://127.0.0.1:5189/business/erp/warehouse
[data-testid="warehouse-tree"] visible = true
.wh-node data-code = ["WH01","WH02","WH-TRANSIT"]
（直链 /business/erp/warehouse 同样正常，树渲染一致）
```

**判断依据小结**：spec 第 130 行的 `getByText('ERP 主数据')` 已经不存在（目录被删）；第 131 行的 `getByText('仓库管理').first()` 现在命中的是**根级目录本身**（真机实测：`.first()` 点击后 URL 仍是 `/dashboard`，只有点 `.t-submenu__item` 那一项才会跳到 `/business/erp/warehouse`），所以整段必须换成"根级目录展开 → 子项"的写法。→ A。

**处置（FIX-03）**：

```ts
await test.step('① 菜单进入「仓库管理 → 仓库管理」', async () => {
  await openAuthed(page, '/dashboard');
  await page.locator('.t-submenu', { hasText: '仓库管理' }).first().locator('.t-submenu__title').first().click();
  await page.locator('.t-submenu__item', { hasText: /^仓库管理$/ }).first().click();
  await expect(page).toHaveURL(/\/business\/erp\/warehouse/);
  await expect(page.locator('[data-testid="warehouse-tree"]')).toBeVisible();
});
```
（本机已用这段选择器实测通过，见 §5 依据 4。）

---

## 三、副产品 / 顺手发现（都不是 B，登记备查）

1. **注释文档债（不影响行为）**：`frontend/src/views/business/task/index.vue:446-452` 与 `:985` 的注释仍写"箭头挂在**标题列**"，但 `treeConfigOf` 实际反查 `task_no`（第 0 列）——`dcda130` 只改了代码没改注释。建议随 FIX-03 一并订正，避免下次又照着注释写断言。
2. **夹具残留（C 的源头）**：副本库里现存 9 条 `[E2E]` 前缀任务（2 父 + 7 子）与 1 条 `[自检] 父B` 未清，正好是第 ③ 条 `31 - 28 = 3` 的全部来源。这类残留会让**任何**依赖"总数"的只读断言随别人跑测试而红；根治办法是"依赖总数的断言一律自造夹具 + 按 keyword 过滤"。
3. **隔离实例是可再生的**：本次在副本库上跑时，后端启动补齐了乱序迁移（V0160/V0171 告警、V0174 applied），说明手起隔离库只要 `set -a; . /tmp/e2e-iso/backend2.env; ./target/debug/cloudpivot` 即可，迁移会自动补到 HEAD。

## 四、证据文件清单

| 文件 | 内容 |
|---|---|
| `/tmp/e2e-iso/fix02-run1.log` | 09 两条复跑原文（含 expected/actual） |
| `/tmp/e2e-iso/fix02-08.log` | 08 两条复跑原文 |
| `/tmp/e2e-iso/fix02-10.log` | 10-erp 一条复跑原文 |
| `/tmp/e2e-iso/test-results.head-fix02/` | HEAD 这一轮**最后一跑**（10-erp）的 screenshot + trace + error-context（`playwright test` 每轮都会清空 `frontend/test-results/`，08/09 那一轮的现场因此被后续跑覆盖；它们的**报错原文完整留在上面三个 log 里**，不改口径） |
| `/tmp/e2e-iso/test-results.prev-154351/` | 测试E 那一轮（报告来源）的 5 份现场，用于"两轮同红"对比 |
| `/tmp/e2e-iso/fix02-probe*.json` | 只读探针原始输出（DOM/token/网络请求/菜单结构） |
| `/tmp/e2e-iso/fix02-probe-tasklist.png`、`-probe2-expanded.png`、`-probe4-board.png`、`-probe5b-warehouse.png` | 探针截图 |

> 探针脚本 `frontend/fix02-probe*.cjs` 为临时文件，跑完已移出仓库（`/tmp/e2e-iso/probes/`）；`git status` 里只剩这份报告（+ 别的 agent 自己在途的 V0175/payable_bridge.rs，与本卡无关）——业务代码与 spec **一字未动**。
