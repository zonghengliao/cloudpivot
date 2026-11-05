# 组织落位 · **生产执行**证据（172.18.10.144 / cloudpivot）

> 卡片 `t_db82c38e`｜授权：卡面评论 `laoliu` 2026-11-05 15:57（用户原文「确认」）
> 脚本 `scripts/datafix/org_landing/`（与仓库 `d51889e` 同一份；生产执行副本 `/root/orglanding-run/`）
> 执行时刻：**2026-11-05 15:58:07 → 15:59:40（CST）**，中间无中断

## 0. 一句话结论

生产已按决策落位：`admin`/`laoliu`/`cl_43870` → **HQ**、`laoliu_test` → **TECH**；
`henry.liao`/`verify-ai`/`qa.viewer` 仍无部门，`qa.*` 与 `sys_dept.leader_id` 未动；
**9 个账号的数据可见范围前后逐格一致**（`(kind, dept_ids, self_only)` + 5 张受控表行数）；
幂等（第 2 次 0 行）、回滚往返（down → 全 NULL → up）、巡检 N2（3 → 1）、范围缓存失效已做。

## 1. 执行时序（全部可复跑，命令即卡面 §5 的顺序）

| # | 动作 | 命令 | 结果 |
|:--:|---|---|---|
| 1 | 服务健康 | `systemctl is-active` / `curl :8023/health` | `active` / `health=200` |
| 2 | 只读前置核查 | `apply.sh --check` + 现状 SQL | 4 账号全 NULL；`sys_dept.leader_id` 4/4 NULL；`sys_user_role` 16 行（`30_prod_preflight.txt`） |
| 3 | 落位前巡检 | `erp_invariants.py --group 权限` | N1 ✅ / N2 ⚠️ 点名 **3**（admin、henry.liao、laoliu）（`31_`） |
| 4 | 落位前影响面 | `org_landing_impact.py --out impact-before.json` | 9 账号快照（`32_`） |
| 5 | **备份** | `00_backup.sh` | `/root/datafix-backup-orglanding-20261105-155815`，`full.dump` **2,092,538 B**（≥100KB 门槛），`pg_restore --list` 1759 项（`33_`/`33b_`） |
| 6 | **apply** | `apply.sh` | 快照 4 行 → 「本次共变更 4 行」→ 收口复核通过（`34_`） |
| 7 | 幂等 | `verify_idempotent.sh`（连跑 2 遍） | 两遍均「本次共变更 0 行」，受影响 4 行 + 快照逐字节一致，PASS（`35_`） |
| 8 | 落位后巡检 | `erp_invariants.py --group 权限` | N1 ✅ / N2 ⚠️ 点名 **1**（henry.liao）（`38_`） |
| 9 | 影响面对照 | `org_landing_impact.py --before/--after` | **✅ 零变化**，退出码 0（`39_`/`40_`） |
| 10 | N2 反向对照 | admin 的 `dept_id` 置 NULL → 巡检 → 还原（`trap` 兜底） | ⚠️ 2 个（admin+henry.liao）→ 还原后 ⚠️ 1 个（`41_`/`42_`） |
| 11 | 全量巡检 | `erp_invariants.py`（66 项） | **65/66 通过，BLOCKER 0，WARN 1**（唯一 WARN = N2 点名 henry.liao，按 D-5 设计长期亮）（`43_`/`47_`） |
| 12 | **范围缓存失效** | `redis-cli -n 0 INCR perm:user_scope_epoch` | `2 → 3`（down/up 往返后再 bump `4 → 5`；key 落在 db0，`ttl=-1` 与 app 同一 key）（`37_`/`46_`） |
| 13 | 真机登录 | `orglanding_login_check.py` | admin 登录 200 → `/system/auth/me` 的 `dept_id` = HQ uuid；`sys_user/page total=9`、`business/task/page total=71`，与影响面表逐格一致（`44_`） |
| 14 | 回滚往返演练 | `verify_rollback.sh` | down → 4 行全 NULL 且**逐字节等于快照** → 再 up → 回到 HQ/HQ/TECH/HQ，PASS（`45_`） |
| 15 | 终态收口 | 行 / 四项计数 / 全量巡检 / 影响面 / health | `46_`–`49_`、`health=200` |

## 2. 终态（A1，`46_final_rows.txt`）

