# 批次 E · E6b 列表树形表格 + TaskForm 父任务选择 —— 验证证据

> 卡片：`t_d7e00c96`　执行：manong　日期：2026-10-30（CST）
> 目标：任务列表改树形表格（父行可展开、子行懒加载）+ 新建任务可选父任务

## 0. 产物指纹

| 项 | 值 |
|---|---|
| 提交 | `5b6aa79`（6 文件，+286 / −11） |
| HEAD / origin/main / bare main | `5b6aa79641e066c412cbda52bad9ba66fdc28841`（三者一致） |
| prod 运行二进制 | `adbdd745cf91a44857efa574751470c331ba5257ad5672ff6d4be232d22b2388`（`/proc/1715302/exe` == `target/release/cloudpivot`，10:53:23 启动） |
| dist 任务页 chunk | `frontend/dist/assets/index-BKNCBH-i.js`（含「父任务」「子任务」文案） |
| 迁移 | **0**（无 DDL） |
| 新接口 | **0**（复用 `GET /task/page`、`GET /task/{id}/subtasks`） |

改动文件：

```
backend/src/business/task/model.rs        +6  CARD_COLUMNS 补 t.parent_id / TaskCardRow 字段 / from_row / 单测断言
backend/src/business/task/dto.rs          +2  TaskCardRes.parent_id
backend/src/business/task/handler.rs      +1  card_res 映射
frontend/src/api/business/task-api.ts     +2  TaskCard.parent_id
frontend/src/views/business/task/index.vue            +183  t-enhanced-table 树形 + 懒加载 + 父行样式
frontend/src/views/business/task/components/TaskForm.vue  +92  父任务选择器（仅新建）
```

## 1. 与卡面的偏差（必须先看）

1. **后端改了 3 个文件**：卡面写「不改后端」，但实测 `TaskCardRes` / `TaskCardRow` /
   `CARD_COLUMNS` **都没有 `parent_id`**（E6a 只加了 `child_count` / `child_done_count`），
   而 `TaskCard` 上也没有 —— 前端拿不到层级，树形表格无法实现（子任务会既在顶层、
   又在父行下面出现两份）。故做**纯追加**：`CARD_COLUMNS` 补一列 `t.parent_id`，
   DTO 补一个字段。可见性、分页、排序、既有语义一律未动（825 单测全过）。
2. **`t-table` 换成 `t-enhanced-table`**：tree 能力（`formatTreeColumn` /
   `initialTreeDataMap`）只在 `EnhancedTable` 里，`t-table` 是 `PrimaryTable`。
   第一次按卡面用 `t-table` 时箭头根本不渲染（真机已复现），换组件后才出来。
3. **懒加载靠 `children: true` + 重建 rows**：TDesign 用 `children === true` 表示
   「有子节点未取」，此时箭头照常渲染但内部 `toggleExpandData` 直接 return。
   取完数后必须**重建 `rows` 数组**（触发 `resetData → initialTreeDataMap`）才能让
   子行拿到 `level=1`（不重建则不缩进，已真机复现）；重建会让展开态丢失，
   所以用受控 `expandedTreeNodes` 兜住。卡面写的 `rowExpandable` 在 TDesign 里不存在。
4. **父任务截止日只提示、不阻断**：卡面要求「子任务截止不得晚于父任务」，
   但后端 `63112` 挂在 `PATCH /task/{id}/schedule`（改期）上，创建 / 编辑接口并不拦
   （已实测：创建晚于父任务截止日的子任务 `code=0` 成功）。前端若硬拦会挡掉后端
   允许的创建，因此改为橙色提示「晚于父任务截止日，改期时会被 63112 拦下」。

## 2. 接口实测（prod 127.0.0.1:8023，`/root/e6b_verify.py`，17 条断言全过）

| # | 断言 | 结果 |
|---|---|---|
| 1 | `/health` code=0（profile=prod） | ✅ |
| 2 | admin 登录 | ✅ |
| 3 | `GET /task/page` 列表项含 `parent_id` 字段 | ✅ |
| 4 | 建 1 父 + 2 子（子带 `parent_id`、父 2026-11-30、子 11-10 / 11-20） | ✅ 3/3 |
| 5 | 父任务详情 `child_count=2`、`parent_id=null` | ✅ |
| 6 | `GET /task/{id}/subtasks` `total=2`、`list=2` | ✅ |
| 7 | 子任务卡片 `parent_id` 均指向父任务 | ✅ |
| 8 | 关键词筛出 3 条（父 1 + 子 2），父 `child_count=2` / `child_done_count=0` | ✅ |
| 9 | `PATCH /schedule` 把子任务改到 2026-12-31 → `code=63112` | ✅ |
| 10 | dist 命中「父任务」「子任务」（index / detail chunk） | ✅ |

