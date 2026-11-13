# 快照/词表漂移处置：`biz_flow_def.biz_type` 4 个域外值（2026-11-13 · t_c49010bf）

> 本目录是「巡检判据 + 漂移处置记录」的落脚点。本文件不是迁移，不改任何已应用 SQL。
> 设计层结论见 `docs/architecture/P2-5-业务类型词表盘点.md` §10（补记）。

## 1. 漂移事实

`scripts/verify/biz_type_domain.py --strict` 在任何**全量迁移库**上红（不是只在某个客户库）：

| 值 | 行数 | 来源迁移 | 谁在用它 |
|---|---|---|---|
| `SO_APPROVE` | 1 | `V0281__sales_order_flow` | `erp::sales_approval_bridge::BIZ_TYPE` |
| `PR_APPROVE` | 1 | `V0331__purchase_requisition` | `erp::requisition_bridge::BIZ_TYPE` |
| `PRT_APPROVE` | 1 | `V0332__purchase_return` | `erp::purchase_return_bridge::BIZ_TYPE` |
| `COST_SETTLE` | 1 | `V0381__cost_flow_def` | `erp::cost_period_bridge::BIZ_TYPE` |

唯一真相 `backend/src/business/shared/doc_kind.rs::FLOW_BIZ_TYPE_VALUES` 当时是 20 个值，
不含这 4 个 ⇒ 巡检判红；而库里的行是**合法业务数据**（标准流程定义 + 历史审批实例）。

## 2. 处置（已执行）

**补登记进词表（`FlowBizType` 20 → 24），存量行一字未改。**

- 为什么不改存量行 / 不映射到 `PO_APPROVE` 之类：`biz_type` 是各 bridge 认领审批实例的
  **唯一凭据**，也是审批档案的一部分。改值 = 把「销售订单审批」记成「采购订单审批」，
  审批详情、通知、联动、报表分类会全部指向错误对象；历史审批记录被篡改（审计不可接受）。
- 为什么补登记是安全的：这 4 个字面量**本来就写在代码常量和单测里**（`pub const BIZ_TYPE`
  + `assert_eq!(BIZ_TYPE, "SO_APPROVE")`），词表只是漏收编 —— 补登记是让词表追上事实，
  不引入任何新行为。
- 对象轴（`doc_kind()`）按业务如实映射：`SO_APPROVE→SALE_ORDER`、`PRT_APPROVE→PURCHASE_RETURN`、
  `COST_SETTLE→COST_PERIOD`；`PR_APPROVE` 采购申请在对象轴没有成员（不挂附件 / 提醒，
  也没有 `DocKind` 变体）⇒ 返回 `None`，**不硬塞** `PURCHASE_ORDER`。

## 3. 为什么漂移能活这么久（根因，别只修表象）

`doc_kind_tests::approval_entries_use_registered_biz_types` 是「bridge 常量 ∈ 词表」的
机械判据，但它的 bridge 清单是**手写**的 8 个。这 4 个 bridge 后来上线时没人往清单里加，
判据对它们从未执行 ⇒ 词表漏登记无人拦，直到全量库巡检才发现。

本卡已把这 4 个 bridge 补进清单（单测文件），下一条 bridge 漏登记会红在 `cargo test` 上，
不再依赖「有人记得跑全量库巡检」。

## 4. 复查方式（任何人可复跑）

```bash
# 词表 ↔ 库（需要全量迁移库）
NIK_DATABASE_HOST=… NIK_DATABASE_PORT=… NIK_DATABASE_USER=… \
NIK_DATABASE_NAME=… NIK_DATABASE_PASSWORD=… \
  python3 scripts/verify/biz_type_domain.py --strict      # 期望 PASS

# 词表 ↔ 枚举 ↔ 前端镜像（不连库）
cd backend && cargo test --lib doc_kind                    # 期望 32 passed
cd frontend && npx tsx --test tests/unit/flow-biz-link.test.ts
```

实测证据：`docs/evidence/2026-11-13-erp-debt49/01_gate4_biz_type_domain_after.txt`。
