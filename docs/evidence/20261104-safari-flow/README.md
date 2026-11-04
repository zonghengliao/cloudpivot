# SAFARI-FLOW：Safari 下流程编辑器节点塌陷（画布原点）修复证据

* 卡片：`t_b4adce9d`
* 日期：2026-11-04
* 结论一句话：**根因不是「节点坐标算错」，而是 WebKit 对「祖先 `<g transform>` + foreignObject 里的 HTML」
  只更新布局、不更新绘制** —— 卡片被画在「未变换」的位置上，节点 g 上再有 translate 时就是全部叠在画布原点
  （用户看到的「只能看到『结束』和一张被遮的卡片」）。修复 = 内层 `<g>` 不再承担节点/画布变换，
  节点坐标落在 foreignObject 的 `x/y`，画布平移缩放挪到 **svg 元素自己的 CSS transform**，
  并在卡片上盖一层透明 SVG 命中矩形（WebKit 里 HTML 卡片会盖住 SVG 锚点，导致拉不了线）。

## 1. 现象与根因（实测）

### 1.1 用户现象
Safari 打开流程编辑器：节点全挤在画布左上角重叠；连线和条件标签正常；左下缩略图正常。Chrome 正常。

### 1.2 关键实测：布局与绘制不一致
用 Playwright 的 WebKit（2370，Safari 27.2 UA）打开同一张图，`getBoundingClientRect` **全部正确**，
但截图上卡片全在原点 —— 也就是说「量出来的位置」和「画出来的位置」不是一回事：

```
改前（<g transform="translate(x,y)"> 包 foreignObject）· WebKit
  布局: s@(487,517) c_amt@(623,517) n_dept@(790,411) n_fin@(790,608) n_gm@(956,608) e@(1122,517)
  绘制: x[0,149] y[0,63] ← 6 张卡片全部画在画布原点附近（互相重叠）
```

（证据：`diag-webkit-before.json` 的 `paint.ratio ≈ 0`（每个节点矩形里几乎没有卡片像素）、
`shot-webkit-before.png`。）

### 1.3 三个变体的对照实验（`fo-mini2.html` + `variant-test.cjs`）
| 承载方式 | WebKit 静态 | WebKit 加载后再改变换 | Chromium |
|---|---|---|---|
| A `<g transform>` + foreignObject | ✗ 画在原点 | ✗ 不跟随 | ✓ |
| C CSS transform 挂外层 `<div>` | ✓ | ✓ | ✓ |
| D CSS transform 挂 `<svg>` 元素 | ✓ | ✓ | ✓ |

→ WebKit 只对「SVG 元素的 CSS transform」正确合成 foreignObject 里的 HTML；
对祖先 `<g>` 的 transform（无论静态还是运行时修改）都只做布局、不做绘制。
这解释了为什么「连线 / 标签 / 缩略图都对」（纯 SVG 图元跟随 `<g>` 变换正常）、只有卡片跑位。

## 2. 改了什么（共 3 处结构性改动 + 1 处守卫测试）

`frontend/src/views/business/flow/components/FlowCanvas.vue`

1. **节点坐标从 `<g transform>` 挪到 foreignObject 的 `x/y`**（卡片要求里的「方案 A」）：
   `<g class="flow-node">` 不再有 transform，`<foreignObject :x="node.x" :y="node.y">`。
2. **画布平移 / 缩放从 scene `<g transform>` 改成 svg 元素自己的 CSS transform**
   （`sceneStyle = { transform: translate(x px, y px) scale(s), transformOrigin: '0 0' }`）。
   这是真正修好 WebKit 的一步：整个 svg 作为一个普通盒子被合成，绘制位置与布局一致。
   `translate() scale()` + `origin 0 0` 与 `<g transform>` 数学等价，`screenToCanvas` / `fitView` /
   小地图的口径一行没改。配套 `.flow-canvas__svg { overflow: visible }`（缩小/平移到画布外时内容不能被自身盒子裁掉，
   裁剪仍由容器 `.flow-canvas { overflow: hidden }` 负责）。
