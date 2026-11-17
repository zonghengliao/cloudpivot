# t_8dcb79d5 · 采购申请建单明细：t-table 插槽名与 colKey 不匹配（明细行没有输入控件）

日期：2026-11-18　卡：t_8dcb79d5（M4-06 采购计划页面卡 t_62aa0328 里顺带发现的存量缺陷）
改动文件：`frontend/src/views/business/erp/purchase-requisition/index.vue`（7 行）

---

## 一、现象（真浏览器才看得见）

「采购申请」页 → 新建申请 → 明细表格：

| 列 | 修复前 | 修复后 |
|----|--------|--------|
| 物料 | **空白**（裸值 `''`） | 下拉框，选中后显示「M4PRQ-MAT-A M4PRQ 物料 A」 |
| 数量 | 文本 `1` | `t-input-number`，带步进器，显示 `3.0000` |
| 预估单价 | 文本 `""` | `t-input-number`，显示 `2.500000` |
| 税率% | 文本 `13` | `t-input-number`，显示 `13.0000` |
| 需求日期 | 文本 `""` | `t-date-picker`，点开有日历面板 |
| 建议供应商 | 文本 `""` | 下拉框 |
| 操作 | **没有「删除」链接** | `删除` 链接（`t-link theme=danger`） |

⇒ 明细行**等于没有任何输入控件**：用户在页面上填不了物料，建单走不通（只能靠别处转单）。

## 二、根因

```vue
<!-- 修复前：插槽名是自定义的 camelCase -->
<t-table row-key="key" :data="createForm.lines" :columns="lineColumns">
  <template #lineMaterial="{ row }">…</template>
  <template #lineQty="{ row }">…</template>
  …
```

而同一文件里的列定义是：

```ts
const lineColumns: PrimaryTableCol[] = [
  { colKey: 'material_id', … }, { colKey: 'qty', … }, { colKey: 'est_price', … },
  { colKey: 'tax_rate', … },    { colKey: 'demand_date', … },
  { colKey: 'suggest_supplier_id', … }, { colKey: 'ops', … },
];
```

**TDesign `t-table` 是按列的 `colKey` 去找单元格插槽 `#<colKey>`**。名字对不上时：

- 插槽**静默失效**（没有任何 warning / 报错）；
- 单元格回落到默认渲染 = 把字段值当文本输出（物料 id 是空串 → 空白；`qty` 的 `1` 直接写成文本）；
- `vue-tsc`（`pnpm typecheck`）与 `pnpm build` **全绿**：模板里多一个没人用的具名插槽不是类型错误。

所以这类缺陷**只有真浏览器**（或读源码的契约单测）能发现。同机制说明见
`docs/evidence/2026-11-18-m406-purchase-plan-ui/README.md` §五 与
`skill cloudpivot-backend-workflow/references/frontend-page-card-e2e.md` §6。

## 三、修复

7 个插槽名改成对应 `colKey`（一次 sed 级别的机械改名，业务口径 / 审批链 / 合并转单逻辑一行未动）：

```
#lineMaterial -> #material_id
#lineQty      -> #qty
#linePrice    -> #est_price
#lineTax      -> #tax_rate
#lineDemand   -> #demand_date
#lineSupplier -> #suggest_supplier_id
#lineOps      -> #ops
```

## 四、同仓排查（`30_scan_slots.py`，结果见 `31_scan_after_fix.out`）

判定：解析每个 `.vue` 里 `<t-table :columns="X">` 的 `X`（支持 `: Type = [`、别名 `A = B`、
展开 `...IMPORTED`、`@/` 导入的常量模块与 `export *` 再导出），取该表的 `colKey` 集合，与表内
`<template #xxx="{ row }">` 的插槽名比对；嵌套子表单独判定；忽略内置插槽（`expandedRow` 等）。

规模与结论（`31_scan_after_fix.out` 首两行）：

```
扫过 93 张「有自定义插槽的 t-table」/ 494 个单元格插槽；其中 91 张按表级解析出 colKey、2 张回退文件级池
带输入控件且插槽名≠colKey 的槽子：0 个
```

**带输入控件（`t-select` / `t-input-number` / `t-date-picker`）的同类错误，全仓只有本文件这 7 个**，
已全部修掉。剩余 494 个自定义单元格插槽里，除下面那张表列的 9 个（只做格式化、无输入控件）之外，
名字都在各自表的 `colKey` 集合里。

### 顺带查到（**本卡按「不夹带」未改**，同一文件、同一机制、但只影响格式化）

| 表 | 插槽 | 该表的 colKey | 现在的实际表现 |
|----|------|---------------|----------------|
| 合并转采购单弹窗 `mergeColumns` | `#mergeQty` `#mergePrice` `#mergeSources` | `qty` `est_price` `source_req_nos` | 数量/单价不套 `decimalText`（直接显示后端原始小数位） |
| 详情抽屉 `detailItemColumns` | `#dQty` `#dPrice` `#dPriceIncl` `#dAmount` `#dSupplier` `#dPoItem` | `qty` `est_price` `est_price_incl` `amount` `suggest_supplier_id` `po_item_id` | **「建议供应商」列显示的是 UUID** 而不是供应商名；金额不套 4 位小数；采购单行不显示 `—` |

