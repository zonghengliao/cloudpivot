# ERP 存量债收口（闸④ 域外 biz_type / 闸⑦ 孤儿按钮 / cost_labor 过期用例）

卡：`t_c49010bf`（来源：HR-48 全量回归 `t_fb2e95f4` 的「非 HR 存量债」清单）
环境：真库只在 **145**（库名 `debt49_drill`，跑完 DROP）；99 不建库（迁移实例与用例都经 ssh 隧道打到 145）
日期：2026-11-13

---

## 0. 一句话结论

| 项 | 之前 | 现在 | 证据 |
|---|---|---|---|
| 闸④ `biz_type_domain.py --strict` | ❌ `biz_flow_def.biz_type` 4 个域外值 | ✅ PASS（23 种取值全在域内） | `01_gate4_biz_type_domain_after.txt` |
| 闸⑦ `orphan_button_gate.py` | ❌ 2 对（SALES_CLERK × statement 按钮）**+ 1 对**（HR_MANAGER × flow:approve，见 §3） | ✅ 孤儿 0 对 | `02_gate7_orphan_after.txt` |
| `cost_labor_db_tests` ⑮ | ❌ 前提过期（「直到 ARCH-9-2 上线」）**且**真库上 500 | ✅ 25 passed / 0 failed | `06_cost_labor_module.txt` |
| `expense_linkage_tests::lnk05_*`（6 条） | ❌ 全红（65018 无可用审批人） | ✅ 6 passed / 0 failed | `07_lnk05_tests.txt` |
| 闸⑥ 死权限码 / 闸⑧ 角色码 / menu_invariants | 绿 / 绿 / 绿 | ✅ 仍绿（闸⑧ 另修了闸自身的解析缺口，见 §4） | `03` `04` `05` |
| L0 静态闸 | 绿 | ✅ rc=0（role_code_gate 自检 39/39） | `09_check_l0.txt` |
| L1（fmt / clippy / typecheck） | — | ✅ clippy rc=0、typecheck rc=0；`cargo fmt --check` **仅**因并行卡的未提交新文件 `hr/fixture_hygiene.rs` 缩进红（不是本卡文件，未动） | `10` `11` `12` |

---

## 1. 闸④：`biz_flow_def.biz_type` 4 个域外值（补登记，不映射）

4 个值 `SO_APPROVE` / `PR_APPROVE` / `PRT_APPROVE` / `COST_SETTLE` 来自
V0281 / V0331 / V0332 / V0381，各自是 bridge 里的 `pub const BIZ_TYPE`（单测逐字钉住），
只是 `doc_kind.rs::FlowBizType` 漏登记。

**处置：补登记 20 → 24，存量行一字未改。** 理由（完整版见
`docs/architecture/P2-5-业务类型词表盘点.md` §10 与
`docs/architecture/snapshot-drift/NOTE-20261113-biz-flow-def-4-域外值.md`）：

- `biz_type` 是 bridge 认领审批实例的唯一凭据，也是审批档案的一部分 —— 改值等于把
  「销售订单审批」记成「采购订单审批」，审批详情 / 通知 / 联动 / 报表全部串味；
- 4 个字面量本来就写在代码常量和单测里，词表只是漏收编，补登记不引入新行为；
- 对象轴如实映射：`SO_APPROVE→SALE_ORDER`、`PRT_APPROVE→PURCHASE_RETURN`、
  `COST_SETTLE→COST_PERIOD`、`PR_APPROVE→None`（采购申请在对象轴无成员，不硬塞）。

涟漪同步：`doc-kinds.ts` 三个镜像常量 + `flow-biz-link.test.ts` 计数 20→24 +
`doc_kind_tests::approval_entries_use_registered_biz_types` **补进这 4 个 bridge**
（原来清单只有 8 个，正是漏检根因）。

## 2. 闸⑦：SALES_CLERK 2 对孤儿按钮（补授父页，不改 parent_id）

