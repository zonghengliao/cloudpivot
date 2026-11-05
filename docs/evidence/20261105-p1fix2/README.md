# P1-FIX-2：出库库存不足时把「拆行指定库位」甩给用户，无 FIFO 自动分配

- 卡：`t_85e8e7fc`（P1-FIX-2，来源：`docs/评审报告-现有功能评分-20261104.md` §P1-2 / 操作便捷性 3 分主因）
- 日期：2026-11-05
- 环境：本机（172.18.10.99）隔离库 `cloudpivot_p1fix2_a`（真机 HTTP）+ `cloudpivot_p1fix2_b`（真库单测），
  隔离实例端口 **18115**（`ALUN_PROFILE`→`NIK_PROFILE=test`，独立库，**没有**动生产 144；
  18095 被另一张卡的在途实例占着，所以本卡用 18115 —— 见 `p1fix2_instance.sh`）
- 基线：`main` @ `ed81995`

---

## 0. 一句话结论

出库的 FIFO 以前只在**过账**时用，而且只挑「单个够量」的库位：货分散在 2 个库位、加起来够，
也会 409 让用户「把这一行拆成多行并分别指定库位」。本卡把它改成：

1. **建单 / 改单就按 FIFO 预填库位**（`stock_out::fifo_plan` + `allocate_fifo`）；
2. 跨库位够量 → **系统自动拆成 N 行**（每行一个库位账行），过账一次走通，用户不用手工拆；
3. **手填 `location_id` 覆盖自动值**（原样尊重、只扣那一桶）；
4. 真不够货时，详情 `stock_warnings` 与过账 409 **都说清「差多少 / 现有货在哪些库位 / 该怎么办」**。

---

## 1. 评审证据（旧文案原文）

`scripts/verify/audit_features_20261104/05_chains_run3.txt` ③ 段：

```
[PASS] 负库存出库被拦截（非 200）   HTTP 409 库存不足，过账已拒绝：物料 M-W1A-001 原料A（种子）
 在仓库 WH01（总仓（种子））本次要出库 99999.0000，没有任何一个库位够量（按库位建账时间 FIFO
 也挑不出来）；当前各库位可用：未分配: 8.0000。请把这一行拆成多行并分别指定库位，或先补货
```

---

## 2. 根因（代码位置）

| 位置 | 旧行为 |
|---|---|
| `stockout_handler.rs::plan()` | 建单只把用户传的 `location_id` 存下来，**不指定就留空**，FIFO 完全不参与建单 |
| `stockout_handler.rs::post_stock_out()` ③ | 只有「明细没库位」时才调 `fifo_location_row`，而它 `WHERE qty >= 本次数量` —— 只挑**单个够量**的桶 |
| 同上 `None` 分支 | 一个桶都不够 → 409「请把这一行拆成多行并分别指定库位」← **把系统该做的活甩给用户** |
| `stockout_model.rs::location_availability()` | 只回 `(库位, 数量)` 列表，不含缺口，文案也点不出「差多少」 |

---

## 3. 修法（改了哪些文件 / 函数）

### 3.1 `backend/src/business/erp/stockout_model.rs`（数据层）

| 新增 / 改动 | 作用 |
|---|---|
| `struct FifoBucket { location_row_id, location_code, available }` | 一个库位桶的快照；`None` = 「未分配」桶（与成本账 `location_id IS NULL` 同口径） |
| `struct FifoPick { location_row_id, location_code, take }` | 拣货一行（从哪个桶取多少） |
| `struct FifoPlan { buckets, picks, available, shortfall }` + `is_complete()` / `availability_text()` | 一条明细的分配结论 + 可读摘要（`A-01-03: 30、未分配: 20`） |
| `fn allocate_fifo(buckets, qty) -> (picks, shortfall)` | **纯函数**贪心分配：FIFO 序依次取、取满即停，缺口如实返回（无 DB 依赖，单元测试直打） |
| `async fn fifo_buckets(ex, material, warehouse)` | 该物料该仓**可拣**的桶（`qty > 0`），`ORDER BY created_at ASC, id ASC`（FIFO 键不变） |
| `async fn fifo_plan(ex, material, warehouse, qty)` | 桶列表 + 贪心分配 + 合计 / 缺口 |
| `async fn location_label(ex, row_id)` | 库位账行 id → 库位编码（「该库位不够」的文案用） |
| `fn describe_buckets(buckets)` | 分布摘要唯一拼接口径（过账 409 与详情提示共用） |
| **删除** `location_availability()` | 只回数量、点不出缺口；被 `fifo_plan` 取代 |

