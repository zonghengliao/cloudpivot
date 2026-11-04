# 并发自检证据：受影响 4 模块 `--test-threads=4` 连跑 3 次 + 残留重跑（卡 t_25926f63）

> 结论：**全部通过**。4 个模块在**同一个隔离库**上共 **28 次**并发运行（`--test-threads=4`），
> 累计 **1246** 条用例执行、**0 failed**；其中 Phase B 是「先完整跑一遍 → 立刻再连跑 3 次」
> 的残留场景，**库从不清空**，第 2 轮之后仍处处 0 failed。
> 唯一遗留问题（不影响本次验收）：`stockin_handler` 夹具每跑一轮在
> `biz_status_log(entity_type='PURCHASE_ORDER')` 留下 **35 条孤儿行**（线性累积，7 轮 → 245 条），
> 已建卡交后端（见 §6）。

- 执行时间：2026-11-04 20:00–20:57 CST
- 卡面验收对象：`period_items` / `contract_approval_tests` / `payable_bridge` / `stockin_handler`
- 执行人：test-enginner（本卡不写业务代码，只做自检）

---

## 1. 被测代码版本（可复现）

| 项 | 值 |
|---|---|
| 基线提交 | `88fe5489c2dd43c21afaabec4c9f1a2d509e314a`（t_bfa81333 的 zz_teardown 按 id 清场） |
| 叠加的未提交修复 | t_f404258c 的 5 个文件（夹具身份唯一化 / SEED_ROLE_LOCK） |
| 独立验证树 | `/root/25926f63-tree`（`git archive 88fe548` + 上述 5 文件 + gitignore 的 `backend/config/*.toml`） |
| 独立构建目录 | `/root/25926f63-target`（不污染共享 `target/`） |

叠加文件的 sha256 前 24 位（与共享工作树逐字节一致，已 `cmp` 校验）：

```
1a6c7d60be6a519b7eab3ce7  contract_approval_tests.rs
16f78823b2756aabbf657736  fixture_hygiene.rs
a949a24ea126576d9ec7a31e  period_items_tests.rs
8193d1f0fc7e220e85ada846  period_test_support.rs
833d64f13b5ad17b665c64d5  purchase_handler.rs
```

**为什么用独立树而不是直接在 `/opt/cloudpivot` 跑**：共享主仓当时还有**另一张卡未提交的 WIP**
（`opening.rs` / `payable_payment.rs` / `serial_handler.rs` / `voucher.rs` 的 settle_orphans 改造 +
前端 flow 编辑器改动）。那 4 个文件不属于两张修复卡的交付物，且随时可能被改动，
直接跑会让证据对应不上任何确定版本。独立树 =「两张修复卡交付态」的精确快照。

## 2. 库（同一个隔离库、全程未重建）

| 项 | 值 |
|---|---|
| 库名 | `cloudpivot_25926f63`（owner `alun`） |
| 建库方式 | `scripts/verify/erp_clean_db.sh`（`RECREATE=1`，用独立树的二进制自举一次） |
| 迁移指纹 | `schema_migrations` max V0219 / **163 条**，超管 1 人（会签人数基线） |
| 连接 | `SYS_DB_URL` = `ERP_DB_URL` = `TASK_DB_URL` = `postgres://alun:***@127.0.0.1:5432/cloudpivot_25926f63` |
| 是否中途清库 | **否**。Phase A → Phase B 之间无 drop/create/truncate；结束时 `schema_migrations` 仍是 163 条 |

## 3. Phase A —— 每个模块连跑 3 次（`--test-threads=4`）

命令模板（4 个模块各一条，连跑 3 次；库不清理）：

```bash
cd /root/25926f63-tree/backend
export CARGO_TARGET_DIR=/root/25926f63-target NIK_PROFILE=test
export SYS_DB_URL=$ERP_DB_URL TASK_DB_URL=$ERP_DB_URL   # ERP_DB_URL 指向 cloudpivot_25926f63
cargo test --lib business::erp::period_items            -- --test-threads=4
cargo test --lib business::erp::contract_approval_tests -- --test-threads=4
cargo test --lib business::erp::payable_bridge          -- --test-threads=4
cargo test --lib business::erp::stockin_handler         -- --test-threads=4
```

