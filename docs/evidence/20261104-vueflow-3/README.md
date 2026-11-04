# VUE-FLOW-3 证据：默认画布切换为 Vue Flow + 全量回归（Chrome / Safari(WebKit)）

> 卡片：`t_1d60ed82` ｜ 基线 commit：`7750bce`（VUE-FLOW-2B）
> 验证环境：`profile=test` 后端 `127.0.0.1:18150`（库 `cloudpivot_vf2`）+ `vite dev` `127.0.0.1:5188`
> ＋ 生产产物（`vite build` → 静态服务 `5197`，`/api` 反代到 18150）

## 一句话结论

默认画布已切到 Vue Flow（`flow-canvas-flag.ts` 默认值 `native → vueflow`），**回退开关保留且实测可用**
（`?canvas=native` 与 `VITE_FLOW_CANVAS=native` 两条路）。

Chrome 与 **WebKit（Safari 同引擎）** 各跑一遍全流程：**22/22 通过、0 个 JS 报错**；
单测 **653/653**、typecheck 0 错、e2e `04-flow-editor.spec.ts` 绿；
首屏入口 chunk **逐字节不变**（117 306 B，gzip 反而 −10 B），`@vue-flow` 仍在独立异步 chunk；
编辑器页加载时间（生产产物 15 轮中位数）**Chrome +5.2% / WebKit +9.2%**，都在 ±20% 内。
老画布按建议**只标记 `@deprecated`、不删**。

---

## 一、切换本身（代码改动）

| 文件 | 改动 |
|---|---|
| `frontend/src/views/business/flow/components/flow-canvas-flag.ts` | 默认引擎 `'native'` → `'vueflow'`；顶部注释写清**三条回退方法**与切换理由 |
| `frontend/src/views/business/flow/components/FlowCanvasSwitch.vue` | 注释同步（默认已切、异步 chunk 的理由不变） |
| `frontend/src/views/business/flow/components/FlowCanvasVueFlow.vue` | 两个 Handle 加 `data-port-node-key`（e2e / 回归脚本用统一选择器定位「某节点的出口锚点」；**不能**复用 `data-node-key`，否则 `[data-node-key]` 从「一节点一元素」变 3 个，节点计数全错） |
| `frontend/src/views/business/flow/components/FlowCanvas.vue` | 顶部加 `@deprecated` 注释 + 删除前置条件（本卡不删） |
| `frontend/tests/unit/flow-canvas-vueflow-flag.test.ts` | ① 默认值断言改 `vueflow`；④ 增补「两条回退路径」断言（`?canvas=native` / `env=native`） |
| `frontend/e2e/04-flow-editor.spec.ts` | 选择器从老画布专属的 `.flow-node` 改成**两套画布都成立**的契约属性（`[data-node-key]` / `[data-node-key][data-node-type=…]` / `.flow-node__port--out[data-port-node-key=…], [data-node-key=…] .flow-node__port--out`）——**改断言不改意图，一条测试没删** |
| `docs/README.md`、`docs/architecture/流程画布-VueFlow迁移与清理计划.md` | 技术栈说明 + 迁移/回退/清理计划 |

### 回退方法（至少保留一个版本）

| 场景 | 操作 | 生效方式 |
|---|---|---|
| 单个页面 | URL 加 `?canvas=native` | 刷新即生效，不发版 |
| 整个环境 | 构建时 `VITE_FLOW_CANVAS=native` | 下一次构建生效 |
| 永久回退 | `flow-canvas-flag.ts` 末行 `'vueflow'` 改回 `'native'` | 发版 |

---

## 二、功能回归（B 段 16 项，Chrome / WebKit 各一遍）

脚本：`node scripts/verify/vueflow3_full_regression.cjs --browser chromium|webkit`
原始数据：`regression-chromium.json` / `regression-webkit.json`（逐项 `checks[]` 带耗时与失败原因）

