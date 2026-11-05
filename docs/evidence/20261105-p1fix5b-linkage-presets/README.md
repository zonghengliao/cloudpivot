# P1-FIX-5B · 跨单据联动：3 条预置规则 + 端到端验证（卡 t_53c23814）

## 一句话

`biz_flow_linkage_rule` 从**0 行**变成 **3 条真实业务规则**（迁移 V0254），并且把
**`STOCK_IN/POSTED` 触发点真正接进了通用引擎** —— 在这之前，收货过账只走了「凭证」那条专用路由
（`linkage::engine::stockin_voucher`），通用引擎的 `run` / `run_deferred` 在收货链路上**根本没被调用过**
（`git show HEAD:backend/src/business/erp/stockin_handler_post.rs | grep linkage` 只有 ⑦ 步的
`stockin_voucher`，没有 `run`），所以「配一条收货过账的通知规则」在改动前是**配了也不会触发**。
本卡补齐第 ⑧ 步 + 提交后执行，并在隔离库上真机跑通 8 轮 e2e（每轮 53 条断言全 PASS）。

## 交付物（改动文件 / 函数）

| # | 文件 | 内容 |
|---|------|------|
| 1 | `backend/migrations/business/V0254__flow_linkage_presets.up.sql`（新增） | 种 3 条预置规则 + 迁移期自检（3 条在、全部 `enabled`、全部 `mode=TAKEOVER`）。`ON CONFLICT (trigger_entity, trigger_status, action_code) DO UPDATE` 幂等 |
| 2 | `backend/migrations/business/V0254__flow_linkage_presets.down.sql`（新增） | 物理删同一批键（只删本迁移种的 3 个键，不碰运维后加的规则）+ 自检「删干净了」 |
| 3 | `backend/src/business/erp/stockin_handler_post.rs`（+103 行） | **真正的断链修复**：`post_in_tx` 末尾新增第 ⑧ 步 `linkage::engine::run_excluding(ex, ctx, &[ACTION_GENERATE_VOUCHER])`，返回值改为 `linkage::LinkageRun`；`post_stock_in` / `post_after_approval` 在 `tx.commit()` **之后**调 `run_stockin_deferred_linkage`（跑 `AfterCommit` 动作）；新增 `stockin_post_linkage_ctx`（收件人/文案/幂等键取自单据，`LNK:STOCK_IN:POSTED:{id}`）、`run_stockin_deferred_linkage`（永不返回 Err、永不回滚已落定的账） |
| 4 | `backend/src/business/flow/linkage/engine.rs`（+33 行） | 新增 `run_excluding(ex, ctx, skip_actions)` + `is_routed_elsewhere`：排除「由专用路由负责」的动作码（目前只有 `GENERATE_VOUCHER`），避免同一触发点两条路都跑 ⇒ 一张单两张凭证。被跳过的规则**不产生** `RuleRun`（是「不归通用引擎管」，不是「跳过了一次执行」） |
| 5 | `backend/src/business/flow/linkage.rs`（+3 行） | 声明 `#[cfg(test)] mod tests_presets` |
| 6 | `backend/src/business/flow/linkage/tests_presets.rs`（新增，13 条单测） | 用 `include_str!` 读**迁移原文**与代码契约对拍：3 条数量、触发点白名单、状态可达、动作已接管、参数过第 2 层校验、无 SQL 键、`TAKEOVER` 非 `SHADOW`、唯一键不重复、up 逐条种入、down 删同批键、`run_excluding` 只排除 `GENERATE_VOUCHER`、覆盖「专用路由 + 通用引擎」两条入口；最后一条是**调用点漂移守卫**（收货过账源码必须含 `run_excluding(… &[ACTION_GENERATE_VOUCHER])` 与 `run_deferred(`） |
| 7 | `backend/src/business/flow/linkage/tests.rs`（2 处既有用例调整，见「偏差」） | 预置数据进了规则表，两个用例的「空表前提」不再成立 |

## 3 条规则（配置原文 = 迁移种子，跑完之后库里就是这样）

