# COST-B3-2 证据：成本结算审批流种子（V0381）

- **卡**：`t_bef918b4`（COST-B3-2【成本】审批流种子）· 2026-11-11
- **设计依据**：`docs/architecture/ARCH-9-成本核算升级设计.md`
  §4.2「审批流怎么接」（biz_type / flow_code / 两级节点 / 驳回打回 DRAFT / 自审批 / 免审 FALSE）、
  §7.1 的 `cost_flow_def` 行（四重守卫）、§8.1 的 B3-2。
- **范本**：`V0332__purchase_return.up.sql` §⑦（流程种子 + 四层守卫 + 自检）。

## 一句结论

`cost_settle_approve`（`biz_type=COST_SETTLE`，财务经理 → 财务总监，**两级**、**或签**、**绑角色**）
作为 PUBLISHED 默认流程种进库；四重守卫保证「重复执行不重复插 / 客户自建不被覆盖 / 版本号递增」；
`graph_hash` 与快照、节点行三方对账（迁移内自检 + 静态单测 + 真库用例各一道）。

## 一、交付文件

| 文件 | 说明 |
|---|---|
| `backend/migrations/business/V0381__cost_flow_def.up.sql` | 种子：def + version + 4 节点 + 3 边 + 2 审批人 + 7 段自检 |
| `backend/migrations/business/V0381__cost_flow_def.down.sql` | 回滚：3 道安全阀 + 自底向上删 |
| `backend/tests/cost_b3_2_flow_seed.rs` | 静态不变量 12 条（纯文件解析，不连库） |
| `backend/tests/cost_b3_2_flow_seed_db.rs` | 真库用例 3 条（`ERP_DB_URL` 开关，读形态 + 引擎校验 + hash 对账） |
| `docs/evidence/2026-11-11-cost-b3-2-flow-seed-v0381/01_replay_fresh.sh/.out` | 空库全链重放 301 条 + 12 条断言 |
| `docs/evidence/2026-11-11-cost-b3-2-flow-seed-v0381/02_drill.sh/.out` | 八段真库演练，57 条断言 |
| `docs/evidence/2026-11-11-cost-b3-2-flow-seed-v0381/03_db_tests.*` | 真库用例原始输出 |
| `docs/evidence/2026-11-11-cost-b3-2-flow-seed-v0381/04_gates.*` | fmt / clippy / 全量 cargo test / 隔离工作树说明 |

## 二、号段：卡面写 V0378，实际 V0381

前置卡 B1-7 已占 **V0378/V0379/V0380**（成本菜单权限 / 号段字典 / 批号回填，整体顺延 3 位）。
nik 迁移加载器对版本号做跨目录全局唯一校验，重号 = 服务启动即失败（AGENTS 约定 6），故本卡取 **V0381**。
动手前现核：`ls backend/migrations/business | sed 's/_.*//' | sort -u | tail` → `V0380`。

## 三、流程形状（最终落库值）

```
开始 → 财务经理审批（FINANCE_MANAGER …f0005）→ 财务总监审批（FINANCE_DIRECTOR …f0006）→ 结束
```

| 项 | 值 | 依据 |
|---|---|---|
| `flow_code` / `biz_type` | `cost_settle_approve` / `COST_SETTLE` | 设计 §4.2 |
| `status` / `is_default` / `allow_auto_approve` | `PUBLISHED` / `TRUE` / `FALSE` | 设计 §4.2（免审不允许） |
| 节点 | 4 节点 / 3 边，`node_count=4` `edge_count=3` | 两级顺序审批 |
| 审批节点 `approval_mode` | **`ANY`（显式落库，不靠 NULL 兜底）** | 「不是全员会签」；V0359/V0367 的 P0 是 `ALL` |
| 审批人 | `assignee_type='ROLE'` + `biz_flow_node_assignee` 绑 f0005/f0006 | 卡面「绑角色，不是全员会签」 |
| 空审批人兜底 | `FALLBACK_ADMIN` | 角色成员全停用时不留死单 |
| 超时 | 24h `REMIND`（**不自动通过**） | 结算不许被超时静默放行 |
| `graph_hash` | `8a4fbd64cf5fb5cfc3b133ef2e4ff6103cd14a9afbabfae2b6f63f5845eb4171` | sha256(canonical 快照) |

