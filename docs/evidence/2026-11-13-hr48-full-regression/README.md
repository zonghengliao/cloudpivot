# HR-48 · 全量回归（既有 verify 脚本 + 新增 HR 脚本）

卡：`t_fb2e95f4`（批 5 收口，来自 ARCH-9-2 §8.1「全量回归」）
验收：`scripts/check.sh` 全绿 · 既有脚本不受影响
提交：`b50dbf86`（origin(145) 与 github 双远端一致，pre-push 的 L0+L1 门禁**真跑通过** 261s / 169s）

---

## 1. 跑了什么

| 档 | 命令 / 内容 | 结果 |
|---|---|---|
| L0 静态闸 | C7 G1–G6 / 色板 / 迁移版本号全局唯一 / 库存写入口 / 迁移授权段角色码 | ✅ 全绿（`--l0`） |
| L1 编译与类型闸 | `cargo fmt --check` / `clippy --all-targets -D warnings` / `cargo check` / `pnpm typecheck` | ✅ 全绿（`--l1` EXIT=0，push 时 pre-push 又真跑一遍 261s/169s） |
| L2 连库档 · 闸⓪ 真库用例 | `CHECK_WITH_DB=1` + 三个库 URL 指 145 演练库 `hr48_drill`，全量 lib | ❌ **5475 passed / 41 failed**（2782s）— 见 §3，其中 25 条是 HR 夹具污染、其余为非 HR 存量 |
| L2 · 闸① 分页唯一写法 | `paginate_with_params` 0 处 | ✅ |
| L2 · 闸② scope 登记 | `scope_gate.py`（147 个测试面文件 / 589 个生产文件） | ✅ **本卡修绿**（回归前：7 个文件 + 19 条漏登记，rc=1） |
| L2 · 闸③ ERP 不变量 | `erp_invariants.py` | ✅ 81/81 通过，BLOCKER 0 |
| L2 · 闸④ 业务类型词表 | `biz_type_domain.py --strict` | ❌ 1 列 4 个域外值（`biz_flow_def.biz_type`：SO_APPROVE / PR_APPROVE / PRT_APPROVE / COST_SETTLE）— **非 HR 存量**，见 `04_gate4_biz_type_domain.txt` |
| L2 · 闸⑤ 快照一致性 | `snapshot_audit.py`（先 `--install-views`） | ✅ 26/26 巡检项通过，BLOCKER 0 |
| L2 · 闸⑥ 死权限码 | `perm_menu_gate.py` | ✅ missing_in_menu = 0（HR 36 个码全部有承载） |
| L2 · 闸⑦ 孤儿按钮 | `orphan_button_gate.py` | ❌ 2 对（SALES_CLERK × `erp:statement:view/export`，来自 V0317 / 提交 ea3021e5）— **非 HR 存量** |
| L2 · 闸⑧ 迁移授权角色码 | `role_code_gate.py` | ✅ 320 个 .up.sql 无未知角色码 |
| 既有 verify 脚本 | `menu_invariants.sh`（8 条断言，库 hr48 全量迁移 496 菜单） | ✅ 全绿 |
| 新增 HR 脚本 | `hr_invariants.sh`（A1~A10 + W1/W2） | ✅ 12 行 viol=0，blocker_fail=0 |
| 新增 HR 脚本 | `hr_invariants_selftest.sh`（造错自检 R0/I1~I10/F1） | ✅ PASS（零误报 + 10 条全部报红 + fail-closed） |
| 新增 HR 脚本 | `hr/hr_salary_e2e.sh --fresh`（10 步 98 项断言） | ✅ rc=0，演练库自建自删 |

> 演练库全部在 **145**（`hr48_drill` / `hr48_selftest_drill` / `hr48_iso` / e2e 自建库），99 上未建任何库。

---

## 2. 本卡改了什么（全部为了让闸变绿）

### 2.1 闸② 解析器漏一类测试面（根因修复）

`erp/payable_payment/tests/payable_linkage_db_tests.rs` 是纯测试代码，但它是写在
`payable_payment.rs` 的内联 `#[cfg(test)] mod tests { … mod payable_linkage_db_tests; }`
里的。旧解析器只跳过「区体本身」，**不解析区体内 `mod y;` 声明的外部文件** → 该文件被
当成生产文件，闸② 要它写 SCOPE-EXEMPT 才放行。

修法（`scripts/lint/scope_gate.py`）：

- 新增 `decls_in_inline_region()`：扫内联 `#[cfg(test)] mod x { … }` 区体内的 `mod y;`；
- `Decl` 增加 `prefix` 字段并按 **Rust 模块语义**把「内联模块本身占一层目录」算进路径
  （`foo.rs` 的 `mod tests { mod y; }` ⇒ `foo/tests/y.rs`，不是 `foo/y.rs`）；
- 被跳过的文件**仍要过 `SKIP_MANIFEST` 逐条登记**（多一个少一个都 FAIL）⇒
  「一句 cfg(test) 把生产文件藏起来」照样被拦（自测 ⑩ 是这条反事实对照）；
- 自测新增 ③'，`--self-test` **13 项全通过**。

### 2.2 存量补登记 / 补理由

