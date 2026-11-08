# P3-09 · 工作流全链路回归演练（ARCH-8 八场景 + 并行/条件/版本 + 驳回四目标）

- 卡：`t_91bba6c3`（前置 P3-02~P3-08 全部 done）
- 日期：2026-11-08
- 结论：**16 个场景 / 228 条断言全部 PASS，FAIL=0**；另复现出 1 个真实缺陷（B2b），已建修复卡 `t_886189f8`（`assignee=backend-developer`，`parents=[t_91bba6c3]`）。
- **修复后复核（`t_886189f8` 交付）**：B2b 改写成「必须 APPROVED」的回归 + 新增 B2c（并行两支汇到同一个 END 必须等齐），全量复跑 **17 个场景 / PASS=243 / FAIL=0**，见 `raw/00_run_fix.txt` 与本文第 12 节。
- 本卡**不改业务代码**：只造夹具（全部走真 HTTP API）、写演练脚本、写证据。

---

## 1. 一条命令复跑

```bash
cd /opt/cloudpivot
bash docs/evidence/20261108-p309-wf-drill/scripts/00_run.sh          # 建全新空库 → 起 :18091 → 跑 A/B/C → 停实例 → 复核端口
bash docs/evidence/20261108-p309-wf-drill/scripts/30_snapshot.sh     # 关库后导关键 DB 行快照（只读）
```

`00_run.sh` 每次先 `dropdb --force` + `createdb`（**全新空库，不复用任何人的证据库**，避免 `schema_migrations` 指纹被人改过导致校验和失败），然后跑全量迁移 → 起隔离实例 → 跑 `scripts/e2e.py` → 停实例并复核 `18091` 已释放。

只跑某一组场景（实例起来后）：

```bash
BASE=http://127.0.0.1:18091 DB=cloudpivot_p309_drill python3 docs/evidence/20261108-p309-wf-drill/scripts/e2e.py A1 A7 C4 B2b
```

幂等性实测：**连跑三次**（`raw/00_run_2nd.txt`、`raw/00_run.txt` 为第 2、3 次）均为 `PASS=228 FAIL=0`，端口复核均为 `18091 监听行数=0 health=000`。

修复后（`t_886189f8`）再跑一次全量：**`PASS=243 FAIL=0`**（`raw/00_run_fix.txt`），B2b 与新增的 B2c 全绿。

---

## 2. 环境口径（与卡面一致）

| 项 | 值 |
|---|---|
| DB | 本机 PG `127.0.0.1:5432`，库 `cloudpivot_p309_drill`（每次新建） |
| 迁移 | 首启 `应用 274 个迁移`；`schema_migrations` 中 `kind='versioned'` = 263 条 |
| 实例 | 隔离端口 `18091`，工作目录 `/opt/cloudpivot/backend`，`setsid` 后台启动 |
| 二进制 | `/root/p308/bin/cloudpivot`，md5 `e1c33cd1fbfc565cf8fdfd6837ac993e` = **HEAD `506fa34`（P3-08 交付树）的 debug 构建产物**；`git status backend/src/business/flow/` 干净 |
| 二进制（修复后复核） | `/root/p309/bin/cloudpivot`，md5 `7df255ef3b8f3dacbd5c207b569e9000` = **`38b813c` + `t_886189f8`（END 汇聚口径）的 debug 构建产物**；`lib.sh` 默认 `BIN` 已指向它 |
| 三把密钥 | `NIK_MIDDLEWARE_AUTH_JWT_SECRET`(64) / `CLOUDPIVOT_STORAGE_MASTER_KEY`(64) / `X_CONFIG_SECRET_KEY`(32)，只在 `600` 权限的 `/root/p309/instance.env` 里，**仓库零密钥** |
| 144 生产库 | **零读写**（脚本只连 127.0.0.1 本机测试库） |
| 跑完状态 | 实例已停、库保留待复核：`18091 监听行数=0 / health=000`（见 `raw/00_run.txt` 末段、`raw/30_db_snapshot.txt` 第 7 节） |

