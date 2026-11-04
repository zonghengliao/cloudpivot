# VUE-FLOW-2：Vue Flow 画布「业务贴合」补齐（一项不落）

* 卡片：`t_08732d96`（父卡 VUE-FLOW-1 `t_090c5611`）
* 日期：2026-11-04
* 结论一句话：**老画布 `FlowCanvas.vue` 的 11 项业务特性已全部落到 Vue Flow 画布上**，同一份图、
  同一视口（40,40 / 100%）下**整画布平均像素差 0.58/255、逐卡片最差 0.28/255、校验角标区域 0.00/255**
  （卡面要求 ≤3/255）；真机断言 **74/74 PASS**，Handle 专项 **13/13 PASS**；
  顺带修掉两个用户真机反馈的 bug（只读态连线方向退化、只读态提示文案误导），
  单元测试 644/644 全绿（新增 17 条），typecheck 0 错误。

---

## 0. 先看结论：11 项对照

| # | 卡面要求 | 落地情况 | 真机断言 |
|---|---|---|---|
| ① | 校验角标（红圆 `!`，`(w-4, 4)`，悬停给原因） | ✅ 卡片**兄弟节点**绘制（卡片 `overflow:hidden` 会裁掉）、16×16、右上角外扩 4px、`title` 提示 | `角标几何与老画布逐像素一致`、`角标区域平均像素差 0.00` |
| ② | 5 种运行状态（✓/脉冲/✕/灰淡化/默认灰） | ✅ 档位 / 文案 / 边框色 / 透明度逐节点一致；ACTIVE 保留 `flow-status-pulse` 脉冲环 | `5 种状态的角标档位与老画布一致`、`ACTIVE 保留脉冲动画` |
| ③ | 入口/出口锚点（可拖、hover 放大、只读不可拖） | ✅ Handle 左/右（r=5 / 描边 1.5 / 外径 11.5px）、hover `scale 1.25`；只读 `connectable=false` + `opacity:0` | `锚点落在卡片左/右边缘垂直中点`、`hover 放大（scale 1.25）` |
| ④ | 已走过的边（深色 + 加粗 + 换箭头） | ✅ `${from}>${to}` 命中 → 2.4px + 品牌色 + `vf-flow-arrow-taken` | `taken 边加粗（2.4px）`、`taken 边换箭头 marker` |
| ⑤ | 选中态（点节点/边/空白三种 emit） | ✅ 视觉由 props 驱动（不用 Vue Flow 内部 selection） | `点节点 → select-node`、`点连线 → select-edge`、`点空白 → 两个 null` |
| ⑥ | 只读态（不可拖 / 不可连 / 不可删） | ✅ `nodesDraggable` / `nodesConnectable` / `deleteKeyCode` + 键盘判断 | `拖节点不产生 move-node`、`从圆点拉不出线`、`Delete 不删除节点` |
| ⑦ | 从左侧面板拖拽创建 | ✅ 调用方 `def/edit.vue` 用 `canvasRef.screenToCanvas()` 换算落点（两套画布同一入口），拖入有幽灵预览 | `新节点落在松手位置（≤2px）`、`两套画布创建的节点画布坐标一致` |
| ⑧ | 条件标签（白底/1px 边框/3px 圆角/边中点/虚线区分） | ✅ 复用 `edgeLabelPoint` + `labelTextWidth`；兜底分支虚线 `5 3` | `标签文案一致`、`标签位置一致（≤0.6px）`、`虚线/实线区分` |
| ⑨ | 键盘（Delete / 空格平移 / Esc） | ✅ Delete 删除、空格+拖 = 平移、**Esc 取消选中 + 放弃拉线**（老画布没有，本卡新增） | `Delete 删除选中节点`、`空格平移`、`Esc 取消选中` |
| ⑩ | 底部提示（同文案同位置） | ✅ 编辑态逐字一致；**只读态改成只读说明**（见 §3.2） | `提示文案逐字一致`、`只读态不再出现「拖出连线」` |
| ⑪ | 副标题（11 种情况） | ✅ 共用实现 `flow-node-view.ts::nodeSubLabel`，15 种组合逐条断言 | `副标题 15 种情况文案逐条正确`、`与老画布逐字一致` |

---

