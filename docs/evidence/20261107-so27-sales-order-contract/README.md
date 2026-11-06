# SO-27 销售订单关联销售合同 + 前端类型放开（证据包）

卡：`t_ef63b009`（ARCH-7 §15.1.2 / §15.1.3）｜分支：`main`｜工作副本：`/opt/cloudpivot`
实例：`cloudpivot_so27` @ `127.0.0.1:18227`（模板 `cloudpivot_so25`，227 条迁移）

---

## 一、交付了什么

| # | 卡面要求 | 落点 |
|---|---|---|
| 1 | 迁移加 `contract_id`（可空、FK → `biz_contract`、加索引） | `backend/migrations/business/V0302__sales_order_contract.{up,down}.sql` |
| 2 | 销售订单 DTO / Handler 增加 `contract_id` | `sales_order_handler.rs`（`CreateSalesOrderReq` / `UpdateSalesOrderReq` / `SalesOrderRes`）、`sales_order_model.rs`（`SalesOrderRow` / `NewSalesOrder` / INSERT / UPDATE） |
| 3 | 提交时校验累计订单额 ≤ 合同金额（超出 409） | `guard_contract_amount`（事务内 `FOR UPDATE` 锁合同 + 现算 `SUM(total_with_tax)`）→ 409 `66019` |
| 4 | 前端合同页放开 SALES、乙方选择器按类型切换客户 | `contract-logic.ts`（`SUPPORTED_CONTRACT_TYPES` / `contractPartyBKind` / `contractPartyBLabel`）、`contract/index.vue` |
| 5 | 前端销售订单页加合同选择器（只列 SALES + ACTIVE） | `sales-order/index.vue`、`sales-order-query.ts`、`api/business/erp/index.ts` |

**号段**：卡面写 `V0268`，实测 `V0268` 已被销售链占用（V0264–V0297 + V0301 全在），
按「已应用迁移不可改」取 **V0302**（开工时 `business/` 最大号 = V0301）。

### 迁移做了什么

```sql
ALTER TABLE biz_sales_order ADD COLUMN IF NOT EXISTS contract_id uuid;          -- ① 可空，不回填
ALTER TABLE biz_sales_order ADD CONSTRAINT fk_biz_sales_order_contract
    FOREIGN KEY (contract_id) REFERENCES biz_contract(id) ON DELETE SET NULL;    -- ② 外部存在性守卫
CREATE INDEX IF NOT EXISTS idx_biz_sales_order_contract
    ON biz_sales_order (contract_id, order_date DESC) WHERE contract_id IS NOT NULL; -- ③ 部分索引
```

迁移内自检（④）：列 / 外键类型 / 索引三者缺一即 `RAISE EXCEPTION`，并顺手拦
「存量订单挂着非 SALES 合同」这种方向性异常；末尾 NOTICE 播报存量张数。

> **与 ADR-06「单据链零外键」的差异（已在迁移头注释里单列）**：那里禁外键的理由是
> **多态引用**（合同乙方可能指供应商或客户，一列表达不了）与单据间互相锁死。
> `contract_id` 是**单态**引用（只会指向 `biz_contract`），软删不受外键影响，
> 物理清场用 `ON DELETE SET NULL`（不是 CASCADE：合同没了不等于订单没发生过）。
> 卡面第 1 条逐字要求 FK，故落 FK；撤掉只需 `DROP CONSTRAINT`，应用层校验与索引不依赖它。

### 后端校验链（挂合同的四关 + 额度）

| 判据 | HTTP / 码 | 位置 |
|---|---|---|
| 合同不存在 / 已软删 | 404 `RESOURCE_NOT_FOUND` | `resolve_sales_contract` |
| `contract_type <> 'SALES'` | 400 `66014` | `ensure_sales_contract_usable` |
| 状态不在 `contract_model::USABLE_FOR_ORDER`（`SIGNED`/`ACTIVE`） | 400 `66014` | 同上（**与采购单同一常量**，避免「详情说可用、下单却被拒」） |
| 合同乙方 ≠ 订单客户 | 400 `66044`（本卡新增码） | 同上 |
| `已下单(未作废) + 本单 > 合同金额` | 409 `66019`（复用采购侧同义码） | `ensure_within_contract_amount` / `guard_contract_amount` |

并发安全：额度判定在**建单/改单事务内**先 `contract_model::contract_lock_full`（`FOR UPDATE`）
再 `SUM`，锁序 = 合同行 → 订单写入（与采购链同序，不互死锁）；改单时 `exclude = 本单 id`
（否则自己算两遍）。累计额**不缓存**（缓存必漂移）。

改单语义：`contract_id` 不传 = 不改；传 `""` = 解绑；传 id = 换成这张（重过四关 + 额度）。

---

## 二、验收证据（全部可复跑）

```bash
# 后端（真库 + 真后端 + 真 token）
bash scripts/verify/so27_instance.sh up                 # 启动即应用 V0302
bash scripts/verify/so27_sales_order_contract_e2e.sh    # 58 项断言
```

### ① 迁移 APPLY_OK

`raw/` 下的响应体之外，关键记账查自 `schema_migrations`：

```
302|sales_order_contract|versioned|executed   ← kind=versioned, applied_via=executed（卡面第 3 条）
applied_seq=229（当前最大；本次启动最后应用的一条）
迁移条数 = 229（模板 227 + V0302；另 V0266 因邻居卡 checksum 漂移被重放，原地 update 不增行）
列 / 外键(fk_biz_sales_order_contract, confdeltype=n=SET NULL) / 部分索引 三项自检全绿
```

