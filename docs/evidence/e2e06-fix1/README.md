# E2E06-FIX-1 证据：报销人附件权限 + 预算审批部门主管节点（卡 t_1fafa2ae）

验证环境：**145 `cloudpivot-test`（`/opt/cloudpivot`，库 `cloudpivot_dev`，后端 `127.0.0.1:8033`）**
部署提交：`c6a8ee38`（`git pull` + `cargo build` 51.9s + `systemctl restart cloudpivot-test` + health 200）
验证日期：2026-11-09 11:09–11:15（CST）

---

## 一、问题与改法

### 问题①（P2）报销人本人没有附件上传权限 → 员工自助报销提交不了

| 事实 | 修前 | 修后 |
|------|------|------|
| `business:attachment:upload` 的持有角色 | SUPER_ADMIN / FINANCE_DIRECTOR / FINANCE_MANAGER / PURCHASE_MANAGER / WAREHOUSE_MANAGER / GENERAL_MANAGER | **+ EMPLOYEE** |
| `business:attachment:list` | 同上 6 个 | **+ EMPLOYEE**（详情附件面板 / 提交前 `/attachment/check` 的读码） |
| `business:attachment:delete` | FINANCE_DIRECTOR 等 | **仍不给 EMPLOYEE**（上传人删自己的由 handler 行级放行） |
| 员工提交报销 | 传不了发票 → 提交必然 400「必须上传发票影像或费用凭证」 | 本人传 → 提交 200 |

改法：`backend/migrations/seed/S__employee_self_service_permissions.sql`（repeatable 种子）
授权 11 行 → 13 行、8 码 → 10 码；自检同步升级，并新增两条**反向**断言
（EMPLOYEE 不得持有 `business:attachment:delete`、`business:erp:expense:pay`）。

> 为什么不是版本化迁移：EMPLOYEE 角色本身就是这个种子种的，版本化迁移在空库首启时
> `sys_role` 还是空的（迁移先于 seed，见 V0212 文件头记的同一坑）→ 授权会插 0 行。

### 问题②（P2）预算审批的「部门主管」节点是全局角色会签

| | 修前（v1） | 修后（v2） |
|---|---|---|
| `n_dept.assignee_type` | `ROLE`（DEPT_LEADER） | `INITIATOR_SUPERIOR`（`superior_levels=1`） |
| `n_dept.approval_mode` | `ALL`（会签） | `ANY`（或签，一人同意即通过） |
| 候选人数 | 角色全体成员（任意部门） | **发起人所在部门的 `leader_id`（1 人）** |
| 坏号影响 | 任一成员不可用 → 预算永远批不完 | 与其它部门无关；部门没负责人时 `FALLBACK_ADMIN` 兜底 |

**为什么不是 `DEPT_LEADER`**（卡面原建议）：`flow/assignee.rs` 里 `DEPT_LEADER` 的语义是
「`biz_flow_node_assignee` 里**指定部门**的 `leader_id`」——部门是**发布期写死**的，一张流程图
接所有部门就等于「A 部门的预算永远派给写死的那个部门」，仍是错配；而且发布校验 R14
（`graph::check_assignees`）把 `DEPT_LEADER` 归入**可枚举来源**，必须落部门行，空行直接拒发布。
「随发起人变化的部门」只能由 `INITIATOR_SUPERIOR` 表达：`superior_levels=1` = 沿 `dept_path`
取**本级**（`index = levels-1 = 0`）部门的 `leader_id`。这与 `expense_approve.n_sup`
（V0243「直属上级审批」）同一口径，不是新机制。

改法：`backend/migrations/business/V0355__budget_approve_dept_leader_per_dept.{up,down}.sql`
——走「归档 v1 → 发 v2」，**v1 的行 / 快照 / graph_hash 逐字节不动**（已发布版本只读），
v2 的快照与 hash 在库内现算（`pg_temp.canonical_json` 与 Rust `graph::snapshot_json()` /
`graph_hash()` 逐字同义），并先自检「canonical(v1 快照) 的 sha256 == v1 的 graph_hash」校准口径，
再落 v2。幂等（已有 v2+ 则跳过）、客户改过图则 NOTICE 跳过、down 有实例在 v2 上则拒绝回滚。

