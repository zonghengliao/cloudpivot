# LNK-09 · 固定资产（FIXED_ASSET）接入联动引擎 — 物证

> 卡：`t_f874341d` · 执行：backend-developer · 日期：2026-11-07
> 关联：`erp/asset_linkage.rs`（唯一 hook）· `spec::FIXED_ASSET_EXCLUDED_ACTIONS` ·
> `tests_wiring::asset_wires_generic_engine_and_excludes_voucher`

## 1. 接线结论（一处 hook、两处真实状态推进）

| 触发点 | 入口 | 目标态 | 事务边界 |
|---|---|---|---|
| 处置（审批通过） | `erp/asset_bridge.rs::execute` 第 ⑤ 步 | `IN_USE\|IDLE → DISPOSED` | 事务内 `run_excluding`，commit 后 `run_deferred` |
| 作废处置 | `erp/asset_handler.rs::asset_dispose_cancel` 第 ⑤ 步 | `DISPOSED → IN_USE` | 事务内 `run_excluding`，commit 后 `run_deferred` |

`spec.rs`：`FIXED_ASSET.wired = true`，`wiring` 写真实调用点，`proofs` 指
`src/business/erp/asset_linkage.rs` 的 `linkage::engine::run_excluding` / `run_deferred`
（`tests_wiring::wired_trigger_proofs_exist_in_source` 与反向扫描双向钉住）。
`tests_wiring`：wired 11 → **12/15**，调用点 12 → **13 个文件**。

### 排除清单（防重复落账）

`FIXED_ASSET_EXCLUDED_ACTIONS = [GENERATE_VOUCHER, REVERSE_VOUCHER]`：

- 处置凭证由 `asset_bridge::post_disposal_vouchers` 出 1~3 张（出售 3：1606 / 1002 / 2221.02；
  报废 / 盘亏 2），通用引擎再跑 `GENERATE_VOUCHER` 就是「一张卡两套处置凭证」，
  巡检 K7「1606 / 1901 必须归零」当场红；
- 作废处置的红冲由 `asset_bridge::reverse_disposal_vouchers` 原路做，再跑
  `REVERSE_VOUCHER` 会撞「已经红冲」→ 500；
- `SEND_NOTICE` / `CREATE_TASK` **不排除**（排掉就等于接线了永远不响）。

## 2. 卡面「停用 / 启用」的实况（必须诚实说明）

卡面写「四处状态推进（停用/启用/处置/作废处置）」，**代码里实际只有两处**：

```
$ grep -rn "state_machine::transition" backend/src/business/erp/asset*.rs
asset_bridge.rs:222   ← 处置（IN_USE|IDLE → DISPOSED）
asset_handler.rs:1067 ← 作废处置（DISPOSED → IN_USE）

$ grep -rn "STATUS_IDLE" backend/src | grep -v asset_dto.rs
（空）—— 停用 / 启用没有任何写入点
```

- `state_machine` 里有 `IN_USE ⇄ IDLE` 两条 mandatory 边、`doc_kind` 有 `IDLE`/`ENABLE`
  变动类型，但 `asset_handler.rs` 的 14 个接口里**没有状态变更接口**
  （`asset_model::apply_asset_status` 的调用方只有单测），前端资产页也只有
  转移 / 换人 / 处置 / 作废处置四个按钮。
- 结论：**不是接线漏了，是功能没有入口**。在本卡凭空虚造 `POST /asset/{id}/status`
  属于资产模块二开（新接口 + 权限种子 + 前端），不是接线卡的范围，所以：
  - 不虚造调用点；
  - 把这一点写进 `spec::TRIGGER_SPECS.wiring`（配置页直接可见），
    `FIXED_ASSET/IDLE` 上的规则「可保存、暂无触发源」；
  - 已按「发现后续工作 → 建卡，不自己扩范围」的口径登记后续卡（见卡内评论）。
- 建卡（`NULL → IN_USE`）是**建行**，按开账同口径不接（导入 N 张卡不能扇出 N 条通知）。

## 3. 单测

