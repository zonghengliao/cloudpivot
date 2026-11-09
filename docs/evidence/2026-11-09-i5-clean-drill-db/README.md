# E2E-14 评审 I-5：清共享演练库 + 重建干净验收基线

> 卡：`REVIEW-FIX-3【I-5】`（t_3ec444ff）　机器：**145**（`172.18.10.145`）。**144 生产未触碰**。
> 运行时间：2026-11-09 23:10–23:35 CST

---

## 一句话结论

- 评审说的「5 张孤儿应收」**属实**（合计 2762 元，全部来自已不存在的出库单），已**软删**（保审计痕迹）；
- 评审说的「11 组库存差异」**是汇总 SQL 的口径错**：卡片/评审用的 `LEFT JOIN` 会把流水按库存桶行重复计入，
  用**两侧先聚合再 FULL JOIN** 的正确口径，`cloudpivot_dev` 实测 **0 组真差异**（逐桶成本账 vs 库位账也是 0）；
- 已重建/确认干净库 **`cloudpivot_dev_clean`**：287 条 versioned 迁移（max `367`）、菜单 433、admin 1 人、
  业务表 0 行，**不变量 80/80 全绿**；冒烟（登录 + 入库 3/出库 1 + 对账）**PASS=12 FAIL=0**；
- 新增**验收闸** `scripts/ops/acceptance_gate.sh`，把「巡检全绿」做成可执行前置条件（clean 绿 RC=0，dev 红 RC=1）。

---

## ① 孤儿应收清单（5 张，已软删）

判据：`deleted_at IS NULL AND source_type='SALES_OUT' AND source_id` 指向的 `biz_stock_out` 不存在。
（卡面 SQL 少了 `source_type` 过滤，会把 16 张 `MANUAL` 应收也带出来 → 21 行；见 `11_disposition_probe.out` 第 1/2/4 节。）

| ar_no | source_no | source_id | 金额 | 状态 | 凭证 | 核销 |
|---|---|---|---|---|---|---|
| AR-20261109-000012 | OUT-20261109-000007 | 01a1bc96-eb4d-751e-b100-54eb471149d5 | 50.0000 | OPEN | 0 | 0 |
| AR-20261109-000013 | OUT-20261109-000008 | 01a1bc99-1732-7469-bc42-965b3d6e966e | 1130.0000 | OPEN | 0 | 0 |
| AR-20261109-000014 | OUT-20261109-000009 | 01a1bc99-1df2-71b9-b24e-eb56c027f6d7 | 339.0000 | OPEN | 0 | 0 |
| AR-20261109-000015 | OUT-20261109-000010 | 01a1bc99-2077-7509-93c1-ce51ea37ec70 | 791.0000 | OPEN | 0 | 0 |
| AR-20261109-000016 | OUT-20261109-000011 | 01a1bc99-270e-73b9-ae4e-7af9734d18cc | 452.0000 | OPEN | 0 | 0 |

合计 2762.0000 元；`biz_stock_out` 里这些 id **物理不存在**（不是软删），因此无法「补回 source_id」，只能软删。
处置 SQL：`scripts/datafix/i5/01_orphan_ar_softdel.up.sql`（固定 id 白名单 + 同一判据双保险 + 非 0/5 整单回滚），
实测 `soft-deleted rows = 5`，事后孤儿 0、`MANUAL` 16 张一行未动（`20_orphan_softdel.out`）。

---

## ② 库存差异清单（现测 12 组 —— 全是假差异，真差异 0）

卡面 SQL（`biz_inventory i LEFT JOIN biz_inventory_txn t ON 物料+仓库` 然后 `SUM(i.qty)`）在 `cloudpivot_dev` 上得 12 行。
原因：`biz_inventory` 是 **per-bucket 桶表**（同物料×仓库可有多行：不同库位 / 未分配），
一条流水会被该物料×仓库的**每个桶行**各匹配一次，于是「库存合计」被放大、`txn_sum` 只等于一条流水：

| 物料 | 仓库 | 卡面 SQL 的 inv_sum | 重复计入的 txn_sum | 假差异 |
|---|---|---|---|---|
| DEMO-INV01 | WH-DVA | 460 | 92 | 368 |
| DEMO-INV02 | WH-DVA | 330 | 110 | 220 |
| DEMO-M001 | WH01 | 264 | 88 | 176 |
| DEMO-M002 | WH01 | 282 | 94 | 188 |
| DEMO-P101 | E2E01-WH | 270 | 90 | 180 |
| DEMO-P102 | WH01 | 300 | 150 | 150 |
| E2E02-M1 | E2E02-WH | 688 | 344 | 344 |
| E2E02-M2 | E2E02-WH | 390 | 195 | 195 |
| E2E03-M1 | E2E03-WH-A | 679 | 194 | 485 |
| E2E03-M1 | E2E03-WH-B | 100 | 50 | 50 |
| E2E03-M2 | E2E03-WH-A | 60 | 20 | 40 |
| E2E03-MS | E2E03-WH-A | 20 | 10 | 10 |

- 正确口径（`WITH inv … , txn … FULL JOIN USING (material_id, warehouse_id)`）：**0 行**；
- 逐桶（物料×仓库×库位）成本账 vs 库位账：**0 行**；
- 评审当天数到 11 组、现在 12 组，只是期间夹具增删导致桶数变化，不影响结论：**不是账错，是 SQL 口径错**。
- 原始输出：`11_disposition_probe.out` 第 5/6/7 节。

