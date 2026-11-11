# COST-B3-9 证据：结账 blocker `COST_UNSETTLED`（ARCH-9 §4.11 / §8.1）

- **卡**：`t_fc70b4e8`（批次 B3，前置 B3-1）
- **范围**：`period.rs` 结账预检加第 5 项 + `period_items` 的 items 口径（纯读校验，无迁移、无新错误码）
- **设计**：`docs/architecture/ARCH-9-成本核算升级设计.md` §4.11 + 新增 §4.11.1「实现口径（B3-9 落地）」

## 验收三条 → 证据位置

| 卡面验收 | 结论 | 证据 |
|---|---|---|
| 有未结算的成本期间 → 结账被拦 | ✅ 真库实测 409，且期间仍是 `OPEN` | `02_blocker_nocapture.out`、`01_period_tests_real_db.out` 里 `cost_period_unsettled_blocks_close_and_names_objects` |
| blocker 文案说清「哪个期间、哪个成本对象」 | ✅ 文案含期间码 + 批次号 + 中文状态 + 成本对象号（最多点 3 个 + 「等」） | 见下方实测原文 |
| 现有 4 项 blocker 不受影响 | ✅ 同一次预检里 `DRAFT_VOUCHER` 与 `COST_UNSETTLED` 同时出现；`period` / `period_items` / `period_remind` / `stocktake` / `cost_period_approval` 全部既有用例 277 条零失败 | `02_blocker_nocapture.out`、`01_period_tests_real_db.out`（`277 passed; 0 failed`） |

实测 blocker 原文（真库，`--nocapture`）：

```text
BLOCKERS(kinds) => ["DRAFT_VOUCHER", "COST_UNSETTLED"]
  - DRAFT_VOUCHER :: 还有 1 张草稿凭证未过账：结账前必须全部过账或作废
  - COST_UNSETTLED :: 成本核算未结算（2040-10 有 1 个批次：批次 CS-204010-0001 状态「草稿（未提交）」，
      2 个成本对象（P2I-CB-204010-001、P2I-CB-204010-002））：请先结算或作废后再结账
```

## 改动文件

| 文件 | 改了什么 |
|---|---|
| `backend/src/business/erp/period.rs` | `BLOCKER_COST_UNSETTLED` 常量；`cost_unsettled_blocker()`（纯函数，文案 + items）；`cost_batch_phrase()`；`collect_blockers` 第 ⑤ 项；模块头 4→5 项 |
| `backend/src/business/erp/period_items.rs` | `DOC_COST_PERIOD` / `ROUTE_COST_PERIOD`；`CostBatch`；`cost_status_settled()`（与 SQL 字面量同源）；`cost_settled_status_list()`；`cost_unsettled_where()`；`cost_period_items()`（计数 + 明细 + 对象号批量取，无 N+1）；`cost_batch_item()`；`cost_batch_next()`；`cost_batch_status_label()` |
| `backend/src/business/erp/period_items_tests.rs` | 2 条真库用例（拦截 + 四态不拦 + 无批次不拦）、1 条纯函数用例、3 个夹具（批次 / 成本对象 / 计算单） |
| `backend/src/business/erp/period.rs`（`mod tests`） | 3 条纯逻辑用例：文案点名期间/批次/对象、无批次不拦 + 截断、四态判定与设计逐值等价 |
| `backend/src/business/erp/period_test_support.rs` | 夹具清场补 `biz_cost_period`（按 2033-/2040- 期次）与 `P2I-` 成本对象（FK 顺序） |
| `frontend/src/api/business/erp/index.ts` | `PeriodCloseBlocker.type` 加 `'COST_UNSETTLED'`；`doc_type` 注释补 `COST_PERIOD` |
| `frontend/src/views/business/erp/shared/blocker-logic.ts` | `BLOCKING_DOC_TYPE_LABELS` 加 `COST_PERIOD: '成本批次'`（与后端 `doc_type_label` 同口径） |
| `frontend/tests/unit/erp-period-close-items.test.ts` | 断言 `blockingDocTypeLabel('COST_PERIOD') === '成本批次'` |
| `docs/architecture/ARCH-9-成本核算升级设计.md` | §4.11.1 实现口径表（判定同源 / 取数 / 文案 / items / 处理人 / 前端 / 无迁移） |

