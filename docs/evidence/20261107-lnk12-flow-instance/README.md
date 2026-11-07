# LNK-12 审批实例（`FLOW_INSTANCE`）接入联动引擎 —— 设计 + 取证

- 卡号：`t_2417bec2`
- 日期：2026-11-07
- 提交：`6eb7699`（主体）+ `e83e1f8`（补 `create_instance` 第五个调用点）
- 测试环境：145 `http://127.0.0.1:8033`，库 `cloudpivot_dev`，代码 `e83e1f8`

---

## 1. 设计结论（卡面三问）

### ① 谁先谁后 → **审批实例先，业务单据后**

```
handler::approve_task
  ① db().transaction(engine::approve)      ← 实例落终态 APPROVED，提交
  ② drain(out)                             ← 引擎自己的通知（事务外）
  ③ instance_linkage::fire_after_commit    ← 【本卡】FLOW_INSTANCE/APPROVED
  ④ task_bridge / purchase_bridge / …      ← 业务桥改单据状态 SUBMITTED→APPROVED
                                              └─ 单据自己的触发点 PURCHASE_ORDER/APPROVED
```

三条理由：

1. **因果**：实例状态是原因，单据状态是结果；业务桥的输入就是 `out.instance_status`；
2. **失败语义**：业务桥的错误一律被吞掉（`on_po_settled` 返回 `Option`，失败只打日志），
   把本触发点排在桥后面，等于「桥今天挂了 → 审批通过的通知 / 待办永远丢」；
3. **事务与锁**：本触发点只有 `Phase::AfterCommit` 动作（见 ②），不开事务、不占锁，排最前面开销最小。

### ② 会不会两遍？→ **同一触发点绝不两遍；跨触发点只可能两条不同语义的通知，且绝不会重复落账**

三层去重：

| 层 | 机制 | 挡住的形态 |
|----|------|-----------|
| 入口唯一 | 只有 `flow::instance_linkage::fire_after_commit` 调引擎；五个调用点：`approve_task` / `reject_task` / `withdraw_instance` / `create_instance`（发起即自动走完）/ `timeout::auto_decide`（超时自动代点） | 「同一处理器里两处各调一次」 |
| 状态闸 | 只认 `APPROVED / REJECTED / WITHDRAWN`，且**实例当前状态 == 触发状态** | 中间票（会签第一票仍是 PENDING）、传错状态、旧出参重放 |
| 幂等键 | 通知 `idempotency_key = LNK:FLOW_INSTANCE:<状态>:<实例 id>`（`uk_sys_notice_idem`） | 重放 / 重试：只落一条 |

**跨触发点**：同一张采购单既配 `FLOW_INSTANCE/APPROVED` 又配 `PURCHASE_ORDER/APPROVED`
会各发一条 —— 这是两条不同业务事实（「你提交的审批单通过了」vs「采购单已审批，可以去收货了」）。
本卡不做跨触发点通知合并（合并必须有「谁去重谁」的全局约定，任一方向都会静默吞掉合法通知）；
要只发一条就只在一处配。

**真正危险的是落账，不是通知**：本触发点在**审批事务提交之后**执行，进程里没有事务。
所以全部 `Phase::InTx` 动作**配置期即 400**（`spec::FLOW_INSTANCE_FORBIDDEN_ACTIONS`）
+ **运行期排除**（`spec::FLOW_INSTANCE_EXCLUDED_ACTIONS`，兜历史脏行 / 绕过 API 写库）。
「审批通过 → 改单据状态 / 写库存 / 出凭证」的唯一实现是业务桥，因此同一笔账不可能落两遍。
单测 `instance_linkage::tests::allowed_actions_are_all_after_commit` 会在「有人把某个 InTx
动作从禁令里放出来」时立刻变红。

### ③ 驳回 / 撤回是否触发 → **都触发**

`REJECTED`（驳回并终止）与 `WITHDRAWN`（发起人撤回）是审批的另外两种**结论**，
而且是最需要通知发起人的两种。不接的三个状态与理由：

| 状态 | 为什么不接 |
|---|---|
| `PENDING` | 不是结论；发起那一刻由流程自己的通知节点（`notify_node`）负责，再发一次就是重复站内信 |
| `SKIPPED` | 节点 / 流程任务的状态，**从来不是实例状态**（DDL CHECK 里没有它） |
| `CANCELLED` | 唯一来源是级联 / 行政终止（任务取消、业务单据删除、报销作废），其中「单据删除 → 终止实例」走 `cancel_pending_of_biz` 直接 SQL、不经过引擎 ⇒ 接在这里会「同一张单有时响、有时不响」，比不接更糟 |

于是**可配状态 = 会触发的状态 = `APPROVED / REJECTED / WITHDRAWN`**，三处
（`spec::TRIGGER_SPECS.status_labels`、`linkage::model::known_statuses`、
`instance_linkage::TERMINAL_STATUSES`）由单测 `statuses_match_fired_verdicts` 钉住逐字一致
—— 「配得上、永远不响」正是 P1-FIX-5C 那类静默失效。

---

## 2. 改动清单

