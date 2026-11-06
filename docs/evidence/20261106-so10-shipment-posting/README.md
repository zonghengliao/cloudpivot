# SO-10 发货过账 → 销售出库 → 库存扣减（t_83e23c1d）

一句话：**发货单 `PENDING_POST → POSTED` 在同一个事务里生成销售出库单、扣减三本账、
回写订单行 `delivered_qty`、把销售订单推进到 `PARTIAL_OUT` / `OUTBOUND`；任何一步失败整笔回滚。**

---

## 一、做了什么（改了哪些文件 / 函数）

| 文件 | 改动 | 关键函数 |
|---|---|---|
| `backend/src/business/erp/shipment_posting.rs`（**新增** 1118 行） | 过账的库存效果全部在这里：建销售出库单 → 复用过账内核扣账 → 回填出库单引用 → 回写 `delivered_qty` → 推订单状态；纯函数 + 5 条真库用例 | `post_effects` / `to_stock_out_lines` / `bump_delivered` / `order_totals` / `order_status_after` / `stock_out_reason` / `line_remark` / `location_ledger_row` / `lock_order` |
| `backend/src/business/erp/shipment_handler.rs` | 过账分支重构：**先 CAS 落状态、再跑库存效果**（并发重复过账在 CAS 就分出生死）；commit 之后才跑延迟联动；详情新增 `stock_out_no`（SO-11 要显示「关联出库单号」） | `change_shipment_status`（事务块）、`ShipmentRes::of`、`detail_of` |
| `backend/src/business/erp/stockout_handler.rs` | 把出库过账内核暴露给销售链复用（不复制实现）；新增提交后联动入口 | `post_out_in_tx`（`post_out_tx` 的 `pub(super)` 包装）、`run_deferred_linkage_after_commit` |
| `backend/src/business/erp/stockout_base.rs` | `NewStockOut` 增加 `source_type` / `source_id`；新增常量 `SOURCE_TYPE_SALES` | `NewStockOut`、`SOURCE_TYPE_SALES` |
| `backend/src/business/erp/stockout_model.rs` | 建单 INSERT 带来源两列（手工建单两个都空 → `NULL`，不参与唯一索引） | `create` |
| `backend/src/business/erp/shipment_model.rs` | 新增只读小查询（详情显示出库单号） | `stock_out_no_of` |
| `backend/src/business/erp/mod.rs` | 注册模块 + 模块表登记 SO-10 | `pub mod shipment_posting;` |
| `backend/src/business/flow/linkage/spec.rs` | 更新 `ENTITY_SHIPMENT` 的豁免理由（联动改由它生成的出库单触发） | `TRIGGER_EXEMPT` |
| `backend/migrations/business/V0285__sales_stock_out_source.up.sql` / `.down.sql`（新增） | `biz_stock_out` 加 `source_type` / `source_id` + 部分唯一索引 `uk_biz_so_source` + 自检 | — |

### 关键设计决定（都写进了代码注释，这里汇总一份）

1. **不另写扣库存逻辑**：销售出库走 `stockout_handler::post_out_in_tx`（= 出库单自己的过账内核），
   负库存闸 / 库位 FIFO / 成本结转凭证 / 联动与原出库单**逐字同一份实现**。
   两份实现迟早漂移，一份改了另一份不改 = 账对不上。三本账的最终写入口仍是
   `inventory_writer.rs`（`shipment_posting.rs` 里**一行库存写 SQL 都没有**）。
2. **`source_type='SALES'` 是新列，不是新 `out_type`**：`out_type`（ISSUE/SCRAP/LOSS/OTHER）说的是
   「领用性质」（决定借方科目），`source_type` 说的是「谁生成了这张单」。销售出库取
   `out_type=OTHER`（借 6401 主营业务成本，与销售成本结转一致）+ `source_type=SALES`。
   把 SALES 塞进 `out_type` 会同时污染 CHECK 约束、凭证科目映射、前端字典与巡检词表。
3. **幂等三道闸**：发货单 CAS（`WHERE status=PENDING_POST`）→ `biz_shipment.stock_out_id` 回填 CAS
   （`WHERE stock_out_id IS NULL`）→ 库层部分唯一索引 `uk_biz_so_source(source_type, source_id)`
   （并发下最后一道，撞了就是 409）。库存只会扣一次。
