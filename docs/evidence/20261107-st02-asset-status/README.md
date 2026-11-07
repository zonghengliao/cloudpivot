# FIN-ASSET-ST02 卡 A 证据：资产停用 / 启用接口 + FIXED_ASSET 第三处联动接线

- 卡：`t_4201b539`（backend-developer）｜设计：`docs/architecture/FIN-ASSET-ST02-资产停用启用接口设计.md`
- commit：`e089d77`（`origin`(145) 与 `github` 均为 `e089d77`，HEAD == 两远端 main）
- 基线：`2c4765f`（LNK-10 折旧批次接线）

## 1. 交付内容（9 个文件）

| 文件 | 改动 |
|---|---|
| `erp/asset_dto.rs` | `StatusChangeReq` / `StatusChangeSpec` / `validate_status_change` / `change_type_of` + 单测 `status_change_validation` |
| `erp/asset_handler.rs` | 新 handler `asset_status_change`（`POST /business/erp/asset/{id}/status`）；`asset_dispose_cancel` 接住 `insert_change` 返回值 |
| `erp/asset_bridge.rs` | 第 ④ 步接住变动行 id 传给 hook |
| `erp/asset_linkage.rs` | `run_in_tx` 第 6 参数 `event_ref` + `AssetStatusChange.event_ref` + 幂等键事件维度 + `assert!(key.len() <= 128)`；单测改「同事件同键 / 不同事件不同键」 |
| `shared/error_code.rs` | `ERP_ASSET_STATUS_ILLEGAL = 66046` + 稳定表一行（无迁移） |
| `flow/linkage/spec.rs` | `FIXED_ASSET.wiring` 三处接线文案（删「暂无触发源」免责句） |
| `flow/linkage/tests_wiring.rs` | handler 内 `run_in_tx(` / `run_deferred_after_commit(` 计数 1→2；新增存在性 + 顺序断言（文件级调用点计数不动） |
| `erp/asset_linkage_tests.rs` | 真库用例 +6（`FA-ST02-` / `ST02-` 号段） |
| `docs/architecture/P1-FIX-5C-联动触发点接线清单.md` | `FIXED_ASSET` 总表行 + §3.2 三处触发源 |

## 2. 全量 lib 测试（staged 内容、隔离 clone）

```
$ cargo test --lib -- --skip wired_triggers_match_call_sites
test result: ok. 3232 passed; 0 failed; 1 filtered out
```

> 为何 skip 那 1 条：`wired_triggers_match_call_sites` 属于 **LNK-11（年结）在途工作**——
> `2c4765f` 已把 `tests_wiring` 的年结守卫（含 `include_str!("../../erp/year_end_linkage.rs")`）
> 提交，但 `erp/year_end_linkage.rs` 本身还没提交，于是该用例要么编译不过（文件缺失）、
> 要么计数不一致（文件补上但 spec 未 wired）。本卡文件与该用例无关（`git diff HEAD` 只含
> `asset_*` / `error_code` / `FIXED_ASSET.wiring` / asset 测试）。

## 3. clippy / fmt

```
$ cargo clippy --lib --tests -- -D warnings -A clippy::too_many_arguments -A clippy::useless_format
    Finished `dev` profile ... （无告警）
```
- 放行的两条是 **main 上既有的**别人的告警，与本卡无关（`git status --porcelain` 对这两个文件为空）：
  - `src/business/erp/sales_order_model.rs:699` `too_many_arguments`（`bfd275a` SO-40）
  - `src/business/erp/fx_revaluation.rs:788` `useless_format`（`1bf926b` SO-41）
- `cargo fmt -p cloudpivot -- --check`：本卡 6 个 `.rs` 零 diff（唯一 diff 在
  `currency_admin_model.rs`，属别人在途改动）。

## 4. 真库用例（隔离库 `cloudpivot_st02`，`ERP_DB_URL` + `--test-threads=1`）

```
running 11 tests
test asset_linkage_tests::lnk09_cancel_disposal_triggers_restore_notice ... ok
test asset_linkage_tests::lnk09_disposal_triggers_disposed_notice ... ok
test asset_linkage_tests::lnk09_excluded_voucher_actions_never_run ... ok
test asset_linkage_tests::lnk09_repeat_disposal_does_not_trigger_twice ... ok
test asset_linkage_tests::lnk09_rolled_back_tx_never_sends_notice ... ok
test asset_linkage_tests::st02_dispose_restore_dispose_notifies_twice ... ok
test asset_linkage_tests::st02_enable_triggers_once ... ok
test asset_linkage_tests::st02_excluded_actions_not_run ... ok
test asset_linkage_tests::st02_idle_triggers_once ... ok
test asset_linkage_tests::st02_repeat_idle_conflicts ... ok
test asset_linkage_tests::st02_reversible_edges_notify_twice ... ok
test result: ok. 11 passed; 0 failed; 0 ignored; ... finished in 22.03s
```

