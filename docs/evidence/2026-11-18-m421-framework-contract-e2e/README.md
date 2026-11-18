# M4-21 采购缺口 E2E：框架协议 + 合同关联（含超限场景）· 验收证据

- 卡：`t_b62c50ae`（M4-21【M4·采购】采购缺口 E2E：框架协议 + 合同关联）
- 日期：2026-11-18
- 性质：**验证卡**（只补 E2E 用例 + 演练脚本；**零迁移 / 零权限码 / 零路由 / 零前端**，见 04 号证据）
- 数据库：演练库建在 **145（172.18.10.145:5432，用户 `cloudpivot`）**，从模板 `cloudpivot_test`
  克隆 → 补 V0405…V0458（54 条）→ 跑完脚本自己 DROP

## 一、这一份补的是哪个缺口

采购单有**两条带定价约束的下单路线**，各自的单据段前面几张卡都钉过了，但**没有一条用例把它们串到入库**：

```text
framework_agreement_db_tests        ← 协议两表结构 / 有效期 / 上限语义（协议是 SQL 夹具）
framework_agreement_ref_db_tests    ← 下单引用校验（单段：一条一条建单，从不收货）
framework_agreement_execution_db_tests ← 执行量报表（单段：不检查协议怎么来的）
framework_agreement_over_limit_tests   ← 超限纯逻辑 + 真库反例（单段）
framework_agreement_crud_db_tests   ← 建/改/删/提交/两节点审批 + **一张**单涨执行量（到下单为止）
contract_execution_db_tests (M4-19) ← 合同执行量（单段：合同是 SQL 夹具）
purchase_chain_e2e_tests (M4-20)    ← 计划→申请→订单→入库（**不挂协议、不挂合同**）
本文件（M4-21）                     ← 协议/合同**从建单到入库**的完整路线，超限夹在链中间
```

于是这三件事在库里从来没有被验证过：

1. 审批通过的框架协议**真能一路收到账吗**（协议价会不会落成库存成本、含税额会不会落成应付）；
2. 超出协议/合同上限时**是不是真的挡得住、且不落半张单**（行上限 / 单价容差 / 协议外物料 /
   有效期 / 供应商 / 单头总量 / 单头总额 / 合同额 —— 八类拒绝）；
3. 作废**会不会把额度还回来**，收货**会不会重复计额**（后者是超卖的另一种形态）。

## 二、交付物

| 文件 | 行数 | 说明 |
|:--|--:|:--|
| `backend/app/src/business/erp/purchase/framework_contract_e2e_tests.rs` | 1844 | **5 条真库用例 / 153 个断言点**（133 常规断言 + 19 拒绝断言 + 1 expect_err） |
| `backend/app/src/business/erp/purchase/mod.rs` | +4 | `#[cfg(test)] mod framework_contract_e2e_tests;` |
| `scripts/verify/m421_framework_contract_e2e_drill.sh` | 238 | 克隆 → 补迁移 → 前置 13 条 → 清污 → 4 组用例 → 夹具 13 条零残留；`TEETH=1` 有牙自检 |
| `docs/evidence/2026-11-18-m421-framework-contract-e2e/*` | — | 本目录（README + 5 份原始输出） |

## 三、五条用例与断言落点

### ① `agreement_chain_with_caps_and_release` —— 协议链 14 步

