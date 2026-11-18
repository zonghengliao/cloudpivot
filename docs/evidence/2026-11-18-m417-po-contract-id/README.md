# M4-17 采购订单挂合同字段（`contract_id`）· 交付与验收证据

- 卡：`t_566fc2dc`（M4-17【M4·采购】采购订单加 contract_id 字段 —— 迁移 + DTO）
- 日期：2026-11-18
- 结论一句话：**卡面要求的「迁移 + DTO」在仓库里早已完整落地（来源迁移 `V0198`），
  本次没有新写迁移、没有改业务代码**；本卡交付的是这个字段的**可复跑验收**
  `scripts/verify/m417_purchase_order_contract_id_drill.sh`（54 条断言全绿）+ 本文证据。

---

## 1. 为什么没有新迁移（这是决定，不是漏做）

`biz_purchase_order.contract_id` 早在 **ARCH-2 合同勾稽批次**就随
`backend/app/migrations/business/V0198__erp_po_contract_bridge.up.sql` 落地，
本次核对时它已在 `schema_migrations` 里 `executed`（version=198，checksum 与仓库文件
sha256 逐字节一致）。同批还一起落地了 `purchase_type` / `contract_no_snapshot` /
`supplier_name_snapshot` 三列、双向钉死约束 `ck_biz_po_contract_pair`、
部分索引 `idx_biz_po_contract`，以及 DTO / 模型 / handler / 前端全套接线（见第 3 节）。

再补一条同义迁移的代价：

1. **白占迁移号**：AGENTS 约定 6「版本 4 位补零、跨目录全局唯一、先落地者定号」，
   当前已到 `V0457`，加一条不改变任何约束的迁移只是把号往后推一格；
2. **加不出新约束**：现有 `ck_biz_po_contract_pair` 已经是**双向钉死**
   （`CONTRACT` 必须有 id + 合同号快照；`SPOT` 两者必须为空），再写只会重复；
3. **两处真相**：字段「归谁管」会变得说不清，将来改口径要改两个文件。

所以本卡的验收面改成：**证明这个字段从库到前端一路都在，并且掉一截就会红**。

---

## 2. 验收命令与结果

```bash
bash scripts/verify/m417_purchase_order_contract_id_drill.sh          # 99 上跑，默认连 145 的 PG
PG_HOST=127.0.0.1 bash scripts/verify/m417_purchase_order_contract_id_drill.sh   # 在 145 上跑
```

本次实测（`02_drill.txt`，库 = `cloudpivot_test` 克隆 + 迁移 V0405…HEAD 回放）：

```
== 汇总：PASS=54 FAIL=0 ==
演练通过（rc=0）
```

五段各验什么：

| 段 | 验什么 | 关键断言（节选） |
|---|---|---|
| ① 迁移来源 | V0198 已 executed、指纹一致、up/down 的重复执行守卫是 existence-guard 形状 | `checksum == sha256(文件)`；`ADD COLUMN IF NOT EXISTS`；`IF NOT EXISTS (SELECT 1 FROM pg_constraint …)` |
| ② 库结构 | 四列的类型/可空/默认/长度、两条 CHECK、**部分**索引、**零外键**、四列都有 COMMENT、筛选真能走索引 | `contract_id uuid 可空无默认`；`purchase_type NOT NULL DEFAULT 'SPOT'`；`idx_biz_po_contract … WHERE (contract_id IS NOT NULL)`；`fk_count = 0`；`EXPLAIN → Index Scan using idx_biz_po_contract` |
| ③ 约束反证 | 四种非法组合**必须被库拦下**，合法行**必须插得进** | `CONTRACT` 缺 id / 缺合同号快照 / `SPOT` 带 id / 枚举拼成 `CONTRCAT` → 全部报指定约束名；合法 `CONTRACT` 行 `INSERT 0 1` |
| ④ 代码接线 | 列清单 / 行映射 / 分页筛选 / DTO 入参出参 / 写库 SQL / 累计额现算 / handler 两处守卫 / 前端只发 id | 13 条 grep 断言，锚点见第 3 节 |
| ⑤ 真库用例 | `purchase_handler::tests` 里 **13 条**合同相关用例 | 8（contract_*）+ 2（spot_order_*）+ 1（legacy_spot_order_still_readable）+ 1（update_without_type_keeps_contract_type）+ 1（page_exposes_contract_snapshot_columns），失败 0 / SKIP 0 |
| 收尾 | 夹具残留 | 脚本按固定 id 清场后断言「采购单 / 合同回到跑测试前的基线」「`M417-%` 探针行 0 残留」 |

