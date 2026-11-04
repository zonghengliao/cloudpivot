# P2-1b 实施：收货 / 出库凭证按物料拆分录 —— 验收证据（2026-11-04）

> 卡：`t_1dee3cc9`【P2-1b-实施】收货/出库凭证按物料拆分录
> 依据：`docs/ERP-辅助核算口径.md` **§4（P2-1b 决议，commit `7ada1c5`）**
> 前序：`t_8c3144b4`【P2-1】辅助核算写入 + 辅助账接口（`722f030`）
> 切换日：**2026-11-04**（代码落地当天；生产生效时点 = 下次 prod 重启部署）

## 0. 结论速览

| 卡面验收项 | 结果 | 证据 |
|---|---|---|
| ① 单测 `business::erp` / `business::flow` 全绿 + 6 类新用例 | ✅ `erp 893 passed / 0 failed`、`flow 339 / 0`、`flow_p0fix 7 / 0` | `unit-tests-erp.txt` / `unit-tests-flow.txt` / `unit-tests-flow-p0fix.txt` |
| ② 尾差复算 SQL | ✅ `SPLIT_SUM=1000.0000  HEADER=1000.0000  TAIL_DIFF=0.0100`（与期望逐字一致） | `tail-diff-sql.txt` |
| ③ 真机 e2e（隔离实例 + 副本库） | ✅ `p21_aux_e2e` **56/56**、`d3_generate_voucher_e2e` **66/66**（B3/B6 仍 3 行 + 新增多物料 SHADOW 一致）、`dc13_stock_out_e2e` **53/53** | `e2e-p21-aux.txt` / `e2e-d3.txt` / `e2e-dc13.txt` |
| ④ 切换点证据（3 物料收货 / 2 物料出库） | ✅ 收货 **5 行**（1405×3 + 税 + 应付）、逐行物料、`Σ1405 = 单头 260`、该单 1405 行 `aux_material_id` 空行数 = 0；出库 **4 行** | `switch-point.txt`（20/20） |
| ⑤ 不变量 | ✅ 生产库只读 **40/40（BLOCKER 0 / WARN 0）**；**干净库只放本卡夹具也 40/40**；测试副本库残留逐条点名（见 §6） | `invariants-prod.txt` / `invariants-clean-db.txt` / `invariants-testdb-p16md.txt` |
| ⑥ `scripts/check.sh` `EXIT=0` | ✅ fmt / clippy `-D warnings` / check / 前端 typecheck / 批次B 闸①② / C7 六条门禁全过 | `check.txt` |
| ⑦ 推送 | 见卡内 `metadata.commit` / `branch_head`（`git ls-remote` 回读与 HEAD 一致） | — |

## 1. 改了哪些文件（按 §4.5 影响面）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/voucher.rs` | 新增 `VoucherMaterialLine{material_id, qty, amount, line_from, line_to}` + `normalize()`（按 `material_id` 升序 + 同物料合并）；新增 `absorb_tail_diff()`（尾差吸收，吸收不了 `InvalidValue` 报错 + WARN 点名）；新增 `material_inventory_accounts()`（**一条 SQL** 批取逐物料存货科目：规则 `debit_account_id` → 该物料 `inventory_account_id` → `1405`）+ `material_codes()`；`purchase_in_lines()` 增 `material_lines`/`material_accounts`/`doc_no`，空 ⇒ 回退单头一行，非空 ⇒ 每物料一行 1405（`aux_material_id` + `aux_warehouse_id`）尾差入末行；`VoucherBizCtx` 的 `aux_material_id`/`material_count` → `material_lines`（`material_id` 保留）；`create_by_rule` 逐物料解析科目 + 删「多物料留空 + WARN」；`create_for_stock_in` 签名带 `material_lines`；`StockOutVoucher` 增 `material_lines`，`create_for_stock_out` 出 `2M` 行（借成本科目×M 不挂仓 / 贷 1405×M 挂仓），空 ⇒ 保留 2 行回退 |
| `backend/src/business/erp/stockin_query.rs` | `first_material_of` + `material_count_of` → **`material_lines_of`**（一条聚合 SQL：`material_id, sum(amount), sum(qty), min/max(line_no)`，`GROUP BY material_id ORDER BY material_id`）；`voucher_biz_ctx` 改用它 |
| `backend/src/business/erp/stockin_handler_post.rs` | 过账路径用 `VoucherMaterialLine::normalize(ordered…)` 在内存明细上聚合（与配置路径同口径），删「多物料留空」判定，留一条 `info` 记 `M` |
| `backend/src/business/erp/stockout_handler.rs` | 过账循环里把 `(item.material_id, item.qty, applied.amount, line_no)` 逐行交给凭证层，删 `distinct_materials` + WARN 段 |
| `backend/src/business/erp/aux_tests.rs` | 3 条「多物料留空」断言改写为 7 条新口径用例（见 §3） |
| `backend/src/business/flow/linkage/tests.rs` | D3 SHADOW 一致性用例改用 `material_lines_of`（签名同步） |
| `scripts/verify/p21_aux_e2e.py` | ③ 改写为「存货行 2 条（甲 160 / 乙 150）+ 各带物料 + 仓库照写 + 合计 310 = 单头」，新增「Σ 1405 = 单头」「按物料辅助账 = 该物料成本」；新增 ③b 多物料出库 4 行；⑥ 的甲 1405 数字随之更新（960/160/800） |
| `scripts/verify/d3_generate_voucher_e2e.py` | 新增 C13–C22：**多物料单的 SHADOW 一致性**（配置路径 vs 旧路径逐行一致、凭证号相同） |
| `scripts/verify/p21b_switch_point.py` | **新增**：切换点证据脚本（3 物料收货 5 行 / 2 物料出库 4 行 / 逐行物料 / `Σ1405 = 单头` / 按物料辅助账） |
| `docs/ERP-辅助核算口径.md` | §2 表格改为「逐物料一行」；§4.7 点名切换日 2026-11-04 与证据路径（正文口径未改） |

