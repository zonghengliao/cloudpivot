# POC-FLOW · 三个画布方案对比（自研 / Vue Flow / LogicFlow）

同一份流程数据（`shared-data.js`）、同一套视觉规格（`shared-style.css`），三个方案各画一遍。
目的：让「要不要换画布库」这个决定有可看的依据，而不是靠感觉。

入口：<http://172.18.10.144/demo/flow-poc/>

| 页面 | 说明 |
|---|---|
| `index.html` | 入口：三栏并排 / 单栏切换 + 对比表 + 结论 |
| `demo-native.html` | ① 现有自研 SVG 画布（从 `FlowCanvas.vue` 抽出的最小可运行版，纯 HTML+JS） |
| `demo-vue-flow.html` | ② Vue Flow（Vue 3 + `@vue-flow/core`） |
| `demo-logicflow.html` | ③ LogicFlow（`@logicflow/core`，纯 SVG 自定义节点） |
| `shared-data.js` | 唯一数据源 + 贝塞尔/标签排版的共享函数 |
| `shared-style.css` | 统一视觉规格（卡片 150×64 / 圆角 11 / 色条 3.5 / 图标 30×30 圆角 9） |

三个 demo 的节点坐标、连线路径、标签位置都来自**同一批函数**（`bezierPath` / `labelLayout`），
所以「几何一致」是构造出来的，不是对着调的。

---

## 一、实测数据（都是量出来的）

### 运行体积（`curl` 下载后 `wc -c`，gzip = `gzip -9`）

| 方案 | 运行时代码 | 字节 | gzip |
|---|---|---|---|
| ① 自研 | `demo-native.html`（含内联脚本，零依赖） | 12,774 B = 12.5 KiB | 4.7 KiB |
| ② Vue Flow | `vue-flow.bundle.js` 290,655 + `vue-flow.css` 8,039 | 298,694 B = **291.7 KiB** | 101.4 KiB |
| ③ LogicFlow | `logicflow.bundle.js` 393,597 + `logicflow.css` 2,833 | 396,430 B = **387.1 KiB** | 99.3 KiB |

> 三栏共用的 `shared-data.js`（8.1 KiB）+ `shared-style.css`（7.9 KiB）是同一份，不计入单栏对比。
> ②的 bundle 里含 **Vue 3.5.44 运行时**（不打包 Vue 就没法跑，这样才是公平的「整包」对比）；
> ③的 bundle 里含 preact（LogicFlow 的渲染层依赖）。

### 依赖

| 方案 | 第三方包 | 版本 |
|---|---|---|
| ① 自研 | **0** | —— （坐标来自流程数据；若要自动布局才需要 dagre） |
| ② Vue Flow | 3 | `@vue-flow/core` 1.48.2 + `@vue-flow/background` 1.3.2 + `@vue-flow/controls` 1.1.3（另需 `vue`） |
| ③ LogicFlow | 1 | `@logicflow/core` 2.2.5 |

### 视觉一致度（像素级）

Chromium 1280×760，同一 fit 算法，只比画布主体区域（避开信息栏 / 控件 / 说明卡）：

| 对比 | 平均像素差 | 差异 > 40 的像素 |
|---|---|---|
| 自研 vs Vue Flow | **0.58 / 255** | 0.28% |
| 自研 vs LogicFlow | **2.15 / 255** | 1.77% |

LogicFlow 多出来的差异来自「纯 SVG 画字」：文字基线/字重与 HTML 排版有 1px 级偏差，
阴影用 `feDropShadow` 等价 `box-shadow`，边缘过渡略有不同。

---

## 二、三个方案到底差在哪（实测结论，含反常识发现）

### 1）Vue Flow 的节点**不是** foreignObject

很多文章说「Vue Flow 也是 foreignObject」，实测**不是**：它的节点是普通的
`<div class="vue-flow__node">`，靠 `transform: translate(x,y)` 定位，外面另有一个 `<svg>` 只画连线。

```
document.querySelector('.vue-flow__node foreignObject')  →  null
```

这条结论很重要：
- 「Safari 下 foreignObject 塌陷」这个坑，**Vue Flow 天然不存在**；
- 但它也意味着**不能**直接复用我们现有的 `foreignObject + SVG` 画布，必须重写成 Vue 组件树。

### 2）Safari/WebKit 塌陷的根因复盘（本仓库旧 bug）

自研画布的老写法是 `<g transform="translate/scale">` 包住 `foreignObject` 里的 HTML。
WebKit 对这种组合只更新布局、不更新绘制：`getBoundingClientRect()` 全部正确，但卡片被画在
**未变换**的位置。修法是把平移缩放改成 **`<svg>` 元素自己的 CSS transform**（`transform-origin: 0 0`），
祖先 `<g>` 一律不加 transform。

`demo-native.html` 右上角有一个开关「旧写法对照：`<g transform>`」，可以现场复现/对照：

| 引擎 | 现写法（svg CSS transform） | 旧写法（`<g transform>`） |
|---|---|---|
| Chromium 141 | ✅ 卡片与连线对齐 | ✅ 也正常（坑只在 WebKit） |
| **WebKit（Safari 内核）** | ✅ 卡片与连线对齐 | ❌ **卡片整体错位**（布局正确、绘制错位，连线还在原地） |

