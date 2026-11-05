# union merge 收口后全量回归（基线 `9867fbe`，144 bare main）

- 卡：`t_e7c3cf7d`（test-enginner）
- 被测基线：`9867fbe5307b9bcc1070bba919029d7694717493`（= 144 bare main，= `t_72c90ae4` union merge 收口点）
- 回归时间：2026-11-05 08:14–10:00（CST）
- 执行机：99（`/opt/cloudpivot` 所在机），**不在共享工作树上跑**

## 0. 隔离做法（可复现）

```bash
git clone --no-checkout /opt/cloudpivot /root/regress-9867fbe
cd /root/regress-9867fbe && git checkout --detach 9867fbe5307b9bcc1070bba919029d7694717493
cp -a /opt/cloudpivot/backend/config/config-{dev,test}.toml backend/config/   # gitignore，缺了 startup_guard 用例必红
ln -s /opt/cloudpivot/frontend/node_modules frontend/node_modules             # 不重装前端依赖
cp -a --reflink=always /opt/cloudpivot/backend/target /root/regress-9867fbe-target   # 私有 CARGO_TARGET_DIR
```

- 隔离树 `git status --short` **0 行**（HEAD = 9867fbe，未带入共享树任何未提交文件）。
- 共享工作树在回归期间被别人推进到 `0f7b27e`（与本卡无关）：**本报告结论只对 `9867fbe` 成立**。
- 对照树 `/root/regress-9ed4bf8`（`git archive 9ed4bf8`）用于判定「红是不是合并引入」。

### 库（真库用例一律连隔离库，生产库 `cloudpivot` 全程零连接）

| 库 | 用途 | 指纹 |
|---|---|---|
| `cloudpivot_regress_20261105` | 主库（SYS/ERP/TASK + 全部 `*_DB_URL`） | 182 迁移 / V0241 / 112 表 / 超管 1 |
| `cloudpivot_test_regress_e7c3` | 模板库（per-target 库由它 `createdb -T` 克隆） | 同上 |
| `cloudpivot_p0fix_regress_e7c3` | `internal_control::db_tests` 专用（库名守卫要求） | 同上 |
| `cloudpivot_{b2,b7,c3,c4,c5,c6,c7,e7,t1,t5,fix04,a4,linkage,pflow1a,pflow1b,pflow5,prefv,testd}_*` 等 19 个 | 集成 target 一 target 一库（库名满足各自 `contains("/cloudpivot_…")` 守卫） | 同上 |
| `cloudpivot_test_tpl_9ed4bf8` | 对照树模板库 | **163 迁移 / V0219**（合并前基线，比合并后少 19 个迁移） |

库指纹原始输出：`db-fingerprint.log.txt`。

## 1. 结论总表

| # | 项 | 结论 | 证据 |
|---|---|---|---|
| 1 | 前端 `vue-tsc --noEmit`（typecheck） | **PASS**（exit 0，0 诊断） | `frontend-typecheck-evidence.txt` |
| 2 | 前端单测（本仓是 `tsx --test`，**没有 vitest**） | **FAIL** 772/773，1 红 | `frontend-unit.log.txt` |
| 3 | 后端 `cargo test --all-targets`（卡面原命令，单库） | **FAIL** lib 2533 passed / 8 failed（7 条环境守卫 + 1 条真红）；因 cargo fail-fast，集成 target 一条没跑 | `backend-all-targets-round1.log.txt` |
| 4 | 后端 lib 分库重跑（除 p0fix 七条，同库第二轮=残留态） | **FAIL** 2489 / 45（**全部是同库残留并发互踩**，单跑全绿，见 §5） | `backend-lib-round2-skip-p0fix.log.txt` |
| 5 | 后端 lib p0fix 七条（独立 `cloudpivot_p0fix_*` 库） | **PASS** 7/0 | `backend-lib-round2-p0fix.log.txt` |
| 6 | 后端 20 个集成 target（一 target 一库） | **PASS 11 / FAIL 9**，204 passed / 24 failed | `per-target-summary.txt`、`result-counts.txt` |
| 7 | `bash scripts/check.sh` | **FAIL（首闸即断）**：`cargo fmt -- --check` 2 文件 3 处 | `check-sh.log.txt` |
| 8 | check.sh 其余闸门（跳过 fmt 后再跑） | clippy/check/typecheck **PASS**；闸① PASS；闸② **FAIL 1 条**；C7 G1–G6 PASS；色板 PASS；库存写入口闸 22/22 + 零越界 PASS；闸③ **SKIP**（本机无生产 env） | `check-sh-skip-fmt.log.txt`、`check-sh-warn-batchb-rest.log.txt` |
| 9 | 巡检 SQL / 静态一致性（合并时已验，本卡复验） | **PASS** `snapshot_audit.py --dry-run` 合计 26 项；`chains_e2e.py` py_compile OK；`invariant_registry` 1 passed | `integ-invariant_registry.log.txt` |

