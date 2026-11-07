# 盘点单「审批 → 待过账 → 过账」两步口径

> 卡面（FIX-8 / t_6ddb0036）问题 2 的结论：**两步是设计意图，不是漏了自动过账**。
> 卡面写的 `/stock-take/{id}/confirm` 并不存在（也不该存在）——过账入口是
> `PATCH /api/business/erp/stock-take/{id}/status`，body `{"status":"POSTED"}`。
> 代码里的口径说明在 `backend/src/business/erp/stocktake_handler.rs` 文件头
> 「为什么『审批通过』与『过账』是两次动作」，本文是对外的同一份口径。

## 1. 状态机（后端唯一真相：`stocktake_model::LEGAL_EDGES`）

```text
DRAFT ──提交审批──→ PENDING_POST ──过账──→ POSTED ──作废(必填原因)──→ CANCELLED
  │                     │                    └──────────作废──────────→ CANCELLED
  │                     └──撤回 / 驳回──→ DRAFT
  └──作废(必填原因)────────────────────────────────────────────────→ CANCELLED
```

**盘点上没有 `DRAFT → POSTED` 这条边**：想动账必须先提交审批（或按 `approval_gate` 口径显式跳过），
再由另一人过账。`PENDING_POST` 在盘点里的语义是「**已提交、账一分没动**」——
这一点与出库单不同（出库单的 `PENDING_POST` 是「待发货确认」），解释问题时不要混用。

| 状态 | 中文（后端下发 `status_label`） | 账 | 说明 |
|------|-------------------------------|----|------|
| `DRAFT` | 草稿 | 不动 | 可改日期 / 备注 / 逐行实盘数；账面数是建单那一刻的快照 |
| `PENDING_POST` | 待过账 | **不动** | 审批通过（或被显式跳过）后停在这里，等持过账权限的人点「过账」 |
| `POSTED` | 已过账 | 已调平 | 三本账（成本账 / 库位账 / 流水）一起动，并生成盘点差异凭证 |
| `CANCELLED` | 已作废 | 已回滚 | 已过账的作废会按快照回滚 + 红冲凭证，**原因必填** |

## 2. 接口（真实路径，前端 `frontend/src/api/business/erp/stock-take.ts` 逐字对应）

| 动作 | 接口 | 权限码 |
|------|------|--------|
| 建单 | `POST /api/business/erp/stock-take` | `business:erp:stock-take:create` |
| 改实盘数 | `PUT /api/business/erp/stock-take/{id}`（仅 `DRAFT`） | `...:update` |
| 删单 | `DELETE /api/business/erp/stock-take/{id}`（仅 `DRAFT`） | `...:delete` |
| **状态流转（含过账）** | `PATCH /api/business/erp/stock-take/{id}/status` | `...:status` |
| 详情 | `GET /api/business/erp/stock-take/{id}` | `...:detail` |
| 分页 | `GET /api/business/erp/stock-take/page` | `...:list` |

状态流转的 `status` 取值：`PENDING_POST`（提交审批）/ `POSTED`（**过账**）/ `DRAFT`（撤回）/ `CANCELLED`（作废）。
详情响应带 `next_statuses`：**前端按钮只读这一份**，不自己维护第二套状态机。

## 3. 为什么是两步（不改成「审批通过自动过账」）

1. **过账不可撤回**。它真的动三本账 + 出凭证，撤销只能「作废 + 红冲」。审批只回答
   「这份差异能不能调平」，过账回答「现在调」——把不可撤回的动作绑在审批回调里，
   审批人一次误点就多一对红蓝凭证。
2. **`posted_by` 是审计要件**。卡片详情里要能回答「谁过的账」，两步才有人可追责；
   自动过账时过账人只能是「审批通过的那次系统调用」。
3. **制单 / 审批 / 过账 三人可分**（`erp.separation_of_duties.post` 开关）：
   营业所会计盘点、仓管主管批、财务过账 —— 这是内控要求，一步到位会把它压成两人。
4. **审批结论是证据不是触发器**。`stocktake_bridge` 在实例 `APPROVED` 时**不改单据状态**
   （单据本来就在 `PENDING_POST`），真正拦住「没过审就过账」的是过账入口对实例状态的硬校验
   （`stocktake_handler::ensure_approved`）。所以「审批通过后单据还停在待过账」是**正确**的。

## 4. 前端表现（已具备，无需改动）

- 列表 / 详情：`待过账` 是独立状态标签（`tag-theme=warning`），差异金额列在
  `DRAFT` / `PENDING_POST` 下显示「过账后才有值」灰字（金额要过账那刻才按账上加权单价算出来）。
- 详情抽屉底部：「过账」按钮由 `next_statuses` 驱动，点下去弹二次确认，写清
  「过账后差异按正负分别走入库 / 出库（三本账一起调平），并生成盘点差异凭证；过账不可撤销」。
- 提交审批后 toast 明确「已提交审批（待过账）」；审批通过后再进详情就是「过账」按钮。
- 未审批就点过账 → 后端 409（`ensure_approved`），前端原样展示报文。

## 5. 真机证据（145 隔离实例 + 拷贝库，全部真 HTTP）

`scripts/verify/p13_stock_take_e2e.py`（本卡复跑，日志见
`docs/evidence/20261108-fix8-seq-and-stocktake/raw/`）：

```text
② 建单（全盘）→ 状态 DRAFT，账面数服务端快照
③ 填实盘 92    → 差异 -8（盘亏），账未动
④ 提交审批     → PENDING_POST，挂审批实例；**未审批就过账 → 409**
   审批人（仓管主管）在自己的待办里通过
⑤ 过账         → POSTED：成本账 100/800 → 92/736、库位账 100 → 92、
                 流水净额 -8、凭证 借 1901 64 / 贷 1405 64
⑥ 作废         → CANCELLED：三本账回到 100/800/100，蓝字 + 红字两张凭证
```

即「审批 → 待过账 → 过账 → 库存变动」每一步都有 HTTP 状态码 + 三本账读数 + 凭证分录作证。
