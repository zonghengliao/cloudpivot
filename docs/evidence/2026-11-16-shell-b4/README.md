# 壳变体 B4 批：半变体补全 33 块（证据）

> 卡 `t_6cec42fc`（frontend-developer）。上游决策：`docs/architecture/shell-样式变体收敛-决策.md`
> §2.1（三条铁律）/ §2.2（属性分级）/ §2.3（变体清单）/ §4.1–§4.4 / §5 B4 批。
> 前置卡：`t_84bc7f25`（B3：`--dense` 定型、`--lg` 已定义）。
> 同族证据法：`docs/evidence/2026-11-16-shell-b1/`、`...-b2/`、`...-b3/`（本目录 9 个脚本是它们的 B4 版）。

**三句话结论：**

1. **33 块 / 17 文件删净，B4 归零**（199 → **166** 块；B1/B2/B3 仍 0，B5 136 / B6 26 一格未动）；
   `shell.less` 34 → **36 类**（新增 `.list-card--side`、`.panel-head__side--loose`；`--lg` 本批挂到 6 页 7 个元素）。
2. **逐页裁定：A（删本地吃基线）25 块 / B（命名变体）8 块 / C（白名单）0 块**，每一块都能在 §3 的
   裁定表里查到「今天缺什么 → 出路 → 补进来的属性 → 截图/探针结论」。
3. **观感变化逐条对得上**：静态等价 `LOST=0 CHANGED=0 NEW=0`（37 条有意变化全在白名单）；
   184 个壳类元素前后几何 32 条 / 样式值 47 条，全部归因到「字号两档 / 圆角归一 / 内边距归一 / 1–2px 行位移」；
   20 路由 × 2 视口逐像素比，**6 页有意义差异、14 页零差异**（后者含 4 页死代码页与 3 页「间距换容器」页）。

---

## 1. 改了什么

| 项 | 数量 | 内容 |
|---|---|---|
| 删掉的页面本地壳类声明 | **33 块 / 17 文件** | `panel-head__title`×10、`panel-head`×8、`list-card`×7、`tool-card`×5、`panel-head__main`×2、`panel-head__side`×1 |
| `shell.less` | 34 → **36 类** | 新增 `.list-card--side`（属性写全 5 条）、`.panel-head__side--loose`（属性写全 4 条） |
| 模板改动 | **10 行**（只加 class token） | `panel-head__title--lg`×7 元素 / `.panel-head__side--loose`×1 / `.list-card--side`×1 / `.ar-aging-tool`×1（页面自有类） |
| 页面自有布局类（非壳类） | 3 处 | `receivable` `.ar-aging-tool{margin-bottom:12px}`；`year-end` `.year-end-page{gap:12px}`；`support/config` `.config-page{gap:16px}` |
| 门禁快照 | 199 → **166 条** | `frontend/scripts/shell-variant-allowlist.json`（减 33、增 0） |

块数（`node frontend/scripts/shell-variant-report.cjs`，干净 worktree 实测）：

```
B4 落地前：199 块 / 62 文件 / 约 1110 行
B4 落地后：166 块 / 55 文件 / 约  932 行
逐批：B0 4、B1 0、B2 0、B3 0、B4 0、B5 136、B6 26
```

## 2. 卡面三处前提与实测不符（先看这段，再看裁定表）

这批的价值一半在「补属性」，一半在**把卡面假设换成实测**。下面每一条都有探针数字：

1. **`tool-card` 的 `flex` 并不缺。** 卡面说「今天它们等价于 `0 1 auto`（可缩），删本地后会变不可压缩」。
   实测 `opening / price / receivable / report-item / account / mapping / report` 的 `.tool-card` 计算值
   **全部是 `flex: 0 0 auto`** —— 基线 `.tool-card{flex:0 0 auto}` 早就生效，本地块只声明 padding /
   margin / 圆角 / 背景。「删了会挤坏列表卡」这个风险**不存在**；真正会变的是那几条装饰属性。
   同理 `list-card` 的 6 页实测都是 `flex:1 1 auto` + `min-width:0`。
