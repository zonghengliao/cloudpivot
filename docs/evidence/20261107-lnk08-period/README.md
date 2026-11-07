# LNK-08 物证：会计期间（ACCOUNTING_PERIOD）接入联动引擎

- 卡：`t_4096e1d6`（【LNK-08】接线：会计期间（PERIOD））
- 唯一 hook：`backend/src/business/erp/period_linkage.rs`
- 调用点：`backend/src/business/erp/period.rs::close_period`（结账 `OPEN → CLOSED`）/
  `reopen_period`（反结账 `CLOSED → OPEN`）
- 事务边界：事务内 `run_excluding(spec::PERIOD_EXCLUDED_ACTIONS)`；
  通知一律 `tx.commit()` 之后 `run_deferred_after_commit` → `run_deferred`

## 1. 真机 HTTP（`curl_evidence.txt` 是同一次运行的完整输出）

实例：`bash scripts/verify/lnk08_start.sh`（DB=`cloudpivot_lnk08`，端口 18055）
取证：`python3 scripts/verify/lnk08_http_evidence.py`

```
① GET /api/business/flow/linkage?trigger_entity=ACCOUNTING_PERIOD → wired=true（配置页不再骗人）
② PUT ACCOUNTING_PERIOD/CLOSED、/OPEN 各一条 SEND_NOTICE（TAKEOVER→OPERATOR）
③ POST /api/business/erp/period {2033,5}        → 200 OPEN
④ POST /api/business/erp/period/{id}/close      → 200 CLOSED，通知 LNK:PERIOD:CLOSED:<id> = 1
⑤ 重复 close                                    → 409，通知仍 = 1
⑥ POST /api/business/erp/period/{id}/reopen     → 200 OPEN，通知 LNK:PERIOD:OPEN:<id> = 1
⑦ 开 2033-06 + 注入「commit 期失败」延迟约束触发器
     close → 500 / 期间 OPEN / 通知 0    ← 事务回滚时通知不发（卡面硬要求）
     拆掉注入后再 close → 200 CLOSED / 通知 1   ← 证明上面 0 条是回滚造成的
ALL PASS
```

## 2. 真库用例（`ERP_DB_URL` 开关，6 条）

```
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_lnk08 \
  cargo test --lib business::erp::period_linkage_tests -- --test-threads=1
```

| 用例 | 证明什么 |
|---|---|
| `lnk08_close_triggers_closed_notice` | 结账真的触发一次通知 |
| `lnk08_reopen_triggers_open_notice` | 反结账触发；结账**不会**误触发 OPEN 规则 |
| `lnk08_rolled_back_tx_never_sends_notice` | 事务内 `SEND_NOTICE` 只进 deferred；回滚后 0 条；commit+flush 后 1 条（正向对照） |
| `lnk08_commit_failure_keeps_period_open_and_silent` | 真实结账事务在 commit 处失败 → 期间仍 OPEN + 0 条通知；拆注入后 1 条 |
| `lnk08_repeat_close_does_not_trigger_twice` | 重复结账 409，通知不增 |
| `lnk08_excluded_ledger_actions_never_run` | 期间上 `GENERATE_VOUCHER` 被排除（引擎级对照 + 端到端 0 凭证），同状态 `SEND_NOTICE` 照发 |

## 3. 单测 / 漂移守卫

- `cargo test --lib`：**3208 passed / 0 failed**（基线 3199，本卡 +9）
- `tests_wiring`：18 条全绿；调用点 11 → **12**，wired 10/15 → **11/15**，
  新增 `period_wires_generic_engine_and_excludes_ledger_actions`（含源码切片断言
  「`run_in_tx` 在 commit 前、`run_deferred_after_commit` 在 commit 后」）

## 4. 变异红证据（证明测试不是虚绿）

| 变异 | 结果 |
|---|---|
| `run_excluding(ex, &ctx, &[])`（把排除清单改成空） | `tests_wiring::period_wires_generic_engine_and_excludes_ledger_actions` **FAILED**（「必须真的把 PERIOD_EXCLUDED_ACTIONS 传下去」） |
| `close_period` 传给 hook 的目标态改成 `STATUS_OPEN` | `period_linkage_tests::lnk08_close_triggers_closed_notice` **FAILED**（left: 0 / right: 1） |

两处均已还原并重跑全绿。