---

## 3. 交付物

| 文件 | 内容 |
|---|---|
| `scripts/00_run.sh` | 一条命令：建新库 → 起实例 → 跑演练 → 停实例 → 端口复核 |
| `scripts/lib.sh` | 公共函数（起停实例 / 等 health / 端口探测） |
| `scripts/e2e.py` | **演练脚本本体**（stdlib urllib，形态照 `docs/evidence/20261108-p203-purchase-return/e2e.py`）：夹具现建、每个场景独立计数、末尾打印 PASS/FAIL 汇总表，FAIL 非零则 exit 1 |
| `scripts/30_snapshot.sh` | 关键 DB 行快照（实例/待办留痕/审计/round/并行汇聚/条件边/版本对照/种子 END join_mode） |
| `raw/00_run.txt` | 第 3 次（最终）完整输出：`PASS=228 FAIL=0` |
| `raw/00_run_2nd.txt` | 第 2 次完整输出（幂等证明，同样 `PASS=228 FAIL=0`） |
| `raw/21_sql_assertions.txt` | 每条 SQL 断言 + 实际值 + 期望值；含各场景关键行快照（**内容为修复后那一次**；修复前版本见 git 历史） |
| `raw/30_db_snapshot.txt` | 关库后的 7 组关键 DB 行快照（**修复前**那次，含 B2b 卡单行） |
| `raw/00_run_fix.txt` | 修复后（`t_886189f8`）全量输出：`PASS=243 FAIL=0`（17 个场景） |
| `raw/30_db_snapshot_fix.txt` | 修复后的 DB 行快照（B2b/B2c 两个 END 都是 `APPROVED`） |

---

## 4. 结果总表

| 组 | 场景 | 断言数 | 结果 |
|---|---|---|---|
| 夹具 | 管理员/14 角色/19 用户/权限/成员数/令牌 | 42 | ALL PASS |
| A | ARCH-8 ① 角色 3 人 + 制单人 1 人 | 15 | PASS |
| A | ARCH-8 ② 角色 1 人（=制单人）→ 升级上级角色 | 11 | PASS |
| A | ARCH-8 ③ 角色 1 人 + 无上级 → 兜底人 | 7 | PASS |
| A | ARCH-8 ④ 一人公司 + ALLOW 自审 + 审计 + 报表 | 11 | PASS |
| A | ARCH-8 ⑤ 审批人委托（代 X 审批 / 期外回本人） | 14 | PASS |
| A | ARCH-8 ⑥ 财务节点 FORBID → 422/65019 | 5 | PASS |
| A | ARCH-8 ⑦ 角色节点默认或签（1 人过 / 2 条 CANCELLED） | 7 | PASS |
| A | ARCH-8 ⑧ 留痕完整（升级/兜底/委托/自审 SKIP/ALLOW） | 6 | PASS |
| B | 并行网关（AND 汇聚 / 只签一臂不汇聚 / OR 汇聚拒发布 / 条件 OR 汇聚先到先走） | 26 | PASS |
| B | 条件动态路由（5000 / 50000 / 500000 三档） | 21 | PASS |
| B | **B2b 缺陷复现**：END 默认 join=AND → 实例永久 PENDING | 7 | PASS（复现成立） |
| B | 流程版本冻结（老实例旧图 / 新单新图） | 23 | PASS |
| C | 驳回 PREV | 9 | PASS |
| C | 驳回 INITIATOR（RESUBMIT） | 7 | PASS |
| C | 驳回 NONE(FINAL) | 5 | PASS |
| C | 驳回 `<node_key>`（非 admin 403 / admin 放行 / 65025） | 12 | PASS |
| | **合计** | **228** | **FAIL=0** |

---

## 5. A 组：ARCH-8 八条（输入 → 期望 → 实际）