真实返回样例：

```
父：{"task_no":"TSK-202610-000289","parent_id":null,"child_count":2}
子：{"task_no":"TSK-202610-000290","parent_id":"01a18aae-7ca2-…","child_count":0}
63112：{"code":63112,"msg":"子任务…超出父任务计划区间…"}
```

## 3. 真机 DOM（headless Chromium，prod nginx `http://172.18.10.144`）

全部断言读 `textContent` / `getComputedStyle`，网络用 Resource Timing 取证：

| 验收项 | 实测 |
|---|---|
| 父行有展开箭头 | `.t-table__tree-op-icon` 内有 `t-icon-add-rectangle` |
| 父行 `（N 子·M 完成）` 标记 | `（2 子·0 完成）`，行 class `task-row--parent`（`font-weight:500`） |
| 展开才加载子任务 | 点击前 0 请求；点击后 `GET /api/business/task/{id}/subtasks`（200），行数 1 → 3 |
| 子任务缩进正确 | 子行 `t-table-tr--level-1`，首格 `padding-left: 16px`（父行 1px） |
| 状态 / 进度 / 负责人正常 | 每行 `待开始 / P2 / 0% / 超级管理员 / 2026-11-30` 全部在列 |
| **折叠后子任务消失** | 再点箭头 → 行数 3 → 1，只剩父行 |
| 子任务不出现在顶层 | 关键词筛出 3 条时顶层只有 1 行（父任务） |
| 我的任务 → 子任务作为顶层 | 开关打开后 2 条子任务均为 `t-table-tr--level-0` |
| 新建可选父任务 + `parent_id` 正确 | 弹窗「父任务」下拉选 `TSK-202610-000289 E6B-UI验收-父任务` → 创建 `TSK-202610-000316` → 回读 `parent_id` = 父 id，父 `child_count` 2 → 3 |
| 编辑不显示父任务选择器 | 父任务项 `hasSelect: false`，只读文本 `TSK-202610-000289 E6B-UI验收-父任务` |
| 选中父任务后的截止日提示 | `父任务截止 2026-11-30`；越界时橙色「晚于父任务截止日，改期时会被 63112 拦下」 |

截图：`e6b_tree_list.png`（父行 + 3 子行展开态）。

## 4. 静态检查

```
cargo test --lib                          → 825 passed / 0 failed（E6a 824 → +1 断言同用例内）
cargo clippy --all-targets -- -D warnings → Finished（无告警）
pnpm typecheck (vue-tsc --noEmit)         → 0 error
pnpm build                                → 成功；grep -rl 父任务 dist/assets → index-BKNCBH-i.js / detail-XoQoQm2z.js
```

## 5. 生产部署 + 一次并发部署事故（留证）

部署脚本 `/root/e6b_deploy.sh`：`cp → mv`（同盘替换，避免 Text file busy）→
`systemctl restart cloudpivot.service` → `/health` 2s 起来 → `active`，启动日志
「cloudpivot 启动完成, 耗时 75ms」。

**事故**：10:53 复验时发现「列表里子任务跑到顶层、点箭头无请求」。排查结论：
另一张卡（E6c）的部署在 10:50:32 把 **旧 release 二进制**（`c8c57216…`，不含本卡
`parent_id`）换到了 prod，而 dist 又是我这一版 —— 前后端版本错位。按 HEAD 重新
`cargo build --release`（`adbdd745…`）+ 重部署后一切正常。已在卡片留 `hotspot` 备注：
**prod 二进制与 dist 是多卡共用部署面，验收前必须核对 `/proc/<MainPID>/exe` 与
`sha256sum target/release/cloudpivot` 一致**。

**收尾**：两轮验收数据（1 父 + 3 子、1 父 + 1 子）全部 `DELETE` 软删，
关键词 `E6B-UI验收` 残留 0 条；prod 8023 / dev 8024 未受影响。
