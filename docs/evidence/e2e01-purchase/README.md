# E2E-01 采购域全流程演练证据（订单→审批→入库→应付→付款 + 申请 + 询比价 + 退货）

> 卡号：`t_9b0d0a5c`【E2E-01】
> 环境：**145 测试机** `http://172.18.10.145:8033`（直连后端）+ nginx 80；库 `cloudpivot_dev`（演练数据）
> 代码版本：修复提交 `2ce3bd85`（被验链路在 `d2dd9ab` 上跑完，修复后复跑 step3 验证回参）
> 结论：**8 个页面全部通畅**；三级勾稽 / 六本账 / 状态机全对得上；**BUG-01 免审过账死单修复有效**；本卡发现并修复 1 个 P3（≤2 源文件）

---

## 0. 环境与方法（为什么这份证据可信）

| 项 | 值 |
|---|---|
| 后端 | `systemctl status cloudpivot-test` → active；`/health` = 200；监听 `127.0.0.1:8033` |
| 数据库 | `cloudpivot_dev`（145 本机 PG 18，`127.0.0.1:5432`） |
| 认证 | 真实 `POST /api/system/auth/login`（admin，密码取 `/etc/cloudpivot/cloudpivot-dev.env` 的 `NIK_ADMIN_PASSWORD`）→ Bearer JWT，全程真 curl |
| 演练专用数据 | 部门 `E2E01-DEPT`、审批人 `e2e01_appr`（6 个审批角色）、仓库 `E2E01-WH` + 默认库位 `E2E01-L01`、供应商 `E2E01-SUPA/B/C/D` |
| 只读复用 | 物料 `DEMO-P101`(P1) / `DEMO-P102`(P2) —— 只读引用，未修改主数据 |
| 脚本 | `scripts/verify/e2e01_purchase_chain.sh`（`step0`~`step10` 分段，状态落 `/tmp/e2e01/state.sh`）；`scripts/verify/e2e01_purchase_final_verify.sh`（只读终验） |
| 原始报文 | `raw/*.json`（逐步 curl 的完整响应体，已剔除登录 token 文件） |
| 终验快照 | `final_verify.txt`（单据清单 + 勾稽 SQL + 六本账 + 列表接口） |

> 审批链路过「会签 ALL」节点：`purchase_manager / dept_leader / gm / warehouse_manager / finance_*` 这些共享角色上残留着别的卡的夹具账号
> （`p3a_0002/0003/0004`、`p2c1_reviewer`），会让节点停在「还差一个人」。本卡按仓内既有做法
> （`scripts/verify/p22b_credit_branch_e2e.py` 同款）**演练期间临时把这些绑定移出、跑完即恢复**，
> 恢复后复核：`DEPT_LEADER 2 / PURCHASE_MANAGER 2 / GM 2 / WAREHOUSE_MANAGER 2 / FINANCE_MANAGER 1 / FINANCE_DIRECTOR 1`（与演练前一致）。

---

## 1. 结果总览（8 个页面）

