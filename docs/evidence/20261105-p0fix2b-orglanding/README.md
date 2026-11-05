# 组织落位（P0-FIX-2B-A）· 真机证据

> 卡片 `t_db82c38e`｜上游决策 `docs/architecture/P0-FIX-2B-生产组织落位与超管收敛-决策.md`（D-1 / D-6）
> 脚本 `scripts/datafix/org_landing/`｜巡检 `scripts/verify/erp_invariants.py`（N1 + **N2**）
> 影响面 `scripts/verify/org_landing_impact.py`｜真机 `scripts/verify/org_landing_e2e.py`

**标的空间**：生产副本 → 99 的 `cloudpivot_orgland`（`pg_dump` 自 144 prod，只读）；
隔离实例 `127.0.0.1:18095`（profile=test，同一副本库）；
**生产 144 已按授权执行**（2026-11-05 15:58–15:59 CST，见 `prod/` 子目录 21 份现场输出）。


---

## 0. 一句话结论

- 落位脚本在**生产副本**上跑通了完整闭环：落位 → 幂等（第 2 次 0 行）→ 回滚（回到全 NULL）→ 再落位，
  快照 4 行，`sys_user_role` / `biz_task` / `biz_project` / `sys_dept.leader_id` 一行未变；
- 影响面**零变化**：9 个在用账号的 `(kind, dept_ids, self_only)` 与 5 张受控表可见行数前后完全一致；
  且这条断言**有牙**——把「非超管 + 部门级角色」的探针账号真落位一次，核算当场报错（`22_`）；
- 巡检 N2 上线：落位前 ⚠️ 点名 3 个（`admin`/`henry.liao`/`laoliu`）→ 落位后只剩 `henry.liao`；
  反向对照 ✅→⚠️（点名 `laoliu`）→✅（`13_`）；
- 真机：部门隔离 16/16（含 `data_scope` 1↔5 反向对照、建单 `dept_id` 快照）+ 落位因果 8/8（`19_`/`20_`）。
- **生产 144 已执行**（15:58–15:59）：4 行落成 HQ/HQ/TECH/HQ、幂等第 2 次 0 行、回滚往返 PASS、
  影响面零变化、N2 3 → 1、`perm:user_scope_epoch` 2→3（往返后再 4→5）、`health=200` —— 见 `prod/`。

## 1. 验收逐条对照

| # | 断言 | 结果 | 证据 |
|:--:|---|---|---|
| A1 | 4 行 `dept_id` = HQ/HQ/TECH/HQ；henry.liao / verify-ai / qa.viewer 仍 NULL | ✅（副本库） | `06_after_rows.txt`、`15_final_state.txt` |
| A2 | 9 个在用账号逐条 `(kind, dept_ids, self_only)` + 5 表可见行数前后完全一致 | ✅ | `03_impact_before.txt`、`09_impact_after.txt`、`10_impact_compare.txt`、`22_impact_sensitivity.txt`（断言有牙） |
| A3 | 连跑 2 次第 2 次 0 行；`down` 回到落位前；快照 4 行 | ✅ | `11_idempotent.txt`、`12_rollback.txt`、`07_snapshot_rows.txt` |
| A4 | N1 仍 ✅；N2 前 → 后收敛；反向对照 ✅→⚠️→✅ | ✅ | `02_invariants_before.txt`、`08_invariants_after.txt`、`13_invariant_negative_control.txt` |
| A5 | 隔离库：部门隔离 + 反向对照 + 建单 dept 快照 | ✅ 16/16 + 8/8 | `19_iso_e2e_departments.txt`、`20_iso_e2e_landing.txt` |
| A6 | 生产 `health=200`、`INCR perm:user_scope_epoch`、全量 BLOCKER 0 / WARN 0 | ✅ 前两条；**WARN=1（= N2 的 henry.liao，按 D-5 设计长期亮，A6 的「WARN 0」在设计上不可达）** | 生产 `prod/37_`（epoch 2→3、往返后 4→5）、`prod/43_`/`prod/47_`（65/66、BLOCKER 0、WARN 1）、`prod/44_`（admin 真机登录 total 9/71）、`prod/` 其余 21 份 |

> 生产已在 144 原样重跑同一组命令（`00_backup.sh` → `apply.sh` → `verify_idempotent.sh` →
> `erp_invariants.py` → `org_landing_impact.py --after` + 回滚往返 + 真机登录），
> 输出见 `prod/`（`30_`–`49_`）与 `prod/README.md`。生产副本与本卡结论完全一致。


## 2. 影响面核算（A2 原文）

```
账号            kind        dept_ids  self    sys_user  task  project  budget  expense_report
admin           All         0         False   9         71    3        0       0
cl_43870        SelfOnly    0         True    1         0     0        0       0
henry.liao      All         0         False   9         71    3        0       0
laoliu          All         0         False   9         71    3        0       0
laoliu_test     All         0         False   9         71    3        0       0
qa.limited      SelfOnly    0         True    1         0     0        0       0
qa.mutex        SelfOnly    0         True    1         0     0        0       0
qa.viewer       SelfOnly    0         True    1         0     0        0       0
verify-ai       SelfOnly    0         True    1         1     0        0       0
```