2. **4 页的卡片头/标题是死代码。** `asset`、`sales-order`、`sales-return`、`shipment` 的 `.panel-head` /
   `.panel-head__title` DOM 属 **ListPage 组件模板**，不带页面 `data-v`，页面 scoped 从未命中
   （before 探针实测：`panel-head` margin-bottom=10px、`__title` font-size=14px，都是 ListPage 的值）。
   所以 `--lg` 只挂了 **6 页 7 个元素**（mapping / report / UncollectedPanel / price×2 / report-item /
   hr-salary），不是卡面写的 10 页；那 4 页删块 = **零观感变化**（像素 0 差异）。
   `list-card` 在这 3 个单据页同样是死选择器（ListPage 的卡类叫 `lp__list-card`）。
3. **`flex-wrap:wrap` 在两页从未触发。** `account` 的 `.panel-head` / `.panel-head__main` 与
   `UncollectedPanel` 的两级头都写了 `flex-wrap:wrap`；1280 与 1600 实测**都是单行**
   （标题组 x=272、右侧组 x=810/1156，同一 y），所以不立 `.panel-head--wrap`（卡面预留的 B 出口没有触发条件）。

另有两类必须靠「页面自有类」而不是壳类变体解决，理由见 §3 与 §5：

- **装载性间距**：`receivable`（账龄 Tab 两卡 12px）、`year-end`（12px）、`support/config`（16px）的卡片间距
  原本写在壳类的 `margin` 上，一删两张卡就贴在一起（`:bordered="false"` 会看起来像一张卡）。
  按决策 §2.2 末行「颜色/间距以外的装饰留在页面」，改成 **页面自有类 / 容器 `gap`** 承接，壳类只吃基线。
- **`help/center` 的 `.list-card` 不是右侧栏**，是三栏布局的中列（目录 230 / 命中列表 320 / 正文吃满）。
  它必须同时带 `display:flex; flex-direction:column; min-height:0`，否则内部不再滚动、pager 不会沉底 ——
  所以 `.list-card--side` 按铁律 B「属性写全」把 **5 个属性全写**（不是只写 flex/width）。

## 3. 逐页裁定表（本批核心产物）

口径：**「今天缺什么」= 与 shell.less 基线逐属性比（`shell-variant-report.cjs` 的 diff）**；
**「A 补进来的属性」= 删本地后基线补进的值**；截图结论 = `logs/pixels-1600.log` / `-1280.log` 的实测
（有意义差异阈值参考对照组噪声天花板：4px / maxΔ=22 / 全在 y≤8 的通知铃）。

