# E2E05-FIX-1 证据：补提历史折旧按「期间归属部门」记费用

> 卡：`t_abb4bb0b`（E2E05-FIX-1【P3】）· 2026-11-09 · 后端改动 · 期间 145 测试机验证
> 远端 HEAD：`36c8bdb1`（代码 `6337443f` / `4fcdc5ec` / `a84a141d` / `36c8bdb1`）

## 一句话结论

`run_batch` 汇总折旧借方不再用卡片**当下**的 `use_dept_id`，而是按 `biz_fixed_asset_change`
的 TRANSFER 留痕还原**该期间期末归属部门**：期末（含）前最后一次转移取**转入**部门、
期末之后才转移的取**转出**部门、从未转移的用卡片当前部门（行为不变）。
真机（145）实测：A 台 2031-02-01 从设备部转采购部之后补跑 2031-01，凭证借方 1 900
记**演练设备部**（修前记采购演练部）；2031-02 记采购演练部。

## 问题回顾（E2E-05 演练 Q3）

```
2028-02-01  A 台 设备部 → 采购部
事后补跑 2028-01 折旧
→ 2028-01 凭证借方 1900 记 采购部（应为设备部）
→ 5622.01 的 DEPT 辅助核算 / 部门损益报表整月偏
（按月准时计提不触发，只在补提历史期间冒出来）
```

## 改了什么（3 个代码文件 + 1 个测试文件 + 2 个验证脚本）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/asset_model.rs` | 新增 `DeptSnapshot` / `DeptAtPeriod` / `dept_at_period_hint`：**两次**批量 `DISTINCT ON` 查询（不是每台一次，无 N+1）取「期末含前最后一次转移的转入部门」与「期末后第一次转移的转出部门」；名字优先用留痕快照，为空再 join `sys_dept`（不过滤 `deleted_at`），再空给 `部门 <id 前 8 位>` 占位名 |
| `backend/src/business/erp/depreciation_model.rs` | ① `compute_detail` 的部门改成**显式入参**（期间归属部门），不再读 `asset.use_dept_id`；② `aggregate_debits` 聚合键 `(科目, 部门名)` → `(科目, 部门 id)`；③ `run_batch` ③′ 步批量取 hint 后逐台 `resolve`，与卡片当前部门不同时打 INFO 日志；④ 模块文档写明补提口径 |
| `backend/src/business/erp/depreciation_dept_tests.rs`（新） | 真库用例 2 条（`ERP_DB_URL` 开关）：补提历史月记旧部门 / 次月记新部门 / 无转移资产不变 / DEPT 辅助账按期间正确 / 期间内转移整月算新部门 |
| `backend/src/business/erp/mod.rs` | 注册上面的 `#[cfg(test)] mod` |
| `backend/tests/linkage/voucher_chain.rs` | 顺手修一个**我自己**上一张卡（`bf3f7c8f` E2E04-FIX-1）留下的编译回归：`ManualEntryReq` 加了 4 个 aux 字段，这个集成用例没跟着改 → `cargo check --tests` E0063 |
| `scripts/verify/e2e05_asset_b.sh` | 把演练里**钉住 bug 现场**的两条断言翻到修复后口径（2028-01 借 1 行 / 设备部 7204.1667 / 采购部 0 行 / A 明细部门=设备部）；2028-02（A 已转）的断言保持原样 |
| `scripts/verify/e2e05fix1_dept_at_period_e2e.sh`（新） | 145 真 token curl 验收脚本（期间 2029/2030/2031 可用 `YEAR=` 选，夹具按年度分套 ⇒ 幂等） |

### 边界口径（与卡面 ③ 逐条对应）

| 场景 | 取哪个部门 | 判定 |
|---|---|---|
| 期间内转移（如 2/15 转） | **新**部门（整月算新） | `change_date <= period_end` 的最后一次 TRANSFER 的**转入**部门 |
| 期间之前转移 | 新部门 | 同上 |
| 期间之后才转移 | **旧**部门（= 那次转移的**转出**部门） | `change_date > period_end` 的第一次 TRANSFER 的**转出**部门 |
| 从没转移 | 卡片当前 `use_dept_id` | 保持原行为，无转移资产零影响 |

> 「期间内转移整月算新部门」是**费用归属**口径；与「当月减少当月照提」的**资产**口径
> 不是一回事（后者决定提不提，前者决定记哪个部门），两条在代码里分别落在
> `scan_assets_for_period`（提不提）与 `dept_at_period_hint`（记给谁）。

## 验收对照（卡面 ①~⑥）

