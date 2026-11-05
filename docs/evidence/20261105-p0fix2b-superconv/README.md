# P0-FIX-2B-C · 超管收敛（`t_db2506d2`）执行证据

> 目标态（决策 D-2）：**生产只剩 `admin` 一个真超管。**
> 生产库：`172.18.10.144` / `cloudpivot`，服务 `cloudpivot`（systemd）。
> 施工窗口：**2026-11-05 16:54:10 → 16:55:42（CST）**，无重启、无中断。
> 执行副本：`144:/root/superconv-run2/scripts/`（与仓库 `scripts/datafix/superconv/` 逐字节一致，
> `rsync` 同步；证据在 `144:/root/superconv-evidence/`，已整体拷回本目录 `prod/`）。

## 0. 授权（两句话都齐了，均留痕在卡上）

| 时间 | 来源 | 内容 |
|---|---|---|
| 16:36 | `laoliu` 评论 | 用户原文「确认，同意」→ 同意摘 `laoliu_test` / `laoliu` 的超管；**并把 `laoliu` 的方案从「OPS_ADMIN 单角色」改成「5 个业务角色」** |
| 16:45 | `laoliu` 评论 | 用户原文「henry.liao 是我建的自己的测试号。可以调整」→ `henry.liao` 摘超管、保留 4 个业务角色 |

第一条同时是**对卡面的口径修改**：卡面 C3 要求「`laoliu` 摘权后仍能进 `/system/*` 运维」，
用户批准的是「`laoliu` 只拿业务角色、不能再进系统管理高危页」。
⇒ 本卡按用户批准执行，**不创建 `OPS_ADMIN`**；代价与影响写在 §5。

## 1. 一句话结果

生产 4 个超管收敛到 **1 个（`admin`）**：`laoliu_test` 摘 `SUPER_ADMIN`+`GENERAL_MANAGER`（留 3 个 3 档角色，
范围 = TECH 子树）、`laoliu` 摘 `SUPER_ADMIN`（补 5 个业务角色）、`henry.liao` 落位 HQ + 摘 `SUPER_ADMIN`（留 4 个）；
顺带补了 **9 行缺失的祖先目录授权**（不补则摘权后侧边栏是空的）；`erp_invariants` **66/66 BLOCKER 0 / WARN 0**、
`health=200`、2 张 PENDING 待办的 4 行一行未变、回滚往返实测可逆。

## 2. 目标态对照（`prod/11_rows_before.txt` → `prod/33_final_rows.txt`）

| 账号 | 之前 | 之后 | 部门 |
|---|---|---|---|
| `admin` | `SUPER_ADMIN` | **`SUPER_ADMIN`（唯一）** | HQ |
| `laoliu` | `SUPER_ADMIN` | `DEPT_LEADER` `FINANCE_MANAGER` `WAREHOUSE_MANAGER` `PURCHASE_MANAGER` `GENERAL_MANAGER` | HQ |
| `laoliu_test` | `SUPER_ADMIN` `GENERAL_MANAGER` + 3 个 3 档 | `DEPT_LEADER` `PURCHASE_MANAGER` `WAREHOUSE_MANAGER` | TECH |
| `henry.liao` | `SUPER_ADMIN` + 4 业务角色 | `DEPT_LEADER` `GENERAL_MANAGER` `PURCHASE_MANAGER` `WAREHOUSE_MANAGER` | **NULL → HQ** |

计数：`SUPER_ADMIN` 成员 **4 → 1**；`sys_user_role` **16 → 17**（−4 超管绑定 +5 业务绑定）；
`sys_role_menu` **594 → 603**（+9 祖先目录）；`sys_user` 26 行、`biz_task.dept_id` 非空 0、
`biz_project.dept_id` 非空 0、`sys_dept.leader_id` 非空 0 **全部未变**；三个账号 `status`/`expire_time` 未动
（`laoliu_test` 仍是 `status=0`）。

## 3. 与卡面/口述的三处偏差（都是实测结论，不是偷懒）

### ① 不建 `OPS_ADMIN`（C3 的形态被用户批准方案取代）
用户 16:36 的方案明确要求 `laoliu`「不能再访问 SUPER_ADMIN 专属功能（系统设置里的高危项）」。
若按卡面建 `OPS_ADMIN` 并授予他，他会保留 `/system/user`、`/system/role`、`/system/menu`
（= 能给自己或别人重新发超管），与用户要求直接冲突。
⇒ **不建**。原「OPS_ADMIN 版」脚本已从仓库移除（历史版本在提交 `cc80bc0` 里可查）。
代价：生产运维能力只剩 `admin`（见 §5 风险 1）。

