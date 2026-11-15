# P3 · ListPage 页面边界决策（财务 4 页：会计科目 / 期初 / 年结 / 报表项）

**日期**：2026-11-15
**状态**：accepted
**决策人**：architect（卡 `t_c50edd08`，源自 P3-08 落地卡 `t_481d5de1` / 提交 `82774729`）
**影响面**：`frontend/src/components/ListPage/*`、财务 4 页、P3-09 ~ P3-12 四波迁移的验收口径

---

## 0. 结论（先看这段）

| 问题 | 结论 |
|------|------|
| `account`（会计科目）进 ListPage？ | **不进**。要的是「另一种表格实现 + 另一种数据模型」，不是给 ListPage 加个 prop |
| `report/report-item`（报表项）进 ListPage？ | **不进**，且**不引入 `paginated=false`**。它的语义是「有序配置表」而不是列表 |
| `opening`（期初余额）/ `year-end`（年度结转）抽 `EditableGrid` / `CheckPanel`？ | **不抽**，保持页面自绘。单消费者，抽出来就是为 1 个页面设计 DSL |
| 「筛选行 + 卡片外壳」的重复怎么办？ | 下沉为**全局样式**（`src/style/shell.less`），**不是**新组件 `list-shell`；时机挂在 P3-12 之后 |
| 本卡 | 可关。另开 2 张后续卡：①`report-item` 排序移错行（真 bug）②shell 样式下沉 |

一句话：**ListPage 只收「服务端分页的只读行列表」这一种形态。第二、第三种形态另立组件，不往 ListPage 里加 mode。**

---

## 1. 事实基座（都是这次实测出来的，不是回忆）

### 1.1 ListPage 现在长什么样

| 项 | 实测值 | 命令 |
|---|---|---|
| 已用页面 | **29** 个 view 引用（P3-08 收尾时 27；写稿时 P3-09 正并行迁成本 5 页，这个数还在涨） | `grep -rl "components/ListPage" frontend/src/views \| wc -l` |
| props | **23**（P3-04 建时 13，P3-05~08 四波各补几项，全部向后兼容） | `sed -n '/defineProps</,/}>(),/p' .../ListPage/index.vue` |
| 事件 | 6（`loaded` / `reset` / `refresh` / `selection-change` / `batch-done` / `row-click`） | 同上 |
| 插槽 | 8 个具名 + 2 个动态（`filter-<key>` / `cell-<colKey>`） | 同上 |
| 渲染的表格 | **只有 `<t-table>`**（PrimaryTable），一处 `t-enhanced-table` 都没有 | `grep -c t-enhanced-table .../index.vue` → 0 |

> 23 个 props 这个数字本身就是一条论据：组件四波下来从 13 涨到 23，**都是标量开关的加法**（`rowKey` / `metaUnit` / `rowClassName` / `FilterDef.defaultValue` 放开 `null`），这类加法的风险是线性的。而「换表格实现 / 换数据模型」是**分支乘法**——7 处插槽转发、空态、错误条、`共 N 条`、导出列、勾选列都要判两次。

### 1.2 剩下能吃到 ListPage 红利的量在哪

| 类别 | 数量 | 说明 |
|---|---|---|
| 已迁 | **29** | 采购 8 / 销售 8 / 仓库 8 / 财务 4 / 成本 1（在途） |
| **`index.vue` 带表格但没接 ListPage** | **41** | `budget` / `commission` / `cost-*` / `currency` / `expense` / `hr/*` / `flow/*` / `task` / `impexp` / `project` … |
| 带表格但没接 ListPage（含弹窗/面板子组件） | 53 | 上表 41 + 抽屉 / 面板类组件（`BlockerCheckPanel` / `StockPickerDialog` / `CalcSheetPanel` …） |
| 非 ListPage 且完全无表格 | 55 | 含 `opening` / `year-end` / 4 张报表 / 大量组件 |

```bash
# 带表格的 index.vue 里还有多少没接 ListPage（写稿时 41）
comm -23 <(grep -rl "<t-table\|<t-enhanced-table" views/business --include=index.vue | sort) \
         <(grep -rl "components/ListPage" views --include=index.vue | sort) | wc -l
```

