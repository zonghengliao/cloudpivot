# 批次D · P1-B1 验证证据（when 条件树编辑器）

> 云枢任务 TSK-202610-000687 ｜ 父任务 P1-B（TSK-000674）｜ 前置 P1-A2（`5756bec`）
> 验证时间：2026-10-30 16:0x–16:14 CST ｜ 基线：`c8c4e16`（main）

## 一、交付物

| 文件 | 说明 |
| --- | --- |
| `frontend/src/views/business/flow/components/condition-tree.ts`（新） | 纯逻辑层：JSON ↔ 规则树、字段白名单、深度/节点/args 上限、增删。**不 import Vue**，`node --test` 直接跑 |
| `frontend/src/views/business/flow/components/ConditionTree.vue`（新） | 编辑器根组件（`v-model` 就是后端 `edge.condition`，计数/上限/JSON 预览） |
| `frontend/src/views/business/flow/components/ConditionNodeRow.vue`（新） | 递归行组件（cmp 行 / and·or·not 分组行） |
| `frontend/src/views/business/flow/components/NodeProps.vue`（改 1 处） | 「条件表达式」`t-form-item` 内的 `t-textarea` → `ConditionTree`；删掉随之失效的 `conditionText` / `commitCondition`（其余未动） |
| `frontend/tests/unit/condition-tree.test.ts`（新） | 18 条单测（要求的 10 条 + 1 条深度补充 + 7 条附加） |

> 说明：本仓库**没有 vitest**，`pnpm test:unit` = `node --test "tests/unit/**/*.test.ts"`（Node 22 原生类型擦除，零新依赖），
> 与既有 `board-receipt.test.ts` 同一套约定。卡面写的 vitest 按仓库现状实现为 `node:test`。

## 二、与后端逐字兼容的依据

读的代码：`backend/src/business/flow/graph.rs`

- `FIXED_FIELDS`（10 项）逐字复制进 `condition-tree.ts::FIXED_FIELDS`
- `field_allowed`：`form.` + 首字符 `[A-Za-z_]`，其余 `[A-Za-z0-9_]`，后缀 ≤ 64 → 同规则实现为 `fieldError()`
- `parse_condition_inner`：`op` ∈ {and, or, not, cmp}；and/or 的 `args` 非空且 ≤ 16；not 的 `args` 恰好 1；
  cmp 的 `cmp` ∈ 11 个算子；`is_null`/`not_null` 不吃 `value`；`in`/`nin` 的 value 必须是数组
- 常量：`MAX_CONDITION_DEPTH = 8`、`MAX_CONDITION_NODES = 64`、`MAX_CONDITION_ARGS = 16`、
  `MAX_LITERAL_CHARS = 200`、`MAX_LITERAL_ARRAY = 50`

生成的键名固定为 `op` / `field` / `cmp` / `value` / `args`，键顺序 `op→field→cmp→value`、`op→args`。

## 三、门禁实测（全部在本机跑出）

```
$ pnpm typecheck      → vue-tsc --noEmit，退出码 0（0 错误）
$ pnpm build          → ✓ built in 35.42s，退出码 0
    产物含我们的组件：dist/assets/edit-DC3YYNSe.js（流程编辑器 chunk 内可见「添加分组 / 添加条件」文案）
$ pnpm check:palettes → 色阶算法断言 330 条失败 0 条；[check-palettes] 全部通过 ✓
    新增两个 .vue 里 **0 个裸 hex**（grep '#[0-9a-fA-F]{3,8}' 无命中），颜色全部走 --td-* token
$ node --test "tests/unit/**/*.test.ts"
    # tests 28   # pass 28   # fail 0      （condition-tree 18 条 + 既有 board-receipt 10 条）
```

单测逐条（`tests/unit/condition-tree.test.ts`）：