| # | 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|---|
| ① | 供应商档案 | 建 SUPA/B/C/D（编码/名称/账期/信用额度） | 200 ×4 | `biz_supplier` 4 行，status=1 | ✅ |
| ① | 供应商档案 | 改 SUP D：名称 + 联系人 + 账期 30→45 | 200 | `E2E-01 供应商丁（改名）/45` | ✅ |
| ① | 供应商档案 | 停用 SUP D（`UPDATE` status=0）→ 再启用 | 200 ×2 | `status` 0 → 1 | ✅ |
| ② | 询比价 | 建询价单 2 行（P1×100 / P2×50，仓库 E2E01-WH） | 200 | `PIQ-20261109-000002` DRAFT | ✅ |
| ② | 询比价 | 3 家各报 2 行（甲 10/20、乙 9.5/21、丙 11/19，税率 13%） | 200 ×3 | `biz_price_quote` 6 行，状态 `QUOTING` | ✅ |
| ② | 询比价 | 比价表（`comparison`/`quotes` 按行展开 3 家单价、含税价、交期） | 200 | 6 条报价可比 | ✅ |
| ② | 询比价 | 定标：行1→乙 9.5、行2→丙 19 | 200 | `AWARDED`，`award_mode=MULTI` | ✅ |
| ② | 询比价 | 转采购订单（按供应商分组） | 200 | 2 张 PO `PO-…054`(乙,1073.50) / `PO-…055`(丙,1073.50)，询价单 `CONVERTED` | ✅ |
| ② | **报价单价带入** | 校验 PO 明细单价 | — | `biz_purchase_order_item.price` = **9.500000 / 19.000000**（= 中标报价） | ✅ |
| ③ | 采购申请 | 建 2 张（PRQ-…005 10 件 / PRQ-…006 20 件，单价 5，税率 13%，建议供应商甲） | 200 ×2 | DRAFT | ✅ |
| ③ | 采购申请 | 分别提交 → `pr_approve` 审批（采购经理节点） | 200 ×2 | `SUBMITTED` → `APPROVED` | ✅ |
| ③ | **合并转单** | `to-po` 带 `requisition_ids=[005,006]` | 200 | 一张 `PO-…056`：数量 **30**、单价 5、价税 169.50；两申请均 `CONVERTED` 且 `po_id` 相同 | ✅ |
| ④ | 采购订单 | 3 张 PO 提交审批（`po_approve`，<1 万走部门主管档） | 200 ×3 | `DRAFT` → `APPROVED`，实例 `APPROVED` | ✅ |
| ④ | 采购订单 | 列表 / 明细分页 / 详情 / 状态筛选 | 200 ×5 | total 10 / 11 行 | ✅ |
| ⑤ | 收货入库 | 建收货单 1（来源 PO-…054，60 件 @9.5） | 200 | `IN-…237` DRAFT，价税 644.10 | ✅ |
| ⑤ | 收货入库 | `submit-post`（644.10 < 1000 → **n_notify 免审自动过账**） | 200 | 直接 `POSTED` | ✅ |
| ⑤ | **六本账** | 库存 / 流水 / 凭证 / 应付 全部由过账内核落库 | — | 库存 0→**60**；流水 `IN/60/PURCHASE_IN`；凭证 `PZ-…473` 借 1405 570 + 2221.01 74.10 / 贷 2202 644.10；应付 `AP-…197` 644.10 OPEN | ✅ |
| ⑥ | 应付账款 | 可付信息 `payment-info`（默认金额 = 未付） | 200 | 644.10 / can_pay | ✅ |
| ⑥ | 应付→付款 | 付 300（`PM-…038`）→ 部门主管审批 | 200 | `PARTIAL_PAID`，已付 300，未付 344.10 | ✅ |
| ⑥ | 应付→付款 | 付余额 344.10（`PM-…039`）→ 审批 | 200 | **`SETTLED`，未付 0.0000**；凭证 `PZ-…474/475` 借 2202 / 贷 1002 | ✅ |
| ⑦ | 采购退货 | 可退明细（`source/{stock_in_id}`，可退 60） | 200 | 1 行可退 | ✅ |
| ⑦ | 采购退货 | 建退货单 10 件（单价锁死入库快照 9.5） | 200 | `PRT-…001` DRAFT，价税 107.35 | ✅ |
| ⑦ | 采购退货 | 提交审批（**附件必传规则 V0347** → 上传 PDF 后放行） | 200 + 200 | `SUBMITTED` | ✅ |
| ⑦ | 采购退货 | `prt_approve`（采购经理 → 财务经理） | 200 ×2 | `APPROVED` | ✅ |
| ⑦ | **防超退** | 再建 51 件（可退只剩 50） | **400 / 66051** | 拒绝建单，文案点名「原入库 60、已退 10、可退 50、本次 51」 | ✅ |
| ⑦ | 采购退货 | 确认退货（`confirm`） | 200 | `CONFIRMED`；库存 60→**50**（`OUT/10/PURCHASE_RETURN`）；冲销凭证 `PZ-…476` 借 2202 107.35 / 贷 1405 95 + 2221.01 12.35；应付已结清 → 生成**供应商应退款** `SR-…001` 107.35 OPEN | ✅ |
| ⑧ | 采购明细 | `purchase-order-item/page` 全量 / 按订单 / 按物料 | 200 ×3 | total 11 / 1 / 4 | ✅ |
| ⑨ | **BUG-01** | 采购单「只剩 10 件」时，另一张 40 件单走**免审过账** | **409 / 66032** | 单据 **`PENDING_POST` → `DRAFT`**（不是死单）；`biz_status_log` 记「免审过账失败：…只剩 10 件待收」；应付 0 行、库存未动 | ✅ **修复有效** |
| ⑩ | 收满再收 | PO-…055 已 `RECEIVED` 后再建收货单 | **400 / 10001** | 「只有已提交/已审批/部分收货的采购单可以收」 | ✅ 被拦 |
| ⑪ | **BUG-02** | `repost` 对已过账单据 | **400 / 10001** | 「已过账的单据不能重复过账：库存只加一次」 | ✅ 冒烟通过 |

