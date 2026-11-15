# 壳样式（shell）变体收敛决策

> 卡 `t_3d2fcc4d`（architect）。上游卡 `t_3a87ffac` 已完成壳样式下沉（`src/style/shell.less`，提交 `060fb85a`，
> 248 块 / 净 −1221 行、零观感变化），本卡只做**变体收敛的决策**，不动业务代码。
>
> 本文件是壳类（`shell.less` 里的类 + `panel-empty*` 家族）的**唯一契约文档**：
> 页面能改什么、不能改什么、要改走哪条路、怎么验、谁在什么时候改。实施卡按 §5 开。
>
> 度量工具：`frontend/scripts/shell-variant-report.cjs`（随本卡入库，可重复跑）。

---

## 0. 决策摘要

| # | 决策 | 一句话 |
|---|---|---|
| 1 | **壳类 = 契约，页面零声明** | 页面 `<style>` 里**不得**再出现壳类规则块；壳类取值只改 `shell.less`。这条由 lint 兜住（§5 B1 卡落） |
| 2 | **页面差异只走「命名变体」一条出口** | 差异必须表达为 `shell.less` 里登记过的 `.壳类--修饰`，**属性写全**、与基线同特指度、文件内靠后；不给页面留变量旋钮，避免第二种无记录的口子 |
| 3 | **特指度纪律** | 壳类要压过 TDesign 一律写 `.父 .子`（0,2,0）；动壳类前必须查 `tdesign-*.css` 有无 (0,2,0)+ 同名属性规则（父卡 `.panel-empty__icon` 就栽在这） |
| 4 | **4 个「刻意未全局化」的类：收敛，不长期保留本地** | 用 4 个空态变体 + 1 个标签变体覆盖 **36/37 页**，剩 1 页（HR 薪资首页）进白名单。这 4 类实测是 **119 块 / 682 行**，不是父卡正文写的「42 块 / 250 行」（§1.3） |
| 5 | **分批 7 批、6 张实施卡** | B0 组件自持(4) / B1 纯冗余(20) / B2 间距颜色(38) / B3 尺寸字号(16) / B4 半变体(33) / B5 空态标签族(136) / B6 单页专有(26)。顺序 B1→B2→B3→B4→B5→B6，B0 不动（§5） |

**总量**（机器实测，`views/` + `components/` + `layouts/`，less 展平后逐规则计块）：
**273 块 / 68 文件 / 约 1591 行**。全部收敛后页面 `<style>` 里壳类声明应为 **0**（ListPage 例外，见 §5 B0）。

---

## 1. 实测口径与数字（先对账，再决策）

### 1.1 本卡的计数口径

`frontend/scripts/shell-variant-report.cjs` 的做法：

1. 用 `less` 编译 `src/style/shell.less` → 得到 27 个类的**完整属性集**（基线）；
2. 逐个 `.vue` 取 `<style>` 块，用 `less` 编译**展平嵌套**（`&__title` → `.panel-empty__title`），
   再逐条规则取声明；跨文件的页面级 `.less`（如 `report/result.less`）跟进扫描；
3. 规则块归属壳类的方式：选择器最后一个简单选择器命中壳类家族（`panel-head*` / `panel-empty*` /
   `tool-row*` / `tool-card*` / `list-card*`）即计入；
4. 与基线**逐属性**比对（`var(--x, fb)` 与 `var(--x)` 视为同 token），得出 5 种关系：
   `same` == 基线 / `superset` 本地 ⊇ 基线 / `conflict` 同属性不同值 / `subset` 只写一部分且值相同 /
   `partial` 半个变体（缺属性 + 有差异）。

### 1.2 与父卡「198 块」的对账（差 75 块，逐条已核）

用父卡自己的解析器（`shelllib.py` + `count_blocks.py`）复跑当前树，得 **198 块 / 1693 行**，可复现。
两个口径的差集 **68 块**已经逐条列出：

| 差异原因 | 块 | 例 |
|---|---|---|
| 父卡解析器**只扫 `views/`**，本卡还扫 `components/` 与 `layouts/` | 4 | `components/ListPage/index.vue` 的 `panel-head` / `__main` / `__title` / `__meta` |
| 父卡 `SHELL` 正则只认 `__` 与精确名，**不认 `--` 修饰类** | 6 | `tool-row--second`×4、`tool-row--filter`、`panel-empty--inline` |
| 父卡**不展开 less 嵌套**：`&__title` / `&__desc` / `&__icon` / `&__main` 子块不匹配正则 | 58 | 58 条里 54 条所在文件的 `<style>` 确有 `&__` 嵌套写法；`panel-head__main/title/meta` 各 +6、`panel-empty__title/__desc/__icon` 各 +6 |
| 计法差：一条规则含多个壳选择器时父卡按「块」计 1、本卡按「类」计 N | 7 | — |

