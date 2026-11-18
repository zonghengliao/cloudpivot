# M4-14 供应商准入 / 黑名单流程（走 flow）· 证据包

- 卡：`t_583852b1`（M4-14【M4·采购】供应商准入/黑名单流程）
- 上游：`t_252f2eaf`（M4-13 打分 + 评级，提交 `a1d98ca2`）
- 日期：2026-11-18
- 结论：本卡交付**完整后端纵切** —— 新增迁移 `V0455`（申请单表 + `SAC` 取号 +
  `supplier_access` 审批流程 + 5 行菜单 + 授权）、四件套（model / dto / handler / flow）、
  审批回调落地、接口 4 个权限码、以及采购订单建单的**供应商启停闸**。
  一键演练 **55 条断言全过 / rc=0**（[`01-drill.out`](01-drill.out)）。

## 一、卡面怎么落的

卡面两行：「供应商准入/黑名单流程」「走 flow」。落到代码是四件事：

| 卡面 | 落地 | 位置 |
|---|---|---|
| 准入 / 黑名单是**流程** | 一张申请单承载三种动作（准入 `ADMIT` / 拉黑 `BLACKLIST` / 解除 `RELEASE`），走 `supplier_access` 审批流（`biz_type='SUPPLIER_ACCESS'`） | `V0455` 第五节 + `supplier_access_flow.rs` |
| 分级审批 | 拉黑要**财务经理复核**（停掉在供货的供应商影响在手订单与应付），准入/解除采购经理一票 —— 分级写在**流程定义的条件边**上（`form.access_type eq 'BLACKLIST'`），代码里没有任何 `if 类型 == BLACKLIST` | `V0455` 第六节 `e_type_blk` |
| 审批通过要**落地** | 通过 → 申请单 `APPROVED` + `biz_supplier.status` 落地（拉黑 → 停用，准入/解除 → 启用）；驳回 / 撤回 → 申请单打回 `DRAFT`，**供应商状态一个字节不动** | `supplier_access_flow::settle` |
| 落地要**有牙** | 采购订单建单（`SPOT` 手选 + `CONTRACT` 合同乙方派生两条路径）统一校验 `biz_supplier.status = 1`，否则 409 + `66099` | `purchase_handler::ensure_supplier_usable` |

### 为什么落点是 `biz_supplier.status`（1 启用 / 0 停用），而不是新加一列

1. 「黑名单」的业务事实就是「这家供应商不能下单」，而 `status` 本来就是那个语义（V0137 起就在，
   主数据编辑页也在维护它）。再造 `blacklist_flag` 等于让「能不能下单」有两个真相，
   两边不一致时**没有任何入口会去对齐**。
2. 本卡**不改 `biz_supplier` 一个字节**（不加列、不改默认值）：存量 181 家供应商与全部既有夹具
   的行为一个字节都不变 —— 这是「只追加、不侵入」的硬要求。
3. 历史与理由不丢：谁在什么时候因为什么把谁拉黑 / 准入 / 解除，全在 `biz_supplier_access`
   （单号 + 理由 + 评估等级快照 + 申请人/结论人姓名快照 + 结论时间）。
4. 主数据仍可直接改 `status`（现状不变）；准入/黑名单流程提供的是**有审批留痕的**启停。

### 三种申请单的起点（`supplier_access_model::check_start`，纯函数 + 单测）

| 类型 | 允许的起点 | 通过后 `supplier.status` |
|---|---|---|
| `ADMIT` 准入 | 未在黑名单中，且**从未**通过准入（首次进合格名录） | 1（启用） |
| `BLACKLIST` 拉黑 | 未在黑名单中 | 0（停用） |
| `RELEASE` 解除 | 正在黑名单中 | 1（启用） |

「是否在黑名单」= 该供应商**最近一张已通过的**申请单类型是不是 `BLACKLIST`（`decided_at` 现算），
不读回写列 —— 回写列被动过就与单据链分叉（与 V0449 / M4-09 同一条理由）。
**一种状态只有一个合法动作**：已拉黑的不能再拉黑（要解除）、没拉黑的不能解除、准入过的不需要再准入。

