# P12 · 审批流画布视觉升级 + dagre 自动布局（升级前 / 升级后证据）

> 卡片：`t_5348f018`【P12】。老板拍板方案 B：**不换渲染引擎**（继续手写 SVG + foreignObject），
> 只做视觉升级 + 加 dagre 自动布局。
> 全部证据取自 **真浏览器**（Playwright + Chromium，无打桩），API 打生产后端 `172.18.10.144`。

## 一、升级前 / 升级后（同一条流程：`p12_canvas_demo`，11 节点 / 12 边）

| | 升级前（生产线上 dist） | 升级后（本次构建产物） |
|---|---|---|
| 截图 | `before-prod-canvas.png` | `after-canvas.png` |
| 背景 | 20px **线格**（`<path d="M 20 0 L 0 0 0 20">`） | 18px **点阵**（`<circle r="1.15">`）+ 顶部品牌光晕 |
| 节点 | SVG `rect`/`diamond`，`border-radius: 0` | `foreignObject` 卡片，`border-radius: 11px` |
| 类型标识 | 无 | 左侧 3.5px 类型色条 + 30px 图标底块（颜色全走主题变量） |
| 副标题 | 无 | 11.5px 次要色单行（会签模式 / 缺审批人 / 网关语义 · 运行状态） |
| 状态点 | 无 | 17px 圆点，四档（已通过 ✓ / 进行中脉冲 / 待处理 / 驳回 ✕） |
| 连线 | 正交硬折角，1px 灰 | 圆角折线（`Q` 二次贝塞尔），1.9px；走过的边品牌色 2.4px；指向进行中节点的边流动虚线 |
| 边标签 | 裸文字 | 白底 `rx=3` 小圆角标签（宽 = 文字宽 + 8） |
| 缩放 UI | 只有一个 `xx%` 文本 + 「适配」 | 右下角竖排：`+` / `−` / 适配 / 百分比（点击回 100%） |
| 小地图 | 无 | 左下角 150×96，节点色块 + 视野框，点击 / 拖动都能平移 |
| 自动布局 | 顶栏按钮，按拓扑序简单分层，**不可撤销** | 画布右上角按钮 → 二次确认 → dagre 树状布局 → fitView → 「撤销布局」 |

线上 DOM 实测（升级前，`before-prod-canvas.png` 同一次运行）：

```json
{ "patternWidth": "20", "patternPathD": "M 20 0 L 0 0 0 20", "patternCircles": 0,
  "htmlCards": 0, "typeBars": 0, "zoomBtns": 0, "minimap": 0, "layoutActions": 0,
  "firstRectRadius": "0px", "edgeWithQuad": 0, "nodeCount": 11, "edgeCount": 12 }
```

升级后同口径实测：

```json
{ "patternWidth": "18", "patternCircleR": "1.15", "patternLines": 0,
  "glow": "radial-gradient(60% 45% at 50% 0%, color-mix(brand 10%), transparent 70%)",
  "cards": 11, "cardRadius": "11px", "barWidth": "3.5px", "titleSize": "13px/600",
  "edges": 12, "edgesWithQuad": 12, "edgeStrokeWidth": "1.9px", "edgeLinecap": "round",
  "labels": 2, "labelRx": "3", "labelTextSize": "10.5px",
  "zoomBtn": "34×32", "minimap": "150×96", "miniNodes": 11, "miniViewStrokeWidth": "1.5px" }
```

## 二、dagre 自动布局（核心增值项）

- 纯算法层：`frontend/src/views/business/flow/auto-layout.ts`（`computeAutoLayout`），**只返回左上角坐标**，
  不碰任何节点数据；`graph.ts`（630 行坐标口径）**一行未动**。
- 方向：`LR`（本画布锚点固定**左进右出**，`TB` 会让边从右侧出来再绕回下一个节点左侧）。
  函数默认值仍是卡面约定的 `'TB'`。
