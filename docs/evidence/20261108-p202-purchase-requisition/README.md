# P2-02 采购申请闭环 · 验收证据（2026-11-08）

> 卡面：t_06298ddf —— 需求申请 → 审批 → 转采购订单（**可合并采购**）
> 结论：**验收 1–6 全部 PASS**；全链 API 实测 **57 条断言 0 失败**；DB 侧判据 **A–N 全绿**（含 4 组负向注入）。

## 0. 本次实测的机器与隔离环境（不碰 144 / 不碰 8033）

| 项 | 值 |
|----|----|
| 主机 | `172.18.10.99`（工作仓 `/opt/cloudpivot`） |
| 后端进程 | `backend/target/debug/cloudpivot`，`NIK_SERVER_LISTEN=127.0.0.1:18105` |
| 数据库 | **全新空库** `cloudpivot_prq03`（owner `alun`，首启迁移 **263** 条，含 V0331） |
| 账号 | `admin / Prq02@123456`（首登强制改密已在脚本里走完）；两个申请人 `p202_a` / `p202_b`（DEPT_LEADER） |
| 起停脚本 | `~/.hermes/profiles/backend-developer/cache/scratch/prq02_instance.sh`（`PRQ_DB=cloudpivot_prq03 PRQ_PORT=18105 bash prq02_instance.sh start`） |

两次启动的迁移日志：首启应用 263 条；**重启后「应用 0 个迁移」**（迁移幂等，V0331 自检块每次跑通）。

## 1. 文件清单（交付物）

| 层 | 文件 |
|----|------|
| 迁移 | `backend/migrations/business/V0331__purchase_requisition.up.sql` / `.down.sql` |
| 后端 | `backend/src/business/erp/purchase_requisition_{dto,model,handler}.rs`、`requisition_bridge.rs` |
| 接线 | `backend/src/business/erp/mod.rs`（模块注册）、`flow/handler.rs`（approve/reject/withdraw/自审批闸 4 处）、`flow/internal_control.rs`（制单人排除）、`erp/budget_guard.rs`（`material_type_code_of` 提为 `pub(crate)` 复用取数口径） |
| 错误码 | `backend/src/shared/error_code.rs`：`ERP_REQUISITION_STATUS_GUARD = 66049`、`ERP_REQUISITION_MERGE_MISMATCH = 66050` |
| 前端 | `frontend/src/api/business/erp/purchase-requisition.ts`、`frontend/src/views/business/erp/purchase-requisition/{index.vue,purchase-requisition-logic.ts}`、`frontend/src/router/index.ts` |
| 菜单/权限 | V0331 内：页面节点 `…3331` + 3 个按钮 `…3332–3334` + 4 个权限码 + `PURCHASE_MANAGER` 全授 / `DEPT_LEADER` 提需求 / `FINANCE_MANAGER` 只读 |
| 审批流种子 | V0331 内：`pr_approve`（`biz_type = PR_APPROVE`）5 节点 5 边，PUBLISHED + is_default，`graph_hash = c0abd5f5…f437` |

## 2. 接口（真实前缀 `/api/business/erp/...`）

| 方法 | 路径 | 权限码 |
|------|------|--------|
| POST | `/api/business/erp/purchase-requisition` | `business:erp:purchase-requisition:create` |
| GET | `/api/business/erp/purchase-requisition/page` | `…:list`（`convertible_only=true` 只给可转单的） |
| GET | `/api/business/erp/purchase-requisition/{id}` | `…:list`（单头 + 明细） |
| POST | `/api/business/erp/purchase-requisition/{id}/submit` | `…:submit` |
| POST | `/api/business/erp/purchase-requisition/{id}/to-po` | `…:to-po`（`requisition_ids` 带多张 = **合并**） |

与卡面差异（卡面写 `/erp/purchase-requisition`）：前缀由 `[router].prefix=/api` 注入、业务容器段 `business`，与采购订单 / 询比价一致。卡面只列 4 个接口，本卡多给了 `GET /{id}`（详情页与合并预览都要它）。