### 状态域只有三个值

```text
DRAFT（拟稿，可改可删）→ SUBMITTED（审批中，必有实例）→ APPROVED（已生效）
驳回 / 撤回 / 终止由回调打回 DRAFT —— 不造「已驳回」状态（打回后要做的事和草稿完全一样）。
```

提交与通过**都不改主数据**（准入单也不冻结供应商 —— 收紧方向的落地同样等审批结论），
所以「驳回后供应商状态没回滚」这类缺陷在本模块结构上不存在：**唯一**改
`biz_supplier.status` 的业务代码就是批准分支，且与申请单状态在**同一个事务**里。

## 二、事务提交后失败必须有补偿（AGENTS 约定 11）

- 免审直过分支（流程没有审批节点 → 实例当场 `APPROVED`）在事务提交后立即回调
  `on_sa_settled`；回调失败 → **显式打回草稿**并报错（用户能重新提交，不需要改库）。
  这一条与 `purchase_plan_handler` 的字面同款，见 `submit_with_approval` 尾部。
- 回调里的落地（申请单 CAS + 供应商 status）在**同一事务**里：任一失败整体回滚，
  不会出现「单据说已生效、主数据没动」的假象；供应商在审批期间被删 → 明确 409 + `66099` 文案。

## 三、改动清单

| 文件 | 说明 |
|:--|:--|
| `backend/app/migrations/business/V0455__supplier_access.up.sql` | 建表（21 列）+ 4 索引 + `SAC` 取号 + `supplier_access` 流程四层（5 节点 / 5 边 / 2 审批人，含真算的 `graph_hash`）+ 5 行菜单 + PURCHASE_MANAGER 授权 + 末尾 DO 自检 |
| `…/V0455__supplier_access.down.sql` | 回滚：**有单据 / 在跑实例就拒绝**，再删菜单/授权/流程四层/取号/表 |
| `backend/app/src/business/erp/purchase/supplier_access_model.rs` | 表名/类型/状态常量、取号、读入口、CAS 写入口、起点判据与落地值（纯函数）、供应商可下单判据 + 7 条纯逻辑单测 |
| `…/supplier_access_dto.rs` | 4 个权限码常量、分页筛选、入参/出参、`bad_request`/`status_conflict`/`not_found` 出口 |
| `…/supplier_access_handler.rs` | 6 条路由（建 / 改 / 删 / 分页 / 详情 / 提交）、提交编排（有流程走流程 / 没流程显式跳过）、免审直过补偿 |
| `…/supplier_access_flow.rs` | `BIZ_TYPE`/`FLOW_CODE`、已发布流程查询、结论映射、`on_sa_settled` 落地、自审闸、内控制单人 + 3 条纯逻辑单测 |
| `…/supplier_access_db_tests.rs` | 10 条真库用例（走真审批引擎：提交 → 采购经理 → 财务经理 → 落地；驳回打回；起点判据；未结单唯一；自审闸；种子 hash 自检；清场） |
| `backend/crates/cloudpivot-doc/src/lib.rs` | `FlowBizType::SupplierAccess`（词表 26 → **27**，前端镜像同步） |
| `backend/crates/cloudpivot-kernel/src/shared/error_code.rs` | `66099 ERP_SUPPLIER_NOT_USABLE` / `66100 ERP_SUPPLIER_ACCESS_STATUS_GUARD` + 码表快照断言 |
| `backend/app/src/business/erp/{doc_reader_impl,doc_reader_impl_tests}.rs` | `ErpDoc::SupplierAccess`（注册 19 → **20**）：终态回调 / 自审闸 / 制单人 / 单据快照 |
| `backend/app/src/business/flow/internal_control.rs` | `GOVERNED_CODES` 18 → **19**（申请人不可自审自己提的申请） |
| `backend/app/src/business/erp/purchase/purchase_handler.rs` | 供应商启停闸（`resolve_head` 之后统一闸；SPOT 与合同乙方两条路径都过） |
| `backend/app/src/system/menu/permission_audit_tests.rs` | 页面显形码登记进 `ROUTE_LEVEL_ONLY`（N18 持续校验） |
| `frontend/src/api/business/erp/doc-kinds.ts` + `frontend/tests/unit/flow-biz-link.test.ts` | 词表镜像（保序逐字比对 27 个） |
| `docs/dead-permissions-20261101.md` | §13 补记：新页面显形码与 4 个接口码的承载行 |
| `scripts/verify/m414_supplier_access_flow_drill.sh` | 一键演练（55 条断言） |

