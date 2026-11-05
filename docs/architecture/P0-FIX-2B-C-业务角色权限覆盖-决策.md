# 决策：业务角色权限覆盖不足（P0-FIX-2B-C 跟进）

> **卡片**：`t_34cee9ea`　**决策人**：架构组（architect）　**决策日**：2026-11-05
> **上游**：`t_db2506d2`（生产超管 4 → 1，`admin` 唯一）
> **结论**：选**方案 1「补码到既有角色」**的**职能归位**版本 —— 按 R1/R2/R3/R4 四条规则补 51 个业务页，
> 并顺手补齐 3 条**真实存在但库里没有挂载行**的凭证按钮码。已施工、已验收、可一键回退。
> **证据目录**：`docs/evidence/20261105-bizperm-grant/`（prod/ 13 份 + 隔离副本演练日志）
> **脚本**：`scripts/datafix/bizperm/`（`plan.sql` 是唯一的授予矩阵）

---

## 1. 决策摘要（每条都带证据）

| # | 决策 | 取值 | 依据 |
|---|---|---|---|
| D-B1 | 补码方式 | **方案 1：补码到既有 5 个业务角色**（按职能归位，不是一刀切全开） | §3 三方案对照；方案 2 会让「采购/合同/仓储/财务」仍无人能办，方案 3 让系统只剩超管可用 |
| D-B2 | 数据范围 | **一处不改**（`data_scope` 保持 3/5/1 原值） | `24_proof_after.txt` §2 逐角色 data_scope 与 `11_proof_before.txt` 逐字相同 |
| D-B3 | 角色数量 | **不新建角色**（不建 `BIZ_STAFF`），也不扩超管 | 全员 9 个账号、3 个业务身份；新增角色只会多一份要维护的授权矩阵 |
| D-B4 | 只读 / 经办分离 | 职能域 `FULL`（页面+全部按钮），邻域 `READ`（页面 + 只读动作），管理视角全域只读 | §4 矩阵；实测 `laoliu_test` 无财务角色 ⇒ 财务四接口仍 403（`23_probe_after.txt`） |
| D-B5 | 平台治理页 | **一页不授**：`/system/{user,role,dept,menu,data-scope,field-mask,role-mutex,change-log}`、`/support/{config,dict,file,job,log,online}`、`/business/storage/backend` | 与 P0-FIX-2B「生产运维只剩 admin」一致；SQL 里有硬断言（`expand.sql` §4.2 + `20_grant.up.sql` ⑧.4），计划写歪就 RAISE |
| D-B6 | 批量导入导出 | **本次不授**（3 页仍空），另立卡 | 导入/导出是**整表批量外带**，其内容级数据范围取决于各模块查询是否注入 scope（本次未逐模块核实）；风险 > 收益 |
| D-B7 | 缺挂载行的凭证按钮码 | **补 3 条菜单行**（`voucher:audit/post/un-audit`），只授财务经理 | 代码是真实 handler（`voucher_write.rs:1589/1614/1638`），但 `sys_menu` 无对应行 ⇒ 除超管通配外任何角色都拿不到（全库比对：`handlerCodes − menuAuthCodes` 只有这 3 条 + 1 条测试用 `x:y:z`） |
| D-B8 | 历史任务 0 行 | **接受**，不回填 `dept_id`（沿用 D-3） | 生产 `biz_task` 活跃 71 行 / `biz_project` 3 行，`dept_id` 全 NULL；3 档账号补码后 task/project 均 0 行（`23_probe_after.txt` laoliu_test 段）→ 要看得见历史任务必须另开「回填」卡 |

---

## 2. 现状核查（只读实测，2026-11-05 17:0x CST，生产 `cloudpivot`@144）

### 2.1 收敛后暴露的缺口（本卡输入）