```text
 ① 建协议（A 10 元/行上限 100；B 50 元/行上限 1000；单头总量 130 / 总额 1980）→ DRAFT
    断言：FWA- 单号 19 位 / DRAFT / 派生合计 0 / 2 行 / editable / 行「未开工」
 ② 反证：DRAFT 协议下单 → 400 / 66096（草稿能下单 = 没有审批），且**一行不落**
 ③ 提交 → FWA_APPROVE（采购经理 → 财务经理）**两节点真批**（hops 必须 = 2）→ 协议 APPROVED
 ④ PO1：A 60@10.5（= 10×1.05 容差上限）+ B 2@50
    断言：SPOT / 协议 id + 协议号快照落库 / 不含税 730 / 税 94.9 / 价税 824.9 /
          两行都带**服务端现查**的协议行 id 且互不相同 /
          A 行已用 60、剩余 40、PARTIAL、回写 drift = 0、单头 62 量 / 730 额
 ⑤ PO2：A 40@10 → A 行 100/100 = DONE、剩余 0（== 上限不是 OVER）
 ⑥ 反证：A 再下 1 → 409 / 66098（行数量上限）
 ⑦ 反证：A 单价 10.6 > 10×1.05 → 409 / 66097（容差外）
 ⑧ 反证：协议外物料 → 400 / 66096（报文点「不在框架协议」）
 ⑨ 反证：下单日 2025-12-31（协议期外）→ 400 / 66096（报文点「有效期」）
 ⑩ 反证：换供应商挂同一份协议 → 400 / 66096（协议价只对签约方有效）
 ⑪ PO8：B 17@50 → 单头总额**正好 1980 = 上限**→ 放行（边界不是拒绝）；三张未作废单都计入
 ⑫ 反证：B 1@50 → 409 / 66098（单头总额超限）
 ⑬ 作废 PO2 → A 回 60 / 单头 1580；下钻里 PO2 `counted=false` + 「作废」；
     再下 A 40 → **又能下满**（额度不是只减不加）
 ⑭ 第二份协议（只有单头总量 100、无行上限）→ 恰好 100 放行（行状态 UNLIMITED）、再下 1 → 66098「总量」
```

⑥~⑫ 每一个拒绝之后都断言 **`order_count` 不变**（被拒的单不许落库）—— 这是本文件对
「事务里 rollback 干净」的硬闸，不是「报文对了就算过」。

### ② `agreement_order_posts_to_stock_and_payable` —— 协议单走到账

```text
① 协议（A 10 元/不限量）→ 两节点真批
② 下单 A 60@10（600）→ 采购单提交审批 → po_approve 真批
③ 建收货单（items 留空 = 服务端带出未收完的行）→ DRAFT：库存 0 / 无凭证 / 无应付（草稿不算已收）
④ 过账：
   成本账 60 件 / **金额 600（不含税协议价，不是 678）**
   借 1405 = 600；借 2221.01 = 78（600×13%）；贷 2202 = 678；借贷平衡（合计 678）
   应付一条：678 / OPEN；采购单 → RECEIVED；PURCHASE_IN 流水 1 条
⑤ 收口：协议已用量**过账前后一动不动**（60 / 600 / drift 0 / 下钻仍只有 1 张采购单）
```

⑤ 是本卡最容易被写错的一条：**收货不是「又下了一次单」**。若把收货量也计入协议已用量，
报表立刻「超上限」而实际只下过一次单 —— 这就是重复计额。

### ③ `contract_linked_chain_with_amount_cap` —— 合同采购链 8 步

```text
① 合同采购单 10×2.00+13% = 22.60：CONTRACT / contract_id 落库 / 合同号快照 /
   供应商**从合同乙方派生**（请求里根本不传）/ framework_agreement_id 为空 / 价税 22.60
② PO2 20×2.00 = 45.20 → 累计**正好 67.80 = 合同额**→ 放行；合同 DONE / 剩余 0 / 计 2 单
③ 反证：再下 1 件（+2.26）→ 409 / 66019（额度不足），不落库、已执行额不变
④ 作废 PO2 → 已执行回 22.60 / 剩余 45.20 / released_doc_count = 1 / 下钻 counted=false
⑤ 释放后重下 1 件 → 放行（24.86）
⑥ 收货过账 PO1：库存 10 / 金额 20 / 借 1405 20 / 借 2221.01 2.6 / 贷 2202 22.60
⑦ 收口：合同已执行额仍是 24.86（**收货不改合同执行量**：合同看下单额，不看入库额）
⑧ 反证：已收货订单从 `PATCH /status` 直接作废 → 409（必须走联动作废，R-P1-5）
```

### ④ `route_exclusivity_and_unusable_references` —— 两条路线的互斥与四类不可用引用

