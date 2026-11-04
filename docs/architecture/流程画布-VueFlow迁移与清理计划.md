# 流程画布：Vue Flow 迁移与老画布清理计划

> 状态：**默认画布已切换为 Vue Flow（VUE-FLOW-3，2026-11-04）｜老画布进入 deprecated，仅作回退**
> 涉及代码：`frontend/src/views/business/flow/components/`

## 1. 一句话结论

流程编辑器（`/flow/def/:id/edit`）的画布引擎**默认已是 Vue Flow**（`FlowCanvasVueFlow.vue`），
老的纯 SVG 自绘实现（`FlowCanvas.vue`）**保留但标记 `@deprecated`**，可随时用 URL 参数或构建期
环境变量切回。切换前做了 Chrome + WebKit(Safari 引擎) 双引擎全量回归、单测/e2e 回归、体积与加载时间对照。

## 2. 为什么要换

关键动因是 **Safari/WebKit 下的两个老画布缺陷**（Chromium 不复现，自动化用例一直是绿的）：

| 老画布问题 | 现象 | 根因 |
|---|---|---|
| 节点塌陷 | Safari 打开编辑器，卡片全糊在左上角（`getBoundingClientRect` 却全对） | WebKit 对「祖先 `<g transform>` + `foreignObject` 里的 HTML」只更新布局、不更新绘制（SAFARI-FLOW 卡） |
| 拖拽被吃 | 在画布上拖过一次后，后续拖拽全部无反应（连线也拉不出来） | `.flow-canvas` 根节点漏 `user-select:none`，WebKit 把后续拖拽当成「拖动选中文本」，`pointermove` 从 19 条掉到 2 条、`mouseup` 收不到（VUE-FLOW-2B 卡） |

Vue Flow 用 HTML 节点层 + SVG 连线层，天然绕开上面两类问题；且平移/缩放/连线/小地图是库能力，
不必继续维护自研的 pointer 状态机。

## 3. 引擎开关（唯一接入点）

`components/flow-canvas-flag.ts` + `components/FlowCanvasSwitch.vue`：

```
优先级（高 → 低）：
1. URL 参数   ?canvas=vueflow | ?canvas=native      # 单页即时生效，刷新即可
2. 构建期 env  VITE_FLOW_CANVAS=vueflow|native      # 一整个环境灰度 / 回退
3. 默认值     vueflow（VUE-FLOW-3 起）
```

- 调用方只认 `FlowCanvasSwitch.vue`（props / emits / `defineExpose` 与老画布逐字一致），
  不需要关心当前用哪一套。
- 新画布用 `defineAsyncComponent` 异步加载 → `@vue-flow/*` **只进编辑器路由的 chunk**，
  不进首屏（`vite build` 产物 `assets/FlowCanvasVueFlow-*.js` 独立成块，≈226 KB raw / 75 KB gzip）。

### 回退方法（至少保留一个版本）

| 场景 | 操作 | 生效方式 |
|---|---|---|
| 单个页面出问题 | URL 加 `?canvas=native` | 刷新即生效，不发版 |
| 整个环境出问题 | 构建/部署时 `VITE_FLOW_CANVAS=native` | 下一次构建生效 |
| 永久回退 | 把 `flow-canvas-flag.ts` 里 `resolveFlowCanvasEngine()` 末行 `'vueflow'` 改回 `'native'` | 发版 |

回退开关的可用性由 `tests/unit/flow-canvas-vueflow-flag.test.ts` ①③④ 钉住（改默认值时不会顺手把回退弄丢）。

## 4. 两套画布的契约（切换期间必须保持）

| 契约 | 说明 |
|---|---|
| props（13 个）/ emits（10 个）/ `defineExpose`（7 个） | 由 `tests/unit/flow-canvas-vueflow-contract.test.ts` 直接比对两个 SFC 源码的集合 |
| DOM 契约 | 一节点一元素带 `data-node-key` + `data-node-type`；出口锚点 `.flow-node__port--out`；连线命中层 `.flow-edge__hit`（e2e 与回归脚本都只用这些） |
| 锚点方向 | 左进（target/Left）右出（source/Right），否则连线几何整体变形 |
| 连线几何 | 两套画布共用 `graph.ts` 的 `edgePath` + `rounded-path.ts` 的 `roundedEdgePath` |