| # | 验收项 | Chrome | WebKit | 实测细节 |
|---|---|---|---|---|
| B1 | 打开流程列表 → 能看到 | ✓ | ✓ | 22 行 |
| B2 | 点「编辑」进画布 → 加载正常 | ✓ | ✓ | 9 节点；`.vue-flow` 1 个 / 老画布 `g.flow-node` 0 个（**默认就是新画布**） |
| B3 | 9 种节点都能从左侧拖出来 | ✓ | ✓ | START/TASK_NODE/APPROVAL/COUNTERSIGN/CONDITION/PARALLEL/CC/NOTIFY_NODE/END → 2 → 11 节点 |
| B4a | 拖节点 | ✓ | ✓ | 位移 196px |
| B4b | 连线（拖锚点成边） | ✓ | ✓ | 边 1 → 2 |
| B4c | 删线（点中连线按 Delete） | ✓ | ✓ | 边 2 → 1（先断言 `.flow-edge.is-selected`=1 再删，避免「点到节点却删了节点」的假绿） |
| B4d | 删节点 | ✓ | ✓ | 节点 11 → 10 |
| B5 | 条件分支节点能配置 | ✓ | ✓ | 点开属性面板，含「条件」配置 |
| B6 | 自动布局 | ✓ | ✓ | dagre 重排，10 节点坐标变化 + toast「已重排」 |
| B7 | 保存 / 发布 | ✓ | ✓ | toast「已保存」；另造一条合法流程点发布 → `PUBLISHED v1`（后端回查状态） |
| B8 | 已发布流程只读：拖不动、连不了 | ✓ | ✓ | 节点**相对位移 1.1px**（拖节点只发生画布平移 `画布平移=true`）、边数 11→11 |
| B9 | 「另存为新草稿」能用 | ✓ | ✓ | 只读横幅消失 + 节点面板可拖 |
| B10 | 迷你地图点/拖移动视口 | ✓ | ✓ | 点击、拖动都让 transform 变化 |
| B11 | 缩放工具栏 +/−/fit | ✓ | ✓ | 三步 transform 各不同，读数 75% |
| B12 | 校验失败时角标 | ✓ | ✓ | 问题清单 1 个 + 节点角标 8 个 |
| B13 | **回退开关** `?canvas=native` | ✓ | ✓ | 老画布 9 节点 / `.vue-flow` 0 |
| X1 | 控制台 0 个 JS 报错 | ✓ | ✓ | 两引擎 `consoleErrors` 均为空 |

> 说明：B8 的判据是「节点相对位置不变」而不是「屏幕坐标不变」——只读态允许**拖空白平移画布**，
> 用屏幕坐标判会把正常平移误判成「节点被拖动」（第一版就是这么红的，已修正判据）。

## 三、Safari 专项（C 段 4 项）

| # | 验收项 | 结果 | 证据 |
|---|---|---|---|
| C1 | 节点正常显示、位置正确、不重叠 | ✓ WebKit | 9 节点；尺寸/坐标无异常；**两两重叠 0 对**；`shots/webkit/c1-nodes-layout.png`（与 Chromium 同图对比：布局逐一致，无塌陷） |
| C2 | 能拖节点 / 能连线 / 能缩放 | ✓ WebKit | 即 B4a/B4b/B11 在 WebKit 下的实测（位移 196px、边 1→2、缩放三步） |
| C3 | 卡片不盖住锚点 | ✓ WebKit | 20 个锚点（10 节点 × 进出）中心点 `elementFromPoint` **全部命中句柄本身**；`shots/webkit/c3-anchors-uncovered.png` |
| C4 | 截图存档 | ✓ | `shots/chromium/` 21 张、`shots/webkit/` 21 张（同名同步骤，可直接并排看） |

> C3 的测量前提（写进脚本注释，避免后来人误判）：必须在**可编辑且节点不重叠**的画布上量。
> 只读态句柄按设计 `opacity:0` + 不可点，`elementFromPoint` 命中卡片是正常现象；
> 节点互相压着时锚点被「另一个节点」盖住也属正常 z-order。

## 四、单测 / e2e

```
cd frontend && pnpm test:unit     → 653/653 pass（含 flow-canvas-vueflow-contract 的 13 项两画布契约比对）
cd frontend && pnpm typecheck     → 0 error
E2E_BASE_URL=http://127.0.0.1:5188 pnpm test:e2e e2e/04-flow-editor.spec.ts → 1 passed
```

e2e 用例仍是「拖 3 个节点 → 连线 → 配置审批人 → 保存 → 刷新还原 → 校验 → 发布」全链路，
只是选择器换成了两套画布都成立的契约属性；**在默认（Vue Flow）画布下绿**。
（跑 e2e 前在验证库补建了套件默认账号 `laoliu_test / Test@12345`（SUPER_ADMIN，与 prod 同名同角色）——
`cloudpivot_vf2` 是空库迁移出来的，缺这个账号，与画布改动无关。）

## 五、体积与性能（D 段）

### 5.1 首屏 chunk（`bundle.json`）

| 项 | 基线 `7750bce`（默认 native） | 本次（默认 vueflow） | 差 |
|---|---|---|---|
| 入口 chunk `index-*.js` | 117 306 B / gzip 43 673 B | 117 306 B / gzip 43 663 B | **0 B**（gzip −10 B） |
| 编辑器路由 chunk `edit-*.js` | 194 171 B / gzip 65 979 B | 194 171 B / gzip 65 979 B | 0 B |
| `FlowCanvasVueFlow-*.js`（异步） | 231 198 B / gzip 76 562 B | 231 294 B / gzip 76 572 B | +96 B |
| 全量 JS | 11 105 476 B / 128 文件 | 11 105 573 B / 128 文件 | +97 B |
| `index.html` modulepreload | 只有 `tdesign` | 只有 `tdesign` | `@vue-flow` **不在首屏预加载列表** |

