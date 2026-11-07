# LNK-05 取证：费用报销（EXPENSE_REPORT）接入联动引擎

卡片：`t_1254748f`　时间：2026-11-07（CST）　验证机：172.18.10.99（隔离实例 `:18052`，库 `cloudpivot_so44`）

## 交付内容

| 项 | 位置 |
|---|---|
| 唯一联动 hook | `backend/src/business/erp/expense_linkage.rs`（新） |
| 调用点 | `expense_handler.rs`（提交 ⑥ / 付清 ⑥ / 作废 ⑥）+ `expense_bridge.rs`（通过 ⑤ / 打回 ④） |
| 排除清单 | `flow/linkage/spec.rs::EXPENSE_EXCLUDED_ACTIONS = [GENERATE_VOUCHER, REVERSE_VOUCHER]` |
| 接线声明 | `spec::TRIGGER_SPECS` 中 `EXPENSE_REPORT` → `wired=true` + `wiring` + 2 条 `proofs` |
| 静态漂移守卫 | `flow/linkage/tests_wiring.rs`（2 条新用例）+ `flow/linkage/admin.rs`（配置页字典断言） |
| 真库用例 | `backend/src/business/erp/expense_linkage_tests.rs`（6 条，`ERP_DB_URL` 开关） |
| 取证脚本 | `scripts/verify/lnk05_{start.sh,curl_evidence.py,cleanup.sh}` |
| 取证输出 | `curl_evidence.txt`（本目录） |

## 五个接线点（不只卡面的四条）

| 触发点 | 入口 | 目标态 | 事务 |
|---|---|---|---|
| 提交 | `expense_handler::report_submit` | `DRAFT → SUBMITTED` | 事务内 `run_excluding`，commit 后 `run_deferred` |
| 通过 | `expense_bridge::execute_approval` | `SUBMITTED → APPROVED` | 同上 |
| 付清 | `expense_handler::report_pay`（仅 `new_status == PAID`） | `APPROVED → PAID` | 同上 |
| 作废 | `expense_handler::report_cancel` | `→ CANCELLED` | 同上 |
| 打回（额外） | `expense_bridge::revert_to_draft` | `SUBMITTED → DRAFT` | 同上 |

打回必须接：`DRAFT` 是能走到的状态（驳回 / 撤回都回到它），只接四条的话
`EXPENSE_REPORT/DRAFT` 上配的规则**能保存、能启用、永远不触发**（P1-FIX-5C 的原病）。

## 排除清单为什么是这两个码

| 动作 | 报销侧专用落账点 | 通用引擎再跑一次的后果 |
|---|---|---|
| `GENERATE_VOUCHER` | `post_report_voucher`（计提）/ `post_payment_voucher`（付款） | 一张单两张计提凭证，总账翻倍 |
| `REVERSE_VOUCHER` | `reverse_report_vouchers`（作废红冲） | 同一张蓝字被红冲两次 |

`SEND_NOTICE` / `CREATE_TASK` **不能排除** —— 通知与待办就靠通用引擎的 `AfterCommit` 动作。

> 诚实标注：通用引擎的 `GENERATE_VOUCHER` 目前只接管 `STOCK_IN`（`actions::prepare_voucher`
> 对其它实体直接 `Skipped`），`REVERSE_VOUCHER` 仍是占位动作（`implemented=false`）。
> 所以排除清单今天**不是唯一挡板**，它是前向防御（实测：把它清空，真库用例仍绿）。
> 它由静态守卫（`tests_wiring` 两条）锁住；真库用例负责证明「配了凭证规则后凭证仍然恰好 1 张」。

## 验证结果（全部为本机真实执行）

### 1. 全量单测

```
cargo test --lib
test result: ok. 3183 passed; 0 failed
```

基线（LNK-04 后）= 3175，本卡 +8（6 条真库用例默认 SKIP 计数在内 + 2 条 wiring 守卫）。

### 2. 真库用例（`ERP_DB_URL=cloudpivot_so44`，`--test-threads=1`）

```
business::erp::expense_linkage_tests:  6 passed / 0 failed
  lnk05_submit_triggers_linkage_notice
  lnk05_approve_triggers_linkage_notice_and_single_voucher
  lnk05_pay_triggers_only_on_full_payment
  lnk05_cancel_triggers_linkage_notice
  lnk05_repeat_submit_and_pay_do_not_trigger_twice
  lnk05_reject_reverts_to_draft_and_triggers
```

### 3. 红证据（变异测试）

把 `report_submit` 里 `run_in_tx` 的状态实参从 `STATUS_SUBMITTED` 改成 `STATUS_DRAFT`
（规则挂在 SUBMITTED 上 → 引擎查不到规则）：

```
lnk05_submit_triggers_linkage_notice ... FAILED
left: 0  right: 1  （EXPENSE_REPORT/SUBMITTED 上配的 SEND_NOTICE 必须真的发出一条通知）
```

改回即绿 —— 证明这套用例盯的是「handler 真的在调引擎」，不是只看 `wired` 标记。

### 4. 真机 HTTP（`scripts/verify/lnk05_curl_evidence.py`，详见 `curl_evidence.txt`）

```
配置页 wired=true
四条规则 problems=[]（三个状态各一条 SEND_NOTICE + APPROVED 上一条 GENERATE_VOUCHER）
销售员甲建单 EXP-20261107-000004（含税 500）→ 提交 SUBMITTED
  通知 LNK:EXPENSE:SUBMITTED:<id> 条数 1 标题「报销单审批中」
admin（部门负责人）审批通过 → APPROVED
  通知 LNK:EXPENSE:APPROVED:<id> 条数 1 标题「报销单已审批（待付款）」
  该报销单名下凭证总数 1（配着 GENERATE_VOUCHER 规则，排除清单挡掉 → 恰好 1 张）
全额付款 500 → PAID，已付 500 / 未付 0
  通知 LNK:EXPENSE:PAID:<id> 条数 1
  凭证总数 2（计提 1 + 付款 1）
ALL PASS
```

夹具全部自建自清，跑完复核：`reports=0 rules=0 notices=0 instances=0`，
部门负责人恢复 NULL、销售员甲的临时授权已删、`:18052` 已释放。

## 复跑方法

```bash
bash scripts/verify/lnk05_cleanup.sh          # 先硬清（软删的规则占唯一键坑位）
bash scripts/verify/lnk05_start.sh            # 隔离实例 :18052（cloudpivot_so44）
python3 scripts/verify/lnk05_curl_evidence.py # 取证
# 收尾：kill $(ss -ltnp | grep 18052 ...)、再跑一次 cleanup
```

## 已知边界

- 未新增迁移（只接线不改表）。
- `docs/architecture/P1-FIX-5C-联动触发点接线清单.md` 仍是旧时点快照；接线真相以 `spec::TRIGGER_SPECS` 为准。
- 打回 / 撤回的通知与「通过」共用同一份 hook，但文案按目标状态区分（`报销单已退回`）。
