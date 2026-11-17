# M4-12 供应商评估表结构（质量 / 交期 / 价格 / 服务 四维打分 + 权重）· 证据包

- 卡：`t_3e550d38`（M4-12【M4·采购】供应商评估：表结构（质量/交期/价格/服务四维））
- 分支/提交：`main`（提交号见卡内汇报）
- 本卡范围：**只交付表结构**（+ 取号 + 读入口 + 权重算术）。评估打分编排与评级阈值（M4-13）、
  准入/黑名单（M4-14）、单测（M4-15）、前端（M4-16）都不在本卡。
  零路由 / 零权限码 / 零菜单 / 前端 0 改动 / `biz_supplier` 0 新列。

## 1. 交付物

| 文件 | 说明 |
|:--|:--|
| `backend/app/migrations/business/V0454__supplier_evaluation.up.sql` | 1 张表 + 取号行 SEV + 4 索引 + 末尾 DO 自检（列数 / 生成列属性与表达式 / 11 类坏数据反证 / 生成列拒写 / 唯一键 / 探针清理） |
| `backend/app/migrations/business/V0454__supplier_evaluation.down.sql` | 回滚：**有数据就拒绝**，再删表与取号行 |
| `backend/app/src/business/erp/purchase/supplier_evaluation_model.rs` | 表名/四维/等级常量 + 行结构 + `Weights`/`DimensionScores` 判据 + `weighted_total`（与生成列同一公式）+ 取号 `next_evaluation_no` + 5 个读入口 + 期间标签 + 13 条纯逻辑单测 |
| `backend/app/src/business/erp/purchase/supplier_evaluation_db_tests.rs` | 真库用例 12 条（约束行为 + 生成列算术 + 与 model 对拍） |
| `backend/app/src/business/erp/purchase/mod.rs` | 注册上面两个模块（+13 行） |
| `scripts/verify/m412_supplier_evaluation_schema_drill.sh` | 演练：克隆库 → 推到 V0453 → 上 V0454 两遍 → 结构/约束/生成列/范围守卫/空库全链重放/回滚三段/用例/门禁/残留 |

## 2. 表设计（卡面逐条落点 + 理由）

`biz_supplier_evaluation`（25 列，一行 = 一家供应商在一个评估期间的一份四维评估结论）

| 卡面原话 | 落点 | 理由 |
|:--|:--|:--|
| **质量/交期/价格/服务四维** | 四列分数 `quality_score` / `delivery_score` / `price_score` / `service_score`（各 0–100，`ck_biz_sev_scores`）+ 四列权重 `*_weight`（各 0–1） | 一维一对、都在单头。**不建明细表**：卡面给的是「四维」这一层，细分指标（来料合格率/准时交付率）的分子分母与抽样口径属 M4-13 的业务设计，现在建空表就是死结构；真要拆时追加迁移建明细并把四维降级为汇总 |
| **权重** | 四列和**精确 = 1**（`ck_biz_sev_weights`）；默认四等分 0.25 | 权重是「这次评估怎么算总分」的全部依据；留空或 0.9 就是口径不明，且 NULL 权重会让生成列永远算不出值。40/30/20/10 只要四列一起写，不需要配置表 |
| **打分 + 权重** | `total_score` 是 **`GENERATED ALWAYS AS (round(Σ 权重 × 分数, 2)) STORED`** | 加权是确定性算术，写成普通列必然出现「客户端传总分」「改分忘重算」两条漂移路径（与 V0420 在途量 / V0211 盘盈亏同一条理由）。生成列让任何写入路径（handler / 手工 SQL / 导入）显式写它都被 PG 直接拒（428C9 / 「can only be updated to DEFAULT」） |
| 评级 A/B/C/D（卡面属 M4-13） | `grade VARCHAR(2)`，`ck_biz_sev_grade`：NULL 或 A/B/C/D；**NULL = 未评级** | 分数→等级的分界是**政策**不是算术：写进 DDL 就成了「改阈值必须发迁移」。本卡只钉值域，阈值与赋值统一由 M4-13 实现。不造 `UNRATED` 字符串（那就成了第二个「空」） |
| 评估期间（卡面未写但必须有） | `period_start` / `period_end` + `ck_biz_sev_period`（起 ≤ 止）+ `eval_date` + `ck_biz_sev_eval_date`（不早于期间起点） | 评的是「一段时间内的表现」，没有期间就没有可比性。用起止日而不是 year/month：月度/季度/半年度/年度是同一种结构（Q1 = 01-01..03-31），不必为季度加列。评估日早于期间起点只能是录错 |
| 评估人 / 供应商（卡面「供应商」） | `supplier_id` + `supplier_name`（快照）+ `evaluator_id` + `evaluator_name`（快照） | 约定 13：审计/历史类存**姓名快照**（改名/离职后要能回溯「当时是谁」）；展示「现名」时 join `biz_supplier` / `sys_user`，两列并存不是冗余 |
| 一份期间一个结论（卡面未写） | `uk_biz_sev_supplier_period (supplier_id, period_start, period_end) WHERE deleted_at IS NULL` | 期间评估结论是**唯一的对账口径**（准入、黑名单、下单优先级都按它查）。允许同期间多份就必须再定义「取哪一份」，那是没人拍板的口径。要多评估人走**追加迁移**加评估人维度，不先放开唯一键 |
| 状态（**刻意不设**） | 无 `status` 列 | 流转（拟稿→已评级→已发布）没有任何卡定义入口；造了就是永远停在 DRAFT 的死状态（V0450 头注释 6 同一条理由） |

