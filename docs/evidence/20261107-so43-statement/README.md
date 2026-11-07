# SO-43 客户对账单（查询 + Excel 导出 + 确认）—— 交付证据

> ARCH-7 §15.7。接口：`GET /api/business/erp/receivable/statement`、
> `.../statement/export?format=xlsx`；表 `biz_statement_confirm`；迁移 `V0317`。

## 1. 交付内容

| 文件 | 内容 |
|------|------|
| `backend/src/business/erp/statement.rs`（新，~1900 行含单测） | 事件流对账引擎 + 自校验 + 6 个接口（查询 / 导出 / 签发 / 确认 / 异议 / 台账分页）+ 12 条纯逻辑单测 |
| `backend/src/business/erp/mod.rs` | `pub mod statement;`（一处） |
| `backend/migrations/business/V0317__statement_confirm.{up,down}.sql` | `biz_statement_confirm` 24 列 + 5 个权限码（挂在「应收台账」页下）+ 角色授权 + 自检块（含 4 条行为探针） |
| `scripts/verify/so43_seed.sql` | 验收夹具（期初 / 挂账 / 退货 / 坏账 / 收款 / 退款 / 作废应收 七种形态） |
| `scripts/verify/so43_statement_e2e.py` | 真实 token + 真实 HTTP 的 46 项断言 |
| `scripts/verify/so43_verify.sh` | 一键：重放夹具 → e2e → 台账落库核对 |
| `scripts/verify/so43_curl_evidence.sh` | 原始 curl 取证（响应头 / xlsx 字节 / 无 token 401） |
| `docs/evidence/20261107-so43-statement/api_evidence.txt` | e2e 全量输出（46 PASS / 0 FAIL） |
| `docs/evidence/20261107-so43-statement/curl_evidence.txt` | curl 原始输出（含 xlsx 头与 sharedStrings 片段） |

## 2. 口径（为什么这么算）

对账单按**单据事件流重放**，不读 `biz_receivable.unsettled_amount` 的当前值：

| 事件 | 来源 | 日期 | 金额 |
|------|------|------|------|
| `INVOICE` | `biz_receivable` | `created_at`（CST 日期） | `+（amount + 该应收已过账退货合计）` |
| `RETURN` | `biz_sales_return`（`POSTED` + 有 `receivable_id`） | `return_date` | `− total_with_tax` |
| `RECEIPT` | `biz_receipt_allocation ⋈ biz_receipt` | `receipt_date` | `− 核销额` |
| `REFUND` | `biz_receipt_refund_allocation ⋈ biz_receipt_refund` | `refund_date` | `+ 冲减额` |
| `WRITE_OFF` | `biz_receivable.status='WRITTEN_OFF'` | `settled_at`（CST 日期） | `− unsettled_amount` |

三条关键决定（都有数据依据，写在模块头注释里）：

1. **挂账金额必须加回已过账退货**：退货过账是**原地扣减** `biz_receivable.amount` 的
   （`sales_return_posting.rs::offset_receivable`），100 挂账退 30 后库里只剩 70；
   若挂账事件直接用 70 再记 −30 退货，期末凭空少 30。
2. **作废应收整笔剔除**：`CANCELLED` 的两种来路（出库作废 / 全额退货）金额都不动，
   任何「原始额 + 当前额」的混算都会让期末多出一笔；作废 = 销售没发生，不进对账单。
3. **补 `WRITE_OFF` / `REFUND` 两类事件**：ARCH-7 §15.7.2 只列了 3 类，但坏账核销与收款退款
   都会改变应收余额，不补这两类则「期末 = Σ 未收」必然对不上（自校验会硬失败）。

## 3. 自校验（卡面硬要求）

`opening + sum(items) = closing` 用**两条互不依赖的路径**对照，任一对不上直接 500：

- 路径 A（明细）：Rust 侧按事件流逐笔累加（用户看到的就是这个数）；
- 路径 B（台账）：同一条事件视图上 `SELECT SUM(amt) WHERE dt <= period_to` 的 SQL 聚合；
- 期初同样双路径（`dt < period_from`）；
- 容差 0.01 分，超出即 `500`「对账单自校验失败」，并回显两条路径的数字与差额。

实测（`api_evidence.txt`）：

```
identity: {passed: True, opening_balance: 12000.00, opening_by_ledger: 12000.00,
           closing_balance: 11500.00, closing_by_ledger: 11500.00, diff: 0.00, tolerance: 0.01}
```

## 4. 隔离环境（可复现）

```bash
# ① 建库（99 上表 owner = alun；root 可 peer 登录 postgres）
su - postgres -c "createdb -O alun cloudpivot_so43"

# ② 起实例（三把密钥必须显式注入，99 上没有 /etc/cloudpivot/*.env）
NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18043 \
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_NAME=cloudpivot_so43 \
NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET='so43-...-0123456789abcdefghij' \
CLOUDPIVOT_STORAGE_MASTER_KEY='0123...(64 hex)' X_CONFIG_SECRET_KEY='so43-...' \
  ./target/debug/cloudpivot      # 日志：数据库迁移完成: 应用 249 个迁移

# ③ 验收
bash scripts/verify/so43_verify.sh        # 46 PASS / 0 FAIL
bash scripts/verify/so43_curl_evidence.sh # 原始 curl 取证
```

迁移记录：`select version,name,kind,applied_via,success` → `317|statement_confirm|versioned|executed|t`；
自检 NOTICE 原文：`SO-43: 对账确认台账 + 5 个权限码 + 应收台账按钮 + 授权自检通过`。

## 5. 实测数据与期望值

夹具（`so43_seed.sql`）与期望值：

```
期初 = AR-SO43-0001 10000（9 月挂账）+ AR-SO43-0003 2000（9 月挂账） = 12000
明细 = AR-SO43-0002 挂账 +5000（10-05）
       SR-SO43-0001 退货 -1000（10-12）
       AR-SO43-0003 坏账 -2000（10-18）
       RC-SO43-0001 收款 -3000（10-20）
       RR-SO43-0001 退款  +500（10-25）
期末 = 12000 + 5000 - 1000 - 2000 - 3000 + 500 = 11500
台账独立核对：AR1 未收 10000-2500=7500 + AR2 未收 4000 = 11500 ✓
作废应收 AR-SO43-0004（999）不在明细里 ✓
```

## 6. 已知边界（不藏着）

1. **金额按单据原币**，不做汇率换算：跨币种混加没有会计意义。客户多币种往来时
   **必须传 `currency`**（否则 400 并列出可选币种）；单一币种自动取。
2. 挂账日期取 `biz_receivable.created_at` 的 CST 日期（= 出库过账建应收那天）：
   表里没有独立的「挂账业务日」列，`invoice_date` 是开票日、`due_date` 是到期日，都不等价。
3. 退货**未挂应收**（`receivable_id IS NULL`，即原发货未过账）不进对账单 —— 它没有冲减任何应收余额。
4. 对账单**不落库快照**（查询与导出都是现算），只有「签发」写 `biz_statement_confirm` 快照；
   签发后账再变，回读台账仍是签发那一版（这是「对账要对得上」的前提）。
5. 本卡**不做前端页面**（对账单挂在既有「应收台账」页里出具），故无 `pnpm` 门禁项；
   5 个权限码已有按钮承载行，前端接入时直接用。