### 3.2 `backend/src/business/erp/stockout_handler.rs`（编排）

| 函数 | 改动 |
|---|---|
| `plan()`（建单 / 改单共用） | ① 手填 `location_id` → 原样保留并登记「已预占桶量」；② 没填 → `fifo_plan` 后按 `allocate_fifo` **拆成 N 行**（够量时）；③ 不够量 → 保持单行未指定（草稿可存）。同一张单多行同物料按 `(物料, 库位账行)` 记账，**不重复分配同一桶** |
| `post_stock_out()` `None` 分支 | 文案重写：`第 N 行要出库 X，该仓可用合计 Y，差 Z；现有库存分布：A: 30、B: 20（FIFO 序）…` + 三条处理办法（补货 / 改数量 / 重新保存草稿自动拆行） |
| `post_stock_out()` 逐桶负库存闸 | 文案加：`库位 <编码>` / `该库位可用 X` / `差 Z` / `该物料在本仓的全部可用（FIFO 序）` / 「清空本行库位让系统按 FIFO 自动分配」 |
| `detail_of()` + 新增 `stock_warnings()` | 草稿 / 待过账的详情返回 `stock_warnings`：逐行比「要出的量 vs 这一行能拿到的量」，够量不出提示，不够就点名缺口与库位分布（**只提示不拦截**） |
| `fifo_location_row` 文档 | 改为「老单据 / 手清库位」的兜底路径，正常路径用不到了 |

### 3.3 `backend/src/business/erp/stockout_dto.rs`

`StockOutDetailRes` 新增 `stock_warnings: Vec<String>`（`of()` / `detail_of()` 各加一个入参）。
建单 / 改单 / 详情 / 状态流转四个接口都返回详情，所以**建单成功的那一刻前端就能看到提示**。

### 3.4 测试与文档

- 新增 `backend/src/business/erp/stockout_fifo_tests.rs`（14 条：5 条纯函数 + 9 条真库），在 `stockout_model.rs` 里注册为 `fifo_tests`；
- `docs/ERP-库位策略.md` §0 表格 / §3 整节改写（旧口径「都不够就 409 让用户拆行」已作废），§6 验收表补一行。

---

## 4. 验收逐条对照

| 卡面验收项 | 结果 | 证据 |
|---|---|---|
| 建单时库位自动预填（FIFO） | ✅ | `fifo_create_prefills_single_location`；e2e A 段建单返回明细第 1 行即最早建账的 A 库位 |
| 用户可手改（覆盖自动值） | ✅ | `fifo_manual_location_overrides_auto`（指定后建的 B → 只扣 B，A 一分未动）；e2e B 段 |
| 库存不足提示清晰（前后文案对比） | ✅ | 见下方 §5；e2e C / D 段（`stock_warnings` + 409 原文） |
| 真机：出库一个跨库位物料，不再要求手工拆行 | ✅ | e2e A 段：A 30 + B 30，领 60 → 建单 **2 行**（A 30 / B 30）→ `PATCH status=POSTED` **HTTP 200**，两桶清零 |
| 单测 ≥6 条（FIFO 顺序 / 部分库存 / 完全不足 / 手改覆盖） | ✅ **14 条** | `fifo_allocate_takes_earliest_bucket_first`、`fifo_allocate_exact_sum_has_no_shortfall`、`fifo_allocate_reports_shortfall`、`fifo_allocate_without_stock_is_all_shortfall`、`fifo_allocate_keeps_four_decimals`、`fifo_create_prefills_single_location`、`fifo_create_auto_splits_across_locations_and_posts`、`fifo_split_follows_creation_order`、`fifo_manual_location_overrides_auto`、`fifo_insufficient_stock_warns_then_rejects_post`、`fifo_no_stock_at_all_is_explained`、`fifo_null_bucket_participates`、`fifo_multiple_lines_do_not_double_book`、`fifo_negative_warehouse_still_posts_short` |
| git commit | ✅ | 见 §7（本卡只按路径提交自己的 6 个路径，未碰其它在途卡的改动） |