**结论**：198 不是错的，是**更窄的口径**（等价于「`views/` 下按顶层块计」）。本卡按 273 走，
因为它才是「页面 `<style>` 里实际生效的壳类声明数」——收敛目标必须是这个数归零，而不是 198 归零。

### 1.3 纠正父卡正文的一个数字：「4 类合计 42 块 ≈250 行」

实测两个口径**都对不上 42**：

| 类 | 父卡口径（块） | 本卡口径（块） | 本卡估算行 |
|---|---|---|---|
| `.panel-empty` | 37 | 37 | 338 |
| `.panel-empty__title` | 31 | 37 | 152 |
| `.panel-empty__desc` | 29 | 35 | 155 |
| `.tool-row__label` | 10 | 10 | 37 |
| **合计** | **107** | **119** | **682** |

即「本次没吃到的量」是 **119 块 / 约 682 行 ≈ 全部剩余壳类声明的 44% 块、43% 行**，
不是 42 块 250 行（2.8× 低估）。这个纠正直接影响决策权重：**这 4 类不是「一点点例外」，是现存债的一半**，
没有理由长期保留本地（§3）。

### 1.4 现状分布（逐批，见 §5 表）

```
B0 组件自持（allowlist，不动）      4 块 /  1 文件 /  24 行
B1 纯冗余删除（零观感）            20 块 / 16 文件 /  75 行
B2 间距/颜色归一（2px 级）         38 块 / 24 文件 / 273 行
B3 尺寸/字号档位                   16 块 / 13 文件 /  61 行
B4 半变体补全（缺基线属性）        33 块 / 17 文件 / 245 行
B5 空态与标签变体族               136 块 / 46 文件 / 808 行
B6 单页专有壳类                    26 块 / 20 文件 / 105 行
```

---

## 2. 决策一：壳类契约（根治变体再生长）

变体为什么会长出来？不是「有人不守规矩」，是**当前规则给了一条合法但危险的表达方式**：
页面想微调，就在自己的 `<style scoped>` 里重写半个规则块。因为 CSS **按属性级联、不按规则块**，
「半个变体」会被全局基线补齐缺的属性——父卡实测 8 个页面空态因补 `display:flex` 而变高，
3 个报表页 `.tool-row__label` 因补 `font-size:13px` 而缩水。这不是 bug，是**这种表达方式本身的后果**。

因此契约不是「少写点本地样式」，而是**把那条表达方式关掉，只留两条受控出口**。

### 2.1 三条铁律（强制，违反由 lint 拦）

**铁律 A —— 页面零壳类声明。**
`src/views/**`、`src/components/**`、`src/layouts/**` 的 `<style>` 里**不得出现**壳类（`shell.less`
定义的 27 个类 + 其 `--` 变体 + `panel-empty*` 家族）的规则块。要改壳类取值 → 改 `shell.less`。
> 唯一例外：`components/ListPage/index.vue`（组件自持 scoped 样式，组件必须自包含，见 `shell.less` 头部注释）。
> 该文件进 lint 白名单，新增例外需架构评审。

**铁律 B —— 页面差异只有一条出口：命名变体。**
确需与基线不同的取值，表达为 `shell.less` 里**已登记**的 `.壳类--修饰` 类，模板上给元素加这个类。
变体类必须：
1. 定义在 `shell.less`（单一真源），不在页面；
2. **属性写全**（该变体要改的属性全列出，不依赖继承补全）；
3. 与基线**同特指度**（(0,1,0) 裸类），靠「文件内更靠后」取胜——这样它的行为与 scoped (0,2,0) 无关，
   也不会被 TDesign 意外压过（若压不过，按铁律 C 写成 `.父 .变体`）；
4. 在 §2.3 的变体清单里登记语义（为什么这个变体存在）。

**不提供变量旋钮（`--shell-*`）**：本卡明确不给页面级 CSS 变量口子。理由是变量看起来更「轻」，
但它能把一个属性拉到任意值，等于把特指度问题换成「值不可枚举」问题——比本地规则块更难审计。
个别页面确需不同值 → 那就是一个变体，命名 + 登记，代价是必须说清为什么。

