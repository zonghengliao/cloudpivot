# t_a6e8aca3 证据：审批角色缺 `business:flow:approve` → 审批链永远推不动

> 卡：【流程权限种子缺口】审批角色（DEPT_LEADER/FINANCE_MANAGER/GM/FD）没有 business:flow:approve
> 结论：**根因是「授权迁移跑在建角色之前」的执行顺序**，不是漏给某个角色一行。新增
> `V0239__flow_approver_role_grants` 一条迁移固化授权，并在 `system::menu::permission_audit_tests`
> 里加 N19 常驻断言。全新空库真机实测：部门主管 + 财务经理把一条预算审批**批到 APPROVED**、
> 预算变 ACTIVE；撤掉授权重启后同一账号立刻回到 403（复现卡面现象）。

## 1. 根因（三段事实，都可复核）

1. `…0800–…0804` 五个实例侧权限点由 `V0124__flow_instance_permissions.up.sql` 种下，该文件第 33–37 行
   明确写「**只种权限点，不替任何角色授权**」。
2. 唯一给角色授过实例侧菜单的是 `V0128__flow_menus.up.sql` 第 45–58 行
   （`…0861 发起审批` / `…0862 审批中心` →「授权给所有未删除的角色」），但它按
   `SELECT … FROM sys_role` 展开 —— 执行时**全库还没有任何业务角色**：
   种子 `S__role_data.sql` 只种 SUPER_ADMIN，而 nik-db 的执行序是
   `versioned(1) → repeatable(2) → seed(3)`，种子在所有版本化迁移**之后**。
   空库上这三条 INSERT 影响 **0 行**（实测 manual2：`menu_id=…0860/0861/0862` 授权行 = 0）。
3. 业务审批角色是后来由 `V0182`（DEPT_LEADER / PURCHASE_MANAGER / GENERAL_MANAGER /
   WAREHOUSE_MANAGER）、`V0191`（FINANCE_MANAGER / FINANCE_DIRECTOR）、
   `…/FLOW_CONFIG_APPROVER` 种出来的，**没有任何迁移回头补授权**。

⇒ 所有审批岗位角色手里一条菜单授权都没有（approve / instance:read / instance:create 全缺，
连「审批中心」菜单都不显示）。超管靠 `is_super_admin` 短路，验收时完全看不见 ——
与 V0124 文件头记的「超管有短路，所以缺口在验收里是隐形的」同一个坑。

复算（应用本迁移前，`cloudpivot_manual2`）：

```
SELECT count(*) FROM sys_role_menu WHERE menu_id='20000000-0000-0000-0000-000000000803';  -- 0
SELECT count(*) FROM sys_role_menu WHERE menu_id='20000000-0000-0000-0000-000000000862';  -- 0
```

## 2. 改了什么

| 文件 | 作用 |
|------|------|
| `backend/migrations/business/V0239__flow_approver_role_grants.up.sql` | 授权补种 + fail-closed 自检（下详） |
| `backend/migrations/business/V0239__flow_approver_role_grants.down.sql` | 按节点清理这 8 个节点的授权 + 自检 |
| `backend/src/system/menu/permission_audit_tests.rs` | 新增 **N19**：每个 `APPROVAL`/`COUNTERSIGN` 节点的 `ROLE` 指派角色都必须有 `business:flow:approve` |

授权口径（V0239 三段）：

1. `business:flow:approve`（…0803）→ **所有会被指派为审批人的角色**：
   ① 数据驱动扫 `biz_flow_node_assignee`（`APPROVAL`/`COUNTERSIGN` + 版本 `is_current`&`PUBLISHED`
   + 流程定义 `PUBLISHED` 未软删）；② 并上 7 个标准审批岗位 role_code 兜底
   （DEPT_LEADER / PURCHASE_MANAGER / GENERAL_MANAGER / WAREHOUSE_MANAGER / FINANCE_MANAGER /
   FINANCE_DIRECTOR / FLOW_CONFIG_APPROVER）—— 覆盖「先建角色、流程还没接上」的空窗。
