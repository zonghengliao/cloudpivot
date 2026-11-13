# 待应用的文档补丁

这里放**已写好、但需要人确认才能落地**的文档改动（`git apply` 一次即可）。

## AGENTS-hr50.diff

**用途**：`AGENTS.md` 菜单/权限口径回写 —— 既覆盖 HR-50（`t_4d9117bb`）的六项基线回写，也覆盖
`t_2721945d`（「更新 AGENTS.md：HR 菜单与权限口径」）要求的**完整菜单树 + 40 个权限码逐条表**。
`AGENTS.md` 在 Agent 会话里是**受保护文件**（写它需要人工批准），worker 跑在无人值守的看板里
拿不到批准，故把改动落成补丁留在仓库里，等内容审核后一次应用。

**改了什么**（6 处，全部是回写既有事实，不改任何约定语义）：

| # | 位置 | 改动 |
|---|---|---|
| 1 | 目录 | 补 `backend/src/business/`（原文只列 `system/ support/`，与实际 20+ 模块不符） |
| 2 | 常用命令 | 补 3 条 HR 验收入口（上线演练 / 薪资链路 E2E / 数据巡检） |
| 3 | 约定 6 迁移号段 | 「基础版最大 `V0017`」→ 实测已到 `V0403` + 跨目录全局唯一 + 先落地者定号 |
| 4 | 约定 12 菜单 | `menu_invariants.sh` 从「6 条断言」→ 实际 **8 条**并逐条命名 |
| 5 | 约定 14（新增） | **页面显形码 `*:page` 与全量接口码 `*:list` 解耦**（HR 试点基线，指向编码规范 §1.7.2 与 PERM-01） |
| 6 | 新增小节「人力资源模块（HR）口径」 | ①实测基数；②**菜单树 = 权限码表**（41 行菜单 / 40 个码逐行：type / path / sort / auth_code / 承载接口）；③分组计数表（5 页面码 + 35 按钮码 = 40）；④40 个码的四种状态；⑤角色矩阵；⑥与 ARCH-9 §5/§6 的交叉自查；⑦错误码段 / 数据范围 / 门禁验收 / 演练库 |

**新增小节里的菜单树与权限码表**（`t_2721945d` 的交付物，逐条可核对）：

- 41 行菜单 = 1 目录（`/business/hr`，sort 77，`Layout`，`user-business`）+ 5 页面（`*:page`）+ 35 按钮，层级、`title`、`sort`、父级与迁移 `V0387`+`V0401` **逐格一致**（脚本对拍 0 不符）。
- 40 个权限码 = 5 `*:page` + 35 按钮码（含 5 个 `*:list`）；每行标注「能做什么 / 承载接口」，接口路径省略公共前缀 `/business/hr`。
- 交叉自查：ARCH-9 §5.2 的 32 个权限码 → 32/32 有落点（缺 0）；§6.1 的 6 个 path → 6/6 一致（5 菜单页 + `salary/:id` 独立详情路由）。
- **ARCH-9 自身 3 处口径偏差（只报不改，以本表与迁移为准）**：① §5.2 表头「42 条」vs 正文 46 条 → 实际 46（`ROUTE_CONTRACT` 50 = 46 + HR-19 补 3 + HR-41 补 `/leave/my`）；② §5.2 脚注「4 处 `/my`」只列了 2 条（`/leave/my` 由 HR-41 补；「我的档案」`GET /employee/{my_employee_id}` 至今无卡/无码/无路由）；③ §5.2「权限码共 30 个」指业务按钮码，`V0401` 解耦后全量 40 个。

**应用**：

```bash
cd /opt/cloudpivot
git apply --check docs/patches/AGENTS-hr50.diff   # 先试
git apply         docs/patches/AGENTS-hr50.diff
git add AGENTS.md && git commit -m "docs(agents): HR 菜单树 + 40 个权限码口径回写（含页面码解耦）"
```

应用后请删除本目录下的 `AGENTS-hr50.diff` 与本节（补丁落地即无意义）。