```
              name               | trigger_entity | trigger_status |   action_code    |                  action_params                  | enabled | seq
---------------------------------+----------------+----------------+------------------+-------------------------------------------------+---------+-----
 收货过账 → 生成凭证（联动接管） | STOCK_IN       | POSTED         | GENERATE_VOUCHER | {"mode": "TAKEOVER", "biz_type": "PURCHASE_IN"} | t       |  10
 收货过账 → 通知制单人           | STOCK_IN       | POSTED         | SEND_NOTICE      | {"mode": "TAKEOVER", "recipient": "CREATOR"}    | t       |  20
 任务开工 → 通知负责人与创建人   | TASK           | DOING          | SEND_NOTICE      | {"mode": "TAKEOVER"}                            | t       |  10
(3 rows)
```

原始 SQL 与往返日志：`logs/migration_updown_and_config.txt`。

三条规则的选型理由（都是**现有流程里已经存在的动作**，不是为演示编的）：

1. **收货过账 → 生成凭证**：原来硬编码在 `post_in_tx` ⑦ 步（`voucher::create_by_rule`）。
   规则接管后仍是**同一个函数、同一份取号** —— 配置只是把「生成权」从代码搬到配置。
   走**专用路由** `engine::stockin_voucher`（先影子试算→回滚→旧路径照旧→事后逐字段对比），
   所以 ⑧ 步必须 `run_excluding(GENERATE_VOUCHER)`，否则一张单两张凭证。
2. **收货过账 → 通知制单人**：以前过账**不发任何通知**（只有审批流程节点会发）。
   这条是新增联动，走**通用引擎**（`run_deferred`）：`AfterCommit`，失败只记日志不回滚账。
   收件人 = `row.create_by`，幂等键 `LNK:STOCK_IN:POSTED:{id}`。
3. **任务开工 → 通知负责人与创建人**：原来是 `task::handler::notify_status_change` 里的硬编码文案。
   接管后由规则决定「发不发 / 发给谁 / 覆盖哪些字段」，**故意不覆盖任何字段**（`recipient` 缺省 = LEGACY），
   所以接管前后用户看到的通知逐字相同 —— 第一步是「等价接管」，不是顺手改业务口径。
   走**专用路由** `engine::status_notice`（要影子对比）。触发点选 `DOING` 而非 `REVIEW`：
   `→ REVIEW` 在配了 `task_accept` 验收流程的环境里会先起审批实例，通知被流程接管，规则永远等不到。

## 每条规则端到端（源单据 → 目标单据）

`logs/e2e_round7.txt` / `logs/e2e_round8.txt`（连跑两轮，互相独立；下同）
—— 卡在两次 gateway 断连后重跑的第二轮见 `logs/e2e_round9.txt` / `logs/e2e_round10.txt`（同一二进制、同一库，SUMMARY 逐字节相同）

| 规则 | 源单据动作 | 目标单据（实测取值） |
|------|-----------|---------------------|
| 1 收货过账 → 生成凭证 | 收货单 `IN-20261105-000009` 过账 `POSTED`（B1/B2） | 凭证 `PZ-20261105-000009`：**恰好 1 张**（B3，接管不双发）、3 行分录（B4）、借 = 贷 = 113.0000（B5）、源单回写 `voucher_id`（B6）；日志点名「凭证已由联动配置接管（旧路径本次不生成）」（B7），且不是影子模式（B8） |
| 2 收货过账 → 通知制单人 | 同一张 `IN-20261105-000009` 过账 | `sys_notice` 1 条：标题「收货单已过账」、`biz_type=STOCK_IN`（可按单据反查）、收件人 = 制单人 `admin`（C1–C4）；`run_deferred` 的 outcome=SUCCESS（C5）。**旧逻辑根本不发这条通知，所以「存在」本身就是接管证据** |
| 3 任务开工 → 通知负责人与创建人 | 任务 `LNK5B-…` `TODO → DOING`（D1） | `sys_notice` 1 条：标题「任务状态变更」、收件人 = 创建人 `lnk5b_creator`（D2–D4）；D5 是反证（没被改发给负责人）；日志「联动已接管通知：配置发出（旧逻辑本次不发）」+ `rule_id` 与规则 3 主键一致（D6/D7） |

## 停用 / 改参数（可停、可改）