## 四、验证（真实输出见同目录 `.out`）

| 项 | 结果 |
|:--|:--|
| 演练脚本 | **55 条断言全过 / 0 失败 / rc=0**，含：迁移 51 条全应用、V0455 幂等重跑、20 条用例 0 失败 0 SKIP、库层 5 条坏数据逐条被拒、流程四层 + 条件边 + hash、菜单 5 行 / sort 唯一 / 授权 5 行、夹具 0 残留、不夹带、权限审计 8/8、错误码表、注册表契约 16/16、`menu_invariants.sh` 8 条 |
| `cargo fmt -p cloudpivot -p cloudpivot-kernel -p cloudpivot-doc` | rc=0 |
| `cargo clippy -p cloudpivot --lib -- -D warnings` | rc=0（真检查，无 warning） |
| 本卡用例 | `supplier_access` 全组 **20 passed / 0 failed**（10 纯逻辑 + 10 真库），SKIP=0 |
| 回归 | M4-13 打分/评级 16 passed；`flow::doc_reader_tests` 18 passed；`erp::doc_reader_impl_tests` 16 passed；`flow_biz_type_contract_tests` 2 passed；`error_code` 码表 1 passed |
| 前端 | `pnpm typecheck` + `pnpm build`（本卡只改镜像，见 `02-frontend.out`） |

## 五、实测踩到的坑（写进 skill）

1. **ANY 会签模式下「票数 ≠ 节点数」**：模板库里 采购经理 角色有多名成员，提交后同一个节点
   会生成**多条**待办（每个候选人一条），只批第一条才能推进节点。用例最初按「票数」断言
   （期望 1 票）实测拿到 3 票 / 7 票 —— 改成「走过的节点 key 个数」断言才稳。
2. **`graph_hash` 只能由代码算**：迁移里的 canonical JSON 不能用 SQL 拼（`jsonb::text` 会插空格、
   键序也不等于 `canonical_json`）。做法是先落行 → `flow::model::load_graph()` 重建图 →
   打印 `snapshot_json()` / `graph_hash()` → 回填迁移 → 用例 `seeded_flow_hash_matches_rows`
   每次跑都复算一遍（谁改了节点/边一个字节就红）。
3. **迁移里的占位值必须先是合法 JSON**：第一版把 snapshot 写成 `__SNAPSHOT__`，PG 在
   `::jsonb` 转换时就报 `invalid input syntax`，**后面的节点/边插入全跳过**（表建好了、流程没图）。
   占位也得是 `{"edges":[],"nodes":[],"schema_version":1}`。
4. **`#![deny]` 级别的 clippy**：`map_err(|msg| f(msg))` 会被判 redundant closure ——
   但只有当 `Err` 是 `String` 时 point-free 才编得过；`nik::prelude::*` 的 `Result` 别名
   只接受一个泛型参数，模型层用 `std::result::Result<_, String>` 才不会撞别名。
5. **`const fn` 不能 match `&str`**（Rust 1.94 仍是 E0658）：`type_label` / `status_label` /
   `target_supplier_status` 必须是普通 `fn`。
6. **并行工作区里别断言全局 `git status`**：别人同时在改 `frontend/`，本卡演练脚本一度因为
   「前端改动文件数」断言红掉 —— 只断言**自己那几处**（镜像里有没有 `SUPPLIER_ACCESS`、
   supplier-access 页面目录是否存在）。
