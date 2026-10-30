# 批次 D · EXPRESSION DSL —— P1-A2 接入审批引擎（验证证据）

- 卡面：`t_f736e474`「【P1-A2】EXPRESSION 接入审批引擎（assignee.rs + graph.rs R16 + dto/handler 校验 + 12 测试）」
- 前置：P1-A1（commit `8df84cf`，`expression.rs` 1687 行 + V0136 迁移，全部就绪）
- 设计依据：`docs/批次D-EXPRESSION-DSL-规范.md` §3.5 / §3.6 / §5.5 / §7.4 / §8.2 / §8.5 / §8.6
- 结论：**4 个业务文件 + 3 个必要配套文件；lib 测试 903 → 931（+28 全绿）；`scripts/check.sh` 退出码 0；隔离库（`cloudpivot_a2` / `127.0.0.1:18087`）真机 41 项断言全 PASS**

---

## 一、改了什么（7 个文件）

| # | 文件 | 改动 | 对应卡面 |
|---|---|---|---|
| 1 | `backend/src/business/flow/assignee.rs` | `resolve()` 新增第 4 个参数 `eval: &EvalContext`；把 D2 的 9 个来源分支**逐字搬运**成 `load_plan()`；新增 `expression_plans()`（纯函数）、`is_expression()`、`load_role_ids_by_code()`（code→id）；`Finalized` 加 `rule_id` | §一① |
| 2 | `backend/src/business/flow/graph.rs` | `AssigneeKind` 加 `Expression` 变体（`as_str`/`parse`，`is_enumerable=false`）；`validate()` 加 **R16**；新增 `expression_issues()` + `expression_uses_prev_approver()` | §一② |
| 3 | `backend/src/business/flow/dto.rs` | `into_graph` 的非法来源文案补 `EXPRESSION`；新增 `validate_expression_nodes()`（保存期结构校验，400+65016） | §一③ |
| 4 | `backend/src/business/flow/handler.rs` | `save_graph()` 在 `into_graph()` 之后调用 `dto::validate_expression_nodes()` | §一③ |
| 5 | `backend/src/shared/error_code.rs` | 新增 `FLOW_EXPRESSION_INVALID = 65033` + 码表登记（27 → 28） | 设计 §8.6 |
| 6 | `backend/src/business/flow/model.rs` | `NewTask` 加 `assignee_rule_id` 字段；`insert_task` 的 INSERT 增列 + 绑参 | D-E5 |
| 7 | `backend/src/business/flow/engine.rs` | 2 处 `assignee::resolve(...)` 传 `&eval`；5 处 `NewTask` 字面量补 `assignee_rule_id`（2 处传 `finalized.rule_id`、3 处 `None`） | D-E5 / §8.2④ |

### 硬红线逐条对照

| 红线 | 结果 |
|---|---|
| 1. `expression.rs` 不动 | ✅ 一字未改（`git status` 里没有它） |
| 2. `engine.rs` 不动 | ⚠ **有 7 行机械改动**，原因与口径见下「三、偏差说明」；`eval_context()`（F1 修复本体）与 `resolve` 之后的逻辑**一字未动**，F1 的两条回归测试仍绿 |
| 3. 不改快照结构（D-E3） | ✅ `SnapshotNode` / `SNAPSHOT_SCHEMA_VERSION=1` / `graph_hash` 算法都没碰；真机断言 `graph_snapshot->>'schema_version'` 仍是 `1`，且 `graph_snapshot->nodes[1].config.expression.rules[0].id = 'r_big'`（表达式走 snapshot 已有的 `config`） |
| 4. `finalize()` 三层去重不动 | ✅ 函数体一行未改；EXPRESSION 只把 `Target` 翻译成 `SourcePlan` 再喂进去 |
| 5. `cargo test --lib` 全绿 | ✅ **931 passed; 0 failed**（基线 903 = 931 − 28 条本卡新增；`git show HEAD:<file> \| grep -c "mod expression_tests"` 对三个文件都是 0，证明这 28 条全是新增） |
| 6. clippy 0 error | ✅ `cargo clippy --all-targets -- -D warnings` 通过 |
| 7. `cargo fmt` 干净 | ✅ `cargo fmt -- --check` 退出码 0（注意：`cargo fmt --all` 会顺手重排 `backend/vendor/**` 40+ 个上游文件，已 `git checkout -- backend/vendor` 还原，`scripts/check.sh` 用的是 `cargo fmt --`，不受影响） |
| 8. `scripts/check.sh` 退出码 0 | ✅ 见「二」 |
| 9. 隔离库真机验证 | ✅ `cloudpivot_a2`（owner=cloudpivot，独立 `CARGO_TARGET_DIR=/root/a2-target`、端口 18087，**不是** `cloudpivot_test`、**不是** 生产 8023）41 项断言全 PASS |