```
ok 11 - ① 单个 cmp 节点生成正确 JSON（键名/键顺序 = op field cmp value）
ok 12 - ② and 包两个条件 → args 长度 2，结构正确
ok 13 - ③ or 嵌套 and → 树形结构正确（or.args[0] 是 and，and.args 有 2 项）
ok 14 - ④ not 单条件 → args 长度 1（多给会被截断到 1 并记问题）
ok 15 - ⑤ JSON → 树 → JSON 逐字相等（往返一致）
ok 16 - ⑥ 深度到 8 层 → 不能再加（addBlock 给出原因，addChild 拒绝）
ok 17 - ⑥b 手工构造「第 8 层还是分组」→ 两个添加按钮都被深度挡住
ok 18 - ⑦ 节点到 64 → 不能再加；65 个节点被体检测出超限
ok 19 - ⑧ 字段不在白名单（foo.bar）→ 红框提示；空字段也提示
ok 20 - ⑨ 空规则树 → 生成 null（表示无条件）
ok 21 - ⑩ form.amount 前缀输入 → 通过校验，且生成 JSON 带完整字段名
ok 22 - 附加：右值输入框文本 → JSON 字面量（数字/布尔/null/数组）
ok 23 - 附加：is_null / not_null 不落 value 键（与后端求值形状一致）
ok 24 - 附加：in 的右值必须是数组（标量会被体检拦下）
ok 25 - 附加：单组 args 到 16 项封顶（后端 MAX_CONDITION_ARGS）
ok 26 - 附加：addChild 造出来的分组自带一个空 cmp（点开就能填，且节点数 +2）
ok 27 - 附加：非对象/未知算子反解不抛异常（老数据防御）
ok 28 - 附加：semanticKey 忽略键顺序（避免 watch 回灌冲掉正在编辑的输入）
```

## 四、浏览器实机验证（真 Vue + 真 TDesign，headless Chrome over CDP）

做法：临时挂一个只渲染 `ConditionTree` 的预览入口（`pnpm dev` :5173，**验完即删，不入库**），
用 CDP 真实点击/打字，读页面上的 `v-model` 输出。

| 场景 | 实测结果 |
| --- | --- |
| 后端 JSON 回填 `{and:[cmp(form.amount,gte,10000), cmp(instance.biz_type,eq,TASK)]}` | 渲染出「2 项 / 第 1/8 层」，三列输入框值 = `form.amount`、`≥ 大于等于`、`10000`；输出 JSON **与原 JSON 逐字相同** |
| 空态 + 「+ 添加分组」 | 节点头 `2/64`、深度 `2/8`，输出 `{"op":"and","args":[{"op":"cmp","field":"","cmp":"eq","value":""}]}` |
| 「+ 添加分组」再嵌套一层 | 深度徽标出现 `第 1/8 层`、`第 2/8 层`，节点头 `4/64`，输出里 `args[1]` 是 and 分组 |
| 字段下拉 input 敲 `form.amount` 并选 creatable 选项 | 输出 `field` 变为 `"form.amount"` |
| 右值 `10000` + 算子切「属于（多值）」+ 右值敲 `100, 200, vip` | 输出 `{"cmp":"in","value":[100,200,"vip"]}`（数字/字符串按字面量类型分开） |
| × 删节点 | 嵌套分组删掉后 `args` 从 2 → 1 |
| 注入非法字段 `foo.bar` | 行内红框 + 红字「字段「foo.bar」不在白名单内…」，容器边框转红（`ct--error`） |
| 注入 `null` | 空态文案「未设置条件 —— 这条连线无条件通过。」，输出 `null` |
| 注入 8 层深链 | 节点头 `节点 8/64`/`深度 8/8`，最深那一组「+ 添加分组」`disabled=true`、`title=深度已达上限 8 层`，并显示红字提示 |
| 注入单组 16 项 | 两个按钮都 `disabled=true`、`title=同一分组最多 16 个条件` |
| 注入 64 个节点 | 全部分组的两个按钮 `disabled=true`、`title=条件节点数已达上限 64 个` |
| 根算子切「非（not）」 | args 从 2 截到 1，**不渲染任何添加按钮**（后端 not 恰好 1 项） |

## 五、过程中修掉的一个真 bug（记录以免回退）

右值输入框原本把「文本 → 字面量 → 规范化文本」每敲一字回写成 `:value`：
打 `100,` 时逗号被规范化吃掉（变 `100`），接着打 `200` 就成 `100200`（实测输出 `["100200vip"]`）。
修法：`ConditionNodeRow.vue` 加 `valueDraft` 草稿 —— 编辑期显示用户原样输入，失焦后换规范化文本；
算子切换时清空草稿显示转换结果。

## 六、未做 / 明确不做

- 不改后端、不改 `NodeProps.vue` 的其它字段（diff 仅「条件表达式」一段 + import + 删除失效的 textarea 逻辑）
- 不引入新依赖（无 JSON schema 编辑器库；字段下拉用 TDesign `t-select` 的 `filterable + creatable`）
- **多规则 if/else 链**（卡面 mock 里的「+ 添加规则」）不在本卡范围：后端条件边/`when` 是**单棵** `Condition` 树，
  多规则属于 EXPRESSION 审批人的 `rules[]`，留给 P1-B2 及之后
- 未连真后端（本机 8023 无进程）：本卡是纯前端卡，"合法性唯一权威是后端 V11 校验" 的路径未变，
  组件只保证「生成的 JSON 形状合法」并提前拦住后端必拒的形状（深度/节点/args/白名单）