**数据权限**：与采购计划 / 框架协议同口径 **SCOPE-EXEMPT**（公司级采购文件，本表也没有 dept 列），
不登记 `SCOPED_TABLES`；可见性交给 M4-16 的权限码。

**命名**：表 `biz_supplier_evaluation`、约束/索引前缀 `sev`、单号前缀 `SEV`
（`SEV-yyyyMMdd-000001`，`biz_seq.seq_type='SEV'`，`biz_seq` 行 id `…4482`）。
动手前 grep 过：`sev` 前缀、`SEV` 取号键、`biz_supplier_evaluation` 表名全库无占用。

## 3. 三个真实修掉的坑

1. **自检里 `'UNRATED'` 反证先撞列宽**：`grade VARCHAR(2)` 装不下 7 个字符，PG 报
   `22001 value too long` 而不是 `check_violation` —— 只写 `EXCEPTION WHEN check_violation`
   会让迁移**自己炸在自检里**（实测第一次跑就红在这里）。修法：`WHEN check_violation OR
   string_data_right_truncation`，并在注释里写明原因（拒绝是对的，只是拦它的不是 CHECK）。
2. **真库用例被自己被测的约束绊倒**：`grade_domain_and_null_means_unrated` 要给同一供应商插
   5 份评估（A/B/C/D/未评级），全用默认期间 → 第二条起撞 `uk_biz_sev_supplier_period`。
   修法：夹具期间池 `FIXTURE_PERIODS[8]` + `NewEval::at(index)`，一份一个期间 ——
   这恰好是「同期间只能一份」这条约束在测试侧的显形。
3. **生成列的 UPDATE 报错文案与 INSERT 不同**：INSERT 是
   `cannot insert a non-DEFAULT value into column "total_score"`（SQLSTATE 428C9），
   UPDATE 是 `column "total_score" can only be updated to DEFAULT`。演练脚本按固定片段断言，
   一开始只写了 `cannot update` → 假红（拒是拒了，但不是那句话）。

## 4. 验证（真实输出，见同目录 `.out`）

| 项 | 结果 |
|:--|:--|
| 演练脚本（全量，含空库全链重放 374/374） | `PASS=67 FAIL=0`，rc=0（见 `01_drill_full.out`） |
| 真库 + 纯逻辑用例 | `25 passed / 0 failed`（13 纯逻辑 + 12 真库），SKIP=0 |
| 空库全链重放 `mig_replay_fresh.sh` | 374/374 条版本化迁移按版本号升序通过 |
| 回滚三段 | 有数据 → 拒（数据护栏）；清空 → 成功；再 up → 表与 25 列复原 |
| 范围守卫 | 评估权限码 0 / 评估菜单 0 / 源码 0 权限码常量 / 0 路由 / `biz_supplier` 0 新列（见 `04_scope_no_routes_menus_frontend.txt`） |
| 门禁 | `permission_audit_tests` 全绿；`menu_invariants.sh` 8 条全绿（菜单 550 条，本卡零新增）；`cargo fmt --check` + `clippy --lib -D warnings` EXIT=0（见 `05_gates.out`） |
| 生成列算术 | 40/30/20/10 × 80/90/70/60 → `79.00`；改 delivery=100 → 自动 `82.00` |
| 生成列拒写 | INSERT 428C9、UPDATE「can only be updated to DEFAULT」都实测被拒 |
| 与 model 对拍 | 5 组（含 0.5×0.01=0.005 中点 → 0.01）`model::weighted_total` == 生成列，逐位一致 |
| 变异反证（断言有牙） | ① 摘 `ck_biz_sev_weights` → 权重用例红；② 生成列降级为普通列 → 总分用例红；③ `M412_BAD_URL=1` → SKIP 判红（rc=1）。见 `02_teeth_skip_guard.out`、`06_mutation_teeth.out` |