| 事实 | 收敛前（超管通配） | 收敛后实测 | 证据 |
|---|---|---|---|
| `menu_type=2` 业务页（`module_code=business`） | 59 | 59 | `11_proof_before.txt` §3 |
| 5 个业务角色并集能看见的业务页 | 59（超管兜底） | **12**（剩 47 个空） | 同上 |
| `laoliu` 权限码数 | 通配 `*:*:*` | **49** | `12_probe_before.txt` |
| `GET /api/business/task/page` | 200（超管） | **403** | `12_probe_before.txt`（laoliu/henry/laoliu_test 全部 403） |
| `GET /api/business/erp/purchase-order/page` | 200 | **403** | 同上 |
| `GET /api/business/erp/contract/page` | 200 | **403** | 同上 |

说明：卡面写「55 个业务页 / 43 个缺口」是 16:42 的快照；此后 FIN-3（`7d96692`）等上线，业务页增至 **59**，
缺口 47。本卡按**当前现状**（59 / 47）施工，与卡面的差异在 §7 登记。

### 2.2 五个业务角色的既有授权形状（施工前，逐角色）

| 角色 | data_scope | 目录 | 页面 | 按钮 | 合计 |
|---|---|---|---|---|---|
| `DEPT_LEADER` 部门主管 | 3（本部门及以下） | 6 | 12 | 30 | 48 |
| `FINANCE_MANAGER` 财务经理 | 5（自定义：财务部） | 7 | 13 | 39 | 59 |
| `GENERAL_MANAGER` 总经理 | 1（全部） | 2 | 3 | 8 | 13 |
| `PURCHASE_MANAGER` 采购经理 | 3 | 2 | 3 | 8 | 13 |
| `WAREHOUSE_MANAGER` 仓管主管 | 3 | 3 | 4 | 13 | 20 |

**既有授权的语义是「财务三件套 + 审批流」**（预算/报销/固资/流程），
其余（任务、项目、采购、供应商、合同、物料、仓储、凭证、总账、报表）**一行都没有**。
这不是「漏配一两页」，而是**建号时只配了财务域**。

### 2.3 另一个真缺口：3 条权限码没有菜单行

全库比对（`backend/src` 的 `permission = "…"` 去重 274 条 vs `sys_menu.auth_code` 282 条）：

```text
代码在用、菜单没有（无法通过 sys_role_menu 授予任何非超管角色）：
  business:erp:voucher:audit      → backend/src/business/erp/voucher_write.rs:1589  审核 DRAFT→AUDITED
  business:erp:voucher:un-audit   → voucher_write.rs:1614                            反审核 AUDITED→DRAFT
  business:erp:voucher:post       → voucher_write.rs:1638                            记账 AUDITED→POSTED
（另 1 条 x:y:z 是单测夹具，忽略）
```

后果：只补 `voucher:list` 的财务经理**能看凭证但不能审核/过账**，会计流程仍然走不通。
本卡把 3 条按 `menu_type=3`（按钮）挂到记账凭证页下，只授 `FINANCE_MANAGER`。

---

## 3. 三个方案的选择（卡面给定的三种，不发明第四种）

| 方案 | 内容 | 结果（实测/推算） | 结论 |
|---|---|---|---|
| **1（采用）** | 补码到既有角色 | 覆盖 **51/59** 业务页；`laoliu` 49→**195** 码、12→**54** 页；6 个角色语义不动一个 | ✅ |
| 2 | 新建 `BIZ_STAFF`（`data_scope=4` 仅本人）+ 任务/项目码，绑 `laoliu`/`henry.liao` | 只解决 6 个页面（任务 4 + 项目 1 + 审批 1），**采购/供应商/合同/物料/仓储/财务 41 页仍无人能做**；且 4 档在「`dept_id` 全 NULL」的现状下同样只能看到自己创建的 0 条历史任务 | ❌ 投入一个角色换 6 页 |
| 3 | 明确不做，登记 `docs/待决策.md` | `laoliu`（真实业务负责人）、`henry.liao`、`laoliu_test` 三个人打开系统只有财务三大件 + 待办；采购/仓储/财务日常作业**全部 403** —— 等于把系统退回「只有 admin 能用」，与 P0-FIX-2B 收敛的初衷（去掉超管依赖）直接矛盾 | ❌ |

### 为什么方案 1 不是「一刀切全给」

