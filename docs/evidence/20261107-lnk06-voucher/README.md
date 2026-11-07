# LNK-06 凭证（VOUCHER）接入联动引擎 —— 取证与口径

## 一句话

`VOUCHER` 的**自身状态推进点**（手工落草稿 / 审核 / 反审核 / 记账 / 红冲）全部接上联动引擎；
`GENERATE_VOUCHER` 是「凭证生凭证」的死循环，**两道闸**拦住它：运行时不跑 + 保存时 400。

## 接线在哪（`spec::TRIGGER_SPECS[VOUCHER]` 的 `wiring` 是真相）

| 触发点 | 事务入口（都是自己 commit 的入口） | 目标态 |
|--------|-----------------------------------|--------|
| 手工凭证落草稿 | `erp/voucher_write.rs::manual_in_tx`（`create_manual_voucher`） | 新建 → `DRAFT` |
| 审核 | `erp/voucher_write.rs::transition_voucher_in_tx`（`audit_voucher`） | `DRAFT → AUDITED` |
| 反审核 | 同上（`un_audit_voucher`） | `AUDITED → DRAFT` |
| 记账 | 同上（`post_audited_voucher`） | `AUDITED → POSTED` |
| 手工红冲 | `erp/voucher_write.rs::reverse_in_tx`（`reverse_voucher`） | 红字凭证新建 → `POSTED` |
| 红冲审批通过 | `erp/voucher_write.rs::reverse_after_approval`（`voucher_bridge` 回调） | 红字凭证新建 → `POSTED` |

唯一的联动 hook 是 `erp/voucher_linkage.rs`（上下文 / 收件人 / 幂等键只写一份）：

- 事务内 `run_excluding(spec::VOUCHER_EXCLUDED_ACTIONS)` → `linkage::engine::run_excluding`；
- commit 之后 `run_deferred_after_commit` → `linkage::engine::run_deferred`（通知 / 建任务永不回滚业务）。

**口径**：只接「凭证自身的状态流转」。来源单据（收货 / 付款 / 报销 …）在
`voucher::post_voucher` 里自动落 `POSTED` 的凭证**不再触发一次** —— 那条路径本身跑在别的
单据的联动事务里（`actions::generate_voucher` → `create_by_rule` → `post_voucher`），
在它内部再调引擎 = 嵌套联动，同一次业务动作会跑两遍规则。

## 死循环的两道闸

1. **运行时**：`spec::VOUCHER_EXCLUDED_ACTIONS = [GENERATE_VOUCHER]`
   → `voucher_linkage::run_in_tx` 传给 `run_excluding`，命中的规则**不产生 `RuleRun`**。
   这一闸兜的是「历史脏行 / 绕过 API 直接写库的行」。
2. **配置期**：`spec::VOUCHER_FORBIDDEN_ACTIONS = [GENERATE_VOUCHER]` + `spec::forbidden_action_reason`
   → `model::upsert_rule`（写规则的唯一入口）与 `admin::check_draft`（配置页预览）
   都直接 **400**，`admin::rule_problems` 给历史脏行标红。

为什么两条都要：只做运行时排除 = 管理员**配得上一条永远不执行的规则**（本仓最讨厌的
「看起来配好了」）；只做配置期 400 = 绕过 API 直接写库的行会把事务打进递归。

## 验证（全部是本机真实执行）

### 1. `cargo test --lib`（不带 `ERP_DB_URL`，真库用例 SKIP）

见卡内汇报的 passed/failed 计数。

### 2. 真库用例（`ERP_DB_URL=…/cloudpivot_so44`，`--test-threads=1`）

```text
business::erp::voucher_linkage_tests 6 passed / 0 failed
  lnk06_forbidden_actions_are_declared                 两道闸都在 spec 里（无库也跑）
  lnk06_generate_voucher_rule_is_rejected_on_save      **配 GENERATE_VOUCHER → 400，且不落库**
  lnk06_create_audit_post_each_trigger_once            DRAFT / AUDITED / POSTED 各一条通知
  lnk06_repeat_post_does_not_trigger_twice             重复记账 409，通知仍 1 条
  lnk06_excluded_generate_voucher_never_runs           绕闸插入的 GENERATE_VOUCHER **不跑**
                                                       （同状态 SEND_NOTICE 照进 deferred，
                                                        证明不是「引擎没读到规则」）
  lnk06_reverse_triggers_posted_for_red_voucher         红字凭证（POSTED）触发一次通知
```

### 3. 漂移守卫 `business::flow::linkage::tests_wiring`

16 passed / 0 failed，其中本卡新增
`voucher_wires_generic_engine_excludes_and_forbids_generate`：

- hook 里 `run_excluding` + `VOUCHER_EXCLUDED_ACTIONS` + `run_deferred` 都在；
- 排除清单含 `GENERATE_VOUCHER`，且**不含** `SEND_NOTICE` / `CREATE_TASK`；
- `model.rs` / `admin.rs` 都真的读 `spec::forbidden_action_reason(`（只在 spec 加常量不算数）；
- `voucher_write.rs` 里 `voucher_linkage::run_in_tx(` 与
  `voucher_linkage::run_deferred_after_commit(` **各 4 处**（四个事务入口都要接）；
- 反向扫描：引擎调用点文件数 9 → 10（新增 `erp/voucher_linkage.rs`）。

### 4. 变异红证据（拆闸必红）

同时拆两道闸（`VOUCHER_EXCLUDED_ACTIONS` 清空 + `forbidden_action_reason` 短路）后：

```text
lnk06_excluded_generate_voucher_never_runs        FAILED（runs 里出现 ["GENERATE_VOUCHER"]）
lnk06_forbidden_actions_are_declared              FAILED（排除清单不含 GENERATE_VOUCHER）
lnk06_generate_voucher_rule_is_rejected_on_save   FAILED（upsert_rule 竟然返回了规则行）
```

还原后 6/6 全绿。

### 5. 真机（生产同款 HTTP，隔离实例 `:18053` + `cloudpivot_so44`）

`scripts/verify/lnk06_start.sh` → `scripts/verify/lnk06_curl_evidence.py`，结果见
`curl_evidence.txt`：

```text
配置页 VOUCHER wired=true
PUT VOUCHER/POSTED + GENERATE_VOUCHER → HTTP 400 code=10001「…凭证生凭证的死循环…」，库里 0 行
POST /api/business/erp/voucher        → DRAFT   PZ-20261107-000595，通知「凭证草稿」1 条
POST /{id}/audit（另一个账号复核）     → AUDITED，通知「凭证已审核」1 条
POST /{id}/post                       → POSTED，通知「凭证已记账」1 条
再 POST /{id}/post                     → 409 code=10003，通知仍 1 条
summary LIKE 'LNK06%' 凭证条数 = 1（没有「凭证生凭证」多出来的凭证）
ALL PASS；夹具（凭证 / 3 条规则 / 通知 / 临时超管授权）已清，实例已关
```

## 复现

```bash
cd /opt/cloudpivot
bash scripts/verify/lnk06_start.sh                # :18053 + cloudpivot_so44
python3 scripts/verify/lnk06_curl_evidence.py     # 证据写进本目录 curl_evidence.txt
bash scripts/verify/lnk06_cleanup.sh cloudpivot_so44   # 兜底清场
```

真库单测：

```bash
cd backend
ERP_DB_URL="postgres://alun:alun@127.0.0.1:5432/cloudpivot_so44" \
  cargo test --lib business::erp::voucher_linkage_tests -- --test-threads=1
```
