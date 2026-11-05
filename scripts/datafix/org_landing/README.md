# 组织落位（P0-FIX-2B-A）· datafix 脚本

一次性数据修复，**不是迁移**（不进 `backend/migrations/`，不参与启动自动应用）。
上游决策：[`docs/architecture/P0-FIX-2B-生产组织落位与超管收敛-决策.md`](../../../docs/architecture/P0-FIX-2B-生产组织落位与超管收敛-决策.md)（D-1 / D-6）。
卡片：`t_db82c38e`。证据：`docs/evidence/20261105-p0fix2b-orglanding/`。

## 做什么 / 不做什么

| 账号 | 目标 dept | 说明 |
|---|---|---|
| `admin` | `HQ` 云枢总部 | 唯一保留超管；落位让「无部门账号」告警归零（不留豁免名单） |
| `laoliu` | `HQ` 云枢总部 | 平台实施 / 总管，业务上属总部职能 |
| `laoliu_test` | `TECH` 技术部 | 持 3 档业务角色；落 HQ 会让 3 档 = 全公司（HQ 是根），摘权等于没摘 |
| `cl_43870` | `HQ` 云枢总部 | 0 次登录的残留号；P0-FIX-3 随号删除 |

**不动**：`henry.liao`（`docs/待决策.md` 铁律①未确认，归 C 卡）、`verify-ai`（`ROBOT` 不进组织树）、
`qa.limited` / `qa.mutex` / `qa.viewer`（锁定夹具）、`sys_dept.leader_id`（D-6 不设）、
`sys_user_role`（一条不增删，摘权是 C 卡）、`biz_task` / `biz_project` 的 `dept_id`（D-3 不回填历史）。

部门一律按 **`dept_code`**（`HQ` / `TECH`）解析 uuid，不硬编码（V0247 同一口径）。
目标库没有组织树：`RAISE NOTICE` 跳过 + `RAISE EXCEPTION`（退出码非 0），**不猜、不造部门**
（实测见 `docs/evidence/20261105-p0fix2b-orglanding/16_no_tree_guard.txt`：rc=3、4 行一行未改）。

## 文件

| 文件 | 做什么 | 对应验收 |
|---|---|---|
| `00_backup.sh` | 整库 + `sys_user`/`sys_dept`/`sys_user_role` 全表备份 → `/root/datafix-backup-orglanding-<ts>/`，另存 4 个落位账号逐列快照 + 三个「不该变」的计数 | 先备份 |
| `10_org_landing.up.sql` | 一个事务里：整行快照 → 硬断言「快照行数 = 目标行数」→ 逐账号幂等 UPDATE → 落位/不动名单/禁改项复核 | A1 / A3 |
| `10_org_landing.down.sql` | 按快照写回 `dept_id`（回到落位前，4 行全 NULL），幂等 | A3 |
| `apply.sh` | `--check` 看现状 / 默认应用 / `--rollback` 回滚；应用前要求本库备份存在（`FORCE=1` 可跳） | — |
| `verify_idempotent.sh` | 连跑 2 次；第二遍必须「本次共变更 0 行」且受影响行逐字节相同 | A3 |
| `verify_rollback.sh` | down 回到落位前（与快照逐字节一致）→ 再 up 回到 HQ/HQ/TECH/HQ | A3 |

## 用法

```bash
cd /opt/cloudpivot

# 生产（需要老大书面确认，见卡片 §5）
bash scripts/datafix/org_landing/00_backup.sh
bash scripts/datafix/org_landing/apply.sh             # 应用
bash scripts/datafix/org_landing/verify_idempotent.sh # 连跑 2 次
python3 scripts/verify/erp_invariants.py --group 权限  # N1 + N2
python3 scripts/verify/org_landing_impact.py --out /tmp/impact-after.json   # 影响面对照
bash scripts/datafix/org_landing/apply.sh --rollback   # 需要时回滚

# 演练库 / 隔离库（覆盖库名）
ORGLANDING_DB=cloudpivot_orgland FORCE=1 bash scripts/datafix/org_landing/apply.sh
```

环境变量：`ORGLANDING_DB`（默认 `NIK_DATABASE_NAME`）、`FORCE=1`、`CLOUDPIVOT_ENV_FILE`（默认 `/etc/cloudpivot/cloudpivot.env`）。

## 为什么不是版本迁移（V0248）

组织落位是**数据决定**（`dept_code` → uuid 各环境不同）而不是结构变更；写成迁移就得连带发一版二进制，
而本卡的全部意义是「生产能在不回滚代码的前提下单独跑 / 单独回滚」。与 `scripts/datafix/p9|p10|p14` 同形态。

## 幂等与回滚的口径

- **幂等**：每条 `UPDATE` 都带 `dept_id IS DISTINCT FROM <目标>`，第二遍命中 0 行；
  快照断言看的是**表里这一批次的总行数**（第二次跑 `ON CONFLICT DO NOTHING` 插 0 行是正确的）。
- **回滚**：写回 `datafix_orglanding_snapshot` 里的原值（不是无条件置 NULL）——本次原值恰好全 NULL，
  但口径仍是快照值，防「up 与 down 之间被人改过」。
- **安全阀**（命中即整脚本回滚）：账号不存在/已软删、`dept_code` 缺失或不唯一、账号已有**别的**部门、
  快照行数 ≠ 4、落位未生效、不动名单被改、`sys_user_role` 行数变化、`biz_task`/`biz_project`
  `dept_id` 非空行数变化、`sys_dept.leader_id` 非空行数变化。

## 落位之后必须做的两件事

1. `INCR perm:user_scope_epoch`（范围缓存失效）——否则改完 5 分钟内列表仍按旧范围出数据
   （隔离实例实测：不 bump，改完 `dept_id` 登录后仍看到旧部门的数据；bump 后立刻变，
   见 `docs/evidence/20261105-p0fix2b-orglanding/20_iso_e2e_landing.txt` 第 ② 项）。
2. 跑 `erp_invariants.py --group 权限`：N1 仍 ✅、N2 从 N → 1（只剩 `henry.liao`）。