复现命令见 `docs/evidence/20261104-poc-flow/`（`shots-local/webkit-native-legacy.png`）。

### 3）LogicFlow 的两个真实成本

- **core 不带滚轮缩放**：`@logicflow/core` 的 wheel 语义是「滚动画布」，缩放要靠
  `@logicflow/extension` 的 `MouseWheelZoom` 插件。本 demo 只用 core，所以自己接了
  `wheel → zoom`（约 20 行）。要上生产得把这条算进工作量。
- **纯 SVG 画不出 HTML 的排版能力**：省略号、多行折行、`box-shadow` 都得自己实现
  （本 demo 用 `feDropShadow` + 手写折行近似，观感接近但不是同一套排版引擎）。

### 4）Vue Flow 的一个坑（本 demo 踩过）

节点如果不给 `Handle`，Vue Flow 一律兜底 **bottom → top** 锚点（它只看 `handleBounds`，
不看 `node.sourcePosition`），连线会从节点底部出去、从顶部进来，和现有画布的左右连线完全对不上。
本 demo 的做法是：**连线端点不用它给的 `sourceX/Y`，直接从节点位置算左右锚点** —— 几何与另外两个完全一致。

---

## 三、三个 demo 都能操作（实测）

Chromium + WebKit 双引擎自动化验证（`docs/evidence/20261104-poc-flow/shots-local/interact.json`）：

| 方案 | 拖节点 | 滚轮缩放 | 空白处平移 | 控制台报错 |
|---|---|---|---|---|
| ① 自研 | ✅ 1 个节点移动 ~108px | ✅ 1.00 → 1.82 | ✅ -90px | 0 |
| ② Vue Flow | ✅ 1 个节点移动 ~97px | ✅ 1.00 → 1.74 | ✅ -90px | 0 |
| ③ LogicFlow | ✅ 1 个节点移动 ~97px | ✅ 1.00 → 1.82 | ✅ -90px | 0 |

（Vue Flow / LogicFlow 有 3~5px 的拖拽阈值或吸附，位移略小于鼠标位移，属正常行为。）

---

## 四、怎么做的（技术要点）

```
shared-data.js      流程数据 + NODE 规格 + nodeRect/anchor + bezierPath(起止点→d)
                    + labelLayout(起止点→标签盒/基线) + fitViewport(统一 fit 算法)
shared-style.css    云枢视觉规格（前缀段是卡片给定的值，一个字没改）
```

- **连线**：三个库都用 `bezierPath(sx,sy,tx,ty)` 生成同一个 `d`，标签用 `labelLayout` 算同一个盒子。
- **箭头**：一个隐藏 `<svg>` 里的 `<marker id="poc-arrow">`，三个库都 `marker-end` 引用它
  （Chromium 与 WebKit 都实测可跨 `<svg>` 引用；LogicFlow 用自己 edge 内的 marker，形状同一条 path）。
- **文字量宽**：不用 `canvas.measureText`（字体回落与实际 SVG 渲染差约 20%，标签框会包不住字），
  改用离屏 `<text>` 的 `getBBox()`。
- **库怎么来的**：`@vue-flow/*` 与 `@logicflow/core` **没有**进 `package.json`。
  POC 需要「不联网也能开、体积可实测」，所以用 esbuild 打成单文件 IIFE 放 `vendor/`：
  ```bash
  esbuild vf-entry.js --bundle --format=iife --global-name=VFKit --minify --target=es2020 --outfile=vendor/vue-flow.bundle.js
  esbuild lf-entry.js --bundle --format=iife --global-name=LFKit --minify --target=es2020 --outfile=vendor/logicflow.bundle.js
  ```
  想换成 CDN（esm.sh）只需把 `<script src="./vendor/...">` 换成
  `https://esm.sh/@vue-flow/core@1.48.2` 等，页面代码不用改（注：国内网络下 esm.sh 不稳定，所以本地托管）。

---

## 五、结论（给决策用）

1. **只为修 Safari，不必换库。** 修法已在自研画布上验证通过，且三个库「照着我们的视觉做」
   都能做到像素级接近（0.58~2.15/255）—— 说明自研的渲染能力不是短板。
2. **换库买到的是交互与生态**（框选、对齐、连接线、撤销、Vue 原生集成），
   代价是 +291.7 KiB（Vue Flow）或 +387.1 KiB（LogicFlow）+ 一次画布重写 + 现有 15 条画布测试全改。
3. **如果将来要做「用户可编排」的流程设计器**（而不只是看流程图），Vue Flow 更划算：
   Vue 3 原生、节点可用 Vue 组件（业务信息直接渲染）、社区活跃。
   LogicFlow 的优势只有「纯 SVG 渲染确定性」，而我们现在的坑已经修完了，这个优势对我们不值 387 KiB。

---

## 六、已知限制

- 三个 demo 的**点阵网格**实现不同（自研 = SVG pattern；Vue Flow = Background 组件；LogicFlow = 自带 grid），
  同色同间距但相位略有差异，属各库自带行为，不影响节点/连线对比。
- LogicFlow 的文本排版与 HTML 有 1px 级差异（见上表 2.15/255）。
- WebKit 验证用的是 Playwright 1.64.1 的 `webkit-2370`（Safari 内核），**不是**真机 Safari；
  真机 Safari 上的最终确认仍需人工看一眼（尤其 `backdrop-filter` 的说明卡）。