**结论：下一波的真实收益在那 41~53 页（P3-09 ~ P3-12 已排上），不在这 4 页。**

### 1.3 这 4 页的结构实测

| 页 | 总行数 | 模板/脚本/样式 | 表格 | 壳样式行数 |
|---|---|---|---|---|
| `account` | 538 | 185 / 230 / 123 | `t-enhanced-table` + `:tree` | 116 |
| `opening` | 885 | 341 / 385 / 159 | **0 个** | 52 |
| `year-end` | 610 | 224 / 253 / 133 | **0 个**（挡路单据明细已抽成组件） | 24 |
| `report/report-item` | 1006 | 400 / 403 / 203 | `t-table`（**无 `:pagination`**） | 53 |

> 「壳样式行数」= 该页 `<style>` 里 `.panel-head*` / `.tool-card` / `.tool-row*` / `.list-card` / `.panel-empty*` 这些类的行数合计。**这就是「套进 ListPage 能省掉的那部分」的上限**：24~116 行，而每页 230~403 行的业务脚本一行都少不了。

---

## 2. 决策一：`account` 不进 ListPage

**要的不是 prop，是第二套表格实现。** 这一页真实需要的清单：

1. `t-enhanced-table` 渲染（`t-table` 是 PrimaryTable，**不认 tree**，页面注释 L108-111 已写明）；
2. 受控展开态 `expandedTreeNodes` + `@expanded-tree-nodes-change`（「全部展开 / 全部收起」两个按钮靠它）；
3. `treeConfig` 的箭头列下标按 `colKey` 反查（插列不错位）；
4. **前端拼树 + 前端过滤**（`buildAccountTree` / `filterAccountTree`，命中 + 祖先保留）；
5. 拼树后的计数口径（`accountTreeMeta`：节点数 / 命中数 / 层数 / 「后端共 N，仅载入前 500」）；
6. 404 降级（接口未上线时红条换信息条 + 空态，`isApiMissing`）。

第 4、5 两项是**数据模型不同**：树页手里是「嵌套 children」，ListPage 手里是「扁平一页 + total」。这不是加一个 `tree?: boolean` 能兼容的，是两套 after-fetch 管线。

**消费者数量**（决定值不值得做）：`business/erp` 里 `t-enhanced-table` **只有 `account` 一个**（`grep -rl t-enhanced-table frontend/src/views/business/erp`）。另外 6 处 `:tree=` 里，`warehouse` / `location` 是传给**弹窗里的 TreeSelect**，不是树表。

> 为 1 个页面在 29 个页面依赖的组件里开第二条渲染分支，收益/风险不成立。

**什么时候翻案**：`system/menu`、`system/dept` 与 `account` 是同一形态（筛选行 + 全量树 + 共 N 节点 + 展开全部，见 `views/system/menu/index.vue` L1-60）。若 P3-11/P3-12 要把它们一起收，做法是**另立兄弟组件 `TreeListPage`**，与 ListPage 共用「壳」但各自持有自己的表格与 after-fetch 管线——**不是**在 ListPage 里加 `mode`。

---

## 3. 决策二：`report-item` 不进 ListPage，也**不要** `paginated=false`

这一页的语义是「**整表顺序即业务语义**」：

- `move()` 必须把**整表 id 顺序**提交给后端（`sortReportItems(report_code, next.map(r => r.id))`，L579-596，页面注释 L573-578：公式只能引用排在前面的项，局部交换无法判断换完还有没有引用落到后面，后端整表校验）；
- 筛选是**本地过滤**（`visibleRows`，L468-482：keyword / status / onlyUnmatched 三个条件都在前端），而行操作 ↑↓ 作用在**整表**上 —— 这套「本地视图 + 全局顺序」正是 ListPage 的 `filter → queryBuilder → fetcher → 分页` 模型表达不了的；
- 服务端分页会把「整表顺序」切成每页 20 条的局部顺序，排序功能直接失效。

