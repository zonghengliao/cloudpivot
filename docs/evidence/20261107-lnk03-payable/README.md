# LNK-03 取证：应付单（PAYABLE）接入联动引擎

卡号 `t_1a6b0193`。本目录是真机（HTTP 全链路）+ 库内物证的落盘。

## 一句话结论

`PAYABLE` 已**真正接线**：全额付款 / 部分付款 / 结清三条状态推进都会触发联动规则
（通知落 `sys_notice`，幂等键 `LNK:PAYABLE:<状态>:<应付单 id>`），
且**余额只扣一次**（100→0 / 100→70）、**付款凭证每笔恰好 1 张** —— 排除清单生效。

## 接线点（唯一实现，两条入口共用）

```text
直付        payable_payment::pay_in_tx ─┐
                                        ├→ payable_payment::settle_payment_in_tx
审批通过    payable_payment::settle_approved_in_tx ─┘
                                            └─ ④ transition_if_changed（状态真的变才跑）
                                            └─ ⑤ payable_linkage::run_in_tx
                                                  → linkage::engine::run_excluding(
                                                        spec::PAYABLE_EXCLUDED_ACTIONS)
                                        commit 之后
                                            → payable_linkage::run_deferred_after_commit
                                              → linkage::engine::run_deferred（通知 / 建任务）
```

排除清单 `spec::PAYABLE_EXCLUDED_ACTIONS = [UPDATE_PAYABLE_BALANCE, GENERATE_VOUCHER]`：

| 动作码 | 付款事务里的专用落账点 | 通用引擎再跑一次的后果 |
|---|---|---|
| `UPDATE_PAYABLE_BALANCE` | `settle_payment_in_tx` 第 ④ 步条件回写（CAS 在 `paid_amount`） | 已付翻倍 / 未付变负（撞 `ck_biz_payable_unpaid`） |
| `GENERATE_VOUCHER` | `settle_payment_in_tx` 第 ② 步 `voucher::post_voucher`（借 2202 / 贷 1002） | 一笔付款两张付款凭证，总账汇总翻倍 |

`SEND_NOTICE` / `CREATE_TASK` 永远不排除（通知 / 建任务就靠通用引擎的 AfterCommit 动作）。

## 真机取证怎么跑

```bash
# 1) 起隔离实例（DB=cloudpivot_so44，端口 18049）
bash scripts/verify/lnk03_start.sh
# 2) 跑取证（走生产同款 HTTP：配置页 API 建规则 → POST /pay → 查库）
python3 scripts/verify/lnk03_curl_evidence.py
# 3) 停实例
pkill -f 'target/debug/cloudpivot'
```

取证前置：`payable_pay` 流程在取证库里是 `PUBLISHED`（V0175 种子），此时 `POST /pay` 会走
审批（202）。脚本把它临时置 `DRAFT` 走**直付**路径，跑完（含异常退出 `atexit` 兜底）
恢复 `PUBLISHED`。审批路径与直付路径共用同一个 `settle_payment_in_tx`（联动只在那一个函数里接），
代码级由 `tests_wiring::payable_status_wires_generic_engine_and_excludes_balance_and_voucher`
的「两条入口都要 flush」断言 + `business::erp::pflow3b`（23 passed）覆盖。

## 落盘物证（`curl_evidence.txt` 摘要）

| 断言 | 实测 |
|---|---|
| 配置页 `PAYABLE.trigger.wired` | `true`（`wiring` 写明第 ⑤ 步） |
| 写入口 `PUT /api/business/flow/linkage` | 两条规则 `problems=[]` |
| 全额付款 `AP-LNK03-FULL-*`（100/100） | HTTP 200、`SETTLED`、paid=100.00、unpaid=0.00 |
| 部分付款 `AP-LNK03-PART-*`（30/100） | HTTP 200、`PARTIAL_PAID`、paid=30.00、unpaid=70.00 |
| 联动通知 | `应付单已结清` / `应付单部分已付`，`USER` ← 制单人，链回 `/business/erp/payable?id=…` |
| 状态日志 | `OPEN→SETTLED` |
| 付款凭证 | 每笔付款 **1 张**（排除 `GENERATE_VOUCHER` 生效） |

## 残留（取证物证，业务规则不许硬删）

- `cloudpivot_so44` 留 2 张应付单：`AP-LNK03-FULL-*`（SETTLED）/ `AP-LNK03-PART-*`（PARTIAL_PAID）
  + 各 1 条付款记录 + 各 1 张付款凭证 + 各 1 条联动通知。
- 取证规则已按写入口 `DELETE` 删除（`uk_flow_linkage_rule` 不含 `WHERE deleted_at IS NULL`，
  软删仍占坑位 ⇒ 复跑前用 `scripts/verify/lnk03_cleanup.sh` 按 `name LIKE 'LNK03%'` 硬清）。
- 未新增迁移（只接线不改表）。

## 测试

| 用例 | 结果 |
|---|---|
| `cargo test --lib business::flow::linkage`（接线漂移守卫） | 71 passed |
| `cargo test --lib business::erp::payable_payment`（P7/D5 + 本卡 5 条真库） | 34 passed |
| `cargo test --lib business::erp::pflow3b`（付款审批 / 作废审批） | 23 passed |
| 本卡真库 5 条（含「重复过账不重复触发」「排除动作确实不跑」反证） | 5 passed |
