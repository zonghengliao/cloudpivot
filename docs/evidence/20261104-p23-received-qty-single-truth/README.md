# P2-3 received_qty 双真相收敛 —— 验收证据（2026-11-04）

> 卡：`t_95c36658`【P2-3】received_qty 双真相收敛（一列增量维护 vs 现算，两个数会打架）
> 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §5 DC-05（P2）
> 切号：迁移 **V0217**（`erp_p2_3_received_qty_derived`）
> 切换日：**2026-11-04**（代码落库当天；**生产未部署**，prod 下次重启才生效）

## 0. 一句话结论

`biz_purchase_order_item.received_qty` 这一列**退场**了：迁移 V0217 删列（连同 `ck_biz_po_item_recv`），
「已收数量」只剩一个口径 —— 只算 `status='POSTED'` 且未软删的收货单明细（`POSTED_RECEIVED_SQL`）。
采购单详情、待收明细、过账闸、判收满、联动动作**全部读同一段 SQL**，写路径上的 `+N / −N` 两个函数
（`add_received_qty` / `revert_received_qty`）连同它们的调用点一起删除 —— 没有第二份数可漂。

| 卡面验收项 | 结果 | 证据 |
|---|---|---|
| ① 对账 SQL 差异行数（改之前） | ✅ 生产库 **17 行采购明细 / diff_rows=0**；生产库副本同样 0（各值逐行贴在下文 §1） | `reconcile-prod-before.txt`、`reconcile-prod-copy-before-migration.txt` |
| ② 选定方案 + 理由 | ✅ **方案 A**（列退场、只留现算），理由见 §2 | 本文件 §2 |
| ③ 方案 A：列已删 | ✅ `col_count=0`、`ck_biz_po_item_recv` 已删；down 能完整还原（含 CHECK + 回填） | `migration-after-check.txt`、`migration-roundtrip.txt` |
| ④ grep 全仓无第二条路径 | ✅ `add_received_qty`/`revert_received_qty` **0 定义 0 调用**；对 `biz_purchase_order_item` 的写语句只剩 INSERT（不含该列）；读点全指向派生表达式 | `grep-single-truth.txt`、`grep-received-qty.txt` |
| ⑤ 单测：收货 / 作废 / 部分收货 | ✅ `d4_received_qty_has_single_truth`（收货 2 张累加 = 50、作废回退到 20、三条读路径一致）；`a4_11`（过账 6、作废回 0）；`d4_cancel_posted_restores_pending` 等既有用例全绿 | `unit-tests-erp.txt`、`unit-tests-flow.txt` |
| ⑥ 真机：详情「已收 X/Y」= 去收货「待收」 | ✅ 13 张采购单 13 行**两条接口逐一相等**；部分收货场景截图：详情「已收 1 / 订单 3（待收 2）」vs 去收货「订单 3 / 已收 1 / 待收 2」 | `e2e-two-endpoints.txt`、`e2e-partial-post-cancel.txt`、`p23-A-*.png`、`p23-B-*.png` |
| ⑦ 不变量 | ✅ 生产库副本（跑完 V0217 + 一次真实「收货→作废」）**40/40**，BLOCKER 0 / WARN 0 | `invariants-prod-copy.txt` |
| ⑧ cargo test 全绿 | ✅ `business::erp 893 passed / 0 failed`（1060.59s，干净库 `cp_p23_erp2`，最终代码）、`business::flow 339 passed / 0 failed`（干净库 `cp_p23_flow`） | `unit-tests-erp.txt`、`unit-tests-flow.txt` |
| ⑨ `scripts/check.sh` | ✅ EXIT=0（fmt / clippy `-D warnings` / check / 前端 typecheck / 批次B 两道闸） | `check.txt` |

## 1. 对账 SQL（改之前，144 生产库）

口径：`列值` = `biz_purchase_order_item.received_qty`；`现算值` = 只算 `status='POSTED'` 且单头未软删的
收货单明细按采购明细行求和（注意：`biz_stock_in_item` **没有** `deleted_at`，软删在 `biz_stock_in` 上，
卡面示例里的 `si.deleted_at IS NULL` 照抄会报 `column does not exist`）。

```sql
with recomputed as (
  select i.id, i.received_qty as col_value,
         coalesce(sum(si.qty) filter (where s.status='POSTED' and s.deleted_at is null),0) as recalc
    from biz_purchase_order_item i
    left join biz_stock_in_item si on si.source_item_id = i.id
    left join biz_stock_in s on s.id = si.stock_in_id
   group by i.id, i.received_qty)
select count(*) from recomputed where col_value <> recalc;   --> 0
```

