# P2-1C · erp/transfer_handler.rs 巨型文件拆分（纯搬迁）

卡片：`t_33cf3073`　|　类型：纯 refactor（零行为变更）　|　执行日：2026-11-05

## 0. 结论先说

`backend/src/business/erp/transfer_handler.rs` **4676 行 → 114 行 facade + 13 个部件文件**
（6 个代码部件 + 7 个测试部件），最大文件 **885 行**（全部 < 1200）。调用方路径零改动
（`erp::transfer_handler::xxx` 一行没动，`mod.rs` 只加 11 行 `mod` 声明）。

四项验收逐条对照：

| 验收项 | 结果 | 证据 |
|---|---|---|
| 拆成 ≥4 个文件，每个 <1200 行 | **PASS**：14 个文件（facade 1 + 代码部件 6 + 测试部件 7），最大 885 行 | `line_counts.txt` |
| 行为零变化（全量测试对比） | **PASS**：`verify_move` 四重多重集全等；`business::erp::transfer_handler` 56 条用例在拆分前后**逐条同名同结果**（56/56，两轮 56 passed/0 failed）；`linkage::transfer_chain` 12 条链路用例拆分前后逐条同名同结果（12/0 vs 12/0） | `verify_move.txt`、`baseline_tests.txt` vs `split_tests_round1.txt`/`round2.txt`、`test_names_compare.txt`、`linkage_transfer_chain_*.txt` |
| 调拨在途→完成链路实测仍正确 | **PASS**：真机隔离实例 + 真库跑 W3c 调拨 e2e，拆分后 **PASS=59 FAIL=0**，与拆分前二进制断言集合逐条一致（`PENDING → APPROVED → IN_TRANSIT → DONE`，库存双写 / 序列号跟随 / 成本原价转移 / 金额守恒 / DONE 幂等 409 / 结算失败整笔回滚） | `e2e_chain_split.txt`、`e2e_chain_base.txt`、`e2e_chain_compare.txt` |
| git commit（纯 refactor） | **PASS**：只含 `erp/{transfer_handler.rs, transfer_handler_*.rs, mod.rs}` + `scripts/refactor/{verify_move.py, plans/transfer_handler.json}` + 本证据目录，无任何逻辑改动 | `git show --stat` |

> 卡面写 4576 行，实测拆分前 **4676 行**（`git show HEAD:backend/src/business/erp/transfer_handler.rs | wc -l`）——卡面数字过期，其余口径一致。

## 1. 拆了哪些文件（职责）

| 文件 | 行数 | 职责 |
|---|---:|---|
| `transfer_handler.rs`（facade） | 114 | 模块文档（7 接口表 / 审批接入 / 数据权限 / 库存双写）+ `pub use` 重导出 5 个代码部件 + `#[path]` 挂 7 个测试部件 |
| `transfer_handler_common.rs` | 296 | 底座：错误出口（409/404/写库映射/结算错误映射）、入参解析、`operator_of`、明细预处理 `prepare_items`（行校验 + 单位与序列号） |
| `transfer_handler_create.rs` | 321 | **建单** / 复制 / 改单：取号 `TK-yyyyMMdd-000001` + 明细落库 + 草稿态校验 |
| `transfer_handler_query.rs` | 218 | 只读入口：分页 / 详情 / 涉及序列号分组 / 在途超期汇总 |
| `transfer_handler_status.rs` | 420 | **六态状态机**（审核 PENDING / 发出 APPROVED→IN_TRANSIT / 在途 IN_TRANSIT / 收货 DONE（含结算）/ 作废 CANCELLED）+ 审批发起与回调上下文 |
| `transfer_handler_delete.rs` | 61 | 软删（仅 `DRAFT` / `CANCELLED`；与「作废」区分：只做删除标记） |
| `transfer_handler_test_support.rs` | 57 | 测试支撑：走生产审批路径把审批实例批到终态（`#[cfg(test)]`） |
| `transfer_handler_tests.rs` | 82 | 单测 2 条（不连库）：权限码导出、审批冲突 409 文案 |
| `transfer_handler_db_tests_fixtures.rs` | 885 | 真库夹具：连库 / 串行锁 / 造单小工具 / 清理（两套真库用例共用） |
| `transfer_handler_db_tests_create.rs` | 329 | 真库：建单 / 改单 / 复制 / 校验（10 条） |
| `transfer_handler_db_tests_status.rs` | 845 | 真库：六态状态机 + 审批发起与回调（17 条，含收尾清理 `zz_teardown_purges_fixture_residue`） |
| `transfer_handler_db_tests_settle.rs` | 844 | 真库：在途→收货结算（21 条：库存双写 / 成本转移 / 序列号 / 幂等 / 回滚） |
| `transfer_handler_db_tests_books.rs` | 278 | 真库：库位账 = 成本账不变式（4 条） |
| `transfer_handler_db_tests_query.rs` | 102 | 真库：分页 / 详情 / 序列号分组（2 条） |

