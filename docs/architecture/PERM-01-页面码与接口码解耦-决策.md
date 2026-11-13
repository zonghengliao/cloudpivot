# PERM-01 页面显形码与全量接口码解耦（决策 + 试点记录）

- **卡**：`t_0af7ec46`（平台级根治项；前置卡 `t_be54535c` 的"选项 3"）
- **状态**：决策落地 + **人力资源模块试点**完成（其余模块按 §6 逐模块推进）
- **日期**：2026-11-13
- **迁移**：`backend/migrations/business/V0401__hr_page_code_split.{up,down}.sql`
- **相关**：`docs/编码规范.md` §1.7.2、`docs/architecture/ARCH-9-人力资源模块设计.md` §3.2/§6.1

---

## 1 问题（根因，不是症状）

`sys_menu` 只有一列 `auth_code`，而**页面行（`menu_type=2`）的那一列同时被当成「全量列表接口」的码**。
员工自助按设计必须拿到页面才能让「我的 xx」tab 显形（ARCH-9 §6.1），于是：

```
授「页面」(期望：进得来)  ⇒ 用户权限集合里出现 salary:list  ⇒ 命中 GET /salary/page 的路由闸 ⇒ 看到全公司工资单
```

`t_be54535c` 用两个**handler 人肉闸**把症状关住了（`/salary/page` 缺 `salary:detail` → 403；
`/leave/page` 缺 `leave:detail` → 强制 `mine=true`）。但那是"每一处都要记得写"的兜底：
HR 员工/考勤/薪资项的 page 接口当时没踩，ERP / 成本 / 佣金 / 系统模块**根本没审计**。

## 2 决策 1：码形态用 `{模块}:{资源}:page`

| 候选 | 结论 | 理由 |
|---|---|---|
| `{模块}:{资源}:page` | **采用** | 与前端概念一一对应（`router` 的 `meta.permission`、侧边栏显隐）；与既有 `*:list / *:detail / *:create` 同构（`N15` 形态校验直接通过）；含义是"这一页"。 |
| `{模块}:{资源}:open` | 否决 | `open` 在本仓已被用作**状态动作**：`business:erp:period:open`（开账）。同名后缀承载两种语义 = 下一个人必然误读。 |

命名副作用（接受并写进口径）：`business:hr:salary:page`（页面码）与接口路径 `/business/hr/salary/page`
字面相近但**不是一回事** —— 前者"能不能打开这一页"，后者"全量薪资单"。反过来 `:open` 的歧义更贵，
所以选 `:page` 并在 §1.7.2 写死"页面码不参与任何后端判权"。

## 3 决策 2：迁移策略（授权不丢、可回放、可回滚）

**三段式，一个迁移里做完**（`V0401`），顺序不能换：

1. **页面行原地改名** `*:list → *:page`（`UPDATE … WHERE id = … AND menu_type = 2 AND auth_code = '<旧码>'`）。
   `sys_role_menu` 挂的是 `menu_id` ⇒ **已授的授权一行不丢**，侧边栏不会整片消失。
2. **`*:list` 挪到新按钮行**（`menu_type=3`，`is_visible=FALSE`，挂在对应页面下）。
   接口码必须仍有一条 `sys_menu` 行承载：`N10`（注解 → 必须有菜单）与"角色能勾授权"都依赖它。
3. **按设计意图逐角色补 `*:list`**（同一迁移的 `(role_code, auth_code) VALUES` 矩阵）。
   旧库里 `*:list` 是随页面码白来的，改名后必须显式回答"谁**该**看全量"：

   | 角色 | 补的 `*:list` | 理由 |
   |---|---|---|
   | `SUPER_ADMIN` | 5 个全给 | 超管本可短路，显式授予只为角色树上看清范围 |
   | `HR_MANAGER` | 5 个全给 | 人事经理的岗位就是维护全量档案 / 考勤 / 请假 / 薪资 |
   | `FINANCE_MANAGER` | `salary:list` | 看全公司工资单（它已有 salary 页 + detail/post/export） |
   | `DEPT_LEADER` | `employee:list` | 本来就有"员工档案页 + employee:detail"且 `data_scope=1`，不补 = 功能静默残废 |
   | `DEPT_LEADER` | **不补** `leave:list` | 刻意收窄：它的 `data_scope=1` 是"全部"，补了就能看**全公司**请假单，比解耦前更宽 |
   | `EMPLOYEE` | **一个都不补** | 这正是要关掉的越权面：员工只走 `/leave/my`、`/salary/my` |

   `DEPT_LEADER` 那两条的差别是本决策最容易被"顺手统一"抹掉的地方：**"解耦"不等于"人人都补全量"**，
   补不补取决于"这个人该不该看全量"以及"数据范围能不能兜住"。