**铁律 C —— 特指度纪律（对 TDesign）。**
- 壳类要压过 TDesign 的样式，写成 `.父 .子`（0,2,0，靠 `index-*.css` 在 `tdesign-*.css` 之后加载取胜），
  不要靠 `!important`、也不要指望驼峰巧合。
- 新增或修改壳类前，必须跑一遍：
  ```bash
  grep -rn "选择器里的类名" frontend/node_modules/tdesign-vue-next/es/style/*.css  # 或 dist 里的 tdesign-*.css
  ```
  看有没有 (0,2,0)+ 的同名属性规则。父卡的教训：`.panel-empty__icon{font-size:28px}` 裸类 (0,1,0)
  压不住 `.t-icon.t-size-m{font-size:16px}` (0,2,0)，不修则 19 页空态图标 28px→16px。

### 2.2 属性分级表（哪类属性是硬约定，哪类是旋钮）

这张表回答「页面凭什么叫板」——**不是所有属性都能谈**：

| 属性组 | 归属 | 页面/变体能改吗 | 判据 |
|---|---|---|---|
| `display` / `flex-direction` / `align-items` / `justify-content` / `flex-wrap` / `flex` | **骨架（硬约定）** | 只能通过命名变体 | 决定块级/行内与主轴对齐，跨页不一致就是排版事故；父卡实测补 `display:flex` 让 8 页空态高度/文字宽度变化 |
| `gap` / `margin` / `padding` | **骨架（硬约定，同族单值）** | 只能通过命名变体（如 `--dense`） | 2px 级差异（8/10/12px）没有设计语义，是手抄漂移，不是需求（§4.1 实测共现关系） |
| `font-size` | **排版（两档制）** | 只许 14px（默认）/ 16px（`--lg`）两档 | 15px 这种中间值是手填的，不是设计档位 |
| `color` / `font-weight` | **排版（语义 token）** | 只能取语义色板里的角色色 | 同一角色两色（tip 的 secondary vs placeholder）= 视觉噪声 |
| `width`（工具行控件） | **半可调** | 默认吃基线；整行紧凑用 `--dense` | 宽度取决于该页筛选项多少，属页面裁量——这是唯一真正有正当性差异的属性 |
| `white-space` / `text-align` | **排版（守卫）** | 需要就提进基线或变体 | 防换行/防溢出是结构意图，不是观感偏好 |
| 颜色/间距以外的装饰（阴影、圆角…） | 不入壳类 | — | 壳类只管列表页骨架，装饰留在页面或组件 |

### 2.3 变体清单（登记制，新增需架构评审）

| 变体 | 语义 | 页数 | 作用于 |
|---|---|---|---|
| `.tool-row--dense` | 紧凑工具行（成本/预算类页面，筛选项多、需要一行塞下） | 7 | `gap:8px`；`__period`/`__date` 收窄 140px |
| `.tool-row--second` | 工具行第二排（单据类页面的第二行筛选） | 4 | `margin-top:8px` |
| `.tool-row--filter` | 报表页筛选条 | 1 | `margin-top:12px` |
| `.panel-head__title--lg` | 大卡片标题 | 10 | `font-size:16px` |
| `.panel-head__tip--end` / `__hint--end` | 提示推到卡片头右侧 | 5 | `margin-left:auto`（tip 另加 `text-align:right`） |
| `.tool-row__label--sm` | 报表/单据页标签小一号 | 6 | `font-size:13px; white-space:nowrap` |
| `.panel-empty--rich` | 主数据页大空态（带说明文案） | 6 | `justify-content:center; gap:6px; padding:48px 16px`；icon 32px；desc `max-width:460px; line-height:1.6` |
| `.panel-empty--file` | 导入导出/存储类空态（标题更大、说明更粗） | 4 | `display:block; padding:40px 0; text-align:center`；icon 40px；title 15px/primary/margin 12px 0 4px；desc 13px/secondary/margin 0 0 16px |
| `.panel-empty--doc` | 单据类空态（居中标题 + 说明） | 6 | `display:block; padding:40px 0; text-align:center`；icon 40px；title 14px/primary/margin 8px 0 4px |
| `.panel-empty--inline` | 卡片内嵌小空态（薪资审批详情） | 1 | `display:block; text-align:center; padding:16px 12px; font-size:13px` |
| `.list-card--side` | 侧栏卡（帮助中心，固定 320px） | 1 | `flex:0 0 320px; width:320px` |