4. **CAS 先于库存效果**：先把发货单状态推到 `POSTED`（0 行 = 并发被别人推走 → 整笔回滚），
   再建出库单 / 扣库存。反过来的话两个并发请求会各扣一遍库存、然后第二个才在状态上失败。
5. **锁来源订单行（`SELECT … FOR UPDATE`）**：同一订单的多张发货单并发过账在这里串行化，
   否则两张单各自读到 `APPROVED`，订单状态会被后写的那张覆盖回退。
6. **卡面状态名与库层不一致**：卡面写 `PARTIAL_SHIPPED` / `SHIPPED`，V0264 的
   `ck_sales_order_status` 里**没有**这两个值（迁移自检专门插了一条 `SHIPPED` 断言必被拒），
   PRD §4.13.3 用的是 `PARTIAL_OUT` / `OUTBOUND`。本卡落库只可能是后两者
   （`order_status_after` 用单测钉死）。
7. **超发闸放在 SQL 里**：`UPDATE … SET delivered_qty = delivered_qty + ? WHERE delivered_qty + ? <= qty`，
   0 行 = 并发超发 → 409 → 整笔回滚（这时库存已经扣了，回滚把它还回去 —— 见 `late_failure_rolls_back_the_whole_posting`）。
8. **联动仍豁免 `ENTITY_SHIPMENT`**：过账的跨单据联动走它生成的**出库单**（`STOCK_OUT/POSTED`
   触发点已接线），同一件事只触发一次，避免通知 / 凭证重复。

---

## 二、验证（全部为真实执行）

### 1) 迁移落库（真库，`cloudpivot_so10`）

```
 version |            name            |   kind    | applied_via
---------+----------------------------+-----------+-------------
     284 | sales_shipment_permissions | versioned | executed
     285 | sales_stock_out_source     | versioned | executed
```
`\d biz_stock_out` 实测：

```
 source_type   | character varying(20)
 source_id     | uuid
 "uk_biz_so_source" UNIQUE, btree (source_type, source_id) WHERE source_type IS NOT NULL AND deleted_at IS NULL
```
迁移末尾自检覆盖：两列存在且可空、索引是**部分**唯一索引、同一来源第二张出库单必被拒、
`source_type IS NULL` 的行可多行（老数据与手工建单不被误伤）。

### 2) 模块单测（真库，两轮）

```
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so10' \
  cargo test --lib --features test-hooks shipment_posting -- --test-threads=1
```
两轮均 **9 passed / 0 failed**（4 条纯逻辑 + 5 条真库），用例：

| 用例 | 验的是什么 |
|---|---|
| `normal_post_deducts_three_books_and_closes_order` | 全程 10 件：出库单 `POSTED/OTHER/SALES` + 有凭证、成本账/库位账各 −10、流水 −10、`delivered_qty=10`、订单 `OUTBOUND`、详情回显 `OUT-…` |
| `partial_post_moves_to_partial_out_then_outbound` | 3 件 → `PARTIAL_OUT`（库存 17）→ 再 7 件 → `OUTBOUND`（库存 10，累计已发 10） |
| `insufficient_stock_leaves_everything_untouched` | 账上 2 发 5 → 409；三账 / 出库单 / `delivered_qty` / 状态全不动 |
| `repeated_post_is_idempotent` | 第二次过账 409；库存不再变、销售出库单仍 1 张、`delivered_qty` 没被加两遍 |
| `late_failure_rolls_back_the_whole_posting` | **失败点在扣完库存之后**（订单行已发 8/10、再发 5）→ 409 且库存/流水/出库单/状态全部回滚 |

### 3) 真实 token curl 闭环（两轮，各 34 条断言全通过）

```bash
# 实例：cloudpivot_so10 + 127.0.0.1:18093（NIK_PROFILE=test）
bash docs/evidence/20261106-so10-shipment-posting/curl-e2e.sh
```
两轮输出：`curl-round-1.txt` / `curl-round-2.txt`，末行均 `===== 结果：全部断言通过 =====`。
实测样例（第一轮）：