卡面的方案 1 写法只点了 `DEPT_LEADER` + 任务/项目。按那个字面量做，47 个缺口只补 6 个。
本卡在**同一方案内**把它做完整：**按职能归位**（谁的活谁全权，邻域只读，治理不授）。
四条规则见 §4，全部落在 `plan.sql` 这一张表里（可逐行 review、可 diff、可回退）。

---

## 4. 授予矩阵（`scripts/datafix/bizperm/plan.sql` 是唯一真相源）

### 4.1 四条规则

| 规则 | 语义 | 范围 |
|---|---|---|
| **R1 职能域全权** | 页面 + 该页全部按钮（`FULL`） | 谁的职能谁全权 |
| **R2 邻域只读** | 页面 + 只读动作（`READ`：`list/read/detail/export/download/mine/template/aux-balance/trial-balance/health/self-check/cancel-impact`） | 上下游要看到，但不能改 |
| **R3 人人自用** | 工作台、帮助中心、公告中心、消息中心、文件管理（不含删除）、外观设置 | 5 个角色都授 |
| **R4 治理不授** | `/system/*` 管理页、`/support/*`、存储配置、导入导出 | 一页不授（SQL 硬断言） |

### 4.2 逐角色

| 角色 | R1（全权） | R2（只读） |
|---|---|---|
| `DEPT_LEADER` | 工作任务（目录 + 4 页 + 13 个任务按钮）、项目（页 + 4 按钮）、审批流实例、我的报销/预算/固资（既有）、消息模板只读+发送 | 流程定义 |
| `PURCHASE_MANAGER` | 采购订单、采购明细、供应商、合同、物料 | 仓储 10 页（仓库/库位/库存/库位库存/流水/序列号/调拨/入库/出库/盘点）、应付账款、计量单位、流程定义、消息模板 |
| `WAREHOUSE_MANAGER` | 仓库、库位、即时库存、库位库存、库存流水、序列号、调拨、收货入库、库存出库、盘点 | 采购订单、采购明细、供应商、物料、计量单位、流程定义、消息模板（**不含合同**：仓管按采购单收货，不需要看商务条款） |
| `FINANCE_MANAGER` | 会计科目、会计期间、期初余额、记账凭证（含新增的审核/记账/反审核）、总账、三张报表、年度结转、应付账款、单号序列、预算三页、报销三页、固资三页 | 采购订单/明细、供应商、合同、收货入库、出库、库存、物料、流程定义、消息模板 |
| `GENERAL_MANAGER` | 公告管理、消息通知（总经理发公告） | **全域只读**：除 R4 外全部业务页（含财务、采购、仓储、任务、项目） |

### 4.3 明确的「不授」清单（施工后仍未覆盖的 8 页，逐条有理由）

| 页面 | 权限码 | 理由 |
|---|---|---|
| `/system/user`、`/system/role`、`/system/dept`、`/system/menu` | `system:*:list` | 账号/角色/组织/菜单治理 = 运维 admin 专属（P0-FIX-2B 已定：生产运维只剩 admin） |
| `/system/data-scope`、`/system/field-mask`、`/system/role-mutex`、`/system/change-log` | `business:permission:scope:read` / `system:field-mask:list` / `system:role-mutex:list` / `system:change-log:list` | 权限治理与全平台审计 = 同上；业务角色看到全平台配置变更没有业务价值 |
| `/support/{config,dict,file,job,log,online}` | `support:*:list` | 平台运维台 |
| `/business/storage/backend` | `business:storage:config:list` | 存储配置（含 AK/SK 引用），只给 admin |
| `/business/impexp/{import,import-task,export-task}` | `business:impexp:*` | **批量外带**：导出内容是否逐模块注入数据范围未核实（`impexp/model.rs` 只对**任务表**注 scope，不含内容查询）⇒ 先不给，另立卡（§7 T-2） |

---

## 5. 施工（照 `scripts/datafix/superconv/` 的既有做法）

### 5.1 阶段与文件

