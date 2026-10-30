# 任务列表 `root_only`（只返回顶层任务）+ 前端默认启用 —— T8-C 证据

> 老大诉求：「任务列表应该也要表示出父子任务。现在的表现形式不行」
> 本卡结论：**分页 + 树形唯一可行的组合 = 列表只返回顶层任务，子任务靠展开懒加载**。
> 后端加 `root_only`（1 个无参数 SQL 片段），前端列表视图默认传 `true`，其它视图一律不传。

---

## 一、改了什么

| 文件 | 改动 |
|---|---|
| `backend/src/business/task/dto.rs` | `TaskPageQuery` 新增 `#[serde(default)] pub root_only: Option<bool>`（带「为什么需要」的文档） |
| `backend/src/business/task/model.rs` | `TaskFilter` 新增 `pub root_only: bool`；`build_filters()` 末尾追加 **无参数常量片段** `AND t.parent_id IS NULL`；新增 `root_only_tests`（7 条） |
| `backend/src/business/task/handler.rs` | `build_filter()` 归一化 `root_only: query.root_only.unwrap_or(false)`；`page()` 文档补参数说明 |
| `frontend/src/api/business/task-api.ts` | `TaskPageQuery` 新增 `root_only?: boolean` |
| `frontend/src/views/business/task/index.vue` | `loadList()` 的 `pageTasks({…})` 新增 `root_only: true`（**仅列表视图**） |
| `frontend/tests/unit/task-root-only.test.ts` | 前端单测 4 条（类型契约 / 列表默认启用 / 其它视图不传 / 跨层一致） |
| `frontend/e2e/09-t8c-root-only.spec.ts` | Playwright 真机 2 条（列表树形 6 项判据 + 看板/甘特/燃尽图不受影响） |
| `scripts/verify/t8c_root_only_e2e.py` | 接口层验收 16 条断言（参数化 `T8C_BASE/T8C_USER/T8C_PASS`，可复跑） |

**红线保持**：`scope::paginate` 一行未改；`root_only` 是**无绑定参数**的常量片段，
所以「片段参数恒前置」的约定与 `$1/$2…` 编号完全不变（有硬断言，见 §3 ①②）。

### 为什么 `total` 天然同口径
`scope::paginate` 用**同一条 base_sql**（去掉 ORDER BY 的前缀）做 `SELECT COUNT(*)`，
`root_only` 片段就在 base_sql 里 → `list` 与 `total` 不可能分叉，不会出现「43 条 / 5 页」
但只有 28 条顶层、第 6 页空白的问题。接口验收实测 `total=28 / list=28`。

---

## 二、单测（后端 7 条 + 前端 4 条）

### 后端 `cargo test --lib`（真库 + 不连库各一遍）

```
########## cargo test --lib（不连库）##########
test result: ok. 1012 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.56s

########## cargo test --lib（真库 T8C，全量并行）##########
test result: ok. 1012 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.00s
```

基线 1005（W1c 交付后） + 本卡 7 = **1012**。本卡 7 条明细（单跑输出）：

```
running 7 tests
test business::task::model::root_only_tests::root_only_absent_is_backward_compatible ... ok
test business::task::model::root_only_tests::root_only_keyword_searches_within_top_level ... ok
test business::task::model::root_only_tests::root_only_lists_top_level_rows_only ... ok
test business::task::model::root_only_tests::root_only_snippet_adds_no_bind_params ... ok
test business::task::model::root_only_tests::root_only_snippet_keeps_param_order ... ok
test business::task::model::root_only_tests::root_only_status_filters_within_top_level ... ok
test business::task::model::root_only_tests::root_only_total_counts_top_level_only ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 1005 filtered out; finished in 2.14s
```

对应卡面六条判据：

| 卡面 | 用例 | 断言要点 |
|---|---|---|
| ① 只返回 parent_id IS NULL | `root_only_lists_top_level_rows_only` | 页内 id 集合 == {R1,R2}，且 `parent_id` 全 None；父行 `child_count` 仍为 3（前端据此画箭头） |
| ② total 也只算顶层 | `root_only_total_counts_top_level_only` | `root_only.total=2` vs 不带 `total=5`；且 `total == list.len()` |
| ③ false / 不传 → 全部 | `root_only_absent_is_backward_compatible` | 缺省（`TaskFilter::default().root_only=false`）与显式 `false` 都 `total=5`，子任务照旧出现 |
| ④ + keyword 在顶层里搜 | `root_only_keyword_searches_within_top_level` | 关键词命中父 → 1 条；只命中子（`T8C-C1`）→ **0 条**（不会把父带出来） |
| ⑤ + status 在顶层里筛 | `root_only_status_filters_within_top_level` | `root_only` → 1 条（R1）；关掉 → 3 条（R1+C2+C3） |
| ⑥ 参数顺序不变 | `root_only_snippet_adds_no_bind_params` + `root_only_snippet_keeps_param_order` | 片段里 0 个占位符、params 个数 0 增长；`status+keyword+root_only` 与关掉时 params **逐项相等**，且 `sql_on == sql_off + " AND t.parent_id IS NULL"`（片段必在业务过滤之后） |

