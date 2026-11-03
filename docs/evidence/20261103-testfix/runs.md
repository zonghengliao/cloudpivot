# 复跑原始输出（t_6d815eda）

同一份代码基线（`42f671e` + 本卡改动）、**同一个库**（`cloudpivot_t6d8`，里面 3 个超管 = 3 人会签），
只有夹具不同。

## A. 修前（stash 掉本卡改动，重编后跑同一条用例集）

命令：

```
ERP_DB_URL=postgres://…/cloudpivot_t6d8 \
  <cargo test 产出的 lib 测试二进制> stockin_handler_tests_cancel --test-threads=1
```

结果：

```
test result: FAILED. 4 passed; 11 failed; 0 ignored; 0 measured; 2072 filtered out; finished in 38.06s

failures:
    …::cancel_keeps_invariant_e4
    …::cancel_posted_voids_serials_of_the_receipt
    …::cancel_rolls_back_cost_inventory
    …::cancel_rolls_back_location_ledger
    …::cancel_still_generates_red_voucher
    …::cancel_twice_is_409
    …::cancel_uses_posted_amount_not_current_avg
    …::cancel_voids_only_serials_of_that_receipt
    …::cancel_writes_out_txn
    …::cancel_zero_qty_zeroes_amount
    …::concurrent_cancels_do_not_deadlock
```

失败原文（与 DEPLOY-B 卡面记录逐字一致）：

```
thread '…::cancel_writes_out_txn' panicked at src/business/erp/stockin_handler_tests_helpers.rs:343:5:
assertion `left == right` failed: 复核通过后单据必须真的作废
  left: "PENDING_CANCEL"
 right: "CANCELLED"
```

完整原始输出：`runs-prefix.txt`。

## B. 修后（本卡改动恢复，同一个库、同一条命令）

```
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 2072 filtered out; finished in 47.92s
```

完整原始输出：`runs-postfix.txt`。

## C. 全量套件

| 场景 | 命令 | 结果 |
|------|------|------|
| 干净库（1 超管），修前 | `cargo test -j 1` | lib `2086 passed / 1 failed`（唯一红 = `shared::startup_guard::tests::merge_semantics_or_base_and_profile`，因为隔离 clone 缺 `config/config-dev.toml`（gitignore 的本机文件），补上后即绿 —— 见 §D） |
| 干净库（1 超管），修后 | `cargo test -j 1` | **lib `2087 passed / 0 failed / 0 ignored`**；`tests/testd` 66/1（竞态，见 §E） |
| 3 超管库（会签 3 人），修后 | `cargo test -j 1` | **lib `2087 passed / 0 failed / 0 ignored`；22 个 target 全 ok、0 FAILED**（含 `testd` 67/0） |
| 3 超管库，修后（定点） | 逐模块 filter | `stockin_handler_tests_cancel` 15/15、`_p16` 16/16、`_a4` 13/13、`_d4` 17/17、`voucher::tests` 21/21、`transfer_handler` 56/56、`payable_bridge::tests` 23/23、`purchase_handler::tests` 44/44、`edge_admin_tests` 14/14 |

> 3 超管库的构造：`sys_user` 插 `t6d8_ap1` / `t6d8_ap2` 并挂 `SUPER_ADMIN`
> 角色（与 `payable_bridge` 夹具的 `p17_ap1/p17_ap2` 同形态），见 `/root/t6d8-repro.sh`。
> 这两个人在其它 ADMIN 节点的流程里同样算审批人 —— 这正是「同一提交两种结果」的成因。

## D. `shared::startup_guard` 那条红是环境，不是本卡

该用例读**仓库里的真实配置文件**（`config.toml` / `config-dev.toml` / `config-prod.toml` /
`config-test.toml`）断言合并语义。`config-dev.toml` 与 `config.toml` 是 `.gitignore` 的本机文件，
隔离 clone 里天然没有 → `auth_enabled_in_files(DEFAULT_CONFIG_DIR, "dev")` 返回 `Some(false)`，
断言 `Some(true)` 失败。按既有惯例从 `/opt/cloudpivot/backend/config/` 拷过来后即绿（不需要改代码）。

## E. `tests/testd` 的 `zz_no_fixture_residue_after_full_suite` 是既有竞态

它自己模块文档写着「`zz_` 前缀在 `--test-threads=1` 下最后执行」；`cargo test` 默认按 CPU 数
并行（`-j 1` 只限制**构建**任务数，不限制测试线程数），4 线程时它会与兄弟用例并发读到瞬时行
（本次实测 `sys_file=1`，随后该行已被兄弟用例清掉）。同一条命令加 `--test-threads=1`：`67 passed / 0 failed`。
与本卡改动无关（`TD-` 前缀夹具属 testd 自己的目标），故未在本卡范围内修。