> 夹具：14 个角色、19 个用户、权限码按 `auth_code` 直授（等价于权限页勾选，脚本注释写明是夹具构造）。所有签法都以**实例节点行落库的 `approval_mode`** 为准，不靠猜。

### A1 ① 角色 3 人、制单人 1 人

| 输入 | 期望 | 实际 |
|---|---|---|
| 制单人 `u_t1` ∈ `P309_THREE`(3 人)，节点 ROLE=该角色、`approval_mode=ALL`（与 V0339/bootstrap 冻结口径一致：候选 ≥2 → ALL） | 剩余 2 人收到待办；制单人落 SKIPPED 留痕 | 实例节点 `approval_mode=ALL`；PENDING 待办 **2**（u_t2/u_t3）；SKIPPED **1**（u_t1，comment 含 `self_approval=SKIP`） |
| 只签 u_t2 | **签法 = ALL → 2 人都要签**，实例仍 PENDING、节点仍 ACTIVE | PENDING / ACTIVE；`escalated_from_role` 全空、`assigned_by_rule=ROLE`（2 行） |
| u_t3 补签 | 实例 APPROVED | APPROVED，节点 `approved_count=2` |

### A2 ② 角色 1 人（=制单人）→ 自动升级上级角色

| 输入 | 期望 | 实际 |
|---|---|---|
| `P309_MAKER`(1 人 = `u_m1`) 的 `escalation_role_id → P309_BOSS`(2 人)，节点 self=SKIP | 升级后 2 人收到待办，`escalated_from_role` / `escalate_reason` 有值 | PENDING 2 行；`escalated_from_role=P309_MAKER`；`escalate_reason=SKIP_SELF_APPROVAL`；`assigned_by_rule=ESCALATED`；2 行都属于 `P309_BOSS` 成员 |
| 上级成员 `u_b1` 审批 | 成功、实例 APPROVED | 200 / APPROVED；升级留痕行仍在（2 行） |

### A3 ③ 角色 1 人 + 无上级 → 节点兜底人

| 输入 | 期望 | 实际 |
|---|---|---|
| `P309_LONE`(1 人 = `u_l1`，**无上级**)，节点 `fallback_user_id=u_fb` | 兜底人收到待办；`assigned_by_rule='FALLBACK'` | 待办 assignee=u_fb（1 行）；`assigned_by_rule=FALLBACK` |
| `u_fb` 审批 | 成功 | 200 / 实例 APPROVED |

### A4 ④ 一人公司 + ALLOW

| 输入 | 期望 | 实际 |
|---|---|---|
| `u_solo` 单人角色、节点 `self_approval=ALLOW`、金额 8888 | 自审成功 | 待办派给本人且 `is_self_approval=t`；审批后实例 APPROVED |
| `operation_logs` | 有可读记录 | `FLOW_SELF_APPROVAL_ALLOWED` 1 行 + `FLOW_SELF_APPROVAL_VOTED` 1 行（含 node_name / amount=8888.0 / vote=APPROVED / reason） |
| `GET /business/flow/stats/self-approval?month=2026-11&biz_type=P309_A4` | 报表有该单 | list 含该 `instance_id`；`self_approval_instances≥1`、`self_approval_amount≥8888` |

### A5 ⑤ 审批人设委托

| 输入 | 期望 | 实际 |
|---|---|---|
| `u_del` 委托给 `u_pa`（现在 → +3 天，reason=出差北京） | 新建委托 200 | 200，委托 id 就绪 |
| 委托期内发起 | 受托人收到待办 + 显示「代 X 审批」 | assignee=u_pa；`assigned_by_rule=DELEGATED`；comment=`代 P309_DEL u_del 审批（委托至 2026-11-11）` |
| 受托人审批 | 成功，留痕记在受托人名下 | 200 / APPROVED；行 `P309_PA u_pa|APPROVED|DELEGATED` |
| 把委托改成已过期窗口后新发起 | 回到本人、不带「代 … 审批」 | assignee=u_del；`comment like '代 %审批%'` 计数 0 |