> 为什么第 ③ 段要求「失败原因必须是**指定那条约束**」：只判「INSERT 失败」会把
> 语法错、列不存在当成「约束拦下了」—— 那是假绿。实测中 `CONTRCAT` 这一条同时违反
> 两条 CHECK，PG 报的是先命中的 `ck_biz_po_contract_pair`，所以这一条按「二选一」匹配。

---

## 3. 字段落点（改这个字段前先看这张表）

| 层 | 文件:行 | 内容 |
|---|---|---|
| 迁移 | `backend/app/migrations/business/V0198__erp_po_contract_bridge.up.sql:66` / `:92` / `:112` | `ADD COLUMN IF NOT EXISTS contract_id uuid` / `ck_biz_po_contract_pair` / `idx_biz_po_contract`（部分索引） |
| 行结构 | `backend/app/src/business/erp/model_order.rs:97` / `:155` / `:29` | `PurchaseOrderRow.contract_id: Option<Uuid>` / `from_row` 读列 / `PURCHASE_ORDER_COLUMNS` 带上四列 |
| 列表筛选 | `model_order.rs:213-215` | `AND contract_id = ?`（注释写明走部分索引） |
| 出参 | `backend/app/src/business/erp/dto.rs:2526` / `:2528` | `PurchaseOrderRes.contract_id` / `contract_no`（快照列） |
| 筛选入参 | `dto.rs:2368` | `PurchaseOrderFilter.contract_id` |
| 建单/改单入参 | `backend/app/src/business/erp/purchase/purchase_dto.rs:113` / `:330-343` | `CreatePurchaseOrderReq.contract_id`；`CONTRACT` 必填、`SPOT` 必须为空（400） |
| 写库 | `backend/app/src/business/erp/purchase/purchase_model.rs:507` / `:591` | INSERT 写四列；UPDATE 用 `NULLIF(?::text,'')::uuid` 归一空串（API 层与实体层不对齐就撞这句） |
| 额度现算 | `purchase_model.rs:1052`（`committed_amount`） | `SUM(total_with_tax) WHERE contract_id = ? AND status <> 'CANCELLED'` —— **不缓存**（缓存必漂移） |
| 合同解析 | `purchase_handler.rs:202`（`resolve_contract`） | 存在 / 未软删 → `contract_type = PURCHASE` → `SIGNED`/`ACTIVE` → 下单日落在有效期内 |
| 额度守卫 | `purchase_handler.rs:359`，调用在建单 `:715`、改单 `:1057` | 事务内 `FOR UPDATE` 锁合同行 → 现算累计额 → 超限拒绝 |
| 前端表单 | `frontend/src/views/business/erp/purchase/purchase-logic.ts:125` / `:903-908` / `:818` | 表单态；合同采购**只发** `contract_id`（供应商由服务端从乙方派生）；空则前端先拦 |
| 前端展示 | `frontend/src/views/business/erp/purchase/index.vue:95-97` / `:662` / `:969` | 列表只读快照列（不 JOIN 合同表）；详情用 `contract_id` 跳合同页 |

原始 grep 输出见 `05_wiring.txt`。

---

## 4. 变异反证（演练会不会红）

把 `model_order.rs:155` 的 `contract_id: row.get_typed("contract_id")?` 改成
`contract_id: None`（编译无碍，语义上等于「详情永远 null」），演练结果：

```
FAIL 行映射读 contract_id（漏读 = 详情永远 null）（期望 [1]，实际 [0]）
== 汇总：PASS=35 FAIL=1 ==
```

**恰好 1 条红，且红的正是被变异的那一条**；还原后 `sha256` 与变异前逐字节一致、
`git status` 该文件干净。原始输出见 `03_mutation.txt`。

---

## 5. 门禁