---

## ③ 处置记录

| 对象 | 动作 | 依据 |
|---|---|---|
| 5 张孤儿销售应收（`cloudpivot_dev`） | **软删**（`deleted_at=now()`），记录保留 | 有审计价值；无凭证/无核销，删了不破坏账 |
| 16 张 `MANUAL` 应收 | **保留**，不动 | `source_id` 是挂账占位 UUID，本就不是孤儿（口径误报） |
| `cloudpivot_dev` 历史夹具（P1F2/SO14/W2B… 共 211 凭证 / 48 流水 / 21 库存行） | **保留整库**（方案 A：老库留作历史参考） | 清这些要按 FK 逐表扒，风险高、收益低；验收已改用干净库 |
| `cloudpivot_dev_clean` | 只保留迁移+种子；冒烟夹具 `I5-SMOKE-*` 跑完即清（0 业务行） | 验收基线必须可复现、可重复巡检 |
| 巡检红项（dev 剩 5 BLOCKER / 3 WARN） | **保留现状并记录**，不刷数据掩盖 | 全是历史夹具/历史 biz_type 残留，见下方「遗留发现」 |

处置前后 `cloudpivot_dev` 的不变量：**71/80（6 BLOCKER）→ 72/80（5 BLOCKER）**，消失的一条即孤儿应收相关项
（`21_dev_invariants_after.out`）。

---

## ④ 干净库 `cloudpivot_dev_clean` 终态

```
mig_rows=298 (versioned=287, max=367)  menus=433  users=1
inv=0  txn=0  serial=0  ar=0  so=0  vou=0  non_seed_material=0
admin → dept HQ(总部)          ← N2 由启动期 land_users_into_dept_by_code 落位
不变量：80/80 通过   BLOCKER 0   WARN 0        （40_clean_invariants_final.out）
```

---

## ⑤ 干净库冒烟（真实 token curl，隔离实例 :18059）

`30_clean_db_smoke.out`（脚本 `scripts/datafix/i5/03_smoke.sh`，`PASS=12 FAIL=0`）：

```
login code=0 (must_change_password=false) → /auth/me username=admin dept_id=…f0366 roles=[SUPER_ADMIN]
建 unit / warehouse / material(serial_required=true)                    → 200 code=0
serial/batch-generate count=3 unit_cost=10                              → 200 code=0，生成 3 号
GET /inventory/page → qty=3.0000 ；GET /inventory-txn/page → total=1 SERIAL_IN
PATCH /serial/{id}/status OUTBOUND                                      → 200 code=0，qty=2.0000
库存 2.0000 − 流水 2.0000 = 0（2 条流水，IN_STOCK=2 / OUTBOUND=1，凭证 2 张）
巡检（冒烟数据在位）80/80 → 清场（I5-SMOKE 全清，9 张表 0 行）→ 巡检（空基线）80/80
```

> 清场时踩到并写进脚本的坑：序列号出库写的是 `SERIAL_OUT` 凭证（source_id = 序列号 id），
> 只清 `SERIAL_BATCH` 会留 1 张凭证 → `B2 1405` 红 10 元。脚本已按「无序列号业务时兜底清无主 SERIAL_* 凭证」处理。

---

## ⑥ 运维手册与可执行前置条件

- 新增 `docs/运维规程-演练库与验收基线.md`（两库角色、重建步骤、验收闸、冒烟、5 条坑），
  已加入 `docs/README.md` 索引。**明文规定：每次演练开始前、每次上线前必须验收闸退出 0。**
- 新增 `scripts/ops/acceptance_gate.sh`：三条判据（80/80 不变量 / 孤儿应收 0 / 库存流水差异 0+逐桶 0），
  退出码 0/1，生产库名直接拒绝（exit 2）。
- 实测：clean → `✅ 全绿（可作演练/上线验收基线）` RC=0（`50_*`）；dev → `❌ 未通过` RC=1（`51_*`，证明闸不是空的）。

---

## ⑦ 遗留发现（另开卡跟进，不在本卡范围）

1. **`SALES_RETURN_IN` 未登记**：`backend/src/business/erp/sales_return_posting.rs` 写这个 `biz_type`，
   但它既不在 `doc_kind.rs::TXN_BIZ_TYPE_VALUES`（22 个）里，也不在 `erp_invariants.py::E4_BIZ_TYPE_AXIS` 里
   ⇒ 任何跑过销售退货的库，`E4b` 恒红 BLOCKER（`cloudpivot_dev` 就是这样，`e4_registry_gap.py` 输出见
   `52_e4_registry_gap.out`）。这是**真·代码/巡检口径缺口**，不是夹具噪音。
2. **孤儿应收判据**要落进巡检/文档时记得带 `source_type='SALES_OUT'`，否则 `MANUAL` 占位 source_id 会把数字放大 4 倍。

---

## 复跑

```bash
# 验收闸（clean 应绿，dev 应红）
bash scripts/ops/acceptance_gate.sh
DB=cloudpivot_dev bash scripts/ops/acceptance_gate.sh
# 干净库冒烟（起 :18059，跑完自动清场）
bash scripts/datafix/i5/03_smoke.sh
# 孤儿应收只读盘点 / 软删（幂等）
psql -d cloudpivot_dev -f scripts/datafix/i5/01_orphan_ar_softdel.up.sql
```
