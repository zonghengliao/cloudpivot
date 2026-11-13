# 待应用的文档补丁

这里放**已写好、但需要人确认才能落地**的文档改动（`git apply` 一次即可）。

## AGENTS-hr50.diff

**用途**：HR-50（`t_4d9117bb`）要求的「`AGENTS.md` 菜单/权限口径回写」——
`AGENTS.md` 在 Agent 会话里是**受保护文件**（写它需要人工批准），本卡跑在无人值守的
看板 worker 里拿不到批准，故把改动落成补丁留在仓库里，等内容审核后一次应用。

**改了什么**（6 处，全部是回写既有事实，不改任何约定语义）：

| # | 位置 | 改动 |
|---|---|---|
| 1 | 目录 | 补 `backend/src/business/`（原文只列 `system/ support/`，与实际 20+ 模块不符） |
| 2 | 常用命令 | 补 3 条 HR 验收入口（上线演练 / 薪资链路 E2E / 数据巡检） |
| 3 | 约定 6 迁移号段 | 「基础版最大 `V0017`」→ 实测已到 `V0403` + 跨目录全局唯一 + 先落地者定号 |
| 4 | 约定 12 菜单 | `menu_invariants.sh` 从「6 条断言」→ 实际 **8 条**并逐条命名 |
| 5 | 约定 14（新增） | **页面显形码 `*:page` 与全量接口码 `*:list` 解耦**（HR 试点基线，指向编码规范 §1.7.2 与 PERM-01） |
| 6 | 新增小节「人力资源模块（HR）口径」 | 实测基数（10 表 / 50 路由 / 40 码 / 41 行菜单 / 17 薪资项 / 4 流程 / 68001~68012）、角色矩阵、数据范围、门禁与验收命令、演练库只在 145 |

**应用**：

```bash
cd /opt/cloudpivot
git apply --check docs/patches/AGENTS-hr50.diff   # 先试
git apply         docs/patches/AGENTS-hr50.diff
git add AGENTS.md && git commit -m "docs(agents): HR-50 回写菜单/权限口径（页面码解耦 + HR 实测基数）"
```

应用后请删除本目录下的 `AGENTS-hr50.diff` 与本节（补丁落地即无意义）。

**基线来源**（可复核，两处独立取证）：

1. 2026-11-13 在 145 上跑 `bash scripts/verify/hr/hr_49_release_drill.sh fresh`
   （空库 `hr49_fresh_drill`，回放 **335 条** = 323 版本化 + 12 种子 到 V0403）后统计 `sys_menu` /
   `biz_salary_item` / `biz_flow_def` / `schema_migrations`；同一库上 `hr_invariants.sh` 8 条 BLOCKER
   零违规（WARN 1 条：演练造的员工没合同）、`menu_invariants.sh` 8 条断言全绿（菜单总数 501 ∈ 400–520）。
2. 2026-11-13 HR-50 本卡再取一份独立证据：`bash docs/evidence/2026-11-13-hr50-doc-verify/run_accept.sh`
   （另一个空库 `hr50_docs_drill`）—— 迁移 335（最大 V0403）、菜单 41（5 页面 + 35 按钮 + 1 目录）、
   权限码 40（5 `*:page` + 5 `*:list` + 30 按钮）、薪资项 17（7 条公式）、流程 4、HR 表 10、
   `hr_invariants.sh` rc=0 / `menu_invariants.sh` 8 条全绿、`/health` 200 / `hr/health` 401 / `hr/nope` 404。
   逐项对拍表见该目录 `README.md`；角色矩阵期望值由 `python3 scripts/verify/hr/hr_perm_expect.py` 解析得出
   （HR_MANAGER 40 / FINANCE_MANAGER 5 / DEPT_LEADER 5 / EMPLOYEE 4 / FINANCE_DIRECTOR 0）。