实测（生产库 144 `cloudpivot`，2026-11-04）：

```
po_items=17  si_items_with_src=9  po_items_recv_nonzero=8
diff_rows=0                      -- 差异行数：0（详见 reconcile-prod-before.txt 的 17 行逐行值）
收货单状态（带来源单）：DRAFT=1 POSTED=8
采购单状态：APPROVED=2 CANCELLED=2 DRAFT=5 RECEIVED=8
```

> **诚实说明**：生产库此刻两个值**恰好一致**（所以本卡不会"顺手修数据"），
> 但「一致」只是运气 —— 没有任何机制阻止它们漂开（并发过账 / 作废异常 / 手工改库）。
> 本卡消灭的是**会漂的结构**，不是某一刻的脏数据。

## 2. 方案选择：A（列退场，只留现算）

**选 A，理由是「口径只有一处可写错」而不是「性能更好」：**

1. **第二条真理的维护面在写路径上，A 能把它整体删除。** B 方案要求「过账 +N / 作废 −N /
   任何新写入点都必须在同一事务里」，这是**每加一条收货路径就要记得一次**的约束；
   A 方案把这些调用点（`add_received_qty` 在 `post_in_tx` ⑤、`revert_received_qty` 在作废 ⑥、
   联动动作 `UPDATE_RECEIVED_QTY`）**全部删掉**，约束从"记得写"变成"根本没有地方写"。
2. **过账闸本来就已经在读派生口径。** DC-03 的 `order_item_balances` 用的是 `POSTED_RECEIVED_SQL`，
   如果按 B 把读改成列，等于把「过账放行多少」降级成读缓存列 —— 那是把最硬的一道闸交给最容易漂的数。
3. **符合本项目「不留缓存列」的一贯判断**（同类判决见 `used_amount`）。
4. **代价可接受**：一张单几十行，派生表达式是相关子查询 + 索引 join（见 §5）；
   实测 17 行、9 条收货明细的库上 EXPLAIN 关掉 seqscan 后走 `idx_biz_stock_in_source_posted` +
   `idx_biz_si_item_in`，没有全表扫。
5. **前端/接口形状零改动**：DTO 字段仍叫 `received_qty`，前端 28 处引用一个字没改。

## 3. 改了什么

| 文件 | 改动 |
|---|---|
| `backend/migrations/business/V0217__erp_p2_3_received_qty_derived.{up,down}.sql` | **新增**：up 删列（先删 `ck_biz_po_item_recv`）+ 建部分索引 `idx_biz_stock_in_source_posted (source_order_id) WHERE status='POSTED' AND deleted_at IS NULL`；down 加回列 + 按派生值回填（`LEAST(sum, qty)` 夹取）+ 恢复 CHECK + 删索引 |
| `backend/src/business/erp/model_order.rs` | 列清单拆成 `PURCHASE_ORDER_ITEM_COLUMNS`（不含该列、带 `i.` 前缀）+ `purchase_order_item_columns()`（追加 `POSTED_RECEIVED_SQL AS received_qty`）；`purchase_order_items` / `purchase_order_item_find_by_id` / `purchase_order_item_page` 三处查询改成显式别名 `i`（`find_by_id` 通用模板不支持别名，改为专用查询） |
| `backend/src/business/erp/purchase_model.rs` | 建单 `insert_items` 去掉该列；`PendingReceiveLine.ledger_received_qty` 字段退场（它就是「第二份数」在 DTO 里的投影）；`POSTED_RECEIVED_SQL` 文档升格为「唯一真相」 |
| `backend/src/business/erp/stockin_purchase.rs` | **删除** `add_received_qty` / `revert_received_qty`；`sync_purchase_status` 的「还有几行没收完」判据改用派生表达式（原来是 `received_qty < qty`）；`remaining_qty` 文档同步 |
| `backend/src/business/erp/stockin_handler_post.rs` | 删除过账第 ⑤ 步（回写 + 超收 400 文案）与 `po_snapshot` 参数链路；超收只剩 ①b `guard_purchase_before_post`（409 + 66032） |
| `backend/src/business/erp/stockin_handler_cancel.rs` | 删除作废第 ⑥ 步的逐行 `revert_received_qty` 循环，只保留 `sync_purchase_status`（状态先变 CANCELLED，派生值自动不含本单） |
| `backend/src/business/erp/stockin_handler_purchase.rs` | 删除 `purchase_item_snapshot`（只服务于已删的 ⑤ 步文案） |
| `backend/src/business/erp/stockin_query.rs` | 删除 `source_lines_of`（只服务于已删的回退循环） |
| `backend/src/business/flow/linkage/actions.rs` + `spec.rs` | `UPDATE_RECEIVED_QTY` 语义从「写一列」改为**校验**：`其他单据已过账收货（派生 − 本单贡献）+ 本次 qty ≤ 订购量`，超了仍按关键动作失败回滚；动作码保留（`ck_flow_linkage_action_code` 不用迁移） |
| 单测 | `seed_purchase_order(qty, received)` 改为**造真账**（`received>0` 时补一张 POSTED 收货单）；`d4_ledger_received` 改读派生表达式；`d4_pending_matches_ledger_column` → `d4_received_qty_has_single_truth`（锁三条读路径 + 作废回退）；a4/update/model/linkage/cancel_cascade 里的列读写全部改派生 |
| `scripts/verify/*` | `p2b1_stockin.sh`（夹具造 POSTED 收货单 + 断言派生值）、`d9_document_copy_e2e.py`、`dc03_posting_guard_e2e.py` 同步 |
| `docs/architecture/ARCH-3-模块勾稽关系审计.md` | §5.2 DC-05 行 + §5.4 第 1/7 行标注「✅ 已解决（V0217）」 |