**一句话**：`9867fbe` **不能当作"全绿"放行**——合并引入了 2 处确定性红（后端 lib 1 条、前端单测 1 条），另有 1 条 check.sh 闸门红（合并前就红）与一批"合并前 144 main 上本来就红"的用例；其余 45 条后端红是**同库残留并发互踩**，不是产品缺陷。

## 2. 合并引入的真红（必须修，已开卡）

### 2.1 后端 lib：`doc_kind_registries_match_doc_kind_table`（确定性）

```
cargo test --lib -- --exact business::shared::doc_kind::doc_kind_tests::doc_kind_registries_match_doc_kind_table
```

首个失败断言（`backend/src/business/shared/doc_kind_tests.rs:813`）：

```
ATTACHMENT_DOC 登记行数不符（SQL 5 行 / Rust 6 项）；left: 5  right: 6
```

**根因**：`docs/architecture/snapshot-drift/10_checks.sql` 的登记表缺 4 行，而 Rust 词表已含这些取值：

| 域 | 缺的行 |
|---|---|
| `ATTACHMENT_DOC` | `('ATTACHMENT_DOC', 'FIXED_ASSET', 'biz_fixed_asset')` |
| `STATUS_ENTITY` | `('STATUS_ENTITY', 'BUDGET', 'biz_budget')` |
| `STATUS_ENTITY` | `('STATUS_ENTITY', 'FIXED_ASSET', 'biz_fixed_asset')` |
| `STATUS_ENTITY` | `('STATUS_ENTITY', 'DEPRECIATION', 'biz_depreciation_batch')` |

**归因**（`registry-diff-by-rev.txt`）：

- `9ed4bf8`（合并前 144 bare main）：4 个域全 **PASS**（ATTACHMENT_DOC 5/5、STATUS_ENTITY 14/14）
- `4f8230a`（合并前 99 本地 main，无该测试文件）：10_checks.sql 里 ATTACHMENT_DOC/REMINDER_DOC **一行都没有**，STATUS_ENTITY 14 行也缺 BUDGET/FIXED_ASSET/DEPRECIATION ⇒ 本地 FIN-1/FIN-2 加枚举时没同步登记表
- `9867fbe`（合并后）：**FAIL** ⇒ 冲突裁定"10_checks.sql 取远端全量"保住了远端的登记表，但本地 FIN 词表扩了 3 个取值就没登记；远端那套"词表↔登记表"用例（本地没有）合并进来后立刻把差异打成红

> 也就是说：**不是冲突解错，而是 union merge 把"本地扩词表"和"远端登记表用例"首次拼到了一起**，暴露出本地 FIN-1/FIN-2 漏登记。这正是全量回归存在的意义。

### 2.2 前端单测：`flow-biz-link.test.ts` 硬编码 13

```
cd frontend && npx tsx --test tests/unit/flow-biz-link.test.ts
```

首个失败断言（`frontend/tests/unit/flow-biz-link.test.ts:59`）：

```
not ok 351 - 后端 FlowBizType 的落库字面量 = 前端 FLOW_BIZ_TYPES（逐字、同序，13 个）
解析到 15 个字面量：PO_APPROVE,…,BUDGET_APPROVE,ASSET_DISPOSAL
15 !== 13   （expected: 13, actual: 15）
```

