# 待应用的文档补丁

这里放**已写好、但需要人确认才能落地**的文档改动（`git apply` 一次即可）。

> `AGENTS.md` 是**受保护文件**：Agent 会话里写它需要人工批准，看板 worker 跑在无人值守环境
> 拿不到批准（批准超时＝拒绝，且禁止改走 terminal / execute_code 绕道），故这类改动一律落成
> 补丁留在仓库里，等人一条命令应用。**这是唯一未走自动化的一步，其余文档改动都已经直接提交。**

## 现状（2026-11-13 收口）

| 补丁 | 状态 |
|---|---|
| `AGENTS-hr50.diff` | ✅ **已应用**（`8e299227`，`laoliu` 于 14:51→15:18 应用，文件已随该提交删除） |
| `AGENTS-hr50-count.diff` | ⏳ **待应用**（2 处计数：`ROUTE_CONTRACT` 50 → 51） |

### `AGENTS-hr50.diff` 已落地了什么（留档，便于回溯）

`AGENTS.md` 菜单/权限口径回写 —— 既覆盖 HR-50（`t_4d9117bb`）的六项基线回写，也覆盖
`t_2721945d` 要求的**完整菜单树 + 40 个权限码逐条表**；应用后 `AGENTS.md` 由 99 行 → 219 行。

| # | 位置 | 改动 |
|---|---|---|
| 1 | 目录 | 补 `backend/src/business/`（原文只列 `system/ support/`，与实际 20+ 模块不符） |
| 2 | 常用命令 | 补 3 条 HR 验收入口（上线演练 / 薪资链路 E2E / 数据巡检） |
| 3 | 约定 6 迁移号段 | 「基础版最大 `V0017`」→ 实测已到 `V0403` + 跨目录全局唯一 + 先落地者定号 |
| 4 | 约定 12 菜单 | `menu_invariants.sh` 从「6 条断言」→ 实际 **8 条**并逐条命名 |
| 5 | 约定 14（新增） | **页面显形码 `*:page` 与全量接口码 `*:list` 解耦**（HR 试点基线，指向编码规范 §1.7.2 与 PERM-01） |
| 6 | 新增小节「人力资源模块（HR）口径」 | ①实测基数；②**菜单树 = 权限码表**（41 行菜单 / 40 个码逐行：type / path / sort / auth_code / 承载接口）；③分组计数表（5 页面码 + 35 按钮码 = 40）；④40 个码的四种状态；⑤角色矩阵；⑥与 ARCH-9 §5/§6 的交叉自查；⑦错误码段 / 数据范围 / 门禁验收 / 演练库 |

新增小节里的数字全部可与仓内真源对拍（脚本可重跑）：

- 41 行菜单 = 1 目录（`/business/hr`，sort 77，`Layout`，`user-business`）+ 5 页面（`*:page`）+ 35 按钮，
  层级 / `title` / `sort` / 父级与迁移 `V0387` + `V0401` 逐格一致（对拍 0 不符）。
- 40 个权限码 = 5 `*:page` + 35 按钮码（含 5 个 `*:list`）；期望值由
  `python3 scripts/verify/hr/hr_perm_expect.py` 解析迁移得出（HR_MANAGER 40 / FINANCE_MANAGER 5 /
  DEPT_LEADER 5 / EMPLOYEE 4 / FINANCE_DIRECTOR 0），不手抄。
- 后端 `business/hr/` 里 `#[nik::(get|post|put|delete)]` 实际注册 **50 条**（49 业务 + `/health`），
  与表格「承载接口」列一一对应；未落地的只有 `POST /employee/import`、`GET /employee/export`（表中已标）。
- 交叉自查：ARCH-9 §5.2 的 32 个权限码 → 32/32 有落点；§6.1 的 6 个 path → 6/6 一致。

## AGENTS-hr50-count.diff（待应用）

**用途**：HR-50 新人走查（`t_a6978b4d`）发现 `AGENTS.md` 有两处 `ROUTE_CONTRACT` 行数过期
（写 50，实为 51 —— `HR-47` 把 `GET /employee/import-template` 登记进契约后没回写）。两处都在
「人力资源模块（HR）口径」小节内：

| 位置 | 现在 | 应为 |
|---|---|---|
| 「30 个有真路由」条 | `ROUTE_CONTRACT` 编译期契约，50 行 | **51 行** |
| 「设计文档自身 3 处口径偏差」① | `ROUTE_CONTRACT` 50 = 46 + HR-19 补 3 + HR-41 补 `/leave/my` | **51** = 上述 + HR-47 补 `/employee/import-template` |

**应用**（`AGENTS.md` 是受保护文件，需要人工批准）：

```bash
cd /opt/cloudpivot
git apply --check docs/patches/AGENTS-hr50-count.diff   # 已试，APPLY_CHECK_OK
git apply         docs/patches/AGENTS-hr50-count.diff
git add AGENTS.md && git commit -m "docs(agents): ROUTE_CONTRACT 50→51（HR-47 登记 import-template 后未回写）"
rm docs/patches/AGENTS-hr50-count.diff   # 落地后删补丁与本节
```

**判据**（三条都能自己跑，别信文档）：

```bash
# ① 契约行数 = 51
grep -n "pub const ROUTE_CONTRACT" backend/src/business/hr/handler.rs      # → [RouteSpec; 51]
# ② 已注册路由 = 50（49 业务 + /health）
grep -h '^#\[nik::\(get\|post\|put\|delete\)' backend/src/business/hr/*.rs | wc -l   # → 50
# ③ docs/README.md 与二开指南 §17.9.4 已是 51（只有 AGENTS.md 落后）
grep -n "ROUTE_CONTRACT" docs/README.md docs/后端二开指南.md
```

走查记录见 `docs/evidence/2026-11-13-hr50-newcomer-walkthrough/`（三份文档各被实际使用一次，
E1 即本条计数问题）。