这三组不含输入控件、不影响「能不能建单」，属于另一类（展示层）缺陷，留给后续卡（建议一起改，
改动同样是 9 行机械改名 + 一条 e2e 断言「详情抽屉的建议供应商列不是 UUID」）。

## 五、验收（全部真跑）

| 项 | 命令 | 结果 |
|----|------|------|
| 类型检查 | `pnpm typecheck` | rc=0 |
| 单测类型检查 | `pnpm typecheck:unit` | rc=0 |
| 构建 | `pnpm build` | rc=0 |
| 全量单测 | `pnpm test:unit` | 见 §5.4 |
| 源码契约单测 | `npx tsx --test tests/unit/erp-purchase-requisition-lines-slot.test.ts` | 3/3 pass（**修复前 2 条红**，见 §5.1） |
| 真浏览器 | `bash scripts/verify/m4_prq_lines_ui_drill.sh` | **PASS=17 FAIL=0**，Playwright 5/5 |

### 5.1 红 → 绿（不是只交绿）

把改前的文件 `cp` 回来跑一遍契约单测，再恢复：

```
== 旧实现（bug 现场）跑契约单测 ==
not ok 1 - ① 建单明细的插槽名必须逐字等于 lineColumns 的 colKey
ok     2 - ② 带输入控件的槽子必须挂 data-testid
not ok 3 - ③ 反证：旧的自定义命名（#lineMaterial / #lineQty …）不许再出现
# pass 1 / # fail 2
== 恢复修复后 ==
# pass 3 / # fail 0
```

### 5.2 真浏览器用例（`frontend/e2e/m4-prq-create-lines.spec.ts`，5 条）

1. **① 明细行真的有输入控件**：6 个控件逐个 `toBeVisible`；物料下拉点开 → 选中 → 读回选中值；
   数量/单价/税率逐字敲进去 → 失焦后按数值断言；需求日期点开日历面板 → 选一天 → 输入框回填 `YYYY-MM-DD`；
   建议供应商下拉能选；「删除」链接在。
2. **② 空提交被拦**：什么都不填 → 「请选择申请部门」；补单头 → 「第 1 行：请选择物料」；弹窗不关。
3. **③ 值真的进了报文**：拦 POST 读 `postData`，明细 `qty=3` / `est_price=2.5` / `material_id` 非空；
   响应 200 → 拿到 `PRQ-…` → 列表立刻出现这张单。
4. **④ 加一行 / 删一行**：新行控件同样在；删除回到 1 行。
5. **⑤ 取证截图**：`01-lines-controls.png`（明细行控件）、`02-material-dropdown.png`（物料下拉展开）。

### 5.3 一键演练（`scripts/verify/m4_prq_lines_ui_drill.sh`）

空库 `cp_m4prq_lines`（145）→ 隔离后端 `:18102`（自己跑完迁移）→ 夹具（部门/仓库/物料/供应商）
→ 接口自检（列表/物料/供应商/部门树 200，且下拉里能取到夹具）→ vite `:5191` 反代 → `/api` 未带令牌 401
→ 契约单测 → Playwright 5/5 → **落库复核**：浏览器在明细行里填的 `3` / `2.5` / `13` 真在
`biz_purchase_requisition_item` 里（`3.0000 | 2.500000 | 13.0000`）。

全量输出：`00_drill_full.out`；Playwright：`20_playwright.out`；契约单测：`10_unit_contract.out`。

### 5.4 全量单测

`pnpm test:unit` → 2290 passed / 0 failed（原 2287 + 本卡新增 3 条契约用例）。

## 六、这次踩到 / 复核过的 e2e 坑（补进 §6 那三条）

- **`t-select` 上不能断 `toContainText`**：`data-testid` 落在 `.t-select__wrap` 上，选中项文字在里面的
  只读 `<input>` 的 **value** 里，`textContent` 是空串（断言会一直等到超时）。
  读法：`getByTestId(x).locator('input').first()` → `toHaveValue(...)`。
- **`t-input-number` 不能 `.fill()`**：初版 `fill('3')` 在默认值 `1` 上变成了 `13`
  （受控组件在 input 事件后又用旧 `modelValue` 回写一次，一次性的赋值被吞）。
  正解：点进去 → `ControlOrMeta+a` → `Backspace` → `pressSequentially` → `blur()`（失焦才 commit）。
- **别断言 `t-input-number` 的字面值**：它按 `decimal-places` 补齐（数量 `3.0000`、单价 `2.500000`），
  断言要比数值。
- **演练脚本的退出码不要靠最后一条 `[ ... ]`**：`EXIT` trap 里 `ss | grep` 未命中会让脚本以 1 退出
  （明明 PASS=17 FAIL=0 却 rc=1）。显式 `exit 0/1`。

## 七、观察到但不属于本卡

- 明细 7 列宽度合计 1050px > 弹窗 1000px，**「操作」列要横向滚动**才能看见（截图 01 里就是被裁掉的）。
  能点（Playwright 会滚动进视口），但体验一般；调列宽属于另一张「像素级」卡的事。
