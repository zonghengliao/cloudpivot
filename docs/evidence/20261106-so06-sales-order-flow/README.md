# SO-06（t_860ea3d4）销售订单审批流 —— 证据包

> 卡面：建销售订单审批流程定义（`flow_code=SO_APPROVE` / 金额三档 / 按角色指派 / 提交订单自动发起实例 /
> 审批通过 → APPROVED、驳回 → DRAFT / 复用 separation.rs 内控），迁移 APPLY_OK + 三档分别发起流程 + 状态正确流转。
> 上游交接：t_9832ecaf（SO-05，提交 `d573d0f`，销售订单 API + 10 条边状态机 + V0265 权限码）。

## 1. 交付清单（改了哪些文件 / 函数）

| 文件 | 类型 | 内容 |
|---|---|---|
| `backend/migrations/business/V0281__sales_order_flow.{up,down}.sql` | 新增 | ① `biz_sales_order.approval_instance_id` + 部分索引；② 角色 `SALES_SUPERVISOR`（复用 `FINANCE_MANAGER`/`GENERAL_MANAGER`）；③ 流程定义 `so_approve`（`biz_type='SO_APPROVE'`）版本/7 节点/8 边/3 条角色审批人；④ 自检 DO 块（条数/hash/条件边/审批人/END 汇聚/角色在位） |
| `backend/src/business/erp/sales_approval_bridge.rs` | 新增 ~960 行 | `SoFlow` / `published_so_flow`、`form_data`（冻结 `form.amount`）、`SoVerdict` / `so_verdict`、`on_so_settled`（审批回调）、`internal_control_creator`、`guard_self_approval`（复用 `separation::Duty::Approve`）、`cancel_approval_of`；5 条纯逻辑单测 + 4 条真库单测 |
| `backend/src/business/erp/sales_order_handler.rs` | 改 | 新增 `submit_approval`（提交审批唯一入口）/`start_approval`/`start_approval_tx`/`submit_conflict`/`pending_instance_no`、`SalesOrderSubmitReq`；`PATCH /status` 路由（SUBMITTED→审批入口、APPROVED/DRAFT→400）；新增 `POST /{id}/submit-approval`；审批中作废同一事务撤审批单 |
| `backend/src/business/erp/sales_order_model.rs` | 改 | `LEGAL_EDGES` 10→11；新增 `apply_status_with_instance`（SUBMITTED 写实例 / DRAFT 清实例并追加 remark）+ `approval_instance_of`；单测同步 |
| `backend/src/business/shared/state_machine.rs` | 改 | `SALES_ORDER_EDGES` 10→11（新增 `SUBMITTED→DRAFT`「审批驳回打回草稿」）、`SALES_ORDER_BASE` 同步 11 条 |
| `backend/src/business/flow/handler.rs` | 改 | `withdraw_instance` / `approve_task` / `reject_task` 各加一条 `sales_approval_bridge::on_so_settled`；`approve_task` 加 `guard_self_approval` |
| `backend/src/business/flow/internal_control.rs` | 改 | `SO_APPROVE` 分支：制单人从审批人列表排除（落 SKIPPED 留痕） |
| `backend/src/business/erp/mod.rs` | 改 | 注册 `sales_approval_bridge` |

### 接口（前缀 `/api` 由 `[router].prefix` 注入）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| POST | `/business/erp/sales-order/{id}/submit-approval` | `business:erp:sale-order:status` | 提交审批（`DRAFT→SUBMITTED` + 发起 `so_approve` 实例，同一事务）；支持 `skip_approval`/`skip_reason` |
| PATCH | `/business/erp/sales-order/{id}/status` | `business:erp:sale-order:status` | `target=SUBMITTED` 走审批入口；`APPROVED`/`DRAFT` 人工推 → 400（审批结论只能由审批中心写） |
| GET/POST | `/business/flow/task/todo`、`/business/flow/task/{id}/{approve,reject}` | 既有 | 审批中心（本卡未改，只接回调） |

### 流程定义（`V0281` 种子，`so_approve` / `SO_APPROVE`）

