# t_92234f6b · 演示种子缺口：5 账号无部门（N2 WARN）+ 普通员工无 `expense:submit`（员工自助提报 403）

- 卡号：`t_92234f6b`（REVERIFY `t_2f92ee87` 发现③）
- 分支/提交：`main`
- 改动性质：**只动种子 / 权限数据**（无业务规则、无迁移号段、无前端改动）
- 证据平台：**145 测试机**（全新库 `cloudpivot_seed_emp` + 8053 隔离实例）
  - 144 生产：**零读写**（本轮未执行任何 144 命令）
  - 145 8033 服役实例 / `cloudpivot_dev` / 8049-8052 其它卡实例：**未触碰**（只按端口找 8053 的 PID）

---

## 1. 结论（先看这里）

| 验收项 | 结果 | 证据 |
|--------|------|------|
| ① 全新库 `NIK_SEED_DEMO_USERS=true` 首启后 N2 = 0 | ✅ `erp_invariants --group 权限` 2/2 通过，N2 ✅，BLOCKER 0 / WARN 0；DB 直查 `n2_violations = 0` | `raw/seedemp_inv_perm.txt`、`raw/145_db_assert.txt` ①/② |
| ② `demo_sales` 提报 → `demo_lead` 待办审批 → `demo_finance` 付款 全链走通 | ✅ 真机 39/39 断言通过（含审批人来源 `INITIATOR_SUPERIOR` = demo_lead、单据 APPROVED → PAID、凭证借贷平衡） | `raw/seedemp_chain.txt`、`raw/145_db_assert.txt` ⑦/⑧ |
| ③ 种子幂等不变：同库重启 → `created=0 existed=9` | ✅ 第二次启动日志逐字：`created=0 existed=9 bindings_added=0 dept_created=false dept_backfilled=0`，`应用 0 个迁移` | `raw/145_boot_seed.log` |
| ④ `cargo test` 全绿 + 145 curl/DB 留证 + 不碰 144 + 不改已应用迁移 | ⚠️ 全量 `3373 passed / 4 failed`，**4 条全部可追溯到他人未提交的「采购退货」WIP**（`V0332` + `purchase_return_handler.rs`，均为未跟踪文件），本卡改动面 0 失败；两条 DB 相关的失败在「不含 V0332 的干净树」库上实测 **ok**。详见 §5 | `raw/seedemp_cargo_test.txt`、`raw/attribute_p16_failure.txt` |
| 顺带：`报销` 不变量组 | ✅ 9/9 通过，BLOCKER 0 / WARN 0 | `raw/seedemp_inv_exp.txt` |

---

## 2. 根因

### ① 5 个账号没有部门

`seed_demo_users` 的 `DemoUser` 结构体上有个 `in_demo_dept: bool` 开关，原意是
「审批岗不必进部门」；实测后果远超设计意图：

- `sys_user.dept_id IS NULL` ⇒ `principal::RESOLVE_SQL` 解析不出 `dept_id` ⇒
  `data_scope=3` 的部门类角色被 `merge_scope_rows` **静默降级成「仅本人」**
  （S11 分支）——「没部门」不是缺个字段，而是**权限静默缩水**；
- 资产建卡强制 `use_dept_id`、折旧按部门核算、预算按 `(部门, 科目)` 槽唯一 ——
  这些账号只要经办就 400；
- `erp_invariants` 的 N2 把**所有**在用 HUMAN 账号（含超管 `admin`）算进来 ⇒
  每次回归挂一条 WARN。

而 `admin` 根本不在 `DEMO_USERS` 里（它由 `bootstrap::seed_super_admin` 建），
所以「把 9 个账号改成 in_demo_dept=true」**单靠改清单仍然修不干净**。

### ② 普通员工没有 `expense:submit`