（表里是「落位前后都等于这个值」——脚本发现任何一格变化就退出 1 并点名。）

`dept_ids` 全是 0 是**预期的**：落位的 4 个账号里有 3 个持 `SUPER_ADMIN`（`merge_scope_rows`
在超管/ALL 时清空 `dept_ids`，S9 短路），`cl_43870` 没有角色（`SelfOnly`）。
也就是说「落位对今天任何人的可见性都没有影响」不是巧合，是这 4 个人本来就都在
「超管」或「无角色」这两端。

### 2.1 为什么这条断言不是橡皮章（`22_impact_sensitivity.txt`）

造一个「非超管 + `DEPT_LEADER`(scope=3) + 无部门」的探针账号 `zz_probe`：

- 落位前：`SelfOnly`，`dept_ids=[]`，`sys_user` 可见 1 行（自己）；
- 只把它落到 `TECH`：`DeptAndChild`，`dept_ids=[TECH]`，`sys_user` 可见 3 行；
- 核算脚本当场报错并点名：

```
🚨 影响面发生了变化（落位改到了不该改的东西，停手）：
  · zz_probe: kind SelfOnly → DeptAndChild
  · zz_probe: dept_ids [] → ['01a18381-…']
  · zz_probe: self_only True → False
  · zz_probe: sys_user 可见行数 1 → 3
对照退出码=1
```

结论：**落位确实会改变可见性——只要被落位的人手里有部门级角色且不是超管**。
这正是「`laoliu_test` 落 `TECH`（不是 `HQ`）」和「`henry.liao` 先不落位」的技术原因。

## 3. N2 巡检（含口径说明）

```
N2 在用 HUMAN 账号必须有部门归属   （WARN）
  判据：status=1 AND is_locked=false AND user_type='HUMAN' AND deleted_at IS NULL
        ⇒ dept_id IS NOT NULL
  豁免：user_type='ROBOT' / is_locked=true / status=0（停用号登不进来）
```

| 时点 | 命中 | 明细 |
|---|---|---|
| 落位前（`02_`） | 3 | `admin`、`henry.liao`、`laoliu` |
| 落位后（`08_`） | 1 | `henry.liao`（身份待确认，C 卡） |
| 反向对照（`13_`） | ✅（给 henry 临时部门）→ ⚠️ 点名 `laoliu`（`laoliu.dept_id` 置 NULL）→ ✅（还原）→ ⚠️（还原 henry） |

**口径差异（必须说清）**：卡面写「落位前 5 → 落位后 1」，那 5 个是
`admin / laoliu / laoliu_test / henry.liao / cl_43870`，是按「`deleted_at IS NULL` 的 HUMAN」数的。
生产实测（2026-11-05 15:2x）：`laoliu_test` 与 `cl_43870` 的 `status=0`（13:44 被某次夹具清理停用，
`updated_at=2026-11-05 13:44:19`），照卡片自己的判据（`status=1`）它们不算「在用」，
所以严格口径是 **3 → 1**。为了跟决策文档对账，N2 的输出里额外打一行「宽松口径」
（含 `status=0`：落位前 6 → 落位后 2，多出来的 `qa.viewer` 也是 `status=0` 的锁定夹具）。
两套数法都能在 `02_` / `08_` 里逐字看到。

## 4. 隔离实例真机（A5）

### 4.1 部门隔离 + `data_scope` 1↔5 反向对照 + 建单快照（`19_`，16/16）

`scripts/verify/p0fix2_data_scope_e2e.py`（P0-FIX-2 既有套件，本卡在副本库上复跑）：

- `p2_fin`(FIN) 看不到 `p2_sal`(SALES) 的任务，反之亦然；
- 把 `FINANCE_MANAGER` 的 `data_scope` 改回 **1**（修复前的空转状态）→ 立刻互相可见；
  改回 **5** → 又不通（因果成立）；
- 建单 `biz_task.dept_id` = 负责人所在部门；`SUPER_ADMIN` 两种配置下都看得到（没被限死）；
- 跑完夹具自清（残留 0 行）。

### 4.2 落位本身造成什么（`20_`，8/8）

`scripts/verify/org_landing_e2e.py`（本卡新增）—— 用**真实部门 `TECH` / `FIN`**（按 `dept_code`
解析 uuid）各造 1 人（`DEPT_LEADER`）各 1 张任务：

- TECH 账号看不到 FIN 任务、FIN 账号看不到 TECH 任务；
- 把 TECH 那个人的 `dept_id` 改到 FIN + 范围缓存失效 → **原先看不到的 FIN 任务立刻可见**
  （自己负责的 TECH 单仍可见，属 `VISIBLE_OR_TAIL` 的「范围 ∪ 参与关系」并集语义）；