| 门禁 | 结果 | 证据 |
|---|---|---|
| `cargo fmt --check` | rc=0 | `04_gates.txt` |
| `cargo clippy -p cloudpivot --lib -- -D warnings` | rc=0（Finished 36.89s） | `04_gates.txt` |
| 真库用例（13 条合同相关 + 全演练） | 失败 0 / SKIP 0 | `02_drill.txt` |

本卡没有前端改动（字段与页面都已在 HEAD），因此未重复跑 `pnpm typecheck/build`；
前端接线由第 3 节的行号 + 演练第 ④ 段的 grep 断言守住。

---

## 6. 本次实测发现（只报不改）

### 6.1 `V0198` 的 up 在升到 HEAD 的库上**不能重跑**（与它自己的「幂等」声明冲突）

V0198 头注释写「up 重复执行是 no-op」，但它开头的 **S2-① 存量体检**要求
「非 `PURCHASE` 的入库不得带供应商」；而后来的迁移把 `ck_biz_si_supplier_scope`
放宽到了 `SOURCELESS` 也允许带供应商（当前库里有 **12 行** `SOURCELESS` 带供应商）。
于是：

- 首次应用（≤V0198 的库）→ 通过；
- 在 HEAD 库上重跑 up → `ERROR: S2-① 有 12 行非采购入库带着供应商`；
- 先跑 down（会删四列 + 删该约束）再 up → 同样被 S2-① 拦下，**字段就回不来了**。

这是「迁移写死了当时的数据假设」的典型，不是本卡引入的缺陷；已应用迁移按约定 6
**不可改**（指纹校验），故记录在案、不动它。实际影响面很小：`schema_migrations`
保证 V0198 不会被重跑；只有「手工回滚这一段」的场景才会踩到。
**结论**：不要拿 `V0198 down → up` 当回滚演练；真要回滚，用文件末尾那套「档 A（无损）」。

### 6.2 M4-18（订单挂合同校验，金额不超合同）在 HEAD 上也已生效

卡链上下一张卡 M4-18 的目标是「超限拒绝」。它在 A3 批次已随
`purchase_handler::guard_contract_amount`（`:359`，建单 `:715` / 改单 `:1057` 两处调用）
+ `purchase_model::committed_amount`（`:1052`）落地，且有用例
`contract_amount_limit_is_enforced` 钉住（本演练第 ⑤ 段真跑通过）。
**建议下一张卡先核验、再决定是否新写**，否则会重复实现同一道守卫。

### 6.3 夹具卫生

13 条合同相关用例自带清场：`setup()` 先 `purge_fixture_docs()`（先解弱引用 / 自引用外键、
删实例与状态日志、删采购单、最后删三张合同夹具），演练跑完净增量 0
（`02_drill.txt` 第 7 段：采购单回到基线 15、合同回到基线 11）。

---

## 7. 环境与清理

- 在 **99（开发机）** 上跑，连 **145** 的 PG（`172.18.10.145:5432`，用户 `cloudpivot`，
  模板库 `cloudpivot_test` = V0404，脚本再回放 V0405…HEAD）。
- 演练库名 `cp_m417_po_contract_drill_<pid>` / 证据库 `cp_m417_evidence_drill`，
  都含 `drill`（脚本硬校验，不匹配直接 rc=2）；跑完脚本自己 DROP，
  证据采集脚本也在采完结构后 DROP。
- 收尾核对：`pg_database WHERE datname LIKE 'cp_m417%'` → 0 行。
- 脚本里的临时日志写在 `$TMPDIR`，不落仓库。

---

## 8. 本目录文件

| 文件 | 内容 |
|---|---|
| `01_db_structure.txt` | 四列的类型/默认/长度、列注释、三条 CHECK、部分索引定义、外键数 0、`EXPLAIN` 走索引、合同单/零星单分布 |
| `02_drill.txt` | 完整演练输出（54 条断言，`PASS=54 FAIL=0`，`DRILL_RC=0`） |
| `03_mutation.txt` | 变异反证：改一行 → 恰好 1 条红；还原后 sha256 一致 |
| `04_gates.txt` | `cargo fmt --check` / `cargo clippy --lib -D warnings` |
| `05_wiring.txt` | `contract_id` 在代码里每一段落点的原始 `grep -n` |
