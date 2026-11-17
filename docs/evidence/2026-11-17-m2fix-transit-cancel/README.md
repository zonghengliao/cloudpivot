# M2-FIX 出库待确认单作废 → 释放在途 · 证据

- 卡：`t_f123b57f`（M2-FIX【M2·仓库】出库待确认单作废时释放在途（在途行置 CANCELLED））
- 上游：`t_5f324a4c`（M2-03 签收确认 → 在途转已出库，commit `46d85d0d`）/ `t_6c090f97`（M2-02 发货过账 → 在途 + 出库待确认）
- 执行环境：99（开发机）→ PG `172.18.10.145:5432`，模板库 `cloudpivot_test`（停在 V0404，脚本/脚本手工补迁移）
- 结论：**演练脚本 24/24 断言全绿**（`00-drill.out`，`PASS=24 FAIL=0 / ALL PASS`）；本卡模块 10 条用例全绿（`01-unit-tests.out`，真库 0 SKIP）；宽回归 590 条全绿（`02-wide-groups.out`）；`fmt` / `clippy --lib` / `clippy --all-targets`（均 `-D warnings`）/ `check.sh --l0` 全通过（`03-gates.out`、`04-check-l0.out`）

## 一、缺口与修法

M2-02 起「发货过账」只做两件事：写一条在途（`biz_inventory_in_transit`）+ 生成一张停在
`PENDING_POST`（= 出库待确认）的销售出库单，**库存一分不扣**。可用量口径是
`可用 = 账面 − 未签收在途`。M2-03 让「签收确认 / 出库单过账」把在途推到 `RECEIVED`。

遗留缺口：把这张**出库待确认**单作废时，在途行没人处理 ——

```text
作废（cancel_out_tx 的 from_status != POSTED 分支）→ 账没动、直接 return
  ⇒ 该发货单的在途行仍是 IN_TRANSIT
  ⇒ 可用量被永久扣着这批货；单据已是终态 CANCELLED、界面上无按钮可点
  ⇒ 只能改库才能恢复（约定 11 的反面：不许留需要工程师改库才能恢复的状态）
```

修法（零迁移 / 零新权限码 / 零前端改动）：

```text
stockout_handler::cancel_out_tx   from_status ∈ {DRAFT, PENDING_POST} 且 source_type='SALES' 且 source_id 有值
  └─ shipment_transit::cancel_open(ex, shipment_id, operator, now, Some(reason))   ← 同一事务
       · 锁该发货单 status='IN_TRANSIT' 的行（SELECT … FOR UPDATE）
       · 逐行 UPDATE：status='CANCELLED'，remark=作废原因，updated_at/update_by
       · 数量列一律不动（shipped/received 是「发过货」的历史事实；V0420 的
         ck_biz_inv_transit_state 对 CANCELLED 不限数量，所以一条 UPDATE 就够）
       · CAS：affected=0 → 409（行被别人改掉就整笔回滚，不留半截账）
  └─ 作废事务回滚 ⇒ 在途行一起回滚（同一事务，不是提交后补偿）

POSTED 分支：**不碰在途**。那条路走红冲（三本账回滚 + 红字凭证 + 冲销应收），
在途早在过账 ⑤c 里 RECEIVED 了；再作废一次 = 把已签收的量重复放回可用量。
```

## 二、为什么收口在 `cancel_out_tx` 而不是某个入口

出库单的状态流转只有一条入口（`PATCH /business/erp/stock-out/{id}/status` → `change_status`
→ `cancel_stock_out` → `cancel_out_tx`），发货单作废走不通（`POSTED` 是终态）。所以在
`cancel_out_tx` 里加，任何未来新增的作废入口都自动带上，不会出现「第二条路忘了释放在途」。

## 三、真发现（只报不改，需单开卡）

1. **出库单边表没有 `PENDING_POST → CANCELLED`**。
   `state_machine::STOCK_OUT_EDGES` 是 6 条边，作废只允许 `DRAFT → CANCELLED` 与
   `POSTED → CANCELLED`。所以「出库待确认」单要作废，真实操作路径是 **撤回 → 作废** 两步
   （`PENDING_POST → DRAFT` 再 `DRAFT → CANCELLED`）。而 `cancel_out_tx` 的注释与实现都写着
   `from_status ∈ {DRAFT, PENDING_POST, POSTED}`（`stockout_handler.rs` 的「状态流转」表也这么写）
   —— **文档/实现与边表口径不一致**，`PENDING_POST → CANCELLED` 那段目前是死代码。
   本卡按现状实现：`cancel_out_tx` 两个起始态都做释放，边表将来放开即自动生效；
   用例按可达路径（撤回→作废）写，并额外钉住「撤回**不**释放在途」（单据还可能再提交）。
   要不要放开这条边（一步作废）是产品/流程决定，牵动 `STOCK_OUT_EDGES` / `STOCK_OUT_BASE` /
   流程配置 UI，本卡不动。
