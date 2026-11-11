# HR-04 · 迁移 V0387（36 菜单 + HR_MANAGER 角色 + 授权）实测证据

卡：`t_248d6edc`（HR-04【HR】迁移 V0381）
设计：`docs/architecture/ARCH-9-人力资源模块设计.md` §3.2 ④⑤ / §5.2 / §6.1 / §7.1 / §8.1

## 一句话结论

`V0387__hr_menu_perms` 在**全新空库**上由迁移加载器应用成功（`应用 1 个迁移`，`applied_seq=318`，
6 条语句 = 2 段菜单 INSERT + 1 段角色 INSERT + 3 个 `DO` 块），种下 **1 目录 + 5 页面 + 30 按钮 = 36 行**
菜单与 `HR_MANAGER` 角色 + 87 条授权；`up×2` / `down×2` 幂等；7 条护栏负向反证全红；
`scripts/verify/menu_invariants.sh` **8 条断言全绿**；二次启动 `应用 0 个迁移` + `/health` 200。

## 环境

| 项 | 值 |
|---|---|
| 库 | `hr0387_t1`（`su - postgres` 新建，owner `alun`，空库首启） |
| 后端 | 127.0.0.1:18387（`NIK_DATABASE_NAME=hr0387_t1`，三把密钥显式注入） |
| 迁移数 | 317（V0387 之前）→ 318（含 V0387） |
| 前端图标库 | `frontend/node_modules/tdesign-icons-vue-next/esm/components`（2356 个名字） |

## 文件

| 文件 | 内容 |
|---|---|
| `01-guards-drill.log` | 14 条断言：幂等 up / down 三道护栏 / up 三道前置护栏（含"拒绝后库未被改动"） |
| `02-menu-invariants.log` | `menu_invariants.sh` 8 条断言全绿（①–⑧，含 ⑤ 菜单总数 495 ∈ [400,520]） |
| `03-db-snapshot.txt` | 36 行菜单逐行、HR_MANAGER 角色、按角色授权条数、孤儿=0、`schema_migrations` 行、加载器日志 |

## 关键数字

| 角色 | 授权条数 | 目录 | 页面 | 按钮 |
|---|--:|--:|--:|--:|
| SUPER_ADMIN | 36 | 1 | 5 | 30 |
| HR_MANAGER | 36 | 1 | 5 | 30 |
| FINANCE_MANAGER | 5 | 1 | 1 | 3（salary:detail/post/export，**不含红冲**） |
| DEPT_LEADER | 5 | 1 | 2 | 2（employee:detail / leave:detail） |
| EMPLOYEE | 5 | 1 | 2 | 2（leave:create / salary:my，自助） |

孤儿检查（按钮授了但父页面没授）= **0**；页面授了但祖先目录没授 = **0**。

## 7 条负向反证（`01-guards-drill.log` 里逐条 PASS）

1. down：目录下多出别人的子菜单 → `拒绝执行`（报文点名路径与 id），**且库未被改动**；
2. down：按钮下挂子节点 → `拒绝执行`；
3. down：HR_MANAGER 被客户多授了别的菜单 → 只删本迁移 36 条授权、**保留角色**，客户的 dashboard 授权没被连坐；
4. down 再执行一遍（已空）→ `已删除 0 条授权、0 条菜单`（幂等）；
5. up：`/business/hr/leave` 被别的 id 占着 → `拒绝执行`；
6. up：`business:hr:salary:post` 被别的 id 占码 → `拒绝执行`；
7. up：顶级 `sort_order=77` 被别人占位 → `拒绝执行`。

## 两个必须知道的纠偏（真库实测，不是"看着像冲突"）

设计阶段的证据脚本 `docs/evidence/2026-11-10-arch9-2-hr-ddl/02_alter_tables_check.sql §④` 里两段 UUID
**今天已被占用**，照抄会出事故：

| 证据脚本写的 | 实际占用者 | 后果 | 本迁移改用 |
|---|---|---|---|
| 菜单 `…-000000001600` 段 | `business:erp:report:payable-aging`（应付账龄分析） | 首行 `ON CONFLICT (id) DO NOTHING` 静默不插，36 行变 35 行还挂在别人 id 上 | `…-00000000e0xx` 段（实测 0 行占用） |
| 角色 `…-0000000f0008` | `SALES_CLERK`（销售员） | `HR_MANAGER` 的 INSERT 撞 `sys_role` 主键 → 迁移直接失败 | `…-0000000f0016`（f0001~f0009/f0015 已占，f0016 空闲） |

另外：`sys_role.role_code` **没有唯一约束**，所以角色幂等只能靠 `NOT EXISTS (role_code='HR_MANAGER')`。

## 连带改动（本卡范围内必须做，已在卡内汇报里说明）

1. `scripts/verify/menu_invariants.sh` ⑤ 上限 **460 → 520**：实测 459（含成本 26 条）+ 36 = **495**，
   460 会红。这是脚本头注释里写明的合法调整路径（"再破线时先确认真实功能增长，再改这里和 README"）；
   `scripts/verify/README.md` 同步。
2. `backend/src/system/menu/permission_audit_tests.rs` 新增 `PENDING_LANDING` 临时台账（30 条，每条写明
   由哪张卡落地）。理由：本卡是批 1「菜单先种」，handler 要到 HR-12/13/15/19/20/21/23/25/30/31/36/41/42
   才写；不登记，N14 会判 30 个死码、L2 连库闸整片红。N18 已扩展为**校验这本台账也是活的**
   （码不在菜单里 / 源码已出现引用 / 没写落地卡 → 红），落地即自动逼着删条目。
