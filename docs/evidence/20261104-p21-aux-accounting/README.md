# P2-1 凭证辅助核算落地 —— 验收证据（2026-11-04）

> 卡：`t_8c3144b4`【P2-1】凭证辅助核算落地（物料/仓库/部门三个维度永远是空的）
> 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §5 **DC-11**（P2）
> 口径文档：`docs/ERP-辅助核算口径.md`
> 提交：见卡内 `metadata.commit`

## 0. 结论速览

| 卡面验收项 | 结果 | 证据 |
|---|---|---|
| 新建收货单 → 过账 → 凭证分录 维度有值 | ✅ `aux_material_id` / `aux_warehouse_id` / `aux_supplier_id` 有值；`aux_dept_id` **故意为空** | `e2e-run.txt` ② |
| 新建付款凭证 `aux_dept_id` 有值 | ✅ 借方 2202 带申请人部门（来自 `sys_user.dept_id`），银行行不挂 | `e2e-run.txt` ⑤ |
| 单测 ≥ 8 条 | ✅ **18 条**（16 + 付款 1 + 金额自检 1），全绿 | `unit-tests-p21.txt` |
| 真机调辅助账接口 | ✅ `GET /business/erp/ledger/aux-balance` 按物料 / 部门 / 供应商都查出借贷发生额 | `e2e-run.txt` ⑥ |
| 历史凭证辅助列保持为空 | ✅ 生产库 55 条分录前后逐列计数完全一致（27/21/0/15）；库内还放了一张"历史凭证"跑完全流程仍全空 | `db-evidence.txt`、`e2e-run.txt` ⑦ |
| 不变量全绿 | ✅ 生产库 **40/40 通过（BLOCKER 0 / WARN 0）**；测试副本库的残留逐条点名、与本卡无关 | `invariants-prod.txt`、`invariants-README.md` |
| git commit | `feat(erp): P2-1 凭证辅助核算落地` | — |

## 1. 改了什么（按文件）

| 文件 | 改动 |
|---|---|
| `erp/voucher.rs` | `EntryLine` / `EntryRow` / `VoucherLineSnapshot` 增 `aux_dept_id`；`ENTRY_COLUMNS` 与 `write_entries` 落库该列；`reversed_lines` 红冲复制四个辅助列；`VoucherBizCtx` 增 `aux_material_id` + `material_count`（多物料时辅助物料留空 + WARN）；新增 `user_dept_of`（`sys_user.dept_id`，快照语义） |
| `erp/stockin_query.rs` | 新增 `material_count_of`；`voucher_biz_ctx` 取物料种类数（配置路径与过账路径同口径） |
| `erp/stockin_handler_post.rs` | 过账时算去重物料数传给凭证生成 |
| `erp/stockout_handler.rs` | 出库结转凭证：单物料才写 `aux_material_id`，多物料留空 + WARN |
| `erp/payable_payment.rs` | 付款凭证：借方 2202 写申请人部门 `aux_dept_id`，贷方 1002 不挂；查不到部门 → WARN 不阻断 |
| `erp/ledger.rs` | 新增辅助账 `aux_balance` + `GET /business/erp/ledger/aux-balance`（4 维度白名单、期初/本期/期末、`unassigned` 覆盖度自证） |
| `migrations/business/V0214__erp_ledger_aux_balance_permission.{up,down}.sql` | 种 `business:erp:ledger:aux-balance` 权限码（挂「总账 / 科目余额表」菜单，幂等 + 自检） |
| `erp/aux_tests.rs`（新增）+ `erp/mod.rs` | 辅助核算真库用例 17 条 |
| `payable_payment.rs`（测） | 付款部门维度用例 1 条 |
| `scripts/lint/scope_gate.py` | `SKIP_MANIFEST` +1（`business/erp/aux_tests.rs`，测试面） |
| `scripts/verify/p21_{instance.sh,e2e.py,cleanup.sql}` | 隔离实例 / 真机验收（42 项断言）/ 夹具清场 |

## 2. 维度矩阵（哪类凭证补了哪几个维度）