**没动**：收货过账的业务算法（库存三本账 / 移动加权 / 凭证 / 应付）、`scripts/verify/erp_invariants.py`、
调拨单 `biz_stock_transfer_item.received_qty`（另一张表、另一个业务，本卡不碰）、任何历史数据修补。

## 4. 迁移：逐行无损

生产库副本（144 `pg_dump cloudpivot` → 本机 `cp_p23_prod`）先记「旧列值」，再让后端启动跑迁移，
迁移后按派生值重算逐行比对：

```
rows_pre=17 rows_post=17 identical=True      （migration-after-check.txt）
迁移尾部：erp_ledger_aux_balance_permission, erp_p2_2_stub_fields, erp_p2_2b_po_credit_branch, erp_p2_3_received_qty_derived
col_count=0（列已删）  idx=1（部分索引已建）
```

down / up 往返（`migration-roundtrip.txt`）：down 后 `col_count=1`、`check_exists=1`、回填值与派生值
**17/17 逐行相等**；再 up 回到 `col_count=0`、`idx=1`。

## 5. 索引与性能

卡面写的 `biz_stock_in_item (source_item_id) WHERE deleted_at IS NULL` **不适用**：明细表没有
`deleted_at`（软删在单头 `biz_stock_in`），照抄会 `column does not exist`。实际做法：

* 明细侧沿用既有 `idx_biz_si_item_src (source_item_id)`；
* 新增部分索引 `idx_biz_stock_in_source_posted (source_order_id) WHERE status='POSTED' AND deleted_at IS NULL`，
  让「按来源采购单取已过账收货单」有索引入口（原来只能全表扫 `biz_stock_in`）。

EXPLAIN（关掉 seqscan，证明索引可用）：

```
Aggregate
  ->  Nested Loop
        ->  Index Scan using idx_biz_stock_in_source_posted on biz_stock_in s
              Index Cond: (source_order_id = '01a18dee-...'::uuid)
        ->  Index Scan using idx_biz_si_item_in on biz_stock_in_item si
              Index Cond: (stock_in_id = s.id)
```

## 6. 真机（隔离实例 :18125 → 144 生产库副本）

隔离实例：`NIK_DATABASE_NAME=cp_p23_prod`、`NIK_SERVER_LISTEN=127.0.0.1:18125`；
前端 `BACKEND_ORIGIN=http://127.0.0.1:18125 pnpm dev --port 5199`。
（演示前置：把副本库里 `stock_in_post` / `stock_in_cancel` 两个流程置 `DISABLED`，让过账/作废走
「显式跳过审批 + 理由」路径 —— **只改副本库，生产库不动**。）

### 6.1 全量比对（13 张采购单 / 13 行明细）

`e2e-two-endpoints.txt`：逐单比对「采购单详情的明细表（`/purchase-order-item/page?order_id=`）」
与「去收货的待收明细（`/purchase-order/{id}/pending-items`）」——

```
order_no              status            行   详情已收       待收已收       待收       一致
PO-20261031-000002    RECEIVED          1   100.0000    100.0000    0.0000   OK
PO-20261031-000005    CANCELLED         1   0           0           100.0000 OK
...
订单数=13 明细行=13 不一致行=0
```

### 6.2 部分收货 → 过账 → 作废（同一条链路四面读）

`e2e-partial-post-cancel.txt`（真实 API：建草稿收货单 → 过账 → 作废）：