### 4.1 测试与检查（本卡文件）

| 项 | 结果 |
|---|---|
| `cargo test --lib business::erp::stockout_model::fifo_tests` | **14 passed / 0 failed**，连跑三轮（`unit_run1/2.txt`、`unit_run3.txt`，round3 为 fmt 后最终树） |
| `cargo test --lib business::erp::stockout_model::tests`（既有 DC-13 出库用例） | **7 passed / 0 failed**（`unit_stockout_legacy.txt`） |
| `cargo test --lib business::erp::stockin_handler::stockin_handler_tests_location_dict`（P2-4 库位：出库 FIFO / 指定） | **10 passed / 0 failed**（`unit_location_dict.txt`） |
| `cargo test --lib business::erp::cancel_cascade_tests`（作废级联，受明细行结构影响） | **14 passed / 0 failed**（`unit_cancel_cascade.txt`） |
| `cargo clippy --lib --all-targets` | **exit 0，0 告警**（`clippy.log`，整 crate 干净） |
| `rustfmt +1.94.1 --edition 2024 --check`（本卡 4 个 .rs） | exit 0 |
| 真机 e2e | **连跑三轮 PASS=34 / FAIL=0**（`e2e_run1/2.txt`、round3 在最终树上） |

---

## 5. 文案前后对比（贴原文）

### 5.1 跨库位够量（旧：409 要手工拆行 → 新：自动拆行并过账）

```
旧（05_chains_run3.txt ③）：
  库存不足，过账已拒绝：… 本次要出库 99999.0000，没有任何一个库位够量（按库位建账时间 FIFO 也挑不出来）；
  当前各库位可用：未分配: 8.0000。请把这一行拆成多行并分别指定库位，或先补货

新（e2e A 段，A 30 + B 30 领 60）：
  POST /api/business/erp/stock-out  → HTTP 200，明细 2 行：
    line 1 → 库位账行 A、数量 30（最早建账）
    line 2 → 库位账行 B、数量 30
  PATCH .../status {"status":"POSTED"} → HTTP 200，status=POSTED，两桶清零
```

### 5.2 库存真不够（旧的说不清差多少 → 新的点名缺口 + 分布 + 动作）

```
旧：… 没有任何一个库位够量（按库位建账时间 FIFO 也挑不出来）；当前各库位可用：未分配: 8.0000。
    请把这一行拆成多行并分别指定库位，或先补货

新（建单详情 stock_warnings）：
  物料 P1F2E-M3 部分库存物料 第 1 行：要出 60.0000，本仓可用合计 40.0000，差 20.0000；
  现有库存分布：P1F2E-L01: 20.0000、P1F2E-L02: 20.0000。请先补货 / 收货，或把数量改到 40.0000 以内
  —— 够量时保存草稿会按 FIFO 自动拆成多行并分别占用库位

新（过账 409，HTTP 409）：
  库存不足，过账已拒绝：物料 P1F2E-M3 部分库存物料 在仓库 P1F2E-WH（P1F2 真机仓）第 1 行要出库 60.0000，
  该仓可用合计 40.0000，差 20.0000；现有库存分布：P1F2E-L01: 20.0000、P1F2E-L02: 20.0000
  （按库位建账时间 FIFO 排序，单个库位都不够这一行要的量）。处理办法：① 先补货 / 收货后再过账；
  ② 把本行数量改到 40.0000 以内；③ 重新保存草稿 —— 系统会按 FIFO 自动把这一行拆成多行、分别占用各库位，
  不需要手工拆行

零库存（e2e D 段）：
  物料 P1F2E-M4 零库存物料 第 1 行：要出 5.0000，本仓可用合计 0，差 5.0000；现有库存分布：无库存。
  该仓当前一件可用库存都没有：请先收货 / 入库，或改用别的仓库
```