## 5. 切换时的验证（证据）

全部脚本与产物在 `docs/evidence/20261104-vueflow-3/`：

| 项 | 命令 | 结果 |
|---|---|---|
| 单测 | `cd frontend && pnpm test:unit` | 653/653 通过 |
| 类型 | `cd frontend && pnpm typecheck` | 0 error |
| e2e（默认画布） | `E2E_BASE_URL=… pnpm test:e2e e2e/04-flow-editor.spec.ts` | 1 passed（拖节点/连线/保存还原/校验/发布） |
| 全流程回归 Chrome | `node scripts/verify/vueflow3_full_regression.cjs --browser chromium` | 见 `regression-chromium.json` |
| 全流程回归 WebKit(Safari 引擎) | `node scripts/verify/vueflow3_full_regression.cjs --browser webkit` | 见 `regression-webkit.json` |
| 加载时间 | `node scripts/verify/vueflow3_load_bench.cjs --browser …` | 见 `bench-chromium.json` / `bench-webkit.json` |
| 首屏体积 | `cd frontend && vite build` 后比对入口 chunk | 见 `bundle.json`（入口 `index-*.js` 117 306 B，与切换前逐字节相同；`@vue-flow` 独立 chunk） |

## 6. 老画布清理计划（本卡只标记 deprecated，不删）

**为什么不在本卡删**：新画布切换后至少要观察一个版本的真实使用（有人回退过、有浏览器特有异常、
有业务方反馈），删除是不可逆动作，而保留成本只是一个不进首屏的 chunk + 一个文件。

执行顺序（每步都可独立回退）：

1. **本卡（VUE-FLOW-3）**：`FlowCanvas.vue` 顶部加 `@deprecated` 注释 + 本文档记录计划；默认值切 vueflow；回退开关保留。
2. **上线后一周**：查 nginx access log 里是否出现过 `?canvas=native`；查前端错误监控里是否有 Vue Flow 画布相关异常；
   两者都干净才进入第 3 步。
3. **删除**：
   - 删 `frontend/src/views/business/flow/components/FlowCanvas.vue`；
   - 删 `FlowCanvasSwitch.vue`（改为直接渲染 `FlowCanvasVueFlow.vue`），并把 `flow-canvas-flag.ts` 一并删掉；
   - 同步下线 `tests/unit/flow-canvas-vueflow-contract.test.ts`（它比对的是「两套实现等价」，只剩一套时无意义）
     与 `tests/unit/flow-canvas-vueflow-flag.test.ts` 的 ①–⑤（flag 已不存在）；
   - `graph.ts` / `rounded-path.ts` 保留（新画布仍在用，是几何唯一来源）；
   - `flow-components` 里其他组件（`NodeProps` / `TaskNodeConfig` / `ConditionTree` 等）**不动**。
4. **删除后回归**：`pnpm test:unit` + `pnpm typecheck` + `e2e/04-flow-editor.spec.ts` + 本目录的
   `vueflow3_full_regression.cjs`（把 `B13` 回退开关用例删掉）。

## 7. 已知问题（与本次切换无关，但值得单独开卡）

- **登录限流计数不衰减**：`backend/src/system/auth/handler.rs` 的 `check_login_rate()` 用
  `Cache::incr` + `if count == 1 { set_ex(…) }`，而本地 `profile=test`（无 Redis）走进程内缓存时
  `set_ex` 的 TTL 不生效 → 累计 10 次登录后该「账号+IP」**永久 429**，只能重启进程才恢复。
  本次验证期间被它堵过一次（重启 18150 验证实例后恢复，生产有 Redis 时按 `LOGIN_RATE_WINDOW_SECS` 正常过期）。
  建议：内存缓存实现补 TTL，或 `set_ex` 失败时降级为「按时间戳滑窗」。