### A6 ⑥ 财务节点 FORBID

| 输入 | 期望 | 实际 |
|---|---|---|
| `u_fin` 单节点 `self_approval=FORBID` 自审发起 | 422 / 65019 | 422 / `65019`，报文含 `self_approval=FORBID`；库里**没有半截实例**（该标题 0 行） |

### A7 ⑦ 角色节点默认或签（NULL）

| 输入 | 期望 | 实际 |
|---|---|---|
| 发起人不在角色内，节点 **不填** `approval_mode`，角色 3 人 | 3 条待办；任 1 人审批即通过，另 2 条 CANCELLED | 3 条 PENDING；实例节点 `approval_mode` 落库为 **NULL**；u_a1 审批后实例 APPROVED，u_a2/u_a3 = CANCELLED |

### A8 ⑧ 留痕完整

| 输入 | 期望 | 实际（四类都能查到原因） |
|---|---|---|
| 汇总 A1~A5 产生的行 | 升级 / 兜底 / 委托 / 自审 SKIP 每类都能查到原因 | 升级：`P309_MAKER / SKIP_SELF_APPROVAL / ESCALATED`；兜底：`FALLBACK`；委托：`DELEGATED` + comment `代 …（委托至 …）`；自审 SKIP：`SKIPPED` + `审批人是发起人本人（self_approval=SKIP），已跳过并留痕`；自审 ALLOW：`FLOW_SELF_APPROVAL_ALLOWED/VOTED` |

---

## 6. B 组：ARCH-5 三项「已交付但零生产用例」

### B1 并行网关（26 条断言 PASS）

图：`s → PARALLEL(fork=AND) → {n_a(ROLE 1 人), n_b(ROLE 1 人)} → PARALLEL(join=AND) → {e1, e2}`。
> 注：R7 要求网关出度 ≥ 2，所以汇聚节点也必须分叉 → 用两个 END 收尾（END 不做汇聚门控场景判定，见 §8 缺陷）。

| 输入 | 期望 | 实际 |
|---|---|---|
| 发起 | 两臂同时激活 | n_a/n_b 都 ACTIVE；2 条 PENDING；汇聚节点 `j` **尚无节点行** |
| 只签臂甲（n_a） | 汇聚不启动，实例仍 PENDING | 实例 PENDING；`j` 行已登记但 `status=PENDING`（等人齐、不建待办）；臂乙待办仍 PENDING |
| 再签臂乙 | 汇聚放行、实例 APPROVED | APPROVED；`j=APPROVED`；e1/e2 都 APPROVED |
| `join_mode=OR` 配在 PARALLEL 汇聚上 → 发布 | 被 R8 拒（并行只许 AND） | 存草稿 200 **但发布 400 / 65014**，报文「必须用 AND 汇聚」 |
| CONDITION 分叉 + `join_mode=OR` 汇聚（先到先走） | 只 1 支到达即放行 | 只激活命中支（n_a），另一支 0 节点行 / 0 待办；签完立刻 APPROVED，`j=APPROVED` |

### B2 条件动态路由（21 条断言 PASS）

图：`s → c1 --(<1万)--> n_low | --默认--> c2 --(<10万)--> n_mid | --默认--> n_high`，三条分支 → 同一 END（**END 显式 `join_mode=OR`**，同种子流程）。

| 输入 amount | 期望走的边 / 节点 | 实际 |
|---|---|---|
| 5000 | `c1→e_low`，`n_low` ACTIVE，其它两节点不激活 | `c1.taken_edge_key=e_low`；n_low ACTIVE；n_mid/n_high 0 行；签完 APPROVED |
| 50000 | `c1→e_c2`、`c2→e_mid`，`n_mid` ACTIVE | 一致；签完 APPROVED |
| 500000 | `c1→e_c2`、`c2→e_high`，`n_high` ACTIVE | 一致；签完 APPROVED |

