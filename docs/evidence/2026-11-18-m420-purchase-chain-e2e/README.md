# M4-20 采购缺口 E2E（计划 → 申请 → 订单 → 入库）· 验收证据

- 卡：`t_6a4a60ba`（M4-20【M4·采购】采购缺口 E2E：计划 → 申请 → 订单 → 入库）
- 日期：2026-11-18
- 性质：**验证卡**（只补全链用例 + 演练脚本；零迁移、零权限码、零路由、零前端）
- 数据库：演练库建在 **145（172.18.10.145:5432，用户 `cloudpivot`）**，从模板 `cloudpivot_test`
  克隆、补 V0405…V0458（54 条）、跑完脚本自己 DROP

## 一、这一份钉的是什么

**一张年度采购计划怎么一路变成仓里的库存和应付账款。** 前面几张卡各自钉死了自己那一段
（M4-02 计划 CRUD+审批 / M4-03 计划转申请 / M4-04 执行率 / 收货域建单过账），但**没有一条
用例把它们串起来** —— 在 M4-04 的用例里收货从未发生过，在收货域的用例里来源采购单是 SQL 夹具。
于是「计划转出来的申请、合并成的订单，能不能真的被批、真的收、真的落账」这件事，
在库里从来没有被验证过。这就是本卡要补的缺口（与 M3-24 销售缺口 E2E 同构）。

## 二、交付物

| 文件 | 行数 | 说明 |
|:--|--:|:--|
| `backend/app/src/business/erp/purchase/purchase_chain_e2e_tests.rs` | 1494 | 全链 3 条用例：全链 1 条 + 分批收货 1 条 + 清场 1 条；**118 条断言 + 6 条反证（`expect_err`）** |
| `backend/app/src/business/erp/purchase/mod.rs` | +4 | 声明测试模块（`#[cfg(test)] mod purchase_chain_e2e_tests;`） |
| `scripts/verify/m420_purchase_chain_e2e_drill.sh` | 237 | 演练脚本：克隆 → 补迁移 → 前置断言 → 清污 → 3 组用例 → 夹具零残留；`TEETH=1` 有牙自检 |
| `docs/evidence/2026-11-18-m420-purchase-chain-e2e/*` | — | 本目录（README + 4 份原始输出） |

## 三、链路与断言（10 步，逐步从库里取事实）

```text
① 建采购计划（DRAFT，PPL-yyyyMMdd-nnnnnn，2 行 / 同一仓，100@2.5 + 10@100 = 1250）
   断言：单号前缀 / 状态 DRAFT / 类型年度 / 金额 1250 / 2 行明细 / 执行率「未开工」
② 反证：DRAFT 计划转申请 → 409（文案要指名叫先批准），且不许留下申请单
③ 计划提交审批 → pp_approve → 逐节点真审批 → APPROVED（回调把计划推到 APPROVED）
④ 计划转申请（一仓一单 → 1 张 PRQ，税率 13%）
   断言：PRQ- 前缀 / DRAFT / 数量 110 / 预估金额 1250 / source_plan_id 回链 / 2 行带 plan_item_id
⑤ 反证：DRAFT 申请转采购单 → 409，且不许留下采购单
⑥ 申请提交审批 → pr_approve → 逐节点真审批 → APPROVED
⑦ 申请合并转采购订单（PO）
   断言：PO- 前缀 / DRAFT / 数量 110 / 不含税 1250 / 税 162.5 / 价税合计 1412.5 /
        供应商与收货仓沿用 / 申请单转 CONVERTED 且回填 po_id+po_no+converted_at
⑧ 反证：DRAFT 采购单收货 → 400（只有 SUBMITTED/APPROVED/PARTIAL_RECEIVED 能收），且不许留下收货单
⑨ 采购单提交审批 → po_approve（<1 万走部门主管）→ APPROVED；待收 = 订单量、已收 0
⑩ 建收货单（items 留空 = 服务端带出未收完的行）→ DRAFT
   断言：IN- 前缀 / DRAFT / 数量 110 / 金额三个数 / in_type=PURCHASE /
        2 行带 source_item_id / **库存仍为 0**、无凭证、无应付、采购单状态不动（草稿不算已收）
⑪ 过账（POSTED）→ 六本账一次落
   断言：成本账 100/10（金额 250/1000，**不含税**）+ 库位账同量 + 流水 PURCHASE_IN /
        凭证 1 张（借 1405 合计 1250、借 2221.01 162.5、贷 2202 1412.5、借贷平衡）/
        应付 1 张（AP- 前缀、金额 1412.5、OPEN、来源单号=收货单号）/ 收货单回写 voucher_id+payable_id /
        采购单 RECEIVED + 已收数量派生 = 订单量 + 待收 0 + can_receive=false /
        计划执行率：已申请 110、已下单 110、剩余 0、状态「已下满」、执行率 1.00
⑫ 收口：重复过账 409 / 已收满再收 400（文案带「收满」）/ 申请二次转单 409 且全链只有 1 张 PO /
        计划可转量 0 / 三道审批实例无 PENDING（无死单）/ 采购单与收货单状态日志条数达标
```

第二条用例（`partial_receipt_advances_po_and_keeps_plan_execution`）钉住「分批」：

