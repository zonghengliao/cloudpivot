# SO-30 证据包：退货过账（回库 + 冲应收 + 红字凭证）

卡：`t_126909b0` · ARCH-7 §15.2.3 · 日期 2026-11-07

## 一句话

退货单 `PENDING_POST → POSTED` 现在是一个**真事务**：生成退货入库单（三本账经
`inventory_writer` 回库）→ 红字入库凭证（借 1405 / 贷 6401）→ 冲减原应收 →
已开票则造红字发票（OUT_RED，金额全负）+ 收入冲减凭证 → 回写 `delivered_qty` →
释放客户信用占用。任何一步失败整笔回滚（含状态 CAS）。

## 复现命令（一条不漏）

```bash
bash scripts/verify/so30_db_setup.sh                                  # 空库 cloudpivot_so30
bash scripts/verify/so30_instance.sh up                               # :18230，全量迁移 232 个
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so30 \
     -f scripts/verify/so30_posting_fixture.sql                       # 三场景夹具
bash scripts/verify/so30_return_posting_e2e.sh                        # 81 条断言
bash scripts/verify/so30_v0305_drill.sh                               # down 守卫 / down / up / 二次 up
```

## 本包文件

| 文件 | 内容 |
|---|---|
| `e2e_curl.txt` | 真实 token curl 实测 **81/81 全绿**（响应体逐份落 `raw/`，JWT 已脱敏） |
| `raw/` | 每次 curl 的原始响应 JSON（`a_create.json`、`b_posted.json`、`c_posted.json` …） |
| `cargo_test.txt` | `cargo test --lib` → **2888 passed / 0 failed**（含本卡 13 条新单测） |
| `clean_db_replay.txt` | 空库（tables=0）全量重放 232 迁移；V0303/304/305 均 `versioned/executed`；fmt + clippy 口径说明 |
| `clippy_raw.txt` | `cargo clippy --lib` 原始输出（11 条告警全在邻居 WIP `receipt.rs`） |
| `v0305_drill.txt` | `V0305` down 守卫拒绝 → down → up → 二次 up 幂等 |

## 三账 / 应收的关键实测数字（场景 A：未开票 · 退 2 件 @100，13%）

| 断言 | 期望 | 实测 |
|---|---|---|
| 入库单 `in_type / status / source_type` | RETURN / POSTED / SALES_RETURN | ✅ |
| 入库单金额（回库成本 2×60） | 120 | 120.0000 |
| 成本账 物料1 数量 / 金额 | 100→102 / 6000→6120 | ✅ |
| 库位账合计 | 100→102 | ✅ |
| 流水 `biz_type / doc_type` | SALES_RETURN_IN / STOCK_IN，`after = before + qty` | ✅ |
| 红字入库凭证 借 1405 = 贷 6401 | 120 = 120（借贷平衡） | ✅ |
| 应收 amount / unsettled | 1921 → 1695 / 1921 → 1695（状态仍 OPEN） | ✅ |
| `delivered_qty` | 7 → 5 | ✅ |
| 红字发票 | 不造（未开票） | `null` ✅ |
| 信用额度释放 | 同一张新订单：退货前 409「额度不足」，退货后提交不再被拦；额度压到 2100 后被拦（反证守卫在算数） | ✅ |

## 场景 B（已开票 · 全退 12 件 = 1921）

| 断言 | 期望 | 实测 |
|---|---|---|
| 红字发票 | `OUT_RED / ISSUED / -1700 / -221 / -1921 / source_type=SALES_RETURN / receivable_id=原应收` | ✅ |
| 收入冲减凭证 | 借 5001 1700 + 借 2221.02 221 / 贷 1122 1921（借贷平衡） | ✅ |
| 原应收 | `OPEN → CANCELLED`（全额退 + 一分没收），`biz_status_log` 留痕 1 条 | ✅ |
| 库存回库 | 物料1 102→109 / 6120→6540；物料2 50→55 / 6000→6600 | ✅ |
| 红字入库凭证金额 | 7×60 + 5×120 = 1020 | ✅ |
| `delivered_qty` | 7→0 / 5→0 | ✅ |

## 场景 C（已收款 · 退货 791 > 未收 291）—— 原子性证据

`POSTED` 返回 **409**，文案点名「收款退款单」，且以下六项**一分未动**：

```
状态仍 PENDING_POST（状态 CAS 已回滚） / 没有生成入库单 / 库存数量未变
delivered_qty 未变 / 应验收支未变（791|291|PARTIAL）/ 没有多出库存流水
```

## 幂等

* 已 `POSTED` 的退货单再推 `POSTED` → 409（终态无边），同一退货单只有 1 张入库单；
* 库层最后一道：`uk_biz_si_source_doc (source_type, source_id)` 部分唯一索引（V0305）。

## 单测（13 条，纯逻辑 + 边界）

`receivable_after` 六档（部分退 / 全额退作废 / 已收款拒绝 / 超未收拒绝 / 终态拒绝 / 冲减后收清）、
`red_invoice_amounts`（负数符号 + 价税恒等 + 零金额不造票）、`invoice_tax_rate`（夹 0–1）、
`cost_amount_of` / `unit_cost_of`（除零）、信用额度抵减不下穿 0、常量与库层域值逐字对齐。

## 遗留 / 后续卡建议（不在本卡范围）

1. **佣金扣回**（ARCH-7 §15.4.3 ⑥）：退货过账后佣金按比例扣回 —— 佣金模块（SO-35/36）尚未落地。
2. **红字发票的开票额度闸**：本卡在同一事务里先把应收金额调小再造红票，跳过
   `invoice::validate_headroom`（那一刻的额度语义已被本事务改写）；从发票页单独开红票仍需走原闸。
3. **一物一码物料退货**：本卡 400 显式拒绝（回库要走序列号状态流转），与发货侧
   「一物一码不走普通出库单」同口径，属独立卡。
4. **`cargo fmt --check` 的 voucher.rs 漂移**：仓库既有状态（该文件 git 干净），未在本卡处理。
