# T8-B 任务列表树形改造 — 证据

> 卡面：`t_d85d0a76`（T8-B）｜依赖 T8-A（后端放开 8 层，已推 main `eac2660`）
> 结论：**前端已改完并全绿**；8 层「逐层展开」在 UI 层用真浏览器验证通过，
> 但生产后端仍是 T8-A 之前的二进制（2 层上限），真机建不出 8 层链 —— 见 §5。

## 1. 改了什么（五点，全部落在前端）

| 卡面 | 做法 | 代码位置 |
| --- | --- | --- |
| ① 箭头移到标题列 | `treeNodeColumnIndex` 由写死 `0` 改为**按 `colKey === 'title'` 反查**，`indent` 16 → 24 | `components/tree-layout.ts::treeConfigOf` + `index.vue` 里 `const treeConfig` |
| ② 父行强化 | `td` 画 4px `--td-brand-color` 左竖条（`padding-left` 减 4px 保证任务号仍对齐）+ `--td-brand-color-light` 浅底 + 标题 600 | `index.vue` `<style>`「T8-B：树形层级视觉」段 |
| ③ 子任务进度 | 「（N 子·M 完成）」纯文字 → 新增 `SubProgressBar.vue`（6px 小条 + `N/M` 分数，满进度换 `--td-success-color`），挂在**进度列**；父行显示子任务完成度，非父行仍显示自己的百分比 | `components/SubProgressBar.vue`、`index.vue` `#progress` 插槽 |
| ④ 子行区分 | `--td-bg-color-secondarycontainer` 更浅底 + 箭头前 `::before` 画出 12×12 的 `└─` 连接线 | `index.vue` `<style>` |
| ⑤ 逐节点折叠 | 沿用受控 `expandedKeys` + `children === true` 懒加载占位（机制本来是对的，未改逻辑），只把行 class / 视觉分开 | `index.vue::rowClassName`、`handleTreeExpandChange`（未改） |

新增纯逻辑模块 `components/tree-layout.ts`：`rowClassNameOf` / `childProgressPercent` / `childProgressText` /
`titleColumnIndex` / `treeConfigOf`，无 Vue 依赖，可被 `tsx --test` 直接测。

## 2. 三个必须知道的实现偏离（都带原因）

1. **父行底色没用卡面的 `--td-bg-color-container-select`**：该 token 在浅色主题下就是 `#fff`
   （深色下是灰 9），画出来等于「没有浅底」。改用 `--td-brand-color-light`（品牌色阶 1，
   22 套配色都由 `src/style/palettes.ts` 统一派生），hover 用 `--td-brand-color-light-hover`。
2. **连接线画在箭头（`.t-table__tree-op-icon`）的 `::before` 上，不是 `.cell-title::before`**：
   箭头是表格在标题单元格里自己渲染的，插在 `.cell-title` 里会变成「▼└─ 标题」（连接线跑到箭头后面）。
   挂在图标前面才是「└─▼ 标题」，而且**跟着 level 缩进一起移动**，8 层都不会错位。
3. **补了两条 TDesign 适配**（不补就是坏的，实测出来的）：
   - 标题列声明了 `ellipsis: true`，TDesign 会把树形容器改成 `display: inline`，
     导致「行内箭头 + 块级 `.cell-title`」分行：实测**箭头与标题垂直差 23px**、行也变高。
     已把 `.t-table__tree-col` / `.t-table__tree-leaf-node` 改回 `display: flex`（截断仍由
     `.cell-title__text` 自己的 `text-overflow: ellipsis` 负责）。
   - 原文件里 5 处 `::deep(...)` 是**双冒号**，Vue 3 的 SFC 编译器只认 `:deep(...)`，
     编译产物里会原样留下 `::deep(` → 选择器永远不匹配（E6b 的父行加粗其实一直没生效）。
     已全部改成单冒号，并在 `dist/assets/*.css` 里核对到 `[data-v-xxxx] .task-row--parent>td{...}`。