**`paginated=false` 的性价比核算**：它能省的是筛选行 + 面板头 + 错误条 ≈ 40~50 行（该页壳样式只有 53 行），代价是给已经 29 个消费者的组件再加一条**贯穿式分支**：

| 触点到的地方 | 行 |
|---|---|
| `pagination` computed | `index.vue` L428 |
| `load()` 里的页码回夹 + **多拉一次** | L466-470 |
| `handlePageChange` | L517-524 |
| `pageSize` ref / `showPageSize` | L420 / L331 |
| 批量条 `:filtered-total="total"` 的口径 | L186 |
| `共 N {{ metaUnit }}` | L168 |
| 导出「只导当前页」的前提 | L52-53 / `list-page-pure.ts` L176-181 |

7 处判据全部要加 `if (paginated)`，其中「回夹 + 二次请求」和「导出只导当前页」两条是**语义级**的（无分页时「当前页」= 全表，`rowsToCsv` 的注释直接就不成立）。

**顺带发现的真 bug（不是不进 ListPage 的理由，但必须修）**：`move(rowIndex, ±1)` 的 `rowIndex` 来自表格 `:data="visibleRows"`（L102、L119/L131），而 `move()` 里 `rows.value.slice()` 用的是**全表**下标（L584）。**有筛选时（`visibleRows ⊂ rows`）会移错行**：`rows=[A,B,C,D]` 筛出 `[B,D]`，在 D 上点 ↑（rowIndex=1）→ 实际交换的是 B 和 A。已开独立卡。**已修（commit `ce05f7e2`）**：换算抽成 `report-item-logic.ts::moveVisibleRow`（按行 id 定位全表下标，在相邻**可见**行之间换位），`move()` 只留接线，14 条单测钉住（含无筛选对齐旧行为、`[A,B,C,D]/[B,D]` 算例、提交 id 恒为整表全集、传回 `rowIndex` 即红）。

---

## 4. 决策三：`opening` / `year-end` 不抽新组件

**`opening`**：0 个 `t-table`。它的本体是两个 tab 的**可编辑台账**（科目下拉 / 借 / 贷 / 备注 / 删除 / 合计行 / 借贷平衡判定 / 结账后整页只读 / 行级错误聚合）。抽 `EditableGrid` 要先定义「列类型 + 校验口径 + 合计口径 + 只读态 + 脏值」这套 DSL——而现在全仓只有**它一个**页面级台账。`v-model="row."` 的另外 9 处（`voucher` 明细、`sales-order` 明细、`budget`、`exchange-rate` …）全是**抽屉/弹窗里的行编辑器**，列契约各不相同（选物料 / 单位换算 / 含税算价），强行统一是经典的过度抽象。

**`year-end`**：0 个 `t-table`。五项前置检查的**文案与判据和页面强绑定**（期间未结 / 借贷不平 / 下年未开账 / 未完成单据 / 不变量坏凭证），且唯一「成表」的部分——挡路单据明细——**已经抽过了**：`shared/BlockerCheckPanel.vue`，消费者 2 个（`period/CloseCheckPanel.vue` + `year-end`）。

> 团队已有正确实践：**第二个消费者出现时才抽**。这条现在就写进判定表，避免下次再从「能不能抽」开始讨论。

**翻案条件**：出现第 2 个**页面级**可编辑台账，且列契约（类型 + 校验 + 合计）一致时，抽 `EditableLedger`；出现第 2 个五项式检查清单时，把清单本身抽成 `CheckList`（明细表已有 `BlockerCheckPanel`）。

---

## 5. 决策四：下沉「皮」要下沉**样式**，不是造 `list-shell` 组件

任务卡建议是「收进一个 `list-shell` 轻组件」。方向对，**做法要改**：

**度量（`views/business` 下 137 个 `.vue` 的 `<style>` 全扫，脚本按规则块解析、多行选择器逐个拆开）**：

| 指标 | 值 |
|---|---|
| shell 类规则块 | **529** 块 |
| 这些块的总行数 | **2335** 行 |
| 不同选择器 | 58 个 |
| 在 ≥2 个文件里重复的块（选择器 + 声明体完全相同） | **76** 块 |
| 按「每块留一份」可删 | **1712 行**（≈ 总量的 73%） |