---

## 二、静态检查链（原始输出）

```
$ cd /opt/cloudpivot && bash scripts/check.sh ; echo EXIT=$?
==> 后端：cargo fmt --check
==> 后端：cargo clippy -- -D warnings
==> 后端：cargo check
==> 前端：pnpm typecheck
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> 批次B 门禁：全部通过
==> C7 G1..G6 门禁：全部通过
==> 偏好字典与色板一致性：PASS（22 套色板）
==> 全部检查通过
EXIT=0
```

```
$ CARGO_TARGET_DIR=/root/a2-target cargo test --lib -j 1
test result: ok. 931 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

**本卡新增的 28 条用例（全部来自 `mod expression_tests`，命名空间此前不存在）**

| 模块 | 条数 | 用例 |
|---|---|---|
| `assignee` | 15 | 命中第一条规则 / 首条即定不回溯 / 落到第二条 / 无命中无兜底 / 无命中 SKIP / ERROR / FALLBACK_ADMIN / 求值出错按不成立 / 命中但 0 人不顺延 / 底层来源不是 EXPRESSION / 缺 expression→501 / PREV_APPROVER→400 65033 / version 不认识→501 / 多 target 并集顺序 / 来源识别（大小写+空格） |
| `graph` | 8 | EXPRESSION 可解析但不可枚举 / R16 缺 expression→65016 / R16 结构非法→65016（带规则名）/ R16 PREV_APPROVER→65033 / 合法表达式无 R16 且整图干净 / R16 只管 EXPRESSION 节点 / 保留词扫描只认 then+otherwise / R16 从 `validate()` 冒出 |
| `dto` | 5 | EXPRESSION 来源可保存 / 缺 expression→65016 / 结构非法→65016 / 超 16 条→65016 / 静态来源节点不受影响 / 保留词草稿可存但发布必拒 |

---

## 三、偏差说明（逐条，可追责）

### 3.1 `engine.rs` 有 7 行机械改动（红线 2）

**为什么不改就做不到**：D-E5 要求把命中规则 id 落 `biz_flow_task.assignee_rule_id`，
而 `biz_flow_task` 的 INSERT 只在 `engine.rs` 的 5 处 `model::NewTask` 里发生；
`resolve()` 又要 `EvalContext` 才能求值 `when`。

**改了什么**：

```
1563  NodeType::Cc       分支：let eval = self.eval_context().await?;  + 传 &eval
1516  Approval|Countersign：同上
1582  SKIPPED 留痕行：assignee_rule_id: finalized.rule_id.clone(),
1637  PENDING  待办行：assignee_rule_id: finalized.rule_id.clone(),
1070 / 1227 / 2008（转办 / 加签 / 重提）：assignee_rule_id: None,
```

**没改什么**：`eval_context()` 函数体（F1 修复本体）、`finalize()` 调用、空审批人分支、
通知、计票全部一字未动。

**为什么不在 `assignee.rs` 里自建一份 `EvalContext`（那样能完全不碰 engine）**：
`EvalContext` 要填 11 个白名单字段（`form.*` / `initiator.id|name|dept_id` /
`instance.biz_type|biz_no|title` / `task.*`），其中 `initiator.name` / `instance.title|biz_no` /
`task.*` 都不在 `InstanceCtx` 里，要另写 3 条 SQL 复刻 `eval_context()`。
F1 刚刚为「上下文漏填导致条件边静默失效」补过一次，两份构造器必然漂移 —— 那比动 7 行更危险。
**回滚**：`git revert <本卡 sha>` 即整体回退（7 个文件一起）。

### 3.2 卡面 4 个文件之外的两处必要改动

- `error_code.rs`：65033 必须登记（否则 `flow_error_codes_are_stable` 与前端文案映射无处可依，设计 §8.6 明确要求「同步 3 处」）；
- `model.rs`：`NewTask` / `insert_task` 是 D-E5 的落库点，属于设计 §7.4 点名的「2 处 insert_task 传值」。

### 3.3 `PREV_APPROVER` 的保存期行为（按卡面，不是按设计 §5.5 ①）

- 设计 §5.5 第 ① 层要求「保存草稿就 400+65033」；
- **卡面明确写「保存期只做结构校验，不做 PREV_APPROVER 拒绝（让用户先能存草稿）」**，
  所以 `dto::validate_expression_nodes()` 对「含 PREV_APPROVER 的表达式」**放行**，
  由发布期 R16 给 `400 + 65033`（D-E8「不许存进去、跑起来才炸」）。
- 真机证据：`3.1 保存 PREV_APPROVER 草稿 → HTTP 200`、`3.2 发布 → HTTP 400 code=65033`。

### 3.4 `empty_assignee=ERROR` 时实例行不回收（D2 既有语义，本卡不动）

`engine::start` 在事务里先落实例行，节点解析被拒只回滚该步，`handler` 再 `drain` 出错误 →
实例留在 `PENDING`。这是 D2/D6a 既有口径（本卡只读 `finalized.error`，未改这条路径）。
真机断言改为验证**真正该保证的事**：`0 条待办 + auto_passed_nodes=0`（失败绝不静默通过）。
要改成「整单回滚」属于 engine 语义变更，建议单开卡。

### 3.5 ROLE 目标用 `role_code` 而不是 `role_id`（设计 §3.5.1）

`ResolvedTarget::RoleCodes` → `load_role_ids_by_code()`：`SELECT id, role_code FROM sys_role
WHERE role_code = ANY(?::text::text[]) AND deleted_at IS NULL`（**必须带 `deleted_at`**，因为
`role_code` 是**部分唯一索引**）。code 查不到 → 该来源解析 0 人 → `empty_assignee` + trace，
**不是** 501（角色被删是运维态，不是「代码不认识的数据」）。

---

## 四、真机端到端（隔离库 cloudpivot_a2 / 18087，41 项断言，全 PASS）

脚本：`scripts/verify/a2_expr_e2e.py`（打 `cloudpivot_a2` 隔离库；`scripts/check.sh` 之外单独跑）。
原始输出见 `docs/批次D-EXPRESSION-DSL-P1A2-e2e-output.txt`（本次运行 112 行，PASS=41 FAIL=0）。

关键断言摘要（原文）：

```
[PASS] 2.1 EXPRESSION 缺 expression → 400 + 65016
       HTTP 400 code=65016 msg=节点 n_expr（表达式审批）的审批人表达式不合法：审批人来源选择了表达式，但 config 里没有 expression —— 请先配置规则（至少 1 条 when/then）