### ② 「可见业务菜单 ≥ 50」实测是 **12 个页面**
老大口述的 50 是按 `sys_role_menu` **行数**（含按钮）记的：`DEPT_LEADER 46 + FINANCE_MANAGER 56 +
WAREHOUSE_MANAGER 18 + PURCHASE_MANAGER 12 + GENERAL_MANAGER 12`，去重后 = **144 行 / 49 个权限码**。
而侧边栏能出现的是 `menu_type=2` 的**页面**：5 个角色并集 = **12 个页面 + 6 个目录**
（`prod/33_final_rows.txt` 的「各账号可见业务页」段；接口侧同口径：`/api/system/auth/menus` 返回
20 个节点 = 12 页面 + 6 目录 + 2 个顶层节点）。
全平台业务页面共 55 个（`admin` 视角），这 5 个角色覆盖其中 12 个：
费用报销/待付款/费用类型、资产卡片/资产分类/折旧计提、预算编制/预算执行/预算科目映射、
盘点、发起审批、审批中心、`support:invariant`。

### ③ 新增阶段 `15_grant_menu_dirs`（生产数据缺陷，不修则等于白摘）
`load_user_menus_uncached` 对非超管只取「角色直接授予的菜单行」，`build_tree` 把
**父目录不在节点集里的页面直接丢掉**。生产实测：5 个业务角色授予的页面里，
`财务管理`（预算/费用报销/固定资产的根）、`采购管理`（盘点的根）、`support` 三个目录
**只授给了 `SUPER_ADMIN`**，另有 6 处角色 × 目录组合缺失 = **9 行**。
不补这 9 行，`laoliu`/`henry.liao`/`laoliu_test` 摘权后侧边栏一个业务页都不会出现。
本阶段只补 `menu_type=1` 目录、**不新增任何权限码**（目录 `auth_code` 为空，不参与鉴权）。

## 4. 逐条验收（C1–C6 + 用户追加项）

| # | 断言 | 结果 | 证据 |
|:--:|---|---|---|
| C1 | `SUPER_ADMIN` 成员 = 1（`admin`） | ✅ 4 → 1 | `prod/11_rows_before.txt`、`prod/33_final_rows.txt`、`prod/23_revoke.txt` |
| C2 | `laoliu_test` 生效范围 = TECH 子树 | ✅ `All → DeptAndChild(TECH)`；真机反向对照 `dept_id` NULL→0 / TECH→1 / FIN→0 | `prod/29_impact_compare.txt`、`iso/c2_subtree.txt` |
| C3 | `laoliu` 运维动作清单 | ⚠️ **形态被用户批准方案取代**（见 §3①）：实测 `laoliu` 对 `/system/user|role|dept|menu`、`/support/config|log` **全部 403**，业务页 200 | `prod/25_probe_after.txt` |
| C4 | 巡检 N1 ✅ / N2 升 BLOCKER 仍 ✅ | ✅ N1 ✅、N2 **WARN 1 → 0**（`henry.liao` 已落位 HQ） | `prod/10_invariants_before.txt`、`prod/26_invariants_after.txt` |
| C5 | `erp_invariants` BLOCKER 0 / WARN 0；health=200；D13 待办未变 | ✅ **66/66，BLOCKER 0，WARN 0**；`health=200`；4 行待办 `diff` **IDENTICAL** | `prod/34_final_invariants.txt`、`prod/33_final_rows.txt` |
| C6 | 回滚演练：加回 `SUPER_ADMIN` → 范围回 ALL | ✅ 4 个超管回归、`laoliu_test` 回 `All`；再摘权回 `DeptAndChild` | `prod/30_rollback_a.txt`、`prod/30_impact_rollback.json`、`prod/31_rollback_b.txt` |
| 追① | `SUPER_ADMIN` 账号数 = 1 | ✅ | 同上 |
| 追② | `henry.liao` 仍能登录、仍能看到业务菜单 | ✅ 4 角色 / 49 权限码 / 12 个业务页；`/system/*` 403 | `iso/probe_after_henry.txt`（真登录） |
| 追③ | `henry.liao` 进不了系统管理高危页 | ✅ `/system/user|role|dept|menu` = 403 | 同上 + `prod/25_probe_after.txt` |
| 附加 | 影响面：只允许 `laoliu_test` 变化 | ✅ 9 个账号逐格对照，仅 `laoliu_test` `All→DeptAndChild` | `prod/29_impact_compare.txt` |

## 5. 风险与已知后果（需要人看一眼）

1. **`laoliu` 不再是运维账号**：他今天对 `/system/*` 全是 403，生产运维只剩 `admin`
   （唯一超管，口令在 `144:/etc/cloudpivot/cloudpivot.env` 的 `NIK_ADMIN_PASSWORD`）。
   若老大要一个「专职运维号」，应新建账号 + 新建 `OPS_ADMIN` 角色（原脚本在 `cc80bc0`），
   不要复用 `laoliu`——那会立刻违反 16:36 批准的「不能进系统管理」。
2. **`laoliu` / `henry.liao` / `laoliu_test` 看不到「任务 / 项目」**：
   这 5 个业务角色里**没有** `business:task:read` / `business:project:read`，
   以前是超管通配盖住了。实测三者 `/api/business/task/page`、`/api/business/project/page` = **403**。
   → 已开跟进卡（业务权限码归属），本卡不动授权面（改角色授权超出批准范围）。
