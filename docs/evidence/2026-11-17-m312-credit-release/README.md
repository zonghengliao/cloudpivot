# M3-12 客户信用：占用释放（收款核销后）· 核销即释放

- 卡：`t_f4f53ec4`（M3-12【M3·销售】客户信用：占用释放（收款核销后））
- 上游：`t_464b7265`（M3-09 策略三态）、`t_8729b656`（M3-10 实时占用）、
  `t_8a08ce48`（M3-11 超信用拦截）
- 下游：`t_5fc52acf`（M3-13 单测：并发下单 / 部分收款）、`t_5f9041b6`（M3-14 客户信用页）

## 一、这张卡要的到底是什么（先把事实说清）

**释放出口本身在 SO-38 就已经接了**（`credit.rs` 模块头「三个释放出口」表：①订单作废 ②应收核销
③退货过账，外加退款的反向边）。M3-11 的交接也写着「M3-12 直接调 `credit::sync(cause=AR_SETTLED)`」。

但 SO-38 那套用例**是绕过核销写路径**的（`scenario_release_on_settlement` 直接
`UPDATE biz_receivable SET received_amount = 400` 再调 `sync`）：它只能证明「公式对」，
证明不了「核销这条路上真的调了它」。钩子被删、被 `if` 挡、被挪出事务，那些用例照样全绿。

所以本卡做三件事，**不重复造释放逻辑**：

1. **核销即释放的当场回执**（唯一的生产代码改动）：核销 / 退款核销的响应带
   `credit_release`，让「这次动作放了多少额度」可见、可对账（M3-14 页面直接读）；
2. **把「核销 → 释放」这条链钉进真库用例**（走 `receipt::create → receipt::allocate` 真接口）；
3. **一处静默缺陷收敛**：`credit::sync` 的 CAS 重试耗尽时原来什么都不说（额度可能停在旧值），
   现在打 WARN 并把轮数提成常量 `SYNC_ATTEMPTS`。

## 二、交付物

| 类别 | 文件 | 说明 |
|------|------|------|
| 回执类型 | `backend/app/src/business/erp/credit.rs` | `CreditRelease`（`from_sync`：**没真动额度就 `None`**）+ 模块头新增「核销即释放（M3-12）」一节（4 种动作 × 释放量 × 台账 cause × 回执，三个「不许」，三条已知边界） |
| 静默缺陷收敛 | 同上 | `SYNC_ATTEMPTS = 5`（原硬编 3）；轮数耗尽且缓存列仍与公式不一致时 `tracing::warn!`（原分支静默返回 `repaired = false`） |
| 收款核销回执 | `backend/app/src/business/erp/receipt.rs` | `ReceiptDetailRes.credit_release`（`skip_serializing_if` 空值不出现）；`allocate` 同事务回填；`detail_with` 恒 `None` |
| 退款核销回执 | `backend/app/src/business/erp/receipt_refund.rs` | `RefundDetailRes.credit_release`，同上（`released < 0` = 占回） |
| 真库用例 | `backend/app/src/business/erp/credit_release_tests.rs`（新，7 条） | 全部走真接口；含报文契约 |
| 注册 | `backend/app/src/business/erp/mod.rs` | `#[cfg(test)] mod credit_release_tests;` |
| 演练脚本 | `scripts/verify/m312_credit_release_drill.sh`（新，30 断言） | 静态接线 + 4 组真库用例 + 收尾 S9 对拍 |
| 证据 | `docs/evidence/2026-11-17-m312-credit-release/{README.md,00-drill.out,01-backend-lint.out}` | 本文件 |

**不加迁移**（无 DDL，下一个可用号仍是 `V0438`）；**不造新权限码**（回执只是既有接口的响应字段）；
**不动前端**（新增字段是可选、且空值不出现 —— 前端现在的类型不受影响，M3-14 直接读）。

## 三、口径（与 SO-38 / S9 同源，没改公式）

```text
credit_used = SUM(有效订单含税[APPROVED/PARTIAL_OUT/OUTBOUND]) - SUM(已核销应收) - SUM(已过账退货)   下限 0
```

核销**只改**「已核销应收」这一项，所以「核销即释放」= 同一个 `sync` 重算一次：

| 动作 | 释放量 | 台账 `cause` | 回执 |
|------|--------|--------------|------|
| 收款核销（写 `received_amount`） | = 本次核销**增量** | `AR_SETTLED` | `ReceiptDetailRes.credit_release` |
| 同一张收款把核销额**调小** | 负数（敞口**占回**） | `AR_SETTLED` | 同上，`released < 0` |
| 收款退款核销（`received_amount` 减少） | 负数（占回） | `AR_REVERSED` | `RefundDetailRes.credit_release` |
| 重放同一笔（增量 0） | 0 ⇒ **无回执、无台账** | — | `None` |

「有回执 ⇔ 有台账行 ⇔ `credit_used` 变了」三件事**同真同假**（`CreditRelease::from_sync` 只在
`CreditSync::repaired` 时构造）。`released = before_used − after_used`，与台账 `delta`
（`after − before`）互为相反数 —— 各自向着自己的语境（「释放了 400」/「占用 −400」）。

**报文契约**（`credit_release_wire_contract_is_stable` 钉住）：字段
`customer_id / cause / ref_no / before_used / after_used / released`，金额是**定点字符串**
（本仓 `rust_decimal` 开 `serde-str`，与 `ReceiptRes.amount` 等同口径）。

## 四、验证（全部真实执行，日志在 00/01）

