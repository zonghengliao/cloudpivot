# SO-38 · 信用额度占用 / 释放 / 重算 + 不变量 S9

ARCH-7 §15.5。补 v1.0 的漏洞（只占用不释放 ⇒ 客户额度被永久占死）。

## 一、交付物

| 类别 | 文件 |
|------|------|
| 迁移 | `backend/migrations/business/V0313__customer_credit_used.up.sql` / `.down.sql` |
| 口径与实现 | `backend/src/business/erp/credit.rs`（11 个单测：6 纯函数 + 5 真库四场景/重算） |
| 出口钩子 | `sales_order_model.rs`（状态落库，占用/释放唯一写入口：审批回调也走这里）、`receipt.rs`（应收核销）、`receipt_refund.rs`（收款退款反向边）、`sales_return_posting.rs`（退货过账） |
| 提交守卫 | `sales_order_handler.rs::ensure_submittable` 改口径：`credit_used + 在审(SUBMITTED) + 本单 <= limit` |
| 客户接口 | `customer_model.rs` / `customer_handler.rs`：详情多 `credit_used`、`credit_available` |
| 定时任务 | `task/mod.rs`：内置 cron `0 10 2 * * *` + `invoke_target = erp_credit_recompute`（可「执行一次」） |
| 不变量 | `scripts/verify/erp_invariants.py`：新增 **S9**（BLOCKER，容差 0.01） |
| 证据脚本 | `scripts/verify/so38_instance.sh`（隔离实例 18245 / 库 cloudpivot_so38）、`scripts/verify/so38_curl_evidence.sh` |

## 二、口径（唯一真相）

```
credit_used = SUM(有效订单含税) - SUM(已核销应收) - SUM(已过账退货含税)     下限 0，容差 0.01

  有效订单   = biz_sales_order.status IN (APPROVED, PARTIAL_OUT, OUTBOUND)
  已核销应收 = SUM(biz_receivable.received_amount)
  已过账退货 = SUM(biz_sales_return.total_with_tax) WHERE status = 'POSTED'
```

同一份口径出现在三处，**改一处必须改三处**：`V0313` 头注释、`credit.rs::usage_of`、`erp_invariants.py` S9。

### 四个事件（占用 1 + 释放 3 + 反向边 1）

| 事件 | 钩子位置 | cause |
|------|----------|-------|
| 订单迁入 APPROVED（唯一占用时机） | `sales_order_model::apply_status` / `apply_status_with_instance` | `ORDER_OCCUPY` |
| 订单作废/关闭（未发货 → 全额释放） | 同上（迁出占用集合） | `ORDER_RELEASE` |
| 应收核销（**主出口**） | `receipt.rs` 核销回写后，同事务 | `AR_SETTLED` |
| 退货过账 | `sales_return_posting::post_in_tx` 末尾，同事务 | `RETURN_POSTED` |
| 收款退款（② 的反向边） | `receipt_refund.rs` 冲减回写后，同事务 | `AR_REVERSED` |
| 每日兜底重算 | `credit::register_builtin`（02:10）/ 定时任务页「执行一次」 | `RECOMPUTE` |

### 为什么「事件 = 重算」而不是「计数器 += / -=」

卡面写的是 `credit_used += 金额`。实现做成**触发即重算**（同一事务内），理由：

1. **幂等**：收款重放、过账重试、审批回调重复投递结果不变；计数器写法在这三种情况下都会多减/多加。
2. **与 S9 同源**：S9 判据就是这条公式，事件时用同一个函数算 ⇒ 不变量恒真；计数器写法靠「出口一个不漏」，漏一处就漂移。
3. **重算仍必需**：绕过 handler 改库、老数据、将来漏接出口，都由每日 02:10 的重算修平并留痕（`biz_credit_log`）。

代价是每次事件多三条命中 `customer_id` 索引的聚合查询。

## 三、验证（全部真实执行）

### 1) 迁移（`migration_probe.txt`）

```
313|versioned|executed|t|migrations/business/V0313__customer_credit_used.up.sql
```

- `kind=versioned`、`applied_via=executed`（应用启动时真实执行，不是 imported）；
- V0313 落地物：`biz_customer.credit_used numeric(18,4) NOT NULL DEFAULT 0` + `ck_biz_customer_credit_used (>= 0)` + 部分索引 `idx_biz_customer_credit_used`（只索引 `credit_used > 0` 的存活客户）+ 台账表 `biz_credit_log`（含 `ck_biz_credit_log_delta` 恒等约束）+ `sys_job` 种子行（status=0，仅供页面查看/手动执行，避免与内置 cron 双跑）；
- 存量数据**在迁移里一次性按公式回填**（老库直接上线等于「所有客户都没占额度」，比占死更危险）；缺销售链表的库跳过回填并 `RAISE NOTICE`。

### 2) cargo test（真库，非假绿）

```
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so38' \
  cargo test --lib credit::tests -- --test-threads=1
test result: ok. 18 passed; 0 failed   （其中 credit:: 11 条）
```

四场景 + 重算逐条断言（不是「跑通了」，是「等于多少」）：