`panel-head__main{min-width:0}`、`panel-head__title/meta{white-space:nowrap}` 这两条**提进基线**
（不是变体：所有 `__main` 都该能收缩防溢出，所有卡片标题都该防断行）。

### 2.4 落地方式：lint（先报告，后门禁）

`frontend/scripts/shell-variant-report.cjs` 现在就能算出全部违规面。把它升格为门禁的顺序：

1. **B1 卡**先挂 `--strict`-lite：只拦「新增」的壳类页面声明（用一份 `shell-variant-allowlist.json`
   快照当前存量，存量按批次逐步削减，新增即红）。这样门禁立即可用而不会把 200 多处存量变成红海。
2. 每落一批（B1…B6），从 allowlist 里删掉对应条目。
3. 全部落完后 allowlist 只剩 §5 B0 的 `components/ListPage/index.vue`，此时改为「零例外」硬门禁，
   并把报告挂进 `scripts/check.sh` 的 L0。

---

## 3. 决策二：`.panel-empty*` / `.tool-row__label` 4 类的方向 —— **收敛**

**结论：收敛，不长期保留本地。** 但收敛方式不是「全部对齐成基线」，而是「**变体族 + 白名单**」。

### 3.1 为什么不能「长期保留本地」

1. **量级不成立**：4 类 = 119 块 / 682 行（§1.3），占剩余壳类声明的 44%。把它当例外，等于宣布一半的债永不清，
   而「例外」会变成模板：下一个页面看到 `.panel-empty` 可以本地写，就会照抄。
2. **技术根因已被新契约消灭**：父卡不敢全局化，是因为页面写了「半个变体」，全局化会补属性。
   新契约（§2.1 铁律 B）要求变体**属性写全**、且页面零声明——于是不存在「补属性」这一步，
   等价性可以静态证明（变体属性集 ⊇ 旧本地属性集 且冲突项逐条列出）。
3. **页数分布证明这是 4 个族，不是 37 个特例**：37 页按（empty + title + desc + iconSize）组合实测只有 15 组，
   前 4 组覆盖 26 页（70%）（§3.2）。真正的「特例」只有 1 页。

### 3.2 变体族设计（覆盖 36/37 页）

按页面实测组合归族（数据来自 `shell-variant-report.cjs --json`）：

| 变体 | 覆盖页数 | 页面 |
|---|---|---|
| 基线（无修饰） | 19 | account、commission-rule、commission、customer-price、exchange-rate、expense/pay、inventory、ledger、location、payable、period、purchase-order-item、purchase、seq、serial、stockin、transfer、voucher、flow/def |
| `.panel-empty--rich` | 6 | customer、material、supplier、unit、warehouse、hr/employee |
| `.panel-empty--file` | 4 | impexp/export-task、impexp/import-task、storage/backend、storage/file |
| `.panel-empty--doc` | 6 | contract、lot、stock-out、stock-take、hr/attendance、hr/leave |
| `.panel-empty--inline` | 1 | hr/salary/detail |
| **白名单（非标准空态，不改）** | 1 | **hr/salary/index**（`padding:40px 16px` + title `margin:0;line-height:20px` + desc 无颜色，独一份） |

`.tool-row__label` 10 页单独处理：基线只写 `color: var(--td-text-color-secondary)`
（4 个报表页 `balance-sheet`/`cash-flow`/`income-statement`/`payable-aging` 正是这个 → 本地块**直接删**）；
显式 13px 的（invoice、receipt、receivable、receipt-refund、contract）→ `.tool-row__label--sm`；
`cost-labor` 的 12px → 归一 13px（`--sm`，1px 变化，截图确认）。

> 关键设计取舍：`.panel-empty` 基线的 `font-size`/`margin` 等**在变体里必须重写全**。
> 例如 `--doc` 要写 `title{margin:8px 0 4px; font-size:14px; color:primary}`，
> 而不是只写 `margin` 然后指望基线补 `font-size`——后者正是父卡踩过的坑。

### 3.3 收敛要接受的观感变化（逐条列出，实施卡逐页截图）

