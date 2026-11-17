# M4-02 采购计划：CRUD + 审批 —— 证据包

卡：`t_c6473745`（M4-02【M4·采购】采购计划：CRUD + 审批）
上游：`t_c5ecacf7`（M4-01 表结构，提交 `0aef195b`，V0446）
日期：2026-11-17 · 环境：99 开发机 + PG `172.18.10.145:5432`（用户 `cloudpivot`）

## 一句话

采购计划从「只有两张表」补成**能用的一整条链路**：建单 / 改单 / 删单 / 分页 / 详情 / 提交审批，
审批走真流程 `pp_approve`（采购经理 → ≥50 万加财务经理），通过回调把单据推到 `APPROVED`、
驳回打回 `DRAFT` 并把意见写进备注；制单人不能自审（403）。零前端（页面属 M4-06）、
零转申请（M4-03）、零执行率报表（M4-04）。

## 交付物

| 类别 | 文件 |
|---|---|
| 契约（DTO + 纯校验 + 单测） | `backend/app/src/business/erp/purchase/purchase_plan_dto.rs` |
| 数据层（建/改/删/状态流转/分页/合计重算） | `backend/app/src/business/erp/purchase/purchase_plan_model.rs`（M4-01 基础上 +420 行） |
| 接口层（6 条路由） | `backend/app/src/business/erp/purchase/purchase_plan_handler.rs` |
| 审批回调 + 内控 | `backend/app/src/business/erp/purchase/purchase_plan_bridge.rs` |
| 真库用例（14 条，走真引擎审批） | `backend/app/src/business/erp/purchase/purchase_plan_handler_db_tests.rs` |
| 迁移（流程 + 6 行菜单 + 授权） | `backend/app/migrations/business/V0447__purchase_plan_crud.{up,down}.sql` |
| 演练脚本（空库首启 + 真 HTTP 61 断言） | `scripts/verify/m402_purchase_plan_crud_drill.sh` |

### 接线（新增一条审批线必须改的六处，全部落地）

| # | 文件 | 改了什么 |
|---|---|---|
| 1 | `crates/cloudpivot-doc/src/lib.rs` | `FlowBizType::PpApprove`（ALL 25→26、`as_str`=`PP_APPROVE`、`flow_code`=`pp_approve`、`label`、`doc_kind`→`None`） |
| 2 | `frontend/src/api/business/erp/doc-kinds.ts` | `FLOW_BIZ_TYPES` / `FLOW_BIZ_TYPE_LABELS` / `FLOW_BIZ_TYPE_FLOW_CODES` 三处镜像 |
| 3 | `frontend/tests/unit/flow-biz-link.test.ts` | 字面量条数断言 25→26（11/11 通过） |
| 4 | `backend/app/src/business/erp/doc_reader_impl.rs` | `ErpDoc::PurchasePlan`（ALL 18→19）+ `biz_type` / `doc_name` / `DocSpec` / `settle` / `guard` / `guard_approve` / `creator` 七个 match 分支 |
| 5 | `backend/app/src/business/flow/internal_control.rs` | `GOVERNED_CODES` 17→18（`PP_APPROVE`，制单人不可自审） |
| 6 | `backend/app/src/system/menu/permission_audit_tests.rs` + `docs/dead-permissions-20261101.md` | 页面显形码 `…:page` 登记进 `ROUTE_LEVEL_ONLY` 台账（后端零引用，约定 14） |

## 接口（6 条，前缀 `/api`）

| 方法 | 路径 | 权限码 |
|---|---|---|
| POST | `/business/erp/purchase-plan` | `business:erp:purchase-plan:create` |
| PUT | `/business/erp/purchase-plan/{id}` | `…:update` |
| DELETE | `/business/erp/purchase-plan/{id}` | `…:delete` |
| GET | `/business/erp/purchase-plan/page` | `…:list` |
| GET | `/business/erp/purchase-plan/{id}` | `…:list` |
| POST | `/business/erp/purchase-plan/{id}/submit` | `…:submit` |

页面显形码 `…:page` 只在 `sys_menu` 的页面行上（`menu_type=2`），**后端零引用**（证据 10 号 ③）。

## 三条由库层口径驱动的实现选择（不是随手写的）

1. **单头三个 `total_*` 是派生列**：请求体里**没有**这三个字段（DTO 不接收），一律
   `recalc_totals` 在事务里按明细重算 —— 证据 01 号「计划量合计 = 14 / 金额 = 425」是服务端算的。
