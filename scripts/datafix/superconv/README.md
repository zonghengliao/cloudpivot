# SUPERCONV — 超管收敛数据修复（【P0-FIX-2B-C】`t_db2506d2`）

> 走 `docs/architecture/数据修复与清场SOP.md` 的固定顺序：
> 只读复现 → 行级快照 → 幂等 DML → 对平 → 巡检 → 留档。
> 目标态（决策 D-2）：**生产只有 `admin` 一个真超管**。

## 这张卡改什么（三个阶段，顺序不可颠倒）

| 阶段 | 脚本 | 动什么 | 为什么必须在这个顺序 |
|---|---|---|---|
| 1 | `10_grant_ops_admin.up.sql` | 建 `OPS_ADMIN` 角色 + 授权 72 行 + 绑 `laoliu`；顺带把 `GENERAL_MANAGER` 也绑给 `laoliu` | 先建后摘。摘权之后才建角色 = 有一个「他既没有超管也没有运维角色」的窗口 |
| 2 | `20_revoke_super.up.sql` | 删 3 行 `sys_user_role`：`laoliu_test` 的 `SUPER_ADMIN` + `GENERAL_MANAGER`、`laoliu` 的 `SUPER_ADMIN`；推进 2 个账号的 `token_version` | A 卡（组织落位）与 B 卡（流程去 ADMIN 化）必须先 done，否则摘权会静默改写审批人集合/让他只看自己 |
| 3 | `probe_ops.py` / `probe_scope_c2.py` | 只读 + 一次建完即删的写 | 摘权后立刻验证「还能干活」「看不到不该看的」；任一条失败 → 回退绑定 |

## 文件

| 文件 | 作用 |
|---|---|
| `00_backup.sh` | 备份到 `/root/datafix-backup-superconv-<ts>/`（full.dump + 5 张表 + 逐行快照 + MANIFEST 含库名） |
| `10_grant_ops_admin.{up,down}.sql` | 阶段 1 / 撤销阶段 1（删角色，物理删：down→up 要能往返） |
| `20_revoke_super.{up,down}.sql` | 阶段 2 / 按快照把 3 行绑定加回去 |
| `apply.sh` | `--check` / `--grant` / `--revoke` / `--probe` / `--rollback-revoke` / `--rollback-grant` / `--rollback` |
| `probe_ops.py` | C3 运维动作清单（6 个读 + 1 个写 + 1 个反向 403） |
| `probe_scope_c2.py` | C2 可见行数探针（任务 / 项目 / 用户） |
| `verify_c2_scope.sh` | C2 演练（**只许隔离副本**）：before/after + 子树因果反向对照 |
| `verify_drill.sh` | SOP §9 四步演练（**只许隔离副本**）：up → up（幂等）→ down → up |
| `proof_rows.sql` | 留档用只读核对（逐账号绑定 / 超管成员 / 快照 / 角色 remark / D13 待办） |

## 怎么跑

```bash
# 生产（144）
cd /opt/cloudpivot
bash scripts/datafix/superconv/00_backup.sh                 # 0) 备份，确认 full.dump 非空
bash scripts/datafix/superconv/apply.sh --check             # 1) 只读现状
bash scripts/datafix/superconv/apply.sh --grant             # 2) 阶段 1
python3 scripts/datafix/superconv/probe_ops.py --token ...  # 3) 阶段 3a：运维清单（摘权前后各一次）
bash scripts/datafix/superconv/apply.sh --revoke            # 4) 阶段 2
python3 scripts/datafix/superconv/probe_ops.py --token ...  # 5) 阶段 3b：再跑一遍清单
INCR perm:user_scope_epoch；DEL perm:user_perms:{uid} / perm:user_roles:{uid}   # 6) 见下
bash scripts/datafix/superconv/apply.sh --rollback-revoke   # 7) 回滚演练（C6）后再 --revoke
```

## 缓存失效（**直改库不走 app 写点，这一步不能省**）

```bash
redis-cli -n 0 INCR perm:user_scope_epoch              # 数据范围缓存（300s TTL，卡面要求）
redis-cli -n 0 DEL "perm:user_perms:<uid>" "perm:user_roles:<uid>"   # 权限码 / 角色缓存（P1-FIX-3）
```

`<uid>` 从 `SELECT id FROM sys_user WHERE username IN ('laoliu','laoliu_test')` 取。
另外 `20_` 会推进两个账号的 `sys_user.token_version` —— 路由闸只认**令牌里的权限码快照**，
不推进版本号的话，他手上已签发的令牌在有效期内仍然带着 274 个码（app 自己改角色授权时也是这么做的，
见 `system/user/model.rs::bump_token_version_of_role_holders_in`）。

## 与卡面的一处口径修正（**必须知道**）

卡面写「从 `SUPER_ADMIN` 的 274 个码里取 `system/*` 与 `support/*`（用户/角色/菜单/部门/字典/配置/…）」。
**按字面取会得到一个不可用的角色**：生产实测 `SUPER_ADMIN` 名下只有 30 行 `sys_role_menu`
（`change-log` / `field-mask` / `help` / `preference` / `role-mutex` / `dashboard` / `invariant`），
`system:user:*`、`system:role:*`、`system:dept:*`、`system:menu:*`、`support:dict:*`、`support:config:*`、
`support:file:*`、`support:log:*`、`support:online:*`、`support:job:*` **一行都没有** ——
因为 `SUPER_ADMIN` 走的是通配短路（`TokenClaims::is_super_admin()`），不靠授权行。

所以本批次按卡面的**语义**取 `sys_menu` 全量的 `system:*` + `support:*`：
70 行带码菜单（42 个 system: 码 + 23 个 support: 码 = 65 个 distinct 码）+ `system`/`support` 两个目录节点 = **72 行**。
少了目录节点，前端侧边栏树是孤儿（`auth/model.rs::load_user_menus_uncached` 不做父级自动补全）。

## 边界（本批次明确不碰）

- 不删号、不停用、不动 `expire_time`；
- 不动 `qa.*` / `verify-ai` / `cl_43870`；
- 不动流程定义（B 卡已做）、不回填 `biz_task` / `biz_project` 的历史 `dept_id`（D-3）；
- `henry.liao` 原样保留，直到老大回答「内部业务同学 / 客户演示号」（那一支另写 `30_henry_*.sql`）；
- 摘权**不回收**已生成的待办（D13：`biz_flow_task.assignee_id` 是 user_id，不是角色）。

## 回滚

```bash
bash scripts/datafix/superconv/apply.sh --rollback          # 阶段 2 → 阶段 1 依次退
# 兜底：pg_restore -d cloudpivot <backup>/full.dump
```