| # | 变化 | 页数 | 判断 |
|---|---|---|---|
| 1 | `--doc` 族 `padding` 36/32/28 → 40（上下各差 2–6px） | 4（contract、lot、attendance、leave） | 接受：28/32/36 是手填的，不是设计档位 |
| 2 | `--doc` 族 icon 28/32 → 40 | 3（contract、attendance、leave） | 接受：图标尺寸跟空态高度绑定，40 是该族最新取值 |
| 3 | `--rich` 族 desc `max-width` 420 → 460 | 2（material、warehouse） | 接受：同族两个值无理由 |
| 4 | `--rich` 族 desc `line-height` 20px → 1.6（≈19.2px） | 1（hr/employee） | 接受：0.8px |
| 5 | `panel-head__title` 15px → 16px（`--lg`） | 3（price、report-item、cost-period/UncollectedPanel） | 接受：字号只留两档 |
| 6 | `panel-head__tip` `color` secondary → placeholder（略淡） | 14 | 接受：同类角色统一色 |
| 7 | `tool-row` gap 8→12（3）、`panel-head__main` gap 8→10（7）、`panel-head` margin-bottom 12→10（3） | 13 | 接受：2px 级，归一到同族单值 |
| 8 | 控件宽度归一（period 132/140/200→220 或 140、date 140/180→150、status 140→130、range 240→250、type 150→140、keyword 200→220） | 11 | 接受：`--dense` 覆盖紧凑场景，其余取基线 |
| 9 | `tool-row__label` cost-labor 12px→13px | 1 | 接受 |

**不接受任何一条也可以**——但要在对应实施卡的 review 里写清「为什么不接受」，并把它变成一个**命名变体**（登记到 §2.3），
而不是留在页面里。这是本决策唯一的松紧尺度：**可以有更多变体，不能有页面本地声明。**

---

## 4. 决策三：其余变体的归一口径

### 4.1 间距：归一到「同族单值」

实测 gap/margin 的取值集合是 `{4,6,8,10,12,16,18,20,24,28,32,36,40,48}`，
但**同一族内的多值没有语义对应**。例如 `panel-head__main` 的 gap 有 10px（基线）与 8px（7 页）；
`panel-head` 的 margin-bottom 有 10/12；`tool-row` 的 gap 有 12/8。
更直接的证据：`.tool-row--second{margin-top}` 在 4 个文件里是 **3 份 8px + 1 份 10px**——
同一个语义的修饰类都已经漂移了，说明这些数字是手抄的，不是设计的。

**口径**：每族一个值（`panel-head__main` 10px、`panel-head` 10px、`tool-row` 12px）；
整行需要更紧的页面用 `.tool-row--dense`（8px），并且 `--dense` 是**已登记变体**，不是随便写 8。

### 4.2 字号：两档制（14 / 16）

`panel-head__title` 实测 14（基线）/15（3 页）/16（7 页）。15px 是填出来的中间值。
**口径**：14px 默认、16px 用 `--lg`；15px 归一到 16px（那 3 页都是「大卡片」场景，与 16px 的 7 页同类）。

### 4.3 颜色：角色色，不按页面偏好

`panel-head__tip` / `__meta` 实测 9 页用 `--td-text-color-secondary`、基线是 `--td-text-color-placeholder`。
两者都是「次要信息色」，但一个更淡。**口径**：同类角色一个色——`tip`/`meta`/`hint` 统一 placeholder（基线）；
需要更强对比的（`cost-period/UncollectedPanel` 的 meta）→ 提变体，不留在页面。

### 4.4 控件宽度：`--dense` + 基线

宽度是唯一有正当性差异的属性（取决于该页筛选项数量）。但实测 11 处不同值里，
`period` 的 132/140 与 `gap:8px` **共现**在成本/预算类页面（budget×2、cost-alloc-rule、cost-labor、cost-overhead、ledger），
说明它不是「每页各自决定」，而是「这一族页面更紧凑」。→ 定义 `.tool-row--dense`（gap 8px + period/date 140px），
其余页面一律吃基线（220/150/130/250）。payable-aging 的 period 200/date 180 属单独裁量 → 归一到基线（截图确认）。

---

## 5. 分批与实施卡

**顺序**：B1 → B2 → B3 → B4 → B5 → B6（B0 不动）。理由：先做零风险与低风险，把「度量 + 门禁」建起来，
再动会改观感的大批；B5（空态族）最后做，因为它涉及 37 页模板加类 + 逐页截图，最费时且依赖前面积累的验证习惯。