`backend/src/business/erp/mod.rs` 追加 11 行（6 个代码部件 + 1 个 `#[cfg(test)]` 测试支撑部件的 `mod` 声明）。
声明顺序 = facade 的 `pub use` 顺序（底座 → 建单 → 软删 → 查询 → 状态机）。

## 2. 怎么证明「只搬不移」

`scripts/refactor/verify_move.py`（P2-1A/B 建立，本卡加一条 transfer 配方 + 修一处部件文件名）：

```
python3 scripts/refactor/verify_move.py HEAD transfer_handler
  条目多重集         前=  148 后=  148  MISSING=none  ADDED=none
  SQL 字面量         前=   88 后=   88  MISSING=none  ADDED=none
  用例名             前=   56 后=   56  MISSING=none  ADDED=none
  代码 token 多重集  前=26666 后=26666  MISSING=none  ADDED=none
结论： PASS
```

口径：忽略**注释行 / use 语句整块 / `mod` 声明与花括号 / `#[path]` / `#[cfg]`**，其余逐 token 比对
—— 即「没有一行函数体被改过」的机械证明。

## 3. 测试对比（拆分前 vs 拆分后，同一台机器、同一套库配方）

真库用例（`ERP_DB_URL`）：`cargo test --lib business::erp::transfer_handler -- --test-threads=1`

| 运行 | 库 | 结果 |
|---|---|---|
| 拆分前（HEAD 单文件，worktree `/root/p21c-base`） | `cp_p21c_base`（192 条迁移） | **56 passed / 0 failed**（142.55s） |
| 拆分后 第 1 轮 | `cp_p21c_split`（192 条迁移） | **56 passed / 0 failed**（145.83s） |
| 拆分后 第 2 轮 | 同一个库（验可重跑） | **56 passed / 0 failed**（145.80s） |

逐条比对（`test_names_compare.txt`）：拆分前 54 条在 `db_tests::` + 2 条在 `tests::`；
拆分后散到 `db_books 4 / db_create 10 / db_query 2 / db_settle 21 / db_status 17 / tests 2`
—— **用例名 56/56 一一对应，结果全 ok，无 MISSING / 无 ADDED / 无结果不同**。

集成链路用例（`LINKAGE_DB_URL`，`cloudpivot_test` 派生库）：`cargo test --test linkage transfer_chain -- --test-threads=1`

| 运行 | 结果 |
|---|---|
| 拆分前 | **12 passed / 0 failed**（72.62s） |
| 拆分后 | **12 passed / 0 failed**（81.40s） |

`diff` 两边 `test linkage::transfer_chain::*` 逐行相同（`linkage_transfer_chain_compare.txt`）。

编译门禁：`cargo check --lib --tests` EXIT 0 / 0 warning（`check_lib_tests.txt`）；
`cargo clippy --lib --tests -- -D warnings` **EXIT 0 / 0 条 error+warning**（`clippy_lib_tests.txt`）；
`rustfmt +1.94.1 --edition 2024 --check` 对本卡全部 14 个文件 EXIT 0。

## 4. 在途→完成链路实测（真机 + 真库）

配方（见 `e2e_runner.sh`）：`dropdb/createdb` 建干净库 → 用被测二进制起隔离实例（profile=test，
迁移随启动跑到最新）→ `bash w3c_transfer_settle_p21c.sh`（`W3C_BOOTSTRAP=0` 复用实例）。

拆分后（`e2e_chain_split.txt`，端口 18116，库 `cp_p21c_e2e_split`）关键断言：