---

## 6. 偏差 / 没做的事（如实写清）

1. **不够量时不在建单就拦**（草稿可以先建：货在路上 / 先排单后补货）。拦截点仍在过账（409），
   但建单返回的 `stock_warnings` 已经把人话说清了。理由：卡面要求的是「提示清晰」而不是
   「草稿不许存」，且会打破既有「先建单后收货」的作业顺序（也避免改动既有错误码优先级）。
2. **自动拆行发生在建单 / 改单，而不是「一行明细跨桶扣减」**。`biz_stock_out_item` 一行只挂一个
   `location_id`，过账是「一行明细 ↔ 一笔流水 ↔ 一个成本账桶」；跨桶扣会打断这个对应关系
   （`biz_inventory_txn` 没有 location 列），作废原路退回就失去依据。拆行后每行仍一对一，
   下游（过账 / 作废 / 红冲 / 行号 / 凭证按物料合并）零改动。
3. **未做浏览器截图 / 前端联调**：本卡是后端卡，`stock_warnings` 前端展示由前端卡负责；
   验收走 HTTP + 库内实读（脚本每步打印 HTTP 码、明细行、两本账数量、提示原文）。
4. **未部署生产 144**：本机隔离库验证（上线走 `scripts/deploy/deploy.sh`）。
5. **无数据库迁移**：纯代码改动（不改表结构、不加字段）。
6. e2e 用的库存是 psql 直接造到指定库位的（成本账 + 库位账两本，`location_id` 口径与过账一致），
   出库侧全部走生产同一条 HTTP 路径；这是为了把「库位建账时间顺序」钉死（无法用 HTTP 精确控制
   `created_at`）。
7. 端口用 18115：默认的 18095 被另一张卡的在途实例占用（起不来会 `FATAL: Address already in use`），
   脚本已按 `P1FIX2_PORT` / `P1FIX2_DB` 参数化。

---

## 7. 复现

```bash
# 库（一次）：cloudpivot_p1fix2_a（真机）/ cloudpivot_p1fix2_b（单测）
su - postgres -c "createdb -O alun cloudpivot_p1fix2_a"
su - postgres -c "createdb -O alun cloudpivot_p1fix2_b"

# 二进制 + 实例（先起一次触发迁移）
cd /opt/cloudpivot/backend && cargo build --bin cloudpivot
P1FIX2_DB=cloudpivot_p1fix2_b P1FIX2_PORT=18116 bash docs/evidence/20261105-p1fix2/p1fix2_instance.sh start
P1FIX2_DB=cloudpivot_p1fix2_b P1FIX2_PORT=18116 bash docs/evidence/20261105-p1fix2/p1fix2_instance.sh stop

# 单测（真库，ERP_DB_URL 不设会静默 SKIP）
cd /opt/cloudpivot/backend && ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_p1fix2_b \
  cargo test --lib business::erp::stockout_model::fifo_tests -- --test-threads=1

# 真机
P1FIX2_PORT=18115 bash docs/evidence/20261105-p1fix2/p1fix2_instance.sh start
python3 docs/evidence/20261105-p1fix2/p1fix2_fifo_e2e.py http://127.0.0.1:18115 cloudpivot_p1fix2_a 1
P1FIX2_PORT=18115 bash docs/evidence/20261105-p1fix2/p1fix2_instance.sh stop
```