3. **锚点 / 校验角标改成绝对坐标**（`cx = node.x`、`cx = node.x + size.w`、角标圆心 `(node.x+w-4, node.y+4)`），
   不再依赖 g 的 translate；角标也不再用嵌套 `<g transform>`。
4. **新增透明命中层 `.flow-node__hit`**（与卡片同矩形、同一个 `<g>`）+ `.flow-node__card { pointer-events: none }`：
   WebKit 里 foreignObject 的 HTML 会盖住同位置的 SVG 锚点 → 「从锚点拉线」会变成「拖节点」（实测）。
   命中交给 SVG rect 后，两引擎都能拉线、点选、拖拽。

守卫：`frontend/tests/unit/flow-canvas-style.test.ts` 新增 ⑦ / ⑦-b / ⑦-c / ⑧ / ⑨ 共 5 条源码契约断言，
防止有人把 `transform` 加回 `<g>`、或把命中层/`pointer-events` 去掉（这类缺陷 vue-tsc 与几何单测都看不见）。

## 3. 验收证据

### A. Chrome 不退化（最重要）
* **几何逐项对比**（`geometry-diff-chromium.txt`）：改前 / 改后 6 个节点的卡片、foreignObject、`<g>`、
  两个锚点的屏幕矩形**全部相等（差 ≤ 0.01px）**。
* **像素对比**（`pixel-diff-chromium.txt`）：1600×1000 里仅 **537 像素（0.034%）** 不同，且差异全部落在
  **文字字形**上（`diff-orig-new-chromium.png` 放大可见是「全部同意 / 差异金额 < 10000」等文字的抗锯齿），
  没有任何位置偏移 —— 这是「CSS 变换的 svg 层」与「SVG 内部变换」在 0.756 这种非整数缩放下光栅化方式的差别。
* **既有 e2e 用例**：`e2e/04-flow-editor.spec.ts`（拖 3 个节点进画布 → 拖锚点连 3 条线 → 配审批人 → 保存 →
  刷新还原 → 校验 → 发布）改前改后**都通过**（临时副本 `97-safari-flow-tmp.spec.ts`，因隔离库 admin 有未读会弹
  「你有 N 条待处理」挡住按钮，副本里只加了 `sessionStorage['login-unread-shown']=1` 这一行初始化；跑完已删）。
* **交互逐项**（`interaction-both-engines.txt`，chromium 全绿）：选中高亮 / 拖节点（位移 = 吸附后画布位移 × scale，
  fo 坐标同步）/ 滚轮缩放（卡片等比缩放）/ 拖空白平移 / 从出口锚点拉线（边数 +1）/ 校验角标位置（右上角内缩 4px）。

### B. Safari / WebKit
* **Playwright WebKit 上已把「塌陷」修掉**：
  * 改前：`diag-webkit-before.json` → `verdict = not-painted`，6 个节点 `paint.ratio = 0`（卡片不在自己的位置上）；
  * 改后：`diag-webkit-after.json` → `verdict = ok`，每个节点 `paint.ratio ≈ 93-96%`；
  * `paint-positions.txt`：删除 foreignObject 前后变化像素的条带 `y[410,459] / y[516,580] / y[607,655]`
    与布局 `y 410.5 / 516.9 / 607.7` 一一对应 → **画出来的位置 = 量出来的位置**。
  * `shot-webkit-before.png`（卡片糊在画布左上角）vs `shot-webkit-after.png`（6 张卡片各就各位）。
* **WebKit 上验到的交互**：滚轮缩放、拖空白平移、从出口锚点拉线、校验角标位置、点选节点都通过
  （见 `interaction-both-engines.txt` 的 webkit 段）。
* **⚠ 未在真机 Safari 验证**：本机没有 macOS / iOS，只有 Playwright 的 WebKit（自定义 Minibrowser 构建）。
  两者内核同源、这次的现象也完全对应，但**最终仍需真机 Safari 确认**（用户侧复验即可，见 §5）。