| 批 | 块 | 文件 | 估算行 | 内容 | 观感 | 实施卡 |
|---|---|---|---|---|---|---|
| B0 | 4 | 1 | 24 | `components/ListPage/index.vue` 自持 scoped 样式 | 无（不动） | 无（进 lint 白名单） |
| B1 | 20 | 16 | 75 | 本地 == 基线（14 块）+ `flex:1` ≡ `1 1 auto`（5 块）+ `panel-empty__icon` 同值（2 块）；**同时落 lint（allowlist 快照档）** | 零 | **卡 ①** 纯冗余删除 + 门禁建立 |
| B2 | 38 | 24 | 273 | gap/margin/color/text-align 归一 + `--end` 变体 | 2px 级 + 提示色变淡 | **卡 ②** 间距与颜色归一 |
| B3 | 16 | 13 | 61 | 控件宽度归一 + `--dense` 变体 + `--lg` 标题 | 宽度变化 | **卡 ③** 尺寸与字号档位 |
| B4 | 33 | 17 | 245 | 缺基线属性的「半个变体」：`panel-head__title`×10、`panel-head`×8、`list-card`×7、`tool-card`×5 | 补属性会变 → 逐页裁定 | **卡 ④** 半变体补全 |
| B5 | 136 | 46 | 808 | 空态/标签变体族（4 个空态变体 + `--sm` 标签） | 9 条接受的观感变化 | **卡 ⑤** 空态与标签收敛 |
| B6 | 26 | 20 | 105 | 单页专有壳类：提为基线类（`--second`/`--filter`/`--inline` 登记）或改名去壳前缀 | 视裁定 | **卡 ⑥** 单页专有类处置 |

> 文件有重叠（同一页多个批），所以 4+16+24+13+17+46+20 = 140 ≠ 68 个唯一文件。
> 每张卡的正文会带**该批的逐文件逐类清单**（由 `shell-variant-report.cjs --batch <批号> --md` 生成），
> 不让实施者去翻 486 行清单。

### 5.1 每批的验收判据（统一）

所有实施卡都必须给这三样，缺一不算完：

1. **静态等价断言**：对每个改动页，「改造前本地生效声明集」与「改造后（基线 ∪ 变体）」逐类逐属性比对，
   `LOST / CHANGED / NEW` 三类里除本卡明确列出的「有意变化」外必须为 **0**。
   （父卡 `shell-sink-evidence.md` §3.3 的 `equivalence_static` 就是这个，照抄。）
2. **逐元素计算样式比对**：受影响路由用 Playwright 打桩（父卡 §3.1 的打桩口径：`/api/**` 返空表、
   `auth/me` 授 `*:*:*`、菜单走真后端），dump 每个带壳类的元素的计算样式 + 几何框，前后 diff。
   **判定标准**：差异必须落在本卡「有意变化」清单里，其余属性值变化不得伴随几何尺寸变化。
3. **像素比对**：本卡列出的「有意变化」页逐页截图，before/after canvas 逐像素比；同 dist 连拍两次做对照组排除字体抗锯齿噪声。

另外每卡必跑：`pnpm typecheck`（0 error）、`pnpm build`、`bash scripts/check.sh --fast`、
以及 `node scripts/shell-variant-report.cjs` —— **本批的块数必须归零或只剩白名单**。

### 5.2 每批的特别注意

- **B1（卡①）**：`flex: 1` ≡ `flex: 1 1 auto` 的前提是 spacer **没有子内容**（基数为 0 时同解）。
  已机械核对 5 处 spacer 全部是自闭合空 `<div class="tool-row__spacer" />` ✓。删 `panel-empty__icon`
  本地规则的前提是**祖先有 `.panel-empty`**（基线选择器是 `.panel-empty .panel-empty__icon`）；
  已核对 18 个用 icon 的页面全部有 ✓。
- **B2（卡②）**：`margin-left:auto` 类（tip 2 页 + hint 3 页）不是「归一」而是「推右侧」的**结构意图**，
  要提成 `--end` 变体（模板加类），不能当成间距归一删掉。
- **B3（卡③）**：`--dense` 上线后要确认换行点没飘（宽度改动的副作用就是工具行换行位置变化）——
  每页截图必须包含工具行区域。
- **B4（卡④）**：这批的每一条都是「删本地会补属性」，**不能批量删**。逐页裁定：
  补的属性是想要的 → 删本地；不想要 → 提命名变体；都不行 → 进白名单并写明理由。
  `list-card` 里 `help/center` 是 320px 侧栏卡（`flex:0 0 320px; width:320px`）→ `.list-card--side`。
- **B5（卡⑤）**：37 页模板要加 `--rich`/`--file`/`--doc` 类。模板改动要让 DOM 结构**逐字节不变**
  （只在 `class` 属性里追加 token），否则像素 diff 会混入 DOM 变化无法归因。