---

## 2. 勾稽验证（金额 / 数量 / 状态一致性，`final_verify.txt` 原文）

**① 采购订单 → 入库单 → 应付（三级）**

```
PO-20261109-000054 | 订单数量=100 | 已过账收货=100 | 订单价税=1073.50 | 收货价税=1073.50 | 应付=1073.50 | 状态=PARTIAL_RECEIVED
PO-20261109-000055 | 订单数量=50  | 已过账收货=50  | 订单价税=1073.50 | 收货价税=1073.50 | 应付=1073.50 | 状态=RECEIVED
```
- 入库↔应付金额不一致行数：**0**
- 应付恒等式 `unpaid = amount - paid` 违反行数：**0**
- PO-…054 收满 100 后状态是 `PARTIAL_RECEIVED` 而非 `RECEIVED`：因为该单后续**退货 10 件**，净收货 90 —— 退货回冲数量参与状态判定（口径合理，非缺陷）。

**② 库存 = 流水（按 物料+仓库）**

```
DEMO-P101 库存=90 流水=90 差异=0
DEMO-P102 库存=50 流水=50 差异=0
```
流水四行自洽（`after - before = ±qty`）：

```
IN  P101 60 PURCHASE_IN     before=0  after=60
OUT P101 10 PURCHASE_RETURN before=60 after=50
IN  P102 50 PURCHASE_IN     before=0  after=50
IN  P101 40 PURCHASE_IN     before=50 after=90
```

**③ 凭证借贷平衡（本卡单据产生 6 张，借贷不平 0 张）**

| 凭证 | 摘要 | 借 | 贷 |
|---|---|---|---|
| PZ-…473 | 采购入库 IN-…237 | 1405 570.00 + 2221.01 74.10 | 2202 644.10 |
| PZ-…474 | 付款 PM-…038 | 2202 300.00 | 1002 300.00 |
| PZ-…475 | 付款 PM-…039 | 2202 344.10 | 1002 344.10 |
| PZ-…476 | 采购退货 PRT-…001 冲减 | 2202 107.35 | 1405 95.00 + 2221.01 12.35 |
| PZ-…477 | 采购入库 IN-…238 | 1405 950.00 + 2221.01 123.50 | 2202 1073.50 |
| PZ-…478 | 采购入库 IN-…239 | 1405 380.00 + 2221.01 49.40 | 2202 429.40 |

**④ 六本账计数**：库存行 2 / 流水行 4 / 凭证 6 / 应付 3 / 付款 2 / 退货 1 / 状态日志 14。

**⑤ 付款后**：`AP-…197` `SETTLED`、`paid=644.10`、`unpaid=0.0000`（两次付款 300 + 344.10 合计等于应付）。

---