## 四、四重守卫（与 V0332 同款，跳过只 NOTICE 不抛错）

| # | 判据 | 行为 |
|---|---|---|
| ① | 已有 `flow_code='cost_settle_approve'` 的活流程 | 整条跳过，保留客户配置 |
| ② | 已有 `biz_type='COST_SETTLE' AND is_default` 的活流程 | 整条跳过（`uk_biz_flow_def_default` 唯一索引） |
| ③ | `FINANCE_MANAGER` / `FINANCE_DIRECTOR` 两个角色不齐 | 整条跳过（种出来也派不出去） |
| ④ | 同 def 已有本卡版本（`change_note LIKE 'V0381 种子%'` 或同 `graph_hash`） | 不重复插；需要补插时版本号取 `COALESCE(MAX(version_no),0)+1`、`is_current` 只在「还没有当前版」时置真 |

## 五、实测输出（全部真跑，见同目录 .out）

### 1) 空库全链重放（`01_replay_fresh.sh`）

```
重放迁移数：301 条（B1-7 时 300，+1）
✓ 流程定义 1 条且 PUBLISHED / is_default / 不免审（1）
✓ def.current_version_id 指向本卡版本（…b0382）
✓ 版本 1 条：PUBLISHED / is_current / 4 节点 / 3 边（1）
✓ 节点 4 / 边 3 / 审批人绑定 2（角色型）
✓ 审批节点全是或签 ANY（全员会签 ALL 为 0 条）（2/0）
✓ 节点顺序：开始→财务经理审批→财务总监审批→结束
✓ graph_hash = sha256(canonical(快照))（8a4fbd64…4171）
✓ COST_SETTLE 默认流程全库唯一（1）
=== 结果：0 条断言失败 ===
```

> 迁移内 §6.4 用**库内** `pg_temp.canonical_json` + `sha256()` 重算快照并对账 `graph_hash`，
> 所以「离线算的 hash 与 PG canonical 化不一致」这类错在这一步就会整份回滚，不会带到线上。

### 2) 八段真库演练（`02_drill.sh`，57 条断言全绿）

| 段 | 内容 | 关键断言 |
|---|---|---|
| S0 | 基线（< V0381） | 流程表就绪、两角色在、`cost_settle_approve` 不存在 |
| S1 | 首次 up | 1 def / 1 version / 4 节点 / 3 边 / 2 审批人 + 自检通过 |
| S2 | up 幂等 | **指纹 `22f4588e4b614563b21f637484f97c3b` 逐字节相等**、版本数仍 1 |
| S3 | down + down 幂等 | 五张表本流程行全 0；第二遍零异常 |
| S4 | 回滚后重放 | 指纹回到 S1（逐字节一致） |
| S5 | 守卫①（客户自建同名 flow_code） | 只 NOTICE 跳过；客户行 `客户自建结算审批/DRAFT` 未被覆盖；本卡 0 行 |
| S6 | 守卫②（客户已有 COST_SETTLE 默认流） | 只 NOTICE 跳过；默认流仍只有客户那一条 |
| S7 | 守卫③（软删 FINANCE_DIRECTOR） | `审批角色只找到 1/2` → 不种；恢复后 up 成功且 v1 就位 |
| S8 | 守卫④（def 已有客户 v1） | 发 **v2**（`MAX+1`，不撞 `uk_biz_flow_def_version`）、`is_current` 仍只有 1 条、客户 v1 的 hash 一个字没动、def 指针不被抢 |
| S9 | down 安全阀 b | `biz_cost_period` 挂着 `approval_instance_id` → 拒绝回滚，**库零变化**；清掉后 down 成功 |
| S10 | down 安全阀 c | 本流程下还有客户 v2 → 拒绝回滚（点名 1 个非本卡版本） |
| S11 | 收尾 | 残留 0、演练临时行 0、角色软删已复原 |

### 3) 真库用例（`03_db_tests.*`）

```
running 3 tests
test definition_is_published_and_discoverable ... ok         # 按 B3-3 取流程的 SQL 能选中
test graph_passes_engine_validation_and_hash_matches_rows ... ok  # FlowGraph::validate() 0 问题
test nodes_are_two_levels_bound_to_roles ... ok              # 两级 / ANY / 绑角色 / 链路
test result: ok. 3 passed; 0 failed
```