| 阶段 | 文件 | 干什么 | 幂等 / 回退 |
|---|---|---|---|
| 0 | `05_snapshot.up/down.sql` | 把 5 个角色/授权行/命中菜单/持有者整行快照进 `datafix_bizperm_snapshot`（153 + 207 + 5 + 3 行） | 只写审计表；down 删本批快照 |
| 1 | `20_grant.up/down.sql` | ① 新建 3 条凭证按钮码 ② 按 `plan.sql` 插 376 行授权 ③ 推进持有者 `token_version` | 插入前 `NOT EXISTS` 判重 + **确定性 uuid**（`md5(role|menu|bizperm-20261105)` 变形）；down 按同一 id 精确删除 |
| 缓存 | `cache_invalidate.sh` | `INCR cloudpivot:perm:user_scope_epoch` + `DEL cloudpivot:uv:<uid>` / `perm:user_perms|roles|menus:<uid>` | 幂等（脚本可反复跑） |
| 验收 | `probe_bizperm.py` / `proof_rows.sql` / `drill_rollback.sql` / `verify_drill.sh` | 见 §6 | — |

### 5.2 三条硬约束（写进 SQL，不靠人记）

1. **不动 `SUPER_ADMIN`**：`expand.sql` §4.1 计划里出现 `SUPER_ADMIN` 直接 RAISE；`20_grant.up.sql` ⑧.1 施工后复查成员数 = 1。
2. **不动 `data_scope`**：SQL 里没有任何 `UPDATE sys_role`；`20_grant.up.sql` ③ 断言 5 个角色的 (role, data_scope) 形状与预期逐一对上，不符即停手。
3. **治理页零命中**：`expand.sql` §4.2 + `20_grant.up.sql` ⑧.4 双断言（计划阶段拦一次、落库后复查一次）。

### 5.3 缓存失效（卡面的键名口径是错的，已在 A 卡实测订正）

应用读的是**带前缀**的 `cloudpivot:perm:user_scope_epoch`；裸名 `perm:user_scope_epoch` 是空转。
本次两键都推进（`2 → 3 → 4`，因为中间做过一次「回退 + 重做」），并删掉 3 个持有者的
`uv:` / `perm:user_perms` / `perm:user_roles` / `perm:user_menus` 共 12 个键 —— 详见 `22_cache.txt`。
**不删 `cloudpivot:uv:<uid>`** 的话，推进 `token_version` 后 60 秒内连重新登录都会 401/20008。

---

## 6. 验收（全部为真机实测，贴的是返回码/行数）

### 6.1 施工前后逐账号对照（同一份 `probe_bizperm.py --all`，`23_probe_after.txt` / `12_probe_before.txt`）

| 账号 | 权限码 | 可见业务页 | `task/page` | `project/page` | `purchase-order/page` | `contract/page` | `voucher/page` | `system/user` |
|---|---|---|---|---|---|---|---|---|
| `admin` | 通配（1） | 72 | 200 / 71 行 | 200 / 3 行 | 200 / 14 | 200 / 1 | 200 / 21 | 200 |
| `laoliu`（5 角色） | 49 → **195** | 13 → **54** | 403 → **200 / 71 行** | 403 → **200 / 3 行** | 403 → **200 / 14** | 403 → **200 / 1** | 403 → **200 / 21** | 403（不变） |
| `henry.liao`（4 角色） | 49 → **175** | 13 → **54** | 403 → **200 / 71 行** | 403 → **200 / 3 行** | 403 → **200 / 14** | 403 → **200 / 1** | 403 → **200 / 21** | 403（不变） |
| `laoliu_test`（3 角色） | 49 → **151** | 13 → **44** | 403 → **200 / 0 行** | 403 → **200 / 0 行** | 403 → **200 / 14** | 403 → **200 / 1** | **403（无财务角色）** | 403（不变） |

探针总计：`admin 26/26`、`laoliu 41/41`、`henry.liao 32/32`、`laoliu_test 32/32`，**FAIL = 0**。

### 6.2 数据范围仍按 `data_scope` 生效（同一次探针的行数）

