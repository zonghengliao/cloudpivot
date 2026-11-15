# 壳变体 B3 批：尺寸/字号档位 16 块（证据）

> 卡 `t_84bc7f25`（frontend-developer）。上游决策：`docs/architecture/shell-样式变体收敛-决策.md`
> §2.3（变体清单）/ §4.2（字号两档）/ §4.4（宽度：`--dense` + 基线）/ §5 B3 批。
> 前置卡：`t_79f92c08`（B1，门禁）、`t_9c90f3d1`（B2，`--dense` 已存在，只含 `gap:8px`）。
> 同族证据法：`docs/evidence/2026-11-16-shell-b1/`、`...-b2/`（本目录 8 个脚本是它们的 B3 版）。

**三句话结论：**

1. **16 块 / 13 文件删净，B3 归零**（215 → **199** 块；B1/B2 仍 0，B4/B5/B6 一格未动）；`shell.less` 32 → **34 类**。
2. **换行点零变化**：22 条采集路由逐页比「工具行行数 + 每行成员」**全同**（`wrap-probe-b3.mjs`），
   13 页截图另存 `*.toolrow.png`（工具行整行入镜）供人眼复核。
3. **观感变化逐条对得上**：静态等价 `LOST=0 CHANGED=0 NEW=0`（有意变化 16 条全在白名单）；
   238 个壳类元素前后几何 24 条 / 样式值 40 条，全部落在「宽度归属」上；22 页 ×（viewport + 工具行裁切）
   逐像素比，**10 页有意义差异、12 页零差异**（其中 receipt-refund 那 4px 落在右上角 y≤8，与对照组
   同位置同等量级 = 通知铃噪声，真实变化 9 页；对照组噪声天花板 4px，全在 y≤8）。

---

## 1. 改了什么

| 项 | 数量 | 内容 |
|---|---|---|
| 删掉的页面本地壳类声明 | **16 块 / 13 文件** | `tool-row__period`×5、`tool-row__date`×2、`panel-head__meta`×2、`__type`/`__keyword`/`__range`/`__status`/`panel-head__main`/`__title`/`panel-head__hint` 各 1 |
| `shell.less` | 32 → **34 类** | 新增 `.panel-head__title--lg`、`.panel-head__hint--right`；`--dense` 补 `__period/__date{width:140px}`；基线补 `__main{min-width:0}`、`__title/__meta{white-space:nowrap}` |
| 模板改动 | **7 行** | 5 行「新增 class 属性行」（`<ListPage class="tool-row--dense">` ×5）+ 2 行「已有 class 追加 token」（ledger 的 `<div class="tool-row">`、hr/employee 的 `panel-head__hint`） |
| 门禁快照 | 215 → **199 条** | `frontend/scripts/shell-variant-allowlist.json`（减 16、增 0） |

块数（`node frontend/scripts/shell-variant-report.cjs`，干净 worktree 实测）：

```
B3 落地前：215 块 / 63 文件 / 约 1171 行
B3 落地后：199 块 / 62 文件 / 约 1110 行
逐批：B0 4、B1 0、B2 0、B3 0、B4 33、B5 136、B6 26
```

## 2. 宽度归一：`--dense` 的两种用法（这批最容易踩的坑）

卡面写「这 4 页加 `.tool-row--dense`，并在 `--dense` 里补 `__period{width:140px}`」。落地时发现
**`--dense` 的宿主不一样**，必须分开处理，否则改了等于没改：

| 页面族 | 筛选行容器 | `--dense` 挂哪 | `gap:8px` 生效吗 |
|---|---|---|---|
| 非 ListPage（budget/mapping、budget/report、ledger Tab2、report/payable-aging） | `<div class="tool-row">` | 直接挂在那个 `div` 上 | 生效 |
| **ListPage 系**（cost-alloc-rule、cost-labor、cost-overhead、ledger Tab1、receipt-refund） | 组件自持的 `.lp__filter-row`（**不是** `.tool-row`） | 挂在 `<ListPage>` 根节点（Vue 把 class 并到 `.lp` 上） | **不生效**（页面本来也不是 8px） |

