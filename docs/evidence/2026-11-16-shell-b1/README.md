# 壳变体 B1 批：纯冗余删除 20 块 + 「页面零壳类声明」门禁（证据）

> 卡 `t_79f92c08`（frontend-developer）。上游决策：`docs/architecture/shell-样式变体收敛-决策.md`
> §2.4 门禁口径 / §5 B1 批（commit `9c2a8543`）。
> 父卡的同类证据法：`docs/evidence/2026-11-16-shell-style-sink/shell-sink-evidence.md` §3。
>
> 结论三句话：**20 块 / 16 文件删净（273 → 253 块）**；**静态逐属性比对 LOST/CHANGED/NEW 全 0**
> （唯一值变化是 5 处 `.tool-row__spacer{flex:1→1 1 auto}`，几何零变化）；**门禁已挂 L0 warn 档**，
> 反证双向可抓（新增即 exit 1，删除只提示 FIXED）。

---

## 1. 改了什么

| 项 | 内容 |
|---|---|
| 删掉的本地壳类声明 | `.panel-head__meta`×6、`.panel-head__tip`×6、`.tool-row__spacer`×5、`.panel-empty__icon`×2、`.tool-row__hint`×1 = **20 块 / 16 文件** |
| 行数 | 16 个 `.vue` 的 `<style>` 区 **+0 / −89**（`git diff --numstat`），模板与 `<script>` 逐字节未变 |
| 工具 | `frontend/scripts/shell-variant-report.cjs` 新增 `--baseline <json>` / `--write` 门禁档 |
| 快照 | `frontend/scripts/shell-variant-allowlist.json`（**删完之后**的 253 条，含 `shell.less` 27 类的哈希 `d3c3cd330ef153e2`）|
| 门禁 | `frontend/package.json` → `lint:shell`；`scripts/check.sh` **L0** 挂载，`SHELL_VARIANT_GATE=block\|warn\|off`，默认 **warn** |
| 文档 | 决策 §2.4 补「门禁已挂 warn 档 + 开关名」、§1.4 补 B1 后实测、附录 B 补 `--baseline` 用法与快照格式 |

块数变化（`node frontend/scripts/shell-variant-report.cjs`）：

```
B1 落地前：273 块 / 68 文件 / 约 1591 行
B1 落地后：253 块 / 68 文件 / 约 1468 行      ← B1 行消失；B2–B6 块数不变（38/16/33/136/26）
```

## 2. 两条等价前提（在本分支上复核，不是抄上游结论）

1. **`.panel-empty__icon` 的祖先**：基线选择器是 `.panel-empty .panel-empty__icon` (0,2,0)，
   页面本地 scoped 是 `.panel-empty__icon[data-v-x]`（同 (0,2,0)，靠顺序取胜）。删本地规则后
   必须仍有 `.panel-empty` 祖先，否则基线不命中、图标掉回 TDesign 的 16px。
   - `views/business/erp/contract/index.vue`：line 142 `<div class="panel-empty" data-testid="contract-empty">`
     包着 line 143 `<t-icon class="panel-empty__icon" />` ✓
   - `views/business/hr/salary/detail.vue`：line 403 `<div class="panel-empty" data-testid="hr-salary-detail-empty">`
     包着 line 404 `<t-icon class="panel-empty__icon" />` ✓
   - 计算样式复核：两页 icon 前后 `font-size` 都是 `28px`、`color` 都是 `rgb(0,0,0,0.4)`（见 §4）。
2. **`.tool-row__spacer` 为空**：等价前提是基数为 0（`flex:1` 与 `flex:1 1 auto` 对空元素同解）。
   5 处全部是自闭合 `<span class="tool-row__spacer" />`：
   `budget/mapping:31`、`budget/report:88`、`receivable:167`、`report/report-item:35`、`year-end:31` ✓
   （若将来有人给它塞子节点，这条不成立 —— 那时几何会变，探针会当场抓到 GEOM。）

## 3. 静态等价断言（验收第 3 条）

`equivalence-static.mjs`：对 16 个改动文件里 87 个「改造前有本地声明」的类，逐属性比
「生效值 before（基线⊕本地）」vs「生效值 after（基线⊕残留本地）」。