## 实测输出

| 用例 / 门禁 | 结果 | 文件 |
|---|---|---|
| `ERP_DB_URL=costb31_drill cargo test --lib period -- --test-threads=1` | **277 passed / 0 failed**（含 6 条新用例） | `01_period_tests_real_db.out` |
| blocker 原文（`--nocapture`，真库） | 见上 | `02_blocker_nocapture.out` |
| 真库状态取证 / 用例残留 = 0 | 2033-/2040- 批次 0、P2I- 对象 0、P2- 凭证 0、期间 0 | `03_db_state.out` |
| 关键实现位置（行号） | — | `04_source_refs.out` |
| 前端单测 `tsx --test tests/unit/erp-period-close-items.test.ts` | EXIT=0 | `05_frontend_unit.out` |
| 前端 `pnpm typecheck`（`vue-tsc --noEmit`） | EXIT=0 | `06_frontend_typecheck.out` |
| 前端 `vite build`（`--outDir dist-b39check`，5503 modules） | EXIT=0 | `07_frontend_build.out` |
| `cargo clippy --all-targets -- -D warnings` | EXIT=0（零警告） | `08_clippy.out` |
| `cargo fmt -- --check` | 本卡文件零 diff | 见卡内汇报 |

复跑：

```bash
cd backend && ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/costb31_drill \
  cargo test --lib period -- --test-threads=1
```

## 已知偏差 / 口径决定

1. **文案比设计更细**：设计 §4.11 举例只写了「批次 CS-… 状态 DRAFT」，卡面验收要求「说清哪个期间、哪个成本对象」，所以文案落成
   `成本核算未结算（<期间码> 有 N 个批次：批次 <批次号> 状态「<中文态>」，M 个成本对象（<对象号>…））：请先结算或作废后再结账`。
   状态直接用中文标签（不落 `DRAFT` 原码）。
2. **`amount` 一律留空**：`total_*` 是结算事务才回写的列，未结算批次恒 0；显示「0 元」会误导。
3. **处理人留空 = 系统不指定**：批次下一步跨 `cost-period:submit` / `cost-period:settle` 两个权限码，`NextAction` 只装得下一个；
   且催办台账 `biz_reminder_log.doc_type` 的 CHECK（V0257，8 值）还没有 `COST_PERIOD` —— 填权限码会让「催办」通知发出、台账行被库层 CHECK 拒（审计少一行）。
   放宽那条 CHECK（属 reminder 模块，本卡不动）后在此补 `PERM_COST_PERIOD_*` 即可，明细结构不变。**这是需要另开卡的一处小缺口**。
4. **只在期间真有批次时拦**：`biz_cost_period` 没有行 ⇒ 第 5 项连出现都不出现（存量客户 / 未启用成本核算的账不受影响）。
   `SETTLED` / `CLOSED` / `CANCELLED` / `REVERSED` 四态都不算未结算（与设计 `NOT IN (...)` 逐值等价，纯单测 `cost_status_settled_matches_design_list` 钉住）。
5. **无迁移 ⇒ 卡面「迁移幂等 + .down.sql」N/A**（`git status backend/migrations` 零行）；无新错误码。
6. **测试环境注意（不是本卡 bug）**：`mig_replay_b110cost` 这个演练库**没有**已发布的 `cost_settle_approve` 流程（该流程由 `V0381` 种、需要应用启动 seed），
   在那里跑 `cost_period_approval_db_tests` 会失败并残留 2031-07/08/09 三个 DRAFT 批次，**反过来会被本卡第 5 项拦住 2031 期间的结账用例**（16 条红）。
   换成本仓约定的 `costb31_drill`（已发布 `cost_settle_approve`）后 277/277 全绿。已把该库里被我这次跑出来的 3 行残留删掉。
   线上不存在这种组合：真实环境里「期间内有 DRAFT 批次」正是本卡要拦的场景。
