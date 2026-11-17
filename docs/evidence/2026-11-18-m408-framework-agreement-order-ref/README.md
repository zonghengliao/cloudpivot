# M4-08 框架协议：订单引用（校验价格 / 数量）· 证据

卡：`t_06b8c65b`（M4-08【M4·采购】框架协议：订单引用（校验价格/数量）· 超限拒绝）
上游：`t_7eb1c440`（M4-07 表结构，提交 `6019e4e5`）
基线：`git rev-parse HEAD` = `6019e4e5`（本卡提交前）

## 一句话

采购单可以挂框架协议（`framework_agreement_id` + 明细 `framework_agreement_item_id`），
下单时**锁协议行**校验三件事 —— 协议可用（`APPROVED` + 下单日在效期内 + 供应商/币种一致 + 物料在范围）、
单价 `≤ 协议价 × (1 + 容差)`（协议价含税时先按协议税率折成不含税）、累计数量/金额 `≤ 协议上限`
（明细 `max_qty` + 单头 `total_max_qty`/`total_max_amount`）；超了拒绝（`66096` / `66097` / `66098`），
之后按单据链**重算**协议行的已执行量（`executed_qty` / `executed_amount`），作废/删除自动释放。

## 文件

| 文件 | 内容 |
|:--|:--|
| `backend/app/migrations/business/V0451__framework_agreement_order_ref.up.sql` | 三列（单头 2 + 明细 1）+ 两个部分索引 + 末尾 DO 块自检（可空性/索引谓词/存量行不被改动/弱引用无外键/探针清理） |
| `.../V0451__framework_agreement_order_ref.down.sql` | 有单据挂协议就拒回滚；清空后删索引与三列 |
| `backend/app/src/business/erp/purchase/framework_agreement_ref.rs` | 校验（`resolve`）+ 执行量重算（`refresh_executed` / `release_order`）+ 详情出参（`ref_of_order`）+ 5 条纯逻辑单测 |
| `backend/app/src/business/erp/purchase/framework_agreement_ref_db_tests.rs` | 14 条真库用例（走真实建单入口 `create_purchase_order`） |
| `backend/app/src/business/erp/purchase/{purchase_dto,purchase_model,purchase_handler}.rs` | DTO 入参/出参、写库列、建单/改单/删除/作废接线 |
| `backend/crates/cloudpivot-kernel/src/shared/error_code.rs` | `66096` / `66097` / `66098`（+ 66xxx 稳定自检登记） |
| `scripts/verify/m408_framework_agreement_order_ref_drill.sh` | 本目录所有断言的脚本化（36 项） |

## 证据文件

| 文件 | 说明 |
|:--|:--|
| `01_drill_full.out` | 全量演练：**PASS=36 FAIL=0**（迁移幂等 / 结构 / 弱引用 / 范围守卫 / 空库全链重放 371 条 / 回滚三段 / 19 条 Rust 用例 / 门禁 / 夹具残留） |
| `02_teeth_skip_guard.out` | **有牙自检**：`M408_BAD_URL=1` → `PASS=33 FAIL=14 rc=1`（SKIP>0 + 正向标记丢失 ⇒ 假绿会被抓） |
| `03_schema_dump.txt` | 真库实测列/索引/外键/菜单/迁移登记 |
| `04_scope_no_routes_menus_frontend.txt` | 范围守卫：0 路由 / 0 权限码 / 0 菜单 / 0 前端；迁移号段 V0451 |
| `05_gates.out` | `cargo fmt --check` rc=0、`clippy --lib` 与 `--lib --tests` 全绿、66xxx 码段稳定自检 10 项全绿 |

## 口径（决定，不是待办）

1. **引用是可选的**：`framework_agreement_id IS NULL` = 不挂协议 —— 零星/紧急/协议外物料照常下单，
   不校验价格、不占额度。列写成 `NOT NULL` 等于「没有协议不许采购」，那是另一种业务。
2. **只允许「零星采购 + 框架协议」**：`CONTRACT + framework_agreement_id` → 400。合同采购已有合同定价，
   再挂一份协议就是两个价格上限，超限时说不清以哪份为准。
3. **校验看「现算的累计量」，不看回写列**：判据是「同一协议行上所有未作废且未删除采购单的 `Σ` + 本单」。
   回写列是给人看的快照，拿它做准入判据漏刷一次就误拒/误放（与合同额度守卫「缓存必漂移」同源）。
4. **回写是「重算」不是「累加」**：`refresh_executed` 每次把 `executed_qty` 赋成绝对值，
   天然幂等；`+=` 被重试/回滚/并发咬到一次就永久偏。
5. **并发靠协议行锁**：`resolve` 第一句 `SELECT … FOR UPDATE`（V0450 的 `lock_agreement`），
   锁到事务提交；锁顺序固定「合同行 → 协议行」，建单/改单同序，不交叉。
6. **比价前必须折口径**：采购单行价恒为不含税；协议价含税（`price_includes_tax`）时按协议行的
   `tax_rate`（**0–1 分数**，与采购单行的**百分数**税率不同）倒推成不含税再比。
   金额上限比较口径跟协议价走（含税协议比 `Σ with_tax_amount`，反之比 `Σ amount`）。
7. **作废 / 删除释放额度**：作废（`PATCH /status` 与联动作废都走 `apply_status[_with_instance]`）
   与软删都在同一事务里重算协议行 —— 作废单继续吃上限等于把协议量白占着，且没有手工纠正入口。
8. **协议行 id 客户端不许传**：服务端按 `(协议, 物料)` 现查（`uk_biz_fwai_material`），
   避免「传一个别家的协议行」绕过供应商/物料范围校验。
9. **两列都弱引用、无外键**（与 `contract_id` 同口径）：协议是软删表，外键拦不住软删；
   `ON DELETE CASCADE` 更会把历史采购单明细一起删掉 —— 那是账，不是配置。
   协议号另存 `framework_agreement_no_snapshot`，协议被删后历史单仍显示「当时按哪份协议下的单」。

## 与卡面/上游的偏差（只报不改）

- 上游 M4-07 只点名订单侧两个字段（`framework_agreement_id` / `framework_agreement_item_id`）；
  本卡**多存了一列** `framework_agreement_no_snapshot`（协议号快照），理由见口径 9，
  与 `contract_no_snapshot`（V0198）同款。
- 卡面 `超限拒绝` 落成三个错误码而不是一个：协议不可用 400/`66096`、单价超限 409/`66097`、
  数量或金额超限 409/`66098` —— 三种出路（换协议 / 改价 / 减量或补签）不同，混成一个码前端只能匹配中文串。
- 询价定标转采购单（`price_inquiry_handler`）与申请合并转采购单（`purchase_requisition_handler`）
  两条转单路径**显式不挂协议**（`framework_agreement_id: None`）：价格已由中标报价/申请单锁定，
  自动挂协议会让校验与来源单据打架；要不要挂由采购员在采购单上决定。
- M4-07 的 12 条真库用例在同一个库上仍然全绿（本卡没动协议两表结构）。

## 复核命令

```bash
# 演练（含空库全链重放，约 6–8 分钟）
bash scripts/verify/m408_framework_agreement_order_ref_drill.sh

# 只跑本卡真库用例（需先有已应用 V0450+V0451 的库）
ERP_DB_URL="postgres://cloudpivot:***@172.18.10.145:5432/cp_m408_drill" \
  cargo test -p cloudpivot --lib business::erp::purchase::framework_agreement_ref -- --test-threads=1
```