| 项 | 结果 |
|---|---|
| `cargo fmt -p cloudpivot -- --check` | 0 差异（见 `01-backend-lint.out`） |
| `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | 0 warning（比卡面要求多了 `--tests`） |
| `bash scripts/verify/m312_credit_release_drill.sh` | **ALL PASS，PASS=30 FAIL=0**（见 `00-drill.out`） |
| └ `business::erp::credit_release_tests` | 7 条全绿，SKIP=0 |
| └ `business::erp::credit` | 37 条全绿，SKIP=0（含 SO-38 四场景 + M3-09/10/11） |
| └ `business::erp::receipt` | 35 条全绿，SKIP=0 |
| └ `business::erp::receipt_refund` | 24 条全绿，SKIP=0 |
| └ 收尾 S9 对拍 | M312 客户 `credit_used` 与公式 **0 漂移**（容差 0.01） |
| 演练库 | 145 上 `cp_m312%` 残留 **0**（脚本自 DROP）；夹具订单/应收/收款/退款/台账 全 0 |

演练库按卡面约束建在 **145（`172.18.10.145:5432`，用户 `cloudpivot`）**，库名含 `drill`；
模板库 `cloudpivot_test` 落后 33 条业务迁移，脚本按 `V0405…V0437` 逐条幂等补齐（与 M3-10/11 同法）。

### 7 条用例各自钉什么

| 用例 | 断言（不是「跑通了」，是「等于多少」） |
|---|---|
| `db_partial_allocate_releases_by_delta` | 核销 400 ⇒ 占用 1000→600；台账 `AR_SETTLED/-400/API`；回执 1000/600/400；应收 PARTIAL(400/600) |
| `db_full_settlement_zeroes_credit` | 核销 1000 ⇒ 占用 **0**（不是「容差内」）；应收 SETTLED；回执 `released=1000` |
| `db_reallocate_tracks_delta_both_ways_and_replay_is_idempotent` | 400→500 再释放 100；500→300 **占回 200**（`released=-200`）；重放 300 无回执/无台账/额度不动；**GET 详情恒无回执** |
| `db_failed_allocate_leaves_credit_untouched` | 核销 1200 被拒后：额度仍 1000、台账仍 1 行、应收仍 OPEN(0/1000)、`allocated_amount` 仍 0 |
| `db_refund_reoccupies_credit` | 收清后退 400 ⇒ 占用 0→400；台账 `AR_REVERSED/+400`；回执 `released=-400`；应收回 PARTIAL(600/400) |
| `credit_release_is_only_built_when_credit_moved` | `repaired=false` ⇒ `None`；正负号约定 |
| `credit_release_wire_contract_is_stable` | 6 个字段名 + 金额是定点字符串 |

## 五、决策（写清楚，不给人挖坑）

1. **回执不落库、位置在详情 DTO 上**：它是「这次动作的结果」，不是单据属性。存库会变成会过期的
   假数据（明天再读 GET 详情还能看到「本次释放 400」）；所以 `detail_with` 恒 `None`，
   只有 `allocate` 在事务内回填，且空值走 `skip_serializing_if` 不出现在报文里。
2. **`released` 正数=释放、负数=占回**（与台账 `delta` 相反数）：台账读的是「占用怎么变」，
   回执读的是「放出多少」，两边都按各自语境写，换算只有一次减法，且有 `+`/`-` 的对拍断言。
3. **失败一律整体回滚，不做「先释放再校验」**：`sync` 在核销事务里、在任何 Err 之前没有
   `commit`，所以「核销被拒」与「额度白放」不可能同时发生（`db_failed_allocate_leaves_credit_untouched`）。
4. **`SYNC_ATTEMPTS` 提成常量 + 耗尽打 WARN**：原来 3 轮全败是静默返回 `repaired=false` ——
   调用方以为成功、日志里什么都没有，而缓存列已经与真账不一致（S9 会红，但要等到夜里）。
   现在轮数可调、且「未落库」这件事会进日志。
5. **不动 `usage_of` 的公式**：核销释放**复用**同一个重算入口（R1 单一数据源），不写第二份
   「减掉这次核销额」的算法 —— 计数器写法在重放 / 调小 / 退款三种场景都会漂移。

## 六、已知边界（**不是**漏了）

1. **核销不改订单状态**：客户级敞口按客户整体算，不按单核销；订单侧不会多一个「已释放」字段。
2. **一张收款核多张应收 ⇒ 台账只有一行**：客户级重算，`delta` 是这几张增量之和。
3. **预收（收款未核销）不释放**：钱没挂到任何应收上，`received_amount` 没变（口径如此，见 PRD 4.7.2）。
4. **反核销接口仍不存在**（SO-18 起的已知缺口，属 PRD `S4-13` 卡）：收款单要作废得先反核销，
   `change_status` 的文案已经把这条路指出来，但按钮还没落地。本卡没做（那是新接口 + 新码 + 菜单行，
   不属「释放」这一步）；**注意**：反核销一旦落地，必须同时调 `sync(cause=AR_SETTLED)`，
   否则额度只放不占回。
5. **坏账核销（`WRITTEN_OFF`）不释放**：口径边界沿用 SO-38（保守方向，不会超卖），属坏账卡。
6. **`.worktrees/`、README、frontend 的其它在途改动**不是本卡的（同仓多 worker），提交时只按路径
   `git add` 本卡的 8 个文件。

## 七、给下游卡

- **M3-13（并发下单 / 部分收款单测）**：本卡已把「部分收款 → 按增量释放」的真实链路钉住
  （`db_reallocate_tracks_delta_both_ways_and_replay_is_idempotent` 覆盖了调大/调小/重放），
  并发用例可直接在同一夹具上加两个 `tokio` 任务；`credit::SYNC_ATTEMPTS` 与 sync 尾部的 WARN
  是并发打满时的观察点。
- **M3-14（客户信用页）**：核销 toast 直接读 `credit_release`（6 个字段，金额是字符串）；
  页面主数据仍走 `GET /business/erp/credit/{customer_id}`（M3-10）。