## 1. 交付内容

| 文件 | 改动 |
|---|---|
| `frontend/src/views/business/flow/components/FlowCanvasVueFlow.vue` | 角标移到卡片外（兄弟节点）+ `title`；Handle 常驻（只读 `opacity:0`）；圆点改内嵌 SVG `circle r=5`；节点声明 `sourcePosition/targetPosition`；Esc 取消选中/放弃拉线；只读提示文案；`isEdgeTaken` 复用 |
| `frontend/src/views/business/flow/components/FlowCanvas.vue` | **仅 1 处**：只读态底部提示文案（用户真机反馈，见 §3.2；两套画布必须一致） |
| `frontend/src/views/business/flow/components/flow-node-view.ts` | 新增 `isEdgeTaken(pairs, from, to)`（把 `${from}>${to}` 口径变成可单测的函数） |
| `frontend/tests/unit/flow-canvas-vueflow-business.test.ts` | **新增 17 条**单测（①–⑪ + 两个真机 bug 的回归守卫） |
| `frontend/devtools/flow-vf2-compare/{compare.html,compare.ts}` | 视觉比对台：两套画布**同页并排**、同视口、同数据（截图可逐像素直接相减） |
| `scripts/verify/flow_vf2_business_verify.cjs` | 主验收脚本（74 条断言：逐项 + 像素差 + emit 契约 + 编辑器页拖拽创建） |
| `scripts/verify/vueflow_handle_diag.cjs` | Handle 专项（13 条：handle 数量 / `data-handlepos` / 边方向 / **删掉 Handle 后方向仍正确**） |
| `scripts/verify/vueflow_publish_fixture.sh` | 夹具辅助（发一条流程；仅诊断用） |
| `scripts/verify/vueflow_canvas_verify.cjs` | VUE-FLOW-1 的老脚本：只读断言从「不画句柄」更新为「句柄在 DOM 但 opacity:0 / 方向 left,right」 |

老画布除上述 1 行文案外**没有被改**（它仍是 `?canvas=native` 的对照基线）；调用方页面**一行未改**。

---

## 2. 怎么跑（可复现）

```bash
# ① 一台带新代码的前端 + 后端（本卡用的隔离实例：克隆库 + debug 二进制）
su postgres -c "createdb -T cloudpivot_test -O alun cloudpivot_vf2"
cd backend && NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18150 NIK_DATABASE_NAME=cloudpivot_vf2 \
  NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) NIK_ADMIN_PASSWORD=Admin@123456 \
  ./target/debug/cloudpivot &
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18150 ./node_modules/.bin/vite --port 5188 --strictPort &

# ② 造一张覆盖 9 种节点 / 10 条边的草稿流程
python3 scripts/verify/vueflow_fixture.py --base-url http://127.0.0.1:5188
#   → VUE_FLOW_DEF_ID=...

# ③ 主验收（74 条）+ Handle 专项（13 条）
node scripts/verify/flow_vf2_business_verify.cjs --base-url http://127.0.0.1:5188 \
  --def-id <DEF_ID> --shot-dir docs/evidence/20261104-vueflow-business/shots \
  --out docs/evidence/20261104-vueflow-business/report.json
node scripts/verify/vueflow_handle_diag.cjs --base-url http://127.0.0.1:5188 \
  --def-id <DEF_ID> --shot-dir docs/evidence/20261104-vueflow-business/shots \
  --out docs/evidence/20261104-vueflow-business/handle-diag.json

# ④ 视觉比对台（人眼看）：?case=plain|status|issue|readonly|sub
open http://127.0.0.1:5188/devtools/flow-vf2-compare/compare.html?case=status
```

---

## 3. 期间修掉的两个真机 bug（用户反馈）

### 3.1 只读态连线「底部出、顶部进」

* **现象**：已发布流程（`readonly=true`）里所有连线从节点**底部**出去、**顶部**进来；编辑态正常。
* **根因**（运维已定位，本卡确认）：`<Handle v-if="!readonly">` —— 只读态把 Handle 整个摘掉。
  Vue Flow 的 `getHandlePosition()` 优先用 Handle 的 `position`，**Handle 不存在时**才回落到节点上的
  `sourcePosition` / `targetPosition`，而这两个的默认值是 `Bottom` / `Top`（内置 `DefaultNode` 的默认值）。