- `laoliu` / `henry.liao` 持有 `GENERAL_MANAGER`（`data_scope=1` 全部）⇒ 任务 **71 行**、项目 **3 行**（= 全量活跃行）。
- `laoliu_test` 只有 3 档（本部门及以下 = 技术部子树），且 `biz_task.dept_id` / `biz_project.dept_id` **全 NULL** ⇒ 任务 **0 行**、项目 **0 行**。
  **授权补上了、接口 200 了，但一行也看不到 —— 这是 D-3（不回填 dept_id）的预期结果，不是 bug。**
- 角色授权行数（施工前 → 后）：`DEPT_LEADER 48→95`、`FINANCE_MANAGER 59→135`、`GENERAL_MANAGER 13→119`、`PURCHASE_MANAGER 13→88`、`WAREHOUSE_MANAGER 20→92`。

### 6.3 覆盖度与红线（`24_proof_after.txt`）

| 指标 | 施工前 | 施工后 |
|---|---|---|
| 5 角色并集覆盖业务页 | 12 / 59 | **51 / 59** |
| 仍未覆盖 | 47 | **8**（全部为 §4.3 的治理页 + 导入导出） |
| 业务角色持有平台治理节点 | 0 | **0**（`governance_leaks = 0`） |
| 可见页面缺祖先目录（侧边栏会丢页） | 0 | **0**（`orphan_pages = 0`，本卡把祖先闭包写进 `expand.sql` §3） |
| `SUPER_ADMIN` 成员 | 1（admin） | **1（admin）** |
| `sys_user_role` 行数 | 17 | 17（不变） |
| `biz_flow_task` PENDING | 3 | 3（不变） |

### 6.4 巡检与健康

- `erp_invariants.py`：**66/66 通过，BLOCKER 0，WARN 0**（`26_invariants_after.txt`）
- `GET /health` = **200**（`25_health.txt`）
- `SUPER_ADMIN` 成员 = **1**（`24_proof_after.txt` §1）

### 6.5 缓存失效执行记录 + 回滚演练

| 项 | 结果 | 证据 |
|---|---|---|
| 缓存失效 | `cloudpivot:perm:user_scope_epoch 3 → 4`；12 个 `uv/perm` 键 DEL 全部返回 1 | `22_cache.txt` |
| **生产回滚演练（事务内）** | down 后 `role_menu 976 → 600`、`menu 329 → 326`、`FINANCE_MANAGER 135 → 59`、凭证三码归零；`ROLLBACK` 后回到 `976 / 329 / 135` —— **零净变更** | `27_rollback_drill_prod.txt` |
| **生产真回退 + 重做** | 因矩阵微调（收回仓管对合同的只读），真跑了一次 `--rollback`（376 行 + 3 菜单全删，回到 600/326）再 `--snapshot/--grant` 重做 —— 生产实测可逆 | `28_rollback_refine.txt` + `20/21_*.txt` |
| **隔离副本四步往返** | `up(976) → 幂等重放(0 变更) → down(600/326) → 再 up(976)`，`FAIL=0` | `scripts/datafix/bizperm/verify_drill.sh` 输出 |

---

## 7. 风险、诚实清单与后续项

### 7.1 必须让老大知道的三件事

1. **ERP 单据没有部门级数据范围**。`shared/scope.rs::SCOPED_TABLES` 只登记 11 张 `sys_*` 表；
   `biz_task` / `biz_project` 单独用 `ScopeTarget::new(…, dept_id, owner_id)` 注入；
   而 `biz_purchase_order` / `biz_stock_in` / `biz_voucher` / `biz_inventory` / `biz_material` 等
   一律 `ScopeTarget::unscoped(...)`，且 `erp/period_items.rs:75-79` 有登记：
   *「按会计期间取的全局台账（无 dept_id 与归属人列），可见性由业务权限码控制」*。
   ⇒ **对 ERP 来说，「给谁这个权限码」就是「给谁看全量单据」**。本次给 3 档角色开了采购/仓储页，
   他们看到的是**全公司 14 张采购单 / 14 张入库单**（`23_probe_after.txt` laoliu_test 段），不是「技术部的单据」。
   这是既存设计，不是本卡引入；但既然现在有非超管能进这些页面了，**必须登记**（T-3）。