| 场景 | 断言 | 结果 |
|------|------|------|
| 停用规则 2（收货过账→通知制单人） | 过账后 `sys_notice` 里该单 **0 条**、日志无「提交后联动动作执行结果」（E1–E5） | ✅ |
| 停用规则 1（收货过账→生成凭证） | 过账仍成功，凭证**仍由旧逻辑生成恰好 1 张**（不漏记），日志「凭证仍走旧逻辑（联动未接管）reason=没有 GENERATE_VOUCHER 规则」（E7–E11） | ✅ |
| 停用规则 3（任务开工通知） | 通知仍由旧逻辑发（不漏发），日志无「联动已接管通知」（E12–E14） | ✅ |
| 停用后重新启用 | 3 条恢复 `enabled=true`（E15） | ✅ |
| 改规则 3 参数 `LEGACY → OWNER`（走配置页 API） | 通知改发**负责人** `admin`（F1/F2，行为可观测地变了） | ✅ |
| 改规则 1 参数 `TAKEOVER → SHADOW` | 只做影子试算并回滚，库里仍只有 1 张凭证（F3–F6） | ✅ |
| 收尾 | 3 条规则恢复预置（`enabled` + `TAKEOVER` + 原参数），规则表仍 3 行（G1/G2） | ✅ |

停用/修改全部走 5A 交付的配置面 HTTP（`PUT /api/business/flow/linkage/toggle`、`PUT /api/business/flow/linkage`），
不是直接改库 —— 即「运维在页面上点一下」这条路径本身也被 e2e 覆盖。

## 验收逐条

| 验收项 | 结果 | 证据 |
|--------|------|------|
| 3 条规则配好，贴配置 | ✅ | 上面「配置原文」表 + `logs/migration_updown_and_config.txt`（`enabled=3 / takeover=3`）；迁移 down→up 往返：3 行 → 0 行 → 3 行，再跑一次 up 仍是 3 行（幂等） |
| 每条端到端实测（源单据 + 目标单据） | ✅ | `logs/e2e_round7.txt`、`logs/e2e_round8.txt`（各 53 PASS / 0 FAIL）；B/C/D 三组就是三条规则的单据级证据 |
| 停用规则后不再触发 | ✅ | E1–E14（三条规则逐条停用，各带「不再触发 / 退回旧逻辑 / 不漏发」三面） |
| 规则可改 | ✅ | F1–F6（收件人改 OWNER 生效；TAKEOVER 改 SHADOW 只试算不落账） |
| 单测 ≥ 8 条 | ✅ **13 条** | `tests_presets.rs` 13 个 `#[test]`；连同 linkage 模块全部 52 条单测真库跑绿（`52 passed; 0 failed`，13.64s，无 `SKIP`），第二轮复跑同结果（`logs/linkage_unit_round2.txt`） |
| git commit | ✅ | 见卡内汇报 |
| 连跑两轮 | ✅ 8 轮 | 最终内容（含 clippy 修复后的二进制）连跑第 7、8 轮：`PASS=53 FAIL=0` ×2；同一套 e2e 在**从零重建的库**上另跑 5、6 轮也全绿 |

## 第二轮独立复验（crash 后接手，全部重跑一遍）

前两次 attempt 在 **gateway 断连**（`Kanban owner refused: Gateway disconnected`）时被 reclaim，改动与证据都已落盘但没有 commit。
接手后不采信上一轮的自述，用**同一棵树 / 同一二进制 / 同一隔离库**把所有验收项重跑了一遍：

