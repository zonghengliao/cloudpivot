# M5-10 标准成本：发布（审核后生效）— 交付证据

卡号 `t_a7d7a2d5` · 2026-11-19 · 后端 + 前端 + 迁移（V0465）

## 一、做了什么（一句话）

给标准成本卡加了**第二条轴** `publish_state`（`DRAFT → PENDING → PUBLISHED`），
新建 / 导入的卡一律是**草稿**，必须走 `cost_standard_publish_approve` 审批
（财务经理一级或签）通过后才 `PUBLISHED` + `status = 1`，那时才参与差异分析取数
（取数口径 = `status = 1 AND publish_state = 'PUBLISHED'`）。

## 二、卡片清单

| 文件 | 说明 |
|---|---|
| `backend/app/migrations/business/V0465__cost_standard_publish.{up,down}.sql` | 4 列 + 2 约束 + 1 索引 + 种一条审批流（3 节点 / 2 边 / 1 角色绑定）+ 7 段自检 |
| `backend/app/src/business/erp/cost/cost_standard_publish.rs`（新增，约 870 行） | 提交 / 撤回 / 回调桥 / 自审闸 / 内控取制单人 / 2 条路由 / 6 条纯单测 |
| `backend/app/src/business/erp/cost/cost_standard_publish_db_tests.rs`（新增） | 6 条真库用例（提交 / 通过 / 驳回 / 撤回 / 状态闸 / 内控） |
| `backend/app/src/business/erp/cost/cost_standard_{dto,model,handler}.rs` | 发布轴字段、取数闸（`find_covering_in` / `other_versions_in_window` / `period_sheet`）、三处动作闸、列表筛选 |
| `backend/crates/cloudpivot-doc/src/lib.rs` | `FlowBizType::CostStandardPublish`（`COST_STANDARD_PUBLISH` / `cost_standard_publish_approve`） |
| `backend/crates/cloudpivot-kernel/src/shared/error_code.rs` | `66111 ERP_COST_STANDARD_PUBLISH_GUARD`（409） |
| `backend/app/src/business/flow/internal_control.rs` | 受控码表 20 → 21（制单人不可自审自己建的卡） |
| `backend/app/src/business/erp/doc_reader_impl.rs` | 注册表第 22 个实现（`on_settled` / `guard` / `creator` / `snapshot`） |
| 前端 `cost-standard-kinds.ts` / `cost-standard.ts` / `logic.ts` / `index.vue` / `error-messages.ts` / `doc-kinds.ts` | 发布轴类型与词表 + 2 条接口 + `publishGate` 纯逻辑 + 列表「发布」列与筛选 + 提交/撤回按钮 + 66111 人话 |

## 三、验收（全部真跑）

| 证据文件 | 内容 | 结果 |
|---|---|---|
| `02_cost_standard_tests.out` | `cargo test --lib business::erp::cost::cost_standard`（真库 `cp_m510_full`） | **79 passed / 0 failed**（含本卡 13 条：7 纯 + 6 真库） |
| `03_permission_audit.out` | `system::menu::permission_audit_tests` | **8 passed**（零新增权限码，路由全带闸） |
| `04_menu_invariants.out` | `scripts/verify/menu_invariants.sh cp_m510_full` | **8 项全绿**（菜单 570，本卡未种菜单） |
| `05_flow_domain.out` | `cargo test --lib business::flow::` | **433 passed / 0 failed**（含 doc_reader 双向对账、内控 21 码） |
| `06_migration_idempotent.out` | V0465 `up×2 / down×2 / up` 幂等演练 | **0 ERROR**；down 后 `publish* 列=0 / 流程定义=0`，再 up 后 `defs=1 / versions=1 / nodes=3 / edges=2 / 列=4` |
| 成本域回归 | `cargo test --lib business::erp::cost:: -- --test-threads=1`（`cp_m510_full`） | **903 passed / 0 failed** |
| 前端 | `pnpm typecheck` / `pnpm typecheck:unit` / `pnpm build` / `pnpm test:unit` | rc=0 / rc=0 / rc=0 / **2417 passed / 0 failed**（本卡 +2 条纯逻辑单测） |
| 后端静态 | `cargo fmt --check` / `cargo clippy -p cloudpivot -p cloudpivot-doc -p cloudpivot-kernel --lib --tests -D warnings` | rc=0 / rc=0 |

## 四、关键设计决策（含理由，别当 bug 改）