2. `instance:create/read/withdraw`（…0800/…0801/…0802）+ 审批流目录/发起/审批中心
   （…0860/…0861/…0862）→ **所有未软删角色**：与 V0128 第 45 行原意逐条一致，这次按「当下存在的角色」补；
   withdraw 的行级限制仍在 handler（只许发起人本人）。
3. `business:flow:admin`（…0804）→ 只给 SUPER_ADMIN（与 `S__workflow_permissions.sql` 的「写四码只授超管」同口径）。

自检（fail-closed，修不好就炸掉启动）：

- 权限点本身仍在用（软删/停用/无码 → 失败）；
- **每个被审批节点指派的在用角色都有 approve**（修完再失败 = 逻辑错，直接拦升级）；
- 每个未软删角色都有 `instance:read`；
- 8 个节点上必须真的存在授权行（防空转）；
- 流程定义里指派指向「已删除/已软删角色」的行只 `RAISE WARNING` 点名（客户配置问题，不该拦住升级）。

## 3. 真机验收（全新空库，非克隆、非演示库）

| 项 | 值 |
|----|----|
| 库 | `cloudpivot_pg239_fresh`（`createdb -O alun` 空库，首启 **应用 180 个迁移**） |
| 实例 | `./target/debug/cloudpivot`，`127.0.0.1:18161`，profile=test |
| 账号 | `admin`(SUPER_ADMIN) 制单/提交；`e2e_dept_leader`(DEPT_LEADER)、`e2e_fin_mgr`(FINANCE_MANAGER) 审批 |
| 流程 | `budget_approve`（种子流程），预算 12000 元 → `n_dept` → `n_fin` |

结果（完整日志见 `10-e2e-run.log.txt`，`PASS=19 FAIL=0`）：

| 步骤 | 断言 | 实测 |
|------|------|------|
| ⑤ | 部门主管 `GET /api/business/flow/task/todo` | **200**，列表含该待办（`business:flow:instance:read` 已授）→ `05-leader-todo.json` |
| ⑥ | 部门主管 `POST /api/business/flow/task/{id}/approve` | **200** `node_status=APPROVED`，链推进到财务经理 → `06-leader-approve.json` |
| ⑦ | 财务经理 `POST …/approve` | **200** `instance_status=APPROVED` → 桥接回调后 `biz_budget.status=ACTIVE` → `07-finmgr-approve.json` |
| ⑧ | 反向对照：`down.sql` 撤权 + 重启 → 同一账号对新待办再点 | **403** `{"code":403,"msg":"无权限访问，需要权限: business:flow:approve"}`（卡面原样复现）→ `08-approve-after-down.json` |
| ⑨ | 复原：重跑 `up.sql`（幂等 `INSERT 0 0`）+ 重启 → 同一账号再点 | **200** → `09-approve-after-up.json` |

`10-e2e-run.log.txt` 摘要：

```
PASS 待办落在部门主管（task=01a1a853-…）
PASS GET /api/business/flow/task/todo → 200 且含本任务（business:flow:instance:read 已授）
PASS POST /api/business/flow/task/…/approve → 200（不再 403 需要权限 business:flow:approve）
PASS 审批链推进到财务经理
PASS 财务经理 approve 200
PASS 审批实例 APPROVED
PASS 预算随桥接回调 ACTIVE（审批链真的推到底了）
PASS 撤权后同一个部门主管再点 → 403（复现卡面现象）
PASS 重新授权后同一账号 → 200（幂等复原成功）
PASS=19 FAIL=0
```

### 迁移本身的幂等 / down

- 幂等：同一库连跑两次 `up.sql` → 第二次 `INSERT 0 0`（三段全空转），`role_menu` 总数不变（339 → 339）。
- down：`DELETE 110`，8 个节点清零，其余授权不动（339 → 229 = 原 233 − 手工补的 4 行 approve），自检通过。

## 4. `sys_dept` 0 行：**不补种子**（结论 + 理由）

`cloudpivot_manual2` 现在有 2 行（`FIN 财务部` / `PUR 采购部`），是上一张卡 `t_42d844c0`
用 `POST /api/system/dept` 现场建的演示数据，不是种子。本卡判断**不该由迁移种部门**：

