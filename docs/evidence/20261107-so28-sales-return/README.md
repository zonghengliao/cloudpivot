# SO-28 销售退货单表 + 迁移（证据包）

卡：`t_6b51a48b`（ARCH-7 §15.2.2）｜分支：`main`｜工作副本：`/opt/cloudpivot`
验证库：`cloudpivot_so28` @ `127.0.0.1:18228`（`RECREATE=1` 空库全量重放，**230** 条迁移，`max(version)=303`）
二进制：`backend/target/debug/cloudpivot`（迁移按 `backend/config/config.toml` 的 `path = "migrations"` 在启动时落库）

---

## 一、交付了什么

| # | 卡面要求 | 落点 |
|---|---|---|
| 1 | `biz_sales_return` 主表（17 业务列 + 5 审计列） | `backend/migrations/business/V0303__sales_return.up.sql` ① |
| 2 | `biz_sales_return_item` 明细表（12 业务列 + 2 审计列） | 同文件 ② |
| 3 | 单号 `SR-YYYYMMDD-NNNNNN` | 同文件 ③ `biz_seq(seq_type='SR', prefix='SR', date_pattern='yyyyMMdd', separator='-', step=1)` |
| 4 | 约束 `ck_biz_sreturn_item_qty (qty > 0)` | 同文件 ② 明细表 CHECK（**名字与卡面逐字一致**） |
| 5 | 外键 `ON DELETE CASCADE` | 明细 → 单头 `fk_biz_sreturn_item_return … ON DELETE CASCADE`（见下方「外键口径」） |
| 6 | 回滚 | `V0303__sales_return.down.sql`（先明细后单头 + 按确定性 id 删 SR 取号行） |

**号段**：卡面写 `V0269`，实测该号段已被销售链占用（V0264–V0297 + V0301/V0302 全在，
且已应用迁移不可改、指纹校验会炸），本卡取 **V0303**（开工时 `business/` 最大号 = V0302，
SO-27 的 `V0302__sales_order_contract`）。文件头已写明顺延理由，提交前再复核过一次号段。

### 表结构（实测 `\d+` 全文见 `structure.txt`）

`biz_sales_return`（22 列）：`id` / `return_no` / `customer_id` / `so_id` / `shipment_id` /
`return_date` / `return_type` / `warehouse_id` / `status` / `total_amount` / `total_tax` /
`total_with_tax` / `stock_in_id` / `receivable_id` / `red_invoice_id` / `approval_instance_id` /
`remark` / `created_at` / `updated_at` / `create_by` / `update_by` / `deleted_at`

`biz_sales_return_item`（14 列）：`id` / `return_id` / `so_item_id` / `material_id` / `qty` /
`price` / `amount` / `tax_rate` / `tax_amount` / `location_id` / `serial_nos` / `remark` /
`created_at` / `updated_at`

索引：`uk_biz_sreturn_no (return_no) WHERE deleted_at IS NULL`（存活行唯一，软删后单号可复用）
+ `idx_biz_sreturn_{order,shipment,customer,status,date,stock_in}` +
`idx_biz_sreturn_item_{return,so_item,material}`。

### 三条与稿面/卡面不同的口径（都写在迁移头注释里）

1. **金额精度**：ARCH-7 §15.2.2 稿面写 `numeric(18,2)`，仓内实际口径是 `NUMERIC(18,4)`
   （`biz_sales_order` / `biz_receivable` / `biz_voucher_entry` 全 18,4）。照稿面写 (18,2) 会在
   合计与凭证对账时静默四舍五入，故取 **18,4**；`price` 取 `NUMERIC(18,6)`、`tax_rate` 取
   `NUMERIC(7,4)`，与 `biz_sales_order_item` **逐列同型**（退货行从订单行/发货行直接拷贝不丢精度）。
   这一条由迁移自检和外部探针 `S4_amount_types_match_order_item` 双向钉住。
2. **`so_item_id` 可空**：卡面/稿面没标必填；`return_type='OTHER'` 无原单、或原单行已不可考的
   历史退货必须能录，故不设 NOT NULL。超退校验（`sum(已退)+本次 ≤ 已发量`）是**跨表**判定，
   PG 的 CHECK 表达不了、全仓迁移零 TRIGGER ⇒ 交 SO-29 在事务内 `FOR UPDATE` 后判定。
3. **`return_type` 无默认值**（稿面也没给默认）：手工漏传就 23502，避免「默认 OTHER」把
   质量/错发类退货静默记成「其他」。

### 外键口径（卡面「外键 ON DELETE CASCADE」落在哪）

| 列 | 指向 | 口径 | 理由 |
|---|---|---|---|
| `biz_sales_return_item.return_id` | `biz_sales_return(id)` | **硬外键 ON DELETE CASCADE** | 卡面点名的 CASCADE 就在这：明细离开单头无独立意义（与 `biz_sales_order_item` / `biz_shipment_item` 同口径） |
| `so_id` / `shipment_id` | 订单 / 发货单 | 弱引用（无外键） | ADR-06 单据链零外键：退货→订单→发货互相引用，硬外键会让「订单/发货单物理删或迁移演练」被退货单绑死，且级联方向语义错（订单消失不该吞掉退货单）。有效性由 SO-29 建单时校验 |
| `customer_id` / `warehouse_id` / `material_id` / `location_id` | 主数据 | 弱引用（无外键） | 主数据走软删，历史单据必须永远可读（与 `biz_sales_order.customer_id`、`biz_shipment.customer_id` 同风格） |
| `stock_in_id` / `receivable_id` / `red_invoice_id` / `approval_instance_id` | 过账/审批产物 | 弱引用（可空） | 由过账事务**反向生成**的回链列，建外键会让过账事务成环（V0283 的 `stock_out_id` 同理） |