- 改回 TECH → 又只看得到 TECH 的任务（因果双向成立）。

> ⚠️ 这一步暴露了一个**必须记住的操作约束**：直改 `sys_user.dept_id` 后如果不做范围缓存失效，
> 列表仍按旧范围出数据（第一次跑 ② 就是这么失败的）。生产里改部门走 `PUT /system/dept`
> （handler 里 `scope_guard::bump_epoch()`），而 datafix 是直改库 ⇒ **落位后必须
> `INCR perm:user_scope_epoch`**，这正是 A6 的最后一条。

## 5. 组织树缺失时不许猜（`16_`）

克隆副本库、把 `TECH` 改名后再跑 `apply.sh`：

```
NOTICE:  ORGLANDING: 目标库组织树不满足前置条件（HQ 1 个 / TECH 0 个，各需恰好 1 个）—— 跳过，不猜部门、不造部门
ERROR:  ORGLANDING: 跳过组织落位（部门树缺失或 dept_code 不唯一，HQ=1 TECH=0）
退出码=3
admin/cl_43870/laoliu/laoliu_test 的 dept_id 仍 NULL（一行未改）
```

## 6. 脚本安全阀一览（都在同一事务里，命中即整脚本回滚）

账号不存在/已软删、`dept_code` 缺失或不唯一、账号已有**别的**部门（有人做过别的决定）、
快照行数 ≠ 4（SOP 红线：先改后补快照 = 违规）、落位未生效、不动名单（`henry.liao`/`verify-ai`/`qa.viewer`）
被改、`sys_user_role` 行数变化、`biz_task`/`biz_project` 的 `dept_id` 非空行数变化、
`sys_dept.leader_id` 非空行数变化。

## 7. 怎么复跑

```bash
# ① 生产副本（99）
su - postgres -c 'dropdb --force --if-exists cloudpivot_orgland && createdb -O alun cloudpivot_orgland'
ssh root@172.18.10.144 'set -a; . /etc/cloudpivot/cloudpivot.env; set +a; \
  PGPASSWORD="$NIK_DATABASE_PASSWORD" pg_dump -h "$NIK_DATABASE_HOST" -U "$NIK_DATABASE_USER" \
  -d "$NIK_DATABASE_NAME" --no-owner --no-privileges' \
  | PGPASSWORD=alun psql -q -h 127.0.0.1 -U alun -d cloudpivot_orgland

export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
       NIK_DATABASE_NAME=cloudpivot_orgland ORGLANDING_DB=cloudpivot_orgland CLOUDPIVOT_ENV_FILE=/nonexistent

# ② 落位链路
python3 scripts/verify/erp_invariants.py --group 权限                    # 落位前：N2 点名 3 个
bash scripts/datafix/org_landing/00_backup.sh drill
bash scripts/datafix/org_landing/apply.sh
bash scripts/datafix/org_landing/verify_idempotent.sh
bash scripts/datafix/org_landing/verify_rollback.sh
python3 scripts/verify/org_landing_impact.py --out /tmp/impact-after.json

# ③ 真机（隔离实例 18095，profile=test）
bash /root/.hermes/profiles/backend-developer/cache/scratch/start_iso.sh   # 或按
#    scripts/verify/e2e_isolated_instance.sh 的 start_backend 段自起
P2_BASE=http://127.0.0.1:18095 P2_DB_NAME=cloudpivot_orgland P2_DB_USER=alun P2_DB_PASSWORD=alun \
  python3 scripts/verify/p0fix2_data_scope_e2e.py
LANDBASE=http://127.0.0.1:18095 LAND_DB_NAME=cloudpivot_orgland LAND_DB_USER=alun LAND_DB_PASSWORD=alun \
  python3 scripts/verify/org_landing_e2e.py
```

真机夹具的两个前提（只对副本库做，生产不涉及）：

1. 副本库 `admin` 口令重置成已知口令（prod 口令未知；证据里用 `Admin@123456` 的 argon2 哈希直改库）；
2. prod 的 `FINANCE_MANAGER` / `DEPT_LEADER` 没有 `business:task:*` 权限码，而 A5 验的是
   「任务列表的部门隔离」，所以给这两个**角色**在副本库里补了 `business:task:read/create/update`
   （`17_iso_fixture_prep.txt`）。生产角色权限一行不动。

## 8. 还没做的（等老大一句话）

1. prod `00_backup.sh` → `apply.sh` → `verify_idempotent.sh`（连跑 2 次）→ `erp_invariants.py`（N1+N2）→
   `org_landing_impact.py --after` 对照 → 留档；
2. prod `INCR perm:user_scope_epoch`（范围缓存失效）+ `health=200` 复核；
3. 这三步的输出会作为 `30_prod_apply.txt` / `31_prod_invariants.txt` / `32_prod_impact.txt` 追加到本目录。