## 3. 验收标准逐条对照

| # | 卡面验收 | 证据（`01_api_steps.txt` / `03_db_assert_output.txt`） |
|---|----------|--------------------------------------------------------|
| 1 | 部门A 申请 100 个物料X | `PRQ-20261108-000001`，`DRAFT`，部门 `P202 需求部A`，预估 `100×10 = 1000.0000`，含税单价服务端算 `10×1.13 = 11.300000`，单位由物料带出 |
| 2 | 部门B 申请 50 个物料X | `PRQ-20261108-000002`，`DRAFT`，部门 `P202 需求部B`，预估 `50×10 = 500.0000` |
| 3 | 两张申请都审批通过 | 都提交 → `SUBMITTED`（回写审批实例）；**B 超预算**（部门B 1405 科目年度 1000 元，月额度 83.33）→ 流程走 `n_pm → n_fin` 两节点（财务经理特殊审批），A 只走 `n_pm`；两张都 `APPROVED`（DB E 段：A 1 个节点 / B 2 个节点） |
| 4 | 合并转成一张采购订单（150 个物料X） | `to-po` 带 `[A, B]` → `PO-20261108-000001`：**1 行**、`qty = 150.0000`、`price = 10.000000`、`tax 13%`、价税合计 `1695.0000`（=1500×1.13） |
| 5 | 两张申请单状态都"已转单"，指向同一个 PO | 两张都 `CONVERTED`，`po_id` / `po_no` 逐字相同；明细 `po_item_id` **两条指向采购单同一条明细**（DB D 段 `all_match_po_line = t`） |
| 6 | API + DB 实测 | API **57 断言 0 失败**；DB 判据 A–N 全绿 + 4 组负向注入 PASS + 残留 0 |

### 反证（都在 `01_api_steps.txt`）

- 重复提交 → **409 / 66049**（报文带审批单号）；
- 重复转单 → **409 / 66049**；
- `DRAFT` 直接转单 → **409 / 66049**；
- 同物料税率不一致（13% 与 9%）合并 → **409 / 66050**（报文点名物料与两个税率）。

### DB 级兜底（`03_db_assert_output.txt` I–L 段）

| 注入 | 期望 | 实测 |
|------|------|------|
| `status='CONVERTED'` 但没有 PO 号 | 被 `ck_biz_prq_converted` 拒 | PASS |
| `status='SUBMITTED'` 但没有审批实例 | 被 `ck_biz_prq_instance` 拒 | PASS |
| `over_budget=TRUE` 但 `over_amount=0` | 被 `ck_biz_prq_over_consistency` 拒 | PASS |
| 同一申请单两个相同行号 | 被 `uk_biz_prqi_line` 拒 | PASS |

收尾复查：`P202-NEG-%` 残留 = **0**（负向注入全部 `ROLLBACK`，不污染证据库）。

## 4. 关键设计决定（与卡面的差异都写在这）

1. **审批不硬编码**：提交时查「已发布的 `pr_approve` 流程」。
   - 有流程 → 发起审批单，**超预算加签是流程定义里的条件边**（`form.over_budget_amount > 0` → 财务经理节点），代码里没有任何 `if amount > X`；
   - 没流程 → **默认 409**（ARCH-3 DC-01 口径），只有持 `business:erp:skip-approval` + ≥10 字理由才放行，且放行 = 直接 `APPROVED`（理由写进备注留痕）。