V0317（SO-43 客户对账单）把 5 个对账单按钮挂在「应收账款」页（`business:erp:receivable:list`）下，
给 SALES_CLERK 授了 `statement:view` / `statement:export` 却没授父页面码 ⇒ 按钮永远不可达。

**处置：`V0402__sales_clerk_receivable_page`** 补授父页面码一行（幂等 + 自检 + down）。
不选「改 `parent_id` 挪按钮」：对账单本来就住在应收台账页里（V0317 的设计），挪走是拿结构迁就授权；
且 SALES_CLERK 看得到应收台账是业务需要（催款要出对账单），`receivable:create/write-off/detail/aging`
仍然不授（看列表 ≠ 能改账，down 里还专门诊断这一点）。

## 3. 闸⑦ 第二对：HR_MANAGER × `business:flow:approve`（V0400 的连带缺口）

V0402 修掉 SALES_CLERK 之后，同一道闸在「V0400 已应用」的库上报出**新的**一对：

```
· HR_MANAGER  business:flow:approve  不可达父页 /flow/task（business:flow:instance:read）
```

根因链：V0400（HR-48）给 HR_MANAGER 补了 `approve` 按钮（必要修复，否则 HR 三套流程的 `n_hr`
节点永远 403）—— 但该按钮节点挂在「审批中心」页（…0862 / `business:flow:instance:read`）下，
而 HR_MANAGER 既没有页面码也没有「审批流」目录（…0860）⇒ **审批权限给了、界面入口没有**。
V0400 当时的验证口径是 `permission_audit_tests::n19` + 真库审批 E2E，**没重跑闸⑦**，所以漏了。

**处置：`V0403__hr_manager_approval_center_page`** 按库里既有 9 个审批角色的形态，给 HR_MANAGER
补「目录 …0860 + 页面 …0862」两行（幂等 + 自检 + down）。零结构变更、不动 V0400。

## 4. 闸⑧ 连带修：`role_code_gate.py` 不认 `INSERT … SELECT` 形态的角色种子

V0403 一提交，闸⑧ 把 `HR_MANAGER` 判成「未知角色码」。查根因：闸的白名单 = 生产活角色 ∪
**迁移里 `INSERT INTO sys_role … VALUES` 种的角色**，而本仓幂等种子的真实写法是

```sql
INSERT INTO sys_role (id, role_code, …) SELECT …, 'HR_MANAGER', … WHERE NOT EXISTS (…);
```

（`role_code` 没有唯一约束，`VALUES` + `ON CONFLICT` 用不了）。旧解析器只认 `VALUES`
⇒ HR_MANAGER 这类角色根本进不了白名单；历史授权段因落在 sha256 基线里才没暴露。

**处置**：`parse_seeded_roles` 增加 SELECT 形态解析（按 `role_code` 列位取字面量，顶层逗号切分），
并补一条自检用例（`INSERT … SELECT … WHERE NOT EXISTS` 种的角色并入白名单）。
`--self-test` 38 → **39/39 通过**，闸⑧ rc=0，白名单 16 个角色（生产 12 + 迁移种子 12，去重）。

## 5. `cost_labor` 过期用例 → 顺带修出一个真 bug

### 5.1 用例口径反转（原本的修）

`employee_number_is_rejected_until_arch9_2_is_online` 断言「`biz_employee` 未上线 ⇒ 填工号就报错」。HR 批已上线，
前提不存在。改为 `employee_number_must_resolve_against_employee_master`：

- 先用 `model::employee_table_present()` **显式断言库形态**（而不是靠「报错文案里含 ARCH9-2」这种会过期的字符串）；
- 断言未知工号被拒、错误列是「员工工号」、报文**点名工号**、零落库。

### 5.2 真 bug：`cost_labor_model::employee_ids_by_no` 查了不存在的列

改完口径后用例立刻 500：

```
校验导入的员工工号: query failed: : db error          ← column "employee_no" does not exist
```

`biz_employee` 的工号列叫 **`emp_no`**（唯一键 `uk_biz_employee_no (org_id, emp_no)`），
原 SQL 写的是 `employee_no`。因为业务代码有 `employee_table_present()` 的前置分支，
在 `biz_employee` 不存在时这行 SQL **从来没被执行过**，猜错也看不出来；HR 上线后才第一次真跑
⇒ **任何「填了工号的工时导入」都 500**（界面只显示「服务器内部错误」）。