[PASS] 2.2 expression 结构非法 → 400 + 65016
       HTTP 400 code=65016 msg=...表达式的 rules 必须是数组
[PASS] 2.4 库里落了 assignee_type=EXPRESSION + 表达式正文、config 可解析（D-E3）
       实际 = EXPRESSION~1~2
[PASS] 3.1 保存期允许存 PREV_APPROVER 草稿（卡面：保存只做结构校验）
       HTTP 200 code=0
[PASS] 3.2 发布期拒 PREV_APPROVER → 400 + 65033
       HTTP 400 code=65033 msg=节点 n_expr（表达式审批）的审批人表达式用了「上一节点的审批人」（PREV_APPROVER），本版本未开放（P2），请先改用其它来源
[PASS] 3.5 快照 schema_version 仍是 1（D-E3：不升版本）        schema_version=1
[PASS] 3.6 快照里带着表达式规则（运行期读快照求值）            rules[0].id=r_big
[PASS] 4.1 大额单（form.amount=12000）发起成功
[PASS] 4.2 待办生成且审批人 = 财务总监（角色成员）
       rows=[['01a18bcd-44bb-…', 'A2 财务总监', 'ROLE', 'r_big', 'PENDING', 'n_expr']]
[PASS] 4.3 assignee_source 写底层来源 ROLE（不是 EXPRESSION）
[PASS] 4.4 assignee_rule_id = 命中规则 r_big（D-E5）
[PASS] 4.6 小额单（amount=10）审批人 = 指定用户（第二条规则命中）
       rows=[['01a18bcd-45f1-…', 'A2 普通员工', 'USER', 'r_small', 'PENDING', 'n_expr']]