**基线来源**（可复核，两处独立取证）：

1. 2026-11-13 在 145 上跑 `bash scripts/verify/hr/hr_49_release_drill.sh fresh`
   （空库 `hr49_fresh_drill`，回放 **335 条** = 323 版本化 + 12 种子 到 V0403）后统计 `sys_menu` /
   `biz_salary_item` / `biz_flow_def` / `schema_migrations`；同一库上 `hr_invariants.sh` 8 条 BLOCKER
   零违规（WARN 1 条：演练造的员工没合同）、`menu_invariants.sh` 8 条断言全绿（菜单总数 501 ∈ 400–520）。
2. 2026-11-13 HR-50 再取一份独立证据：`bash docs/evidence/2026-11-13-hr50-doc-verify/run_accept.sh`
   （另一个空库 `hr50_docs_drill`）—— 迁移 335（最大 V0403）、菜单 41（5 页面 + 35 按钮 + 1 目录）、
   权限码 40（5 `*:page` + 5 `*:list` + 30 按钮）、薪资项 17（7 条公式）、流程 4、HR 表 10、
   `hr_invariants.sh` rc=0 / `menu_invariants.sh` 8 条全绿、`/health` 200 / `hr/health` 401 / `hr/nope` 404。
   逐项对拍表见该目录 `README.md`；角色矩阵期望值由 `python3 scripts/verify/hr/hr_perm_expect.py` 解析得出
   （HR_MANAGER 40 / FINANCE_MANAGER 5 / DEPT_LEADER 5 / EMPLOYEE 4 / FINANCE_DIRECTOR 0）。
3. 2026-11-13 `t_2721945d` 的**静态对拍**（不依赖起库，可重跑）：迁移 `V0387`+`V0401` 解析出 41 行菜单
   （id / title / menu_type / sort / 父级 / path），与补丁里 AGENTS.md 的表格逐格比对 = 0 不符；
   权限码集合与 `scripts/verify/hr/hr_perm_expect.py` 的种子集合完全相等（40/40）；
   后端 `business/hr/` 里 `#[nik::(get|post|put|delete)]` 实际注册 50 条（49 业务 + `/health`），
   表格中提到的接口与之一一对应，未落地的只有 `POST /employee/import`、`GET /employee/export`（文中已标）。

## AGENTS-hr50-count.diff（小修，与上一份独立）

**用途**：HR-50 新人走查（`t_a6978b4d`）发现 `AGENTS.md` 有两处 `ROUTE_CONTRACT` 行数过期（写 50，实为 51 ——
`HR-47` 把 `GET /employee/import-template` 登记进契约后没回写）。两处都在「人力资源模块（HR）口径」小节内：

| 位置 | 现在 | 应为 |
|---|---|---|
| 「30 个有真路由」条 | `ROUTE_CONTRACT` 编译期契约，50 行 | 51 行 |
| 「设计文档自身 3 处口径偏差」① | `ROUTE_CONTRACT` 50 = 46 + HR-19 补 3 + HR-41 补 `/leave/my` | 51 = 上述 + HR-47 补 `/employee/import-template` |

**应用**（`AGENTS.md` 是受保护文件，需要人工批准）：

```bash
cd /opt/cloudpivot
git apply --check docs/patches/AGENTS-hr50-count.diff   # 已试，APPLY_CHECK_OK
git apply         docs/patches/AGENTS-hr50-count.diff
git add AGENTS.md && git commit -m "docs(agents): ROUTE_CONTRACT 50→51（HR-47 登记 import-template 后未回写）"
```

**判据**：`grep -c "RouteSpec {" backend/src/business/hr/handler.rs` 首行常量 `[RouteSpec; 51]`；
已注册路由 50 条 = 49 业务 + `/health`（`grep -h "#\[nik::(get|post|put|delete)" backend/src/business/hr/*.rs | wc -l` 扣掉
`#[cfg(test)]` 之后的用例路由）。走查记录见 `docs/evidence/2026-11-13-hr50-newcomer-walkthrough/`。
