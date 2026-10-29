# 批次C · C7 验证证据：回滚验证门禁化

> 依据：`docs/批次C-技术架构.md` §4.12（三层测试）+ §7 C7 行 + §5.7 R14。
> 环境：`172.18.10.144:/opt/cloudpivot`，HEAD `699390f`（本卡提交），库 `cloudpivot_test`（V0084 已应用，3 个月分区）。
> 下面全部是**原始输出**（只把探测用的连接串口令替换成 `***`），没有转述。

---

## 1. 验收 ①：`cargo test --features test-hooks` 全绿

```
$ cd /opt/cloudpivot/backend && cargo test -j 2 --features test-hooks
test_rc=0
--- 各 binary 结果 ---
     Running unittests src/lib.rs
test result: ok. 508 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
     Running unittests src/main.rs
test result: ok. 0 passed; 0 failed; 0 ignored
     Running tests/batchB_b2_scope.rs
test result: ok. 1 passed; 0 failed; 0 ignored
     Running tests/batchB_b7_mutex.rs
test result: ok. 1 passed; 0 failed; 0 ignored
     Running tests/batchC_c3_dispatch.rs
test result: ok. 1 passed; 0 failed; 0 ignored
     Running tests/batchC_c4_help.rs
test result: ok. 11 passed; 0 failed; 0 ignored
     Running tests/batchC_c5_changelog.rs
test result: ok. 8 passed; 0 failed; 0 ignored
     Running tests/batchC_c6_changelog.rs
test result: ok. 4 passed; 0 failed; 0 ignored
     Running tests/batchC_c7_rollback.rs          ← 本卡新增（2 条，feature 关闭时编译为空）
test result: ok. 2 passed; 0 failed; 0 ignored
     Running tests/impexp_worker.rs
test result: ok. 7 passed; 0 failed; 0 ignored
     Running tests/s3_minio.rs
test result: ok. 2 passed; 0 failed; 0 ignored
   Doc-tests cloudpivot
test result: ok. 0 passed; 0 failed; 7 ignored
--- 汇总计数 ---
passed=545 failed=0 ignored=7
```

**无回归对照**（不带 feature，即团队日常口径）：

```
$ cargo test -j 2
test_rc=0
passed=541 failed=0 ignored=7

# 差额恰好是 4 = test_hooks 的 2 条纯逻辑单测（lib 508 → 506 即 C6 基线）+ 2 条集成用例
```

带真库跑本卡的两条集成用例：

```
$ C7_DB_URL=postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_test \
  cargo test -j 2 --features test-hooks --test batchC_c7_rollback -- --nocapture --test-threads=1

running 2 tests
test commit_control_group_leaves_one_row ... 清理夹具：删除 0 行 sys_change_log
清理夹具：删除 1 行 sys_change_log
[PASS] commit_control_group_leaves_one_row: 1 行
ok
test rollback_leaves_zero_residue ... 清理夹具：删除 0 行 sys_change_log
ROLLBACK_PROBE=0|1|0
清理夹具：删除 0 行 sys_change_log
[PASS] rollback_leaves_zero_residue: 0|1|0
ok

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

`commit_control_group_leaves_one_row` 是对照组：同样写法**提交**后必须剩 1 行，
证明 `0|1|0` 不是「怎么写都是 0」的假绿。

---

## 2. 验收 ②：`bash scripts/verify/changelog_rollback.sh` → `0|1|0` 且 exit 0

```
$ bash scripts/verify/changelog_rollback.sh
已加载数据库配置：/etc/cloudpivot/cloudpivot-test.env
构建并运行回滚探针（--features test-hooks --example c7_rollback_probe）…
0|1|0
PASS：回滚零残留（回滚前 0 / 事务内 1 / 回滚后 0）
verify_rc=0
```

CI 口径（只看 stdout，按 `|` 切三段）：

```
$ OUT=$(bash scripts/verify/changelog_rollback.sh 2>/dev/null); echo "[$OUT]"
[0|1|0]
$ printf '%s' "$OUT" | grep -qx '0|1|0' && echo PIPE_ASSERT_OK
PIPE_ASSERT_OK
```

---

## 3. 验收 ③④：`bash scripts/check.sh` 整体通过 + 6 条门禁全 `exit 0`

```
$ bash scripts/check.sh
==> 后端：cargo fmt --check
==> 后端：cargo clippy -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.21s
==> 后端：cargo check
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.54s
==> 前端：pnpm typecheck
> vue-tsc --noEmit
==> 批次B 闸①：分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）
    白名单：business/shared/scope.rs（+ 注释行不计）
  PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
    登记表：sys_user sys_dept sys_import_task sys_export_task sys_notice sys_file operation_logs audit_logs auth_logs security_logs
  PASS：0 处
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> 批次B 门禁：全部通过
==> C7 G1：只有 business/changelog/ 能碰 sys_change_log（防止别处偷偷 INSERT）
  PASS：0 处
