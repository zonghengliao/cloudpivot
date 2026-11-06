# 缺陷卡 t_45900ea7 · 发货单列表未下发 `so_no` / `stock_out_no`

> 上游证据：`../20261106-so11-shipment-ui/curl-round1.txt` §⑥（前端卡 t_b143cd85 实测发现）
> 修复提交：见本文末尾「提交」一节（本卡提交时写入）

## 1. 缺陷是什么

`GET /api/business/erp/shipment/page` 的列表行恒为：

```
data.list[0].so_no         = null
data.list[0].stock_out_no  = null
```

而 `GET /api/business/erp/shipment/{id}` 同一个字段有值。同时 `shipment_handler.rs` 里
`ShipmentRes` 的文档注释**承诺**「列表行 `so_no` / `customer_code` / `warehouse_code` 会批量补齐」。
→ 注释与实现互相矛盾，用户体感是「列表缺数据」，前端只能兜底显示「见详情」。

## 2. 根因（两处，都在 `page_shipment`）

1. `ShipmentRes::of` 里 `so_no: None`，列表分支的批量补齐只做了 `customer_*` / `warehouse_*`，
   **漏了 `so_no`**；
2. `stock_out_no` 只在 `detail_of` 里按 `row.stock_out_id` 回填，列表分支不填。

## 3. 改了什么

| 文件 | 变更 |
| --- | --- |
| `backend/src/business/erp/shipment_model.rs` | 新增 `so_no_labels(&[Uuid])` / `stock_out_no_labels(&[Uuid])`：与 `warehouse_labels` 同形（去重 → 一条 `IN (…)` → 空入参短路），判据 `deleted_at IS NULL`；同时给单条入口 `so_no_of` / `stock_out_no_of` **补上同一条判据**（否则列表按软删过滤、详情不过滤，缺陷会以「列表说没有、详情说有」的反向形态复现） |
| `backend/src/business/erp/shipment_handler.rs` | `page_shipment` 按 `rows.map(|r| r.so_id)` / `filter_map(|r| r.stock_out_id)` 各补一次批量查询（**一页固定 4 条补齐 SQL，不做 N+1**）；`ShipmentRes` 文档注释与实现对齐（列表 / 详情**逐字段同口径**；未过账 ⇒ `stock_out_no = null`；订单 / 出库单软删 ⇒ 两个字段同时 `null`） |
| `backend/src/business/erp/shipment_posting.rs` | `so_no_of` 语义收紧后，过账生成出库单 remark 的兜底从「空串」改成「退回订单 id」——订单软删时不会写出「来源销售订单 ／发货单 SH-…」这样的空号 |
| `backend/src/business/erp/shipment_page_fill_tests.rs`（新增） | 真库用例 3 条：列表 vs 详情**逐字相等**、软删后两边**同时**为 `null`、批量函数去重 + 空入参短路 |
| `backend/src/business/erp/mod.rs` | 注册 `#[cfg(test)] mod shipment_page_fill_tests;` |

没有新增迁移、没有改表、没有新增接口 —— 这是纯读数补齐。

## 4. 门禁（都是真跑出来的）

### 4.1 真库单测（`ERP_DB_URL` 指向隔离库 `cloudpivot_so09fix`，从 `cloudpivot_so11`(V0286) 克隆）

```
cargo test --lib shipment -- --test-threads=1
test result: ok. 33 passed; 0 failed; 0 ignored     ← 含本卡新增 3 条
```

日志：`unit-realdb.txt`（本卡 3 条用例的完整输出）与 `unit-shipment-33.txt`（`shipment*` 全量 33 条 =
handler/model/posting 既有用例 + 本卡 3 条，最终源码跑的）。

### 4.2 真 token curl（隔离实例 18099 / 库 `cloudpivot_so09fix`）—— **修复前 vs 修复后**

同一份 `curl-e2e.sh`，同一台隔离实例，只换二进制：

| 轮次 | 二进制 | 结果 |
| --- | --- | --- |
| 修复前（基线复现） | 修复前 `target/debug/cloudpivot`（16:30 编译） | **FAIL=1**，11 条断言红：`列表 so_no = null`、`列表 stock_out_no = null`，而详情有值 → 缺陷稳定复现 |
| 修复后 第 1 轮 | 本卡 worktree 编译（`rustfmt` 前） | 29/29 断言全绿，`FAIL=0` |
| 修复后 第 2 轮 | 同上（重复跑，验残留/幂等） | 29/29 断言全绿，`FAIL=0` |
| 修复后 第 3 轮 | **最终源码**（`rustfmt` 后重新 `cargo build`，零警告） | 29/29 断言全绿，`FAIL=0` ← 对应提交内容 |
| 修复后 第 4 轮 | 同上（再跑一遍） | 29/29 断言全绿，`FAIL=0` |

