# M4-03 采购计划 → 采购申请转换 · 证据包

- 卡：`t_03a60b7a`（M4-03【M4·采购】采购计划：→ 采购申请转换）
- 提交：见卡内汇报（`feat(erp): 采购计划 → 采购申请转换（一仓一单 + 已执行量回写）（M4-03 t_03a60b7a）`）
- 上游：`t_c6473745`（M4-02 采购计划 CRUD + 审批，迁移 V0447 / 流程 `pp_approve`）

## 一句话

把**已批准**的采购计划按「物料 × 仓库」的粒度转成采购申请：**一仓一张申请单**（申请单单头只有一个收货仓库），
申请明细逐行记 `plan_item_id`、申请单头记 `source_plan_id`（V0448 两列），
并在同一事务里回写计划的 `executed_qty` + 重算单头三个 `total_*`。

## 落地面（改了哪些）

| 层 | 文件 | 干什么 |
|---|---|---|
| 迁移 | `backend/app/migrations/business/V0448__purchase_plan_to_requisition.up.sql` / `.down.sql` | 两列溯源（`biz_purchase_requisition.source_plan_id`、`biz_purchase_requisition_item.plan_item_id`）+ 两个部分索引 + 1 行菜单（`business:erp:purchase-plan:to-requisition`，挂采购计划页面行下 sort=60）+ PURCHASE_MANAGER 授权 + DO 自检 |
| 契约 | `backend/app/src/business/erp/purchase/purchase_plan_convert_dto.rs`（新） | 入参 / 出参 / 纯函数（`plan_conversion` 选行与校验、`group_by_warehouse` 拆单、`compose_head_remark`）+ 10 条纯逻辑单测 |
| 接口 | `backend/app/src/business/erp/purchase/purchase_plan_convert_handler.rs`（新） | `POST /api/business/erp/purchase-plan/{id}/to-requisition`：锁单头（重判 APPROVED）→ 锁明细 → 读 → 建 N 张申请单 → 回写已执行量 → `recalc_totals` → commit |
| 数据层 | `purchase_plan_model.rs`（+`lock_items` / `add_executed_qty`）；`purchase_requisition_model.rs`（`create_requisition_in` 事务变体 + 两列写入） | 行锁 + 原子 `executed_qty += qty`；建单逻辑与手工建单共用同一段 INSERT |
| 真库用例 | `purchase_plan_convert_db_tests.rs`（新，11 条） | 拆单 / 溯源 / 回写 / 状态闸 / 分批 / 超量 / 零单价 / 公司级缺部门 / 跨计划行 / 转出的是真申请单 / 清场 |
| 演练 | `scripts/verify/m403_plan_to_requisition_drill.sh`（新，86 断言） | 空库首启 → 真 HTTP 全链 → 迁移幂等/down/up → perm_menu_gate → menu_invariants |

## 口径（决定，不是待办）

1. **只有 APPROVED 能转**：DRAFT（还没批）/ SUBMITTED（审批中）一律 409，报文点出当前状态与前置条件。
2. **一仓一单**：申请单 `warehouse_id` 是必填单头，计划粒度是「物料 × 仓库」⇒ 按仓库分组，N 个仓库 N 张单。
   不拆的话只能把别的仓库的需求记到第一个仓库上。
3. **转多少**：缺省 = 剩余量（`planned_qty − executed_qty`）；可逐行指定更小的量（分批转）。
   **超过剩余量 400**（报文点出剩余量与计划行号），不静默截断、不落库层 500。
   超出计划的新增需求请直接新建采购申请单 —— 否则执行率永远 >100%，计划就白编了。
4. **单价**：缺省取计划单价；计划行单价为 0 时 **必须**在请求里给 `items[].est_price` 覆盖，否则 400。
   申请单价是预算判定的输入，0 元等于「不占预算」，会让「超预算加签」被一行 0 元明细绕过。
5. **公司级计划**（没有编制部门）转申请必须给 `dept_id`（`biz_purchase_requisition.dept_id` NOT NULL）。
6. **事务**：建单 + 回写 + 重算合计同一事务，任一步失败整批回滚 —— 不留「单造了量没记」（重复采购）
   或「量记了单没造」（假执行）。
7. **并发**：`lock_plan`（单头 FOR UPDATE，锁内重判状态）+ `lock_items`（明细按 id 序 FOR UPDATE，先锁后读）。
8. **已执行量不加 `<= planned_qty` 的 CAS**：库层刻意允许超计划（V0446 头注释 4），
   超量判定在 handler 的行锁内做（这样报文能说人话），不是靠库层报 500。

## 证据清单

| 文件 | 内容 |
|---|---|
| `01_drill_full.out` | 演练脚本全量输出：**PASS=86 FAIL=0**（空库首启 368 迁移 → 真 HTTP 全链 → 迁移往返 → 两道门禁） |
| `02_unit_and_db_tests.out` | `cargo test --lib business::erp::purchase::purchase_plan_convert`：**23 passed / 0 failed**（10 纯逻辑 + 2 handler + 11 真库） |
| `03_permission_audit.out` | `system::menu::permission_audit_tests`：**8/8**（N14 新增码有落点 / N18 台账仍活） |
| `04_menu_invariants.out` | 菜单结构 8 条断言：**全绿** |
| `05_perm_menu_gate.out` | 死权限码闸：**missing_in_menu = 0** |
| `06_clippy_lib.out` / `07_clippy_tests.out` | `cargo clippy --lib[-tests] -- -D warnings`：**0 告警** |
| `08_fmt_check.out` | `cargo fmt -- --check`：**rc=0** |
| `09_mig_replay.out` | 空库重放：**368/368** 条版本化迁移按版本号升序全部通过（含新增 V0448） |
| `10_scope_inventory.txt` | 新增 / 改动文件的 SCOPE-EXEMPT 与行数盘点 |

## 复核命令

```bash
# ① 演练（空库首启 + 真 HTTP；99 上指向 145 的 PG）
cd /opt/cloudpivot && bash scripts/verify/m403_plan_to_requisition_drill.sh

# ② 纯逻辑 + 真库用例（串行）
cd backend && ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/<drill_db>" \
  cargo test --lib business::erp::purchase::purchase_plan_convert -- --test-threads=1

# ③ 门禁
cd backend && SYS_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/<full_db>" \
  cargo test --lib system::menu::permission_audit_tests -- --test-threads=1
bash scripts/verify/menu_invariants.sh <full_db>
```

## 已知边界（刻意不做）

- **前端页面与「转申请」按钮**属 M4-06（本卡只做后端转换；`sys_menu` 里的按钮行是权限承载，页面还没画）。
- **执行率报表**属 M4-04（本卡只写 `executed_qty` 与单头合计）。
- 转出来的申请单是 **DRAFT**：要再走 `pr_approve` 审批、再转采购订单（`to-po`），本卡不代劳。
- `down` 会 DROP 两列 ⇒ 溯源指针值随之丢失（迁移注释已写明；生产不跑 down）。