| 用例 | 断言 |
|------|------|
| `scenario_occupy_on_approved_only` | APPROVED 1000 → used 1000；SUBMITTED 1000 不进计数器、只在 `pending_credit` 里算；重放同一事件不写库不记台账（幂等） |
| `scenario_release_on_settlement` | 核销 400 → 600；收清 1000 → 0；退款回写 400 → 600（反向边） |
| `scenario_release_on_return_posted` | 退货 APPROVED 300 → 仍 1000（未过账不释放）；过账 → 700 |
| `scenario_release_on_cancel` | APPROVED → CANCELLED → 0，Δ = −1000 |
| `scenario_recompute_detects_and_fixes_drift` | 手工把 credit_used 改成 999 → 重算发现并修正回 600，写 `biz_credit_log(cause=RECOMPUTE, trigger=JOB, Δ=-399)`；再跑一次不重复修正 |

改动到的模块**逐模块串行**真库回归（`--test-threads=1`，同一个 cloudpivot_so38）：

| 过滤 | 结果 |
|------|------|
| `credit::tests` | 18 passed / 0 failed（含 purchase_credit 7 条） |
| `sales_order`（含 model + handler） | 27 passed / 0 failed |
| `receipt` | 55 passed / 0 failed |
| `sales_return` | 32 passed / 0 failed |
| `customer` | 30 passed / 0 failed |
| `task::` | 228 passed / 0 failed |

> 全库 `cargo test --lib -- --test-threads=4` 在**改动前就存在**的跨模块夹具争用上挂住
> （进程 CPU 在涨、`pg_stat_activity` 无活动查询 ⇒ 卡在进程内 mutex 上，不是 SQL 行锁），
> 因此本卡的回归按模块串行跑（本仓 DB 用例文档本来就要求 `--test-threads=1`）。
> 这是既有隐患，不属本卡引入；建议后续卡统一把 `cargo test --lib` 固定成串行。

### 3) 接口面（真 token curl，`curl_transcript.txt` → ALL CHECKS PASSED (11)）

- `GET /api/business/erp/customer/{id}` 返回 `credit_used` / `credit_available`，且 `credit_available = credit_limit − credit_used`；
- 注入漂移（`UPDATE biz_customer SET credit_used = 999`）→ 真跑 `erp_invariants.py --group 销售` → **S9 转 🚨 BLOCKER**（报「额度漂移 1 个客户」）；
- `POST /api/support/job/{id}/trigger`（`erp_credit_recompute`）→ HTTP 200，`sys_job_log.message = 信用额度重算：扫描 2 个客户，修正 1 个（CUS-0001 | 999.00 → 0）`；
- 重算后 `credit_used` 回到真值；`biz_credit_log` 有 `RECOMPUTE/JOB` 行（`999.0000→0.0000 Δ-999.0000`）；
- 再跑巡检 → **S9 回绿**。

### 4) 不变量

`s9_group_sales.txt`：`结果：9/9 通过   🚨 BLOCKER 0   ⚠️ WARN 0`（S1–S8 + 新增 S9 全绿）。

## 四、决策与边界（写清楚，不给人挖坑）

1. **占用集合不含 SUBMITTED**：ARCH §15.5.2 说占用唯一时机是 APPROVED。待审单不进 `credit_used`，但**提交守卫**把在审单算成管道占用（否则连开 10 张草稿一起提交，每张单独看都不超、一起就超）。
2. **公式比 ARCH 原文多减「已过账退货」**：ARCH 只写 `Σ有效订单 − Σ已核销应收`。退货过账会冲减原应收的 `amount`（`sales_return_posting::receivable_after`，未收部分才能冲），此时如果不同时减退货额，S9 会在「退货 300、未收款」的库上永久假红。这条已按实际取值口径补齐，三处同源。
3. **`PARTIAL_OUT → CLOSED`（部分发货后关闭）**：订单整张出集（ARCH ①「关闭即全额释放」），已发货未收款部分的敞口不再由本计数器覆盖。这是 ARCH 公式自身的边界，S9 忠实照抄，本卡不改口径。
4. **坏账核销（WRITTEN_OFF）**：只按 `received_amount` 释放，客户继续占着核销损失那部分额度（保守方向，不会超卖）。彻底解法要「坏账释放」第四个出口，属坏账卡范围。
5. **`sys_job` 种子行 status=0**：与 V0210 的四条提醒任务同范式 —— 真调度由代码内置注册，种子行只为「页面可见 / 可手动执行一次 / 可查 cron」。改成 1 会同一 cron 双跑。
6. **重算排在 02:10、巡检 02:20**：先修正再巡检，巡检还红就说明今晚真没修成（不是「修完掩盖问题」：每次修正都写 WARN 日志 + `biz_credit_log`，`/support/job/log/page` 可查）。

## 五、复现

```bash
# 1) 隔离库（克隆 cloudpivot_so37 模板，多跑 V0313）
runuser -u postgres -- createdb -T cloudpivot_so37 -O alun cloudpivot_so38
bash scripts/verify/so38_instance.sh up      # 端口 18245，自动跑 V0313

# 2) 单测（真库）
cd backend && ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_so38' \
  cargo test --lib credit::tests -- --test-threads=1

# 3) 接口证据 + S9
bash scripts/verify/so38_curl_evidence.sh

# 4) 收尾
bash scripts/verify/so38_instance.sh down
```

## 六、残留

- 前端客户档案页还没展示 `credit_used / credit_available`（后端字段已就绪，前端卡可直接用）；
- `biz_credit_log` 暂无查询接口（台账先落库，供对账 / 调查用；要页面再加 `GET /business/erp/credit/log/page`）；
- 隔离库 `cloudpivot_so38` 保留（实例已 down），`cloudpivot_so37` 模板未动；
- 本地 main 提交未 push 到 bare（由 DEPLOY-SYNC 卡收敛）。