| # | 页 | 类 | 今天缺什么（本地 vs 基线） | 出路 | A 补进来 / B 变体写全 | 截图结论 |
|--:|---|---|---|---|---|---|
| 1 | erp/account | `panel-head` | 无缺；**多写** `flex-wrap:wrap` | **A** | 无新属性，仅失 `flex-wrap` | 两档宽度**从未换行**；像素 0 差异 |
| 2 | erp/account | `panel-head__main` | 缺 `min-width`；多写 `flex-wrap` | **A** | `min-width:0` | 无几何变化（0 差异） |
| 3 | erp/asset | `panel-head` | 缺 `gap`；`margin-bottom` 12≠10 | **A**（死代码） | `gap:12px`、`mb:10px` | 像素 0 差异（before 实测已是 10px） |
| 4 | erp/asset | `panel-head__title` | 缺 `color`/`white-space` | **A**（死代码） | `color:primary`、`nowrap`（ListPage 早给了） | 像素 0 差异（before 实测 14px） |
| 5 | erp/budget/mapping | `panel-head__title` | 同上 | **B** `--lg` | 基线 color/nowrap + `--lg` 16px | 仅 CLASS 变化，像素 0 |
| 6 | erp/budget/report | `panel-head__title` | 同上 | **B** `--lg` | 同上 | 仅 CLASS，像素 0 |
| 7 | erp/cost-period/UncollectedPanel | `panel-head` | 缺 `justify-content`/`margin-bottom`；`ai:center`≠baseline、`gap:8`≠12；多写 wrap | **A** | `jc:space-between`、`mb:10px`、`ai:baseline`、`gap:12px`、`nowrap` | 标题行 y −1px、ops `margin-left` 213→200；差异带 y∈[212,232] |
| 8 | 同上 | `panel-head__main` | 缺 `min-width`；`ai:center`、`gap:8`；多写 wrap | **A** | `min-width:0`、`ai:baseline`、`gap:10px` | main 宽 719→728（+9 来自 15→16px），高 22→23 |
| 9 | 同上 | `panel-head__title` | 缺 color/nowrap；**15px** | **B** `--lg` | 16px（15→16 属有意变化） | 宽 75→80，差异带 y∈[212,232] |
| 10 | erp/ledger | `panel-head__side` | `display`/`align-items`/`gap`/`flex-wrap` **4 条全不同** | **B** `--side--loose` | 变体写全 4 条 = 本地原样 | 仅 CLASS 变化，像素 0 |
| 11 | erp/opening | `tool-card` | 缺 `flex`（早已是基线值）；多写 `padding:4px 8px` | **A** | flex 无变化；内边距回落 0 | 卡片高 72→64、内容左移 8px；像素 127k（网格整体位移，无挤扁/溢出） |
| 12 | erp/price | `tool-card` | 缺 `flex`（早已）；多写 `background` + `border-radius:var(--td-radius-medium)` | **A** | flex 无变化；背景=TDesign 默认；圆角 6→**10** | 仅圆角（像素 5.7k，布局零变化） |
| 13 | erp/price | `panel-head__title` ×2 | 缺 color/nowrap；**15px** | **B** `--lg` ×2 | 16px | 两个标题宽 60→64 |
| 14 | erp/receivable | `tool-card` | 缺 `flex`/`min-width`（早已）；多写 `mb:12`+`radius:8` | **A**（间距转页面类 `.ar-aging-tool`） | `min-width:0`；圆角 8→10 | 卡片矩形零变化（间距 12px 保住），仅圆角 |
| 15 | erp/receivable | `list-card` | 同上 | **A** | `flex:1 1 auto`/`min-width:0`（早已）；圆角 10 | 圆角变化；`mb` 落在页底，无害 |
| 16 | erp/report/report-item | `tool-card` | 缺 `flex`（早已）；多写 `radius:8` | **A** | 圆角 8→10 | 行位移 ≤2px |
| 17 | 同上 | `list-card` | 同上 | **A** | 圆角 8→10 | 列表卡高 233→231（头部下边距 12→10） |
| 18 | 同上 | `panel-head` | `align-items:flex-end`、`mb:12` | **A** | `ai:baseline`、`mb:10px` | 行位移 ≤2px（表格整体上移 2px） |
| 19 | 同上 | `panel-head__title` | 缺 color/nowrap；**15px** | **B** `--lg` | 16px | 字号变化（标题本页为空渲染，见 §7 打桩说明） |
| 20 | erp/sales-order | `list-card` | 缺 `flex`/`min-width`；多写 radius（**死选择器**） | **A** | 无（ListPage 的 `lp__list-card`） | 像素 0 差异 |
| 21 | erp/sales-return | `list-card` | 同上 | **A** | 同上 | 像素 0 差异 |
| 22 | erp/shipment | `list-card` | 同上 | **A** | 同上 | 像素 0 差异 |
| 23 | erp/sales-order | `panel-head` | 缺 `justify-content`；`mb:8`（**死代码**） | **A** | 无（ListPage 自带 space-between/10） | 像素 0 差异 |
| 24 | erp/sales-return | `panel-head` | 同上 | **A** | 同上 | 像素 0 差异 |
| 25 | erp/shipment | `panel-head` | 同上 | **A** | 同上 | 像素 0 差异 |
| 26 | erp/sales-order | `panel-head__title` | 缺 color/nowrap；16px（**死代码**） | **A** | 无（ListPage 14px） | 像素 0 差异 |
| 27 | erp/sales-return | `panel-head__title` | 同上 | **A** | 同上 | 像素 0 差异 |
| 28 | erp/shipment | `panel-head__title` | 同上 | **A** | 同上 | 像素 0 差异 |
| 29 | erp/year-end | `tool-card` | 缺 `flex`（早已）；多写 `mb:12` | **A**（间距转容器 `.year-end-page{gap:12px}`） | flex 无变化 | 像素 0 差异（三张卡 12px 间距不变） |
| 30 | help/center | `list-card` | 缺 `min-width`；`flex:0 0 320px`+`width`+`display/flex-direction/min-height` | **B** `--side`（写全 5 属性） | `min-width:0` 补进来 | 仅 CLASS 变化，像素 0 |
| 31 | hr/salary | `panel-head` | 缺 `gap`；`ai:center`、`mb:12` | **A** | `gap:12px`、`ai:baseline`、`mb:10px` | 单子元素 → 无几何变化；抽屉内容 2px 位移 |
| 32 | hr/salary | `panel-head__title` | 缺 color/nowrap；16px | **B** `--lg` | color/nowrap 补进来 | 仅 CLASS + 无几何 |
| 33 | support/config | `list-card` | 缺 `flex`/`min-width`（早已）；多写 `margin-top:16` | **A**（间距转容器 `.config-page{gap:16px}`） | `flex:1 1 auto`/`min-width:0` | 像素 0 差异（两卡 16px 间距不变） |