### 附带：口令指纹（卡面验收③）

* `backend/src/system/auth/handler.rs`：`Crypto::verify_password` 返回 `Err`（库里存的不是
  argon2 PHC 串）从 **500「服务器内部错误」改为 401「用户名或密码错误」** + `WARN` 日志点名账号
  （登录、改密两处）。脏数据不该表现为「服务器挂了」，也不该绕过登录失败留痕。
* `scripts/verify/erp_invariants.py`：新增不变量 **N3**（`sys_user.password` 必须 `$argon2id$` 前缀），
  报警时列出账号 + 指纹前缀 + 处置路径；已接入每日 `invariant_audit` 巡检。

---

## 二、验证结果（全部为 145 真机真实 HTTP + 真实库）

| 验证项 | 结果 | 证据文件 |
|--------|------|----------|
| 迁移 up / 幂等 / down / 再 up（隔离克隆库） | 全通过 | `migration_probe_clone.out` |
| 行 ↔ `graph_snapshot` ↔ `graph_hash` 三方一致 | **三个 sha256 完全相同**（`769e6112…`） | `graph_consistency.out` |
| 只持 EMPLOYEE 的账号自助报销全链路 | **21/21 PASS** | `employee_only.out` |
| 部署后：坏口令登录 401（改前 500）+ 日志点名 | 401 / `20001` + `WARN …username=fix1_badpw_probe` | `post_deploy.out` |
| E2E-06 全套重跑（A+B+勾稽） | **PASS=206 FAIL=0**（修前 193 条全绿，本卡新增 13 条断言） | `e2e06_asserts.log` / `e2e06_run_all.out` / `e2e06_reconcile.log` |
| 菜单不变量门禁 | 8 条全绿（菜单总数 432，title/排序/图标无违例） | `menu_invariants.out` |
| ERP 不变量「权限」组 | N1/N3 ✅（N2「22 个账号无部门」为演练库既有 WARN，与本卡无关） | `erp_invariants_authz.out` |

### 关键断言摘录（`e2e06_asserts.log`）

```
PASS  报销人持有 EMPLOYEE（附件上传权限载体） = 1
PASS  上传发票影像 draft http = 200          （此前 403）
PASS  n_dept 恰有 1 条待办（不再全员会签） = 1
PASS  n_dept 待办归属本部门主管 e2e06_mgr = e2e06_mgr
PASS  其他 DEPT_LEADER 成员没有被派到 n_dept（部门维度不再错配） = 0
PASS  n_dept 审批人来源=INITIATOR_SUPERIOR（发起人所在部门） = INITIATOR_SUPERIOR
PASS  n_dept 审批模式=ANY（或签，一人同意即通过） = ANY
PASS  编制人自批被内控拒绝 code=65019 = 65019
PASS  全库口令指纹均为 argon2id = 0
```

### 「只持 EMPLOYEE」账号的完整链路（`employee_only.out`）

账号 `e2e06_emp_only`（`role_ids = [EMPLOYEE]`，仅此一个角色）：

```
PASS 该账号角色数（应为 1，且是 EMPLOYEE） = 1|EMPLOYEE
PASS 建报销单 http = 200
PASS 查附件闸 http = 200 / 闸判定（缺发票 → 不放行） = false
PASS 本人上传附件 http（曾经 403） = 200
PASS 上传人姓名快照（不是 UUID） = E2E06 纯员工
PASS 附件列表条数 = 1 / 闸复查（有发票 → 放行） = true
PASS 提交 http = 200 / 单据状态 = SUBMITTED
PASS 审批实例已起（直属上级 = e2e06_mgr） = e2e06_mgr
PASS EMPLOYEE 权限里没有 attachment:delete = 0
PASS EMPLOYEE 权限里没有 expense:pay = 0
PASS 本人调付款接口被拒 http = 403
```

### 迁移来回（隔离克隆库 `cloudpivot_fix1_probe`，不碰 `cloudpivot_dev`）