**漂移证据（这才是真问题）**：

| 选择器 | 出现在多少文件 | 变体数 |
|---|---|---|
| `.panel-head` | 40 | **10** |
| `.panel-head__meta` | 40 | **8** |
| `.panel-head__title` | 40 | 5 |
| `.list-card` | 33 | 5 |
| `.tool-row` | 32 | 6 |
| `.panel-head__tip` | 27 | 5 |
| `.tool-card` | 24 | **7** |
| `.panel-empty__desc` | 24 | 7 |
| `.tool-row__spacer` | 26 | **2**（最接近统一） |

**为什么不是组件**：40+ 页面的模板里已经写着 `<t-card class="tool-card">` / `<div class="panel-head">`。改成组件要动 40+ 个模板、改 DOM（多一层 wrapper）、还要为各页不同的 `t-card` props / `#actions` 插槽做适配——**风险远大于收益**。样式下沉**不改模板、不改 DOM**，只删各页 `<style>` 里的重复定义，类名保持不变（`scoped` 的 `[data-v-x]` 选择器优先级仍高于全局类，页面想局部覆盖照样能覆盖）。

**落点**：`frontend/src/style/shell.less`，由 `style/index.less` `@import`；ListPage 自己继续 `scoped` 自持样式（组件应自包含），全局 shell 只服务非 ListPage 页面。

**顺序**：**不能现在做**。P3-09 ~ P3-12 每一波都在改同一批页面的 `<style>`（迁走的页面不再需要 shell），并行改必冲突。所以挂 P3-12 之后（依赖卡）。

**收敛策略**：**没有一个 shell 类是「全站单变体」**（最接近的 `.tool-row__spacer` 也有 2 个变体、24 个 `.tool-card` 有 7 个变体），所以**不能机械删**。做法是三步：

1. 立全局基线：每个类取「出现最多的那个变体」进 `shell.less`；
2. 逐页 diff：把该页本地定义与基线比，**完全一致 → 删本地**（76 块、1712 行可以直接走这一步）；不一致 → 差异部分留在页面 `scoped` 里当覆盖（`scoped` 的 `[data-v-x]` 优先级高于全局类，覆盖照样生效）；
3. 变体合并单独评审：`.panel-head` 10 个变体、`.panel-head__meta` 8 个、`.tool-card` 7 个这类，要么合并（会改观感，需截图比对）要么**保持原样只删重复**——默认选后者，宁可少省几百行也不让 30 多个页面同时变样。

---

## 6. 判定表：以后什么样的页面**不进** ListPage

给后续所有迁移波次（P3-09 ~ P3-12 与以后的二开页面）用。**三问全「是」才进**：

| # | 问题 | 是 | 否 |
|---|---|---|---|
| ① | 分页是不是**后端给的**（`total` > 一页，翻页要重新请求）？ | ↓ | **不进** |
| ② | 行是不是**只读**的（点开是抽屉/弹窗，不是在行里改）？ | ↓ | **不进** |
| ③ | 行上有没有「顺序 / 层级」这种**整表语义**？ | ↓ | **不进** |
| — | 三问全「是」 | **进 ListPage** | |

按此表逐条核对，判为「不进」的形态与归属：

| 形态 | 例子 | 归属 |
|---|---|---|
| 树 / 层级（全量拉取 + 前端拼树） | `account`、`system/menu`、`system/dept` | 页面自绘；若凑齐 ≥2 页同波改造 → 兄弟组件 `TreeListPage` |
| 整表有序（顺序即语义） | `report/report-item` | 页面自绘 |
| 行内可编辑台账 | `opening` | 页面自绘；第 2 个同契约台账出现 → `EditableLedger` |
| 只有清单 + 动作面板 | `year-end`、报表 4 页 | 页面自绘；成表部分已有 `BlockerCheckPanel` 可复用 |
| 全量但只读、且 < 200 行 | 各类配置型小表 | 页面自绘 + 全局 shell 样式 |