`graph_passes_engine_validation_and_hash_matches_rows` 是这层最有价值的一条：
它走**引擎真实读图路径**（`flow::model::load_graph` → `validate()`），把「种子能加载但走不动」
（如 R28 全员会签无兜底 `65048`）挡在提交前；然后 `graph.graph_hash()` == 库里的 `graph_hash`
== 迁移里的常量 —— 这就是 `flow::maintain` 每日自检的那把尺，提前在测试里跑一遍。

### 4) 门禁（`04_gates.*`）

见同目录 `04_gates.out`：`rustfmt --check` 0 差异、`cargo clippy（两个新目标）-- -D warnings` 干净、
隔离工作树全量 `cargo test` 结果。

## 六、已知偏差与决策（都在卡内说明过）

1. **`self_approval = 'ALLOW'`（不是 FORBID）**。设计 §4.2 写自审批由
   `erp::separation::guard_self_approval` 在审批动作上拦（桥侧），节点侧沿用 V0332/V0191 的
   `ALLOW`：`FORBID` 是**派发阶段整单失败**（`flow/assignee.rs` filter），财务经理本人制单的
   月结批次会直接派发不出来，既没有审批人兜底、界面也没有按钮 —— 那是 AGENTS 约定 11 的死单形态。
   `ALLOW` 还让 `self_approval::over_limit`（金额超阈值自动剔除发起人）这条内控继续生效。
2. **`approval_mode` 显式写 `ANY`**，不靠「NULL = 或签」的引擎兜底。理由是这一列是
   V0359/V0367 生产事故的现场：NULL 语义在 12 个流程里被误读成全员会签过一次，种子里落死值更安全。
3. **守卫④的「版本递增」语义**：本卡版本 id 固定，所以只有「id 空着」时才会插新版本，
   版本号取 `MAX+1`；`is_current` 只在 def 还没有当前版本时置真。**绝不去抢客户的当前版本**
   （S8 演练验证：客户的 v1 仍是 current，本卡 v2 落在非 current）。
4. **自检只在「当前版本仍是本卡种的那一版」时做形态校验**。客户在流程编辑器里改过图、
   发过新版之后，本卡的行已不是出厂形态 —— 这时**只 NOTICE 不报错**（覆盖或阻断客户配置
   比种子没生效更严重）。down 侧反过来：客户发过新版就**拒绝回滚**（安全阀 c）。
5. **`V0381` 而不是卡面的 `V0378`**（号段被前置卡 B1-7 占用，见 §二）。
6. 迁移里 **不种菜单、不种权限码、不建状态机边表** —— 那些是 B3-1（结算四件套 / 状态机边表）
   与 B1-7（菜单权限）的范围；本卡只管流程定义本身。

## 七、交接给下游卡（B3-3 接线时不要漏）

1. **提交审批**：按 `biz_type='COST_SETTLE'` + `is_default` 取流程（`definition_is_published_and_discoverable`
   用例里的 SQL 就是将来 `cost_settle_bridge` 的形状），把 `biz_cost_period.approval_instance_id` 写回。
2. **驳回 / 撤回**：打回 `DRAFT` 并把审批意见追加到 `biz_cost_period.remark`（设计 §4.2）。
   注意 `ck_biz_cost_period_appr`：只要状态是 `SUBMITTED` 及以上，`approval_instance_id` 就必须非空 ——
   打回 DRAFT 时不要顺手把它清空（留着才能回溯「谁审过」）。
3. **自审批**：审批动作入口调 `erp::separation::guard_self_approval(task_id, operator)`
   （与应付作废 / 凭证红冲同一道闸），本卡的节点配置刻意留 `ALLOW` 给它兜。
4. **审批通过 → 结算**：`APPROVED → SETTLED` 的跃迁必须走 `state_machine::transition`，
   且 `SETTLED` 的库层 CHECK 要求 `settled_at` 与 `finish_voucher_id` 同时非空（本卡不碰）。
5. **down 前提**：任何 `COST_SETTLE` 实例存在、或结算批次已挂实例、或客户发过新版时，
   `V0381` down 会拒绝执行并点名原因 —— 回滚前要人工处置（撤回/作废实例）。
