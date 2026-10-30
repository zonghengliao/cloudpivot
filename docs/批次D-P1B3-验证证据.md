# 批次 D · P1-B3 验证证据：实时校验 + 规则预览 + 错误码对齐

> 云枢任务 TSK-202610-000689 ｜ 前端开发（frontend-developer）
> 基线 `609a366`（T8-B）｜ 环境 172.18.10.99，本机 dev 5173/5188 + 本机 build 的 dist（**未碰生产**）
> 时间 2026-10-30 18:00 CST

## 一、交付内容

| 文件 | 行数 | 作用 |
|---|---|---|
| `frontend/src/views/business/flow/components/flow-error-codes.ts` | 新 | 65016 / 65033 的码值、上限、**文案模板**（逐字复制自 backend `.rs`）+ 与 `format!` 同构的 `fill()` |
| `frontend/src/views/business/flow/components/rule-validation.ts` | 新 | 实时校验三条闸（深度 8 / 节点 64 / 规则 32）+ 字段白名单 + then 目标，出 `{issues, counts, ok, codes}` |
| `frontend/src/views/business/flow/components/rule-preview.ts` | 新 | 人话翻译（括号优先级 / 空树 / 空字段提醒）+ 目标节点 → then 目标 |
| `frontend/src/views/business/flow/components/RulePreview.vue` | 新 | 预览面板：句子 + `n/limit` 计数 + ⚠ 提示 + 带码的问题列表 |
| `frontend/src/views/business/flow/components/NodeProps.vue` | 改 | 连线分支：条件树 + 预览整块加红；新增 `targetNode` 入参 |
| `frontend/src/views/business/flow/def/edit.vue` | 改 | 由 `edge.to` 反查目标节点传给属性面板（THEN 段数据源） |
| `frontend/tests/unit/rule-validation.test.ts` | 新 | 19 条：边界两侧 + 读后端 rust 源码断言对齐 |
| `frontend/tests/unit/rule-preview.test.ts` | 新 | 12 条：单/双/OR 混合/嵌套/空树/非法节点 |

## 二、卡面口径 vs 后端真实分工（重要偏差，按后端落地）

卡面写「65016（条件树结构非法）/ **65033（then 目标非法）**」。grep 后端后确认实际是：

| 情形 | 真实码 | 文案模板（rust 原文） |
|---|---|---|
| 条件树结构 / 上限 / 字段白名单 | `65016` | `边 {}（{} → {}）的条件表达式非法：{reason}`（`graph.rs` R10） |
| then 目标为空 / 超 5 个 / 来源不支持 | **`65016`** | `节点 {}（{}）的审批人表达式不合法：{reason}`（`dto.rs` / `graph.rs` R16） |
| 表达式引用 `PREV_APPROVER` | `65033` | `节点 {}（{}）的审批人表达式用了「上一节点的审批人」（PREV_APPROVER）…` |

即 **`65033` 本期只有 `PREV_APPROVER` 一个场景**（D-E8：发布期必须拒，不许「存进去、跑起来才 501」）。
若按卡面把 then 目标标成 65033，前端提示的码就会与后端真实返回的码不一致 —— 正是本卡要消灭的错位，故按后端实现并在代码头注释写清对照表。

另：**「规则（分支）数 ≤ 32」后端没有对应常量**（WHEN 子树只有深度 8 / 节点 64 / args 16）。
前端把 32 作为**更严的编辑期闸**（32 叶子 + 分组 ≤ 64 节点，与节点闸同量级，不可能造出「前端放行、后端 400」的表达式）；
它命中时**不挂后端码**（`code: null`），避免让用户以为后端会返回那个码。

## 三、单测（`pnpm test:unit` = 81 passed / 0 failed，基线 50 + 本卡 31）

对齐类（**直接读 `backend/src/**` 源码**，不是人眼比对）：

1. 码值：`FLOW_CONDITION_INVALID` / `FLOW_EXPRESSION_INVALID` 与 `shared/error_code.rs` 相等（65016 / 65033）
2. 上限：`MAX_CONDITION_DEPTH/NODES/ARGS`（graph.rs）、`MAX_EXPR_RULES/TARGETS`（expression.rs）逐项相等
3. `FIXED_FIELDS` 与 `graph.rs` 逐项相等（10 个，连顺序）
4. 11 条文案模板在 `graph.rs` / `dto.rs` / `assignee.rs` / `expression.rs` 里**逐字存在**（`"<模板原文>"` 命中）
5. `fill()` 语义与 rust `format!` 一致（顺序占位 / 具名占位；缺参数抛错 = 编译期错误）