1. **`publish_state` 与 `status` 两条正交轴**，不把「草稿 / 审批中」叠进 `status`：
   `status` 的既有语义是「参与期间取数」（V0374 注释、M5-08 的重叠护栏、差异分析全指着它）。
   叠上去会让「审批中」在差异分析里被算成「已生效」——M5-08 / M5-09 交接时就点名要避免。
2. **存量行 DEFAULT 'PUBLISHED'**：升级前建的卡没有审批痕迹，降级成草稿 = 让在算的标准突然失效；
   标成已发布与升级前行为逐字一致。新卡由 `insert_head_sql()` **写死 'DRAFT'**
   （不加占位符：任何插入路径 —— 单条 / 批量导入 —— 都造不出「已发布的卡」）。
3. **草稿的 `status` 仍是 1**（默认启用）：它占住生效窗口（重叠护栏照旧生效），
   但**取数读不到**。列表文案由 `headline_status_label` 改成「草稿（未生效）」，避免误导。
4. **未发布的卡不许手工置生效**（`POST …/{id}/status` 的 `status=1` → 409 + `66111`）：
   这道闸放在「已经是目标状态」的幂等早退**之前** —— 否则草稿（status 已是 1）点「生效」
   会得到一句假成功，审批就被绕过了。
5. **审批中（PENDING）不许改口径 / 删 / 置生效**：审批人批的必须是库里那一版。
   要改先「撤回」（发起人）或让审批人「驳回」。
6. **流程只一级（财务经理）**：发布不动账、不出凭证，且可失效回滚（失效即退出取数，
   已落库差异行不受影响），风险等级低于结算（结算两级）；月初换版是定动作，两级会卡单。
   制审分离仍由桥侧 `guard_self_approval`（403 + 65019）兜。
7. **回调不走 `flow::settle_guard`**：那张表要问 `state_machine::effective_edges`，而标准成本卡
   没有登记进 `state_machine::Entity`（它的历史就是版本行本身）。硬塞别的实体必然拿到 `Moved`
   ⇒ 回调静默不落地。这里本地实现同一套三出口语义（已到目标 = 幂等跳过；不在等审批 = WARN；
   在等 = 落状态），并逐条写在代码注释里。
8. **`biz_no` 截断到 32 字符**：`biz_flow_instance.biz_no` 是 `varchar(32)`，物料编码 + 版本号
   拼起来会撞 `22001`（实测 `M510-APPROVE-6199041004 v1` 已 24 字符，自定义长编码必炸）。
   `doc_no_of` 按字符截物料编码、版本号永远保留；没编码时回落 id 前 8 位。
9. **权限码零新增**：提交 / 撤回 = `cost-method:update`（与「改一版」同档，V0460 已授权完）；
   新开码在 M5-18 页落地前是死码（`permission_audit_tests::n14` 会红）。

## 五、给下游卡的接缝

- **M5-11…16（差异）**：取标准仍走 `GET /cost-standard/effective`，本卡已把
  `publish_state = 'PUBLISHED'` 的闸加在这条读路径上 —— 差异分析**不需要任何改动**就只会拿到
  已发布版本；`period-sheet` 同理。
- **M5-18（标准成本页）**：可直接复用 `cost-standard-kinds.ts` 的
  `PUBLISH_STATE_* / publishStateLabel / publishStateTheme` 与 `logic.ts::publishGate`。
- **「谁不能自审」**：`publish::internal_control_creator` 取 `biz_cost_standard.create_by`，
  与审批详情里显示的制单人同源。

## 六、已知边界（本卡刻意不做）

1. **不支持「发布即覆盖旧版」**：发布只把这一版置为已发布；重叠仍由 `ex_biz_cost_std_active_window`
   与 handler 预检拦（要先失效 / 收口旧版），与 M5-08 / M5-09 同一口径。
2. **失效不需要审批**：发布审批只管「生效」，失效是即时动作（可随时恢复启用）。
3. **页面未做浏览器 E2E**：属 M5-34；本卡验收 = 后端 79 条（含 6 条真库审批链路）+
   前端 typecheck/build/2417 条纯逻辑单测。
4. **`biz_type_domain.py` 在演练库上有一条既存 FAIL**：`biz_flow_def.biz_type = 'PURCHASE_ORDER'`
   （V0xxx 老种子写的域外值，与本卡无关；本卡的 `COST_STANDARD_PUBLISH` 未被判为域外值）。