```
应用前 : v1 hash=0bc6441c… ver=1 / n_dept=ROLE+ALL
up     : v1→ARCHIVED（hash 不变）/ v2 PUBLISHED current（hash=769e6112… / 节点 11 / 边 13 / 绑定 7 行）
再 up  : NOTICE 已有 version_no ≥ 2 → 跳过（0 变更）
down   : v1 复位 PUBLISHED current / v2 行边绑定全清（残留 0/0）
再 up  : current=v2（可重复升降）
```

---

## 三、改动清单

| 文件 | 说明 |
|------|------|
| `backend/migrations/business/V0355__budget_approve_dept_leader_per_dept.up.sql` | 预算审批 v2：n_dept = 发起人部门主管 + 或签（含 canonical 序列化与全部自检） |
| `backend/migrations/business/V0355__budget_approve_dept_leader_per_dept.down.sql` | 回滚（有实例在 v2 上则拒绝） |
| `backend/migrations/seed/S__employee_self_service_permissions.sql` | EMPLOYEE + attachment:upload / list（13 行 / 10 码，含反向越权断言） |
| `backend/src/system/auth/handler.rs` | 坏口令指纹：500 → 401 + WARN 点名（登录 / 改密） |
| `scripts/verify/erp_invariants.py` | 新增 N3 口令指纹不变量 |
| `scripts/verify/e2e06_budget_common.sh` | upfile 改回报销人本人；新增 e2e06_planner（属演练部门）与口令指纹修复；budget_active 按待办归属分派 token |
| `scripts/verify/e2e06_budget_a.sh` | n_dept 断言改为「只派本部门主管 + INITIATOR_SUPERIOR + ANY + 65019」 |
| `scripts/verify/fix1_migration_probe.sh` | 克隆库上跑 up/down/幂等/再 up 的探针 |
| `scripts/verify/fix1_graph_consistency.sql` / `fix1_graph_check.py` | 行↔快照↔hash 三方一致性校验（与 Rust 快照规则逐字同义） |
| `scripts/verify/fix1_employee_upload_check.sh` | 「只持 EMPLOYEE」账号的端到端验收（21 条断言） |
| `scripts/verify/fix1_post_deploy_check.sh` | 部署后：版本状态 / 坏口令 401 / 指纹统计 |
| `scripts/verify/fix1_collect_evidence.sh` | 一键收集上述输出到 `/tmp/fix1_evidence/` |

---

## 四、残留与注意事项（如实记录）

1. **生产（144）尚未部署**：本卡只在 145 验证。下次 144 发布时 `V0355` 会自动执行
   （届时 v1→v2，历史实例继续读 v1 快照，不影响在途单据）；`S__` 种子是 repeatable，
   启动即补齐 EMPLOYEE 的两条附件权限。
2. **演练库口令**：`cloudpivot_dev` 有 14 个历史夹具账号的 `password` 是占位符（`x` /
   `p2c1-not-a-r` 之类），本卡在演练脚本里把它们统一重置为演练口令哈希（这些号原本就登不进去，
   无信息损失）。**生产请走「系统管理 → 用户 → 重置密码」**，N3 巡检会把这类行点出来。
3. **新增演练残留**（会在下次 `e2e06_run_all.sh` 的 reset 里按 `title like 'E2E06%'` 清掉）：
   用户 `e2e06_emp_only`（只持 EMPLOYEE）+ 1 张 SUBMITTED 报销单 + 1 个附件；
   用户 `fix1_badpw_probe`（已停用 status=0，口令指纹已复原）。
4. **p3a_0002 不再参与预算审批**：修好后「部门主管」按发起人部门解析，`p3a_0002` 无部门
   （`dept_id IS NULL`）会退化成超管兜底。演练改用 `e2e06_planner`（DEPT_LEADER + 属演练部门），
   也因此**不再需要**上一个卡临时重置 `p3a_0002` 密码哈希的补丁。
5. **`n_dept` 的节点名仍是「部门主管审批（编制）」**：语义已变为「发起人所在部门主管」，
   节点名保持原样以减少图 diff；v2 的 `change_note` 与本文说明了差异。
6. **自批红线不变**：`self_approval=ALLOW` 保留，真正的否决仍是
   `budget_bridge::guard_self_approval`（403 + 65019）+ `flow::internal_control` 的 SKIPPED 留痕，
   本次实测「编制人自批 → 403 + 65019」。
