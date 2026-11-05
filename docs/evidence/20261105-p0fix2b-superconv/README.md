# P0-FIX-2B-C · 超管收敛（`t_db2506d2`）

> 目标态（决策 D-2）：生产只有 `admin` 一个真超管。
> 本目录 = **隔离副本演练全部结论 + 生产执行证据**。
> 生产侧写库在获得老大两句书面确认后执行（`docs/待决策.md` 铁律）。

## 0. 依赖与顺序（已满足）

| 依赖 | 卡 | 状态 | 证据 |
|---|---|---|---|
| A · 组织落位（`laoliu_test` → TECH） | `t_db82c38e` | done（15:58 生产已 apply） | `docs/evidence/20261105-p0fix2b-orglanding/prod/` |
| B · 流程去 ADMIN 化（`assignee_type=ADMIN` 5→0） | `t_1e6a8431` | done（15:52 生产已 apply） | `docs/evidence/20261105-p0fix2b-deadmin/` |

顺序不可颠倒（决策 D-4）：A 没做 ⇒ 摘权者「3 档角色 + 无部门」会被 `merge_scope_rows` 降级 `SelfOnly`（比不摘更糟）；
B 没做 ⇒ 摘一个超管 = 同时改 5 个审批节点的审批人集合。

## 1. 隔离副本（99 · `cloudpivot_superconv_c1` · prod 16:04 dump · 实例 127.0.0.1:18097）

### C3 · 运维动作清单（摘超管**之后**，真实登录 laoliu）→ `iso/c3_ops_probe_laoliu.txt`

14/14 PASS：`/system/user` `/system/role` `/system/dept` `/system/menu` `/support/config` `/support/log`
六个读接口 + 侧边栏菜单树 + **1 次写操作**（新建系统参数 → 删除，净零）+ 角色声明（含 `OPS_ADMIN`、不含 `SUPER_ADMIN`）
+ 用户总数 `total=9`（与 `admin` 一致，证明 `data_scope=1` 让运维看得到全量 `sys_user`）
+ 反向断言 `GET /api/business/erp/account/page` → **403**（业务边界靠权限码收，不靠行级范围收）。

附带发现（写进卡面）：摘超管后 `laoliu` 的侧边栏只剩
`工作台 / system / support / 审批流 / 帮助中心 / 外观设置` —— 业务页面（采购/仓库/财务/基础数据）全部不可见，
因为 `OPS_ADMIN` 按卡面口径不含 `business:*`，而 `GENERAL_MANAGER` 只给审批面。需要保留哪些业务页，等老大点名再加码。

### C2 · `laoliu_test` 的数据范围（真机 before/after + 子树因果）→ `iso/c2_*.json`

| 步骤 | roles | `/task/page` | `/project/page` | `/system/user/page` |
|---|---|---|---|---|
| before（超管） | SUPER_ADMIN + 4 业务角色 | **200 / total=71** | **200 / total=3** | **200 / total=9** |
| after（摘超管 + 摘 GENERAL_MANAGER） | DEPT_LEADER + PURCHASE_MANAGER + WAREHOUSE_MANAGER | **403** | **403** | **403** |

他本身没有 `business:task:read` / `business:project:read`（今天靠超管通配才看得到），所以摘权后连接口都进不去。
为了把「TECH 子树过滤」这件事单独验出来，副本里临时给他加了一个 `data_scope=3` 的探针角色（只含两个 read 码，用完即删）：

| 反向对照（任务 `d5b96f9b…` 的 `dept_id`） | `/task/page` total |
|---|---|
| `NULL`（生产历史现状：846 条任务 `dept_id` 全 NULL） | 0 |
| `TECH` | **1** |
| `FIN` | **0** |

⇒ 子树过滤因果成立（改 `dept_id` 立刻可见/不可见），且**历史任务 `dept_id` 全 NULL ⇒ 摘权后他可见任务 71→0**
（D-3 明令不回填，这是必然结果，不是 bug）。探针角色与 `dept_id` 已还原（`biz_task.dept_id` 非空 = 0）。

### 其它（`iso/impact_*.json`、`iso/drill_four_steps.txt`、`iso/proof_rows_after.txt`）

- **影响面逐账号对照**（`org_landing_impact.py`，与 `principal.rs::merge_scope_rows` 同口径）：
  只有 `laoliu_test` 变化 `All → DeptAndChild(TECH)`，其余 8 个账号（含 `laoliu` = `All`，靠 `GENERAL_MANAGER`）**逐格零变化**。
- **C6 可逆**：`20_revoke_super.down.sql` 把 3 行绑定加回 → `laoliu_test` 范围回到 `All`（`impact_rollback.json`），再摘回去又变 `DeptAndChild`。
- **SOP §9 四步演练**：`up → up（幂等，0 行变更）→ down（3 行回来）→ up`，**FAIL=0**。
- **D13 冻结**：2 张 PENDING 实例的 4 条 `biz_flow_task` 逐条一致（`proof_rows_after.txt` 第 58-67 行），摘权不改它一行。

## 2. 与卡面的口径修正：`OPS_ADMIN` 的权限码不是「SUPER_ADMIN 的子集」

生产实测（16:04 dump）：`SUPER_ADMIN` 名下只有 **30 行** `sys_role_menu`（+7 个空前缀目录节点），
其中 `system:*` 26 行、`support:*` 4 行，且**全是** `change-log / field-mask / help / preference / role-mutex /
dashboard / invariant`；`system:user:*`、`system:role:*`、`system:dept:*`、`system:menu:*`、`support:dict:*`、
`support:config:*`、`support:file:*`、`support:log:*`、`support:online:*`、`support:job:*` **一行都没有**。

原因：`SUPER_ADMIN` 走「通配短路」（`TokenClaims::is_super_admin()`），**不靠授权行**。
照卡面字面「从 274 个码里取 system/support 子集」，`OPS_ADMIN` 只会拿到 30 行、零个用户管理码 ⇒
`laoliu` 摘超管当天不可运维，直接违反 C3。

故按卡面**语义**取 `sys_menu` 全量 `system:*` + `support:*`：
**70 行带码菜单（42 system 码 + 23 support 码 = 65 distinct 码）+ `system`/`support` 两个目录 = 72 行**。
（目录节点必须给：`auth/model.rs::load_user_menus_uncached` 只查显式授予行，不自动补父级。）
`data_scope=1` 的理由写进角色 `remark`（原文见 `iso/proof_rows_after.txt` 第 35-37 行）。

## 3. 生产执行（144 / `cloudpivot`）

**状态：待老大两句书面确认**（`henry.liao` 归属 + 同意摘两个超管）。执行后此处补：备份路径 / 逐账号前后对照 /
巡检 N1·N2 / `erp_invariants` / `health` / D13 待办对照 / C6 回滚演练 / 缓存失效（`INCR perm:user_scope_epoch` + `DEL perm:user_perms:*`）。
