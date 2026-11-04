# 二次独立复验（run 623，只读）

> 卡：t_42d844c0（预算三页修复）。首轮交付见上一级目录（commit `35750d5`）。
> 目的：在被并发守护置为 scheduled 后重新派发时，**不重做、只复验**首轮结论是否仍成立。

## 环境（当轮实跑，非回放）

| 项 | 值 |
|----|----|
| 后端 | `http://127.0.0.1:18097`（`./target/debug/cloudpivot`，实例 `cloudpivot_manual2`，本轮 PID 3234164 在听） |
| 前端 | `http://127.0.0.1:5180`（vite dev，PID 2383632 在听） |
| 代码 | `/opt/cloudpivot` HEAD = `cd41b98`（`35750d5` 的预算文件在 HEAD 上**零差异**：`git status --porcelain -- frontend/src/views/business/erp/budget frontend/src/api/business/erp/budget.ts frontend/src/router/index.ts` 为空） |
| 账号 | `admin`（SUPER_ADMIN） |
| 浏览器 | Playwright Chromium 1440×900 |

## 结果

```
route /business/erp/budget          404=0
route /business/erp/budget/report   404=0
route /business/erp/budget/mapping  404=0
TOTAL_404 = 0
API/PAGE ERRORS = none
```

- 三条路由真机打开 **404 计数 = 0**（脚本用 `text=您访问的页面不存在` 定位兜底 404 页，命中即 exit 1）。
- 全程 `/api/**` 无 ≥400 响应（`/notice/stream` 的已知 401 已排除），无 `pageerror`。
- 本轮**只读**：只打开「新建预算」「新建映射」弹窗取图后 `Escape` 关闭，**未创建、未提交任何数据**（对比首轮脚本会真实建单并走审批链）。

| 文件 | 取景点 |
|------|--------|
| `01-budget-list.png` | 预算编制列表 4 行：`BDG-20261105-000005` 审批中（仅「详情」）、000004/000003/000001 已生效（「详情 / 调整」） |
| `02-budget-create-dialog.png` | 新建预算弹窗：年度 / 部门 / 末级预算科目 / 年度额度 / 控制策略 + 12 个月分解网格 + 「提交前自检」 |
| `03-budget-report.png` | 预算执行：额度合计 600,000 / 占用中 0 / 已执行 0 / 已用合计 0 / 可用合计 600,000 / 执行率 0%（超支槽 0），3 个槽 |
| `04-budget-mapping.png` | 科目映射列表 1 行（默认 → 全公司兜底 → 库存现金 1001，优先级 0，启用） |
| `05-budget-mapping-dialog.png` | 新建映射弹窗 |

## 门禁

| 检查 | 命令 | 结果 |
|------|------|------|
| typecheck | 干净 worktree `--detach HEAD`（cd41b98）+ 复用 `frontend/node_modules`，`pnpm typecheck` | **TYPECHECK_EXIT=0**（日志见 `typecheck-head-cd41b98.txt`） |

## 与首轮的关系

- 首轮（`35750d5`）负责**交付**：三个 `.vue` + 路由注册 + 11 张动作图（列表/新建/上报/映射 + 详情/报表/分月/拦截）。
- 本轮（run 623）负责**复验**：5 张图只证明「HEAD 上仍然不是 404、页面仍在真机上渲染出真实数据」，不重复首轮的动作取证。