`--dense` 的宽度那条写成后代选择器 `.tool-row--dense .tool-row__period/.tool-row__date{width:140px}`，
所以挂根节点照样命中；`gap` 那条是给 `.tool-row` 的，ListPage 页面吃不到 —— 这正是想要的：
**那 5 页只需要「控件收窄」，不需要「行更挤」**。探针实测：cost-alloc-rule / ledger Tab1 / receipt-refund
的 `period/date` 前后都是 140（零几何差异），cost-labor / cost-overhead 从 132 → 140。

## 3. 三个卡面之外的实测发现（都写进决策文档了）

1. **`__type` 不必进 `--dense`**：卡面说「budget/mapping 已是 `--dense` → 在 `--dense` 里统一 `__type{140px}`」，
   但基线 `.tool-row__type` **本来就是 140px**。在 `--dense` 里再写一条是死代码，所以只删本地 150。
2. **`text-align:right` 不能折进 `hint--end`**：卡面让 hr/employee「用 `hint--end` **再加** `text-align:right`」。
   `hint--end` 另有 3 页在用（sales-order / sales-return / shipment），它们的 hint 是**两行长句**，
   把 `text-align` 折进 `--end` 会让这 3 页的长句换成右对齐（本卡文件清单里没有它们）。
   照 `tip--right` 的先例另立 `.panel-head__hint--right`（正交、属性写全、已登记 §2.3），hr/employee 两个都挂。
3. **hr/salary 的 `panel-head__meta` 默认视图里根本不在**：它在「试算」抽屉的「逐人核对」里
   （`v-if="calcResult"`）。为把它采到/截到，本目录的探针与截图脚本加了两个动作：
   点 `hr-salary-preview` 开抽屉 → 点 `hr-salary-calc-run` 跑一次试算（`/salary/calc` 用具形状桩）。
   不这么做的话那一页前后像素必然 0 差异，等于没验。

## 4. `hr/employee` 的 `panel-head__hint`：归一决策（带实测数字）

该说明只在**没有 `business:hr:employee:create` 权限**时才渲染，所以默认超管桩永远看不到它。
本目录脚本带 `--perms narrow`（只给「看」的权限），前后各采一次：

| 属性 | before（本地声明） | after（`--end` + `--right` + 基线） | 判定 |
|---|---|---|---|
| `x` / `w` | 1344 / 216 | **1344 / 216** | 位置与宽度不变 |
| `text-align` | `right` | `right` | 由 `--right` 接管 ✓ |
| `max-width` | `460px` | `none` | **实测无效**（文案只有 216px，从来不换行）→ 归一删掉 |
| `line-height` | `18px` | `22px`（默认） | 盒子高 18 → 22，**与 `panel-head` 行高（22px）和基线对齐位置不变**；18px 是把盒压扁 4px 的填值 → 归一删掉 |
| `margin-left` | `0`（靠 `space-between` 顶到最右） | `auto`（`--end`，计算值 730px） | 见下方「副作用」 |

**副作用（有意变化，已截图）**：这一行的 `.panel-head` 是 `space-between`，该说明本来就贴最右；
加上 `margin-left:auto` 后 `auto` 先吃掉全部自由空间，**同级的那条 `filter-row__hint`
（「默认不含离职…」）从 x=751 挪到 x=386**，与标题聚成一组，中间留空。
卡面明确要求挂 `hint--end`，且截图看这一版分组更自然（标题+口径提示靠左，权限说明单独钉右侧），
故保留，并把这条副作用写进证据与卡面 metadata（`wrap_changes` 之外的 `layout_side_effects`）。

## 5. 逐页核对表（机械生成：探针 JSON + 像素日志 + 换行点 JSON）

见 `INTENTIONAL-CHANGES.md`（由 `gen-intentional-table.mjs` 生成，不手抄数字）。

## 6. 换行点检查（B3 特有验收）

`wrap-probe-b3.mjs`：对每个 `.tool-row` / `.lp__filter-row` 取**直接子元素的 top 分组**（0×0 的
`__spacer` 排除，否则会把 1 排误算成 2 排），算出「行数 + 每行成员」，前后逐页比对：

```
22 条采集路由 → 换行点有变化的页：无
```

现状（after）2 排的页：cost-labor、receipt-refund、stock-out、report/report-item(第 1 条)、hr/employee
—— 前后**完全一致**（宽度在行内腾挪，没有把任何控件挤到下一排，也没有把原本 2 排的页压回 1 排）。
逐页明细见 `logs/wrap-after.log` 与 `logs/wrap-after.json`。