* **修法（双保险）**：
  1. Handle **永远渲染**，只读态 `:connectable="!readonly"` + `.is-readonly { opacity: 0; pointer-events: none }`
     —— 视觉上与老画布「只读不画锚点」等效，但坐标还在（**绝不能用** `v-if` / `display:none` / `width:0`）；
  2. 节点数据上声明 `sourcePosition: Position.Right` / `targetPosition: Position.Left`，
     Handle 因任何原因缺失都不会退化。
* **证据**：`vueflow_handle_diag.cjs` 里专门有一条「把 DOM 里的 handle 全删掉 + 触发重测 →
  方向仍然正确（起点贴着右边缘、到"底边中点"的距离 52.3px）」。

### 3.2 只读态提示文案误导

* **现象**：用户看到「从右侧圆点拖出连线」，但只读态根本没有可拖的圆点。
* **修法**（两个画布都改，文案一致）：

```text
编辑态：拖节点移动 · 从右侧圆点拖出连线 · 滚轮缩放 · 空格/空白处拖拽平移 · Delete 删除
只读态：只读模式（已发布版本）· 如需修改请点顶部「另存为新草稿」
```

* **证据**：`⑩ 只读态底部提示改成只读说明（不再出现「拖出连线」）` + `两套画布文案逐字一致`。

---

## 4. 视觉对比（B · 关键验收）

比对台把两套画布挂在**同一页、同一视口（40,40 / 100%）、同一份数据**（不 fitView），
所以两张 `.flow-canvas` 的元素截图可以**直接逐像素相减**（没有 fitView 取整 / 重采样噪声）。

| 场景 | 整画布平均差 | 逐卡片最差 | >40 的像素占比 | 截图 |
|---|---|---|---|---|
| plain（9 种节点 / 10 条边） | **0.58/255** | 0.28（`e` 结束节点） | 0.21% | `shots/sbs-plain.png` |
| status（5 种状态 + taken 边 + 长标签） | **0.65/255** | 1.73（`n_task` 已驳回） | 0.28% | `shots/sbs-status.png` |
| issue（校验角标） | 角标区域 **0.00/255** | — | — | `shots/sbs-issue.png` |
| readonly（只读） | **0.58/255** | — | 0.22% | `shots/sbs-readonly.png` |
| sub（副标题 15 种） | — | 0.27（`c7` 开始） | — | `shots/sbs-sub.png` |

* 卡面要求 **≤3/255**，实测最好 0.00、最差 1.73。
* `shots/sbs-*.png` 是「上=老画布 / 下=Vue Flow」的并排图（同一张截图直接拼接，未做任何缩放）。
* 锚点圆点的 bbox 差值 **0px**（同圆心同直径）；连线方向 10 条全部与老画布同源（右出左进）。

### 已知差异（都是**不可消除**或**刻意**的，逐条说明）

1. **连线端点从 Handle 盒外边缘起算**：Vue Flow 的 `sourceX = nodeRight + 6.25`，老画布是 `nodeRight`。
   这 6.25px 正好被半径 5.75 的锚点圆点盖住 → 肉眼不可见、卡片区域像素差 0.28/255；
   但 16×16 的「圆环边缘」区域会把抗锯齿差异放大到 8/255（区域里 23 个环边缘像素差 40+）。
   所以验收用「圆点 bbox 一致 + 整图/逐卡片均值」而不是「圆环区域均值」。
2. **背景点阵**：`@vue-flow/background` 的 `size` 是**直径**（内部 `r=size/2`），老画布是 `r=1.15`；
   但老画布的网点色是「占位文字色 × 45%」，实测点阵最暗 239、背景 241（几乎不可见），
   传 1.15 与 2.3 的整图差是 0.11 / 0.54 → 保持 1.15。另：`offset` 传负数会被拼成
   `translate(--0.85)`（字符串拼接）导致整条 transform 失效，调相位只能用正数。
3. **`SKIPPED`「灰色 + 斜线」**：老画布源码里 `is-skipped` **只有 `opacity: 0.55`，没有斜线**
   （`WAITING` 也走同一个 class）。卡面对现状的描述与源码不符 —— 本卡照搬老画布（保证换库无差别）；
   若要斜线属新需求，需两套画布一起加。

