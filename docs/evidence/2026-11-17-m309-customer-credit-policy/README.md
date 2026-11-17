# M3-09 客户信用：额度字段 + 超限策略配置（t_464b7265）

- 分支 / 提交：`main`（见卡内汇报的 commit hash）
- 迁移：`V0437__customer_credit_policy`（up / down 双向幂等，已应用迁移一条没动）
- 前置：`V0260`（`biz_customer` + `credit_limit`）/ `V0289`（`credit_days`）/ `V0313`（`credit_used` + `biz_credit_log`）/ SO-38（占用 / 释放 / 兜底重算）

## 一、本卡做了什么（只做这一件事）

额度字段本身早已在位（`biz_customer.credit_limit` + `credit_used` + 公式口径 SO-38）。本卡补的是**「超限时怎么办」的配置面**，并把它真正接进销售订单提交守卫：

| 层 | 落点 |
|---|---|
| 库 | `biz_customer.credit_policy VARCHAR(10) NOT NULL DEFAULT 'BLOCK'` + `ck_biz_customer_credit_policy CHECK IN ('BLOCK','WARN','PASS')`（V0437） |
| 判定 | `erp::credit::decide()`（纯函数，穷举单测）+ `CreditVerdict`（Uncontrolled / Within / Warn / Pass / Block） |
| 读 | `credit::policy_of()`（单列读）；`CustomerRow.credit_policy` 随整行走 |
| 写 | `credit::note_over_limit()`（`biz_credit_log` cause = `OVER_LIMIT_WARN`，`delta = 0`，**幂等**） |
| 客户 API | 新建 / 编辑收 `credit_policy`（留空 = `BLOCK`，非法值 400）；详情 / 列表出参带 `credit_policy` |
| 订单守卫 | `sales_order_handler::ensure_submittable` 按策略处置；响应新增 `credit_warning`（只有提交接口填） |
| 前端 | 客户档案页「超限策略」列 + 表单下拉；销售订单提交后按 `credit_warning` 弹警告 |

### 三种策略的行为（唯一真相 = `credit::decide`）

| 策略 | 超限提交 | 留痕 | 用户可见 |
|---|---|---|---|
| `BLOCK`（**默认**） | 409，单据留 DRAFT | 无（错误报文即证据） | 提交失败 + 四个数（额度/已占/在审/超出） |
| `WARN` | 放行，正常进审批 | `biz_credit_log` 一条 `OVER_LIMIT_WARN`（`delta = 0`）+ WARN 日志 | 响应 `credit_warning` → 前端长提示 |
| `PASS` | 静默放行 | 无（仅 INFO 日志） | 无提示 |

- `credit_limit = 0`（不控额度）→ `Uncontrolled`，**任何策略都不生效**，也不打额外聚合查询。
- 边界：`已占 + 在审 + 本单 = 额度` **算通过**（与 M3-09 之前的 `after > limit` 逐字一致）。
- 未知策略值 → **fail-closed 到 `BLOCK`**（手改库 / 回滚迁移都不能把内控静默关掉）。

## 二、为什么这样定

1. **默认必须 `BLOCK`**：本卡之前代码是硬编拦截。默认给 `WARN`/`PASS` 等于「升级即静默关掉内控」，是最不能接受的静默变更。存量行由 `DEFAULT` 自动填 `BLOCK`，上线零行为变化。
2. **策略挂在客户上，不做全局开关**：「哪家可以超限发货」是客户维度的商务决策（老客户 / 战略客户 / 预付款客户）；全局开关表达不出来。
3. **判定与处置解耦**：`decide()` 只返回裁决，409 / 提示 / 台账由 handler 决定 —— 判定因此能被单测穷举（7 条纯函数用例覆盖三策略 + 边界 + 未知值）。
4. **WARN 用 `delta = 0` 的台账行留痕**：只打日志会在轮转后丢失；`ck_biz_credit_log_delta` 允许 0，S9（`credit_used` 对公式）完全不受影响。
5. **`note_over_limit` 幂等**：同一请求里 `change_sales_order_status` 与 `submit_approval` 都会调它（后者走「没配流程」分支时会进前者），靠 `(customer_id, cause, ref_no)` 的 `WHERE NOT EXISTS` 把第二次变成 0 行；顺带保证「提交 → 撤回 → 再提交」不会堆一串重复警示。
6. **台账写在「单据已成功流转之后」**：提交失败（没配流程 / 自审被拒）不该留一条「已放行」的假凭据；写失败只 WARN（已提交的单不能因为少一条台账被打回）。
7. **入口不静默降级**：`credit_policy = "WARNING"` / `"warn2"` 一律 400（`parse_policy` 返回 `None`），不许「拼错也存成 BLOCK」——用户会以为改成功了。