## 7. 验真套件（全部可复现）

```bash
# 0) 干净树：before = facc0235（B2 落地后的 HEAD），after = 本批提交
git worktree add --detach /tmp/wt-b3before facc0235
ln -s /opt/cloudpivot/frontend/node_modules /tmp/wt-b3before/frontend/node_modules
git worktree add --detach /tmp/wt-b3after  <本批 commit>
ln -s /opt/cloudpivot/frontend/node_modules /tmp/wt-b3after/frontend/node_modules

# 1) 块数 + 门禁快照（两棵树各跑一次）
node frontend/scripts/shell-variant-report.cjs                 # 215 → 199，B3 归零
node frontend/scripts/shell-variant-report.cjs --baseline frontend/scripts/shell-variant-allowlist.json

# 2) 静态等价（before/after 各出一份 --json，再做逐类逐属性比对）
node frontend/scripts/shell-variant-report.cjs --json /tmp/b3/report-before.json   # 在 before 树
node frontend/scripts/shell-variant-report.cjs --json /tmp/b3/report-after.json    # 在 after 树
node docs/evidence/2026-11-16-shell-b3/equivalence-b3.mjs \
     /tmp/b3/report-before.json /tmp/b3/report-after.json \
     docs/evidence/2026-11-16-shell-b3/variants.json \
     --allow docs/evidence/2026-11-16-shell-b3/allow-intentional.txt

# 3) 模板自查（只许「追加 token」与「新增 class 属性行」）
node docs/evidence/2026-11-16-shell-b3/template-token-check-b3.mjs --base facc0235

# 4) 逐元素探针（3 遍：before / after / after2 对照）
cd frontend
node ../docs/evidence/2026-11-16-shell-b3/shell-probe-b3.mjs --dist /tmp/b3/dist-before --out /tmp/b3/before.json
node ../docs/evidence/2026-11-16-shell-b3/shell-probe-b3.mjs --dist /tmp/b3/dist-after  --out /tmp/b3/after.json
node ../docs/evidence/2026-11-16-shell-b3/shell-probe-b3.mjs --dist /tmp/b3/dist-after  --out /tmp/b3/after2.json
node ../docs/evidence/2026-11-16-shell-b3/shell-probe-diff.mjs /tmp/b3/after.json  /tmp/b3/after2.json   # 对照组必须 0/0
node ../docs/evidence/2026-11-16-shell-b3/shell-probe-diff.mjs /tmp/b3/before.json /tmp/b3/after.json
#    无建档权限态（hint 才渲染）
node ../docs/evidence/2026-11-16-shell-b3/shell-probe-b3.mjs --dist /tmp/b3/dist-before --perms narrow \
     --routes /tmp/b3/routes-narrow.txt --out /tmp/b3/before-narrow.json
node ../docs/evidence/2026-11-16-shell-b3/shell-probe-b3.mjs --dist /tmp/b3/dist-after  --perms narrow \
     --routes /tmp/b3/routes-narrow.txt --out /tmp/b3/after-narrow.json
#    `.panel-head` 全部直接子元素的几何（`filter-row__hint` 不是壳类，主探针不收）
node ../docs/evidence/2026-11-16-shell-b3/head-children-probe.mjs --dist /tmp/b3/dist-before
node ../docs/evidence/2026-11-16-shell-b3/head-children-probe.mjs --dist /tmp/b3/dist-after

# 5) 换行点
node ../docs/evidence/2026-11-16-shell-b3/wrap-probe-b3.mjs --dist /tmp/b3/dist-before --out /tmp/b3/wrap-before.json
node ../docs/evidence/2026-11-16-shell-b3/wrap-probe-b3.mjs --dist /tmp/b3/dist-after  --out /tmp/b3/wrap-after.json

# 6) 截图 + 像素（viewport + toolrow 裁切两套；对照组同 dist 连拍两次）
node ../docs/evidence/2026-11-16-shell-b3/shots-b3.mjs --dist /tmp/b3/dist-before --out /tmp/b3/shots/before
node ../docs/evidence/2026-11-16-shell-b3/shots-b3.mjs --dist /tmp/b3/dist-after  --out /tmp/b3/shots/after
node ../docs/evidence/2026-11-16-shell-b3/shots-b3.mjs --dist /tmp/b3/dist-after  --out /tmp/b3/shots/after2
node ../docs/evidence/2026-11-16-shell-b3/pixels.mjs --before /tmp/b3/shots/before --after /tmp/b3/shots/after  --out /tmp/b3/shots/diff
node ../docs/evidence/2026-11-16-shell-b3/pixels.mjs --before /tmp/b3/shots/after  --after /tmp/b3/shots/after2 --out /tmp/b3/shots/diff-control
#    无建档权限态
node ../docs/evidence/2026-11-16-shell-b3/shots-b3.mjs --dist /tmp/b3/dist-before --perms narrow --routes /tmp/b3/routes-narrow.txt --out /tmp/b3/shots-narrow/before
node ../docs/evidence/2026-11-16-shell-b3/shots-b3.mjs --dist /tmp/b3/dist-after  --perms narrow --routes /tmp/b3/routes-narrow.txt --out /tmp/b3/shots-narrow/after
```