**计数**：A = 25 块 / **13 页**（其中 11 块是死代码或早已由基线生效，零观感）；B = 8 块 / **8 页**（2 个新类 + `--lg` 复用）；
C = **0 块**（没有一条需要进白名单保留页面声明）。A+B 去重后正好覆盖本批 17 个文件（其中 4 页同时有 A 与 B：UncollectedPanel / price / report-item / hr-salary）。

## 4. `receivable` 的间距为什么走页面自有类

`.tabs` 里两张卡（工具卡 + 列表卡）之间原本靠 `.tool-card,.list-card{margin-bottom:12px}`。
壳类不能留声明，而 `.ar-page{padding:16px}` 没有 `gap`、tab 面板内部又是 TDesign 自己的 DOM
（对 `:deep(.t-tab-panel)` 上 `display:flex` 会把 `v-show` 隐藏的面板顶出来），
所以按决策 §2.2 末行「装饰留在页面」：

```less
/* 页面自有布局类，不是壳类声明 */
.ar-aging-tool { margin-bottom: 12px; }
```

模板只加 token：`<t-card class="tool-card ar-aging-tool">`。探针验证：`receivable` 的两张卡
**矩形零变化**（GEOM 0 条），只有圆角 8→10 的样式值变化。

`year-end` / `support/config` 同理但更简单——容器本来就是一列，直接给容器 `gap`，`.tool-card/.main-card`
与 `.list-card` 上的 margin 退役（像素 0 差异，说明间距没变）。

## 5. 两个新变体（登记到决策 §2.3）

| 变体 | 语义 | 页数 | 属性（写全） |
|---|---|--:|---|
| `.list-card--side` | 帮助中心三栏布局的**固定 320px 中列**（目录 230 / 命中列表 320 / 正文吃满），内部要自己滚、pager 沉底 | 1 | `flex:0 0 320px; width:320px; display:flex; flex-direction:column; min-height:0` |
| `.panel-head__side--loose` | 卡片头右侧槽的**松排**（ledger 的「当前科目 + 余额口径说明」）：12px 间距、按文字基线对齐、窄窗允许换行 | 1 | `display:inline-flex; align-items:baseline; gap:12px; flex-wrap:wrap` |

> `.panel-head__side--loose` 的 `display:inline-flex` 在 flex 父里会被 **blockify 成 `flex`**
> （实测计算值就是 `flex`），声明按原样保留以自洽；与基线 `flex/center/6px` 的实际差异是
> `align-items` + `gap` + `flex-wrap` 三条。

## 6. 有意变化（37 条，机械核对）

`allow-intentional.txt` 的 37 条 = 静态等价脚本里唯一被放行的项，按四组归类：

1. **死代码**（11 条）：asset / sales-order / sales-return / shipment 的 `panel-head*` 与 `list-card`；
   before 探针实测已是基线值，像素 0 差异。
2. **档位归一**（14 条）：`__title` 15→16px（3 页）；卡片圆角 8→10 / 6→10（6 处）；
   卡片头 `margin-bottom` 12|8→10、`align-items` center|flex-end→baseline（5 处）。
3. **间距改由页面类/容器承接**（4 条）：receivable ×2、year-end、support/config。
4. **`flex-wrap` 实测从未触发**（4 条）+ **装饰归一**（3 条：opening padding、price 背景/圆角）。

逐条明细见 `logs/equivalence.log`。