```
START → 销售主管审批 → [金额 < 10000] → END                      （< 1 万，1 跳）
                      └[≥ 10000]→ 财务经理审批 → [< 100000] → END  （1–10 万，2 跳）
                                                └[≥ 100000]→ 总经理审批 → END （≥ 10 万，3 跳）
```

- 金额分级落在**流程定义的条件边**（`condition_expr.field = 'form.amount'`，阈值 10000 / 100000），
  不是代码里的 if/else —— 客户改阈值 / 换岗位只需在流程设计器另存版本，不用发版。
- 审批人一律 `biz_flow_node_assignee(assignee_type='ROLE')`：`SALES_SUPERVISOR` / `FINANCE_MANAGER` / `GENERAL_MANAGER`；
  `empty_assignee = FALLBACK_ADMIN`（角色 0 人时回退超管，**不静默自动通过**）、`self_approval = ALLOW`
  （真正的自审否决在 `guard_self_approval`，403 + 65019）。
- 卡面的 `flow_code = SO_APPROVE` 落成 **`biz_type='SO_APPROVE'` + `flow_code='so_approve'`**：
  `ck_biz_flow_def_code` 只认 `^[a-z][a-z0-9_]{1,63}$`（大写写不进去），本仓既有口径也是「biz_type 大写 / flow_code 小写」
  （`PO_APPROVE`/`po_approve` …），单测 `biz_type_and_flow_code_match_seed` 逐字钉住。

### 状态机（`state_machine::SALES_ORDER_EDGES`，10 → **11** 条）

```
DRAFT → SUBMITTED → APPROVED → PARTIAL_OUT → OUTBOUND
                       ├→ OUTBOUND / CLOSED          DRAFT/SUBMITTED/APPROVED → CANCELLED
                       └ SUBMITTED → DRAFT（审批驳回 / 撤回打回草稿，只有审批回调能写）
```

## 2. 验收证据（本目录）

| 文件 | 内容 |
|---|---|
| `round-1-unit-db.txt` / `round-2-unit-db.txt` | 同一个库（`cloudpivot_so06`）连跑两轮 `business::erp::sales*` → 各 **39 passed / 0 failed**（20.17s / 20.42s；耗时证明真库用例没被 SKIP）。含 5 条本卡单测：三档 1/2/3 跳 → APPROVED、驳回 → DRAFT、制单人自审 403、没配流程 409 |
| `curl-round-1.txt` / `curl-round-2.txt` | 真实 admin token 的 curl 实测，**两轮均「全部断言通过」（14 条 ✅）**：内控开 → 提交 422（唯一可审批人=制单人）；关内控 → 三档提交 200 + SUBMITTED；PATCH APPROVED/DRAFT → 400；内控开 → 自审 403/65019；关内控 → 1/2/3 跳全 APPROVED；驳回 → DRAFT + remark 带意见 + 可重提；重复提交 → 409 |
| `regression-shared-flow.txt` | `business::shared` + `business::flow` + `business::erp::customer_` 回归 → **513 passed / 0 failed**（52.64s） |
| `clean-db-apply.txt` | 干净库建成 + 迁移 APPLY_OK：`schema_migrations(max|count) = 281|209`，指纹里 `so_approve / n_sales,n_finance,n_gm / ROLE / ALLOW / AND` 三行齐全 |
| `snapshot-hash.txt` | 库里 `graph_snapshot::text` 的 canonical 序列化 sha256 = 种入的 `graph_hash`（`da3c2eed…`）→ 图无漂移（`flow::maintain` 的 RowsEdited 不会误报） |
| `clippy.txt` | `cargo clippy --all-targets --features test-hooks -- -D warnings` → 零告警 |
| `curl-e2e.sh` | 上面两轮 curl 的脚本（可复跑） |

### 复现步骤

```bash
# ① 干净库（跑全部迁移 + bootstrap；APPLY_OK）
ERP_DB=cloudpivot_so06 PORT=18106 RECREATE=1 bash scripts/verify/erp_clean_db.sh
# ② 单测两轮
cd backend && ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so06' \
  cargo test --lib --features test-hooks -- business::erp::sales --test-threads=1
# ③ 起隔离实例 + curl
bash /root/.hermes/profiles/backend-developer/cache/scratch/so06_instance.sh   # 127.0.0.1:18107
bash docs/evidence/20261106-so06-sales-order-flow/curl-e2e.sh
```

