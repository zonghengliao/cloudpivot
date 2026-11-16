# M2-06 在途库存「发货 → 签收」全流程单测 · 证据

- 卡：`t_b3bcd66b`（M2-06【M2·仓库】在途库存：单测 / 发货→签收 全流程）
- 上游：`t_7ba5b0e2`（M2-01 表）/ `t_6c090f97`（M2-02 过账）/ `t_5f324a4c`（M2-03 签收）
  / `t_f1bce846`（M2-04 查询）/ `t_b545b37a`（M2-05 对账）
- 交付：`backend/app/src/business/erp/inventory_transit/transit_flow_tests.rs`（7 条真库用例）
  + `scripts/verify/m206_transit_flow_drill.sh`（可复跑的真库演练）
- 生产代码：**0 行改动**（本卡只补用例）。迁移：**无**（表结构与权限码 M2-01…M2-05 已就位）。

## 一、这 7 条用例钉的是什么

本卡刻意**不重测**单点口径（过账扣不扣库存归 M2-02，签收推不推 `received_qty` 归 M2-03，
接口字段形状归 M2-04/M2-05），它钉的是**跨卡的一致性** —— 四张卡拼起来，账还对不对得上。

| 用例 | 钉住的口径 | 关键断言 |
|---|---|---|
| `full_flow_three_stages_stay_balanced` | 发货→签收主链路四段（DRAFT / PENDING_POST / POSTED / 签收）各有账 | 段① 在途 0 行、对账 `checked=0`；段② 仍 0 行、无出库单；段③ 在途 6 / 库存 20 未扣 / 出库单 `PENDING_POST` / `/summary` 6 / 对账 `balanced`；段④ 在途 0 / 已签 6 / 库存 14 / 出库单 `POSTED` / 凭证 1 / 应收 1 / 对账 `balanced` |
| `multi_material_shipment_receives_every_line` | 一张单多物料 = 每物料一行在途，签收逐行清零 | 过账后 2 行在途合计 8、明细 2 行、汇总 2 组、对账 `checked=2`；签收后两行都 `RECEIVED` 且余量 0，库存甲 10→5 / 乙 10→7 |
| `split_shipments_accumulate_and_drain_transit` | 同一订单行分两次发货：在途累计、逐张签收递减 | 3+4=7（`shipments=2`）→ 签一张剩 4（`shipments=1`）→ 全签 0；库存 20→17→13；订单 `delivered_qty=7` → `OUTBOUND` |
| `transit_holds_available_qty_so_the_same_goods_cannot_ship_twice` | 在途**占用可用量**（否则同一批货能反复发） | 账面 10 / 在途 8 时再发 5 → 过账 409（文案含「在途未签收」），且发货单仍 `PENDING_POST`、无第二张出库单、库存仍 10；补货 3 后同表单直接重试成功（约定 11：不留死单） |
| `transit_and_availability_are_per_warehouse` | 可用量按仓算；在途行落在**发货仓** | 仓乙 0 库存 → 过账 409 且不进对账（无 `MISSING_TRANSIT` 假红）；仓甲 10 → 5，`warehouse_id` = 仓甲 |
| `read_apis_agree_with_each_other` | 三方读数一致：明细 Σ == `/summary` 合计 == 对账 `in_transit_qty` | 2 物料 × 2 单：14（4 行）三面相等；签掉一张后同为 6 |
| `unposted_shipments_are_out_of_the_transit_books` | DRAFT / PENDING_POST 单不进在途账、不进对账 | 三张单（4 已过账 / 2 待过账 / 3 草稿）在途只有 4、明细 1 行、对账 `checked=1` `untracked=0` 无 `MISSING_TRANSIT`；三张全过账后 9 |

每条用例末尾都跑一次 `identity_violations()`：`shipped = received + qty_in_transit`、
`RECEIVED` 必余量 0 且有签收时间、`IN_TRANSIT` 必有余量（V0420 生成列 + 4 条 CHECK 的语义自检）。

## 二、实测结果（全部对临时演练库，跑完即删）

### 1. 骨架演练 `bash scripts/verify/m206_transit_flow_drill.sh`

```
== 汇总：PASS=16 FAIL=0 ==
ALL PASS
```