2. **作废出库待确认单不回退订单行 `delivered_qty`**。作废只撤在途，`biz_sales_order_item.delivered_qty`
   仍是发货时 `+= qty` 的值 ⇒ 同一订单行不能重发同一批（超发闸 `delivered_qty + qty <= qty` 会拦）。
   本卡用例用「订单行 12 / 两批发货各 6」绕开；是否要在作废时回退 `delivered_qty`（以及是否连带
   回退订单状态 `OUTBOUND → APPROVED`）属 M2-02/M2-05 口径，本卡只报不改。

## 四、验证

| 证据 | 内容 |
|---|---|
| `00-drill.out` | `bash scripts/verify/m203_shipment_receipt_drill.sh` → `PASS=24 FAIL=0 / ALL PASS`（含 3b 新增的 4 条作废不变量 + 正向计数） |
| `01-unit-tests.out` | `business::erp::shipment::shipment_receipt_tests` 10 条全绿（7 条真库 + 3 条纯逻辑；旧 7 条 + 本卡新增 3 条，无 SKIP） |
| `02-wide-groups.out` | 补齐 V0421…V0441 的克隆库上：sales 315 · shipment 49 · stockout 60 · receivable 58 · linkage 108 全绿（0 SKIP） |
| `02b-wide-groups-partial-schema.out` | 同一批模块停在 V0420 的库上跑：sales 81 条红 —— **全是缺 V0421+ 表**（`biz_quotation` / `biz_sales_rebate_*` 不存在），补全后 315 全绿；留档说明「库落后 ≠ 代码红」 |
| `03-gates.out` | `cargo fmt --check` 0 diff；`clippy --lib -- -D warnings` 0 告警；`clippy --all-targets -- -D warnings` 0 告警 |
| `04-check-l0.out` | `bash scripts/check.sh --l0` → 全部检查通过（含 test_seed_gate / 迁移号唯一闸 / 库存三本账写入口闸） |

新增 3 条用例（`shipment_receipt_tests.rs`）：

- `cancel_pending_stock_out_releases_transit` —— 账面 6、发 6（在途 6）⇒ 可用量 0，同一批货再发必被
  「库存不足（在途未签收）」拦下（缺口现场）；撤回（不释放在途）→ 作废 ⇒ 在途行
  `6/0/6/CANCELLED/false`、备注 = 作废原因、`Σ(qty_in_transit WHERE status='IN_TRANSIT')=0`、
  三本账/流水/凭证/应收一分没动、**同一批货再发能过账**、二次作废 409 且不覆盖备注。
- `cancel_posted_stock_out_leaves_received_transit_alone` —— 签收后作废（走红冲）：在途行仍
  `6/6/0/RECEIVED/true`、备注仍是签收文案、库存回 20、凭证蓝红各一张。
- `transit_release_only_applies_to_sales_source` —— 纯逻辑：判据只认 `source_type='SALES'`。

演练脚本新增（`scripts/verify/m203_shipment_receipt_drill.sh` §3b，取样前单独再跑一遍作废用例，
避免空表假绿）：

1. `biz_stock_out.status='CANCELLED' AND source_type='SALES'` ⇒ 该发货单**没有** `IN_TRANSIT` 在途行（0 行）
2. 单据视角：该出库单对应发货单的 `Σ(qty_in_transit WHERE status='IN_TRANSIT')=0`（0 行）
3. 正向计数 ≥1：确有「作废 ⇒ 在途行 CANCELLED 且 remark 非空」的样本
4. 作废行数量列原样（`shipped_qty > 0 AND received_qty = 0`）

## 五、复现

```bash
# 1) 全量演练（自带库克隆 + 补迁移 + 跑用例 + SQL 复核，跑完自删库）
bash scripts/verify/m203_shipment_receipt_drill.sh          # 99 → 145
PG_HOST=127.0.0.1 bash scripts/verify/m203_shipment_receipt_drill.sh   # 在 145 上

# 2) 只跑本卡模块（真库用例必须串行）
HOST=172.18.10.145
PGPASSWORD=*** createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_m2fix_t
ERP_DB_URL="postgres://cloudpivot:<pwd>@$HOST:5432/cp_m2fix_t" \
  cargo test --lib business::erp::shipment::shipment_receipt_tests -- --test-threads=1
PGPASSWORD=*** dropdb -h $HOST -U cloudpivot cp_m2fix_t
```

⚠ 模板库 `cloudpivot_test` 停在 **V0404**：只补 `V0405…V0420` 时 sales 那 81 条会红（缺
`biz_quotation` / `biz_sales_rebate_*`），**补到 V0441 才全绿**（见 `02-wide-groups.out`）。