**根因**：用例把条数写死 13；`FlowBizType` 侧与前端 `FLOW_BIZ_TYPES` 都已是 15（两侧一致，`deepEqual` 那条本身会过），只有"13"这个常量过期。用例来自远端血缘（`4f8230a` 上该文件不存在），远端当时是 13/13/13；本地 FIN-1/FIN-2 两侧都加了 2 个取值 ⇒ 合并后 15/15/13。

**修法**：`assert.equal(rust.length, 15)` + 标题 `13 个 → 15 个`（属测试侧过期常量，实现无需改）。

## 3. check.sh 闸门（合并前就红，但阻塞整链）

- **闸 0 `cargo fmt -- --check` FAIL**：`backend/src/bootstrap.rs:112`、`backend/src/shared/startup_guard.rs:969`、`:1059` 三处换行不符合 rustfmt 默认宽度。
  对照：`9ed4bf8` 上 `cargo fmt -- --check` **同样 exit 1**（3 个文件：bootstrap.rs / doc_kind_tests.rs / startup_guard.rs）⇒ **合并前就红**，非本次合并引入（`parent-9ed4bf8-cargo-fmt.log.txt`）。
- **批次B 闸② FAIL 1 条**：`scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST` 未登记 4 个新测试文件（`asset_model_tests.rs` / `budget_dto_tests.rs` / `budget_guard_tests.rs` / `budget_model_tests.rs`，均来自本地 FIN-1/FIN-2）。对照：`4f8230a` 上这 4 个文件已在、`SKIP_MANIFEST` 同样未登记 ⇒ **合并前就红**。
- 其余全绿：`cargo clippy --all-targets -D warnings`（1m22s）、`cargo check`、`pnpm typecheck`、闸① 0 处、C7 G1–G6、色板 22 套、库存写入口闸 `--self-test` 22/22 + 主闸"零越界"。
- **闸③（ERP 不变量）SKIP**：99 上没有 `/etc/cloudpivot/cloudpivot.env`、也无 `NIK_DATABASE_PASSWORD`/`PGPASSWORD`，脚本按设计判为"无 DB 环境"跳过。**这条没被验证**，不要当成绿。

> 复跑命令（本卡用的副本只是把 fmt 段落摘掉，原 `check.sh` 未改）：
> `bash scripts/check.sh`（会停在 fmt）；`BATCHB_GATE=warn bash <去掉 fmt 的副本>`（拿到其余闸结论）。

## 4. 后端集成 target 逐条（一 target 一库）

原始：`per-target-summary.txt`，每个 target 的完整输出 `integ-<target>.log.txt`。

| target | 结果 | 说明 |
|---|---|---|
| `testd` | **PASS 67/0** | 库名守卫生效（`cloudpivot_test_testd_e7c3`） |
| `linkage` | FAIL 54/8 | 见 §6 既有红 |
| `pflow5` | FAIL 13/2 | 单跑绿 ⇒ target 内互踩（§5） |
| `pflow1b` | FAIL 12/2 | 与对照树部分重叠 ⇒ 既有红 + 偶发 |
| `pflow1a` | FAIL 11/1 | 对照树同红 ⇒ 既有红 |
| `fix04_notice_offline` | FAIL 3/6 | 对照树同红（3 条）⇒ 夹具缺陷，合并后更严重 |
| `impexp_worker` | FAIL 5/2 | 对照树同红 1 条 ⇒ 既有红（+1 条偶发） |
| `batchC_c4_help` | FAIL 10/1 | 对照树同红 ⇒ 既有红（确定性） |
| `batchC_c5_changelog` | FAIL 7/1 | 单跑绿 ⇒ 偶发 |
| `batchB_b2_scope` | FAIL 0/1 | 对照树同红 ⇒ 既有红 |
| `batchB_b7_mutex` / `c3` / `c6` / `c7` / `e7` / `t1_task` / `t5_ai_rules` / `pref_preference` / `invariant_registry` / `s3_minio` | **PASS** | `c7_rollback` 0 条（本 target 无用例，按设计） |

## 5. 45 条后端红 = 同库残留并发互踩（不是产品缺陷）