- 交互：**不自动触发**；点按钮 → `DialogPlugin.confirm`（"这会按层级重排所有节点的位置（布局后可用「撤销布局」还原），是否继续？"）
  → 坐标变更包成一条 `layout` 命令压进撤销栈（Ctrl+Z 也能回退）→ `fitView()` → 画布右上角出现「撤销布局」。
- 撤销：布局前**整图**坐标快照（含未移动的节点），回写时同样压一条 `layout` 命令，所以撤销本身也可再撤销。

实测（11 节点）：

```
布局前: n_start(60,260) n_1(210,330) n_2(380,120) n_3(560,40) n_4(560,400) n_5(750,180)
        n_6(920,60) n_7(920,300) n_8(1100,170) n_9(1160,290) n_end(1290,190)
布局后: n_start(40,105) n_1(208,95) n_2(430,85) n_3(634,40) n_4(634,150) n_5(856,85)
        n_6(1060,150) n_7(1060,40) n_8(1282,95) n_9(1504,97) n_end(1716,105)
撤销后: 与布局前逐键相等（11/11 还原）
```

截图：`after-autolayout.png`（重排后）、`after-undo-layout.png`（撤销后）。

节点不重叠：布局前后各做一次「两两包围盒相交」检查，`overlap = []`；卡片内部
（色条 / 图标 / 标题 / 状态点）也不相交（`inner = []`）。

## 三、运行态（状态点 / 走过边 / 流动虚线）

编辑器页面不传 `nodeStatus`，这段只能在「进度图」场景出现 → 用临时冒烟页挂**真组件**取证
（`__smoke/flowcanvas.html`，验完已删，仓库无残留）。

`after-node-status.png` + 同一次运行的 DOM 实测：

```
状态点 4 个，均 17×17 / border-radius 50%：
  approved  rgb(43,164,113)   内容 ✓
  active    rgb(0,82,217)     无内容 + ::after 动画 flow-status-pulse-*
  rejected  rgb(213,73,65)    内容 ✕
  pending   rgba(0,0,0,.4)    无内容
连线：走过 2 条（品牌色 2.4px）、指向进行中 1 条（dasharray 7 7 + flow-dash 动画）、其余灰 1.9px
```

## 四、功能回归（红线：不破坏既有能力）

| 项 | 结果 |
|---|---|
| 拖节点 → 位置能改 → 保存落库 | ✅ 拖 60/40 屏幕像素（缩放 0.534 → 画布 +110/+80，按 10px 吸附），PUT 后 API 回读 `n_1: 210,330 → 320,400` |
| 撤销 + 再保存 → 坐标还原 | ✅ API 回读回到 `210,330`（演示流程数据未被污染） |
| 拉连线 → 新增 → 撤销 | ✅ `n_start → n_2`，边数 12 → 13 → 撤销回 12 |
| 选中节点 → 属性面板 | ✅ 「审批节点 / n_1 / 名称 / 审批人来源 / 会签模式 / 超时小时 …」全在 |
| 条件树 / then 选择器（P1-B1/B2/B3） | ✅ 选中连线出现「条件规则 节点 0/64 深度 0/8 …+ 添加条件」；截图 `after-edge-condition-tree.png` |
| 发布流程 | ✅ 新建临时流程 → 发布返回 `node_count=2 / edge_count=1`，与画布一致，随后软删清理 |
| 既有 e2e（`e2e/04-flow-editor.spec.ts`） | ✅ 真机通过（拖 3 节点 → 连线 → 保存 → 刷新还原 → 校验 → 发布，10.5s） |
| 小屏 1280×720 | ✅ 小地图 150×96 在左下 16px、缩放控件 36×130 在右下，互不遮挡、都在画布内 |

## 五、门禁