跑完清场复核（`psql -d cloudpivot_st02`）：

```
assets=0   rules=0   notices=0
```

## 5. 变异红（两组，均已还原并复跑全绿）

### ① 删掉「同态 409」判定 → 用例 3 立红

```
thread 'st02_repeat_idle_conflicts' panicked at asset_linkage_tests.rs:974:
assertion `left == right` failed: 必须用 66046（...）：固定资产 FA-ST02-01a1b56b 的状态没有
变化（IDLE → IDLE）：原地流转不是状态变更，不要写状态日志
  left: 10003
 right: 66046
```
（删掉判定后重复停用被 `state_machine::transition` 的原地流转刹车拒成 10003，
说明该分支正是「重复点击给 66046」的唯一来源。）

### ② 幂等键去掉 `event_ref` → 用例 5 立红

```
thread 'st02_reversible_edges_notify_twice' panicked at asset_linkage_tests.rs:1119:
assertion `left == right` failed: 停用 → 启用 → 再停用必须各发一条（共 3 条）：事件维度幂等键
  left: 2
 right: 3
```
（旧键「状态+卡片」下第二次停用撞 `uk_sys_notice_idem` 被 `ON CONFLICT DO NOTHING` 吞掉。）

## 6. 真机 HTTP（隔离实例 `127.0.0.1:18093` + 独立库，验收后已释放）

```
登录成功（admin / Admin@123456，must_change_password=True → 改密后重登）
[PASS] 取到启用中的资产分类  code=200 n=5
[PASS] 取到部门（夹具部门）  code=200 n=1
[PASS] 建夹具资产卡  code=200 no=FA-20261107-000001
[PASS] 建卡后卡片状态 = IN_USE  status=IN_USE
[PASS] 停用 200 且返回详情 status=IDLE  code=200 status=IDLE
[PASS] 重复停用 409 / code=66046  code=409 biz_code=66046
        msg=资产 FA-20261107-000001 当前已经是「闲置」：重复停用不是状态变更（不会重复通知）
[PASS] 详情 changes 出现 IDLE 行  changes=[('IDLE', 'IN_USE', 'IDLE')]
[PASS] DISPOSED 被 400 拒绝并指向 /dispose  code=400
        msg=处置不能走本接口：请调用 POST /business/erp/asset/{id}/dispose 提交处置审批…
汇总：8/8 通过
```

库侧取证（`psql`）：

```
 status | IDLE
 vouchers（source_id = 该卡，deleted_at IS NULL） | 1   ← 只有建卡那张购入凭证，停用零新增
 biz_fixed_asset_change | IDLE / IN_USE → IDLE / reason=生产线搬迁，设备暂不外借 / people_null=t
 biz_status_log          | (NULL → IN_USE) 建卡 + (IN_USE → IDLE) 停用
清场后：residue_assets=0  residue_vouchers=0；实例已停（端口 18093 监听 0）、库已 drop
```

## 7. 口径守卫（D4：一律未动）

`asset_model::scan_assets_for_period` 的 `status IN ('IN_USE','IDLE')`、
`depreciation_model::in_depreciation_scope`、`asset_dto::LIVE_STATUSES`、
`state_machine::FIXED_ASSET_BASE` 边标签 —— 本卡零改动（`git show e089d77 --stat` 里没有这些文件）。

## 8. 并发与提交纪律（如实记录）

共享工作区同期有 LNK-10（已提交 `2c4765f`）与 LNK-11（年结，未提交）在改 `flow/linkage/spec.rs`
与 `tests_wiring.rs`。本卡**只提交自己的 9 个文件**：5 个 asset 文件 + `error_code.rs` + 文档用
`git add`；`spec.rs` / `tests_wiring.rs` 用「`git show HEAD:` + 本卡 hunk」重建后
`git hash-object -w` + `update-index --cacheinfo` 进索引，工作区保留别人的在途改动
（`git diff --cached --name-status` 已核对只有 9 个文件）。