| 项 | 命令 | 结果 | 日志 |
|----|------|------|------|
| 二进制一致性 | `cargo build --bin cloudpivot`（`/root/lnk5b-wt`，target `/root/p1fix4-target`） | `Finished dev profile in 0.28s`（源文件与已构建二进制**零差异**，不存在「跑的旧二进制」） | 见正文 |
| 迁移往返 | V0254 down → up → up（再跑一次验幂等） | `DELETE 3` → `规则行数=0` → `INSERT 0 3` → `规则行数=3` → 再 up 仍 `3` | `logs/migration_updown_round2.txt` |
| 端到端 ×2 | `python3 scripts/p1fix5b_e2e.py`（第 9、10 轮） | **PASS=53 / FAIL=0** ×2（两次 SUMMARY 逐字节相同） | `logs/e2e_round9.txt`、`logs/e2e_round10.txt` |
| linkage 单测 | `ERP_DB_URL=…/cloudpivot_lnk5b_ut cargo test --lib business::flow::linkage -- --test-threads=1` | **52 passed; 0 failed**（含 `tests_presets` 13 条） | `logs/linkage_unit_round2.txt` |
| 受影响模块单测 | `cargo test --lib business::erp::stockin_handler -- --test-threads=1`（真库） | **135 passed; 0 failed**（424s） | `logs/stockin_unit_round2.txt` |
| clippy / fmt | `cargo clippy --lib --tests -- -D warnings` / `cargo fmt --check` | `CLIPPY_EXIT=0` / `FMT_EXIT=0` | `logs/clippy_fmt_round2.txt` |

第 9 轮的本轮实测关键取值（每轮末尾 e2e 自带复位，第 10 轮起点与第 9 轮一致）：

```
[PASS] A5 规则表恰好 3 行  —— rows=3
[PASS] B2 源单据状态 = POSTED  —— IN-20261105-000017 status=POSTED
[PASS] B3 目标单据：凭证恰好 1 张（接管不双发）  —— rows=1
[PASS] B6 源单据回写了 voucher_id  —— voucher_no=PZ-20261105-000017
[PASS] C1 目标单据：这条收货单的通知恰好 1 条（旧逻辑根本不发，存在即接管）  —— rows=1
[PASS] C4 收件人 = 制单人（admin，recipient=CREATOR）  —— admin
[PASS] D1 源单据动作：任务 TODO → DOING（开工）  —— code=200 ok
[PASS] D2 目标单据：任务开工通知 1 条  —— rows=1
[PASS] E4 **停用生效**：该单没有任何通知（联动不再触发）  —— notices=[]
[PASS] E9 **停用生效**：凭证仍由旧逻辑生成，恰好 1 张（不漏记）  —— rows=1
[PASS] E13 **停用生效**：通知仍由旧逻辑发（不漏发），但不是联动发的  —— rows=1
[PASS] F2 **改规则生效**：通知改发负责人  —— receivers=admin
[PASS] F5 SHADOW 只试算 → 库里仍只有 1 张（旧路径落账）  —— rows=1
[PASS] G2 规则表最终仍是 3 行（没有多出版本）  —— rows=3
```

> 记录方式：`e2e_round*.txt` 全部是脚本 stdout 的原样重定向（`> logs/…txt`），不是手抄。

## 校验命令（本目录可直接复现）

```bash
# 1) 隔离库 + 实例（库 cloudpivot_lnk5b，端口 18130）
RECREATE=1 bash scripts/p1fix5b_dbsetup.sh
REPO=/root/lnk5b-wt BIN=/root/p1fix4-target/debug/cloudpivot bash scripts/p1fix5b_instance.sh start

# 2) 端到端（可重复跑，自带复位）
LNK_BASE=http://127.0.0.1:18130 LNK_DB=cloudpivot_lnk5b \
LNK_LOG=/root/p1fix5b-server.log LNK_PW_FILE=/root/lnk5b-admin-password.txt \
python3 scripts/p1fix5b_e2e.py          # 期望 SUMMARY PASS=53 FAIL=0

# 3) 单测（真库；不设 ERP_DB_URL 会静默 SKIP 成假绿）
bash scripts/p1fix5b_auxdb.sh           # 建 cloudpivot_lnk5b_ut / _inv
cd /root/lnk5b-wt/backend && ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_lnk5b_ut \
  CARGO_TARGET_DIR=/root/p1fix4-target cargo test --lib business::flow::linkage -- --test-threads=1
```

日志索引：`logs/e2e_round1..10.txt`（本卡最终内容为 5–10 轮；9、10 是 crash 后第二轮的独立复跑）、
`logs/migration_updown_and_config.txt`、`logs/migration_updown_round2.txt`、
`logs/linkage_unit_final.txt`、`logs/linkage_unit_round2.txt`、
`logs/stockin_unit_final.txt`、`logs/stockin_unit_round2.txt`、
`logs/clippy_fmt.txt`、`logs/clippy_fmt_round2.txt`。

