# CTR-02 合同域缺口收口证据（变更留痕 / 归档口径 / 已执行金额）

- 收口日期：2026-11-09（北京时间）
- 来源：CTR-01 合同链演练（`docs/evidence/20261108-ctr01-contract-chain/README.md` §3）的 F1 / F2 / F3 / F6
- 环境：本机（172.18.10.99）**隔离空库 `cloudpivot_ctr02`**（首启自动跑完 **285** 个迁移）+ 独立实例 `127.0.0.1:18228`（profile=test，未碰 :8023 / 未碰 144 生产）
- 二进制：`sha256=4e93f1644f4c084174b103adca3341e30149b0ce925d53d55505f08917a4a8f7`（`cargo build --bin cloudpivot`，debug）
- 实测：**28 条断言全绿（PASS=28 / FAIL=0）**，原始输出见本目录 `ctr02_drill_out.txt`，逐请求响应与库快照在 `raw/`

> 一句话结论：**草稿改单不再是"零留痕"**（每次 `PUT` 一条 `sys_change_log`，能查到谁、何时、从什么改成什么）；
> 合同详情现在直接给 **已执行金额 / 剩余额度 / 未关闭下游单据**，履约进度有了唯一口径；
> 归档（`ACTIVE→COMPLETED`）仍**不硬拦** —— 这是 ARCH-2 §4.1 E4 的既有决定，本轮把它写成可查的口径并写进文档；
> **SERVICE / LEASE 四类合同仍未开放**（要产品定乙方主档类型与审批人映射，本卡不拍板）。

---

## 1. 本轮改了什么（3 项，逐项对齐 CTR-01 的 finding）

| finding | 处置 | 落点 |
|---|---|---|
| **F2** 草稿改单零留痕 | 合同接入 `business::changelog`：`PUT` 与 `sys_change_log` **同事务**，diff 只记白名单 8 列；值没变不写空日志 | `changelog/diff.rs`（登记 `biz_contract`）+ `contract_model`（快照规范化）+ `contract_handler::contract_update` |
| **F6** 无「已执行金额」 | `ContractDetailRes` 补 `committed_amount` / `remaining_amount`（现算 SUM 两张订单表，与下单额度守卫**同一个函数**） | `contract_model::commitment_of` + `contract_dto` + `contract_handler::detail_of` |
| **F3** 归档不校验履约完成 | **不硬拦**（ARCH-2 §4.1 E4 原文「建议；不硬拦」），改为把口径**显式化**：详情返回 `open_order_count` / `open_order_nos`，并把口径写进 ARCH-2 §4.1 与 DTO 文档 | 同上 + `docs/architecture/ARCH-2-合同表数据模型与校验规则.md` |
| **F1** SERVICE / LEASE 未开放 | **未做**（见 §5） | — |

### 1.1 F2：变更留痕（判据：改金额 5 万 → 5.5 万后能查到「谁、何时、从什么改成什么」）

- 登记：`changelog::diff::BIZ_CONTRACT`，白名单 = `PUT` 真正能改的 8 列
  （`contract_name` / `party_b_id` / `party_b_name` / `amount` / `sign_date` / `start_date` / `end_date` / `remark`）。
  `status` **刻意不进白名单**：状态变更已有 `biz_status_log`（状态机唯一入口 + reason），两处都记等于各写半句。
- 快照：`contract_model::ContractDiffFields` 统一生成改前 / 改后两侧，
  **金额固定 4 位小数**（`NUMERIC(18,4)` 读回来是 `50000.0000`，客户端回传是 `50000` ——
  不规范化就会每次原样保存都写一条假日志）、日期 `YYYY-MM-DD`。
- 事务：`record_in(&tx, …)` 与业务写同事务；写日志失败 → 整笔回滚（不留「合同改了、审计没记」）。
- 实测（`raw/s3_*.json`、`raw/s3_diff.json`）：

  ```
  sys_change_log 行数：改前 0 → 改后 1
  row: operator=admin action=UPDATE(修改合同) changed=amount,end_date,remark ip=203.0.113.9 at=2026-11-09 04:23:37
  diff.fields.amount    = {b: "50000.0000", a: "55000.0000"}
  diff.fields.end_date  = {b: "2026-12-31",   a: "2027-06-30"}
  diff.fields.remark    = {b: "CTR-02 初始备注", a: "CTR-02 改后备注：金额与到期日都改了"}
  ```