**不做别名（关键）**：`shared::permission_alias` 是**双向**互扩（旧码 ⇄ 规范码）。
给 `page`/`list` 建别名 = 又把两个码绑回一起，本卡的根因原样复活。所以本迁移**不登记别名**，
代价与兜底写在 §5。

**幂等 / 回滚**：改名按"旧码精确匹配"（重放 0 行）；按钮 `INSERT … SELECT … NOT EXISTS` +
`ON CONFLICT (id) DO NOTHING`；授权 `ON CONFLICT (role_id, menu_id) DO NOTHING`；
页面行被客户删过的那一组整组跳过（不建孤儿按钮）。`down` 反向三步并在末尾自检。

**空库首启的唯一差异（已实测，无害）**：`SUPER_ADMIN` 由可重复种子 `S__role_data.sql` 创建，
种子在**所有版本化迁移之后**执行 ⇒ 空库上 V0401 给它的 5 行授权被 `JOIN sys_role` 静默跳过。
**故意不在种子里补**（种子重放会把管理员在权限页撤销的授予加回来 —— V0258 §二·方案 C 的教训）。
功能无影响：超管权限是 `*:*:*`、菜单按 `menu_type IN (1,2)` 全量下发，不读 `sys_role_menu`。
老库（生产 144）该角色早已存在，5 行照常落库。

## 4 决策 3：前端判据

- `router/index.ts` 的 `meta.permission` → `*:page`（只判"进得来"）；
- 页内"能不能看全量 / 能不能看别人的单" → `*:list` / `*:detail`；
- **自助入口另走 `/xx/my`**：`/leave/my`（登录即可）、`/salary/my`、`/attendance/my`。
  没有 `leave:list` 的人「我的请假」tab 直接调 `/leave/my`（`fetchMyLeaves`），不再指望
  "`/leave/page` 帮我收窄"。判据与 tab 显隐用同一个开关（`canListAll`），不会出现"tab 显形但接口 403"。

## 5 决策 4：过渡期（新旧码并存，但不互扩）

| 期间 | `sys_menu` | 后端接口 | 前端 |
|---|---|---|---|
| 迁移后 | 页面行 `*:page`（唯一），`*:list` 只在按钮行 | 继续认 `*:list`（未改任何路由注解） | meta 判 `*:page` |
| 其余模块迁移完 | 同上，逐模块一条迁移 | 同上 | 同上 |
| 收口迁移（最后一条） | 删除"旧页面码"历史残留（若有） | 不变 | 不变 |

**在途 JWT 的窗口**：迁移改的是库，令牌是**登录那一刻的快照**。EMPLOYEE 的旧令牌里仍带
`*:list`，最长持续一个 access token TTL（`NIK_MIDDLEWARE_AUTH_JWT_ACCESS_TTL`，生产 2h）。
这期间它仍能调 `/xx/page` 看到旧可见面 —— **这是已知且可接受的**：
不建别名就是为了让窗口自然过期，而不是永久互相放行。要立刻收口只能强制全员重登（本卡不做）。

## 6 推进顺序（每模块一条迁移，可独立发布）

1. **HR（本卡试点，已完成）**：5 个页面行 + 5 个 `:list` 按钮 + 6 条授权；删除两处人肉闸；
   前端 4 条路由 meta + 请假页 my-tab 改造。
2. ERP（`business:erp:*`）→ 3. 成本 → 4. 佣金 → 5. 系统模块（`system:*`）。
   每个模块：`V04xx__<module>_page_code_split.{up,down}.sql` + 前端 meta + 删除该模块的过渡闸
   + 该模块的权限矩阵脚本/表同步。
3. 最后一条清理迁移：确认没有页面行再挂接口码（`SELECT id, auth_code FROM sys_menu WHERE menu_type=2 AND auth_code NOT LIKE '%:page'` 应为空）。

**审计口径（给下一个模块用）**：