## 3. 门禁（全部实跑）

```
pnpm typecheck            → 0 错误
pnpm build                → 成功（dist 已核对含新选择器）
pnpm test:unit            → 50 passed / 0 failed（基线 39 + 本卡 11，全部在 tree-layout.test.ts）
pnpm check:palettes       → 22 套 × 明暗，「全部通过 ✓」
裸 hex 扫描                → 新增/改动文件 0 命中（配色只用 --td-* token）
```

## 4. 真机证据（真浏览器，两条真后端 + 一条 UI 深度）

环境：本机 `pnpm build` 出的 dist（`127.0.0.1:5201`），`/api` 反代 `172.18.10.144:80`（真后端，
账号 `laoliu_test`），**没碰生产 dist**。

### 4.1 仓库内长期用例（新增，可回归）

`frontend/e2e/08-t8b-task-tree.spec.ts` —— 真后端造「根 + 3 子 + 1 独立任务」，断言：
箭头在标题列 / 任务号列无树形容器 / 父行 4px `--td-brand-color` 竖条 + `--td-brand-color-light` 浅底 + 600 /
父行进度列 `1/3` + `width:33%` / 非父任务无子任务条 / 子行 `task-row--child` + `t-table-tr--level-1` +
`::before` 连接线 1px×12px + 缩进 `24px` + `--td-bg-color-secondarycontainer` / 收起后兄弟任务仍在 / 10 列 + 4 Tab。

```
env E2E_BASE_URL=http://127.0.0.1:5201 pnpm test:e2e --grep "T8-B"   → 2 passed (4.0s)
```

### 4.2 仓库外验证目录（不污染 `frontend/e2e`）

`<ws>/t8b-verify/`（playwright.config + tests）：

| 用例 | 断言要点 | 结果 |
| --- | --- | --- |
| `tree-real.spec.ts` ① | 箭头在标题列、父行竖条/浅底/加粗、进度 `1/3`+`33%`、叶子行无箭头 svg、10 列 / 4 Tab / 分页 | pass |
| `tree-real.spec.ts` ② | 展开 3 条子任务、子行连接线与缩进、收起后兄弟独立任务仍在、重复展开不翻倍 | pass |
| `tree-depth.spec.ts` | **8 层**逐层懒加载：level 0→7 全对、缩进 1/24/48…/168px、第 8 层无箭头；折叠第 2 层后第 3~8 层收起而兄弟任务仍在；再展开恢复 | pass |
| `tree-geometry.spec.ts` | 箭头与标题同一行（垂直差 ≤2px）、箭头在标题左侧、超长标题仍省略且不越界、父子行高一致 | pass |

```
cd <ws>/t8b-verify && ./node_modules/.bin/playwright test -c playwright.config.ts   → 4 passed (5.4s)
```

截图：`t8b-evidence/t8b-tree-8level.png`（8 层全展开时的表格，人工复核视觉层次用）。

## 5. 没验到的部分（诚实说明）

- **第 9 层建任务 → 400 + 63005**：生产后端还是 T8-A 之前的二进制（`invalid value: 子任务最多 2 层`
  是旧文案），真机连第 3 层都建不出来，所以这条**无法在真机链路上验**。它属于 T8-A 的范围，
  T8-A 已用 10 条真库单测 + 15/15 HTTP 脚本在同一份代码上验过（第 9 层 400 + 63005）。
  T8-A 发布后，把 `08-t8b-task-tree.spec.ts` 的造数放宽到 8 层即可在真机补上。
- **8 层 UI 验收用的是接口拦截**：只替换 `GET /task/page` 与 `GET /task/{id}/subtasks` 两个读接口
  的返回（字段照真响应 `TaskCardRes` 抄），页面代码/模板/Less/TDesign 树形表格全是真的 ——
  验的是「前端能不能撑住 8 层」，后端那一侧由 T8-A 兜底。
- 看板 / 甘特 / 燃尽三个视图未改动（本卡只动列表视图的表格部分）。