| 文件 | 改动 |
|---|---|
| `backend/src/business/flow/instance_linkage.rs`（新） | 唯一 hook `fire_after_commit`（`run_excluding` + `run_deferred`）+ 通知基线草稿 + 6 条纯逻辑单测 |
| `backend/src/business/flow/instance_linkage_db_tests.rs`（新） | 4 条真库用例（`ERP_DB_URL` 开关） |
| `backend/src/business/flow/handler.rs` | 4 个调用点：`approve_task` / `reject_task` / `withdraw_instance` / `create_instance` |
| `backend/src/business/flow/timeout.rs` | 1 个调用点：`auto_decide`（超时自动代点） |
| `backend/src/business/flow/linkage/spec.rs` | `wired=true` + `wiring` + `proofs`；`FLOW_INSTANCE_EXCLUDED_ACTIONS` / `FLOW_INSTANCE_FORBIDDEN_ACTIONS`；`forbidden_action_reason` 的 FLOW_INSTANCE 文案；可配状态收窄 |
| `backend/src/business/flow/linkage/model.rs` | `known_statuses(FLOW_INSTANCE)` → 三个结论 |
| `backend/src/business/flow/linkage/tests_wiring.rs` | wired 14→15 / unwired 1→0 / 调用点 15→16 + `flow_instance_wires_generic_engine_and_excludes_ledger_actions` |
| `backend/src/business/flow/linkage/admin.rs` | catalog wired 断言加 `FLOW_INSTANCE`；未接线草稿用例改成「新增未接线项时自动生效」 |
| `docs/architecture/P1-FIX-5C-联动触发点接线清单.md` | 表 15 行改 ✅、批次 D 改完成、§12 第 4 条定稿 |

---

## 3. 验证

### 3.1 单测（99，`cargo test --lib`）

```
test result: ok. 3255 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

基线 `3244`（LNK-11 后）→ `3255`（+11：hook 纯逻辑 6 + wiring 1 + 真库 4）。

### 3.2 真库用例（99，`ERP_DB_URL=…/cloudpivot_lnk11`）

```
business::flow::instance_linkage_db_tests::lnk12_approve_fires_once_and_replay_is_deduped ... ok
business::flow::instance_linkage_db_tests::lnk12_pending_instance_never_fires ... ok
business::flow::instance_linkage_db_tests::lnk12_excluded_ledger_action_never_runs ... ok
business::flow::instance_linkage_db_tests::lnk12_reject_verdict_gets_its_own_key ... ok
test result: ok. 4 passed; 0 failed
```

跑完清场复查：`biz_flow_linkage_rule`（FLOW_INSTANCE）0 行、`LNK:FLOW_INSTANCE:%` 通知 0 条、
`LNK12_TEST` 实例 / 定义 0 行。

### 3.3 真机 HTTP（145 测试环境 `:8033`，代码 `e83e1f8`）

脚本：`scripts/verify/lnk12_http_evidence.py`（口令从 `NIK_ADMIN_PASSWORD` 读，不落仓库），
原始输出：[`curl_evidence.txt`](./curl_evidence.txt)。

```
[OK ] 触发点字典 → HTTP 200          FLOW_INSTANCE wired=true，可配状态只有 APPROVED / REJECTED / WITHDRAWN
[OK ] 配 GENERATE_VOUCHER → HTTP 400  报文点名「同一笔账落两遍（一张单两张凭证 / 累计量被推两次）」
[OK ] 配 PENDING 状态 → HTTP 400      「FLOW_INSTANCE 没有状态「PENDING」：可用 APPROVED / REJECTED / WITHDRAWN」
[OK ] 配 FLOW_INSTANCE/APPROVED → SEND_NOTICE → HTTP 200
[OK ] 发起审批单（flow_edge_config）→ HTTP 200，实例 PENDING
[OK ] 同意待办 → HTTP 200，实例 APPROVED
      本卡配置的通知：0 条 → 1 条              ← 正常触发一次
[OK ] 重复同意同一待办 → HTTP 200 idempotent=true，实例仍 APPROVED
      本卡配置的通知：1 条 → 1 条              ← 重复触发被抑制（幂等键挡下）
[OK ] 清场：删规则 → HTTP 200
===== ALL PASS =====
```

> 说明：`approve` 被重复调用时引擎按幂等放行（`200 / idempotent=true`，不是 409），
> 所以「重复触发被抑制」的判据是**通知条数不变**，不是状态码。这一点已写进脚本注释。

### 3.4 145 测试环境部署（`bash /root/test_deploy.sh`）

```
ce413d67 → 6eb76991：迁移 0 条，编译成功 65s，前端无改动跳过构建，
                      服务 active / PID 105384 / health 200，登录 OK / task 200，
                      菜单不变量 5 条全绿（menu_total=408）
6eb76991 → e83e1f89：迁移 0 条，编译成功 49s，服务 active / PID 106192 / health 200，
                      登录 OK / task 200，菜单不变量 5 条全绿（menu_total=408）
```

### 3.5 提交与推送

```
6eb7699  feat(linkage): LNK-12 审批实例（FLOW_INSTANCE）接入联动引擎（审批先、单据后；只接提交后动作）
e83e1f8  fix(linkage): LNK-12 补第五个调用点 —— 发起即自动走完的实例（create_instance）
```

均 `push origin main`（145 bare）：`ce413d6..e83e1f8`。

---

## 4. 诚实清单（没做 / 边界）

1. **跨触发点的通知不合并**：同一张单在两处都配通知会收到两条（语义不同的两条事实），
   这是刻意的决定，理由见 1.②。要去重请只配一处。
2. **`CANCELLED` 不接**：级联终止（任务取消 / 单据删除 / 报销作废）走各自业务回调，
   本触发点只覆盖「审批结论」。理由见 1.③。
3. **审批路径的业务桥没有被本卡改动**：桥出错仍然只打日志（既有语义），
   本卡只保证「桥挂了，审批结论的通知仍然发得出去」。
4. **真机取证用的是 `flow_edge_config` 流程**（审批节点 = 角色兜底管理员），
   业务桥对无 `biz_id` 的实例是 no-op —— 这样能干净地隔离出「审批结论 → 联动」这一段；
   带业务单据的路径由各业务卡自己的端到端证据覆盖。
5. **取证实例留在 145 测试环境**（`title = LNK12 联动取证 *`），规则已清场（软删），
   通知与实例保留供人工复核。