```
[PASS] 提交后真的发起了 STOCK_TRANSFER 审批实例        [PASS] 审批回调把调拨单推到 APPROVED
[PASS] 发货 IN_TRANSIT（200）                        [PASS] 发货不动库存（结算在 DONE）
[PASS] 收货 DONE（触发库存双写 + 成本转移）（200）      [PASS] 单据终态 DONE
[PASS] 分仓 qty 5 → 0 / 总仓 qty 2 → 7             [PASS] 总仓 avg_cost = 90/7（移动加权）
[PASS] 5 个序列号 warehouse_id 全部到总仓（5）         [PASS] 流水 2 条（OUT TRANSFER_OUT + IN TRANSFER_IN）
[PASS] 金额守恒（OUT − IN = 0）                      [PASS] 明细回填 shipped/received/原价/金额 + 单据头金额
[PASS] DONE 之后重复推 DONE → 409，库存/流水不动（幂等）
[PASS] 序列号不符 / 库存不足 → 400/409 且单据仍 IN_TRANSIT、无流水（整笔回滚）
==> 汇总：PASS=59 FAIL=0
```

拆分前二进制同配方：同样 **PASS=59 FAIL=0**；两边断言集合（去掉 uuid / 单号等动态值后）**逐条一致、无增无减**
（`e2e_chain_compare.txt`）。

## 5. 偏差与说明（逐条）

1. **卡面行数过期**：卡面 4576 行，实测拆分前 4676 行。
2. **「审核 / 发出 / 在途 / 收货 / 作废」没有各切一个文件**：这五个目标态全部由**同一个函数**
   `change_transfer_status`（`transfer_handler_status.rs`，含 `TransitionContext` / `apply_transition` /
   `prepare_start` / `transfer_form_data`）实现 —— 它是一张 `match 目标态` 的状态机，共享前置校验、
   CAS 更新与审批闸。按目标态切成 5 个函数**必然改动函数体与控制流**，与「纯搬迁不改逻辑」冲突，
   故整条状态机保留在一个部件里（420 行，远小于 1200 行）。建单 / 查询 / 软删各自成部件。
3. **测试部件文件名统一 `db_tests_*`**：夹具模块由 `db_fixtures` 改名 `db_tests_fixtures`，
   与 `verify_move.py` 的部件清单一致（只是模块名，测试路径 `db_create::` 等不变，用例名 56/56 未变）。
4. **e2e 脚本的夹具补丁未入库**：仓库内 `scripts/verify/w3c_transfer_settle.sh` 的夹具只造了
   `biz_inventory`（成本账），没造 `biz_inventory_location`（库位账），而 `transfer_settle` 有
   「库位账 = 成本账」不变式 ⇒ 收货 DONE 必被拦下。本卡只在**证据副本** `w3c_transfer_settle_p21c.sh`
   里补了 11 行夹具（其余逐字节相同），未改仓库脚本（超出纯 refactor 范围）。
   建议另开卡修 `scripts/verify/w3c_transfer_settle.sh` 的夹具。
5. **只跑受影响模块**（按卡面要求）：`business::erp::transfer_handler` 56 条 + `linkage::transfer_chain`
   12 条 + 全库 `check/clippy`（编译面）+ 调拨 e2e；未跑无关模块的全量回归。
6. **clippy 在共用工作区跑**：`/opt/cloudpivot` 当时其它卡的在途改动也已并入 crate 编译，
   clippy 结果 0 warning / 0 error（含我这批文件的 0 条）。

## 6. 复现命令

```bash
# 保真
python3 scripts/refactor/verify_move.py HEAD transfer_handler

# 真库用例（两轮）
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_p21c_split' \
  cargo test --lib business::erp::transfer_handler -- --test-threads=1

# 集成链路
LINKAGE_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_test_p21c' LINKAGE_REQUIRE_DB=1 \
  cargo test --test linkage transfer_chain -- --test-threads=1

# 在途→完成 e2e（起隔离实例 + 干净库）
bash docs/evidence/20261105-p2-1c-transfer-handler/e2e_runner.sh 18116 cp_p21c_e2e_split \
  /opt/cloudpivot/backend/target/debug/cloudpivot split
```