- **B6（卡⑥）**：`tool-row--second`（4 文件 2 个值）先统一到 8px 再收进 `shell.less`；
  只 1 页用的（`tool-row__dept`/`__year`/`__category`/`__month`/`__item`/`__tip`/`__switch`、
  `panel-head__actions`/`__ops`/`__account`/`__link`/`__warn`/`__sub`/`__overdue`、`tool-row__overdue`、
  `list-card__pager`）要按「是不是全站会重复用」二选一：会 → 进基线；不会 → **改名去掉壳前缀**
  （如 `tool-row__switch` → `inv-only-toggle`），让它不再假装是壳类。

---

## 6. 验收方法（复用父卡那套确定性证据法）

父卡 `shell-sink-evidence.md` §3 的方法是本仓库唯一被验证过「能抓住假等效」的手段，全部照抄：

| 手段 | 干什么 | 抓什么 |
|---|---|---|
| `equivalence_static` | `git HEAD` vs 工作区，逐文件逐类比对「生效声明集」 | 漏删、错删、变体没写全属性 |
| `probe-all.mjs`（逐元素计算样式） | 70 个路由，打桩后 dump 每个壳类元素的 20+ 属性 + 几何框 | 特指度输给 TDesign（`.panel-empty__icon` 就是它抓到的） |
| 像素 diff（22 页） | canvas 逐像素比对，容差单通道 ≤8 | 级联补齐导致的真实观感变化 |
| 同 dist 连拍照对照 | 排除字体抗锯齿 / 打桩 NaN 抖动 | 假阳性 |
| 门禁 | typecheck / build / `check.sh --fast` / 报告工具 | 编译与存量债回归 |

> 打桩口径（父卡 §3.1）：本地 dist + `/api` 反代 `172.18.10.144:80`；`/api/business/**` 与
> `/api/system/dept/**` 返空表、`auth/me` 授 `*:*:*`、菜单接口走真后端（保证侧栏一致）。
> **菜单必须走真后端**，否则侧栏宽度变化会污染像素 diff。

---

## 7. 明确不做（划边界，避免下张卡跑偏）

1. **不做组件化**（不把 `panel-head` 抽成 `<PanelHead>` 组件）。父卡已论证：40+ 模板要动、多一层 wrapper、
   还要适配各页不同 `t-card` props 与插槽。样式契约能解决的问题，不值得用 DOM 变化换。
2. **不用 `@layer`**。壳类要压过 TDesign，而 TDesign 的 CSS 不在任何 layer 里——真实层叠顺序里
   **未分层规则胜过分层规则**，把 `shell.less` 放进 layer 会让它整体输给 TDesign。要 `@layer` 就得把
   第三方 CSS 也裹进去，改动面远大于收益。
3. **不给页面级 CSS 变量旋钮**（§2.1 铁律 B 末段）。
4. **不动 ListPage 组件的自持样式**（§5 B0）。
5. **不追求把 198/273 变成 0 以外的指标**（比如「变体数 ≤ N」）。变体数不是目标，**页面零声明**才是。
6. **不在这批里做视觉改版**。所有变化都是「归一」，不是「重新设计空态」。

---

## 8. 风险与回滚

| 风险 | 影响 | 缓解 |
|---|---|---|
| 变体类特指度不够，被 TDesign 压过 | 局部样式失效（父卡 19 页图标事件） | 铁律 C + 每个变体的计算样式探针必须含图标字号 |
| 37 页模板加类引入 DOM 变化 | 像素 diff 无法归因 | B5 卡要求 `<template>` 除 `class` token 外逐字节不变（用 `git diff -U0` 断言） |
| 「有意变化」被滥用成「随便变」 | 观感漂移 | 每卡必须把有意变化列成表并附截图；review 只认表 |
| 门禁一上来就红 | 拖延 | allowlist 快照档（§2.4），存量按批削减 |
| 后续新页面继续长本地声明 | 债复发 | lint 新增即红（B1 卡）；契约文档进 `docs/编码规范.md` §2.8 与 `docs/前端二开指南.md` §7.1 |

回滚：每批是独立提交（页面 `<style>` 删除 + 模板加类 + `shell.less` 变体），任一批可单独 revert；
变体定义留在 `shell.less` 里不影响未使用它的页面。

---

## 附录 A：目标 `shell.less` 骨架（spec，由实施卡定稿）