==> C7 G2：changelog 模块内禁止自开事务（db() 非注释命中必须为 0）
  PASS：0 处（注释中的示例不计）
==> C7 G3：record 的签名必须带 &impl SqlExecutor
  PASS
==> C7 G4：不得存在『无 executor 版本』的 record 函数
  PASS
==> C7 G5：每个 record/record_in 调用点必须在 transaction 闭包内且传 &tx
G5 PASS：19 个 record/record_in 调用点全部在 transaction 闭包内且传 &tx（扫了 147 个文件）
==> C7 G6：按 id 查 sys_change_log 必须同时带分区键 created_at（R14）
  PASS：0 处
==> C7 门禁汇总：G1–G6 共 6 条，未通过 0 条
==> C7 门禁：全部通过
==> 全部检查通过
check_rc=0
```

G5 的逐点判定（`--list`）：

```
$ python3 scripts/lint/changelog_in_tx.py --root /opt/cloudpivot --list
  ok  backend/src/business/announcement/handler.rs:935  record_in(...) 在 transaction 闭包内
  ok  backend/src/business/announcement/handler.rs:1054 record_in(...) 在 transaction 闭包内
  ok  backend/src/business/announcement/handler.rs:1120 record_in(...) 在 transaction 闭包内
  ok  backend/src/business/announcement/handler.rs:1232 record_in(...) 在 transaction 闭包内
  ok  backend/src/business/announcement/handler.rs:1388 record_in(...) 在 transaction 闭包内
  ok  backend/src/business/changelog/mod.rs:292        record(ex) 模块内部透传 executor
  ok  backend/src/business/changelog/test_hooks.rs:119 record_in(...) 使用 begin() 创建的显式事务句柄
  ok  backend/src/business/help/handler.rs:291         record_in(...) 在 transaction 闭包内
  ok  backend/src/business/help/handler.rs:358         record_in(...) 在 transaction 闭包内
  ok  backend/src/business/help/handler.rs:400         record_in(...) 在 transaction 闭包内
  ok  backend/src/business/help/handler.rs:456         record_in(...) 在 transaction 闭包内
  ok  backend/src/business/help/handler.rs:526         record_in(...) 在 transaction 闭包内
  ok  backend/src/system/role/handler.rs:194           record_in(...) 在 transaction 闭包内
  ok  backend/src/system/role/handler.rs:326           record_in(...) 在 transaction 闭包内
  ok  backend/src/system/role/handler.rs:456           record_in(...) 在 transaction 闭包内
  ok  backend/src/system/user/handler.rs:278           record_in(...) 在 transaction 闭包内
  ok  backend/src/system/user/handler.rs:388           record_in(...) 在 transaction 闭包内
  ok  backend/src/system/user/handler.rs:506           record_in(...) 在 transaction 闭包内
  ok  backend/src/system/user/handler.rs:623           record_in(...) 在 transaction 闭包内
G5 PASS：19 个 record/record_in 调用点全部在 transaction 闭包内且传 &tx（扫了 147 个文件）
```

> G5 的已知局限（与架构 §4.12.4 的诚实说明一致）：静态检查只证明「调用点在事务闭包里 +
> 第一个实参是 tx」，证明不了「运行时的 tx 就是业务事务」——后者由 `0|1|0` 真库探针兜底。

---

## 4. 验收 ⑤：G2 反向验证（故意注入真 `db()` → 必须报红）

注入（`backend/src/business/changelog/model.rs` 的 `insert()` 开头加一行真实调用）：

```
已注入： let _g2_reverse_probe = db(); // 反向验证注入（G2 必须报红）
--- 注入后的命中 ---
backend/src/business/changelog/model.rs:96:    let _g2_reverse_probe = db(); // 反向验证注入（G2 必须报红）
```

红：

```
==> C7 G2：changelog 模块内禁止自开事务（db() 非注释命中必须为 0）
  FAIL：1 处（模块内只能吃 &impl SqlExecutor，不能 db()）
    /opt/cloudpivot/backend/src/business/changelog/model.rs:96:    let _g2_reverse_probe = db(); // 反向验证注入（G2 必须报红）