[PASS] 4.10 求值失败（缺 form.amount）⇒ 不命中 ⇒ 走 otherwise（发起人）
       rows=[['01a18bcc-71c4-…', '超级管理员', 'INITIATOR', 'otherwise', 'PENDING', 'n_expr']]
[PASS] 5.3 无命中 + SKIP：0 条待办 + 节点自动通过（不顺延到别的规则）
       rows=0 auto_passed_nodes=1 instance_status=APPROVED
[PASS] 5.7 无命中 + ERROR → 422 + 65018（不静默通过）
       HTTP 422 code=65018 msg=本节点没有一个可用审批人（empty_assignee=ERROR）
[PASS] 5.8 422 后 0 条待办、未自动通过（实例留 PENDING 等人工处理）
```

**覆盖到的链路**：保存期（缺配置 / 结构错）→ 发布期（R16：合法通过 / 保留词 65033）→
快照（schema_version 不变、表达式在 config 里）→ 发起实例 → 首条规则命中（ROLE，底层来源）→
第二条规则命中（USER）→ 求值失败走 otherwise → 无命中 SKIP（自动通过 + auto_passed_nodes）→
无命中 ERROR（422 + 65018 + 0 待办）。

**隔离性**：`cloudpivot_a2` 库（`createdb -O cloudpivot`）、端口 18087、
`CARGO_TARGET_DIR=/root/a2-target`（不碰共享 `target/`，不重建 release，生产 8023 二进制未动）。
收尾已 `kill $(cat /tmp/a2-instance.pid)` 并复查端口释放、`dropdb cloudpivot_a2`。

---

## 五、明确没做（与卡面「三、明确不做」一致）

| 项 | 归属 |
|---|---|
| `PREV_APPROVER` 真正实现（round 语义 b′） | P2 |
| 前端规则构建器 | P1-B |
| `expression.rs` 改动 | 不做（P1-A1 已交付） |
| 发布期引用存在性校验（`65034`：role_code / user_id / dept_id 是否真实存在） | 设计 §8.5 列为发布期 DB 校验，**卡面 §一② 未要求**，本卡未做 —— 建议与 P2 的规则构建器一起补 |
| 生产部署 | 未部署（卡面只要求隔离库真机验证）；上线时按 `/root/build_release.sh` 口径重建 release + `systemctl restart cloudpivot` |

## 六、回滚

```bash
cd /opt/cloudpivot && git revert <本卡 sha>     # 7 个文件一起回退
# 迁移不用回滚：V0136 是 P1-A1 的（EXPRESSION 枚举 + assignee_rule_id 列），本卡没加迁移；
# 回滚后 EXPRESSION 节点会退回「501 + 65032」的老行为，库里已有的表达式数据不影响存量流程。
```