### B2b（缺陷复现，7 条断言 PASS = 缺陷成立）

| 输入 | 期望（设计口径） | 实际（缺陷） |
|---|---|---|
| 同 B2 的图，但 END **不写** `join_mode`（NULL → 引擎默认 AND），amount=5000 | 实例 APPROVED | 唯一分支 n_low 已 APPROVED，**实例永久 PENDING、END 节点停在 PENDING**；发布期**没有任何报错** |

已建修复卡 **`t_886189f8`**（backend-developer，parents=[本卡]），含三条可选修法（引擎对 END 跳过汇聚闸 / R8 纳入 END 校验 / 默认补 OR）与验收项。

### B3 流程版本冻结（23 条断言 PASS）

| 输入 | 期望 | 实际 |
|---|---|---|
| v1：`s → n_a("审批人甲（旧版）", ROLE=P309_PA) → e`，发布后发起老实例 | 老实例绑 v1、节点名来自旧快照 | 老实例 `version_id=v1`；`node_name=审批人甲（旧版）`；待办派给 u_pa |
| 另存新草稿 → 改名「审批人乙（新版）」+ 审批人改 P309_PB + 新增节点 `n_extra` → 发布 v2 | `def.current_version_id=v2`，版本号 +1 | v2 发布 200；`current_version_id=v2`；`v2.version_no - v1.version_no = 1` |
| 老实例继续签 | 走**旧图**（不出现 n_extra） | 签完实例 APPROVED；`n_extra` 节点行数 0；`node_name` 仍是「审批人甲（旧版）」 |
| 发布后新发起 | 走**新图** | 新实例 `version_id=v2`；节点名「审批人乙（新版）」；待办派 u_pb；签完进入 `n_extra`（ACTIVE）→ 再签 → APPROVED |

---

## 7. C 组：驳回四目标（33 条断言 PASS）

图：`s → n1(甲, P309_PA) → n2(乙: P309_PB 管理员 + P309_PBN 非管理员) → n3(丙) → e`，`approval_mode` 不填（或签）。

| 目标 | 输入 | 期望 | 实际 |
|---|---|---|---|
| `PREV` | n2 驳回（非 admin 也可） | 上一节点 n1 开第 2 轮、重新派待办 | 实例 PENDING；n2 本轮 REJECTED；n1 `round=2` 且 ACTIVE；n1 第 2 轮新 PENDING 待办给 u_pa；`instance.round_total=2` |
| `INITIATOR` | n2 驳回 | 生成 `RESUBMIT` 待办给发起人 | START 节点 round=2；`task_kind=RESUBMIT`、assignee=u_init、PENDING；comment 含「被驳回，请修改后重新提交」；发起人 `/task/todo` 列表可见 |
| `NONE` / `FINAL` | n2 驳回（管理员） | 驳回并终止 | 实例 REJECTED；n2 REJECTED；无新轮次（max round 仍 1）；n3 未激活 |
| `<node_key>` | ①非 admin `u_pbn` 传 `return_to="n1"` | 403 / 65004，无副作用 | **403 / 65004**；max round 仍 1、n1 仍第 1 轮 APPROVED |
| | ②admin `u_pb` 传 `return_to="n1"` | 退回指定节点 | 200；实例 PENDING；n1 `round=2` ACTIVE；第 2 轮新待办生成 |
| | ③admin 传不存在的节点 key | 400 / 65025 | 400 / 65025；实例状态不变 |

---

## 8. 关键 SQL 断言清单（完整见 `raw/21_sql_assertions.txt`）