> 夹具隔离坑（第一次跑全量套件时踩到）：真库是全模块共用的，`is_super + All` 的用例会看到
> 别的模块夹具 → 「total 应该是 5」会漂。收口方式：所有查询带夹具关键词 `T8C-`
> （标题 `T8C T8C-R1` / 任务号 `T8C-R1`），别人的夹具天然进不来。

### 前端 `pnpm test:unit`

```
1..85
# tests 85
# pass 85
# fail 0
```

基线 81（P1-B3 交付后）+ 本卡 4 = **85**。本卡 4 条：

```
ok 1 - TaskPageQuery 包含可选布尔 root_only（与后端 TaskPageQuery.root_only 同名）
ok 2 - 列表视图 loadList 的 pageTasks 调用带 root_only: true，且全页只出现一次
ok 3 - 看板 / 燃尽图 / 甘特图 / 父任务选择器都不传 root_only（它们需要全部任务）
ok 4 - 后端 dto / model / handler 三处与前端契约一致
# pass 4 / # fail 0
```

第 3 条不是「真空通过」：同一用例反向断言 `TaskBoard.vue` 里**确实**有 `pageTasks(` 调用
（只是不传 `root_only`）。

---

## 三、门禁 `scripts/check.sh`（最终代码）

```
==> 后端：cargo fmt --check
==> 后端：cargo clippy -- -D warnings
==> 前端：pnpm typecheck
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> 批次B 门禁：全部通过
==> C7 门禁汇总：G1–G6 共 6 条，未通过 0 条
==> C7 门禁：全部通过
==> 偏好字典与色板一致性（V0091 ui.palette oneof ↔ style/palettes.ts）
  PASS：22 套色板与 V0091 的 ui.palette oneof 逐字一致
==> 全部检查通过
check.sh exit=0
```

`pnpm typecheck` 单独复跑：`typecheck exit=0`。

---

## 四、接口层验收（真 HTTP，`scripts/verify/t8c_root_only_e2e.py`）

环境：隔离实例（端口 18085）指向 `cloudpivot_t8c_e2e`（**生产库的一次性只读副本**，
43 = 5 父 + 15 子 + 23 独立）。

```
[PASS] 登录 :: HTTP 200 code=0 msg=ok
[PASS] 拿到 access_token :: len=463
[PASS] 不传 root_only → total 仍是全部任务（向后兼容） :: total=43 page_size=50 list=43
[PASS] 不传 root_only → 列表里仍有子任务（parent_id 非空） :: 子任务 15 条 / 15
[PASS] root_only=false → 与不传完全一致（total 43） :: total=43
[PASS] root_only=true → total 变成顶层数 28 :: total=28（43 - 15 子）
[PASS] root_only=true → 列表里没有任何子任务 :: list=28 条，parent_id 全空
[PASS] root_only=true → total 与 list 同口径（page_size 足够时不出现空白页） :: total=28 list=28
[PASS] 顶层里 5 个父任务的 child_count > 0（前端据此画可点的箭头） :: child_count: [3, 3, 3, 3, 3]
[PASS] 父任务的 child_count 之和 = 15（子任务一条不少，只是靠懒加载取） :: sum=15
[PASS] root_only + page_size=10 → 第 1 页 10 条顶层，total 28 :: list=10 total=28
[PASS] root_only + 第 3 页仍有数据（8 条），没有空白尾页 :: page=3 list=8
[PASS] root_only + keyword('整理季度') → 结果全部是顶层 :: total=1
[PASS] root_only + status=TODO 的 total ≤ 不带的 total（在顶层里筛） :: root_only=5 / 全部=14
[PASS] 看板/甘特/燃尽图不传 root_only 时行为不变（按状态取数仍有数据） :: TODO=14 DOING=6
[PASS] root_only 传非法值 → 4xx（不静默当 false） :: HTTP 400 code=10001 msg=请求体参数校验失败：…

=== 汇总：16 PASS / 0 FAIL ===
```