`V0258` 的角色矩阵里 `business:erp:expense:submit` 只授给
`DEPT_LEADER` / `FINANCE_MANAGER` / `FINANCE_DIRECTOR` —— 矩阵里**根本没有「员工」这一档**。
于是全新演示库上「员工提报 → 部门经理审批」只能财务代提才跑得通；而部门经理本人提报
又撞职责分离 65018（已由 `t_0127714a` 单独修「提交即回滚」）。这不是回归，是覆盖缺口。

---

## 3. 修法（含取舍）

### ① 部门：删掉「不进部门」这个开关（取卡面建议 1）

- `DemoUser.in_demo_dept` **删除**，9 个账号建号即落 `DEPT_DEMO`；
- 分部门的语义靠**岗位 / 角色**表达（销售主管、财务主管、总经理都是角色码），
  组织树上同属「演示事业部」，`sys_dept.leader_id` 仍只有 `demo_lead` 一人；
- 新增「④ 部门补漏」：对 9 个演示账号 + `admin` 只做
  `dept_id IS NULL → 演示部门`，**已有部门的一行不碰** ⇒ 幂等 + 老库升级可修；
- `bootstrap` 里 `seed_super_admin` 与 `seed_demo_users` 从并发改为**按序**：
  实测两者并发时 `admin` 常常后落地（`tokio::join!` 不保证顺序），
  补漏那一句会 `UPDATE 0` 行 —— 这是本轮实测踩到的真坑（见 §6）。

### ② 权限：新建 `EMPLOYEE`（员工自助）角色，而不是塞给 `SALES_CLERK`（取卡面建议 2 的后者）

| 方案 | 取舍 |
|------|------|
| 把报销码授给 `SALES_CLERK` | 改动最小，但「销售员」不是「员工」的同义词：本仓还有采购员 / 仓管 / 未来任何岗位，授给 `SALES_CLERK` 只是把缺口挪到 403 的下一个人身上 |
| **新建 `EMPLOYEE`（采用）** | 横切的**身份**角色（与岗位角色叠加）：一个员工可以同时是 `SALES_CLERK` 和 `EMPLOYEE`。管理员可把任意员工加进去，缺口一次修净；代价是多一个角色 + 11 行授权行 |

- 落地形态：**可重复种子** `backend/migrations/seed/S__employee_self_service_permissions.sql`
  （不占版本号，老库下次启动自动补上；`ON CONFLICT` + `NOT EXISTS` 幂等）；
- 授权 11 行菜单（含祖先目录闭包 —— 缺了非超管侧边栏整棵子树被静默丢弃，V0258 第五节）：

  ```
  财务管理(目录) → 费用报销(目录) → 我的报销(页)
                                   ├─ 新建 / 编辑 / 删除 / 提交 / 详情（5 个按钮）
                                   └─ 费用类型(页，建单下拉取数)
  审批流(目录) → 审批中心(页)       （报销详情抽屉看审批进度）
  ```

- 整包只用**既有** 8 个权限码（`expense:list/create/update/delete/submit/detail`、
  `expense-type:list`、`flow:instance:read`），**不新造码** ⇒ 前端
  `views/business/erp/expense/index.vue` 的 PERM 表一行不用改（卡面建议 3 自然满足）；
- **不授** `expense:pay` / `expense:cancel` / `expense-type:create|update|delete`
  —— 员工的边界是不碰钱、不碰账（种子自检 3.5 反向断言「无越权码」）；
- `data_scope = 4`（仅本人）。⚠ 口径说明：报销单列表的行级范围只声明了部门列
  （`expense_model::REPORT_TARGET`），`scope::filter_for` ③ 对「仅本人」在无归属人列时
  回落为本部门 —— 这是**既有实现**,本卡不改（改它等于改业务规则）；
- `demo_sales` / `demo_purchase` / `demo_wh` 追加 `EMPLOYEE`（岗位角色 + 身份角色叠加）；
  演示部门负责人 `demo_lead` **不**挂 `EMPLOYEE`（他提报会撞 65018，单测 ⑧ 守着）。

### ③ 生产默认不受影响（但要说清「哪一半会落地」）

