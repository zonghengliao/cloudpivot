# VUE-FLOW-2B 证据：编辑态「拖不出连线」

> 卡片：`t_1ef614bd` ｜ 基线 commit：`d735922`（VUE-FLOW-2 交付）｜ 改动文件：`FlowCanvasVueFlow.vue`、`graph.ts`

## 一句话结论

用户真机上的「看到圆点、拖不出连线」不是**一个** bug，而是三个各自独立、都能复现的缺陷叠在一起：

| # | 缺陷 | 引擎 | 现象 | 证据 |
|---|------|------|------|------|
| ① | 画布根节点漏了 `user-select: none`（老画布把这条写在内部 `<svg>` 上） | **WebKit/Safari 专属** | 第一次在画布上拖过之后，浏览器把后续拖拽当成「拖动选中的文本」吃掉：指针事件从 19 条掉到 2 条、`mouseup` 收不到 → 连线状态卡死（`.vue-flow__connection` 残留、`is-connecting` 不清）→ 之后**任何**拖拽都没反应 | `consecutive-drags-webkit-before.json` / `-after.json` |
| ② | 落点判定口径不一致：高亮按「整张卡片」（老画布口径），成边只认「目标句柄 20 画布 px 内」（Vue Flow 的 `connectionRadius`） | 全引擎 | 线拖到目标卡片上 → 卡片亮起虚线「可以落点」→ 松手什么都不发生（Vue Flow 静默 return，无任何提示） | `drop-tolerance-before.json` C/D 段 |
| ③ | 句柄命中区 = 视觉圆点本身（11.5 CSS px；fitView 到 53% 时屏上只有 ~6px） | 全引擎 | 真鼠标点偏 3px 就落到卡片上 → 变成「拖节点」，连预览线都不出现 | `drop-tolerance-before.json` A 段（命中半径 3px） |

②③ 是「拖了没反应」，①是「拖都不让拖」。三条一起修，用户的动作才真正可用。

## 改了什么

1. **`graph.ts` 新增 `resolveConnectDrop(nodes, point, fromKey)`**（纯函数，口径与老画布 `findDropTarget` 逐条一致：先命中别的卡片 → 再判自环 → 最后吸附 40px 内最近节点的入口锚点）。
2. **`FlowCanvasVueFlow.vue`**
   - `onConnectEnd(event)`：Vue Flow 没认这条线（`connectedInGesture === false`）且没被 Esc 取消时，用 `resolveConnectDrop` 再判一次，命中就 `emit('connect')` —— 父组件原有的「自环 / 重复边」拦截照旧生效。
   - `.flow-canvas` 补 `user-select: none; -webkit-user-select: none; -webkit-user-drag: none; touch-action: none;`
   - 句柄加 `::before { inset: -7px }`：视觉圆点 11.5px 不动，命中区放大到 25.5 CSS px。**刻意不用 padding / width**——`getHandleBounds` 读元素 rect，盒子一变连线端点就会跟着漂；伪元素不参与 rect。

## 量化对比（同一台机、同一流程、同一视口）

```
句柄命中区（从圆点中心向右还能点到 handle 的最远屏幕 px）   改前 3px  →  改后 6px
松手在目标卡片中心                                       改前 ✗ 无反应 → 改后 ✓ 成边(12→13)
松手在目标句柄右 14 / 20px（仍在卡片上）                  改前 ✗ → 改后 ✓
老画布同样动作（对照基线）                               改前 ✓ → 改后 ✓（未改老画布）
WebKit 连续两次拉线：第 2 次 pointermove 条数             改前 2（并卡死）→ 改后 19（正常成边）
```

## 复现/验证脚本

```bash
# 把测试流程重置回「只有已发布版本」（会删掉 version_no>2 的草稿）
bash scripts/verify/vueflow_draft_reset.sh

# ① 句柄命中区 + 落点容差（含老画布对照）
node scripts/verify/vueflow_drop_tolerance_diag.cjs --def-id 20000000-0000-4000-8000-0000000a0182 --tag after
node scripts/verify/vueflow_drop_tolerance_diag.cjs --def-id 20000000-0000-4000-8000-0000000a0182 --browser webkit --tag webkit-after

# ② WebKit 连续两次拖拽（第 2 次会不会卡死）
node scripts/verify/vueflow_two_drags_probe.cjs webkit docs/evidence/20261104-vueflow-2b/consecutive-drags-webkit-after.json

# ③ 用户原路径：已发布 → 点「另存为新草稿」→ 从圆点拖到目标节点
node scripts/verify/vueflow_draft_connect_diag.cjs --def-id 20000000-0000-4000-8000-0000000a0182

# ④ 回归：锚点方向 13 条 + 业务贴合 74 条（含两画布并排逐像素比）
node scripts/verify/vueflow_handle_diag.cjs --def-id 20000000-0000-4000-8000-0000000a0182
node scripts/verify/flow_vf2_business_verify.cjs --def-id 20000000-0000-4000-8000-0000000a0182
```

## 文件清单

- `drop-tolerance-before.json` / `drop-tolerance-after.json`：Chromium，改前/改后（A 段命中扫描 + B/C/D 落点容差 + E 老画布对照）
- `drop-tolerance-webkit-after.json`：WebKit，改后
- `consecutive-drags-webkit-before.json` / `-after.json`：WebKit 连续两次拉线（**① 的直接证据**；before 那份里 `pointermove` 调试计数为 0 是因为该调试钩子随代码一起被回滚了，看 `highlighted` / `connectionAfterUp` / `edges` 三列即可）
- `draft-connect-after.json`：用户原路径（另存草稿后拖拽）的完整 store 快照
- `handle-diag.json`：锚点方向 13 条（改后仍全 PASS，证明没破坏 VUE-FLOW-2 的锚点修复）
- `business-regression.json`：VUE-FLOW-2 的 74 条（含像素比）改后全 PASS
- `shots/`：`drag-preview-{before,after}.png`（拖拽中预览线）、`drop-on-card-hover-{before,after}.png`（卡片高亮时松手）、`draft-after-drag.png`（用户路径成边）、`diag-*.png`（锚点）、`plain/issue/readonly-*`（并排比对）

## 注意

- 验证期间重启过一次本地验证后端（`127.0.0.1:18150`，profile=test / db `cloudpivot_vf2`）：登录限流把 admin 锁在 429 里且窗口不自动过期，重启后才拿到 token。生产不受影响（限流 key 是「用户名+IP」，60 秒窗口）。
- `vueflow_draft_reset.sh` 会**删掉该流程 version_no>2 的草稿**，只对验证库 `cloudpivot_vf2` 的这个夹具定义生效。