| 模块 | R1 | R2 | R3 |
|---|---|---|---|
| `period_items` | `ok. 13 passed; 0 failed` (22.87s) | `ok. 13 passed; 0 failed` (23.11s) | `ok. 13 passed; 0 failed` (22.98s) |
| `contract_approval_tests` | `ok. 9 passed; 0 failed` (13.30s) | `ok. 9 passed; 0 failed` (13.31s) | `ok. 9 passed; 0 failed` (13.31s) |
| `payable_bridge` | `ok. 23 passed; 0 failed` (45.39s) | `ok. 23 passed; 0 failed` (45.68s) | `ok. 23 passed; 0 failed` (45.27s) |
| `stockin_handler` | `ok. 133 passed; 0 failed` (425.24s) | `ok. 133 passed; 0 failed` (423.95s) | `ok. 133 passed; 0 failed` (420.57s) |

12/12 次全绿。

## 4. Phase B —— 残留场景（先完整跑一遍，再立刻连跑 3 次）

同一库、紧接着 Phase A 的残留继续叠加，不做任何清理。先按模块顺序各跑 1 次（`SEED`，故意留下夹具残骸），
随后每个模块再连跑 3 次（`R1..R3`）。

| 模块 | SEED（完整一遍） | R1 | R2 | R3 |
|---|---|---|---|---|
| `period_items` | 13 / 0 (23.57s) | 13 / 0 (23.21s) | 13 / 0 (23.17s) | 13 / 0 (23.12s) |
| `contract_approval_tests` | 9 / 0 (13.56s) | 9 / 0 (13.45s) | 9 / 0 (13.40s) | 9 / 0 (13.57s) |
| `payable_bridge` | 23 / 0 (45.58s) | 23 / 0 (45.63s) | 23 / 0 (45.72s) | 23 / 0 (45.14s) |
| `stockin_handler` | 133 / 0 (421.18s) | 133 / 0 (426.15s) | 133 / 0 (431.15s) | 133 / 0 (427.83s) |

16/16 次全绿。**第 2 轮之后（R2/R3）在没有清库的前提下仍 0 failed ⇒ 验证的是隔离，不是清空库。**

### 4.1 汇总（28 次运行，原始 `test result:` 行）

```
=== A（每模块 ×3）===
period_items            : ok. 13 passed / 0 failed   ×3
contract_approval_tests : ok.  9 passed / 0 failed   ×3
payable_bridge          : ok. 23 passed / 0 failed   ×3
stockin_handler         : ok. 133 passed / 0 failed  ×3
=== B（完整一遍 + ×3）===
period_items            : ok. 13 passed / 0 failed   ×4
contract_approval_tests : ok.  9 passed / 0 failed   ×4
payable_bridge          : ok. 23 passed / 0 failed   ×4
stockin_handler         : ok. 133 passed / 0 failed  ×4
------------------------------
合计 28 次运行 / 1246 条用例执行 / 0 failed / 0 panicked
```

逐条核对方式（不只看脚本自报）：28 个原始日志每个都恰好 1 行 `^test result:`、
0 行 `^test .* FAILED`、0 处 `panicked at`：

```
$ for f in *.log; do echo "$f result=$(grep -c '^test result:' $f) failed=$(grep -c '^test .* FAILED' $f)"; done | sort | uniq -c
     28 ...            # 全部 result=1 failed=0
$ grep -l "panicked at" *.log | wc -l
0
```

## 5. 库侧独立复查（psql，不经测试代码）

`biz_status_log` 的孤儿判据与 `fixture_hygiene::orphans` 完全一致
（`entity_id NOT IN (SELECT id FROM <单据表>)`）：

| 时点 | 已跑 stockin 轮次 | STOCK_IN | PAYABLE | VOUCHER | CONTRACT | ACCOUNTING_PERIOD | **PURCHASE_ORDER** | PENDING 实例 |
|---|---|---|---|---|---|---|---|---|
| Phase A 结束 | 3 | 0 | 0 | 0 | 0 | 0 | **105** | PAYABLE_PAY=1 |
| Phase B SEED 结束 | 4 | 0 | 0 | 0 | 0 | 0 | **140** | PAYABLE_PAY=1 |
| Phase B 结束 | 7 | 0 | 0 | 0 | 0 | 0 | **245** | PAYABLE_PAY=1 |