```
pnpm typecheck        ✅ 无输出（0 错）
pnpm build            ✅ built in 41.31s
pnpm test:unit        ✅ 221 tests / 221 pass / 0 fail（其中 P12 新增 21 条：
                         flow-auto-layout.test.ts 10 条 + flow-canvas-path.test.ts 11 条）
pnpm check:palettes   ✅ 色阶算法断言 330 条 / 失败 0，色板数据校验通过
```

## 六、与卡面规范的偏差（都是「变量优先 / 不越红线」导致的）

1. **阴影不写死 rgba**：卡面给的是 `0 1px 3px rgba(81,105,124,.10)…`，实现用 `var(--td-shadow-1/2)`
   （红线 6「不写死颜色，22 套配色零适配」）。同理顶部光晕用 `color-mix(in srgb, var(--td-brand-color) 10%, transparent)`，
   深色主题 `opacity: .5`（非 scoped 块，避免 `html[theme-mode]` 被 scoped 编译废掉）。
2. **点阵画在 SVG `<pattern>` 里**（画布本身是 SVG）：`<circle r="1.15">` + 18×18，
   `patternTransform` 跟随 `view`，平移/缩放时网点跟内容一起动；点的颜色是
   `color-mix(--td-text-color-placeholder 45%, transparent)`。
3. **开始 / 结束节点用紧凑排版**：这两种节点由 `graph.ts` 固定 96×44（**不许改**），
   44px 高放不下 30px 图标 + 两行文字 → 图标 22px、内边距 5px 10px、副标题隐藏。
   其余节点严格 11px 13px / 图标 30px / 标题 13px 600 / 副标题 11.5px。实测所有卡片内容零溢出。
4. **副标题拿不到「审批人姓名 + 时间」**：画布入参只有 `assignee_type` / `ref_id`，没有姓名与激活时间；
   没有伪造，改为「会签模式 / 未配置审批人 / 网关语义」+「运行状态」。
5. **自动布局按钮位置**：卡面写「画布工具栏」，实现放在画布右上角（`.flow-canvas__layout-actions`），
   顶栏原先那颗「自动布局」已移除（否则两个入口重复且顶栏那颗不可撤销）。既有 e2e 已同步（见下）。
6. **小地图在有校验问题时隐藏**：`edit.vue` 传 `:show-minimap="issues.length === 0"`，
   避免压住底部的问题列表；平时（无校验错误）常显。
7. **缩放百分比实测 120%/100% 与画布 `scale` 逐帧一致**；控件外框 36px = 34px + 2×1px 描边，
   右/下边距实测 17px（容器 1px 描边，卡面写 16px）。

## 七、改动的文件

```
frontend/src/views/business/flow/components/FlowCanvas.vue   # 视觉升级 + 缩放控件 + 小地图 + 圆角连线 + 类型色条/状态点
frontend/src/views/business/flow/components/rounded-path.ts  # 新增：正交折线 → 圆角折线（buildRoundedPath）+ 标签宽度估算
frontend/src/views/business/flow/auto-layout.ts              # 新增：dagre 自动布局（纯函数，只返回坐标）
frontend/src/views/business/flow/def/edit.vue                # 自动布局入口 + 二次确认 + 撤销布局 + fitView
frontend/tests/unit/flow-auto-layout.test.ts                 # 新增 10 条（单链/分支/菱形/孤立/空/自环/成环/异尺寸/LR/取整）
frontend/tests/unit/flow-canvas-path.test.ts                 # 新增 11 条（直线/拐角/Q 数/短段半径/同点/真实折线/标签宽）
frontend/package.json + pnpm-lock.yaml                       # dagre@^0.8.5 + @types/dagre@^0.7.54
frontend/e2e/04-flow-editor.spec.ts                          # 跟随按钮位置/二次确认/toast 文案调整（既有 e2e，不是新用例）
docs/flow-canvas-before-after/*                              # 本文与 7 张截图
```

**未动**：`graph.ts`、`condition-tree.ts`、`then-target.ts`、`rule-validation.ts`、`flow/task/index.vue`
（`git status` 里这几个文件 0 命中）。