## 3. 设计决策

1. **金额分档在流程里，不在代码里**：条件边 `form.amount` + `biz_flow_node_assignee` 角色指派；
   `form_data` 里 `amount` 必须是 **JSON 数字**（字符串会让条件边按字符串比较 → 全掉进最后一档），单测钉住。
2. **提交入口收敛到 `submit_approval`**：`PATCH /status`（老前端）与 `POST /submit-approval`（新按钮）走同一条路，
   否则审批可以换个 URL 绕过。没配流程 → **409 默认拒绝**（`approval_gate::authorize_missing_flow`），
   只有持 `business:erp:skip-approval` 且理由 ≥10 字才放行（理由进 `biz_status_log`）。
3. **审批结论只能由回调写**：`APPROVED` / `DRAFT` 由 `sales_approval_bridge::on_so_settled` 写，
   人工 `PATCH` 推这两个目标态一律 400（否则「审批」只是画在纸上）。
4. **内控复用 `separation.rs`**（`erp.separation_of_duties.approve`，默认开）：两处同用一个开关 ——
   待办生成侧（`internal_control` 把制单人落 SKIPPED）与审批入口（`guard_self_approval` 403 + 65019），
   不可能出现「待办被跳过了但闸说可以批」的自相矛盾。
5. **反向兜底**：审批中作废（`SUBMITTED→CANCELLED`）在**同一事务**里 `cancel_approval_of`，
   否则审批中心留孤儿待办 + `uk_biz_flow_instance_biz_active` 会把下次提交永久堵死。
6. **`SUBMITTED→DRAFT` 这条边**（10→11）：只给审批回调走；`APPROVED→DRAFT` 仍然没有（已审批的承诺不能被悄悄撤销）。

## 4. 踩坑（写给下一个改这条链的人）

1. **参数化 `CASE WHEN ? = 'X'` 在 alun 的驱动下会炸**：`exec_sql` 把 `?` 一律当 `text`，
   PG 在 CASE 里推断不出参数类型 → 客户端 bind 阶段就失败，报文只有一句 `query failed: : db error`
   （看不到 PG 的原始错误）。**解法：分支在 Rust 里定，一条分支一条 SQL，参数个数与占位符一一对应**
   （`purchase_model::apply_status_with_instance` 就是这么写的，照抄最稳）。
2. **`concat_ws(' | ', …, ?)` 里的 `?` 必须写成 `?::text`**：`concat_ws` 是 variadic "any"，
   参数类型推断不出来 → 同样是 bind 失败（症状与上一条一致）。
3. **夹具插入 `NUMERIC` 列要 `?::text::numeric`**：直接 `?::numeric` 会报
   `cannot bind parameter to column type numeric`（驱动按文本送参，PG 期望 numeric）。
4. **`biz_flow_task` 没有 `node_id`**：待办行直接带 `node_key`；JOIN `biz_flow_node` 会报
   `column t.node_id does not exist`。
5. **实例「还在跑」的状态值是 `PENDING`**（不是 `RUNNING`）——断言写 RUNNING 会假红。
6. **内控默认开 + 只有一个超管账号 → 提交就 422**（唯一可审批人 = 制单人本人被排除）。
   这是**正确的 fail-closed**（报文点名「审批人全是制单人 / 发起人」）。真实部署要先给人绑
   `SALES_SUPERVISOR` / `FINANCE_MANAGER` / `GENERAL_MANAGER` 角色；curl 证据里用
   `erp.separation_of_duties.approve` 开关演示了两个方向（422/403 与放行），脚本末尾恢复 `true`。
7. **夹具单号不能用 uuid 前 8 位**：`Uuid::now_v7()` 同一毫秒内前缀相同 → 撞 `uk_biz_sales_order_no`；
   取后 12 位。
8. **迁移号段**：卡面的 `V0262` 早被客户档案菜单卡占用（V0262/V0263），V0264/V0265 是 SO-04/SO-05，
   V0280 是另一张卡的菜单修正 → 本卡取 **V0281**（开工前 `ls backend/migrations/business | tail` 复核）。