## 7. 验真套件（全部可复现）

```bash
# 0) 干净双树：before = 2f5dbea4（B3 落地后的 HEAD），after = 本批提交
git worktree add --detach .worktrees/b4-before 2f5dbea4
git worktree add --detach .worktrees/b4-after  <本批 commit>
ln -s /opt/cloudpivot/frontend/node_modules .worktrees/b4-before/frontend/node_modules
ln -s /opt/cloudpivot/frontend/node_modules .worktrees/b4-after/frontend/node_modules
(cd .worktrees/b4-before/frontend && node ./node_modules/vite/bin/vite.js build)   # → dist-before
(cd .worktrees/b4-after/frontend  && node ./node_modules/vite/bin/vite.js build)   # → dist-after

# 1) 块数 + 门禁快照
node frontend/scripts/shell-variant-report.cjs                    # 166 块，B4 行消失
node frontend/scripts/shell-variant-report.cjs --baseline frontend/scripts/shell-variant-allowlist.json

# 2) 静态等价（before/after 各出一份 --json）
node frontend/scripts/shell-variant-report.cjs --json /tmp/report-before.json   # before 树
node frontend/scripts/shell-variant-report.cjs --json /tmp/report-after.json    # after 树
node docs/evidence/2026-11-16-shell-b4/equivalence-b4.mjs \
     /tmp/report-before.json /tmp/report-after.json \
     docs/evidence/2026-11-16-shell-b4/variants.json \
     --allow docs/evidence/2026-11-16-shell-b4/allow-intentional.txt
#   → LOST=0 CHANGED=0 NEW=0，有意变化 37 条

# 3) 逐元素探针（1280 / 1600 两档；同一份 dist 连跑两遍做对照组）
cd frontend
node ../docs/evidence/2026-11-16-shell-b4/shell-probe-b4.mjs --dist <dist-before> --width 1600 --port 5223 --out /tmp/before-1600.json
node ../docs/evidence/2026-11-16-shell-b4/shell-probe-b4.mjs --dist <dist-after>  --width 1600 --port 5225 --out /tmp/after-1600.json
node ../docs/evidence/2026-11-16-shell-b4/shell-probe-b4.mjs --dist <dist-before> --width 1280 --port 5224 --out /tmp/before-1280.json
node ../docs/evidence/2026-11-16-shell-b4/shell-probe-b4.mjs --dist <dist-after>  --width 1280 --port 5226 --out /tmp/after-1280.json
node ../docs/evidence/2026-11-16-shell-b4/shell-probe-diff.mjs /tmp/before-1600.json /tmp/after-1600.json
node ../docs/evidence/2026-11-16-shell-b4/shell-probe-diff.mjs /tmp/before-1280.json /tmp/after-1280.json

# 4) 截图 + 逐像素（同 dist 连拍两次 = 对照组噪声天花板）
node ../docs/evidence/2026-11-16-shell-b4/shots-b4.mjs --dist <dist-before> --width 1600 --port 5230 --out /tmp/shots/before-1600
node ../docs/evidence/2026-11-16-shell-b4/shots-b4.mjs --dist <dist-after>  --width 1600 --port 5231 --out /tmp/shots/after-1600
node ../docs/evidence/2026-11-16-shell-b4/shots-b4.mjs --dist <dist-after>  --width 1600 --port 5234 --out /tmp/shots/after2-1600
node ../docs/evidence/2026-11-16-shell-b4/pixels.mjs --before /tmp/shots/before-1600 --after /tmp/shots/after-1600  --out /tmp/shots/diff-1600
node ../docs/evidence/2026-11-16-shell-b4/pixels.mjs --before /tmp/shots/after-1600  --after /tmp/shots/after2-1600 --out /tmp/shots/diff-control
#   1280 同法（before-1280 / after-1280）
```

> 本目录 `logs/*.log` 是上列脚本的原样输出（仓库 `.gitignore` 忽略 `*.log`，本批用 `git add -f` 入库，
> 便于复核时不必重跑）：`report-before/after.json`（报告工具 JSON）、`equivalence.log`（静态等价）、
> `probe-*.log` + `diff-*.log`（逐元素探针）、`pixels-*.log`（逐像素）、`check-fast.log`（L0+L1 门禁原样输出）、
> `build-after.log`、`template-token-check.log`、`typecheck*.log`。