```bash
cd frontend
# 改造前的树（干净 worktree = 本批 commit 的父提交）
git worktree add --detach /tmp/svpwt HEAD~1
ln -s "$PWD/node_modules" /tmp/svpwt/frontend/node_modules
( cd /tmp/svpwt/frontend && node scripts/shell-variant-report.cjs --json /tmp/before.json --quiet )
node scripts/shell-variant-report.cjs --json /tmp/after.json --quiet
node ../docs/evidence/2026-11-16-shell-b1/equivalence-static.mjs /tmp/before.json /tmp/after.json --allow-changed flex
```

结果：

```
比对 16 个「有壳类声明被删」的文件 / 87 个类
LOST=0  CHANGED=0  NEW=0   （允许的 CHANGED（flex）=5）
  budget/mapping.vue      tool-row__spacer  flex: "1" → "1 1 auto"
  budget/report.vue       tool-row__spacer  flex: "1" → "1 1 auto"
  receivable/index.vue    tool-row__spacer  flex: "1" → "1 1 auto"
  report/report-item.vue  tool-row__spacer  flex: "1" → "1 1 auto"
  year-end/index.vue      tool-row__spacer  flex: "1" → "1 1 auto"
```

**这 5 条是「值变化、几何零变化」**：`flex:1` 展开是 `1 1 0%`，`flex:1 1 auto` 是 `1 1 auto`，
基数为 0（空元素无内容宽度）—— 那 5 个元素的 `getBoundingClientRect()` 前后逐字段相同（§4）。
`var(--x, fallback)` 与 `var(--x)` 视为同一 token（与报告工具同口径），所以
`hr/salary/detail.vue` 的 `color: var(--td-text-color-placeholder, #0000004d)` 不算差异 ——
实测两边计算值都是 `rgba(0,0,0,0.4)`，也印证了该 token 在本项目里始终有定义。

## 4. 逐元素计算样式 + 几何比对（验收第 4 条）

`shell-probe.mjs`（静态 dist + **全量 API 打桩**，同一份桩跑前后两遍）+ `shell-probe-diff.mjs`：

| 项 | 值 |
|---|---|
| 路由 | 16 条（B1 批涉及的每一页，含 `/business/hr/salary/:id`）|
| 采集 | 每个带壳类的元素 40 个计算属性 + `getBoundingClientRect`，共 **127 个元素** |
| 对照组（同一 dist 连跑两遍）| 127 元素 / **几何 0 条 / 样式 0 条** → 探针无噪声，结论可用 |
| 前后 diff | **几何/结构差异 0 条**；样式值差异 **10 条** = 5×( `flex`、`flex-basis` )，全部落在 `.tool-row__spacer` |
| 其余 4 类（`panel-head__meta` / `__tip` / `tool-row__hint` / `panel-empty__icon`）| 计算样式**逐属性零差异**（含 icon 的 `font-size:28px`、`color:rgba(0,0,0,0.4)`）|

> 为什么打桩而不是连真后端：本机没有可用后端（dev/prod 的 admin 与 e2e 账号都不通用，登录返回
> `20001 用户名或密码错误`）。前后对比只需要「同样的输入」，打桩比真后端更确定：
> `/api/system/auth/me` 授 `*:*:*`、menus 给一棵最小树、其余接口一律空页。
> 打桩导致页面停在「空表」态，正好把**空态壳**（`.panel-empty*`）与**工具栏壳**（`.tool-row*`）都渲染出来。
> `receivable` 的 spacer 在「账龄看板」页签里，探针会先点开那个页签（TDesign tabs 懒渲染）。

复现：

```bash
cd frontend
# before：在 HEAD~1 的干净 worktree 里 build 一份 dist（或本批 commit 前的 dist 备份）
node ../docs/evidence/2026-11-16-shell-b1/shell-probe.mjs --dist <before-dist> --out /tmp/before.json
node ../docs/evidence/2026-11-16-shell-b1/shell-probe.mjs --dist dist          --out /tmp/after.json
node ../docs/evidence/2026-11-16-shell-b1/shell-probe-diff.mjs /tmp/before.json /tmp/after.json
# 对照组：同一份 dist 连跑两遍，diff 必须全 0
```