迁移自检断言「单头 **0** 条外键 + 明细**恰好 1** 条 CASCADE 外键」，外部探针 `S5/S6` 再独立复核一遍
⇒ 后人想偷偷补硬外键会在干净库 APPLY 时就炸。

### 状态机 / 退货类型域

- `ck_biz_sreturn_status`：`DRAFT / SUBMITTED / APPROVED / PENDING_POST / POSTED / CANCELLED`
  六个值逐字对齐 ARCH-7 §15.2.4（含 `DRAFT/SUBMITTED/APPROVED → CANCELLED` 三条作废边）。
- `ck_biz_sreturn_type`：`QUALITY / WRONG_ITEM / OTHER`。
- **本卡只建表，不登记 `LEGAL_EDGES`**：状态迁移边必须登记到 `shared/state_machine`
  （`DocKind::SalesReturn` + `SALES_RETURN_EDGES`）后由 **SO-29** 复用 —— 与 SO-04 建表不登记
  `DocKind::SaleOrder`、SO-05 才登记的分工一致。届时 LEGAL_EDGES 与本 CHECK 域值必须逐字对齐
  （本迁移自检会逐值断言，漂移即炸）。

---

## 二、证据（全部本机实跑，原始输出在旁）

| 证据 | 文件 | 结果 |
|---|---|---|
| **APPLY_OK（空库全量重放）** | `apply_clean_db.txt` | 新建 `cloudpivot_so28` → 引擎一次启动应用 **230** 个迁移；`schema_migrations = 303\|sales_return\|versioned\|executed\|applied_seq=220`；指纹 `max\|count = 303\|230`，超管 1 人 |
| **迁移内自检（结构 + 行为探针）** | 同 `apply_clean_db.txt` 尾 | DO 块全程无 RAISE：列数 22/14、卡面列逐字、唯一索引部分语义、外键口径、域值、默认值、qty=0/−2 被拒、孤儿明细被拒、CASCADE、重号被拒、软删可复用、取号行口径 |
| **外部独立约束探针** | `constraint_probes.txt` | **23 / 23 PASS**（9 条结构 + 14 条行为；psql 退出码 0） |
| **回滚演练** | `rollback_drill.txt` | 克隆库 `cloudpivot_so28_drill`：`down` → 两表消失 + SR 取号行消失 + 父表 `biz_sales_order` 完好；`up` 重放结构/取号行/CHECK/外键全回来；第二轮 `up` 幂等（SR 仍 1 行）；第三轮探针再 23/23；克隆库已删 |
| **启动自洽** | `boot_health.txt` | 已迁移库再起：日志 `数据库迁移完成: 应用 0 个迁移`（V0303 不重放）+ `/health = 200` + 迁移条数 230\|303 不变 |
| **表结构快照** | `structure.txt` | 两表 `\d+` 全文（列/类型/默认/COMMENT/索引/CHECK/外键） + `biz_seq(SR)` 行 |

复跑命令：

```bash
# ① 空库 APPLY_OK（约 3 分钟）
ERP_DB=cloudpivot_so28 PORT=18228 bash scripts/verify/erp_clean_db.sh
# ② 外部探针
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so28 -a \
  -f docs/evidence/20261107-so28-sales-return/constraint-probes.sql
# ③ 回滚演练（自带克隆库与清场）
bash docs/evidence/20261107-so28-sales-return/rollback-drill.sh
# ④ 启动自洽
bash docs/evidence/20261107-so28-sales-return/boot-health.sh
```

测试库 `cloudpivot_so28` 已停实例、保留在机（SO-29 可直接复用或 `-T` 克隆）。

---

## 三、本卡范围边界（不越界，交给下游卡）

| 事项 | 卡 | 说明 |
|---|---|---|
| 退货 CRUD 接口 + 状态机入口（`DocKind::SalesReturn` + `LEGAL_EDGES`） | **SO-29** `t_f7602e08` | 本卡不写 Rust 代码，故**没有** `business/` 新增模块 ⇒ 不存在「新增 business 代码缺 `#[cfg(test)] mod tests`」的问题；`cargo test` 未被本卡改动影响 |
| 超退校验（`sum(已退) + 本次 ≤ 已发量`）、从发货单带出可退明细 | SO-29 | 跨表判定放 model（`FOR UPDATE` 锁订单行/发货行后算），不写触发器 |
| 过账链路：退货入库单（`biz_stock_in.source_type=SALES_RETURN`，走 `inventory_writer.rs`，禁直写三本账）+ 红字入库凭证（`biz_voucher` 借贷互换恒正）+ 冲减应收 + 红字发票（`biz_invoice` `OUT_RED` 负金额）+ 回写 `delivered_qty` | 后续 SO 卡 | 本卡只预留回链列（`stock_in_id` / `receivable_id` / `red_invoice_id`），不建外键 |

**环境噪声**（非本卡产物）：工作区 `backend/src/business/erp/receipt.rs` 仍有邻居未提交改动，
本卡未纳入提交；平行卡的 `V0266` 重放属迁移器对「已应用文件被改」的既有行为。

---

## 四、一句话结论

空库 APPLY_OK（230 条迁移，V0303 `versioned|executed`）+ 迁移内自检全过 + 外部探针 23/23 PASS +
down/up 幂等演练全过 + 重启「应用 0 个迁移」且 `/health` 200 —— 两张表结构与 ARCH-7 §15.2.2
卡面逐字一致（差异仅「金额精度按仓内 18,4」「`so_item_id` 可空」两条，已在迁移头注释与本文单列）。