```text
先收物料甲 40 → 采购单 PARTIAL_RECEIVED、已收 40（派生）/ 待收 60 + 10 /
第一批自己一套凭证+应付（113）/ **计划执行率不受收货影响（仍是已下满）**
再收剩余 70（带出 60+10）→ 采购单 RECEIVED、库存合计 100+10 /
两批应付合计 = 1412.5、两批凭证借方合计 = 1412.5（不许重复记账也不许漏记）
```

## 四、实测到的五个坑（都已写进用例注释 / 脚本注释，后来人别再踩）

1. **模板库 `cloudpivot_test` 停在 V0404**：`biz_purchase_plan`（V0446）与 `pp_approve` 流程
   （V0447）都不存在，演练必须先回放 V0405…V0458（54 条），否则用例会在「表/流程不存在」上
   红成假故障。
2. **待收清单的行序不是计划行序**：`to_po` 用 `merge_by_material` 合并，行号顺序不由计划行决定。
   首版按 `items[0]` 断言「物料甲已收 40」直接假红（实际 items[0] 是物料乙）。
   → 断言一律按**物料 id** 取行（`pending_line(&res, material)`），不按下标。
3. **`biz_purchase_requisition` 没有 `total_qty` 列**（只有 `est_amount`）：申请单数量要从
   `biz_purchase_requisition_item` 聚合，别照采购单/收货单的列清单抄。
4. **建单不动账**：收货单建出来是 `DRAFT`，库存 / 凭证 / 应付**一条都没有**，采购单状态也不动
   （已收数量是「已过账收货单」的派生值，P2-3 起没有回写列）。把建单当入账写断言必假红。
5. **成本口径只取不含税金额**：`biz_inventory.amount` = 1250（不是 1412.5），进项税 162.5 单独走
   `2221.01`，应付贷方 2202 = 价税合计 1412.5。三个数混着写必假红。

另：**真库用例的假绿是真的** —— 通用 `ensure_db()` 连不上只打印 `SKIP:` 后 `return`，
cargo 眼里那是 `ok`。本文件写了自己的 `ensure_db_or_panic()`（配了 `ERP_DB_URL` 却连不上就 panic），
演练脚本再数一遍「SKIP 行数必须 0」，两道防线（`02_teeth_bad_url.out` 是坏连接串下的实测：
rc=101 / 3 failed）。

## 五、演练实测（原始输出见同目录）

```text
bash scripts/verify/m420_purchase_chain_e2e_drill.sh
→ PASS=33 FAIL=0 · ALL PASS
   · 模板克隆 + 54 条迁移全绿
   · 前置 12 条（表 / 三条流程 / 五个岗位 / 三个科目 / 状态机边未被关）
   · 本卡 3 条用例全绿 + SKIP=0
   · 采购域回归 454 条全绿（含计划 / 申请 / 订单 / 框架协议 / 合同 / 供应商评估）
   · 收货域回归 165 条全绿（含建单 / 过账 / 三本账 / 超收 / 作废）
   · 夹具零残留 11 条（计划 / 明细 / 申请 / 订单 / 收货单 / 应付 / 审批实例 / 三本账 / 账号）

TEETH=1 bash scripts/verify/m420_purchase_chain_e2e_drill.sh
→ PASS=17 FAIL=0；坏连接串下 rc=101、`test result: FAILED. 0 passed; 3 failed`
```

### 演练库清污（脚本 §3，只清一条模板自带的历史残留）

模板库自带的演示数据里有**一条** 2026-11-10 的 `PO_APPROVE | PENDING` 实例
（订单号 `W1B-PO-0001`）。采购域收尾用例 `purchase_handler::tests::zz_teardown_purges_po_approval_residue`
的断言口径是「**全库**不剩在跑的采购审批实例」，会红在这条与本卡毫无关系的历史残留上
（实测：不清理 453 passed/1 failed；清掉后 454 passed/0 failed）。脚本按**单号白名单**只删这一条，
并把影响行数打印出来 —— 任何别的失败仍然会让演练红，不是「吞掉失败」。

## 六、门禁

```
cargo fmt -p cloudpivot -- --check                        → rc=0
cargo clippy -p cloudpivot --lib -- -D warnings           → Finished（零告警）
cargo clippy -p cloudpivot --lib --tests -- -D warnings   → Finished（测试模块同样零告警）
cargo test --lib system::menu::permission_audit_tests     → 8 passed / 0 failed（本卡零权限码）
```

## 七、已知边界（照实说）

- **只验一条链**：计划两行同一仓（一仓一单）。一仓多单 / 多仓合并 / 部分转计划量由
  `purchase_plan_convert_db_tests` 覆盖，本卡不重复。
- **计划 / 申请没有 `biz_status_log` 轨迹**（只有采购单与收货单走 `state_machine`），
  所以收口断言只对这两张单断言「轨迹条数 ≥ N」。
- **`po_approve` 只走了 <1 万元的部门主管分支**：10 万以上会签、超信用额度加财务复核
  由 `purchase_linkage_db_tests` / 信用额度卡覆盖。
- **未验证前端**：本卡零前端改动，页面链路（M4-06 / M4-11 / M4-16）不在本卡范围。