---

## 7. 后果

**变得更容易**
- ListPage 只有一种渲染路径：29 个已用页面 + 41 个待迁页面的回归面不再乘 2；
- 23 个 props 的加法继续线性进行，`TreeListPage` 出现时是**新增组件**而不是让 ListPage 变胖；
- P3-09 ~ P3-12 的验收不再被这 4 页卡住（见 §8）；
- 壳样式可删约 1712 行（含 76 个完全重复块），且 `.panel-head` 10 个变体的漂移被一次性摊到明面上。

**变得更难 / 代价**
- 这 4 页继续持有 24~116 行壳样式，直到 shell 下沉落地（下沉后降到 0）；
- 未来做 `TreeListPage` 时要再抽一次壳（但那是「一次成型」，不是「在 ListPage 里分叉」）；
- 判定表是**人判**的，不是 lint 强制的——需要 code review 兜住（见 §7 风险）。

**风险**
- shell 下沉会改观感：**先只删「选择器 + 声明体完全一致」的重复块（76 块 / 1712 行，零风险）**，变体合并另立评审、逐页截图比对；
- 判定表被跳过：`account` 这类页面以后仍可能被新同学「顺手套一下」——缓解：`docs/前端二开指南.md` §三 已加指针指向本文。

---

## 8. 对在途卡片的条款修订（必须照此执行）

**`t_b190fc91`（P3-12 · 剩余页面收口）的验收条款「全站列表页统一走 ListPage」** 修订为：

> 全站列表页统一走 ListPage，**除本文 §6 判定表判为「不进」的页面**。当前明确不进：`business/erp/account`、`business/erp/opening`、`business/erp/year-end`、`business/erp/report/report-item`。P3-11 范围里的 `system/menu`（树表）、`system/dept` 同理不进；若 P3-11 真要做树页统一，**另开卡做 `TreeListPage`**，不要在 ListPage 里加 `tree` 模式。

---

## 9. 证据索引（想反驳先读这些）

| 文件 | 看什么 |
|---|---|
| `frontend/src/components/ListPage/index.vue` L1-59、L163-246、L428、L460-524 | 契约注释 / 唯一的 `<t-table>` / 分页判据链 |
| `frontend/src/components/ListPage/types.ts` L10-15 | 「只声明页面之间真的会漂移的东西」的口径 |
| `frontend/src/views/business/erp/account/index.vue` L108-123、L197-207、L365-383 | 为什么必须 `t-enhanced-table` + 一次拉全量 + 404 降级 |
| `frontend/src/views/business/erp/report/report-item/index.vue` L100-136、L468-482、L573-596 | 无分页 / 本地过滤 / 整表顺序提交 |
| `frontend/src/views/business/erp/opening/index.vue` L58-163 | 可编辑台账（0 个 `t-table`） |
| `frontend/src/views/business/erp/year-end/index.vue` L58-114 | 五项清单 + 已抽出的 `BlockerCheckPanel` |
| `frontend/src/views/system/menu/index.vue` L1-60 | 树页的第二种实例（P3-11 的翻案依据） |

**复现度量命令**

```bash
cd /opt/cloudpivot/frontend/src
# ListPage 消费者 / 待迁页面（两个数都在涨：P3-09~12 在并行迁）
grep -rl "components/ListPage" views | wc -l
comm -23 <(grep -rl "<t-table\|<t-enhanced-table" views/business --include=index.vue | sort) \
         <(grep -rl "components/ListPage" views --include=index.vue | sort) | wc -l
# 树表只有 account 一个（business/erp 内）
grep -rl "t-enhanced-table" views/business/erp
# 壳样式：547 条选择器行 ≈ 529 个规则块（差的是多行选择器续行）/ 展开 2335 行
grep -rh "^\.\(panel-head\|tool-row\|tool-card\|list-card\|panel-empty\)" views/business | wc -l
grep -rh "^\.panel-head" views/business | sort | uniq -c | sort -rn | head   # 同一选择器被多少个文件重复定义（.panel-head 40 份）
```