- **演示账号 / 部门补漏**：`NIK_SEED_DEMO_USERS` 缺省关闭 ⇒ 不建账号、不跑补漏；
- **`EMPLOYEE` 角色与授权行**：属可重复种子，**任何库下次启动都会落地**
  （包括 144 生产）。落地的是一档**0 人绑定**的角色 + 11 行授权 ⇒ 对既有用户零行为变化，
  只是角色树里多一个可勾选项。这是有意为之：真实客户也需要「员工自助」这一档，
  否则同样只能让财务代提。
- 因此「N2 = 0」只在 `NIK_SEED_DEMO_USERS=true` 的库上成立。**只有 admin 的库
  （`false`）里超管仍无部门 ⇒ N2 仍 WARN 1 条**，与修前完全一致 ——
  给生产超管分配哪个部门是客户的**组织树决策**，不该由种子开关悄悄替他决定。
  本卡只负责让「演示库开箱即用」这条路径不再挂 WARN。

---

## 4. 145 真机实测（全部真实输出，见 `raw/`）

### 4.1 全新库首启（`cloudpivot_seed_emp` / 8053）

```
db created: cloudpivot_seed_emp
health=200 after 4s
2026-11-07T22:24:30.160140Z INFO nik_web::app: 数据库迁移完成: 应用 264 个迁移
2026-11-07T22:24:30.718769Z INFO cloudpivot::system::setup::model: 演示部门补漏：账号原先没有部门，已归入演示部门 username="admin" dept="DEPT_DEMO"
2026-11-07T22:24:30.718804Z INFO cloudpivot::bootstrap: 演示账号种子完成（幂等：已有账号不重复建） created=9 existed=0 bindings_added=12 dept_created=true dept_backfilled=1
```

> `dept_backfilled=1` 正是 `admin`：演示账号建号即带部门（created 里就带上了），
> 而超管不经过 `DEMO_USERS`，只能靠补漏这一步。

### 4.2 员工自助全链（39/39）

```
[①] 10 个在用 HUMAN 账号全部 DEPT_DEMO（admin + 9 个演示账号）；n2_violations = 0
[授权] EMPLOYEE|员工自助|data_scope=4|status=1；持有 expense:list/create/update/delete/submit/detail
       + expense-type:list + flow:instance:read；未持有 expense:pay
② demo_sales 建单 200 → reporter_id=demo_sales、dept_id=DEPT_DEMO（不是 NULL）
② demo_sales 提交 200（曾经 403）→ 单据 SUBMITTED、生成实例、待办审批人 = demo_lead
③ demo_lead 待办列表里看到该实例 → 审批 200 → 单据 APPROVED
④ demo_finance 付款 200 → pay_status=PAID、已过账凭证借贷平衡差异 = 0
⑤ 反向：demo_sales 付款 403 / 作废 403 / 删已审批单非 200
```

审批轨迹（`raw/145_db_assert.txt` ⑧）：

```
 node_key |  assignee_source   | assignee  |  status  |              comment
----------+--------------------+-----------+----------+------------------------------------
 n_sup    | INITIATOR_SUPERIOR | demo_lead | APPROVED | SEEDEMP 部门经理同意（员工自助链）
```

### 4.3 幂等（同库二次启动）

```
2026-11-07T22:24:47.171511Z INFO nik_web::app: 数据库迁移完成: 应用 0 个迁移
2026-11-07T22:24:47.558567Z INFO cloudpivot::bootstrap: 演示账号种子完成（幂等：已有账号不重复建）
    created=0 existed=9 bindings_added=0 dept_created=false dept_backfilled=0
```

### 4.4 不变量

```
erp_invariants --group 权限 : 结果 2/2 通过  🚨 BLOCKER 0  ⚠️ WARN 0     （N2 ✅ 在用 HUMAN 账号必须有部门归属）
erp_invariants --group 报销 : 结果 9/9 通过  🚨 BLOCKER 0  ⚠️ WARN 0
```

---

## 5. 本地（99）回归 + cargo test

