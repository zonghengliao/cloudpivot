# 关于 `34_check_sh_run.txt` 的说明（两次运行，必须一起读）

## 运行 1（复核过程中，2026-11-03 23:38 左右，工作树还没被别的卡片弄坏）

`bash scripts/check.sh` **端到端 exit 0**，末尾关键行（当次 stdout 的尾部，工具调用里原样返回）：

```
==> 批次B 闸①：分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）
  PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
闸② 判定面（v2）：扫描生产面 313 个文件 / 跳过测试面 50 个文件
  PASS：扫描范围已覆盖 backend/src/{business/task,business,system,support,shared}
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
G5 PASS：检查了 29 个调用点（changelog::record / changelog::record_in），全部有事务证据（扫了 363 个文件）
==> C7 G6：按 id 查 sys_change_log 必须同时带分区键 created_at（R14）
  PASS：0 处
==> C7 门禁汇总：G1–G6 共 6 条，未通过 0 条
==> 偏好字典与色板一致性（V0091 ui.palette oneof ↔ style/palettes.ts）
  PASS：22 套色板与 V0091 的 ui.palette oneof 逐字一致
==> 闸③：ERP 不变量（scripts/verify/erp_invariants.py）
  ⚠ 没有库口令（NIK_DATABASE_PASSWORD / PGPASSWORD 均为空）→ 视作「无 DB 环境」，跳过
==> 全部检查通过
```

（该次运行的前端 `pnpm typecheck`、`cargo fmt/clippy/check` 也都在 `全部检查通过` 之前跑完了，
`check.sh` 是 `set -euo pipefail`，任一编译关卡失败都不会走到最后的门禁段。）

## 运行 2（`34_check_sh_run.txt`，23:47，EXIT=1 —— **不是本卡复核对象的回归**）

```
==> 后端：cargo fmt --check
Error writing files: failed to resolve mod `cancel_cascade_tests`:
  /opt/cloudpivot/backend/src/business/erp/cancel_cascade_tests.rs does not exist
EXIT=1
```

原因：复核期间**另一张卡片**往共享工作树里放了未跟踪文件
`backend/src/business/erp/cancel_cascade.rs`，它 `mod cancel_cascade_tests;` 但测试文件还没写。
这是并行施工的中间态，与卡 A（只改 scripts/docs）与卡 B（只加 25 行注释）无关，
在 `git archive 2a9db18` 的干净树上不存在（见 `32_gate_runs_on_committed_tree.txt`：干净树 361 个 `.rs`，
两条门禁均 exit 0）。

## 归属

- 编译类三关（fmt / clippy / check）与前端 typecheck 的**干净树**复跑是**卡 D（t_450ee222）**的卡面要求（它会从 bare 仓库 clone 干净树）。
- 本卡（静态复核）交付的是：干净树上两条门禁的实测（§1 / `32_*.txt`）+ 自建负例 + 绕过测试 + 豁免抽检 + 手法检查。
- 两次 check.sh 运行都如实登记于此，不挑选、不隐瞒。