| 用途 | SQL 要点 | 期望 |
|---|---|---|
| 签法口径 | `select approval_mode from biz_flow_instance_node where instance_id=… and node_key='n_app'` | A1=`ALL`；A7=`<NULL>` |
| 升级留痕 | `select distinct escalated_from_role/escalate_reason/assigned_by_rule from biz_flow_task where instance_id=… and status='PENDING'` | `P309_MAKER` / `SKIP_SELF_APPROVAL` / `ESCALATED` |
| 兜底 | 同上 `assigned_by_rule` | `FALLBACK` |
| 委托 | `select comment ... where assigned_by_rule='DELEGATED'` | `代 P309_DEL u_del 审批（委托至 …）` |
| 自审 SKIP | `select count(*) from biz_flow_task where status='SKIPPED' and comment like '%self_approval=SKIP%'` | ≥1，且 A1 那单=1 |
| 自审审计 | `select count(*) from operation_logs where operation='FLOW_SELF_APPROVAL_ALLOWED' and target_id=<iid>` | 1（VOTED 亦 1） |
| 并行汇聚 | `select max(status) from biz_flow_instance_node where instance_id=… and node_key='j'` | 只签一臂=`PENDING`；两臂签完=`APPROVED` |
| 条件路由 | `select taken_edge_key from biz_flow_instance_node where … node_key in ('c1','c2')` | `e_low` / `e_c2`+`e_mid` / `e_c2`+`e_high` |
| 版本冻结 | `select version_id from biz_flow_instance where id=<old>` vs `select current_version_id from biz_flow_def` | 老= v1、def = v2 |
| 驳回轮次 | `select max(round) from biz_flow_instance_node where instance_id=… and node_key='n1'` | 2 |
| RESUBMIT | `select count(*) from biz_flow_task where instance_id=… and task_kind='RESUBMIT' and status='PENDING'` | 1 |
| 非 admin 403 无副作用 | `select max(round)` / `select status … node_key='n1' round=1` | 1 / APPROVED |

---

## 9. 关键 DB 行快照（原文见 `raw/30_db_snapshot.txt`）

```
# 待办留痕（降级 / 自审）
升级 | P309_BOSS u_b1 | APPROVED | rule=ESCALATED | from_role=P309_MAKER | reason=SKIP_SELF_APPROVAL
升级 | P309_BOSS u_b2 | CANCELLED | rule=ESCALATED | from_role=P309_MAKER | reason=SKIP_SELF_APPROVAL
兜底 | P309_FB u_fb | APPROVED | rule=FALLBACK
委托 | P309_PA u_pa | APPROVED | rule=DELEGATED | P3-09 受托人代 A 审批通过
自审SKIP | P309_THREE u_t1 | SKIPPED | 审批人是发起人本人（self_approval=SKIP），已跳过并留痕

# 自审 ALLOW 审计
FLOW_SELF_APPROVAL_ALLOWED | u_solo | node=一人公司审批 | amount=8888.0 | vote=
FLOW_SELF_APPROVAL_VOTED  | u_solo | node=一人公司审批 | amount=8888.0 | vote=APPROVED

# 并行汇聚 / 条件边 / 缺陷 / 版本
FLW-20261108-… | j=APPROVED | arrived_branches=2/2
P3-09 B2 金额 5000   | c1 -> e_low
P3-09 B2 金额 50000  | c1 -> e_c2 ; c2 -> e_mid
P3-09 B2 金额 500000 | c1 -> e_c2 ; c2 -> e_high
FLW-20261108-B04C3FEA | inst=PENDING | END=PENDING          # B2b 缺陷
FLW-20261108-…  | v1 | P3-09 B3 旧版实例 | node=审批人甲（旧版）
FLW-20261108-…  | v2 | P3-09 B3 新版实例 | node=审批人乙（新版）

# 种子流程的 END 口径（说明缺陷为何没在存量流程暴露）
po_approve | END join_mode=OR
pr_approve | END join_mode=OR
```

---

## 10. 失败项清单