- `cargo fmt --check`：干净（只格式化本卡改的 4 个文件，未触碰他人改动）；
- `cargo clippy --all-targets`：**本卡 4 个文件 0 新增告警**（既有 8 条全在
  `sales_order_model/asset_linkage/fx_revaluation/opening/purchase_requisition_dto/voucher/year_end_linkage_tests`，
  均为他人代码，本卡未改）；
- `cargo test --lib system::setup`：**8/8**（新增/改写 3 条单测：演示部门必须容纳全部 9 个账号、
  补漏名单必须含 admin、至少一个非负责人账号持 EMPLOYEE）；
- **全量 `cargo test --lib -- --test-threads=1`（种子完备库 `cloudpivot_test_seedemp`，
  由 `scripts/verify/make_full_suite_db.sh` 用本卡二进制造库）：
  `3373 passed; 4 failed`**（`raw/seedemp_cargo_test.txt`）。

### 4 条 FAILED 的归属：**全部是他人未提交的「采购退货」WIP，与本卡无关**

| 失败用例 | 直接原因 | 归属证据 |
|----------|----------|----------|
| `p16_graph_snapshot_selfcheck_is_clean` | 审批图自检报 `flow_code="prt_approve"` **RowsEdited**（stored `9debbf6…` ≠ recomputed `7cd8074…`） | `prt_approve` 只存在于 `backend/migrations/business/V0332__purchase_return.up.sql`，而该文件在工作区是**未跟踪**（`??`）；`git grep prt_approve 3022a2f -- backend/migrations/` = **0 处**。在「不含 V0332 的干净树」建的库上复跑 → **1 passed**（`raw/attribute_p16_failure.txt` ④） |
| `doc_kind_registries_match_doc_kind_table` | `STATUS_ENTITY` 行数 SQL 26 vs Rust 25 | 多出来的那行是 V0332 种进 `biz_doc_kind` 的 `PURCHASE_RETURN`；同一干净库复跑 → **ok** |
| `frontend_ts_mirror_matches_rust` | Rust `DocKind::ALL` 比前端 `doc-kinds.ts` 多一个（同样是 `PURCHASE_RETURN`） | 同上，属其 WIP 的枚举同步未完成 |
| `wired_triggers_match_call_sites` | 断言原文点名 `src/business/erp/purchase_return_handler.rs`：「调了引擎但没登记」 | 该 `.rs` 同样是**未跟踪**的新文件；本卡提交 `3022a2f` 里没有它 |

> 结论：本卡改的是「种子 / 权限数据」，**0 处流程定义、0 处联动登记、0 处 DocKind 枚举**，
> 4 条失败无一触及本卡改动面。补齐这 4 条要等采购退货那张卡自己的 WIP 收口
> （它在共享工作区里还没提交，我无法也不应替它改）。

- 99 全新库冒烟（`cloudpivot_seedemp_0621`）：链路断言 39/39；同库二次启动
  `created=0 existed=9 dept_backfilled=0`；把 3 个账号部门抹成 NULL 后第三次启动
  `dept_backfilled=3`、N2 `3 → 0`（老库升级路径实证）。
- `python3 scripts/verify/role_code_gate.py` → `✓ 无新增/改动的未知角色码`，退出码 0
  （新种子的 `EMPLOYEE` 走 `INSERT INTO sys_role … VALUES` 形状，被闸自动并入白名单）。

---

## 6. 本轮踩到的坑（写下来免得下一个人再踩）

1. **`tokio::join!` 不保证顺序**：`seed_super_admin` 与 `seed_demo_users` 并发时，
   `admin` 常在演示账号之后才落地 ⇒ 部门补漏 `UPDATE 0` 行，全新库 N2 仍 WARN 一条。
   修法是把两步串成 `async { seed_super_admin().await; seed_demo_users().await }`
   放进同一个 `join!` 分支（其它步骤照旧并发）。