**没动**（卡面禁止项，逐条核对过）：`stocktake_model.rs`、`snapshot_of` / `compare_snapshots` /
`reversed_lines`、`scripts/verify/erp_invariants.py`、任何迁移（无 schema 变化）、
任何 `UPDATE biz_voucher_entry` 回填（生产代码 0 处；仅 `aux_tests.rs` / `year_end.rs` 的
`#[cfg(test)]` 夹具里各 1 处，属既有测试代码）、`aux_qty` / `aux_extra`（0 处写入）。

## 2. 行数表（§4.3 逐字落地）

| 来源 | 借方 | 贷方 | `entry_count` | 实测 |
|---|---|---|---|---|
| 采购入库（含税） | `1405`×M + `2221.01`×1 | `2202`×1 | `M + 2` | M=2 ⇒ 4 行、M=3 ⇒ 5 行 ✅ |
| 采购入库（零税） | `1405`×M | `2202`×1 | `M + 1` | M=3 ⇒ 4 行 ✅ |
| 出库 / 领用 / 报废 | 成本科目（6401/1901）×M | `1405`×M | `2M` | M=2 ⇒ 4 行 ✅ |
| `M = 1`（回归底线） | `1405`×1 + 税 | `2202`×1 | 收货 3 / 出库 2 | D3 B3/B6 原样 3 行 ✅ |

## 3. 单测（真库，按模块分库 + 分进程）

```bash
# business::erp —— 干净库 cp_p21b_erp（启动迁移到 V0216）
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_p21b_erp' \
  cargo test business::erp -- --test-threads=1
→ test result: ok. 893 passed; 0 failed; 0 ignored  （1064.76s）

# business::flow —— 干净库 cp_p21b_flow（internal_control::db_tests 归 p0fix 模块独占进程）
ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cp_p21b_flow' \
  cargo test business::flow -- --test-threads=1 --skip business::flow::internal_control::db_tests
→ test result: ok. 339 passed; 0 failed; 0 ignored  （23.32s）

# 那 7 条要求「连的必须是 P0FIX 库」
ERP_DB_URL=…cp_p21b_p0fix P0FIX_DB_URL=…cp_p21b_p0fix \
  cargo test business::flow::internal_control::db_tests -- --test-threads=1
→ test result: ok. 7 passed; 0 failed; 0 ignored
```

新增/改写的 7 条用例（`aux_tests.rs`，覆盖卡面 ①–⑥）：