## 8. 数字汇总

| 验收项 | 结果 |
|---|---|
| ① 报告工具 | **166 块 / 55 文件 / 约 932 行**；B0 4、B1 0、B2 0、B3 0、**B4 0**、B5 136、B6 26（before 树同命令 199 / B4 33） |
| ① 门禁快照 | 减 33、增 0；`shell.less` classCount 34 → 36；`SHELL_VARIANT_GATE` 在 L0 实测 `✓ PASS` |
| ② 静态等价 | 17 文件 / 53 类：**LOST=0 CHANGED=0 NEW=0**；有意变化 37 条全在白名单 |
| ③ 逐元素探针 | 20 路由 / **184 元素**；几何/结构 **32 条**（GEOM 23 + CLASS 9）、样式值 **47 条**；1280 与 1600 **条数完全一致**（无宽度特异问题） |
| ③ 差异归因 | `font-size` 4、`border-radius` 5、`align-items` 4、`margin-*` 5、`padding` 4、`flex-wrap` 4、`gap×3` 4 …其余为几何位移（≤2px，除 opening 的内边距位移） |
| ④ 像素 1600 | 20 路由 × 2 张（viewport + 工具行裁切）= 40 张；**6 页有意义差异**（cost-period / opening / price / receivable / report-item / hr-salary）、**14 页零差异** |
| ④ 像素 1280 | 同上；**同样是这 6 页**，且零差异页里包含 account / asset / mapping / report / ledger / sales-×3 / year-end / help / support-config |
| ④ 对照组 | after vs after2（同 dist 连拍）：噪声天花板 **4px / maxΔ=22 / 全在 y≤8**（通知铃区域） |
| ⑤ 门禁 | `vue-tsc --noEmit` exit 0；`tsconfig.unit` exit 0；`vite build` exit 0（`pnpm build` = 这两步）；`bash scripts/check.sh --l0` exit 0（含壳类闸 PASS）；`pnpm lint:shell` exit 0（快照 166 = 当前 166） |
| ⑤ 后端 L1（cargo fmt/clippy/check） | **本批零后端改动**（`git diff --stat` 只有 frontend 文件）。`bash scripts/check.sh --fast`（L0+**L1 全层**）已在干净 worktree 跑通：`cargo fmt --check` / `cargo clippy --all-targets -- -D warnings` / `cargo check` / `cargo test -p cloudpivot-doc` / `pnpm typecheck` / `pnpm typecheck:unit` 全绿，末行 `==> 全部检查通过（L0 L1）`，`FAST_EXIT=0`，原文见 `logs/check-fast.log`。<br>跑法：`CARGO_TARGET_DIR=/opt/cloudpivot/backend/target bash scripts/check.sh --fast`（复用主工作区编译缓存，避免与其它卡抢 `:8023` 以外的资源；源码仍是本 worktree 的干净树）。`/opt/cloudpivot` 主工作区在施工期间被其它卡改动了 `backend/app/src/business/erp/voucher/*` 等 29 个文件，那里的结果不属本批，故不在主工作区跑 |

## 9. 已知的探针打桩噪声（与本次改动无关）

`/business/erp/report/report-item` 在截图里有一条红色错误条「Cannot read properties of undefined
(reading 'V')」—— 这是**打桩数据形状不全**导致的页面渲染异常，**before / after 两张图完全一致**
（见 `shots/changed-1600/before-*` 与 `after-*`），因此不影响像素结论；该页的差异只落在
标题行与表格上移 2px 的位置。

## 10. 留给 B5 的两件事

1. `hr/salary` 的 `panel-head__title` 只有「逐人核对」（试算抽屉里）挂了 `--lg`；ListPage 自带的列表头
   仍是 14px 基线 —— B5 若给 ListPage 加 title 变体，需要组件侧开口，不在本批范围。
2. `help/center` 的 `list-card__pager`（B6 块）仍是页面声明；本批只把它的父级 `.list-card` 拆成
   `.list-card--side` + 顶层 `.list-card__pager`，嵌套结构没改语义。B6 收这个类时记得它在
   `list-card--side` 的卡里（pager 要留在列的底部）。
