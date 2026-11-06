# 全量 `cargo test --lib` 归因（SO-10）

## 跑法与口径

```bash
# 干净库（一卡一库）：先起实例做迁移，再跑全量
runuser -u postgres -- createdb -O alun cloudpivot_so10b
cd /opt/cloudpivot/backend
NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_so10b NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_SERVER_LISTEN=127.0.0.1:18094 NIK_ADMIN_PASSWORD='Admin@123456' \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) timeout 150 ./target/debug/cloudpivot
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so10b' \
  cargo test --lib --features test-hooks -- --test-threads=1
```

第一轮原始输出：`full-suite-round-1.txt`
**`test result: FAILED. 2768 passed; 2 failed; … finished in 1406.10s`**

对照：SO-09（本卡父卡）在同一口径下是 **2759 passed / 2 failed**；
本卡新增 9 条单测（`shipment_posting`）→ 2759 + 9 = **2768**，数字对得上，说明没有别的用例被吞掉或变红。

## 2 条失败逐条归因（都是**既有缺陷**，非本卡引入）

| # | 测试 | 归属 | 依据 |
|---|------|------|------|
| 1 | `business::erp::sales_approval_bridge::db_tests::three_tiers_start_flow_and_approve_to_approved` | 既有缺陷（SO-06 线） | 断言 `left: "" / right: "APPROVED"`（审批通过后订单状态没被回调写回）。SO-09 交接记录：**全量跑顺序下失败、单模块 9 passed**，属前序模块残留流程实例 / 待办导致的顺序污染。本卡没动审批桥、没动流程引擎 |
| 2 | `business::erp::stockin_handler::stockin_handler_tests_p16::p16_graph_snapshot_selfcheck_is_clean` | 既有缺陷（SO-06 线） | 失败文案点名 `flow_code: "so_approve"` 的流程节点行与 `graph_hash` 快照漂移（V0281）。SO-08 交接已登记「在任何库上常红，需新开迁移重算 hash」；本卡没碰 V0281、没新增任何流程 |

两条与 SO-09 的 `full-suite-attribution.md` **同因同点**（同一测试、同一断言、同一根因），
因此「全量 0 失败」在当前 main 上不可达；本卡的口径是：**本卡引入的失败为 0**
（9 条新用例全绿，且没有任何既有用例因本卡的改动翻红）。

## 本卡相关的定向回归（不受那两条影响，单独跑）

```bash
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_so10' \
  cargo test --lib --features test-hooks shipment_posting -- --test-threads=1
# => ok. 9 passed; 0 failed（两轮，见 unit-round-1.txt / unit-round-2.txt）
```

## 第二轮全量

同一干净库（`cloudpivot_so10b`）、同一命令的第二轮：`full-suite-round-2.txt`

```
test result: FAILED. 2768 passed; 2 failed; 0 ignored; … finished in 1398.73s
```

**两轮数字完全一致**（2768 / 2），失败的仍是同两条（`sales_approval_bridge` 顺序污染、
`p16_graph_snapshot_selfcheck_is_clean` 的 V0281 `so_approve` hash 漂移），
说明本卡改动稳定、且没有「第二轮才翻红」的顺序依赖问题。

## 顺带发现（SO-06 线第三条既有症状，非本卡）

`scripts/verify/biz_type_domain.py` 在带 V0281 的库上 FAIL：

```
[FAIL] ② 审批动作  biz_flow_def.biz_type  取值 15 种
       ✗ 域外值 'SO_APPROVE' × 1
```

`SO_APPROVE` 不在 `backend/src/business/shared/doc_kind.rs` 的 `COLUMN_DOMAINS` 词表里
（V0281 由 SO-06 引入，与本卡无关）。修它要么把值补进词表、要么改流程种子 —— 属 SO-06 线的
独立缺陷卡，建议与 `graph_hash` 漂移一起修。