- 反例（`raw/s4_noop.json`）：把同样的值原样再 `PUT` 一次 → 仍只有 **1** 行（diff 引擎返回 `Skipped`，不写空日志）。

### 1.2 F6：已执行金额 / 剩余额度

- `GET /api/business/erp/contract/{id}` 新增 4 个字段（`ContractDetailRes`）：
  `committed_amount` / `remaining_amount` / `open_order_count` / `open_order_nos`。
- 金额口径 = `purchase_model::committed_amount` + `sales_order_model::committed_amount`
  —— **与下单额度守卫同一个函数**（`status <> 'CANCELLED' AND deleted_at IS NULL` 的 `SUM(total_with_tax)`），
  所以「详情页的履约进度」和「还能不能再下单」永远对得上，前端不需要自己 SUM。
- `remaining_amount` **允许为负**（超签要看得见，不夹到 0）。
- 实测：无订单合同 `committed=0 / remaining=8000.0000 / open=0`；挂 2 张单后
  `committed=100000.0000 / remaining=-45000.0000 / open_order_count=2 / 单号两张`（`raw/s2_detail.json`、`raw/s5_detail.json`）。

### 1.3 F3：归档与「履约完成」的关系（口径，不是闸）

- **不返回 409**：ARCH-2 §4.1 E4 写的是「无未关闭下游单据（建议；不硬拦）」，§5.2 给的理由是
  「合同状态影响的是**能不能新承诺**，不是能不能继续履行已承诺的」——已成立的采购单不该因为合同被归档就收不了货。
- 「未完成」口径（写进 `contract_model::ContractCommitment` 文档 + ARCH-2 §4.1 注）：

  | 表 | 算「已完成」的状态 |
  |---|---|
  | `biz_purchase_order` | `RECEIVED` / `CLOSED` / `CANCELLED` |
  | `biz_sales_order` | `OUTBOUND` / `CLOSED` / `CANCELLED` |

  `DRAFT` / `SUBMITTED` / `APPROVED` / `PARTIAL_RECEIVED` / `PARTIAL_OUT` 都算未完成（货没到齐）。
- 实测（`raw/s6_*.json`）：合同 `ACTIVE` + 2 张未关闭采购单（33900 + 66100）时
  `PATCH /status {COMPLETED}` **200**（照旧放行），归档后详情仍能查到 `open_order_count=2`、
  `committed_amount=100000.0000` —— 报表要判「已履行」必须用 `open_order_count = 0`，
  不能只看 `status='COMPLETED'`。

---

## 2. 单元测试

| 命令 | 结果 |
|---|---|
| `ERP_DB_URL=…/cloudpivot_ctr02 cargo test --lib business::erp::contract_handler -- --test-threads=1` | **24 passed / 0 failed**（含新增 `update_draft_writes_change_log`、`detail_reports_commitment_and_open_orders`） |
| `ERP_DB_URL=…/cloudpivot_ctr02 cargo test --lib business::erp::contract -- --test-threads=1` | **77 passed / 0 failed**（合同域全量：dto / model / handler / 审批 / 联动 / PDF） |
| `cargo test --lib ctr02_tests`（纯逻辑，无需库） | **5 passed**（快照键集合 == 白名单、金额只差小数位不记、5 万→5.5 万只记 amount、清空备注记 null、剩余额度可为负） |
| `cargo test --lib`（全量，无 `ERP_DB_URL`；DB 用例按既有约定自跳过） | **3845 passed / 0 failed / 0 ignored** |
| `rustfmt +1.94.1 --edition 2024 --check` 4 个改动文件 | ✅ |
| `cargo clippy --lib` | ✅ 仅 3 条**既有**告警（`invoice.rs` 可省略生命周期、`receivable.rs` doc 缩进、`sales_order_model.rs` 参数 8/7），本卡 4 个文件零新增 |

新增测试的**断言面**（不是只跑通）：
- `snapshot_keys_match_registered_whitelist`：快照键集合与 `BIZ_CONTRACT` 白名单**逐字相等**
  （少一个键 = 那一列永远不记；多一个键 = 白名单漏登记）；
- `scale_only_difference_produces_no_change`：`50000` vs `50000.0000` **不得**产生 diff；
- `update_draft_writes_change_log`（真库）：`operator_name` / `client_ip` / `changed_fields` /
  `diff.fields.amount.{b,a}` 逐项断言 + 同值再改仍是 1 行；
