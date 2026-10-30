# 批次 D · P1-A1（EXPRESSION DSL 求值器 + 迁移）验证证据

> 卡面：`t_d8c44e4d`【P1-A1】V0125 迁移 + expression.rs 求值器 + 24 条单测
> 设计文档：`docs/批次D-EXPRESSION-DSL-规范.md`（§3 语法 / §6 安全 / §7.3 DDL / §8.4 接口 / §8.8 测试清单）
> 实测环境：172.18.10.144，`/opt/cloudpivot/backend`，PostgreSQL 18，Rust 1.99.1（项目声明 1.94.1）

## 一、交付清单（4 文件）

| 文件 | 状态 | 说明 |
|---|---|---|
| `backend/src/business/flow/expression.rs` | 新增（~700 行含测试） | 解析 + 纯函数求值 + Target→查询计划 |
| `backend/migrations/business/V0136__flow_expression_assignee.up.sql` | 新增 | 放开枚举 + `ck_biz_flow_node_expression` + `biz_flow_task.assignee_rule_id` + 自检 |
| `backend/migrations/business/V0136__flow_expression_assignee.down.sql` | 新增 | 回滚（先断言无节点在用 EXPRESSION） |
| `backend/src/business/flow/mod.rs` | 改 1 行 | `pub mod expression;` |

**未动**：`engine.rs`（红线 1）、`graph.rs` 的 `SnapshotNode` / `SNAPSHOT_SCHEMA_VERSION` / `graph_hash`（红线 2）、
`assignee.rs` / `dto.rs` / `handler.rs`（P1-A2 范围）。

## 二、与卡面 / 设计文档的差异（逐条说明，不是简化）

| # | 卡面 / 文档 | 实际落地 | 理由 |
|---|---|---|---|
| 1 | 迁移编号 `V0125` | **`V0136`** | `V0125__flow_notice_templates` 已占用（迁移号是全仓共享资源），取「当前最大号 V0135 + 1」 |
| 2 | §7.3 全文含 `BEGIN; … COMMIT;` | 去掉 | 迁移器**自己**在事务里执行迁移文件（实测日志 `WARNING: there is already a transaction in progress`），文件内 `COMMIT` 会提前提交迁移器的事务；仓库 63 个 up 迁移只有 1 个带 `BEGIN` |
| 3 | §7.3 自检块 `FOR cfg IN SELECT unnest(ARRAY[ … ])` | 括号按标准嵌套逐行展开 | 照抄 §7.3 实测报错 `ERROR: mismatched parentheses at or near ";"`（少一层括号闭合 `unnest(`）；语义完全不变：3 条反例必须被拦 + 1 条正例必须能入库 |
| 4 | 卡面把 `EvalOutcome` 写成 `Hit | NoMatch | Error(trace)` | `Hit { rule_id, targets } | Otherwise { targets } | NoMatch`，trace 走 `&mut Vec<String>` 旁路 | 与 D-E6 / §8.4 一致：**求值出错 = 不成立 + trace，不是一种结果**。若返回值里留 `Error`，调用方就能把它当「空审批人」静默处理（S4 失效）。`otherwise` 必须能表达（§3.6 第 4 行），所以有 `Otherwise` 变体 |
| 5 | 卡面列的 10 种来源写作 `SUPERVISOR / INITIATOR_DEPT / STATIC` | 取 §3.5 表的真实 10 种：`USER / ROLE / DEPT_LEADER / INITIATOR / INITIATOR_SUPERIOR / TASK_OWNER / TASK_CREATOR / TASK_ASSIGNER / FORM_VAR / ADMIN` | 与 `biz_flow_node.assignee_type` 的 CHECK、`AssigneeKind`、§9.6 前端 TS 类型逐字一致；卡面那几个名字在库和前端都没有对应值 |
| 6 | 卡面 `resolve_targets(then, ResolveContext) -> ResolvedTargets` | `ResolvedTargets { targets: Vec<ResolvedTarget>, notes }`，其中 `ResolvedTarget = Plan(SourcePlan) | RoleCodes(Vec<String>)` | `ROLE` 给的是**角色编码**，`SourcePlan::Roles` 吃的是 id；按 §8.3 方案 B「先查 code→id 再走现有 `Roles(ids)`」，这一步要查库，**不能**放进纯函数。`DEPT_LEADER` 空部门回落发起人部门（`ResolveContext.initiator_dept_id`） |
| 7 | §8.8 的 E-T17~E-T24 分在 `assignee.rs` / `graph.rs` / `dto.rs` | 全部 24 条落在 `expression.rs::tests` | 那三个文件的接线属 P1-A2（卡面「明确不做」表）。其中「与 `finalize()` 的复用」用**纯函数直接调用** `assignee::finalize` 验证（`finalize` 是纯逻辑，不碰库），R16 只做「解析层前置」（`parse_config` / `parse_snapshot_node` 供 P1-A2 直接调用） |

## 三、检查链（红线 4/5/6/7）

基线（改动前，`cargo test --lib -j 1 -- --list`）：

```
878 tests, 0 benchmarks
```

改动后：

```
$ cargo test --lib -j 1
test result: ok. 902 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.56s
```

```
$ cargo clippy --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 41.73s      # 0 error / 0 warning

$ cargo fmt -- --check
FMT_OK                                                                        # 干净
```

```
$ bash scripts/check.sh
CHECK_EXIT=0
  PASS：0 处（分页全部走 scope::paginate）
  PASS：0 处（批次B 闸②）
G5 PASS：23 个 record/record_in 调用点全部在 transaction 闭包内且传 &tx
  PASS：22 套色板与 V0091 的 ui.palette oneof 逐字一致
==> 全部检查通过
```

