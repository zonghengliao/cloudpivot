# M4-13 供应商评估「打分 + 评级（A/B/C/D）」· 证据包

- 卡：`t_252f2eaf`（M4-13【M4·采购】供应商评估：打分 + 评级（A/B/C/D））
- 上游：`t_3e550d38`（M4-12 表结构，提交 `2f3dec42`，V0454）
- 日期：2026-11-18
- 结论：**本卡零迁移、零路由、零权限码、零菜单、零前端改动**；只交数据层 —
  评级阈值 + 加权总分 → 等级、打分落库（同期间重打分覆盖）、按阈值重评级、交期维度客观参考分。

## 一、卡面怎么落的

卡面只有两句：「供应商评估：打分 + 评级（A/B/C/D）」「按总分分级」。落到代码是四件事：

| 卡面 | 落地 | 位置 |
|---|---|---|
| 按总分分级 | 阈值表 `A≥90 / B≥80 / C≥70 / D<70`（**含下界**），`grade_for_total` 唯一出处 | `supplier_evaluation_grading.rs` `GRADE_THRESHOLDS` |
| 打分 | 四维分数 + 权重 → 加权总分（库层生成列）→ 等级；**同供应商同期间一份单，重打分覆盖** | `save_evaluation` |
| 评级要能改 | 阈值改了要能回算历史单：`regrade_evaluations(scope)` 按总分重写等级、返回改动行数、幂等 | `regrade_evaluations` + `grade_case_sql` |
| 打分要有依据 | 交期维度用**真实入库单**算准时率给参考分（其余三维无数据源，人工打分） | `delivery_stat` / `DeliveryStat::suggested_score` |

### 阈值为什么写在代码、不写进 DDL（决定，不是待办）

- 写进 `CHECK` 或生成列 → 调阈值必须发迁移，而阈值是**年年会调的商务政策**（M4-12 头注释 3 已把阈值这一层显式留给本卡）；
- 写进配置表 → 要再造一张「评估配置」表 + 编辑界面，本仓没有任何卡定义它，造了就是死结构；
- 所以：**`GRADE_THRESHOLDS` 是唯一出处**，`save_evaluation` 与 `regrade_evaluations` 都从它派生
  （后者由 `grade_case_sql` 拼 SQL，单测逐字比对生成的 SQL 串），改阈值只改一行 + 跑一次 regrade。

### 三个维度为什么不做自动算分（诚实口径）

只有**交期**有客观数据：`biz_stock_in`（`in_type='PURCHASE'` / `POSTED` / 未删）挂
`source_order_id` → `biz_purchase_order.expect_date`，比 `in_date` 得准时数量占比。

- 质量：本仓没有来料质检表/拒收数量（`biz_stock_in_item` 只有 `qty`），编一个「质量分」公式就是假数据；
- 价格：`biz_price` 是物料价格策略，不是同料跨供应商报价，没有比价基准；
- 服务：没有任何服务事件数据。

因此三维**由评估人打分**；交期给「建议分」（`suggested_score`，评估人可改）。
**没有到货数据时 `suggested_score = None`**，不写成 0 分（0 分是「全部逾期」的意思）。

### 重打分语义

`uk_biz_sev_supplier_period` 决定「一份供应商一个期间只有一个结论」：

- 没有评估 → 取号（`SEV-yyyyMMdd-000001`）+ 写供应商名快照 + 写等级；
- 已有 → **覆盖**四维分数/权重/评估日/评估人/备注/等级，**单号不变**（重打分不是新单据）；
- 并发建同一份：先撞 `unique_violation` 的一方重查后**退化成覆盖**，不把 500 抛给用户。

落库后立即读回对拍两件事：① 库层生成列 `total_score` == Rust `weighted_total`；
② `grade` == `grade_for_total(读回总分)`。任一条不符直接报错（见变异反证 C）。

## 二、验证（全部真跑，原始输出在本目录）

| 验证项 | 结果 | 原始输出 |
|---|---|---|
| 一键演练 `scripts/verify/m413_supplier_evaluation_grading_drill.sh` | **25 条断言全过 / 0 失败，rc=0** | `01-drill.out` |
| 本卡 Rust 用例 | **16 passed / 0 failed**（6 纯逻辑 + 10 真库），SKIP=0 | `04-tests.out` |
| M4-12 表结构回归（不受我改动影响） | **13 passed / 0 failed** | `04-tests.out` |
| 权限审计 `system::menu::permission_audit_tests` | **8 passed / 0 failed** | `04-tests.out` |
| `menu_invariants.sh` 8 条 | 全绿（本卡零菜单） | `01-drill.out` |
| `cargo fmt --check` | rc=0 | `03-fmt.out` |
| `cargo clippy -p cloudpivot --lib -- -D warnings` | rc=0 | `02-clippy.out` |