```text
CONTRACT + 框架协议                 → 400（两个价格上限说不清以谁为准）
SPOT + contract_id                  → 400
CONTRACT 不给 contract_id           → 400
CONTRACT + 手填非乙方供应商          → 400（供应商从合同乙方带出）
草稿合同 / 销售合同 / 已软删合同      → 400 · 66014 / 66014 / 66002
草稿协议 / 审批中协议 / 已软删协议    → 400 · 66096 ×3
收口：十次拒绝之后采购单张数不变（零残留）
```

### ⑤ `zz_teardown_purges_fixtures` —— 清场自检

先**真跑半条链**（协议审批 → 下单 → 审批 → 收货 → 过账）造出凭证/应付/三本账痕迹，
再 `cleanup()`，然后断言 11 类痕迹（协议 / 协议明细孤儿 / 采购单 / 收货单 / 审批实例 /
凭证 / 应付 / 合同 / 三本账 / 主数据 / 夹具账号）**全部 0 残留**。

## 四、五条实测口径（写断言前必须先看）

1. **协议金额上限按「协议价是否含税」的口径比**：本文件协议 `price_includes_tax=false`，
   `total_max_amount` 比的是 **Σ 不含税金额**；合同额度比的是 `total_with_tax`。两条路线分母不同。
2. **收货过账不占协议 / 合同额度**：已用量来自「未作废未删除的采购单」，收货单不在其中。
3. **草稿单也占额度**（`usage_of_agreement` / `committed_amount` 只排除作废 / 软删，不看状态）——
   所以「释放后重下」必须走**作废**，不是「不提交就不算」。
4. **作废才还额度**：`DRAFT → CANCELLED` 同样释放（M4-08 `release_order`）。
5. **`biz_purchase_order_item` 的列叫 `order_id`**（不是 `po_id`）；`biz_purchase_order_item.amount`
   是不含税金额、`with_tax_amount` 是价税合计 —— 三个数混着写必假红。

## 五、验收（全部实跑，原文见 01/02/03/05/06 号证据）

```
bash scripts/verify/m421_framework_contract_e2e_drill.sh
  → PASS=38 FAIL=0 / ALL PASS
     · 54 条业务迁移全部应用成功
     · 本卡 5 条真库用例全绿，SKIP=0
     · 采购域 459 / 收货域 165 / 合同域 109 全绿，全部 SKIP=0
     · 夹具 13 条零残留
TEETH=1 bash scripts/verify/m421_framework_contract_e2e_drill.sh
  → 坏连接串 rc=101 / 5 failed（有牙：拒绝静默 SKIP 假绿）
cargo fmt -p cloudpivot -- --check            → 0 处 diff
cargo clippy -p cloudpivot --lib --tests -- -D warnings → Finished（零告警，rc=0）
cargo test -p cloudpivot --lib system::menu::permission_audit_tests → 8 passed / 0 failed
```

## 六、遗留 / 交接

- **前端 e2e 仍未覆盖协议写入口**（M4-11b 记的账）：`frontend/e2e/m411-framework-agreement.spec.ts`
  只覆盖只读台账；`fwa-new` / `fwa-form-dialog` / `fwa-ledger-submit` 等 testid 已就位，
  但**本卡是后端验证卡，不夹带前端**。要给 M4-22 收口用，另开前端卡。
- **`framework_agreement_crud_db_tests::full_chain_order_raises_execution` 与本卡用例②有重叠**（都到下单），
  区别是本卡**到过账**并钉住「收货不改额度」。前者不要删（它守的是 CRUD 链路本身）。
- **`m420_purchase_chain_e2e_drill.sh` 的 `ERP_DB_URL` 里口令写成了字面 `***`**，
  在需要口令认证的 145 上连不上（本机实测：用 `postgres://cloudpivot:***@…/cloudpivot_test`
  跑本卡用例 → `0 passed; 5 failed`，即 `***` 无法认证）。本卡脚本改用 `$PGPASSWORD` 注入。
  若 M4-20 的演练曾在 145 上跑通，需复核它当时是否真的连上了库（本卡不修那张卡的脚本，只记录）。