24 条用例全部在册（`business::flow::expression::tests::*` 命中 24 次）：

```
e_t01_single_rule_hit                               e_t13_targets_keep_json_order
e_t02_first_match_wins_and_second_rule_not_evaluated e_t14_duplicate_rule_id_rejected
e_t03_no_match_with_otherwise                       e_t15_structural_limits
e_t04_no_match_without_otherwise                    e_t16_reserved_and_invalid_targets
e_t05_and_short_circuits                            e_t17_all_ten_target_sources_map_to_plans
e_t06_or_short_circuits                             e_t18_empty_arrays_rejected
e_t07_missing_treated_as_false                      e_t19_when_field_not_in_whitelist
e_t08_type_mismatch_treated_as_false                e_t20_otherwise_is_the_only_fallback
e_t09_in_and_nin                                    e_t21_parse_config_without_expression
e_t10_is_null_and_not_null                          e_t22_parse_config_ignores_other_keys_and_rejects_unknown_version
e_t11_contains                                      e_t23_unknown_fields_rejected
e_t12_not                                           e_t24_output_feeds_finalize
```

## 四、迁移真机验证（隔离库，不碰生产）

### 4.1 全空库跑全量迁移（`createdb -O cloudpivot cloudpivot_a1`）

```
== 1) 建干净库 cloudpivot_a1（owner=cloudpivot）
cloudpivot_a1 / cloudpivot
== 2) 空库上跑全量迁移（起隔离实例 :18086，库 cloudpivot_a1）
INFO nik_web::app: 数据库迁移完成: 应用 93 个迁移
== 3) schema_migrations 记录
kind name version checksum applied_via applied_at applied_seq execution_ms success source
136 | flow_expression_assignee
93 条迁移记录
== 4) 结构断言
CHECK (((assignee_type)::text = ANY ((ARRAY['USER',…,'ADMIN','EXPRESSION']::text[])))
1 条 ck_biz_flow_node_expression
1 列 biz_flow_task.assignee_rule_id
审批人来源：10 种静态来源 + EXPRESSION（动态表达式，规则树存 config.expression，见 EXPRESSION-DSL 规范）
== 5) 反例 ×4 + 正例 ×1（库侧兜底真的拦得住）
NOTICE:  结构用例：4 条反例全部被 CHECK 拦住，1 条正例可入库
0 条 a1_verify 残留（必须 0）
```

反例 4 条：`config={}` / `rules=[]` / `rules` 17 条 / `assignee_type='BOSS'` —— 全部 `check_violation`；
正例 1 条（1 条合法规则）可入库。

### 4.2 down → up 往返

```
== 6) down → up 往返
0 列 assignee_rule_id（down 后必须 0）
0 条 ck_biz_flow_node_expression（down 后必须 0）
CHECK (… 'ADMIN' …)                                        # 枚举回到 10 种，无 EXPRESSION
NOTICE:  V0136 自检通过：枚举放开 + 存在性/上限兜底 + assignee_rule_id + 3 条反例被拦 + 1 条正例可入库
1 列 assignee_rule_id（up 后必须 1）
CHECK (… 'ADMIN','EXPRESSION' …)
== 7) 重启实例：应「应用 0 个迁移」（只校验 checksum）
INFO nik_web::app: 数据库迁移完成: 应用 0 个迁移
== 8) 健康检查
flow_route_no_token=401
```

### 4.3 自检块的「空库路径」（连 `biz_flow_def_version` 都是 0 行）

在事务里清空 `biz_flow_node` / `biz_flow_def_version` / `biz_flow_def` 后单独跑自检 DO 块：

```
清空后 def/version/node =
 def | ver | node
   0 |   0 |    0
NOTICE:  V0136 自检通过：枚举放开 + 存在性/上限兜底 + assignee_rule_id + 3 条反例被拦 + 1 条正例可入库
自检后残留 def（必须 0）= 0
ROLLBACK
--- 回滚后 def=1
```

⇒ 「临时造 `biz_flow_def` + `biz_flow_def_version` 夹具 → 用完删掉」这条路径真的走到了，且零残留。

### 4.4 清理

`cloudpivot_a1` / `cloudpivot_a1e` 是本卡自建的隔离库；被删库已 `dropdb`，`cloudpivot_a1` 保留供复核
（生产 `cloudpivot` 与共享 `cloudpivot_test` **全程未连**）。

## 五、设计要点自检（§6.1 四条硬约束）

| # | 约束 | 证据 |
|---|---|---|
| S1 | 无代码执行面 | 只有 `Condition` / `Target` 两个 enum；`then.type` 走 10 值白名单 + `PREV_APPROVER` 解析期拒绝（E-T16） |
| S2 | 求值纯函数 | `eval_expression(&Expression, &EvalContext)`：全文件非注释行 `await` / `db()` / `FROM ` / `SELECT` 命中 **0**（3 处 `await` 全在文档注释里），`SourcePlan` 只是值搬运，不产生查询 |
| S3 | 结构性上限 | 规则 ≤16（E-T15①）、`then` ≤5（E-T15②）、深度 ≤8（E-T15③）、单条节点 ≤64（E-T15④）、总节点 ≤256（E-T15⑤） |
| S4 | 失败不静默 | 解析失败 → 中文 `Err`（保存期 65033 / 运行期 65032）；求值出错 → trace + 按不成立（E-T07/E-T08）；`NoMatch` 不是自动通过（调用方走 `empty_assignee`） |