| # | 现象 | 判定 | 处理 |
|---|---|---|---|
| 1 | `B2b`：`END` 未配 `join_mode`（默认 AND）时，条件多分支汇到同一 END → 唯一到达的分支签完后**实例永久 PENDING**，END 停在 PENDING，发布期不报错（校验侧 R8 明确跳过 END，运行期 `step()` 却门控） | **真实缺陷（静默卡单）** | 修复卡 **`t_886189f8`**（`assignee=backend-developer`，`parents=[t_91bba6c3]`），含复现命令、三条可选修法与验收项 |
| — | 其余 15 个场景 / 221 条断言 | 全 PASS | — |

> 非缺陷但记录的边界：`PARALLEL` 汇聚配 `join_mode=OR` 会被 R8 在**发布期**拒绝（400/65014）——这是设计口径（并行只许 AND），不是 bug；「先到先走」的合法形态是 CONDITION 分叉 + OR 汇聚，已在 B1 第 3 图实测通过。

---

## 11. 已知限制 / 复核提示

- 二进制用的是 HEAD `506fa34` 的构建产物（`md5 e1c33cd1…`），与本卡证据同源；若复核时 `backend/src/business/flow/` 已有人改动，请重新构建后再跑。
- 演练只覆盖**流程引擎侧**（定义/发布/运行/待办/委托/统计）；ERP 单据业务回调（采购/收货/应付 bridge）不在本卡范围（P3-08 的 S8 已覆盖回归面）。
- `e2e.py` 可对同一个库重复跑（`mk_def` 会另存新草稿、用户角色会同步成计划口径），但**正式复跑请用 `00_run.sh` 换新库**，避免历史实例干扰计数型断言。
- B2b 的 3 条「【缺陷】」断言在 `t_886189f8` 已按新口径改写为「必须 APPROVED」，并新增 B2c 钉住「并行两支汇到同一个 END 仍要等齐」——见第 12 节。

---

## 12. 修复后复核（`t_886189f8` · END 汇聚口径）

| 项 | 值 |
|---|---|
| 改动 | `backend/src/business/flow/engine.rs`（END 汇聚判据）、`flow_model_node.rs`（`inflight_node_keys`）、`graph.rs`（仅 R8 文档口径，**零快照/哈希改动**） |
| 设计说明 | [`docs/工作流END汇聚语义修复.md`](../../工作流END汇聚语义修复.md) |
| 二进制 | `/root/p309/bin/cloudpivot`，md5 `7df255ef3b8f3dacbd5c207b569e9000` |
| 命令 | `bash docs/evidence/20261108-p309-wf-drill/scripts/00_run.sh`（`OUT_NAME=00_run_fix.txt`） |
| 结果 | **17 个场景 / PASS=243 / FAIL=0**（原 228 + B2b 由 7→10 条 + 新增 B2c 12 条 + 场景自适应） |
| DB 快照 | `raw/30_db_snapshot_fix.txt`：`B2b FLW-…9B77CEB7 | inst=APPROVED | END=APPROVED`、`B2c FLW-…E833D26F | inst=APPROVED | END=APPROVED | 两臂已签=2/2` |
| 端口复核 | `18091 监听行数=0 / health=000` |

场景口径变化：

| 场景 | 修复前 | 修复后 |
|---|---|---|
| B2b（条件三档 → 同一个 END，END 未配 join_mode） | 7 条断言，其中 3 条断言「实例 PENDING / END PENDING」= **缺陷成立** | 10 条断言全部要求 **实例 APPROVED / END APPROVED / 无残留 ACTIVE-PENDING 行** |
| B2c（**新增**：并行两臂 → 同一个 END，END 未配 join_mode） | — | 12 条断言：只签一臂时 END 仍 `PENDING`、实例仍 `PENDING`、另一臂待办不孤儿；两臂签完才 `APPROVED` |

存量图哈希基线：修复前后各取一次 `biz_flow_def_version` 的 `(flow_code, version_no, graph_hash)`，**18 条非演练（种子 / 存量）流程逐行一致**；`p309_*` 是演练自建流程，快照里嵌当次随机的角色/用户 UUID，本来每次跑都不同（与本次改动无关）。