### ② 合同金额超限被拒（真实 curl）

```
POST /api/business/erp/sales-order  合同额 1000，先下 500（200 OK），再下 500（200 OK，正好用满）
第三单 100 → HTTP 409 + code 66019
msg: 合同 HT-20261107-000001 额度不足：合同金额 1000.0000，已下单 1000.0000（不含作废单），
     本单 100，下单后累计 1100.0000 将超出合同额（剩余额度 0.0000）
被拒的建单没有落库（同客户订单数仍为 2）
```

改单侧同样验过：把第一单改成 600（排除本单后 used=500 ⇒ 1100 > 1000）→ 409 `66019`，
且**库内金额保持 200 未变**（事务回滚干净）。

### ③ 前端能选到销售合同（页面用的那条查询，逐字复刻）

`frontend/src/views/business/erp/sales-order/index.vue::loadOptions()` 发的是
`GET /business/erp/contract/page?contract_type=SALES&status=ACTIVE&page=1&page_size=200`：

```
HTTP 200，返回列表里能查到刚成交的 SALES 合同（HT-20261107-000001）
同一列表里 DRAFT 的销售合同不在（状态闸生效）
下拉项带 contract_no（做 label）与 party_b_id（选合同自动带客户用）
```

### 前端门禁

| 检查 | 命令 | 结果 | 落盘 |
|---|---|---|---|
| 类型 | `pnpm typecheck` | rc=0（vue-tsc 0 错） | `frontend_typecheck.txt` |
| 构建 | `NODE_OPTIONS=--max-old-space-size=8192 pnpm build` | rc=0（`dist/` 全量产物） | `frontend_build.txt` |
| 单测 | `pnpm test:unit` | **987 passed / 0 failed**（含新增 2 条 SO-27 用例） | `frontend_unit.txt` |

> `pnpm build` 首轮 OOM（`vite build` 默认 1.5G 堆）—— 与本卡代码无关，是本机并发跑
> cargo 的老环境问题（SO-05/SO-13/SO-15 的 evidence 里都记过）；加 `NODE_OPTIONS` 后通过。

### 后端单测

| 检查 | 命令 | 结果 | 落盘 |
|---|---|---|---|
| 单测 | `cargo test --lib`（不带 `ERP_DB_URL`） | **2856 passed / 0 failed**（EXIT=0） | `cargo_test_lib.txt` |
| 全量 | `cargo test`（lib + 集成 + doctest） | 末尾 3 个 suite 全绿：`testd::*` 67 passed / 0 failed、doctest 1 passed / 11 ignored（日志尾部 `cargo_test_tail.txt`） | |

新增 3 条纯逻辑用例：`contract_id_is_optional_but_never_silently_ignored` /
`sales_contract_must_be_sales_usable_and_same_customer` /
`contract_amount_guard_rejects_only_when_exceeded`（覆盖可空解析、类型/状态/乙方三关、额度边界
「用满放行 / 超 0.0001 拒绝 / 累计已超也拒绝」）。

> **`cargo clippy --all-targets -- -D warnings`**：本卡 4 个 Rust 文件 **0 条诊断**
> （`grep -c` 计数 0）。整仓本次 clippy 报 11 条 `collapsible_if`，**全部落在
> `src/business/erp/receipt.rs`**（工作区里邻居卡的未提交改动，与本卡无关，故未处理）。

---

## 三、行为变化 / 兼容性

* 不挂合同的订单**行为零变化**（`contract_id` 可空、请求里可省略）；
* 建单 / 改单多两个可选校验，只在 `contract_id` 有值时触发；
* `SalesOrderRes` 多两个字段（`contract_id` / `contract_no`）—— 列表 `contract_no` 恒 `null`
  （与 `approval_instance_id` 同口径：列表不为显示多 JOIN 一次合同表）；
* 新增业务码 `66044 ERP_SO_CONTRACT_CUSTOMER_MISMATCH`（66xxx 段自检表已同步）；
* 前端合同页：`SALES` 从「置灰」变「可选」，乙方标签/占位/校验文案随类型切换
  （`乙方供应商` / `乙方客户`），切换类型会清空已选乙方（供应商 id 与客户 id 是两个 id 空间）。

## 四、已知取舍 / 留给后续卡

1. **前端只列 `ACTIVE`，后端放行 `SIGNED`**：后端口径取自 `contract_model::USABLE_FOR_ORDER`
   （与采购单同一常量），页面按卡面逐字只列 `ACTIVE`。⇒ API 调用方能挂已签署合同，UI 不能；
   若要让 UI 也放开，改 `loadOptions` 的 `status` 一处即可（不建议反过来收紧后端）。
2. **合同状态夹具用 SQL 置 ACTIVE**：`DRAFT → PENDING_APPROVAL → SIGNED → ACTIVE` 走
   `contract_approve` 审批流，属 SO-26 与合同域自己的用例；本卡验的是订单↔合同联动，
   故夹具直改状态并在脚本里逐步打印（不把审批流配置的成败耦合进本卡结论）。
3. `biz_sales_order.contract_id` 没有 `contract_no` 快照列：合同号不可改，显示现查即可；
   若将来合同号允许改，需补快照列（届时按 V0198 的快照纪律做）。