| # | 卡面要求 | 结果 | 证据 |
|---|---|---|---|
| ① | 造场景：A 台 2028-01 在设备部，2028-02 转采购部 | ✅ | 145 真机：`E2E05FIX1-2031 A 台` 建卡在演练设备部 → 2031-02-01 转采购演练部（转移留痕 `演练设备部->采购演练部\|2031-02-01`，卡片当前部门已变采购） |
| ② | 补跑历史月 → 凭证借方记**旧**部门 | ✅ | 145：2031-01 批次明细 A 的 `dept_name = 演练设备部`；凭证「设备部行 = 设备部明细聚合 2137.50」且 ≥ A 的 1900；批次详情接口同值 |
| ③ | 跑次月 → 记**新**部门 | ✅ | 145：2031-02 A 明细 `dept_name = 采购演练部`，且该期 A 无设备部明细 |
| ④ | 无转移的资产行为不变 | ✅ | 145：B 台两个月都是采购演练部 / 950；本机真库用例同样断言 |
| ⑤ | 部门损益（DEPT 辅助账）按期间正确 | ✅ | 145 `GET /business/erp/ledger/aux-balance?aux_type=DEPT…`：设备部 2031-01 = 设备部明细聚合、采购部 2031-02 = 采购部明细聚合（报告与凭证同源） |
| ⑥ | cargo test + push | ✅ | `unit_and_realdb_tests.log`（20 passed）、`asset_suite.log`（61 passed）；远端 HEAD `36c8bdb1`（`git ls-remote` 复核） |

## 证据文件

| 文件 | 内容 |
|---|---|
| `unit_and_realdb_tests.log` | `cargo test --lib business::erp::depreciation`（真库）：**20 passed / 0 failed** —— 8 条纯逻辑单测 + 2 条新真库用例 + LNK-10 联动 5 条 + 其余 |
| `asset_suite.log` | `cargo test --lib business::erp::asset`（真库，回归）：**61 passed / 0 failed** |
| `negative_control_old_behavior.log` | **防假绿负向对照**：把 `run_batch` 临时改回 `asset.use_dept_id` → 主用例**红**（`A 台 2048-01 的折旧必须记「设备部」: left 0 / right 1583.3333`，即钱跑到采购部）；改回后转绿 |
| `145_e2e_2031_fresh.log` | 145 真 token curl 首次跑：**PASS=57 FAIL=0**（覆盖 ①~⑤） |
| `145_e2e_2031_rerun.log` | 同年度重跑（幂等）：**PASS=48 FAIL=0**（转移跳过、批次 EXISTS 复用，断言仍全绿） |

## 其它门禁

- **前端**：本卡没有前端改动（补提口径是后端行为，页面上部门名本来就从凭证/明细读），
  仍按要求跑了 `pnpm typecheck`（**exit 0**）与 `pnpm build --outDir dist-e2e05fix1`
  （产物含 `index.html`，构建成功；产物目录是临时目录，已验证后移出工作区，**没有动
  `frontend/dist`**，避免影响别人的发布）。
- **迁移**：本卡**没有新增/修改任何迁移**（`biz_fixed_asset_change` 的
  `idx_biz_fa_chg_asset (asset_id, change_date DESC)` 就是本次查询要用的索引），
  已应用的迁移一行没动。

## 145 部署（测试机，未碰 144 生产）

```
git fetch origin && git merge --ff-only origin/main   # e9270c9b → a84a141d（后到 36c8bdb1）
cargo build --bin cloudpivot                          # 1m52s
systemctl restart cloudpivot-test                     # 新 PID 1381012
/health → 200；二进制 mtime 20:25:58；HEAD a84a141d
```

脚本与证据提交后在 145 上 `ff` 到 `36c8bdb1` 并重跑一次（幂等），后端二进制无需重建
（`36c8bdb1` 只改脚本）。

## 需要读者知道的三件事（诚实说明）

1. **共享库里有 E2E-05 演练遗留资产，会被顺带计提**。折旧批次是**全库按期间**的
   （`run_batch` 不区分夹具归属）。145 上 2029-01 实测批次 5 台 = 本卡 A/B + E2E-05
   遗留的 `E2E05 设备A/D/E`（1900 / 237.5 / 158.33），所以脚本一条绝对金额都不断言，
   全部改成不变式：**逐资产明细断言部门归属** + 「凭证部门行 = 该部门明细聚合」+
   「借方合计 = 贷方 1602 = 明细合计」+「辅助账本期借方 = 该部门明细聚合」。
   这些遗留资产因本卡验收在 2029/2030/2031 各多提了 1 个月（重跑 E2E-05 演练前先跑
   `scripts/verify/e2e05_reset.sql` 即可归零；没有删任何他人数据）。
2. **`cargo clippy --all-targets -- -D warnings` 在本仓 HEAD 就不绿**（4 处，全部在
   本卡未触碰的文件：`attachment/model.rs`、`erp/invoice.rs`、`erp/receivable.rs`、
   `erp/sales_order_model.rs`）。本卡改动文件按 `--message-format=short` 过滤为 **0 条**
   告警（`grep -c` 结果见卡内汇报），没有顺手改别人的文件。
3. **`scripts/verify/e2e05_asset_b.sh` 的断言口径变了**：它原来刻意钉住 bug 现场
   （「补提按当前部门」），现在钉的是修复后口径。要在 145 上重跑整段 E2E-05 演练，
   必须先 `bash scripts/verify/e2e05_run_all.sh`（里含 reset），否则 2028-01 批次已存在会
   走幂等闸。

## 复现命令

```bash
# 本机真库（干净库 = 迁移跑完的空库）
ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/cloudpivot_dept2048" \
  cargo test --lib business::erp::depreciation -- --test-threads=1

# 145 真机（真 token curl）
ssh root@172.18.10.145 'YEAR=2031 bash /opt/cloudpivot/scripts/verify/e2e05fix1_dept_at_period_e2e.sh'
```