- `detail_reports_commitment_and_open_orders`（真库）：`CANCELLED` 不计入已执行、
  `RECEIVED` 不算未完成、剩余额度为负不夹 0。

> 真库用例的两个夹具坑（已写进测试注释）：`utils::client_ip` 对回环 / 私网一律归零
> （用 TEST-NET-3 地址才能断言 IP）；`ck_biz_po_contract_pair` 要求
> `purchase_type='CONTRACT'` 时 `contract_id` 与 `contract_no_snapshot` 同时非空。

---

## 3. 隔离实例实测（28 条断言）

```bash
# 99 本机：建空库 + 首启（自动迁移 285 个）+ 起实例（见本目录 tools/00_env.sh）
bash tools/00_env.sh up     # 首次会 createdb cloudpivot_ctr02，然后起 127.0.0.1:18228
bash tools/01_drill.sh      # 期望 PASS=28 FAIL=0
bash tools/00_env.sh down   # 跑完停实例（库留着，便于复跑）
```

`01_drill.sh` 步骤：S0 登录改密 → S1 建供应商 + 2 张草稿合同 → S2 无订单口径 →
S3 改金额 5 万→5.5 万并核 diff → S4 同值回传不记 → S5 挂 2 张未关闭采购单核已执行/剩余/单号 →
S6 带未关闭单归档仍放行 + 口径仍可查 → S7 合同现状与状态日志。

`raw/*.json` 里的 `access_token` / `refresh_token` 已打码为 `<redacted>`（隔离实例的令牌本也无价值）。

---

## 4. 与「建议方案」的差异（都写在这里，别去翻对话）

1. **F3 没做 409，改成口径 + 数据**。理由：ARCH-2 §4.1 E4 明确「建议；不硬拦」，
   §5.2 明确「已有单据照旧」。硬拦会造出一个新的死结：合同永远归档不了（长单长期挂着），
   而 F3 真实的伤害是**报表高估**，那份口径用 `open_order_count` 就能算准。
   若产品后续要「必须关单才能归档」，改动点是 `contract_change_status` 里 `target == COMPLETED`
   分支加一道守卫（`commitment_of` 已经现成），本卡预留了这层判断的入口与口径。
2. **`status` 不进变更日志白名单**（状态已有 `biz_status_log`）。
3. **只记 `PUT`（草稿改单）**。`POST`（建单）与状态流转不在 F2 的缺口描述里；
   要记建单可以后续给 `contract_create` 加 `Action::Create`（登记表已就绪）。

---

## 5. F1（SERVICE / LEASE 四类合同）为什么没开

`SUPPORTED_TYPES` 仍是 `[PURCHASE, SALES]`。放开前必须先定三件事，都不是后端能单方面决定的：

1. **乙方主档类型**：服务商走 `biz_supplier` 还是新主档？出租方呢？
   （`ck_biz_contract_type_party_pair` 允许两种，但主数据里没有"服务商 / 出租方"分类）；
2. **审批人映射**：现在只有一套 `contract_approve` 流程（`n_pm` / `n_pm_hi` = `PURCHASE_MANAGER`，
   `n_gm` = `GENERAL_MANAGER`）。销售合同今天仍由采购经理批（CTR-01 §3 F5），
   服务 / 租赁合同该谁批？金额分级阈值是否沿用 10 万？
3. **额度口径**：服务 / 租赁合同要不要挂采购单 / 销售单？挂不上就等于没有履约勾稽
   （`committed_amount` 恒 0），那"已执行金额"对这两类合同没有意义。

建议单开卡（产品 + 架构定口径 → 后端实现），本卡不顺手开。

---

## 6. 未覆盖 / 边界

- 归档守卫按 §1.3 是**口径**不是闸：本轮**没有**制造「必须关单」的 409 断言（有意为之）。
- 采购单/销售单是 **SQL 夹具**插入的（`supplier_id` / `warehouse_id` 是弱引用，无外键）：
  本卡验的是履约口径 SQL 与接口形态，下单链本身由 CTR-01 覆盖。
- 未覆盖 `open_order_nos` 超过 20 张的截断（上限 `OPEN_ORDER_LIST_LIMIT=20`，`open_order_count` 是全量）。
- 未部署：145 / 144 都没动，实例在本机隔离端口，跑完即 kill。