| 业务 | 借方 | 贷方 | 说明 |
|---|---|---|---|
| 采购入库（`STOCK_IN`） | 物料（**仅单物料单**）+ 仓库 | 供应商 | 部门**故意留空**：采购单/收货单没有 `dept_id`（DC-11 → DC-16） |
| 出库 / 领用 / 报废（`STOCK_OUT`） | 物料（仅单物料单），不挂仓库 | 物料 + 仓库 | 6401 / 1901 是全公司口径科目，挂仓库会把报表拆碎 |
| 盘盈盘亏（`STOCK_TAKE`） | 逐物料拆行：物料（+ 盘亏的仓库） | 同左 | 本来就是按物料拆分录，无需改动 |
| 序列号类（`SERIAL_*`） | 物料（+ 存货行仓库） | 物料 | 既有实现，未改 |
| 付款（`PAYMENT`） | 供应商 + **部门** | 供应商（**不挂部门**） | 部门 = 申请人 `sys_user.dept_id` 当前值快照 |
| 手工 / 期初 / 年结 | — | — | 不写维度（本来就没有业务维度） |
| 红字（红冲） | 原样复制原分录四个辅助列 | 同左 | 否则按辅助项查明细会"冲不掉" |

## 3. 单测（真库）

```
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_p16md \
  cargo test --lib p21_ -- --test-threads=1
→ 18 passed / 0 failed（19.05s）
```

覆盖：维度白名单（含大小写与非法值 400）/ 单物料收货三行维度写法 / 多物料汇总行留空 /
`VoucherBizCtx` 的 `material_id` 与 `aux_material_id` 分离语义 / 红冲复制四列 /
四个辅助列落库往返 / 辅助账按物料（期初-本期-期末）/ 按维度列全部辅助项 / `unassigned` 覆盖度 /
按供应商 / 按部门 / 按仓库 / 期间边界 / 参数校验 / `user_dept_of`（有部门 / 无部门 / 用户不存在）/
付款凭证部门维度 / 清场。

## 4. 真机（隔离实例 + 真 HTTP）

```
bash scripts/verify/p21_instance.sh start      # 库 cloudpivot_p16md，端口 18099
python3 scripts/verify/p21_aux_e2e.py --port 18099 --db cloudpivot_p16md
→ 合计：PASS 42 / FAIL 0
```

关键数字（`e2e-run.txt`）：

* 单物料收货 100 @ 8：存货行借方 800、`aux_material_id` = 物料甲、`aux_warehouse_id` = 收货仓、
  应付行 `aux_supplier_id` = 供应商，**三行 `aux_dept_id` 全空**；
* 多物料收货（20 + 30）：存货行只有 1 条汇总行，`aux_material_id` **为空**、仓库照写；
* 领用 10：出库凭证存货行带物料 + 仓库；
* 付款 100：借方 2202 `aux_dept_id` = 采购部，贷方 1002 不挂部门；
* 辅助账：物料甲 1405 本期借 800 / 贷 80 / 期末 720；部门本期借 100；供应商贷方 > 0；
  非法 `aux_type` → 400；`unassigned` 本期借方 > 0；
* 那张"历史凭证"跑完全流程后四个辅助列仍然全空。

> 说明：验收库里 `payable_pay` 审批流程是**发布态**，引擎按 DC-01 拒绝 `skip_approval`。
> 脚本按引擎提示先 `POST /business/flow/def/{id}/offline` 下线、跑完再 `enable` 恢复
> （审批链本身由 `pflow3b` 用例覆盖，本卡只验凭证辅助核算）。

## 5. 存量不回填（生产库只读核对）

```
改造前（开工时实测）  55 条分录：aux_material 27 / aux_warehouse 21 / aux_dept 0 / aux_supplier 15
改造后（收尾时实测）  55 条分录：aux_material 27 / aux_warehouse 21 / aux_dept 0 / aux_supplier 15
```

逐列计数完全一致 ⇒ 生产库一条历史分录都没动过（代码里也没有任何 `UPDATE biz_voucher_entry`
回填语句）。

## 6. 复现命令

```bash
# 单测
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_p16md' \
  cargo test --lib p21_ -- --test-threads=1

# 真机
bash scripts/verify/p21_instance.sh start
python3 scripts/verify/p21_aux_e2e.py --port 18099 --db cloudpivot_p16md

# 不变量（对测试库）
NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_DATABASE_NAME=cloudpivot_p16md \
  python3 scripts/verify/erp_invariants.py

# 清场
psql -h 127.0.0.1 -U alun -d cloudpivot_p16md -f scripts/verify/p21_cleanup.sql
bash scripts/verify/p21_instance.sh stop
```

## 7. 已知缺口（**已报告，未在本卡处理**）

采购入库 / 出库的存货借方是**一条汇总行**（多物料汇总），而分录只有一组辅助列。
本卡的做法是「多物料时物料维度留空 + WARN 点名单号」，**没有**为了填满而拆分录
（卡面明确禁止）。正确解法是按物料拆分录（`biz_stock_in_item.amount` 逐行拆、末行吸尾差、
总额不变），它已经是盘点凭证的做法，但动分录取数会改 `entry_count`、影响 D3 影子对比
与一批既有断言 ⇒ 独立决议卡 **P2-1b**。