第 2 轮 lib 在**第 1 轮残留的同一个库**上跑：2489 passed / 45 failed。分布：

| 模块 | 条数 |
|---|---|
| `business::erp::stockin_handler::*`（a4 / cancel / d4 / d9 / dc03 / location / location_dict） | 42 |
| `business::erp::serial_handler::tests::t1_missing_unit_cost_rejects_with_zero_writes` | 1 |
| `business::erp::cash_flow::tests::r3_real_config_covers_all_accounts` | 1 |
| `business::shared::doc_kind::doc_kind_registries_match_doc_kind_table` | 1（§2.1 的确定性红） |

代表性失败断言：

- `a4_08_over_receipt_is_400`：`采购明细行不存在（01a1a936-…）`（并发方把采购行清掉/未提交可见）
- `serial_handler t1`：`400 必须零落库` left `[0,0,1,0,70,196]` right `[0,0,1,0,69,196]`（整库凭证行数被别的用例推高）
- `cash_flow r3_real_config_covers_all_accounts`：整表 `biz_account` 断言（本仓已知的"全库口径 vs 并发夹具"）

**判定依据**（`same-db-single-run-probe.txt`，同一个脏库上单跑）：

```
a4_submodule   threads=4 exit=0  13 passed / 0 failed
serial_t1      threads=1 exit=0   1 passed / 0 failed
cashflow_r3    threads=1 exit=0   1 passed / 0 failed
```

单跑全绿 + 全量红 + 两轮红集合不同 ⇒ 按 `cloudpivot-backend-tests` 技能 §11/§14 的口径：**夹具互踩，不是回归**。要拿到"同库连跑两轮 0 failed"，得先收口 stockin 系列的夹具（已开卡）。

## 6. 合并前 144 main 上本来就红的（对照树证据）

对照树 `/root/regress-9ed4bf8`（163 迁移 / V0219）+ 同样的 per-target 库跑同一批用例：

| target | 9ed4bf8（合并前） | 9867fbe（合并后） | 判定 |
|---|---|---|---|
| `linkage` | 54/8 —— **失败集合与合并后完全一致**（`s06…s12`、`s15` 作废链） | 54/8，同一集合 | **既有红**：用例期望"直接 CANCELLED"，实现回 `PENDING_CANCEL`（作废走审批） |
| `pflow1a` | 11/1（`publish_regression_keeps_graph_hash`） | 11/1，同一条 | 既有红 |
| `pflow1b` | 12/2（`concurrent_complete_only_one_advances`、`task_accept_approval_advances_node`） | 12/2（`approve_mode_reject_follows_on_reject`、`task_accept_approval_advances_node`） | 既有红 + 偶发（`task_accept…` 两边都红） |
| `batchC_c4_help` | 10/1（`h03_slug_unique_and_format`：期望 409，实得 `Db(InvalidValue("帮助文档：文档 slug 已存在"))`） | 10/1，同一条，**单跑仍红** | 既有红（确定性：唯一键冲突没映射成 409） |
| `impexp_worker` | 6/1（`worker_marks_failed_when_definition_missing`：`failed=0 canceled=1`） | 5/2（同一条 + `zombie_reclaim_…`） | 既有红 |
| `fix04_notice_offline` | 6/3 | 3/6 | 既有夹具缺陷：`ann_no` 取 UUID 前 12 位，同一毫秒内多用例撞唯一键 |
| `batchB_b2_scope` | 0/1（`超管看到全部用户 left 36 right 12`） | 0/1 | 既有红（整库型断言，夹具库不干净时必红） |
| `pflow5` | **12/0 全绿** | 13/2 | **合并后新红，但单跑均绿** ⇒ target 内互踩（本地新增 3 条用例后同库并发概率上升），不是产品缺陷 |

## 7. 本轮没有覆盖到的