---

## 五、Playwright 真机（`frontend/e2e/09-t8c-root-only.spec.ts`）

```
  ✓  1 [chromium] › 列表第 1 页 = 5 个带真箭头的父任务；展开出 3 个子行；total 28 (2.5s)
  ✓  2 [chromium] › 看板 / 甘特 / 燃尽图不传 root_only，数据量不变（全部 43 条） (8.1s)

  2 passed (11.7s)
```

### 列表视图（卡面 ①②③④⑤⑥）—— 原始 DOM 断言输出

```json
[T8C-EVIDENCE-LIST] {
  "countText": "全部可见任务 · 共 28 条",
  "pageTotalText": "共 28 条数据",
  "arrowProbe": [
    { "index": 0, "width": 0,  "svgCount": 0 },
    { "index": 1, "width": 0,  "svgCount": 0 },
    { "index": 2, "width": 0,  "svgCount": 0 },
    { "index": 3, "width": 16, "svgCount": 1 },
    { "index": 4, "width": 16, "svgCount": 1 },
    { "index": 5, "width": 16, "svgCount": 1 },
    { "index": 6, "width": 16, "svgCount": 1 },
    { "index": 7, "width": 16, "svgCount": 1 },
    { "index": 8, "width": 0,  "svgCount": 0 },
    { "index": 9, "width": 0,  "svgCount": 0 }
  ],
  "parentRowCount": 5,
  "parentBorderLeft": { "width": "4px", "style": "solid" },
  "parentProgress": "3/3",
  "parentProgressFill": "100%",
  "expandedParent": "P1-B 前端规则构建器（EXPRESSION 编辑 UI）",
  "childRows": 3,
  "childTitles": ["P1-B1 when 条件树编辑器", "P1-B2 then 目标选择器", "P1-B3 实时校验 + 规则预览 + 错误码对齐"],
  "childConnector": { "borderLeft": "1px", "width": "12px" },
  "childIndent": "24px",
  "listRequestsWithRootOnly": 1
}
```

对照卡面：
- ① 5 个带**真箭头**的父任务 —— `.t-table__tree-op-icon` 共 10 个（TDesign 给每行都渲染容器），
  其中 **5 个 `svgCount=1` 且 `width=16 > 0`**（第 4–8 行），其余 5 个是叶子行的空容器（width=0）。
  判据与卡面一致：宽度 > 0 且有 svg 子元素。
- ② 点箭头 → **3 个子行**，带 `task-row--child` + `t-table-tr--level-1`，`::before` 连接线 1px 宽 12px。
- ③ 父行进度列 `3/3` + `.sub-progress__fill` width=100%（SubProgressBar；本数据集三个子任务都已 DONE，
  所以是 3/3，判据是「N/M 小进度条存在且 N/M 与子任务数一致」）。
- ④ 父行首列 `border-left: 4px solid`。
- ⑤⑥ 工具条与分页器都是 **28 条**（不是 43）；第 1 页 10 行、第 3 页 8 行，无空白尾页。

### 看板 / 甘特 / 燃尽图不受影响（卡面 ⑦）—— 原始网络 + DOM 断言输出

```json
[T8C-EVIDENCE-OTHER-VIEWS] {
  "boardUrls": [
    ".../api/business/task/page?page=1&page_size=50&status=TODO",
    ".../api/business/task/page?page=1&page_size=50&status=DOING",
    ".../api/business/task/page?page=1&page_size=50&status=REVIEW",
    ".../api/business/task/page?page=1&page_size=50&status=DONE",
    ".../api/business/task/page?page=1&page_size=50&status=CANCELLED"
  ],
  "boardTotals": [4, 6, 0, 19, 14],
  "boardSum": 43,
  "boardCardCount": 28,
  "boardCountText": "全部可见任务 · 共 43 条",
  "ganttUrls": [],
  "burndownUrls": []
}
```

- 看板 5 列的请求**全部不带 `root_only`**（断言 `withRootOnly() == []`），四列 total 之和 **43**；
  卡片 28 张 = 顶层数，另外 **15 条子任务内嵌在父卡片的 `.subtasks__row` 里**（E6c 口径，轮询到 15 才断言）。
- 甘特图 / 燃尽图走各自接口，全程 **没有一条 `/business/task/page` 请求**，更不会带 `root_only`。

