# 【P0-FIX-2B-B】流程去 ADMIN 化：5 个节点改业务角色 + 发 v2（`t_1e6a8431`）

> 迁移：`backend/migrations/business/V0251__flow_deadmin_assignee_roles.{up,down}.sql`
> 生产：`172.18.10.144 / cloudpivot`（已应用，2026-11-05 15:52 +08）
> 隔离副本：`cloudpivot_deadmin`（99，prod dump 2026-11-05 15:43）+ 隔离实例 `127.0.0.1:18096`

## 1. 一句话

三个流程（`payable_pay` / `stock_in_cancel` / `stock_transfer`）的 **5 个 `assignee_type='ADMIN'` 节点**
改成**业务角色**并各发 **v2**：旧版本行/快照/`graph_hash` 逐字节不动（只 `is_current=false` + `ARCHIVED`）。
生产 `已发布 × 当前版本` 里的 ADMIN 节点 **5 → 0**。

## 2. 逐节点改法（生产实测结果，见 `prod_after.txt`）

| 流程.节点 | 节点名 | 改动前 | 改动后 | 绑定 | empty | self | 审批模式 |
|---|---|---|---|---|---|---|---|
| `payable_pay.n_dept` | 部门主管审批 | ADMIN | ROLE | `DEPT_LEADER` | FALLBACK_ADMIN | ALLOW（不变） | 缺省 |
| `payable_pay.n_fin` | 财务经理审批 | ADMIN | ROLE | `FINANCE_MANAGER` | FALLBACK_ADMIN | ALLOW（不变） | 缺省 |
| `payable_pay.n_board` | 财务总监+总经理会签 | ADMIN | ROLE | `FINANCE_DIRECTOR` + `GENERAL_MANAGER` | FALLBACK_ADMIN | ALLOW（不变） | **ALL（会签，不变）** |
| `stock_in_cancel.n_review` | 作废复核 | ADMIN | ROLE | `WAREHOUSE_MANAGER` | **ERROR**（见 §5） | FORBID（不变） | 缺省 |
| `stock_transfer.n_approve` | 仓管主管审批（同时改名） | ADMIN | ROLE | `WAREHOUSE_MANAGER` | FALLBACK_ADMIN | ALLOW（不变） | 缺省 |

金额分级、`reject_veto`、`join_mode`、`fork_mode`、`timeout_*`、`cc_on_approved`、画布坐标一律未动
（`verify_mig.txt` §3/§4：迁移产出行 vs 应用接口发版行**逐字段一致**）。

## 3. 版本与 hash（不手算）

先在隔离副本库上走**应用内真实接口**发 v2：`POST /def/{id}/draft` → `PUT /version/{vid}/graph` → `POST /def/{id}/publish`
（脚本 `10_build_v2_via_app_api.py`，回执 `build_v2_report.json`，快照 `snap_v2_*.json`）。
`graph_snapshot` / `graph_hash` 由 `graph.snapshot_json()` / `graph.graph_hash()` 生成，迁移只把同一张图种下去。

| 流程 | v1（归档，hash 未变） | v2（当前，新 hash） |
|---|---|---|
| `payable_pay` | `8341eac726949864…` | `5059115327e291c6e1861fa434f7e74d8ccc50e133e79b802cde6f074e2c65d3` |
| `stock_in_cancel` | `78d34c4557630fd8…` | `80cd6c9c76c165a7b75e0be29f163482825dc31eef1f195e0602cd98d3ca4cb1` |
| `stock_transfer` | `e1947064684e326c…` | `7211229eeec38dca63ee28cd0fceb5a13b11ab5e12c94adb87465ad178901ef6` |

`12_verify_migration.py` 自证口径：对**三个库 × 每个版本**算 `sha256(canonical(graph_snapshot))`，
与库里的 `graph_hash` 逐条比对（含 v1、含 V0248 改过的版本）→ 全 OK；并做「v2 行 ↔ 快照」字段级对照。

## 4. 两条路径收敛（`verify_mig.txt`）

| 路径 | 含义 | 结果 |
|---|---|---|
| A | `V0248` → `V0251`（**新部署/发版顺序**，v1 已是 ROLE+ERROR） | v2 三条全部发布 |
| B | 只跑 `V0251`（**生产手工 apply 顺序**，v1 仍是 ADMIN） | v2 三条全部发布 |
| — | A 与 B 的 v2 节点/边/绑定 | **逐字段一致**（含与「接口发版」基准一致） |
| — | 两条路径重跑 `V0251` | 全部 `NOTICE：已有 version_no ≥ 2，跳过` |

`14_down_up_roundtrip.sh`：down → v1 复位 `PUBLISHED/is_current`、v2 行全删；再 up → 回到 v2，hash 不变。

## 5. 与卡面的两处偏差（有意的，必须让上游看到）