```sql
-- 还有哪些页面行在兼作接口码？（每行都要么改 :page，要么在 ADR 里写明"就是接口码"）
SELECT m.id, m.name, m.path, m.auth_code
  FROM sys_menu m
 WHERE m.deleted_at IS NULL AND m.menu_type = 2 AND m.auth_code <> ''
   AND m.auth_code NOT LIKE '%:page'
 ORDER BY m.path;
```

## 7 试点结果（HR，2026-11-13 实测）

证据目录：`docs/evidence/2026-11-13-t_0af7ec46-page-code-split/`（每条结论后面写明是哪份原始输出）。

- **迁移（空库首启）**：在 145 建空库 → 起实例回放 **323 个版本化迁移 + 12 个可重复种子**（`schema_migrations` 335 行，
  含同工作区并行卡片的 `V0402`/`V0403`）→ `/health` 200。回放后 `sys_menu` 里 `business:hr:*` = **40 个码**
  = 5 `:page` + 30 业务按钮 + 5 `:list` 按钮；`V0401` 自检四条（页面行不再是 `:list` / 页面码与按钮一一对应 /
  按钮父页面正确 / EMPLOYEE 零 `:list`）全过。（`01_migration_and_http.out`）
- **真 HTTP（同一空库、独立实例 127.0.0.1:18148）**：EMPLOYEE 的 `/auth/me` 只有 `leave:page` + `salary:page` +
  `leave:create` + `salary:my`（**0 个 `*:list`**）；`GET /leave/page`、`/salary/page`、`/employee/page` 一律
  **403 且报文点名缺哪个码**；`GET /leave/my` **200 且只出本人 1 条**、`/salary/my` 200；HR_MANAGER
  `/leave/page`=200（2 条，含同事）、`/salary/page`=200、`/employee/page`=200（3 条）—— 解耦没有把经理一起收窄；
  未登录 401。两处人肉闸删除后这些断言全部由**框架路由闸**给出。（`01_migration_and_http.out`）
- **矩阵回归**：`hr_permission_matrix.sh --fresh`（新空库 + 真实例）**597 PASS / 0 FAIL / 0 KNOWN**，
  40 码 × 6 角色 = 240 格授权全一致、186 次真实路由闸判定。（`03_matrix_final_run.out`）
- **幂等 / 回滚**：`V0401.up` 连跑 3 次菜单指纹不变（`b93f8bca…`）；`down` 后 5 个页面行回到 `*:list`、按钮行 30 个；
  再 `up` 后指纹与之前一致、EMPLOYEE 的 `*:list` 仍为 0。唯一差异是**空库首启时 `SUPER_ADMIN` 尚未存在**
  （§3 已写明），重放会补上它的 5 行授权 —— 这是幂等语义的一部分，不是漂移。（`04_idempotency_rollback.out`）
- **门禁**：`permission_audit_tests`（N14–N21）+ `system::menu::tests` 真库档（**N10** 注解↔菜单、**N08** 路由齐全）
  + `menu_invariants.sh` 8 条 + 前端 `test:unit` 1745 条全绿。（`05_cargo_gates.out` / `06_menu_invariants.out` / `07_frontend_unit.out`）
- **一处与解耦无关的既有抖动**：`system::menu::tests::n11_user_menus_filtered_by_role` 偶发 `128 vs 127`
  （超管可见菜单数 vs 库内计数）。本次实测：该用例在**解耦态**与**回滚成耦合态**的两轮里各红过 1 次，
  解耦态的另外 4 次运行 14/14 全绿 ⇒ 抖动是既有的（源码注释里早记录过同类），不是 `V0401` 引入的，
  留在 known_open 交后续卡收口。（`05_cargo_gates.out`）

## 8 回滚点

| 回滚到 | 操作 | 必须同时做 |
|---|---|---|
| 解耦前 | `V0401.down.sql`（撤授权 → 删按钮 → 页面码改回 `*:list`） | **恢复两处 handler 人肉闸**（`ensure_full_list` / `scope_to_self_without_detail`），否则员工立刻能拉全公司工资单 |
| 前端 | 路由 `meta.permission` 改回 `*:list`、请假页 my-tab 改回 `/leave/page?mine=true` | 同上 |

`down` 之后 `EMPLOYEE` 仍持有页面授权 ⇒ 老耦合复活，这是回滚的代价，不是 bug。
