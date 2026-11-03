# 20261104 · 测试卫生：同库串跑 60+ 条假红的定位与分库跑法固化

> 卡：`t_e2a368f0`（【TEST-HYGIENE】同库串跑 changelog→flow→erp 时 erp 段假红）
> 结论：**不是业务回归，是测试隔离缺陷**。两条独立机理（同库数据残留 + 进程级 `set_db` 先到先得）、
> 一族"不可复现的传染性假红"，外加"同一个库连跑第二遍"才暴露的共享角色残留。
> 跑法已固化成 `scripts/verify/run_lib_tests_by_module.sh`；
> 说明文档：[`docs/测试卫生-真库单测分库跑法.md`](../../测试卫生-真库单测分库跑法.md)

## 文件

| 文件 | 内容 | 关键结论 |
|---|---|---|
| `60_shared_db_sequence_changelog_flow_erp.txt` | 同一库串跑 changelog → flow → erp 的**原始输出**（三段各一个进程，`--test-threads=1`） | changelog 51/0、flow 345/0、erp **838 passed / 2 failed**（`dc01`、`zz_teardown`）；卡 D 首轮同口径是 773/67 |
| `61_residue_inventory.txt` | flow 段跑完后，同一个库比干净库多出来的行（逐表比对 + 明细查询） | **27 张表**有残留；`biz_flow_def` +9（7 条 `PUBLISHED` 的 `p0fix_*`，`biz_type=PO_APPROVE`）、`biz_flow_instance` +15（3 条 `PO_APPROVE`/`PENDING`）、`sys_user` +43 |
| `62_setdb_firstwins_probe.txt` | `set_db()` 先到先得三连实验 | 整段 flow 一起跑：`p0fix_*` 定义落进 **ERP 库**，`P0FIX_DB_URL` 指向的库**一行没写**；把 `P0FIX_DB_URL` 指向**不存在的库**仍然 345/0 全绿；把 `internal_control` 单列进程跑，才正确地写进 P0FIX 库 |
| `63_data_residue_only_probe.txt` | 数据残留"单跑"对照（只跑一条/一组，各自独立进程） | `dc01`、`zz_teardown` 在污染库上**稳定红**；`stockin_handler_tests_a4` 在**同一份污染数据**上 13/13 全绿 → 首轮 67 条里那 13 条属"传染性假红" |
| `64_run_lib_tests_by_module_run1run2.txt` | `scripts/verify/run_lib_tests_by_module.sh` 全量跑：4 个模块 × 2 轮（**同一个库连跑两遍**） | changelog 51/0 ×2、flow_p0fix 7/0 ×2（泄漏校验：P0FIX 库 7 / 模块库 0）、flow 345/0 ×2、erp **840/0 ×2**，`EXIT=0` |
| `65_negative_selftest.txt` | 脚本负例自测 | ① 复用脏库 → preflight 点名「流程实例=15 待办=22 采购单=9 p0fix夹具定义=7 用户=44」并 `EXIT=1`；② 过滤器打不中用例 → `passed=0` 判 FAIL 并 `EXIT=1` |
| `66_contract_approval_same_db_second_round_fix.txt` | 「同一库连跑第二遍」红 2 条的定位与修复前后原始输出 | 根因：审批节点按角色展开，共享角色上残留 `purchase_handler` 的 `p3a_0003`（采购经理）/`p3a_0004`（总经理）夹具账号 → 一节点两条待办。修法：`contract_approval_tests::setup()` 摘掉非本模块绑定（与 `purchase_handler` 对称）+ 断言改按"级/人"判（并加严两条） |
| `raw_prefix_2failed/` | 修复前「同一库第 2 轮 838/2」的原始日志 | `contract_approval_tests` 两条：`left: 2 right: 1` |
| `raw/` | 脚本每轮 `cargo test` 原始输出 + 模板库迁移日志 | — |

## 本卡改了哪些代码（只有测试代码，不动业务）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/contract_approval_tests.rs` | ① `setup()` 摘掉共享角色（`PURCHASE_MANAGER`/`GENERAL_MANAGER`）上非本模块夹具的绑定；② 待办断言从"条数"改为"落到哪一级/谁"（`pending_tasks_of_fixtures`），并**新增**两条更严的断言（小额不该给总经理派待办、第二级必须落到总经理） |
| `scripts/verify/run_lib_tests_by_module.sh` | 新增：按模块分库 + 分进程跑、preflight 洁净断言、`flow_p0fix` 泄漏校验、轮次计数一致性、`passed=0` 判假绿 |
| `docs/测试卫生-真库单测分库跑法.md` | 新增：根因、复现命令、跑法、铁律、证据索引 |
| `docs/README.md`、`scripts/verify/README.md` | 文档入口与用法 |

## 复跑

```bash
RUNS=2 bash scripts/verify/run_lib_tests_by_module.sh        # 4 模块 × 2 轮（同一库连跑两遍）
MODULES="erp" RUNS=2 bash scripts/verify/run_lib_tests_by_module.sh
```

## 环境

- 库：本机 PostgreSQL 18（`127.0.0.1:5432`，owner `alun`），模板库 `cloudpivot_libtest_template`
  = 仓库 HEAD 的迁移全量 + 种子，`public` 表 102 张、`schema_migrations` 最大版本 **212**
  （`V0213` 是另一张卡未提交的工作，不在本卡树里）。
- 代码：干净克隆 `/root/t_e2a368f0/repo`，基线 HEAD = `b3d83df`（与 `main` / `origin(144 bare)` / `github` 一致），
  本卡只加测试代码 / 脚本 / 文档。
- 生产库（144）全程未被写入：本卡所有库都是本机一次性副本。