```
出库单号格式 OUT-20261106-000008
出库单 status = POSTED / out_type = OTHER / source_type = SALES / 行数 = 1
成本结转凭证已生成 = true          凭证-出库单双向一致 = 1
成本账 MAT_A（10−10） = 0.0000     库位账 MAT_A（10−10） = 0.0000
出库流水合计 MAT_A = 10.0000
订单行 delivered_qty = 10.0000     订单状态（全发完） = OUTBOUND
④ 重复过账 409 → 成本账仍 0 / 出库单仍 1 张 / delivered_qty 仍是 10
⑤ 部分发货：3 件 → PARTIAL_OUT（库存 7）→ 7 件 → OUTBOUND（库存 0，已发 10）
⑥ 零库存发 5 → 409；三账 0 / 出库单 0 张 / 发货单仍 PENDING_POST / delivered_qty 0 / 订单仍 APPROVED
```

### 4) 静态门禁

```
cargo fmt                     → rc=0
cargo clippy --all-targets --features test-hooks -- -D warnings → rc=0（零告警）
cargo check --all-targets --features test-hooks                → rc=0
```

### 5) 全量回归

见 `full-suite-round-1.txt`（命令与结果）：

```
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so10b' \
  cargo test --lib --features test-hooks -- --test-threads=1
```

---

## 三、复现方式

```bash
# 1) 干净库 + 迁移（owner 必须是应用账号 alun）
runuser -u postgres -- createdb -O alun cloudpivot_so10
cd /opt/cloudpivot/backend
NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_so10 NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun NIK_SERVER_LISTEN=127.0.0.1:18093 NIK_ADMIN_PASSWORD='Admin@123456' \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
setsid nohup ./target/debug/cloudpivot > /tmp/so10_server.log 2>&1 < /dev/null &
# 等 /health 200 且日志出现「数据库迁移完成」

# 2) 单测（真库）
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_so10' \
  cargo test --lib --features test-hooks shipment_posting -- --test-threads=1

# 3) 真实 token 闭环
bash /opt/cloudpivot/docs/evidence/20261106-so10-shipment-posting/curl-e2e.sh
```

---

## 四、留给下游（SO-11 前端）

1. `GET /api/business/erp/shipment/{id}` 的详情现在带 **`stock_out_no`**（过账后非空），
   直接显示「关联出库单号」，不用再按 `stock_out_id` 查一次。
2. 过账按钮仍是 `PATCH /shipment/{id}/status {"target_status":"POSTED"}`，**没有新接口**；
   过账成功后 `status=POSTED`、`stock_out_id` / `stock_out_no` 有值。
3. 过账失败的两类 409 文案（库存不足 / 并发超发）都点名了物料与数量，前端直接 toast 即可。
4. 订单状态显示用 `PARTIAL_OUT` / `OUTBOUND`（不是 `PARTIAL_SHIPPED` / `SHIPPED`）。

## 五、踩过的坑（下一个人直接用）

1. **Uuid::now_v7() 前 8 位是时间戳**：测试夹具用 `&id.to_string()[..8]` 造单号，同一毫秒内
   两张单会撞 `uk_biz_shipment_no`。用尾部 12 位（`[24..]`）。
2. **`db().scalar_with_params::<Option<String>>(...)` 返回的是 `String`**（TypeMapper 会把
   NULL 折成默认值），再 `.expect()` 得到 `String` 而不是 `Option`；要 `Option` 就显式 `::<String>` 后自己包。
3. **`COALESCE(SUM(qty),0)` 空表返回 `0`（无标度）**，断言 `'0.0000'` 会挂；
   加 `::numeric(18,4)` 才有四位。
4. **发货明细的 `location_id` 是库位字典（`biz_location.id`），出库明细的 `location_id` 是库位账行
   （`biz_inventory_location.id`）** —— 两套 id 不能混用（混了就是扣错桶）；指定库位但该库位没有
   该物料的账行 → 409，不能静默改走 FIFO。
5. 卡面点的 `docs/architecture/销售链设计要点-v1.0.md` 在仓库里**不存在**，实际依据是
   `docs/architecture/ARCH-7-销售模块设计.md` §7.1 + V0264 / V0283 / V0284 迁移与 PRD §4.13.3。