```
① 收货前  详情「已收 0/3.0000」 vs 待收明细「已收 0 / 待收 3.0000」 → OK
② 草稿    详情「已收 0/3.0000」 vs 待收明细「已收 0 / 待收 3.0000」 → OK   （只算过账的）
③ 过账后  详情「已收 1.0000/3.0000」 vs 待收明细「已收 1.0000 / 待收 2.0000」 → OK
④ 作废后  详情「已收 0/3.0000」 vs 待收明细「已收 0 / 待收 3.0000」 → OK   （自动回退）
RESULT: ALL OK
```

### 6.3 截图

| 截图 | 内容 |
|---|---|
| `p23-A-采购单详情-已收1of3.png` | 采购单详情：`收货进度：已收 1 / 订单 3（待收 2，1 行未收完）` |
| `p23-B-去收货-待收明细-订单3已收1待收2.png` | 去收货带出的待收明细：`订单 3 / 已收 1 / 待收 2`，数量默认 2 |

两张截图是**同一个采购单（PO-20261031-000009）、同一时刻**：详情说 1/3、待收说还能收 2 ＝ 已收 1/3，一致。

## 7. 不变量

`NIK_DATABASE_NAME=cp_p23_prod python3 scripts/verify/erp_invariants.py` →

```
结果：40/40 通过   🚨 BLOCKER 0   ⚠️ WARN 0
```

（跑完 V0217 迁移 + 真机 §6.2 的「收货→作废」之后采的样；作废路径的三本账/凭证/应付回滚仍然干净。）

## 8. 单测

见 `unit-tests-erp.txt` / `unit-tests-flow.txt`（按模块分库分进程，**跑的是最终代码**）：

```bash
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_p23_erp2' cargo test business::erp -- --test-threads=1
→ test result: ok. 893 passed; 0 failed; 0 ignored; 0 measured; 1402 filtered out; finished in 1060.59s

ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_p23_flow' cargo test business::flow -- --test-threads=1 \
    --skip business::flow::internal_control::db_tests
→ test result: ok. 339 passed; 0 failed; 0 ignored; 0 measured; 1956 filtered out; finished in 23.36s
```

关键用例（都在上面的 893 里）：

| 用例 | 验的是什么 |
|---|---|
| `d4_received_qty_has_single_truth` | 两张部分收货单（30+20）→ 详情/待收明细/按主键三条读路径都是 `50`；作废其中一张后三条同步回退到 `20`、待收回到 `80` |
| `d4_pending_ignores_draft_stock_in` | 草稿收货单**不算已收**（只有 POSTED 计数） |
| `d4_cancel_posted_restores_pending` | 作废已过账收货单 → 已收回退、采购单退回 `PARTIAL_RECEIVED`、还能再收一次 |
| `a4_11_post_and_cancel_keep_purchase_chain` | 过账 6 → 已收 6；作废 → 已收 0（采购链全链路） |
| `over_receipt_against_purchase_order_is_rejected` / `a4_08_over_receipt_is_400` | 超收仍被拦（建单期 400 / 过账期 409 + 66032） |
| `engine_critical_failure_rolls_back`（flow） | 联动动作 `UPDATE_RECEIVED_QTY` 改成校验后，「超收 = 关键动作失败 → 整笔回滚」语义不变 |

## 9. 未做的事（诚实清单）

1. **生产未部署**：本卡只落代码 + 迁移；prod 下次重启（自动迁移）才生效。切换日 = 2026-11-04（代码落地）。
2. **没有改任何历史数据**：生产库两个值本来就相等（diff_rows=0），不存在要修的行；将来若有差异行，
   按卡面要求另开卡处理（现在也没有"列"可修了 —— 差异只可能出现在收货单本身）。
3. **调拨单的 `biz_stock_transfer_item.received_qty` 未动**：那是「签收数量」，属于调拨链，不参与采购待收口径。
4. **`UPDATE_RECEIVED_QTY` 联动动作语义变了**（写 → 校验）。生产库里没有启用该动作的规则
   （`biz_flow_linkage_rule` 实测 0 条），所以线上行为无变化；配置侧的动作码与参数契约不变。

## 10. 复跑方法

```bash
# 1) 干净库（自动迁移）
su postgres -c "createdb -O alun cp_p23_erp" && NIK_DATABASE_NAME=cp_p23_erp ... ./target/debug/cloudpivot
# 2) 单测
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_p23_erp' cargo test business::erp -- --test-threads=1
# 3) 真机比对（隔离实例指向生产库副本）
python3 p23_e2e_compare.py && python3 p23_e2e_cycle.py
# 4) 不变量
NIK_DATABASE_NAME=cp_p23_prod python3 scripts/verify/erp_invariants.py
```