---

## 5. 交互对照表（C · 老画布行为 → 新画布行为）

| 老画布行为 | 新画布行为 | 真机断言 |
|---|---|---|
| 点节点 → `select-node(key)`，卡片 `is-selected` | 同（`node-click` → emit；class 由 props 驱动） | ✅ 事件 + class 双断言 |
| 点连线 → `select-edge(key)` | 同（`edge-click`） | ✅ |
| 点空白 → `select-node(null)` + `select-edge(null)` | 同（`pane-click`；拖拽平移不算点击） | ✅ |
| 拖节点 → 松手吸附 10px 网格 → `move-node` | 同（`新坐标 = max(0, props 原坐标 + snapGrid(位移))`） | ✅（VUE-FLOW-1） |
| 从右侧圆点拖到节点/入口 → `connect({from,to})` | 同（Handle + `connect` 事件；自环/重复边仍由父组件判） | ✅ 边数 +1 |
| 选中后 Delete → `delete-selected` | 同（`deleteKeyCode=null`，只走自己的键盘逻辑） | ✅ |
| 方向键微调 → `nudge({key,dx,dy})` | 同 | ✅（源码同表） |
| 空格按住 + 拖 = 平移 | 同（`is-space-pan` 时节点让位给 pane） | ✅ 视口位移 60/30 |
| Esc —— **老画布没有** | 新增：取消选中（走与点空白同一出口）+ 放弃正在拉的线 | ✅ |
| 只读：拖不动 / 连不了 / 删不掉 / 无锚点 | 同（Handle 在 DOM 但 opacity:0 + pointer-events:none） | ✅ |
| 滚轮缩放以鼠标为锚点 / 拖空白平移 / 工具栏 / 小地图 | 同 | ✅（VUE-FLOW-1 36 条） |

`emit` 的参数与老画布**逐字一致**（契约测试 + 74 条真机断言双保险）。
卡面提到的 `emit('clear-selection')` 在**老画布不存在**（老画布就是发两个 `null`），故按老画布实现。

---

## 6. 代码质量（D）

| 项 | 结果 |
|---|---|
| `pnpm typecheck` | **0 错误** |
| `pnpm test:unit` | **644/644 通过**（新增 `flow-canvas-vueflow-business.test.ts` 17 条） |
| `pnpm check:palettes` | 330 条全通过（无裸 hex） |
| `vite build` | 成功；`@vue-flow` 仍只在懒加载 chunk `FlowCanvasVueFlow-*.js`（231 KB / gzip 74.7 KB），`index.html` **不引用、不 preload** |
| 共用函数无重复实现 | 副标题规则只有 `flow-node-view.ts::nodeSubLabel` 一份（新画布调用它），单测钉住「画布内不许再内联文案」 |

> 老画布 `FlowCanvas.vue` 里仍有一份**内联**的副标题实现 —— 那是刻意保留的对照基线（卡面红线：不改老画布）。
> 单测 `⑪ 副标题规则没有「两份实现漂移」` 会同时检查老画布与共用实现的文案字面量，任何一边少了就红。

---

## 7. 证据清单

| 文件 | 说明 |
|---|---|
| `report.json` | 主验收 74 条（含全部几何 / 像素差 / emit 证据） |
| `handle-diag.json` | Handle 专项 13 条（含「删掉 Handle 方向仍正确」） |
| `shots/sbs-plain.png` | 并排：9 种节点 / 10 条边 / 条件标签 / 锚点 |
| `shots/sbs-status.png` | 并排：5 种运行状态 + 走过边 + 长标签 |
| `shots/sbs-issue.png` | 并排：校验角标（两个命中节点） |
| `shots/sbs-readonly.png` | 并排：只读态（无锚点、提示改成只读说明） |
| `shots/sbs-sub.png` | 并排：副标题 15 种情况 |
| `shots/editor-{native,vueflow}-created.png` | 编辑器页：从左侧面板拖「审批」到画布后的落点 |
| `shots/diag-edit.png` / `shots/diag-no-handle.png` / `shots/diag-readonly.png` | Handle 专项截图 |
| `shots/diag-*.png` | 同上 |