含：模板克隆 / 18 条迁移（V0405…V0422）/ 表与两个权限码就位 / **本卡 7 条全绿 + SKIP=0**
/ 整组 `inventory_transit` 48 条全绿（M2-04 查询 + M2-05 对账 + 纯逻辑）/ 上游 `shipment` 46 条全绿
/ 夹具清场 4 项全 0（`M206-` 单据 / 在途 / 库存均无残留）。

### 2. 逐条用例输出（`--nocapture`，`00_transit_flow_tests.out`）

```
test ...::full_flow_three_stages_stay_balanced ... [M2-06] 主链路：在途 0→6→0，库存 20→20→14，对账两段 balanced
ok
test ...::multi_material_shipment_receives_every_line ... [M2-06] 多物料：在途 8（2 行）→ 0，库存甲 −5 / 乙 −3
ok
test ...::read_apis_agree_with_each_other ... [M2-06] 三方读数一致：明细=汇总=对账 14 → 签一张后同为 6
ok
test ...::split_shipments_accumulate_and_drain_transit ... [M2-06] 分两次发货：在途 3+4=7 → 签一张剩 4 → 全签 0，库存 20→13
ok
test ...::transit_and_availability_are_per_warehouse ... [M2-06] 按仓算可用量：仓乙 0 库存过账被拒，仓甲 10 → 5
ok
test ...::transit_holds_available_qty_so_the_same_goods_cannot_ship_twice ... [M2-06] 在途占可用量：账面 10 / 在途 8 时发 5 被拒 409；补货 3 后重试成功
ok
test ...::unposted_shipments_are_out_of_the_transit_books ... [M2-06] 未过账单不入账：在途 4（过账）→ 9（三张全部过账），全程无假红
ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 5667 filtered out; finished in 1.27s
```

### 3. 清场幂等：同一个库连跑两遍（`01_same_db_twice.out`）

```
===== 第 1 遍（同一个库） =====
test result: ok. 7 passed; 0 failed; ...
===== 第 2 遍（同一个库） =====
test result: ok. 7 passed; 0 failed; ...
```

第二遍不是「空库侥幸」：`cleanup()` 在 setup 与每个用例末尾各跑一次，夹具不叠加、不残留。

### 4. 门禁

| 门禁 | 结果 |
|---|---|
| `rustfmt --edition 2024`（本卡 2 个文件） | 0 diff |
| `cargo clippy --lib -- -D warnings` | 0 warning（`Finished dev profile` in 32.94s） |
| `cargo clippy --lib --all-targets -- -D warnings`（覆盖 `#[cfg(test)]`） | 0 warning（in 2m 16s） |

## 三、怎么自己复跑

```bash
# 99 开发机（连 145 的 PG）
bash scripts/verify/m206_transit_flow_drill.sh

# 145 测试机（loopback）
PG_HOST=127.0.0.1 bash scripts/verify/m206_transit_flow_drill.sh

# 只跑本卡 7 条（自备库 + 补 V0405…V0422）
ERP_DB_URL="postgres://cloudpivot:…@172.18.10.145:5432/cp_xxx_flow" \
  cargo test --lib business::erp::inventory_transit::transit_flow_tests -- --test-threads=1
```

## 四、诚实边界

1. **HTTP 端到端没跑**：99 上没有运行实例，本卡走的是 handler 层（`change_shipment_status`
   / `confirm_receipt` / `page_of` / `summary_of` / `reconcile_of`），路由与权限码由编译期注解 +
   M2-04/M2-05 的 `perm_menu_gate` 证据覆盖。本卡零路由、零迁移，不引入新的授权面。
2. **不加 `--test-threads=1` 会失败**：共用全局 `db()`（框架约定，M2-03/M2-04/M2-05 同款）。
3. **夹具前缀 `M206-` 与清场**：`cleanup()` 在 setup 与每个用例末尾各跑一次，演练脚本另加 4 条
   残留断言；别的模块断言全表行数时不会被本组污染（连续跑两遍验证过：第二遍仍 7/7 绿）。
4. **部分签收不在本卡**：现网口径是整单签收（M2-03 决定），所以「部分到货」用**分批发货**造
   （`split_shipments_accumulate_and_drain_transit`），不是同一条在途行的部分签收。
