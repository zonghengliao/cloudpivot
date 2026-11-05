# 批量操作/导出成功后的结果弹窗看不到 —— 真机证据（t_cea00ecf）

一句话：`BatchActionBar` 的根节点 `<div v-if="visible">` 只判「有勾选 / 正在跑」，
而 `execute()` 成功收尾先 `resultVisible = true` 再 `emit('update:selected', [])` 清空勾选
→ 根节点连同结果弹窗一起被卸载。修法是把「结果弹窗还没被用户关掉」也算进 `visible`。

## 环境

| 项 | 值 |
|----|----|
| 前端 | vite dev `127.0.0.1:5212`（源码 = `/root/cea00ecf-wt/frontend` 工作树，`BACKEND_ORIGIN=http://127.0.0.1:18126`）|
| 后端 | `/opt/cloudpivot/backend/target/debug/cloudpivot`（HEAD 4500b65 的 debug 二进制）`127.0.0.1:18126` |
| 库 | `cloudpivot_cea00ecf` = `cloudpivot_deadmin` 的 `pg_dump` 克隆（收尾已 dropdb）|
| 账号 | `admin`（克隆库沿用 `Admin@123456`，`must_change_password=false`）|
| 脚本 | `<kanban workspace>/verify-before-fix.mjs`（红）、`verify.mjs`（绿），仓库外运行 |

## 红：修复前（HEAD 4500b65 原样，`visible` 只有两个分支）

`report-before-fix.json`：

```json
{"dialogShown": false, "barMounted": false, "barVisible": false, "messageToast": [],
 "requests": [{"url": ".../payable/batch-export", "status": 200, "task_no": "EXP20261105-7151", "total": 1}]}
```

即：批量导出**请求 200 + task_no 有值**，但界面上没有弹窗、没有 toast、工具栏整条被卸载。
截图 `R-before-fix-no-dialog.png`。

## 绿：修复后（本卡改动）

`report.json`（同一脚本、同一库、同一页面）：

- **A 批量导出（应付页）**：勾选 1 行 → 点「批量导出」→ 弹窗出现
  - 标题：`已提交导出任务 EXP20261105-534E（1 行）`
  - 正文：`已提交导出任务，请到「导入导出」页下载「应付单_20261105_181338.xlsx」`
  - 请求：`POST /api/business/erp/payable/batch-export` → **200**，`task_no=EXP20261105-534E`，`total=1`
  - 弹窗打开期间工具栏仍在（`barStillMounted=true`，计数文案「未勾选」——勾选已被 `execute()` 清空）
  - 关掉弹窗后：`barGone=true`、`resultVisible=false` → 回到「未勾选即隐藏」
  - 截图 `A-export-result-dialog.png`
- **B 批量改状态（收货页，1 条会失败）**：勾选 2 条 DRAFT 后，另一用户先把其中 1 条作废（真并发），
  再点「批量作废」（填原因确认）
  - 标题：`共 2 条：成功 1 条，失败 1 条`
  - 正文：`失败的行未做任何改动（逐条独立事务），可按原因修正后重试` + `失败 1 条（逐条原因）` + 复制清单
  - 失败清单：`IN-20261105-000003：收货入库单 IN-20261105-000003 已经是「已作废」（CANCELLED），不能重复作废：库存只退一次`（后端原文）
  - 关掉弹窗后工具栏隐藏
  - 截图 `B-partial-failure-dialog.png`
- **C 换页/重新勾选不残留**：关掉弹窗后重新勾选一行 → 工具栏回来，结果弹窗仍是关闭态，
  标题回到默认 `批量操作结果`、正文为空（`headerHoldsOldExport=false`），说明 `outcome` 已在关闭时清掉。

## 门禁

| 命令 | 结果 |
|------|------|
| `pnpm typecheck`（vue-tsc --noEmit）| 退出码 **0** |
| `npx tsx --test tests/unit/batch-bar-result-dialog.test.ts` | **5 passed / 0 failed** |
| 同上，把 `\|\| resultVisible.value` 删掉（变异）| **4 passed / 1 failed** —— 红在「结果弹窗开着时工具栏必须在」 |
| `pnpm test:unit`（全量）| 822 tests / **820 pass / 2 fail**：两条红是 `flow-biz-link`（探测 `#[nik::get("/business/task/{id}")]`）与 `task-root-only`（DTO 归一化），在 HEAD 上同样红，属他人内容，本卡未改动 |

## 已知噪声（不是本卡引入）

- 控制台 2 条 `401 Unauthorized` = 每页一条 `GET /api/business/notice/stream`（`?token=` 通道恒 401，
  认证层不采信查询令牌），全站既有缺陷；后端日志 `path=/api/business/notice/stream status=401` 可对账。
- 弹窗打开期间工具栏仍挂在弹窗遮罩后面（显示「未勾选」），这是「结果弹窗是根节点子节点」的必然结果；
  关掉弹窗后立即隐藏。若要连这一条也消掉，需要把工具栏内容再包一层 `v-if`，本卡按卡面「关掉后回到未勾选即隐藏」口径未做。