* **WebKit 的 3 个测试环境限制**（写在这里免得下次再踩）：
  1. Playwright WebKit 对 foreignObject 里的 HTML **命中测试不稳**（`elementFromPoint` 常常返回 `svg`），
     所以「点/拖卡片本体」这类用例在 WebKit 里要依赖我们新加的 `.flow-node__hit` rect；
  2. 给 foreignObject 设 `visibility: hidden` / `opacity: 0` **不生效**（像素变化 0），
     想让卡片"消失"必须真把节点从 DOM 删掉 —— `flow_canvas_diag.cjs` 的 paint 校验因此用「删除前后求差」；
  3. 在同一次点击之后立刻拖同一坐标，WebKit 会把这串动作吞掉（双击语义），单独一次拖拽正常
     （`fo 480→530`、`cardLeft 789.6→827.4`，与 Chromium 逐像素同值）。

## 4. 诊断脚本（新增，以后一条命令排查）

`scripts/verify/flow_canvas_diag.cjs`

```bash
node scripts/verify/flow_canvas_diag.cjs --browser chromium|webkit \
  --base-url http://127.0.0.1:5187 --def-id <流程定义id> --token-file token.txt \
  --out diag.json --shot diag.png
```

输出每个节点的：`<g>` 的 transform、foreignObject 的 x/y/w/h、卡片 computed 尺寸、
卡片 / foreignObject / `<g>` 的 `getBoundingClientRect`、锚点与角标位置，以及 **paint 校验**
（删掉 foreignObject 前后，卡片矩形内的变化像素占比）。
判定：`ok` / `collapsed`（几何重叠）/ `not-painted`（几何正常但没画在位置上）/ `no-nodes`，
退出码 0 / 1 / 2 可直接进 CI。

关键设计：**不能只看 `getBoundingClientRect`** —— WebKit 这类 bug 布局是对的、只有绘制错位，
所以脚本必须做像素级 paint 校验才能发现问题（这是本卡最大的方法论教训）。

## 5. 真机 Safari 复验清单（留给用户）

1. Safari 打开 `/flow/def/<任一草稿流程>/edit`：6 张卡片应在各自位置、与端口连线对齐；
2. 滚轮缩放、空白处拖拽平移后卡片仍与连线对齐；
3. 从卡片右边缘圆点拖出一条线到另一个节点（应出现「已连线」）；
4. 拖动节点松手后位置落到吸附网格上。

## 6. 复现环境

```bash
# 后端（隔离库副本，profile=test）
su postgres -c "createdb -T cloudpivot_test -O alun cloudpivot_sfari"
NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18097 NIK_DATABASE_NAME=cloudpivot_sfari \
  NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=<64 hex> CLOUDPIVOT_STORAGE_MASTER_KEY=<64 hex> \
  ./backend/target/debug/cloudpivot
# 前端（vite dev，绝不写生产 dist）
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18097 ./node_modules/.bin/vite --port 5187 --strictPort
```

流程夹具：`POST /api/business/flow/def` 建 `sfari_canvas`（6 节点 / 6 边，
开始 → 金额分级 →（部门主管审批 / 财务复核 → 总经理审批）→ 结束）。

## 7. 门禁

| 门禁 | 结果 |
|---|---|
| `vue-tsc --noEmit` | 通过（exit 0） |
| `tsx --test tests/unit/**/*.test.ts` | 606/606 通过（新增 5 条：⑦/⑦-b/⑦-c/⑧/⑨） |
| `node scripts/check-palettes.mjs` | 全部通过 ✓ |
| `vite build`（`--outDir` 指向 scratch，不碰生产 dist） | 成功；产物里可见 `class:"flow-node__hit"` 与 `transformOrigin` |
| `playwright test e2e/97-safari-flow-tmp.spec.ts`（= 04 的副本） | 改前 / 改后各 1 passed |