...
==> C7 门禁汇总：G1–G6 共 6 条，未通过 1 条
check_rc=1
```

撤销（`git checkout -- backend/src/business/changelog/model.rs`，`git status` 对该文件无输出 = 完全还原）后重新绿：

```
==> C7 G2：changelog 模块内禁止自开事务（db() 非注释命中必须为 0）
  PASS：0 处（注释中的示例不计）
...
==> C7 门禁汇总：G1–G6 共 6 条，未通过 0 条
==> C7 门禁：全部通过
==> 全部检查通过
check_rc=0
```

---

## 5. 红线：`test-hooks` 不进生产二进制

用 `cargo build --release --lib`（**不是** `cargo build --release`，避免覆盖 prod 在跑的
`target/release/cloudpivot`；实测 md5 前后一致）：

```
prod 二进制 md5（前）：f9df20ed720c44027bc2e05eb6ad78ea

--- 2a) 不带 feature ---
cargo build --release --lib → Finished `release` profile in 37.09s
grep 'record_then_rollback' in target/release/libcloudpivot.rlib → 0
grep 'ROLLBACK_PROBE' → 0
grep 'test_hooks' → 1        ← 文档注释里的模块名残留，不影响符号

--- 2b) 对照：带 feature ---
cargo build --release --lib --features test-hooks → Finished in 35.75s
grep 'record_then_rollback' → 2

--- 2c) 复原为不带 feature ---
grep 'record_then_rollback' → 0（再次 0）

prod 二进制 md5（后）：f9df20ed720c44027bc2e05eb6ad78ea
OK：prod 二进制未被改动（--lib 不重建 bin）

--- 2d) 符号表 ---
nm -C target/release/cloudpivot | grep -c record_then_rollback → 0
nm -C target/release/libcloudpivot.rlib | grep -c record_then_rollback → 0
```

双向成立：不带 feature 时符号**不存在**（不是「存在但没被调用」），带 feature 时才出现。

---

## 6. 为让 G6 成立而改的既有代码 + 回归对照

`query::find` 由「拼半句 + 按需 `push_str` 追加 `created_at`」改成两条完整语句
（不传时间时显式写 `created_at BETWEEN '-infinity' AND 'infinity'`，语义与纯 id 查询逐行相同）。
补运行时对照用例 `tests/batchC_c6_changelog.rs::c6_find_detail_with_and_without_created_at`：

```
$ C6_DB_URL=postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_test \
  cargo test -j 2 --features test-hooks --test batchC_c6_changelog -- --nocapture --test-threads=1

running 4 tests
test c6_change_log_scenarios ... T-CL-02 PASS / T-CL-03 PASS / T-CL-04 PASS / T-CL-05 PASS
                                  T-CL-06 PASS / T-CL-11 PASS / T-CL-13 PASS / 错误路径 PASS：62012
ok
test c6_find_detail_with_and_without_created_at ... [PASS] 两条分支同一行（01a1858d-…-fcf49f7ec003），缺失返回 None
ok
test c6_rollback_keeps_zero_residue ... 回滚后：sys_user=0 行，sys_change_log=0 行
ok
test c6_timeline_performance_and_partitions ... timeline 性能：最快 800.024µs（阈值 50 ms，10 万行）
                                              分区运维 PASS：created=[] dropped=["sys_change_log_2020_01"] kept=3
ok

test result: ok. 4 passed; 0 failed; 0 ignored
```

---

## 7. 口径与差异说明（评审要看的三条）

1. **G1–G6 统一排除整行注释**。架构文档写的是裸 `grep`，但 changelog 模块的注释里大量出现
   `db()` / `record(`（都是为了讲清「不要这么写」）。C7 卡面对 G2 已明确「非注释命中必须为 0」，
   其余五条按同一口径对齐——这是**排除注释**，任何真实代码命中仍然报红（第 4 节已反向验证）。
2. **G4 必须有词边界**。`grep "async fn record"` 会同时命中 `async fn record_in(` 与
   `async fn record_then_rollback(`；门禁实现用 `async fn record(_in)?[[:space:]]*\(`，
   并对每个命中检查其后 20 行内的签名里是否有 `impl SqlExecutor`。
3. **G6 只保证语句文本**。它能拦住「有人写了 `WHERE id = ?` 就交」这类最常见退化，
   但拦不住「写了 `created_at BETWEEN …` 却仍扫全分区」——所以第 6 节的运行时对照用例
   与 T-CL-11（`EXPLAIN` 只出现一个分区）必须同时存在。