1. **`stock_in_cancel.n_review` 的 `empty_assignee` 保留 `ERROR`**，不是卡面写的「一律 FALLBACK_ADMIN」。
   理由：`V0248`（P1-FIX-1，14:29 落地，晚于本卡创建）已把它定为 `ERROR`，并在真机上验过
   「角色 0 人 → 422 + 65018 + 配置指引，没派给 admin」。若改回 `FALLBACK_ADMIN`，单超管库里兜出来的
   正好是**发起人本人**，`self_approval=FORBID` 会当场 422 —— 那正是 P1-FIX-1 修掉的死路。
   ⇒ 4 个节点 `FALLBACK_ADMIN`、1 个（作废复核）`ERROR`。
2. **多角色节点在运行期是候选并集**（architect 澄清）：`n_board` 在 `FINANCE_DIRECTOR` 0 成员时
   **不会**落 `FALLBACK_ADMIN`，而是「只剩总经理那几个人」。B4 第 2 段就是这条的实测。

## 6. 验收对照

| # | 断言 | 证据 |
|:--:|---|---|
| B1 | 5 节点全 `ROLE` + 绑定与上表一致；生产 **ADMIN 节点 = 0** | `prod_before.txt` §1（5 行）、`prod_after.txt` §1/§2（0 行 + 5 行新口径） |
| B2 | 解析人集合 before/after（preview 与运行期同一个 `assignee::resolve`） | `b2_resolve_before_after.txt`（含 `ADMIN 命中 4 行` trace） |
| B3 | 2 张 PENDING 实例的 `biz_flow_task` **一行未变** | `prod_before.txt` §2 vs `prod_after.txt` §4：task id/节点/负责人/创建时间逐条一致；`biz_flow_task` 148→148、实例 113→113、`sys_user_role` 16 行未动 |
| B4 | 隔离 e2e：会签=财务总监+总经理两人；空角色兜底 | `b4_e2e.txt`（PASS，FAIL=0） |
| B5 | `erp_invariants` 63/63 BLOCKER 0 WARN 0；`health=200` | `prod_after.txt` §6 + 本次运行尾部；apply 前后同为 63/63 |

## 7. B2 表（生产口径）

| 节点 | before（ADMIN） | after | 说明 |
|---|---|---|---|
| `n_dept` | admin / Henry.Liao / 老六（+停用的老六测试号，trace「ADMIN 命中 4 行」） | **Henry.Liao**（DEPT_LEADER） | 部门主管角色现有 2 人，其中 1 人停用 |
| `n_fin` | 同上 4 人 | **兜底 4 个超管**（`FALLBACK_ADMIN 命中 4 个超管`） | 财务经理 **0 成员** ⇒ 当前仅超管兜底，必须配人 |
| `n_board` | 同上 4 人（**会签**） | **Henry.Liao 一人**（GENERAL_MANAGER；财务总监 0 人） | 名义会签 2 岗，实际只有总经理一档 |
| `stock_in_cancel.n_review` | 同上 4 人 | **Henry.Liao**（WAREHOUSE_MANAGER） | 角色 0 人时 422（ERROR），不再派超管 |
| `stock_transfer.n_approve` | 同上 4 人 | **Henry.Liao**（WAREHOUSE_MANAGER） | — |

> 结论口径：本卡解决的是「审批人来源不再是无差别 4 个超管」，**没有**解决「财务经理/财务总监岗位空缺」
> —— 那是组织问题（决策文档 D-6 / N3）。

## 8. 现场与回滚

- 生产备份：`/root/flowdeadmin-backup-<TAG>/flow_tables.sql`（5 张流程表，apply 前）。
- 回滚：`psql -1 -f V0251__...down.sql`（有单据跑在 v2 上则 `RAISE EXCEPTION` 拒绝）。
- 生产**未**写 `schema_migrations` 记账：下次 `deploy.sh` 会把 V0251 当待跑迁移执行，届时三条 `NOTICE 跳过`
  并正常记账（`prod_after.txt` 同款幂等输出已实测）。

## 9. 顺带发现（不在本卡范围，供上游决策）

1. **`FALLBACK_ADMIN` 兜底不过滤停用账号**：`n_fin` 兜底时给 `laoliu_test`（`status=0`）也落了一条待办
   （`b4_e2e.txt` §3 三条 PENDING 含 `laoliu_test`）。ROLE 来源会过滤停用，ADMIN 来源不会 —— 这是既有行为，
   本卡没改，建议单独开卡评估（摘权后若仍有停用超管，兜底会把待办派给停用账号）。
2. `V0248` 尚未上线；它下次部署时会对 `stock_in_cancel` 的**归档 v1** 做就地修正（其守卫只看 v1 hash）。
   对运行期无影响（实例钉 v2），但归档行的 `graph_hash` 会从 `78d34c…` 变成 `026766…` —— 属 V0248 既定行为。