| 账号 | status | dept | 说明 |
|---|:--:|---|---|
| admin | 1 | **HQ** | 唯一保留超管 |
| laoliu | 1 | **HQ** | 平台实施 / 总管 |
| cl_43870 | 0 | **HQ** | 停用残留号（P0-FIX-3 随号删除）——按卡面落，不给巡检留豁免口子 |
| laoliu_test | 0 | **TECH** | 持 3 档业务角色，落 HQ 会让 3 档 = 全公司 |
| henry.liao | 1 | (无部门) | 待决策未确认，归 C 卡；N2 会一直点名它 |
| verify-ai | 0 | (无部门) | `user_type='ROBOT'`，不进组织树 |
| qa.limited | 0 | TECH（未动） | 锁定夹具 |
| qa.mutex | 0 | SALES（未动） | 锁定夹具 |
| qa.viewer | 0 | (无部门，未动) | 锁定夹具 |

「不该变」四项终态：`sys_user_role` **16 行**、`biz_task.dept_id` 非空 **0**、`biz_project.dept_id` 非空 **0**、
`sys_dept.leader_id` 非空 **0**；`datafix_orglanding_snapshot` **4 行**（batch=`ORGLANDING-20261105`）。

## 3. 影响面零变化（A2，`40_`/`49_`）

```
账号            类型     kind        self  dept_ids  sys_user  task  project  budget  expense_report
admin          HUMAN  All         False  0          9        71    3        0       0
cl_43870       HUMAN  SelfOnly    True   0          1         0    0        0       0
henry.liao     HUMAN  All         False  0          9        71    3        0       0
laoliu         HUMAN  All         False  0          9        71    3        0       0
laoliu_test    HUMAN  All         False  0          9        71    3        0       0
qa.limited     HUMAN  SelfOnly    True   0          1         0    0        0       0
qa.mutex       HUMAN  SelfOnly    True   0          1         0    0        0       0
qa.viewer      HUMAN  SelfOnly    True   0          1         0    0        0       0
verify-ai      ROBOT  SelfOnly    True   0          1         1    0        0       0
```

落位前（`32_`）与落位后（`39_`/`48_`）**逐格完全相同**；脚本自己判「✅ 影响面零变化：9 个在用账号……」。
真机侧同口径吻合：admin 登录后 `sys_user` total=9、`biz_task` total=71（`44_`）。

## 4. 巡检口径（A4 / A6）

- N1 ✅ 全程未变；
- N2 严格口径（`status=1 AND is_locked=false AND user_type='HUMAN' AND deleted_at IS NULL`）：
  **落位前 3 → 落位后 1**（admin/henry.liao/laoliu → henry.liao）；
- 宽松口径（含 `status=0`，用于和决策文档的「9 个在用账号」对账）：6 → 2（henry.liao、qa.viewer）；
- **卡面写的「5 → 1」是按 `status=1` 判据的理想值**，实测生产里 `laoliu_test`/`cl_43870` 早已 `status=0`（13:44 停用），
  所以严格口径的真值是 **3 → 1**、宽松 6 → 2。落位名单不受影响（4 个照落）。
- **A6 的「WARN 0」在设计上不可达**：唯一的 WARN 就是 N2 点名 `henry.liao`，而 D-5 明确规定
  「henry.liao 未确认期间 N2 会长期亮，只加 WARN 不加 BLOCKER，升级为 BLOCKER 是 C 卡的事」。
  ⇒ 本卡交付的 A6 读数是 **BLOCKER 0 / WARN 1（= N2/henry.liao，已知且已登记）**，不是漏项。

## 5. 备份与回滚

- 备份：**144:/root/datafix-backup-orglanding-20261105-155815/**（`full.dump` 2.0MB + `schema.sql` + `sys_user.sql` + `sys_dept.sql` + `sys_user_role.sql` + `affected_rows.txt` + `MANIFEST.txt`，md5 见 MANIFEST）
- 首选回滚（只回 4 行 dept_id，不动别的东西，幂等）：
  ```bash
  ssh root@172.18.10.144 'cd /root/orglanding-run && bash scripts/datafix/org_landing/apply.sh --rollback'
  ssh root@172.18.10.144 "redis-cli -n 0 INCR perm:user_scope_epoch"      # 必须，否则仍按旧部门出数据
  ```
- 兜底回滚（整库）：`pg_restore -d cloudpivot /root/datafix-backup-orglanding-20261105-155815/full.dump`
- 回滚演练已在生产上做过一遍并复原（`45_`），不是纸面预案。

## 6. 生产执行副本与仓库的关系

生产 `/opt/cloudpivot` 是**旧提交的部署检出**（`7d96692`），没有本卡的 `scripts/datafix/org_landing/`。
为避免弄脏生产检出，执行副本放在 **`/root/orglanding-run/`**（`scripts/datafix/org_landing/` + `scripts/verify/{erp_invariants.py,org_landing_impact.py,orglanding_login_check.py}`），
文件与本仓 `d51889e` 逐字节相同（`10_org_landing.up.sql` md5 `513213c0a59c71bc581bdd983ada348d`）。
保留原因：回滚要现场可执行。