> `curl-after-r1.txt` / `curl-after-r2.txt` 存的是**最终源码**那两轮（第 3、4 轮）的完整输出。

关键断言（摘自 `curl-after-r2.txt`）：

```
✓ 列表 so_no 非空（缺陷原状：null） = SO-20261106-000056
✓ 列表 so_no == 订单号              = SO-20261106-000056
✓ 详情 so_no                        = SO-20261106-000056
✓ 列表 so_no 与详情逐字相等          = SO-20261106-000056
✓ 详情 stock_out_no（未过账）        = null
✓ 详情 stock_out_no                 = OUT-20261106-000043
✓ 列表（A 行）so_no 与详情逐字相等    = SO-20261106-000056
✓ 列表（A 行）stock_out_no 与详情逐字相等 = OUT-20261106-000043
✓ 列表（B 行）so_no = 订单B 号        = SO-20261106-000057
✓ 列表（B 行）stock_out_no（未过账）  = null
✓ 同页两行 so_no 各自命中（不是整页同值）
✓ 列表 so_no（订单软删） = null       ← 列表与详情同判据
✓ 详情 so_no（订单软删） = null
✓ 详情 stock_out_no 不受订单软删影响 = OUT-20261106-000043
✓ 详情 stock_out_no（出库单软删） = null
✓ 列表 so_no（恢复） = SO-20261106-000056
```

覆盖到的边界：
* 一页里**同时**有「已过账」与「草稿」两行 → 草稿行 `stock_out_no=null`、已过账行是 OUT 号，
  且两行 `so_no` 各自命中（证明批量补齐按行取值，不是整页取第一个值）；
* 订单软删 / 出库单软删 → 列表与详情**同时** `null`（这是 `so_no_of` 补判据的护栏，
  否则会退回「一边有一边没有」的病灶形态）；
* 恢复后两列都回到有值（证明不是「永久变空」的假修复）；
* 客户 / 仓库编码仍在（没碰掉既有补齐）。

### 4.3 格式 / 编译

```
rustfmt --edition 2024 --check  ← 5 个改动文件全 OK
```

## 5. 为什么用独立 worktree 编译（过程记录，供后续卡参考）

本卡开工时共享工作区 `/opt/cloudpivot` 正被 SO-12（应收账款）卡并发编辑：
`state_machine.rs` 先引用了还不存在的 `DocKind::Receivable`，随后 `receivable.rs` 又有
`query_first_sql(&str)` 的类型错 —— 共享工作区**编译不过**，且与本卡无关。

于是本卡把改动同步到 `git worktree`（`/root/so09fix-wt`，基于 `4d4f036`）+ 独立
`CARGO_TARGET_DIR=/root/so09fix-target` 做门禁，避免「别人的半成品」把本卡门禁拖成假红。
提交时只按路径 `git add` 本卡的 4 个后端文件 + 1 个测试文件（见 §7）。

## 6. 复现环境

```bash
# 1) 克隆库（cloudpivot_so11 已到 V0286）+ 起一台隔离实例 18099
sudo -u postgres createdb -O alun -T cloudpivot_so11 cloudpivot_so09fix
REPO=/opt/cloudpivot BIN=<编译产物> bash <scratch>/start-so09fix-backend.sh   # NIK_* / JWT / STORAGE_KEY
# 2) curl 证据（两轮）
BASE=http://127.0.0.1:18099 DB=cloudpivot_so09fix bash curl-e2e.sh
# 3) 真库单测
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_so09fix' \
  cargo test --lib shipment -- --test-threads=1
```

实例与 worktree 在验收后已停止/清理（见 §7）。

## 7. 提交与清理

* 提交：`4c2c1b0` —— `git add` 按路径只含本卡文件：
  `backend/src/business/erp/{shipment_handler.rs, shipment_model.rs, shipment_posting.rs, shipment_page_fill_tests.rs}`、
  `mod.rs`（**只暂存本卡那 3 行**：共享工作区里该文件同时有 SO-12 卡加的 `pub mod receivable*;`，
  用 `git hash-object -w` + `git update-index --cacheinfo` 造了一份「HEAD + 本卡 3 行」的 blob，
  避免把别人未提交的模块声明带进本提交）与 `docs/evidence/20261106-so09-page-fill/`；
* 隔离实例（18099）验收后已停止，端口已释放；
* 隔离库 `cloudpivot_so09fix` 已 `dropdb`；worktree `/root/so09fix-wt` 已 `git worktree remove`；
* **遗留**：`/root/so09fix-target`（cargo target，约 8.6G）未能自动删除 —— 本卡在无人值守会话，
  `rm -rf` / `find -delete` 被审批策略拦截。需有审批的会话执行 `rm -rf /root/so09fix-target` 回收磁盘。