> 收尾状态：隔离实例（18130）验收跑完即停；库 `cloudpivot_lnk5b` / `_ut` / `_inv` 与 worktree
> `/root/lnk5b-wt`、target `/root/p1fix4-target` 保留，便于复审直接复跑（命令见上）。

## 静态检查

| 项 | 结果 |
|----|------|
| `cargo clippy --lib --tests -- -D warnings` | **EXIT 0**（顺手修掉上一轮遗留的 `clippy::manual_contains`：`skip_actions.iter().any(|i| *i == code)` → `skip_actions.contains(&code)`） |
| `cargo fmt --check` | 0 diff（对 `stockin_handler_post.rs` / `tests_presets.rs` 跑过 rustfmt） |
| 受影响模块真库用例 | `business::flow::linkage` 52/52（含新增的「调用点漂移守卫」）；`business::erp::stockin_handler` 见 `logs/stockin_unit_final.txt` |

## 偏差与范围说明（不假完成）

1. **生产未部署**：`172.18.10.144` 上 `biz_flow_linkage_rule` 仍是 **0 行**，`schema_migrations` 里只有
   `flow_linkage_rule`(V0205)，**V0253（配置页）与 V0254（预置规则）都还没记账**。
   本卡只在隔离库验证；下次跑 `scripts/deploy/deploy.sh` 会一并应用。**风险要提前知道**：
   这两条迁移一应用，生产上 3 条规则立刻是 `TAKEOVER`（真生效）—— 收货过账的凭证生成权从硬编码
   切到配置路径（同一函数同一取号，行为等价，但日志/执行顺序变了），任务开工通知也改由配置发出。
   建议发版时把它当作行为变更来复核（`deploy_check` 的巡检项已覆盖 health/invariants）。
2. **只有 2 个触发点真的接线**：`spec::TRIGGERS` 白名单有 15 个触发点，真正被调用的只有
   `TASK`（`status_notice`）和 `STOCK_IN`（⑧ 步 + `stockin_voucher`）。在其余 13 个触发点上配规则
   **配得上但永不触发**，且配置页没有「该触发点未接线」的提示。这是 5A/5B 范围外的产品缺口，
   已在卡内汇报（建议另开卡：给触发点加 `wired` 标记 + 置灰，或补齐 13 个接线点）。
3. **改了 2 个既有用例的前提**（不是放宽断言）：
   - `engine_skips_disabled_rules`：原来断「`run.runs` 为空」，预置规则进了同一个库之后必然不空；
     改成断「**这条被停用的规则**不在 `runs` / `deferred` 里」——断言更精确，不是放水。
   - `status_notice_bad_params_fall_back_to_legacy`：原来直插 `TASK/DOING/SEND_NOTICE`，而 V0254 正好
     占了这三个键的唯一约束；换到没被占用的合法状态 `TASK/REVIEW`（用例只关心「脏参数能不能退回旧逻辑」）。
4. **`recipient` 缺省 = LEGACY**：规则 3 故意不覆盖字段，所以接管前后通知逐字相同。这不是「预置规则没用」，
   而是「第一步等价接管」；要改收件人，运维在页面上改 `recipient` 即可（F1/F2 已证明生效）。
5. **`GENERATE_VOUCHER` 在配置页仍显示为「已接管」**：它的接管发生在 ⑦ 步专用路由，⑧ 步排除它是
   代码级保证（单测 `engine_skips_only_actions_routed_elsewhere` 钉住），不是配置能改的。

## 一次误判的修正（留档）

上一轮（本卡 attempt 1）的 V0254 收尾 NOTICE 写的是「任务验收→通知创建人」，与规则实际触发点
（`TASK/DOING` 开工）不符 —— 纯文案错位。因为迁移一旦应用就有内容指纹校验、不能再改，
所以本轮**在提交前**改成「任务开工(TODO→DOING)→通知负责人与创建人」，然后**重建隔离库**
（`RECREATE=1`，从零跑 196 个迁移）重新取证；e2e 与单测的最终日志都是改后内容的运行结果。