边界类：深度 8 ✓ / 9 ✗、节点 64 ✓ / 65 ✗、规则 32 ✓ / 33 ✗、白名单 10 字段 + `form.*` ✓ / `user.nickname` ✗、
空字段 → `cmp 节点缺少 field`、then 空 → 65016、then 6 个 → 65016 超限、`PREV_APPROVER` → 65033。

预览类：单条件 / 双条件 AND / OR 混合加括号 / 同算子摊平 / `非(...)` / 空树 / 空字段 `⚠ 第 1 条规则有空字段` / 目标为空。

## 四、真浏览器证据（真组件 `NodeProps.vue` 挂 8 个实例）

冒烟页（临时，跑完已删）：`createApp` 挂 8 个 `NodeProps`，每个一套真实数据形状；Chromium 读 DOM。

| 用例 | `node-props__rule--error` | 面板徽标 | 错误码 | 问题文案（截断） |
|---|---|---|---|---|
| ① 正常 2 条件 + 角色审批 | ✗ | 校验通过 | — | — |
| ② 9 层嵌套 | **✓** | 校验未通过 1 条 | 65016 | 条件嵌套层数超过 8 层（请把条件拆成几条边） |
| ③ 非白名单 `user.nickname` | **✓** | 1 条 | 65016 | 字段 user.nickname 不在白名单内（允许 form.*、…） |
| ④ then 目标留空 | **✓** | 1 条 | 65016 | 第 1 条规则（r_1）的审批人不能为空（至少 1 个） |
| ⑤ 33 条规则 | **✓** | 1 条 | —（纯前端闸） | 规则（分支）数不能超过 32 条（当前 33 条） |
| ⑥ 65 个节点 | **✓** | 2 条 | 65016 | 条件表达式节点数超过 64 个（…）+ 规则 33/32 |
| ⑦ 空树 | — | — | — | 预览行 = 「尚未配置规则」 |

红框几何证据：`getComputedStyle(rule).boxShadow` = `rgb(213, 73, 65) 0px 0px 0px 1px`（= `--td-error-color`）。
计数器：`结构 9/8 层`、`节点 65/64`、`规则 33/32 条` —— 超限那一档单独涂红（`.rp__meter-value--over`）。

**实时性（无提交动作）**：`live` 用例先渲染合法条件（T+0.2s：`red=false / 校验通过 / 结构 2/8 层`），
1.2 秒后**只改数据**（不点任何按钮、不发请求）→ T+2.2s：`red=true / 校验未通过 1 条 / 错误码 65016 / 结构 9/8 层`。
证明「超限立刻红框，不是提交时才报」。

正例预览文本（真 DOM）：`当 form.amount ≥ 10000 且 form.dept 是 财务 时` / `→ 由 角色 FINANCE_DIRECTOR 审批`。

截图：`p1b3-evidence/p1b3-rule-preview-smoke.png`（8 个用例同屏）。

## 五、门禁

```
pnpm typecheck        exit 0（0 错误）
pnpm build            exit 0
pnpm test:unit        81 passed / 0 failed（基线 50 + 本卡 31）
pnpm check:palettes   全部通过 ✓（0 裸 hex；RulePreview.vue 只用 --td-* 变量）
dist 文案检查          dist/assets/edit-*.js 含「规则预览」「条件嵌套层数超过 {MAX_CONDITION_DEPTH} 层」
                      「PREV_APPROVER），本版本未开放」「规则（分支）数不能超过」
```

## 六、已知限制 / 没验的部分

- **未上生产**：本卡只做编辑期校验与预览，未 build/发布生产 dist，未改后端。
- **真正链路边编辑**未验：流程定义编辑页要登录 + 造流程 + 进画布四级深，本卡用挂真组件的冒烟页验 DOM（同一组件、同一数据形状）；
  链路层留给后续整链 e2e。
- **THEN 段的目标来自目标节点**：`USER`/`ROLE`/`DEPT_LEADER` 的 `assignee.ref_id` 在库里是 **id**，
  而 DSL 的 `then` 要 `role_codes`；预览只做人话展示（`角色 <id>`），不做 id→编码翻译（那要再查一次角色表），
  空目标 / 超 5 个 / 保留词这三类判定不受影响。
- **字段中文名**：`form.*` 默认显示原字段名（`form.amount`）；`RulePreview` 留了 `fieldLabels` 入参，
  等字段字典接进来就能显示「金额」。