1. `sys_dept` 是纯客户组织主数据，表里**没有** `is_system` / 来源标记，一旦种进客户库就与客户自建部门
   无法区分（对比 `biz_unit` 那类有 `is_system` 才敢种）；
2. 部门有完整 CRUD（`system:dept:create/update/delete` + 前端 `views/system/dept/index.vue`），
   首次上线「先配组织、再录业务」是产品既有设计（用户 `sys_user.dept_id` 同样可空、同样由管理员建）；
3. 预置审批流本身不依赖具体部门：`assignee_type=DEPT_LEADER` 是「发起人所在部门的主管」，
   运行时按 `sys_user.dept_id` 解析，库里有没有部门都能跑（本卡 e2e 就是在空库先建一个部门再建单的）。

因此**空库照样能验收**：`POST /api/system/dept` 建一个部门即可（本卡 e2e ② 就是这么做的，`dept_code=E2EFIN`）。
若产品后续希望「开箱就有部门」，应当单独开卡并同时给 `sys_dept` 加 `is_system` 列，而不是在权限卡里夹带。

## 5. 门禁

| 检查 | 命令 | 结果 |
|------|------|------|
| 新增用例 | `SYS_DB_URL=…/cloudpivot_pg239_fresh cargo test --lib system::menu::permission_audit_tests` | **6 passed, 0 failed**（N19 在空库/manual2 都是 26 处 ROLE 指派全绿） |
| 菜单模块全量 | `SYS_DB_URL=… cargo test --lib system::menu -- --test-threads=1` | **20 passed, 0 failed** |
| N19 是否假绿 | 把 V0239 授权清掉后在 `cloudpivot_pg239_t` 上跑 | **FAILED**，逐条点名 `po_approve/n_dept→DEPT_LEADER` 等 26 处（真防线） |
| rustfmt | `rustfmt +1.94.1 --edition 2024 --check src/system/menu/permission_audit_tests.rs` | exit 0 |
| clippy | `cargo clippy -- -D warnings`（lib+bins 口径） | 通过 |
| 演示库 | 同库另起进程只跑迁移（不重启 18097） | `数据库迁移完成: 应用 3 个迁移`（V0237/V0238 是别的卡已入库的迁移，V0239 是本卡） |

> ⚠️ 并行跑 `system::menu` 时 n14 会偶发红：n13 造的 `N13-<tag>` 临时菜单在**并行兄弟用例进行中**
> 被 n14 的全表扫描命中（既有测试夹具干扰，与本卡改动无关；`--test-threads=1` 全绿）。

## 6. 复现

```bash
# ① 空库 + 全量迁移（含 V0239）
su postgres -c "createdb -O alun cloudpivot_pg239_fresh"
bash scripts/boot_fresh.sh          # 起 127.0.0.1:18161 的实例，日志里看「应用 180 个迁移」

# ② 真机链（建部门 → 建预算 → 部门主管审 → 财务经理审 → 撤权 403 → 复原 200）
bash scripts/e2e_live_v3.sh

# ③ 演示库只补迁移、不重启别人的 18097
bash scripts/apply_to_manual2.sh
bash scripts/verify_manual2.sh
```

## 7. 已知边界（写清不埋坑）

- 授权行没有「来源」列，`down.sql` 只能按**菜单节点**清理：升级前若有人手工勾过这 8 个权限点，
  回滚会一并删掉（回滚只用于开发/演练环境，生产先导出 `sys_role_menu`）。
- 主体权限在 **JWT 里**（登录时装载），进程内 LRU 缓存 TTL 300s（profile=test `[cache] type="local"`）。
  所以「库里改完授权、不重启」最多有 5 分钟脏读窗口；真实上线路径是「迁移 + 重启」，无此问题
  （本卡 ⑧/⑨ 就是靠重启复现/复原的）。
- 本迁移只覆盖**种子里那 7 个审批岗位**与「流程定义里 ROLE 指派的角色」。管理员在 UI 新建自定义审批角色时，
  仍需在角色树里勾上 `business:flow:approve`（这是既有授权语义，未改动）。
- 新增种子流程若引入新的 ROLE 指派角色，必须同批补 `approve` 授权，否则 N19 变红。