3. **`henry.liao` 保留 `GENERAL_MANAGER`（`data_scope=1`）**：他的数据范围仍是 All。
   这是有意的（他要「总经理审批视角」，且落位 HQ 后 3 档角色的并集本来就是 All）；
   超管收敛收的是**权限码通配**，不是数据范围。若要收窄，得改角色 `data_scope`（另一张卡）。
4. **`laoliu_test` 仍 `status=0`**（停用）：摘权不影响，他也登不进来；本卡未动其状态。
5. **`qa.*` / `verify-ai` / `cl_43870` 一行未动**；`biz_task`/`biz_project` 历史 `dept_id` 未回填（D-3）。

## 6. 缓存失效：卡面口径是错的（重要，容易再踩）

卡面/A 卡写的是 `redis-cli -n 0 INCR perm:user_scope_epoch`。**实测这条对应用是空转**：
应用侧缓存键带实例前缀 `cloudpivot:`（键形 `cloudpivot:perm:user_scope:{epoch}:{uid}`、
`cloudpivot:uv:{uid}`、`cloudpivot:perm:user_perms|user_roles|user_menus:{uid}`），
而裸名 `perm:user_scope_epoch` 是 A 卡用 redis-cli 直接写的，应用读的是
`cloudpivot:perm:user_scope_epoch` —— 证据：生产存量 scope 键全是 `...:user_scope:0:<uid>`
（应用一直读到 epoch=0）。

本卡的处置（`144:/root/superconv-run2/cache_invalidate.sh`，证据 `prod/24c_cache_final.txt`）：

```bash
redis-cli -n 0 INCR cloudpivot:perm:user_scope_epoch        # 应用真正读的 epoch（0 → 1）
redis-cli -n 0 INCR perm:user_scope_epoch                   # 裸名，历史兼容/账面对齐
redis-cli -n 0 DEL cloudpivot:uv:<uid> \
                   cloudpivot:perm:user_perms:<uid> \
                   cloudpivot:perm:user_roles:<uid> \
                   cloudpivot:perm:user_menus:<uid>          # 三个账号逐个
```

**踩坑现场**：`20_revoke_super` 推进 `token_version` 后，`cloudpivot:uv:<uid>`（60s TTL）里还是旧版本号，
自签令牌立刻 401/`20008 您的权限已变更`；删掉 `uv:` 键后全部 200。⇒ 以后「改角色/改版本号」
都必须删 `cloudpivot:uv:<uid>`，否则用户 60s 内重登也是 401。

## 7. 回滚

```bash
# 在 144:/root/superconv-run2/scripts/ 下
bash apply.sh --rollback          # 20.down（4 行超管加回）→ 10.down（摘 5 业务角色）
                                  # → 15.down（拆 9 行目录）→ 05.down（henry dept 回 NULL）
bash /root/superconv-run2/cache_invalidate.sh
# 兜底：pg_restore -d cloudpivot <backup>/full.dump
```

备份：`144:/root/datafix-backup-superconv-<ts>/full.dump`（施工前 16:54:10 一份，`prod/14_backup.txt`）。
注意 `--rollback` **只回退本卡**；A 卡（组织落位）的 `dept_id` 由 A 卡自己的 down 负责。

## 8. 文件与复跑

| 文件 | 作用 |
|---|---|
| `05_land_henry_hq.{up,down}.sql` | `henry.liao` → HQ（N2 归零的前提） |
| `15_grant_menu_dirs.{up,down}.sql` | 5 个业务角色补 9 行祖先目录（不补则侧边栏空） |
| `10_grant_laoliu_roles.{up,down}.sql` | `laoliu` 绑 5 个业务角色（摘权前先授） |
| `20_revoke_super.{up,down}.sql` | 摘 4 行超管绑定 + 推进 3 个 `token_version` |
| `apply.sh` | `--check / --grant / --revoke / --probe / --rollback-revoke / --rollback` |
| `mint_token.py` | 按库内真值自签令牌（生产口令不可猜；通配声明会被库侧复核，**伪造不出超管**） |
| `probe_conv.py` | 逐账号验收探针（角色 / 权限码 / 菜单树 / 6 个系统页 / 2 个业务页） |
| `probe_scope_c2.py` | 可见行数（任务 / 项目 / 用户） |
| `verify_drill.sh` | SOP §9 四步演练（隔离库专用）：`up→up→down→up→down` |
| `verify_c2_subtree.sh` | C2 子树因果对照（隔离库专用，末尾净零还原） |
| `proof_rows.sql` | 留档只读核对（前后各跑一次，diff 即证据） |
| `00_backup.sh` | 整库 + 5 张表 + `affected_rows.txt` + `MANIFEST.txt`（含库名校验） |

隔离副本：`99:cloudpivot_superconv_c3`（prod 16:42 dump）→ 实例 `127.0.0.1:18098`，
演练结论见 `iso/`（四步演练 **FAIL=0**、C2 **FAIL=0**、真登录探针）。