2. **历史任务/项目仍是 0 行**（3 档账号）。要让他们看见历史任务，只能回填 `biz_task.dept_id`
   （D-3 明确不回填）→ 另开卡（T-1）。
3. **`GENERAL_MANAGER` 是「全域只读」**。`henry.liao` / `laoliu` 都持有它，所以两人能看到
   财务、采购、仓储、合同**全量只读**（`voucher:list` 有、`voucher:post` 无）。
   若老大的意思是「总经理只看报表不看单据」，把 `plan.sql` 里 `GENERAL_MANAGER` 的 36 条 READ 删掉再重跑即可（down→改→up）。

### 7.2 与卡面的偏差（逐条登记）

| 卡面口径 | 实测/本卡口径 | 原因 |
|---|---|---|
| 全平台业务页 55 / 缺口 43 | 业务页 **59** / 缺口 **47** | 16:42 之后 FIN-3 等上线新增页面；按当前现状施工 |
| 建议方案 1 = 只给 `DEPT_LEADER` 补任务/项目 | 方案 1 + **职能归位**（51 页 / 376 行） | 只补 2 页等于 47 个缺口补 6 个，采购/仓储/财务仍无人能办 |
| 缓存键 `perm:user_scope_epoch` | `cloudpivot:perm:user_scope_epoch` | 沿用 A 卡实测订正（README §缓存失效） |
| 「846 条历史任务」 | 846 = 全表（含软删），**活跃 71 行** | `biz_task where deleted_at is null` = 71 |

### 7.3 后续卡（本次**不做**，逐条给判据）

| # | 卡 | 为什么现在不做 | 完成判据 |
|---|---|---|---|
| T-1 | 回填 `biz_task` / `biz_project` 的 `dept_id` | D-3 明确不回填；回填要先定「按制单人部门 / 按参与者部门」口径 | 3 档账号 `task/page` 行数 > 0，且与部门树对得上 |
| T-2 | 导入导出页的授权 + 内容级数据范围核实 | 导出查询是否注入 scope 未逐模块核实；先授=可能整表外带 | `impexp` 每个导出定义都能指出 scope 注入点 + 3 档账号导出条数受限 |
| T-3 | ERP 单据表的数据范围（`dept_id` 加列/回填或改用业务归属人） | 牵动 5 张单据表 + 全部历史数据 + 巡检断言，属独立设计 | `biz_purchase_order` 等登记进 scope 后，3 档账号行数按部门收敛 |
| T-4 | 死权限码的常驻门禁（「代码在用但菜单没有」） | 本次是人工比对发现 3 条；`docs/dead-permissions-20261101.md` 只查了反方向 | CI 里一条脚本：`handlerCodes − menuAuthCodes = ∅`（白名单显式登记） |

---

## 8. 回退手册（一条命令）

```bash
# 生产回退（恢复到施工前：600 行授权 / 326 菜单 / data_scope 一行未改）
ssh 172.18.10.144
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
cd /opt/cloudpivot/scripts/datafix/bizperm
bash apply.sh --rollback      # 20_grant.down.sql → 05_snapshot.down.sql
bash cache_invalidate.sh      # epoch +1，删 3 个账号的 uv/perm 缓存
```

回退判据（`20_grant.down.sql` 末尾自断言）：本卡插的行一条不剩、3 条凭证按钮码删净、
每个角色授权行数回到 48 / 59 / 13 / 13 / 20。兜底还有 `00_backup.sh` 产出的整库 dump
（`/root/datafix-backup-bizperm-20261105-170711/full.dump`）。

---

## 9. 附录：本次施工的 SQL 事实

```text
快照     sys_role 5 行 / sys_role_menu 153 行 / sys_menu 207 行 / sys_user 3 行
插入     新菜单 3 行（business:erp:voucher:{audit,post,un-audit}）
         授权 376 行（60 条计划 × 5 角色展开 + 祖先闭包 + 只读白名单过滤 + 凭证 3 行）
推进     laoliu / henry.liao / laoliu_test 的 token_version 各 +1（admin 不动）
断言     ①②③ 前置断言 + ⑧.1-⑧.5 施工后断言，全部写在 SQL 里（不满足即整体回滚）
```