1. `scripts/check.sh` 的**闸③ ERP 不变量**：99 无生产 env ⇒ SKIP（需在有 `/etc/cloudpivot/cloudpivot.env` 的机器上跑）。
2. 前端 **e2e（Playwright）**：不在本卡要求内（卡面写的是 typecheck + 单测），未跑。
3. 卡面写的"`npx vitest run`"在本仓不存在——`frontend/package.json` 只有 `test:unit` = `tsx --test "tests/unit/**/*.test.ts"`，本卡按真实脚本执行。
4. 回归只证明 `9867fbe`；共享工作树在回归期间已被推进到 `c9b78c9`（见 §9），那之后的提交没有纳入本卡回归。
5. 真库用例全部跑在**隔离库**（182 迁移 / V0241 / 112 表 / 超管 1）；生产库 `cloudpivot` 未连接。

## 9. 基线之后 main 已前进（本报告结论的时效边界）

回归期间（08:14–10:00）共享树被别的卡从 `9867fbe` 推进到 `c9b78c9`（9 个提交）。其中 `9f0bc75`「NIGHT-REGRESSION 夜间全量回归」**已经修掉了本卡 §2 的两条合并红**，因此逐项复核如下：

| 项 | `9867fbe`（本卡基线） | `c9b78c9`（当前 main，已复验） |
|---|---|---|
| 10_checks.sql 登记表 4 行 | 缺（§2.1 红） | **已补齐**：`registry-diff-by-rev.txt` 口径复跑 ATTACHMENT_DOC 6/6、REMINDER_DOC 4/4、STATUS_ENTITY 17/17、SERIAL_SOURCE 4/4 全 PASS |
| `flow-biz-link` 硬编码条数 | 13（§2.2 红） | **已改 15**；用 `c9b78c9` 的已提交前端文件重跑 → `# tests 11 / pass 11 / fail 0` |
| `cargo fmt` 2 文件 3 处 | 红 | **仍红**（`bootstrap.rs`、`shared/startup_guard.rs` 静态复验 FMT-DIFF） |
| 批次B 闸② `SKIP_MANIFEST` 4 行 | 缺 | **仍缺**（`scope_gate.py` 里 4 个 FIN 测试文件仍 0 命中） |

→ 对应的两张 P0 卡（`t_cdfcd7cf` 后端登记表 / `t_069bd97c` 前端 13→15）在 HEAD 上已无施工必要，**建议按"已修复"关闭**；`t_56e57909`（check.sh 首闸红）在 HEAD 上仍然成立。

本报告 §1–§8 的所有数字**只对 `9867fbe`** 成立；`c9b78c9` 之后的提交（含 NIGHT-REGRESSION 自己声称的"后端 2769 passed/0 failed、前端 773"）没有纳入本卡回归范围，需要单独复核口径。

## 8. 文件清单


| 文件 | 内容 |
|---|---|
| `result-counts.txt` | 逐轮/逐 target 的 passed/failed 计数（脚本从原始日志解析） |
| `backend-all-targets-round1.log.txt` | 第 1 轮 `cargo test --all-targets` 原始输出 |
| `backend-lib-round2-skip-p0fix.log.txt` / `backend-lib-round2-p0fix.log.txt` | 第 2 轮 lib（分库）原始输出 |
| `backend-integration-all-in-one-db.log.txt` | 单库跑全部集成 target 的原始输出（含各 target 库名守卫 panic） |
| `per-target-summary.txt` + `integ-<target>.log.txt` | 一 target 一库的逐条结果 |
| `parent-9ed4bf8-summary.txt` + `parent-<target>.log.txt` + `parent-9ed4bf8-cargo-fmt.log.txt` | 合并前对照树结果（归因用） |
| `registry-diff-by-rev.txt` | 10_checks.sql 登记表 ↔ Rust 词表逐 rev 比对 |
| `single-test-probe.txt` / `same-db-single-run-probe.txt` | 单跑复核（区分确定性红与互踩） |
| `frontend-unit.log.txt` / `frontend-typecheck-evidence.txt` | 前端单测（773 条）与 typecheck |
| `check-sh.log.txt` / `check-sh-skip-fmt.log.txt` / `check-sh-warn-batchb-rest.log.txt` | 仓库闸门原始输出 |
| `db-fingerprint.log.txt` | 隔离库指纹（迁移条数/版本/表数/超管数） |
| `build.log.txt` / `build.status.txt` | 隔离树冷编译（343s，exit 0） |