- 本卡 4 个模块**自己的**命名空间全部 0 孤儿（STOCK_IN / PAYABLE / VOUCHER / CONTRACT / ACCOUNTING_PERIOD），
  夹具号段 `2040-%` 期间行恒定 1 条、`AP-P17-%` 应付单恒定 1 条 —— 即「每轮 setup 清上一轮、
  跨轮次不累积」，与 t_bfa81333 的结论一致。
- `PAYABLE_PAY=1` 是最后一次运行最后一条用例的中间态产物，跨 7 轮不增长。
- **`PURCHASE_ORDER` 是唯一的线性泄漏**：每轮 stockin **+35 条**（105 = 3×35，140 = 4×35，245 = 7×35）。
  不影响任何断言（4 模块的 zz_ 收尾只复查本模块命名空间），但它是真残留，见 §6。

## 6. 自检发现的问题（不阻塞本次验收，已建卡）

**stockin 夹具删采购单时没删它的状态日志**：
`stockin_handler_tests_fixtures.rs:357` 按 `supplier_id` 删 `biz_purchase_order`，
但 `biz_status_log` 没有外键，`entity_type='PURCHASE_ORDER'` 的
`SUBMITTED→PARTIAL_RECEIVED` / `SUBMITTED→RECEIVED` 行（`entity_no = 'B1-xxxxxxxx'`）留了下来。
实测 245 条孤儿、`created_at` 分布在 20:02–20:57 各轮 stockin 运行期间，每轮 +35，**无上界**。

性质：**测试库卫生缺陷，不是产品缺陷**（PURCHASE_ORDER 之外的 6 类都被按 id 清场收干了）。
建议修法与 t_bfa81333 同款：删单据前先按**本夹具登记的 PO id 集合**删状态日志
（或给 stockin 的 teardown 增加 `PURCHASE_ORDER` 的 id 收敛式复查）。已建卡交 `backend-developer`。

## 7. 复现方式

```bash
# ① 独立树
mkdir -p /root/25926f63-tree && cd /opt/cloudpivot && git archive 88fe548 | tar -x -C /root/25926f63-tree
cp backend/config/config-{dev,test}.toml backend/config/config.toml /root/25926f63-tree/backend/config/
for f in contract_approval_tests.rs fixture_hygiene.rs period_items_tests.rs period_test_support.rs purchase_handler.rs; do
  cp backend/src/business/erp/$f /root/25926f63-tree/backend/src/business/erp/; done

# ② 建库（独立二进制自举一次，等 admin+SUPER_ADMIN 落库）
cd /opt/cloudpivot && ERP_DB=cloudpivot_25926f63 RECREATE=1 PORT=18093 \
  REPO=/root/25926f63-tree BIN=/root/25926f63-target/debug/cloudpivot \
  bash scripts/verify/erp_clean_db.sh

# ③ 跑矩阵（本目录 run_matrix.sh = 本次实际用的驱动脚本：Phase A ×3 + Phase B SEED+×3）
ERP_DB_URL=… bash docs/evidence/20261104-testfix-parallel/run_matrix.sh

# ④ 库侧复查
ERP_DB_URL=… python3 residue_check.py
```

本轮原始产物（未删减）：

- `run_matrix.sh` —— 驱动脚本（28 次运行的编排与判绿口径：rc=0 且 `0 failed` 且 `passed>0`）
- `summary.tsv` —— 28 行 `phase / run / module / passed / failed / verdict / 耗时 / 日志路径`
- `residue.txt` —— 3 个时点的库侧复查原始输出
- `driver.out` —— 驱动全量输出（含 `=== VERDICT === ALL_GREEN`、`DRIVER EXIT=0`）
- 28 个原始日志体积较大（每个含完整用例列表），未入库；可用 §7 在 30 分钟内复跑重现

## 8. 明确没做的事（避免误读为「已覆盖」）

- 未跑全量 `cargo test`（`-j 1` × 2 轮的收口是下一张卡 t_9a6e3d35 的范围）。
- 未跑跨模块**跨进程**并发（本卡只做单进程 `--test-threads=4`；跨进程并发证据见 t_bfa81333 的
  stockin ∥ payable_payment 一轮 133/0 + 29/0）。
- 未提交任何 `src/**` 改动；本卡只新增本文档与驱动脚本。
- 未清理 / 未修改共享主仓里别人的 WIP 文件。