## 三、文件清单

后端：

- `backend/app/migrations/business/V0437__customer_credit_policy.up.sql` / `.down.sql`（新）
- `backend/app/src/business/erp/credit.rs`（策略常量 / `decide` / `policy_of` / `note_over_limit` + 单测 9 条）
- `backend/app/src/business/erp/customer/customer_model.rs`（列清单 / `CustomerRow` / `CustomerWrite` / INSERT / UPDATE）
- `backend/app/src/business/erp/customer/customer_handler.rs`（DTO + 校验 + 出参 + 单测 1 条）
- `backend/app/src/business/erp/sales/sales_order_handler.rs`（`ensure_submittable` 三态 + `CreditWarning` + `SalesOrderRes.credit_warning` + 单测 1 条 + 清场补 `biz_credit_log`）

前端：

- `frontend/src/api/business/erp/index.ts`（`CreditPolicy` 类型 / `Customer.credit_policy` / 入参 / `SalesOrder.credit_warning`）
- `frontend/src/views/business/erp/customer/customer-query.ts`（选项 / 文案 / 表单字段 / payload）
- `frontend/src/views/business/erp/customer/index.vue`（列 + 表单下拉）
- `frontend/src/views/business/erp/sales-order/index.vue`（提交后的 `credit_warning` 提示）
- `frontend/tests/unit/erp-customer.test.ts`（列清单 / payload 期望更新 + 策略用例 4 条）

脚本与证据：

- `scripts/verify/m309_credit_policy_drill.sh`（本卡真库演练，29 断言）
- `docs/evidence/2026-11-17-m309-customer-credit-policy/00-drill.out`（实际输出）

## 四、验收（真实执行结果）

```bash
# 后端
cd backend && cargo fmt && cargo clippy --lib -- -D warnings      # 0 warning
# 前端
cd frontend && pnpm typecheck && pnpm build                        # OK；pnpm test:unit = 2159 pass / 0 fail
# 真库演练（99 → 145，临时库，跑完自删）
bash scripts/verify/m309_credit_policy_drill.sh                    # ALL PASS，PASS=29 FAIL=0
```

演练覆盖：

| 段 | 断言 |
|---|---|
| 迁移 | V0405…V0437 全部应用；V0437 重复 up / down / 再 up 三连都成功；down 不碰 `credit_limit` / `credit_used` / `credit_days` |
| 结构 | `varchar(10) NOT NULL` / CHECK 域值三值 / 存量行全在域内 / 省略列落默认 `BLOCK` / 越界值被库拒 / 探针 0 残留 |
| 真库用例 | `business::erp::credit` 20 条、`business::erp::customer` 20 条、`business::erp::sales` 206 条，**全部 SKIP=0**（真库用例没被打成假绿） |
| 清场 | `CR38-` / `SO5HT-` / `SO2T-` / `__M309_*` / `__V0437_PROBE*` 与 `biz_credit_log` 警示行 0 残留 |

## 五、已知边界 / 交接（不是漏了）

- **M3-10（下单实时占用）**：守卫已经用 `credit::stored`（缓存列）+ `credit::pending_credit`；本卡没改口径，只把「超没超」的判定收敛到 `decide()`。后续做「实时可用额度」接口时请**直接复用 `decide()`**，别再写第二份比较。
- **M3-11（超信用拦截可配置）**：本卡已经把三策略接进销售订单提交守卫（409 / 放行+提示 / 静默放行）。M3-11 若还要做「采购侧」「审批流条件分支按 `over_credit_limit` 走财务复核」，在 `form_data` 里加超限标志即可，不必再改策略存储。
- **告警下游**：`WARN` 目前是「台账 + 响应提示 + 日志」。要接通知（站内信 / 待办）需要另开卡 —— 本卡不夹带。
- **审批通过（APPROVED）时的二次校验**：占用发生在 APPROVED，若审批期间额度被别人用满，审批通过仍会占用（SO-38 现有行为，本卡未改）。这是「放开审批 + 收紧提交」的既有取舍，要改成「过审时再校验」属于 M3-11 的范围。