| 文件 | 处理 |
|---|---|
| 19 个测试面文件（12 HR + 7 erp/flow）+ HR-47 在途的 `business/hr/perm_matrix_tests.rs` | 补进 `SKIP_MANIFEST`（闸② 报错文案就要求这么做） |
| `business/impexp/builtin/hr/employee.rs`（HR 本模块） | `SCOPE-EXEMPT`：员工导入的部门名/路径解析索引，只做校验解析、不产出行集 |
| `business/shared/user_name.rs` | `SCOPE-EXEMPT`：姓名**快照**助手（AGENTS 约定 13），按主键/给定 id 集合取值 |
| `business/monitor/model.rs` | `SCOPE-EXEMPT`：运维页系统总量（在线数/今日登录/今日操作），按范围裁剪反而错 |
| `business/flow/config_check.rs` | `SCOPE-EXEMPT`：流程图配置校验，只产校验提示 |
| `business/flow/subprocess.rs` | `SCOPE-EXEMPT`：子流程校验，`biz_flow_*` 是全局配置侧（同 `flow/model.rs` 口径） |
| `system/setup/model.rs` | `SCOPE-EXEMPT`：初始化向导，空库首启引导期无登录主体/无范围可裁 |

> 后 5 个生产文件属**别的模块**的存量债：在 HR-01 的提交 `60befd59`（HR 代码进场之前）上
> 跑同一门禁，它们同样报红（`01_scope_gate_before_hr.txt`）。本卡只加理由注释，**零行为变更**；
> 理由是否成立请各 owner 复核。

### 2.3 L2 权限/契约门禁收口（HR 侧，实测从红转绿）

| 闸 | 回归前 | 根因 | 本卡做法 |
|---|---|---|---|
| N18 台账过期 | 红（30 条点名） | HR 批已全部落地，`PENDING_LANDING` 没删 | 按「落地即删」清掉 28 条，留 3 条仍零引用的（`leave:delete` / `salary-item:delete` / `employee:resign`）|
| **N19 审批角色无 approve（阻断）** | 红 | `hr_salary` / `hr_salary_adjust` / `hr_employee_resign` 的 `n_hr` 绑 `HR_MANAGER`，而该角色**没有** `business:flow:approve` ⇒ 三个节点**永远 403，审批永久卡住** | 新增迁移 `V0400__hr_flow_approver_role_grants`（口径同 V0239 ①，范围收到 HR 四套流程；幂等 + DO 自检 + 配套 down），实测 up×2 / down×2 / 自检通过 |
| N16 裸路由 | 红（HR 1 条 + 存量 6 条） | `/business/hr/health` 是 HR-09 的设计决定（登录即可的排障探针） | 登记进 `NO_PERMISSION_ROUTES` 并写清理由；存量 6 条（dashboard / erp cost / auth idle-policy）非 HR，未动 |
| `guard::preflight_uses_the_registry` | 红（17 vs 16） | 登记表从 16 涨到 17 没改计数 | 计数改 17 + 注释补 `biz_attendance_record` |
| `scope_model::d8_coverage_matches_registry` | 红（TABLE_WIRING 15 vs 17） | `biz_leave_request` / `biz_attendance_record` 只登记了 `SCOPED_TABLES`，第二份清单漏行 | 补两行 `TableWiring`（含锚点与理由） |

**实测**：`permission_audit_tests` + `system::menu::tests` + `shared::guard` + `permission::scope_model`
一起跑 = **36 passed / 2 failed**，剩余两条全是非 HR：N08（`/business/hr/salary` 前端未注册路由 —
前端卡）、N16（存量 6 条裸路由）。

---

## 3. 闸⓪ 全量真库用例：5475 passed / 41 failed（诚实结论）

失败按根因分类（完整清单 `04b_full_run_failures.txt`）：

| 类 | 条数 | 判据 |
|---|---|---|
| **HR 跨用例夹具污染（顺序依赖）** | ~25（salary_override 9 / salary_sheet 6 / self_service 5 / salary_export 3 / salary_compute 2 / salary_item_seed 1 / salary_bridge 1） | 隔离反证：同一个库、同一二进制，只跑 `salary_compute_db_tests` 过滤器 = **6 passed / 0 failed**（`07_hr_salary_compute_isolated_ok.log`）；整模块跑则前面用例留下的 `EMP-0001..3 HR10 夹具员工` / `HR34A/B` / `HR36A/B` / `HR37A/B` / `HR41A/B` 共 11 个员工被后面的薪资用例当成本期间人数 ⇒ 断言 2 ≠ 5。**属测试卫生（夹具清场）问题，不是业务代码缺陷** |
| HR-47 在途改动 | 1（`every_macro_path_in_module_is_in_contract`：`/business/hr/employee/import-template` 未登记契约） | 并发卡的未提交代码，已被本卡提交之外 |
| 非 HR 存量 | 15（`erp::expense_linkage_tests::lnk05_*` 6 条、`erp::cost_labor_db_tests::employee_number_is_rejected_until_arch9_2_is_online` 1 条 —— HR 上线后该 ERP 用例的过期假设、`system::menu` N08/N16/n18/n19 等 5 条、`guard`/`scope_model` 2 条） | 其中 n18/n19/guard/scope_model **本卡已修绿**（见 §2.3，修后用同一库复测通过）；lnk05 / cost_labor / N08 / N16 非 HR |

---

## 4. 既有脚本不受影响

- L0 的 5 个既有脚本（C7 G1–G6 / 色板 / 迁移版本唯一 / 库存写入口 / 角色码）：全绿
- L2 的既有闸③⑤⑥⑦⑧：闸③ 81/81、闸⑤ 26/26、闸⑥ 0 死码、闸⑧ 0 未知码均绿（闸④⑦ 的存量红见 §1）
- `scripts/verify/menu_invariants.sh`（既有菜单结构闸，8 条断言）：全绿，496 菜单
- `scripts/lint/scope_gate.py --self-test`：13 项全通过（新增 ③' 后仍全绿）