| 用例 | 钉住什么 |
|---|---|
| `p21_purchase_in_lines_write_material_and_warehouse_only` | ① `M=1`：回退分支与按物料拆**逐行一致**（3 行 / 物料 / 仓库 / 部门留空） |
| `p21_purchase_in_lines_split_per_material` | ② `M=2` 含税 4 行、`M=3` 零税 4 行；逐行物料按 `material_id` 升序；行号连续重排；Σ 存货 = 单头 |
| `p21_purchase_in_lines_merge_same_material` | ③ 同物料两行合并（金额相加、摘要写「第 1–2 行」） |
| `p21_absorb_tail_diff_lands_on_last_material` | ④ 尾差 333.33×3 / 1000.00 ⇒ 末行 333.34、Σ=1000.0000；吸收后 ≤0 往前退；全吸收不了 ⇒ 报错点名单号 |
| `p21_purchase_in_lines_skip_zero_amount_material` | ⑤ 金额 0 的物料不出行 |
| `p21_split_resolves_each_materials_own_inventory_account` | ⑥（真库）规则无 `debit_account_id` + 两物料各有科目 ⇒ 两行**两科目**（走 `create_by_rule` 全链路，事务内建夹具后回滚） |
| `p21_biz_ctx_carries_material_lines_sorted` | `of_stock_in` 归一化（升序/合并）、`material_id` = 首个物料、空物料行 ⇒ 回退 |

## 4. 尾差复算 SQL（读数与期望逐字一致）

```
SPLIT_SUM=1000.0000  HEADER=1000.0000  TAIL_DIFF=0.0100
```

复算 SQL（见 `tail-diff-sql.txt` 的原始输出命令）：

```sql
with items(line_no, amount) as (values (1,333.33::numeric),(2,333.33::numeric),(3,333.33::numeric)),
header(total_amount) as (values (1000.0000::numeric)),
calc as (select line_no, amount, (select total_amount from header) - sum(amount) over () as diff,
                max(line_no) over () as last_line from items)
select 'SPLIT_SUM=' || sum(case when line_no=last_line then amount+diff else amount end)::text
       || '  HEADER=' || (select total_amount::text from header) || '  TAIL_DIFF=' || max(diff)::text
from calc;
```

## 5. 真机 e2e（隔离实例 + 副本库，不碰生产）

```bash
bash scripts/verify/p21_instance.sh start            # 库 cloudpivot_p16md，端口 18099
python3 scripts/verify/p21_aux_e2e.py        --port 18099 --db cloudpivot_p16md   # PASS 56 / FAIL 0
python3 scripts/verify/p21b_switch_point.py  --port 18099 --db cloudpivot_p16md   # PASS 20 / FAIL 0
bash scripts/verify/d3_instance.sh start             # 库 cloudpivot_d3，端口 18096
python3 scripts/verify/d3_generate_voucher_e2e.py                                 # PASS 66 / FAIL 0
bash /…/run_dc13.sh                                  # 库 cp_dc13_e2e，端口 18097 → 53/53
```

关键数字：

* **`p21_aux_e2e` ③**：多物料收货（甲 20×8 = 160 / 乙 30×5 = 150，税 13%）⇒ 凭证 4 行
  （`1405`×2 + `2221.01`×1 + `2202`×1），存货行逐行带物料、都挂收货仓、`Σ1405 = 310 = 单头`；
  `按物料辅助账：乙 1405 本期借 150 / 期末 150`。
* **`p21_aux_e2e` ③b**：多物料出库（甲 10 + 乙 10）⇒ 4 行（借 `6401`×2 各 80/50、贷 `1405`×2 各挂仓）。
* **`p21_aux_e2e` ⑥**：甲 1405 本期借 **960**（800 + 160）/ 贷 **160**（80 + 80）/ 期末 **800**。
* **`d3_generate_voucher_e2e`**：B3 / B6 **仍是 3 行**（fixture 单物料）；新增 C15–C22 多物料单
  `entry_count = 4`、逐行物料、`Σ1405 = 单头 200`、影子对比 `matched=true diffs=[]`、
  两路凭证号相同（`PZ-20261104-000003`）—— 证明**拆分只写在共享 builder 里**。
* **`dc13_stock_out_e2e`**：出库 / 报废 / 盘亏 / 作废红冲全链 53/53（红冲自动保留拆分行数）。

## 6. 切换点证据（卡面 ④，逐条 SQL 输出见 `switch-point.txt`）

3 物料收货（甲 10@10 + 乙 20@5 + 丙 30@2，税 13%）过账后的分录（原样 SQL 输出）：

```
1|1405|100.0000|0.0000|P21BSP-M1|<收货仓>
2|1405|100.0000|0.0000|P21BSP-M2|<收货仓>
3|1405|60.0000|0.0000|P21BSP-M3|<收货仓>
4|2221.01|33.8000|0.0000|(空)|(空)
5|2202|0.0000|293.8000|(空)|(空)
```