2. **明细粒度 = 物料 × 仓库**：重复在**请求层**就拒，报文点出「第 3 行与第 1 行是同一个」，
   而不是让库层唯一索引报 500（证据 01 号第 5 步）。
3. **月度计划的父单**必须是与自身同年度、类型为 ANNUAL 的存活计划（`ck_biz_pp_parent` 只拦自指，
   跨行语义在 handler 判）—— 三条非法用例在真库里逐条 400（02 号 `parent_plan_must_be_annual_of_same_year`）。

## 审批（走 flow，不硬编码）

- 提交：`engine::start` 发起 `pp_approve` 实例 + CAS `DRAFT → SUBMITTED` + 写实例 id，**同一事务**；
- 通过：`purchase_plan_bridge::on_pp_settled` 回调 `SUBMITTED → APPROVED`，结论追加进备注；
- 驳回 / 撤回 / 终止：打回 `DRAFT`，驳回意见写进备注（可改完再提）；
- 分级在**流程定义的条件边**上（`form.total_planned_amount >= 500000` 加财务经理），代码里没有 if；
- 内控：制单人 / 发起人不能批自己那一票（403），待办生成时也把制单人排除并留痕；
- **事务提交后失败有补偿**（AGENTS 约定 11）：免审直过分支回调失败时显式打回 `DRAFT` 并报错，
  用户能重提，不需要改库。

## 验收（原始输出见本目录）

| 证据 | 命令 | 结果 |
|---|---|---|
| `01_drill_full.out` | `bash scripts/verify/m402_purchase_plan_crud_drill.sh` | **PASS=61 FAIL=0**，rc=0：空库首启（实例自己跑完 367 条迁移）→ 建/改/删/分页/详情/提交 → 制单人自批 403 → 审批人两票（小额一票、60 万两票）→ 迁移幂等 + down/up 往返 → `menu_invariants` 8 条全绿 → 演练库已删 |
| `02_unit_and_db_tests.out` | `cargo test --lib business::erp::purchase::purchase_plan`（ERP_DB_URL=演练库） | **41 passed / 0 failed**（9 条纯逻辑 + 4 条 model + 14 条真库 + 14 条 DTO/bridge） |
| `03_permission_audit.out` | `cargo test --lib system::menu::permission_audit_tests` | **8/8**（N14 页面码零引用台账、N18 台账存活、N19 审批角色有审批权、N20 非超管有授权、N21 路由不遮蔽） |
| `04_menu_invariants.out` | `bash scripts/verify/menu_invariants.sh <库>` | **全绿**（菜单总数 546 ∈ [400,560]；①②③④⑥⑦⑧ 全 0） |
| `05_mig_replay_fresh.out` | `bash scripts/verify/mig_replay_fresh.sh` | **367/367** 条版本化迁移在空库上按号升序全过（含 V0447） |
| `06_doc_reader.out` | `cargo test --lib business::erp::doc_reader_impl` | **16/16**（19 个实现注册 / 契约对账 / 真库 SQL） |
| `07_doc_crate.out` | `cargo test -p cloudpivot-doc` | **19/19**（前端镜像逐字同序 / 词表域） |
| `08/09_clippy_*.out` | `cargo clippy … --lib [--tests] -- -D warnings` | **0 告警**（lib + lib tests）；`cargo fmt -- --check` rc=0 |
| `10_scope_inventory.txt` | 源码对账 | 6 条路由 / 5 个接口码 / 页面码 0 处可执行引用 / 前端镜像 3 处 / 迁移最大号 V0447 |

前端：`pnpm typecheck` 通过（无输出即通过）、`pnpm build` 成功、`node --test tests/unit/flow-biz-link.test.ts` **11/11**。

## 需要下游注意（两条）

1. **菜单总数上限从 540 提到 560**（`scripts/verify/menu_invariants.sh` + `scripts/verify/README.md`）：
   本卡落 1 页面 + 5 按钮，模板库升级路径实测 540 → 546，已按该脚本自己的「按实测上调」规则补记录。
   该文件在别人名下还有一处未提交的 520→540 历史行，本次一并带入提交（同一行区域，无法拆开）。
2. **M4-06 前端**：页面行已就绪（`/business/erp/purchase-plan`、`component=business/erp/purchase-plan/index`、
   `icon=calendar`、`sort=38` 挂在「采购管理」目录 …946 下），前端只需按这 6 条路由写 api + 页面；
   页面显形码 `…:page` 已在台账里，后端**不要**再引用它。