演练脚本做的事（不只是跑用例）：克隆模板库 → 业务迁移 > V0404 全部应用（50 条，含 V0454）→
跑本卡用例 + M4-12 回归 → psql 侧独立断言（等级值域/权重 CHECK 仍在、夹具 0 残留）→
「不夹带」断言（零迁移、零评估菜单、新文件零路由宏、零权限码）→ 门禁。

### 三处变异反证（证明用例有牙）

| 变异 | 期望 & 实测 | 输出 |
|---|---|---|
| A 阈值 `A` 从 90 改 91 | 3 条纯逻辑用例红（边界/算分/SQL 镜像） | `05-mutation-a-threshold.out` |
| B regrade 的 SQL CASE 偷偷写死 `>= 85 → B` | 95 分被判成 B，regrade 用例红 | `05-mutation-b-regrade-sql.out` |
| C 落库时等级一律写 A | 4 条真库用例红，报「等级漂移：落库 A vs 按总分 89.99 应为 B」 | `05-mutation-c-wrong-grade.out` |

三处变异均已回退（`GRADE_THRESHOLDS` 现为 `90/80/70/0`，`save_evaluation` 无变体）。

## 三、改动文件

```
backend/app/src/business/erp/purchase/supplier_evaluation_grading.rs            （新）阈值/等级/打分落库/regrade/交期参考分 + 6 条纯逻辑单测
backend/app/src/business/erp/purchase/supplier_evaluation_grading_db_tests.rs  （新）10 条真库用例
backend/app/src/business/erp/purchase/mod.rs                                   （+10 行注册）
scripts/verify/m413_supplier_evaluation_grading_drill.sh                       （新）25 条断言的一键演练
docs/evidence/2026-11-18-m413-supplier-evaluation-grading/                     （新）本证据包
```

**没有**新迁移（V0454 属 M4-12）、**没有** dto/handler/route、**没有**权限码与菜单、**没有**前端改动。

## 四、给下游的口径（M4-14 / M4-15 / M4-16 直接用，别再写第二份）

```rust
// 总分 → 等级（阈值唯一出处）
pub fn grade_for_total(total: Decimal) -> DbResult<&'static str>;
// 四维 → (等级, 总分)；先按库层 CHECK 同一份判据校验权重与分数
pub fn grade_and_total(w: &Weights, s: &DimensionScores) -> DbResult<(&'static str, Decimal)>;
// 打分落库：无则取号新建、有则覆盖；写完读回对拍生成列总分与等级
pub async fn save_evaluation(ex: &impl SqlExecutor, input: &EvaluationInput, now: DateTime<Utc>)
    -> DbResult<SaveOutcome>;                       // SaveOutcome { row, created }
// 按当前阈值重写等级（None = 全表；Some((start,end)) = 期间完全落在区间内），返回改动行数
pub async fn regrade_evaluations(ex: &impl SqlExecutor, scope: Option<(NaiveDate, NaiveDate)>)
    -> DbResult<u64>;
// 交期维度客观参考分（只算 POSTED + PURCHASE + 挂采购单 + 有交期）
pub async fn delivery_stat(ex: &impl SqlExecutor, supplier_id: Uuid, start: NaiveDate, end: NaiveDate)
    -> DbResult<DeliveryStat>;                      // DeliveryStat::suggested_score() -> Option<Decimal>
// M4-12 已有的读入口（评估列表/最近一期/期间已评级清单）继续复用
```

- **M4-14（准入/黑名单）**：用 `latest_evaluation(supplier_id)` 取最近一期，读 `grade`；C/D 级是黑名单候选。
- **M4-15（单测）**：本卡已覆盖阈值 8 个边界点、regrade 幂等/scope、交期统计口径、入参五类拒绝；
  仍未覆盖的是「并发打分同一期间」「跨期重打分」「超大方量性能」。
- **M4-16（前端页面）**：本卡**没有接口**（与 M4-12 同口径，权限码属前端卡）。页面要的
  「列表 / 详情 / 打分 / 重评级 / 交期参考分」需要一组 dto + handler + 路由 + 权限码 ——
  建议照 M4-11→M4-11b 的做法**另开一张后端接口卡**（`t_f358da98` 即该先例），不要把权限码塞进前端卡里现造。

## 五、已知边界（不是 bug，是决定）

1. **按时率按数量加权**（不是按单张、不是按金额）：部分到货也能按行判准时；`DeliveryStat` 同时给出
   `docs` / `late_docs` 供页面展示「几张单里几张晚了」。
2. **交期只认挂了采购单的入库**：无源入库（`OTHER`）没有交期可判，不计入分母 —— 否则会把「补录」算成准时。
3. **等级与总分同源校验是硬失败**：如果以后有人改了库层生成列表达式而没改 `weighted_total`，
   打分接口会直接报「加权总分漂移」，而不是写一条总分与等级互相矛盾的记录。
4. **regrade 只改 `grade` 与 `updated_at`**：分数是评估人的事实，等级是政策的函数，回算不许篡改分数。