## 8. 数字汇总

| 验收项 | 结果 |
|---|---|
| ① 报告工具 | **199 块 / 62 文件 / 约 1110 行**；B0 4、B1 0、B2 0、**B3 0**、B4 33、B5 136、B6 26（before 树同命令 215 / B3 16） |
| ① 门禁快照 | 减 16、增 0；`shell.less` classCount 32 → 34 |
| ② 静态等价 | 13 文件 / 54 类：**LOST=0 CHANGED=0 NEW=0**；有意变化 16 条全在白名单（`logs/equivalence.log`） |
| ② 模板自查 | 13 文件：7 个逐字节未变、2 行纯追加 token、5 行新增 class 属性行、**异常 0** |
| ③ 逐元素探针 | 22 路由 / **238 元素**（after 侧 243，多出 5 个是 `<ListPage>` 根节点因新增 `tool-row--dense` 被选择器命中，非 DOM 变化）；对照组 `after vs after2` **几何 0 / 样式 0** |
| ③ 前后差异 | 几何/结构 **24 条**（`GEOM` 18 + `COUNT` 5 个探针假差异（`<ListPage>` 根节点新挂 `--dense` 被选择器命中）+ `CLASS` 1）；样式值 **40 条**（`white-space` 15、`width` 12、`min-width` 9、`gap/row-gap/column-gap` 各 1、`font-size` 1） |
| ④ 变体齐全 | dist 里 7 个变体逐条 `grep` 到：`tool-row--dense{gap:8px}` + `--dense __period/__date{width:140px}`、`title--lg`、`tip--end`、`tip--right`、`hint--end`、`hint--right`、`main--nowrap` |
| ③ narrow 态 | hr/employee hint：`h/max-width/line-height/margin-left` 4 条样式差异 + 高度 18→22（见 §4） |
| ④ 像素 | 22 页 ×（viewport + 工具行裁切）= **42 张/套 × 3 套**（before / after / 对照组 after2）；有意义差异 **10 页**（mapping、report、cost-labor、cost-overhead、ledger-trial、payable-aging、stock-out、transfer、hr-salary，+ receipt-refund 仅右上角 4px 噪声）、**12 页零差异**；对照组只有 1 页 4px 落在右上角 y≤8（maxΔ=22，与改动无关）→ 噪声天花板 |
| ⑤ 门禁 | `pnpm typecheck` 0 / `pnpm build` exit 0（dist 内 7 个变体齐全）/ `bash scripts/check.sh --fast` **exit 0**（L0+L1）/ `pnpm lint:shell` exit 0（快照 199 = 当前 199） |

## 9. 留给 B4 的两件事

1. **`.panel-head__main--nowrap` 已冗余**：基线 `__title/__meta` 现在都是 `nowrap`，`__main` 的 min-content
   不会再塌到单字宽。1600px 下实测无作用；窄窗未验证，故本批保留、只登记（决策 §2.3）。
2. **三个 15px 标题页**（`cost-period/steps/UncollectedPanel`、`price`、`report/report-item`）本批只
   **定义**了 `.panel-head__title--lg`，没挂类 —— 它们的 `panel-head__title` 块同时缺基线属性，
   按卡面归 B4 一起处理，避免两卡改同一文件。