## 3. 问题清单（P0–P4）

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|---|---|---|---|---|
| 1 | **采购申请合并转单响应 `lines[].material_code` / `material_name` 恒为空串** | **P3** | `POST /api/business/erp/purchase-requisition/{id}/to-po` 带 `requisition_ids` → 响应 `data.lines[0].material_code == ""` | 转单结果页按这两列展示「合并了哪些物料」，实际只有 UUID；用户无法核对合并结果 | **已修**（见 §4） |
| 2 | 采购退货「附件必传」只在 **提交时** 拦，草稿态无任何提示 | P4 | 建退货单 → 草稿可直接保存 → `submit` 才 400「必须上传退货单或签收凭证」 | 用户填完单才发现缺附件，来回一趟 | 草稿保存时按 V0347 规则预检（`GET /business/attachment/check?at=SUBMIT` 已有，前端草稿页可提前调） |
| 3 | 采购明细列表只能按 `order_id` / `material_id` 筛，没有单号/物料编码模糊查询 | P4 | `GET /purchase-order-item/page?page=1&page_size=5` 只接受这两个筛选 | 卡面 ⑧「筛选」只能做到「按订单/物料」 | 若产品要「按单号/物料编码搜」，需给 `PurchaseOrderItemFilter` 加 `keyword` 并 join 单头 |
| 4 | 同一 ERP 域出参形状不统一：`to-po` 是 `{data.purchase_order, data.lines}`、收货单是 `{data.header, data.items}`、采购单提交审批是 `{data.order}`、询价建单是 `{data.head, data.items}` | P4 | 逐个接口看响应 | 前端每接一个接口都要单独写解析；写通用表格组件时容易踩空 | 建议定一套 `header/items`（或 `payload/list`）口径，后续卡统一 |
| 5 | 审批实例「已有人审批」后不能撤回，且没有 HTTP「终止」入口（`engine::terminate_instance` 只在进程内可用） | P4（运维缺口，非本卡引入） | 会签节点缺人 → `POST /business/flow/instance/{id}/withdraw` 返回 `409/65023`「已有人审批（1 票），不能撤回；请联系流程管理员终止」 | 演练/生产遇到会签缺人只能改库或找管理员 | 补 `POST /business/flow/instance/{id}/terminate`（权限码 + 原因），把进程内能力暴露出来 |

> P0 / P1 / P2：**0 条**。BUG-01（免审过账死单）、BUG-02（重复过账）本卡复跑均符合修复后的预期。

---

## 4. 修复记录

**提交 `2ce3bd85`**（`fix(erp): 采购申请合并转单响应回填物料编码/名称（E2E-01）+ 采购域全流程演练脚本`）

| 文件 | 改动 |
|---|---|
| `backend/src/business/erp/purchase_requisition_model.rs` | 新增 `material_ref_of(ex, material_id) -> Option<(code, name)>`：按主键取 `biz_material` 编码+名称（未删），查不到给 `None`（与同文件 `dept_name_of` / `po_no_of` 同口径，带 SCOPE-EXEMPT 注释） |
| `backend/src/business/erp/purchase_requisition_handler.rs` | `to_purchase_order` 组装 `ToPoRes` 前按 `material_id` **去重**回填物料编码/名称，`to_merged_res` 增加 `materials` 入参（原来是写死 `String::new()`） |

验证（145 真机，重建后 `systemctl restart cloudpivot-test` + `/health=200`）：

```
转单返回 lines[0] = {"material_code":"DEMO-P101","material_name":"采购演练物料P1",
                    "qty":"30.0000","est_price":"5.000000","tax_rate":"13.0000", ...}
```
修复前同字段为 `"material_code":""` / `"material_name":""`（见 `raw/s3_topo.json` 的历史报文与本次复跑对比）。

检查链：`cargo check --lib` ✅ / `cargo test --lib purchase_requisition` **15 passed** / `rustfmt +1.94.1 --edition 2024 --check` ✅ / 提交前库存三本账唯一写入口闸 ✅（零越界）。

---

## 5. 遗留状态（本卡自己造的数据，未清理）

| 单据 | 状态 | 说明 |
|---|---|---|
| `PIQ-20261109-000001` | DRAFT | 首轮脚本对齐响应形状时的空壳询价（未报价） |
| `PRQ-…001/002` | DRAFT | 首轮建单样例 |
| `PRQ-…003/004` | SUBMITTED | 首轮提交后**撞上会签缺人**卡住的实例（`409/65023` 无法撤回），改用「角色收成单成员」后重跑；三个待办仍挂在 `p3a_0002/0003` 名下 |
| `IN-20261109-000240` | DRAFT | BUG-01 证据单（免审过账被拒后打回草稿，保留作证据） |
| `PO-…054/055/056/057`、`IN-237/238/239`、`AP-197/198/199`、`PM-38/39`、`PRT-001`、`SR-001` | 见 §1 | 演练主链路单据，全部为本卡所造 |

未使用 DROP/TRUNCATE，未删改他人数据，未改任何已应用迁移。