### 截图

- `t8c-list-expanded.png` —— 列表：5 个父行（4px 竖条 + 3/3 小进度条）+ 展开后的 3 个子行（└─ 连接线、24px 缩进）+ 分页「共 28 条」
- `t8c-board-unaffected.png` —— 看板：计数「共 43 条」、28 张卡 + 内嵌子任务清单

（两张图随本卡 attachment 提交；同时保留在 `frontend/test-results/…` 里，重跑即覆盖。）

---

## 六、环境与「生产未动」说明

- 代码验证：共享工作区 `/opt/cloudpivot`（HEAD = `origin/main` = bare `main`，动手前工作区干净），
  编译用独立 `CARGO_TARGET_DIR=/root/t8c-target`（不碰 dev 8024 / prod 8023 的产物）。
- 数据：Playwright 与接口验收都打**副本库 `cloudpivot_t8c_e2e`**（`pg_dump cloudpivot` 还原，
  43 条 CP-DEV 任务），实例端口 **18085**、前端 vite **5185**、Redis 用 DB 9 ——
  **生产库 / 生产服务 / 生产前端 dist 一律没动**。
- 副本库里只改了一样东西：把 `laoliu` 的口令哈希对齐成 `laoliu_test` 的（卡面要求「登录 laoliu」，
  而生产库里的 laoliu 口令无人知晓）。**这是副本库行为，生产库未改**。
  两个账号的任务权限码在库里完全一致（各 18 条 `business:task:*`，含 `business:task:read`）。

---

## 七、与卡面的差异（3 处，均已实测确认）

1. **类型名**：卡面写 `TaskPageFilter`，仓里真实类型是 `dto::TaskPageQuery`（请求）+
   `model::TaskFilter`（归一化后的过滤条件）。改的是这两个真实类型，没有新建 `TaskPageFilter`。
2. **字段形态**：`TaskPageQuery.root_only` 是 `Option<bool>`（与卡面一致，带 `#[serde(default)]`）；
   `TaskFilter.root_only` 用的是 `bool`（handler 里 `unwrap_or(false)` 归一化一次，下游不再重复判空）。
3. **后端单测 7 条而非 6 条**：把卡面 ⑥「参数顺序」拆成两条 —— 一条钉「片段无参数」，
   一条钉「片段位置在业务过滤之后 + params 逐项相等」，因为这两件事失败方式不同。

另有一处**卡面预期与实际实现不符但结论一致**：卡面 ⑦ 写「看板 / 甘特 / 燃尽图的数据量不变」，
实测看板本来就把子任务**内嵌在父卡片里**（E6c），所以卡片数是 28、**内嵌子任务行 15**、
四列 total 之和 43 —— 数据量确实没变，只是展示形态不是「43 张卡」。断言已按真实口径写死（28 + 15 = 43）。

---

## 八、复跑方式

```bash
# ① 后端单测（不连库 / 真库）
cd /opt/cloudpivot/backend
export PATH=/root/.cargo/bin:$PATH
cargo test --lib -j 1
TASK_DB_URL="postgres://<user>:<pass>@127.0.0.1:5432/cloudpivot_t8c" cargo test --lib -j 1

# ② 前端单测 + 类型检查
cd /opt/cloudpivot/frontend && pnpm test:unit && pnpm typecheck

# ③ 门禁
cd /opt/cloudpivot && bash scripts/check.sh

# ④ 接口层验收（先把隔离实例指向有数据的副本库）
T8C_BASE=http://127.0.0.1:18085 T8C_USER=laoliu T8C_PASS=<口令> \
  python3 scripts/verify/t8c_root_only_e2e.py

# ⑤ Playwright 真机（前端 dev 通过 BACKEND_ORIGIN 指到隔离实例）
cd /opt/cloudpivot/frontend
BACKEND_ORIGIN=http://127.0.0.1:18085 pnpm dev --port 5185 --strictPort &
E2E_BASE_URL=http://127.0.0.1:5185 E2E_USERNAME=laoliu E2E_PASSWORD=<口令> \
  npx playwright test e2e/09-t8c-root-only.spec.ts --reporter=list
```

真库夹具自带前缀（用户 `t8c_root_only_user`、任务号 `T8C-*`），跑完只删自己那 5 行，
不碰别人数据；不设 `TASK_DB_URL` 时 4 条真库用例打印 `SKIP` 并直接返回（CI 无库仍全绿）。