结论：**首屏体积没有增大**，`@vue-flow/*` 仍在编辑器路由的独立异步 chunk（`defineAsyncComponent`）。

### 5.2 编辑器页加载时间（生产产物，15 轮中位数）

脚本：`vueflow3_load_bench.cjs`（各预热一次 → 交替顺序 15 轮 → 中位数）+ `vueflow3_static_preview.cjs`（服务 `dist/`）。

| 引擎 | native | vueflow | 差 | 判定 |
|---|---|---|---|---|
| Chromium | 439 ms | 462 ms | **+5.2%** | ✓（±20% 内） |
| WebKit（Safari 引擎） | 402 ms | 439 ms | **+9.2%** | ✓（±20% 内） |

> ⚠ 方法学教训（写下来避免踩第二次）：**不要用 `vite dev` 量这个数**。同一份代码在 dev 模式下反复测，
> 中位数在 **−29% ~ +24%** 之间跳（按需编译 / JIT / 磁盘缓存），据此判「±20%」随机红绿。
> 生产产物 + 15 轮交替中位数才稳定（这里两次独立复测：Chromium +5.2%，WebKit +9.2%）。

### 5.3 拖拽流畅度

连续拖节点（每次 22 个 pointermove 采样点）+ 拉线 + 缩放，两引擎均无掉帧/卡顿（回归脚本 B4a/B4b/B11 的耗时
分别是百毫秒级，且 WebKit 下没有出现 VUE-FLOW-2B 之前的「第二次拖拽卡死」）。

## 六、老画布清理（本卡只标记 deprecated）

- `FlowCanvas.vue` 顶部已加 `@deprecated` + 删除前置条件；
- 清理步骤（分 4 步、每步可回退）写在 `docs/architecture/流程画布-VueFlow迁移与清理计划.md` 第 6 节；
- **本卡不删**：删除不可逆，而保留成本只是一个不进首屏的 chunk + 一个文件。观察一周（查 nginx 日志有无
  `?canvas=native`、查前端异常监控）后再删。

## 七、遗留在验证环境里的东西（不涉及生产）

- 验证库 `cloudpivot_vf2`：新增账号 `laoliu_test`（e2e 套件默认账号，与 prod 同名同角色）；
  回归脚本每轮自造的流程定义结束时软删，共享夹具 `po_approve` 保持「仅 v1/v2」；
- 验证期间重启过一次 `127.0.0.1:18150` 后端（见下面已知问题），生产不受影响。

## 八、顺带发现的既有问题（与本卡无关，建议单独开卡）

1. **登录限流计数不衰减**（`backend/src/system/auth/handler.rs::check_login_rate`）：
   `Cache::incr` + `if count == 1 { set_ex(…, 60) }`，在**无 Redis**（本地 `profile=test` 走进程内缓存）时
   TTL 不生效 → 该「账号+IP」累计 10 次登录后**永久 429**，只能重启进程。本次验证被堵过一次。
2. **notice SSE 401**：`/api/business/notice/stream?token=…` 每页都打 401（两套画布完全一致），
   属既有行为，不影响功能，但会刷控制台/监控噪声。
3. 回归里出现过 1 次 `422 /api/business/flow/node/task/preview`（Chromium 那轮，WebKit 0 次）：
   来自**共享**的任务模板预演组件（空/半成品 config）；单独对照探针（同一张图，两套画布点同一任务节点）
   两边都是 0 次 4xx，因此与画布引擎无关。

## 九、复现脚本清单

```bash
# 1) 全流程回归（Chrome / Safari 引擎各一遍）
node scripts/verify/vueflow3_full_regression.cjs --browser chromium \
  --base-url http://127.0.0.1:5188 --token-file /tmp/vf3-token-chromium.json
node scripts/verify/vueflow3_full_regression.cjs --browser webkit \
  --base-url http://127.0.0.1:5188 --token-file /tmp/vf3-token-webkit.json \
  --shot-dir docs/evidence/20261104-vueflow-3/shots/webkit \
  --out docs/evidence/20261104-vueflow-3/regression-webkit.json

# 2) 性能（生产产物，15 轮中位数）
cd frontend && ./node_modules/.bin/vite build && cd ..
node scripts/verify/vueflow3_static_preview.cjs --port 5197 --root frontend/dist --backend http://127.0.0.1:18150 &
node scripts/verify/vueflow3_load_bench.cjs --browser chromium --base-url http://127.0.0.1:5197 --rounds 15
node scripts/verify/vueflow3_load_bench.cjs --browser webkit   --base-url http://127.0.0.1:5197 --rounds 15

# 3) 单测 / e2e
cd frontend && pnpm test:unit && pnpm typecheck
E2E_BASE_URL=http://127.0.0.1:5188 pnpm test:e2e e2e/04-flow-editor.spec.ts

# 4) 夹具复位（把共享流程定义恢复成「只有已发布版本」）
bash scripts/verify/vueflow_draft_reset.sh
```