> 只列**新增/改动**部分。每一条都要经 §5.1 的三重验收后才能进基线。
> 注意：变体类必须**属性写全**，不依赖基线补全。

```less
// ---------- 变体（modifier）：登记见决策文档 §2.3 ----------
// 与基线同特指度 (0,1,0)，靠文件内更靠后取胜；不依赖 scoped 的 (0,2,0)
.tool-row--dense { gap: 8px; }
.tool-row--dense .tool-row__period,
.tool-row--dense .tool-row__date { width: 140px; }

.tool-row--second { margin-top: 8px; }
.tool-row--filter { margin-top: 12px; }

.panel-head__title--lg { font-size: 16px; }
.panel-head__tip--end { margin-left: auto; text-align: right; }
.panel-head__hint--end { margin-left: auto; }

// ---------- 标签（原刻意未全局化的 .tool-row__label）----------
.tool-row__label { color: var(--td-text-color-secondary); }          // 只给颜色：字号保持继承 14px
.tool-row__label--sm { font-size: 13px; white-space: nowrap; }

// ---------- 空态：1 基线 + 4 变体（原 4 个刻意未全局化的类）----------
.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  padding: 24px 0;
}
.panel-empty .panel-empty__icon { font-size: 28px; color: var(--td-text-color-placeholder); }
.panel-empty__title { margin: 0; font-size: 14px; color: var(--td-text-color-secondary); }
.panel-empty__desc  { margin: 0; font-size: 12px; color: var(--td-text-color-placeholder); }

.panel-empty--rich { justify-content: center; gap: 6px; padding: 48px 16px; text-align: center; }
.panel-empty--rich .panel-empty__icon { font-size: 32px; }
.panel-empty--rich .panel-empty__desc { margin: 0; font-size: 12px; max-width: 460px; line-height: 1.6;
                                        color: var(--td-text-color-placeholder); }

.panel-empty--file { display: block; padding: 40px 0; text-align: center; }
.panel-empty--file .panel-empty__icon { font-size: 40px; }
.panel-empty--file .panel-empty__title { margin: 12px 0 4px; font-size: 15px; color: var(--td-text-color-primary); }
.panel-empty--file .panel-empty__desc  { margin: 0 0 16px; font-size: 13px; color: var(--td-text-color-secondary); }

.panel-empty--doc { display: block; padding: 40px 0; text-align: center; }
.panel-empty--doc .panel-empty__icon { font-size: 40px; }
.panel-empty--doc .panel-empty__title { margin: 8px 0 4px; font-size: 14px; color: var(--td-text-color-primary); }
.panel-empty--doc .panel-empty__desc  { margin: 0; font-size: 12px; color: var(--td-text-color-placeholder); }

.panel-empty--inline { display: block; text-align: center; padding: 16px 12px; font-size: 13px; }

// ---------- 提进基线的两条（不是变体）----------
.panel-head__main { min-width: 0; }              // 允许在 flex 里收缩，防溢出
.panel-head__title, .panel-head__meta { white-space: nowrap; }
```

> `--doc`/`--file` 用 `display: block` 覆盖基线的 `flex`：这两族的页面是「块级 + `text-align:center`」的
> 老写法，改成 flex 会移动图标与文字的相对位置（父卡实测 8 页空态高度变化就是这么来的）。

## 附录 B：度量工具

```bash
cd frontend
node scripts/shell-variant-report.cjs                     # 汇总：7 批的块/文件/行
node scripts/shell-variant-report.cjs --batch B5 --md b5.md   # 某批逐文件明细（写实施卡用）
node scripts/shell-variant-report.cjs --json out.json     # 完整数据（含每条声明与差异属性）
node scripts/shell-variant-report.cjs --strict            # 有非白名单变体即退出 1（待 B1 卡接入门禁）
```

输出示例（当前树，2026-11-16）：

```
壳样式变体报告 —— 基线 src/style/shell.less（27 类）
扫描 198 个 .vue + 1 个页面级 .less，命中 273 块 / 68 文件 / 约 1591 行
| B1 纯冗余删除（零观感）        | 20 | 16 |  75 |
| B2 间距/颜色归一               | 38 | 24 | 273 |
| B3 尺寸/字号档位               | 16 | 13 |  61 |
| B4 半变体补全                  | 33 | 17 | 245 |
| B5 空态与标签变体族           | 136 | 46 | 808 |
| B6 单页专有壳类                | 26 | 20 | 105 |
```