修：`SELECT DISTINCT ON (emp_no) emp_no AS employee_no, id … ORDER BY emp_no, id`
（列名对齐 DDL；保持 Rust 侧字段名不变；多组织下同号确定性取 id 最小者，不给随机映射）。

> 这条是「用例过期」背后的真实缺陷 —— 只改断言口径会把它盖住（旧断言只比字符串，不看 SQL 能否跑）。

## 6. `lnk05_*` 6 条全红：夹具退化，不是接线缺陷

6 条全卡在 `submit()` 的 `65018`：「节点『直属上级审批』的审批人全部是制单人本人」。
根因：`expense` 流程首节点 `n_sup` 没有 `biz_flow_node_assignee` 行，走默认 `INITIATOR_SUPERIOR`
（沿 `dept_path` 取 `sys_dept.leader_id`）。**只跑迁移 + bootstrap 的空库**里只有 `admin`
一个挂部门的用户，而其部门 `leader_id` 为空 ⇒ 解析 0 行 ⇒ 兜底 `ADMIN` = 制单人本人 ⇒
内控按职责分离跳过 ⇒ 无可派审批人。

这不是产品缺陷（真实部署的部门都有负责人，报错文案本身可自救），是**夹具退化**。
修：`expense_linkage_tests::ensure_superior_not_creator` —— 只在退化时（`leader_id` 为空或
恰是制单人）把部门负责人指到另一个在职用户；种子库上一行都不动。

---

## 7. 验收矩阵

| 档 | 命令 | 结果 |
|---|---|---|
| 闸④ | `NIK_DATABASE_*=debt49_drill python3 scripts/verify/biz_type_domain.py --strict` | PASS（rc=0） |
| 闸⑥ | `python3 scripts/verify/perm_menu_gate.py` | PASS（rc=0） |
| 闸⑦ | `python3 scripts/verify/orphan_button_gate.py` | 孤儿 0（rc=0） |
| 闸⑧ | `python3 scripts/verify/role_code_gate.py [--self-test]` | rc=0 / 39-39 |
| 菜单不变量 | `menu_invariants.sh debt49_drill` | 8 条全绿（501 菜单） |
| 迁移往返 | V0402/V0403 的 down→up→up | down 后闸红（1 对 / 2 对）、up 后绿、再 up 0 新增 |
| 真库用例 | `cost_labor_db_tests` 全模块 | 25 passed |
| 真库用例 | `expense_linkage_tests` | 6 passed |
| 词表单测 | `cargo test --lib doc_kind` | 32 passed |
| 前端单测 | `pnpm test:unit`（含 flow-biz-link / doc-kinds-mirror） | 16 passed，全量 0 fail |
| L0 | `bash scripts/check.sh --l0` | rc=0 |
| L1 | `cargo clippy --all-targets -D warnings` / `pnpm typecheck` | rc=0 / rc=0 |

## 8. 残留与移交

1. **`cargo fmt --check` 在共享工作区红**：唯一差异文件 `backend/src/business/hr/fixture_hygiene.rs`
   是并行 HR 卡的**未提交新文件**（缩进），不属本卡，未动别人的工作。
2. **`PR_APPROVE` 在对象轴是 `None`**：采购申请没有 `DocKind` 成员（不挂附件 / 提醒）。
   若以后要给申请单挂附件，需要另开卡：加 `DocKind::PurchaseRequisition` + `ck_biz_doc_attachment_type`
   放宽 + `10_checks.sql` 的 `ATTACHMENT_DOC` 登记（三处同改）。
3. **`HEALTH_CHECKED` 未扩**：健康检查清单绑着已应用的 V0202 自检块，扩清单要另开迁移重写自检块。
4. **演练库已 DROP**（`debt49_drill`，145），99 与 145 均无遗留库；ssh 隧道已关闭。