2. **「先建角色再授」的种子顺序**：本卡的种子文件名 `S__employee_*` 排在
   `S__task_*` / `S__workflow_*` 之前，于是那些「授给所有未软删角色」的种子会
   顺手把任务/项目/偏好菜单也授给 `EMPLOYEE`（26 行菜单而非 11 行）——
   这是**期望行为**（任务管理本来就人人可用），别把它当越权；`expense:pay` 等码
   由本卡自检 3.5 反向守护。
3. **`role_code_gate.py` 只解析 `INSERT INTO sys_role … VALUES …`**：新种子必须用
   `VALUES + ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING` 形状，
   否则同一文件的授权段会被判「未知角色码」而闸红。

---

## 7. 改动清单

| 文件 | 改动 |
|------|------|
| `backend/src/system/setup/model.rs` | 删 `in_demo_dept`（9 个账号全部入演示部门）；新增 `EMPLOYEE_ROLE_CODE` / `dept_required_usernames()` / 「④ 部门补漏」；`DemoSeedOutcome` 增 `dept_backfilled`；3 条单测改/增（共 8 条） |
| `backend/migrations/seed/S__employee_self_service_permissions.sql` | **新增**：`EMPLOYEE` 角色（data_scope=4）+ 11 行菜单授权 + 5 段自检（角色/授权真持有/无孤儿按钮/无越权码/码有菜单承载） |
| `backend/src/bootstrap.rs` | `seed_super_admin` → `seed_demo_users` 按序；启动日志增 `dept_backfilled` |
| `backend/src/system/setup/{dto,handler}.rs` | `SetupDemoUsersRes` 增 `dept_backfilled`（加性字段，接口向后兼容） |
| `frontend/**` | **0 改动**（权限码未变，按钮显隐沿用既有 PERM 表） |
| `backend/migrations/business/V03xx` | **0 新增**（不占号段、不改已应用迁移） |

## 8. 复跑清单

```bash
# 145：全新库首启（库不存在才建，不 DROP）
scp docs/evidence/20261108-t92234f6b-seed-employee/scripts/{20_fresh_145.sh,21_idem_145.sh} root@172.18.10.145:/tmp/
ssh root@172.18.10.145 'bash /tmp/20_fresh_145.sh cloudpivot_seed_emp 8053 true fresh'

# 145：员工自助全链（39 断言）
ssh root@172.18.10.145 'set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a; \
  cd /opt/cloudpivot/docs/evidence/20261108-t92234f6b-seed-employee/scripts && \
  BASE=http://127.0.0.1:8053 DB=cloudpivot_seed_emp PGUSER=$NIK_DATABASE_USER \
  PGPASSWORD=$NIK_DATABASE_PASSWORD ADMIN_PW=$NIK_ADMIN_PASSWORD CHANGED_PW=SeedEmp@2026 \
  python3 sev_chain.py'

# 145：幂等 + 不变量
ssh root@172.18.10.145 'bash /tmp/21_idem_145.sh cloudpivot_seed_emp 8053 second'
ssh root@172.18.10.145 'set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a; cd /opt/cloudpivot && \
  NIK_DATABASE_NAME=cloudpivot_seed_emp python3 scripts/verify/erp_invariants.py --group 权限'

# 99：全量 lib 套件（种子完备库）
ERP_DB=cloudpivot_test_seedemp bash scripts/verify/make_full_suite_db.sh
U='postgres://alun:alun@127.0.0.1:5432/cloudpivot_test_seedemp'
cd backend && ERP_DB_URL=$U SYS_DB_URL=$U TASK_DB_URL=$U P0FIX_DB_URL=$U cargo test --lib -- --test-threads=1
```

> ⚠ `sev_chain.py` 会把演示账号的 `must_change_password` 走完（口令改成 `SeedEmp@2026`），
> 并在库里留下 1 张 PAID 的报销单 + 1 条付款凭证 —— 都在一次性库 `cloudpivot_seed_emp` 里，
> 不影响任何共享库；复跑同一库时它新建另一张（不依赖上一轮数据）。