2. **预算「只判不占」**：申请只是需求，真正占额度的是采购订单提交（`budget_guard::occupy_for_purchase`）。申请阶段若也占，同一笔需求会被算两遍 ⇒ 申请侧只做只读判定，把结论落成快照（`budget_checked / over_budget / over_amount / budget_reason / budget_account_id / budget_period_code`）。`BLOCK` 槽位仍 409 硬拦（与采购侧同口径）。
3. **合并口径**（全部是纯函数 + 19 条单测钉住）：同物料 qty 相加、单价按数量**加权平均**（6 位舍入）、税率必须一致、需求日期取最早、供应商 = 请求指定 or「建议供应商一致」、收货仓库 = 请求指定 or「各单一致」。
4. **卡面之外补的三列**：`warehouse_id`（转 PO 必需，`biz_purchase_order.warehouse_id` NOT NULL）、`po_id/po_no`（合并后多张单指向同一张 PO）、明细 `po_item_id`（并进了采购单哪一行；同物料多张单指向同一行，**刻意不加唯一约束**）。
5. **状态域 4 值** `DRAFT / SUBMITTED / APPROVED / CONVERTED`：没有作废要求就不造没有入口的死状态。

## 5. 已知限制（如实记录）

1. **转单不是跨事务原子**：采购订单由 `purchase_handler::create_purchase_order` 用自己的事务创建（不暴露外部事务入口），所以「建 PO」与「回写申请单」不在同一事务。PO 建成功而回写失败时会留下「PO 已存在、申请单仍 APPROVED」的中间态，重试会重复建单。判定口径：回写用 CAS（`status='APPROVED' AND po_id IS NULL`），并发/重复提交只成功一次；中间态不产生错账（申请单没有账），由「申请单仍可转 → 人工核对 PO 号」兜底。与 P2-01 询比价同款限制。
2. **合并后生成的采购订单是 `DRAFT`**：它自己还要走采购订单审批（`po_approve`）才能收货 —— 这是既有链路口径，本卡不改。
3. **前端验证口径**：`pnpm typecheck` + `pnpm build`（`TC_EXIT=0` / `BUILD_EXIT=0`，`dist/assets` 里能 grep 到 `purchase-requisition` 路由块）+ 路由/菜单一致；**未做浏览器点击级 E2E**（本仓无前端 E2E 设施，与 P2-01 同口径）。

## 6. 复现步骤

```bash
# 1) 全新库 + 隔离实例（18105）
su - postgres -c "psql -c 'CREATE DATABASE cloudpivot_prq03 OWNER alun'"
PRQ_DB=cloudpivot_prq03 PRQ_PORT=18105 bash ~/.hermes/profiles/backend-developer/cache/scratch/prq02_instance.sh start

# 2) API 全链（57 断言）
P202_DB=cloudpivot_prq03 python3 docs/evidence/20261108-p202-purchase-requisition/p202_purchase_requisition_e2e.py

# 3) DB 判据（A–N + 4 组负向注入）
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_prq03 \
  -f docs/evidence/20261108-p202-purchase-requisition/02_db_assert.sql

# 4) 流程快照 hash 对拍（证明种子快照 = canonical 序列化(库内真实图)）
python3 docs/evidence/20261108-p202-purchase-requisition/04_graph_hash_recompute.py

# 5) 单测 / 静态检查
cd backend && cargo test -j 1 --lib -- purchase_requisition requisition_bridge   # 19 passed
cargo fmt -p cloudpivot -- --check && cargo clippy -- -D warnings
cd ../frontend && NODE_OPTIONS=--max-old-space-size=4096 pnpm typecheck && pnpm build
```

## 7. 证据文件

| 文件 | 内容 |
|------|------|
| `00_api_responses.json` | 全链每一步的原始响应（steps + raw） |
| `01_api_steps.txt` | 人读版断言流水（57 条） |
| `02_db_assert.sql` / `03_db_assert_output.txt` | DB 判据 A–N + 4 组负向注入的输入与输出 |
| `04_graph_hash_recompute.py` | 从库内真实图重算 canonical 快照 hash 与种子对拍（PASS） |
| `p202_purchase_requisition_e2e.py` | 可重跑的 API 全链脚本（幂等：部门/用户/供应商存在即复用） |