```
cargo test --lib                                  → 3217 passed / 0 failed（基线 3208，本卡 +9）
cargo test --lib -- business::erp::asset_linkage  → 8 passed
  （3 条纯逻辑：幂等键 / 通知草稿 / 状态中文名对拍；
    5 条真库：见下）
```

真库用例（`ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_lnk09n --test-threads=1`）：

| # | 用例 | 断言 |
|---|---|---|
| 1 | `lnk09_disposal_triggers_disposed_notice` | 审批通过 → `DISPOSED` + 处置凭证 ≥1 + `LNK:FIXED_ASSET:DISPOSED:<id>` = 1 |
| 2 | `lnk09_repeat_disposal_does_not_trigger_twice` | 第二次处置返回 None（409）→ 状态不变 + 通知仍 1 |
| 3 | `lnk09_cancel_disposal_triggers_restore_notice` | 作废处置 → `IN_USE` + `LNK:FIXED_ASSET:IN_USE:<id>` = 1 |
| 4 | `lnk09_rolled_back_tx_never_sends_notice` | 事务内 deferred=1 / 通知 0 → 回滚后仍 0 → commit+flush 后 1（正向对照） |
| 5 | `lnk09_excluded_voucher_actions_never_run` | 引擎级：带排除清单时 `GENERATE_VOUCHER` 不产生 RuleRun（不带时会产生）；端到端：销售处置凭证恰好 3 张、通知照发 1 条 |

```
test result: ok. 5 passed; 0 failed; ... finished in 8.44s
# 夹具残留：assets 0 / instances 0 / notices 0 / rules 0 / users 0
```

### 变异红（证明断言不是虚绿）

| 变异 | 结果 |
|---|---|
| `run_excluding(ex, &ctx, spec::FIXED_ASSET_EXCLUDED_ACTIONS)` → `&[]` | `tests_wiring::asset_wires_generic_engine_and_excludes_voucher` **红**（「必须真的把 FIXED_ASSET_EXCLUDED_ACTIONS 传下去」） |
| `asset_dispose_cancel` 传给 hook 的目标态 `IN_USE` → `DISPOSED` | `lnk09_cancel_disposal_triggers_restore_notice` **红**（left 0 / right 1） |

均还原后重跑：3217 绿 + 真库 5 绿。

## 4. 真机 HTTP 取证（真链路，不是直调函数）

```
bash scripts/verify/lnk09_start.sh          # 隔离实例 :18059 / DB cloudpivot_lnk09n
python3 scripts/verify/lnk09_http_evidence.py
```

输出全文见同目录 `curl_evidence.txt`，关键行：

```
① GET /api/business/flow/linkage → wired=true（wiring 段落含「两处真实状态推进」）
② PUT 规则 DISPOSED / IN_USE 各一条 SEND_NOTICE（TAKEOVER→CREATOR）
③ POST /asset 建卡：FA-20261107-000003 status=IN_USE
④ POST /asset/{id}/dispose → 卡片仍 IN_USE，DISPOSED 通知 = 0（提交不是触发点）
⑤ POST /flow/task/{id}/approve →
   卡片 = DISPOSED；sys_notice[LNK:FIXED_ASSET:DISPOSED:…] = 1；处置凭证 = 3 张
⑥ 再次 approve → idempotent=true；卡片 DISPOSED；通知 = 1；凭证 = 3（不翻倍）
⑦ POST /asset/{id}/dispose/cancel → 卡片 IN_USE；通知[IN_USE] = 1
ALL PASS
```

清场：卡片 / 处置审批实例 / 规则 / 通知 / 凭证 / 夹具部门 / 夹具制单人全部删除
（GENERAL_MANAGER 角色借用已还原），库内残留 0；:18059 实例已停、端口已释放。

## 5. 已知边界

- `FIXED_ASSET/IDLE` 上的规则**当前没有触发源**（无停用/启用接口，见 §2）；
- 部门转移 / 使用人变更**不是状态变更**（`asset_model` 的 CAS UPDATE + `log_event`），
  不在本触发点范围内（与 P1-FIX-5C 文档 §3.2 原口径一致）；
- 未新增迁移（只接线，不改表）。