## 5. 门禁（本批真正的价值）

```bash
cd frontend
node scripts/shell-variant-report.cjs --baseline scripts/shell-variant-allowlist.json
# 门禁档：快照 253 条 / 当前 253 条
# ✓ 无新增页面壳类声明（存量 253 条待按批削减，已删 0 条可同步削 allowlist）
#   → 退出码 0
```

反证（双向，都实测过）：

| 造的场景 | 输出 | 退出码 |
|---|---|---|
| 往树里加一个 `.panel-head__meta` 页面声明 | `NEW views/business/erp/__tmp_gate_probe.vue  .panel-head__meta   [B1]` | **1** |
| 故意从快照里删掉 `ListPage` 的 `.panel-head` 条目 | `NEW components/ListPage/index.vue  .panel-head   [B0]` | **1** |
| 用**删之前**的 273 条快照比当前树 | 20 条 `FIXED ... [B1]`（正是本批删掉的那些） | **0** |
| 快照文件不存在 | 提示先 `--write` | 2 |

挂载点：`scripts/check.sh` 的 L0（`SHELL_VARIANT_GATE=block|warn|off`，默认 `warn`）。
`bash scripts/check.sh --fast` 实测：

```
==> 页面零壳类声明（frontend/scripts/shell-variant-report.cjs --baseline）
✓ 无新增页面壳类声明（存量 253 条待按批削减，已删 0 条可同步削 allowlist）
  ✓ PASS：无新增页面壳类声明（存量按 §5 的 B1–B6 逐批削减）
==> 全部检查通过（L0 L1）
```

## 6. 门禁与产物

| 检查 | 结果 |
|---|---|
| `pnpm typecheck` | 0 error |
| `pnpm build` | 通过 |
| `bash scripts/check.sh --fast`（L0+L1） | 全绿（含新增的 L0 壳类门禁） |
| `pnpm lint:shell` | 退出 0，`NEW` 0 条 |
| 16 个 `.vue` 的模板 + `<script>` | 与 `HEAD~1` **逐字节相同**（脚本断言，`git show HEAD~1:<f>` 前缀比对）；`<style>` 区只删不加（+0/−89） |
| dist 产物核对 | `.panel-empty .panel-empty__icon{font-size:28px;color:var(--td-text-color-placeholder)}`、`.tool-row__spacer{flex:1 1 auto}`、`.panel-head__meta,.panel-head__tip,.panel-head__hint{font-size:12px;color:var(--td-text-color-placeholder)}` 都在 |

## 7. 下一批（B2–B6）直接复用

```bash
cd frontend
# 1) 删完代码后先看块数
node scripts/shell-variant-report.cjs --batch B2
# 2) 静态等价（--allow-changed 列出本批允许的故意变化属性，没有就不传）
git worktree add --detach /tmp/wt HEAD~1 && ln -s "$PWD/node_modules" /tmp/wt/frontend/node_modules
( cd /tmp/wt/frontend && node scripts/shell-variant-report.cjs --json /tmp/before.json --quiet )
node scripts/shell-variant-report.cjs --json /tmp/after.json --quiet
node ../docs/evidence/2026-11-16-shell-b1/equivalence-static.mjs /tmp/before.json /tmp/after.json
# 3) 计算样式 + 几何（--routes 换成本批的路由清单，一行一路径）
node ../docs/evidence/2026-11-16-shell-b1/shell-probe.mjs --dist dist --out /tmp/after.json --routes /tmp/routes-b2.txt
# 4) 最后重写快照（条目数只许降）
node scripts/shell-variant-report.cjs --baseline scripts/shell-variant-allowlist.json --write
```

> 两个已知坑（本批踩过）：
> ① `@playwright/test` 只在 `frontend/` 下可解析 —— 探针必须在 `frontend/` 目录下跑；
> ② 懒渲染的 Tab 里的壳元素（如 `receivable` 的账龄看板）要先把页签点开，否则探针采不到，
>    会误报「该路由没有这个类」。