* `entry_count = M + 2 = 5`、`Σ1405 = 260.0000 = 单头 total_amount`；
* 该单 1405 分录里 `aux_material_id IS NULL` 的行数 = **0** ⇒ **不再进 `unassigned`**
  （`unassigned` 里剩下的都是切换日之前的存量，§4.7 不回填）；
* 2 物料出库（甲 5 + 乙 5）⇒ 4 行：`6401` 借 50/25（逐行物料、不挂仓）、`1405` 贷 50/25（逐行物料 + 仓）；
* 按物料辅助账：丙 的 1405 本期借 60 / 期末 60（= 该物料成本）。

## 7. 不变量

| 库 | 结果 | 说明 |
|---|---|---|
| **生产库**（144 : cloudpivot，只读） | **40/40 通过，BLOCKER 0 / WARN 0** | `invariants-prod.txt`；本卡**未部署**生产，读数即切换前基线 |
| 干净库 `cp_p21b_inv`（只放本卡夹具） | **40/40 通过，BLOCKER 0 / WARN 0** | `invariants-clean-db.txt` —— 证明新的拆分分录本身账账相符 |
| 测试副本库 `cloudpivot_p16md` | 29/40（BLOCKER 6 / WARN 5） | `invariants-testdb-p16md.txt`；**全部是别的卡的夹具残留**，逐条见下表 |

**副本库残留逐条点名（与本卡无关）**：

| 不变量 | 副本库上的体现 | 归属（夹具号） |
|---|---|---|
| A2 / A3 | 1 张头与明细不一致 / `entry_count` 计数错 | `P2C2-V-0001`（有头无分录）→ P2-C2 用例 |
| B1 / B4 | 5 组库位账 ≠ 成本账；8 组流水 ≠ 库存 | `W2C2-M-PLAIN` / `W2C1-M-PLAIN` / `P2C1-M1` / `P2C1-M2` / `W2C2-M-RT` / `W2B-M-SN` / `W3B-M-PLAIN` / `W3B-M-SN` → W2B/W2C1/W2C2/W3B/P2C1 用例 |
| B2 | 库存 2935 / 总账 1405 1114 / 差 1821 | 上一条的库存无凭证；其中 9 元来自 `p21_aux_e2e.py` ⑦**故意**造的「历史凭证」（1405 借 9、无库存对应，用来证明存量不回填），P2-1 记录为 1830 ⇒ 差额 -9 完全是这条夹具 |
| B3 | 应付未付 1910.1 / 总账 2202 1377.1 / 差 533 | 付款/应付夹具未配对（P2-C2 / P7 用例） |
| C5 | 在库序列号 2 件 vs 账面 3 | `W2C2-M-SN`（W2C2 序列号用例） |
| D1 | 1 张已过账收货单无凭证 | `D2L-SI-2`（D2 联动用例） |
| D2 | 1 张自动凭证无 `source_no` | 早前用例残留 |
| E4 | 8 组库存大于有效入库量 | 同 B1/B4 那批 W2/W3 夹具 |
| F2 | 2 张凭证的期间无期间记录 | `0201`（2026-08）/ `P2C2-V-0001`（2026-03） |

> 本卡自己的夹具（`P21E2E-*` / `P21BSP-*`）**没有出现在任何一条**定位里；把它们单独放进
> 干净库跑，不变量 40/40（上表第 2 行）。

## 8. 展示自检（§4.5 前端一行）

凭证详情 / 分录表格按 `detail.entries` 动态渲染（`{{ detail.entries.length }} 行`，
`views/business/erp/voucher/index.vue:321/346`），无行数上限、与分页无耦合 ⇒
5 行 / `2M` 行凭证不需要前端改动（未改前端代码）。

## 9. 诚实边界

* **生产未部署**：本卡只推代码；prod 未重启 ⇒ 线上仍跑旧逻辑，切换**生效**时点 = prod 重启部署日。
  判断某张单属于哪一边，看 `biz_voucher_entry.aux_material_id` 是否为空（老单恒空），不要按凭证日期猜。
* **存量不回填**：生产库 55 条分录的四个辅助列计数（27 / 21 / 0 / 15）与 P2-1 收尾时**完全一致**，
  代码里没有 `UPDATE biz_voucher_entry` 回填语句。
* **e2e 用的是副本库 + 隔离实例**（本机 `alun` 无 `CREATEDB`；新库由 `postgres` 建、后端二进制启动迁移），
  与生产同 schema（V0216）。
* `aux_qty` / `aux_extra` 仍留空（§4.6），本批没有数量账消费方。
