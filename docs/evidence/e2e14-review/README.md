# E2E-14 系统评审证据（2026-11-09 · 145 / cloudpivot_dev）

> 评审报告正文：`docs/review/20261109-系统全面评审.md`
> 采集口径：145 测试机（`cloudpivot-test`，`/health`=200，HEAD `849b58e0`）+ 干净库 `cloudpivot_fresh`
> 未连 144；未 DROP/TRUNCATE；未改已应用迁移；本卡自造的 1 万条字典项已按前缀清理（残留 0）

## 文件清单

| 文件 | 内容 | 怎么复跑 |
|---|---|---|
| `db_stats.txt` | 154 表 / 菜单 432 行（15 目录 + 96 页面 + 321 按钮）/ 其中 **2 个页面 path 为空** / 凭证 173 / 日志 8 158 | `sys_menu` + `information_schema` 直查（见报告 §3） |
| `db_health.txt` | 负库存 0、凭证不平 0、超 3 天中间态单据 0、超 3 天 PENDING 实例 0、**孤儿应收 5 张** | 同左 |
| `invariant_fresh.txt` | **干净库 80 项 78 绿**：仅 H6（种子漏 `is_system`，BLOCKER）+ N2（admin 无部门，WARN），5 项 INFO 跳过 | `NIK_DATABASE_NAME=cloudpivot_fresh python3 scripts/verify/erp_invariants.py` |
| `invariant_dev_failures.txt` | 演练库 80 项 **60 绿 / 20 红（12 BLOCKER + 8 WARN）** + 20 条失败项逐条明细 + 近 6 批稳定性 | 巡检看板 / `sys_invariant_result` |
| `perf_single_and_concurrency.txt` | 14 个只读接口 3 次取中位（登录 299 ms、列表 3–6 ms、资产负债表 14 ms、PDF 165 ms）+ **60 请求 / 20 线程：wall 74 ms、p95 28 ms、非 200 = 0** | `python3 /tmp/perf_probe.py`（脚本口径见报告 §8.3） |
| `perf_bigdata_10k.txt` | **1 万行字典项**：预检 537 ms、入库 804 ms、同步导出 869 ms（10 001 行无丢行）、分页 offset 9 950 仍 30 ms、**全量导出超 1 万 → 409/60015** | `cd scripts/verify && python3 e2e14_review_perf.py` |
| `menu_route_diff.txt` | 菜单 path ↔ 前端路由对拍：94 条有 path 的页面**全部有路由**；**2 条 path 空**；15 条路由无菜单（详情/编辑页属正常） | 见报告 §8 |
| `cargo_test_head_before_fix.txt` | 干净检出（`git archive HEAD`）`cargo test --lib`：**3 878 passed / 2 failed**（含两条失败详情） | `git archive HEAD \| tar -x -C /tmp/head` |
| `cargo_test_after_fix.txt` | 修复后：`system::authz_tests` 15/15、`shared::startup_guard` 26/26、**全量 3 880 passed / 0 failed** | 同左 + 本卡两个修复文件 |
| `clippy_head.txt` | `cargo clippy --all-targets -- -D warnings`：**3 处 error（exit 101）**：`invoice.rs:1903` / `receivable.rs:782` / `sales_order_model.rs:661` | `cd backend && cargo clippy --all-targets -- -D warnings` |
| `static_checks_full.txt` | `fmt --check`（HEAD 3 文件有 diff，本卡 2 文件干净）+ 全量单测 + clippy 的完整输出 | `scripts/check.sh` 的子集 |
| `frontend_typecheck.txt` | `pnpm typecheck` **RC=0** | `cd frontend && pnpm typecheck` |

## 本卡修复（2 个文件，均测试/门禁层，不改运行时行为）

1. `backend/src/system/authz_tests.rs`：权限注解扫描器支持**单行** `#[nik::permission(...)]`
   —— 早先只认多行块，源码里 **89 处**单行写法对 `o13_every_write_route_requires_permission`
   静默空跑，`o02_readonly_cannot_delete_role` 连 `DELETE /system/role/{id}` 都找不到。
2. `backend/src/shared/startup_guard.rs`：`merge_semantics_or_base_and_profile` 不再断言
   gitignored 的 `config-dev.toml`/`config-test.toml`（干净检出必红），改为「私有在则断言私有，
   不在则断言入库的 `*.example.toml`」。

修复后实测：`cargo test --lib` **3 880 passed / 0 failed**；两个文件 `rustfmt --check` 干净。
