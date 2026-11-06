# SO-29 销售退货后端 API + 状态机（证据）

日期：2026-11-07　｜　卡：`t_f7602e08`　｜　前置：SO-28（`V0303__sales_return`，提交 `6f2303b`）
架构依据：`docs/architecture/ARCH-7-销售模块设计.md` §15.2（表 / 流转 / 状态机）

## 1. 交付内容

| 文件 | 性质 | 说明 |
|---|---|---|
| `backend/src/business/erp/sales_return_model.rs` | **新增** | 数据层：`SR-yyyyMMdd-NNNNNN` 取号 / 6 态域 / 7 条边表 / **超退守卫**（`已发货量 − 已退量`，支持排除自身）/ 建单 / 分页 / CAS 落状态 + 10 条单测 |
| `backend/src/business/erp/sales_return_handler.rs` | **新增** | 接口层：5 个接口（建 / 详 / 分页 / 可退明细 / 状态流转）+ 三道闸 + 9 条单测 |
| `backend/src/business/shared/doc_kind.rs` | 改 | `DocKind::SalesReturn`（ALL 23→24 / as_str / label / table / is_document / route）+ `DOC_KIND_VALUES`（24） |
| `backend/src/business/shared/state_machine.rs` | 改 | `ENTITY_SALES_RETURN` + `SALES_RETURN_EDGES`(7) / `SALES_RETURN_BASE`(7) / `SALES_RETURN_REASON_EDGES`(2) / `SALES_RETURN_LOCKED`(2) + `Entity::SalesReturn` 七个注册点（ALL 19→20 + 单测断言） |
| `backend/src/business/flow/linkage/spec.rs` | 改 | `TRIGGER_EXEMPT` 登记 `SALES_RETURN`（不接联动引擎，理由写进注释） |
| `backend/src/business/erp/mod.rs` | 改 | 两个 `pub mod` |
| `docs/architecture/snapshot-drift/10_checks.sql` | 改 | `STATUS_ENTITY.SALES_RETURN → biz_sales_return`（与 `DocKind::ALL` 逐行镜像，单测钉死） |
| `frontend/src/api/business/erp/doc-kinds.ts` | 改 | 词表镜像：`DOC_KINDS` / `DOC_KIND_LABELS` / `DOC_KIND_TABLES` 各补 `SALES_RETURN`（`DOC_KIND_ROUTES` **不加**：加了等于对外宣称支持 `/cancel-impact`，该接口不存在） |
| `backend/migrations/business/V0304__sales_return_permissions.{up,down}.sql` | **新增** | 3 个权限码（list/create/status）：1 页面节点 + 2 按钮，`path/component=NULL`、`is_visible=FALSE`（前端页落地后一条 UPDATE 收尾）；不写 `sys_role_menu`（不静默扩权） |
| `scripts/verify/so29_*.{sh,sql}` | 新增 | 建库 / 起实例 / 夹具 / e2e（79 断言）/ V0304 回滚演练 |

接口（权限码与 V0304 逐字一致）：

| 方法 | 路径 | 权限码 | 关键行为 |
|---|---|---|---|
| POST | `/api/business/erp/sales-return` | `business:erp:sales-return:create` | 落 `DRAFT`；`items` 留空 ⇒ **从原发货单带出可退明细**（数量 = 可退量）；金额服务端算 |
| GET | `/api/business/erp/sales-return/{id}` | `…:list` | 明细 + 客户名 / 仓库 / 原订单号 / 原发货单号 |
| GET | `/api/business/erp/sales-return/page` | `…:list` | 分页（单号模糊 / 状态 / 订单 / 客户 / 发货单），一页固定 4 条补齐 SQL、无 N+1 |
| GET | `/api/business/erp/sales-return/returnable/{shipment_id}` | `…:list` | 该发货单可退明细（已发 / 已退 / 可退 / 单价 / 税率） |
| PATCH | `/api/business/erp/sales-return/{id}/status` | `…:status` | 只走 7 条合法边；非法迁移 409 + 可选去向；`SUBMITTED/APPROVED → CANCELLED` 必须给原因 |

## 2. 设计口径（与稿面的差异，逐条给理由）

1. **超退口径按「单据链」而不是「订单行」**：指定 `shipment_id` 时，已发货量取**该发货单该行**发出的数量、已退量只算**同一张发货单**上未作废退货
   （`biz_return.shipment_id = ? AND so_item_id = ?`）；不指定发货单时退回 `biz_sales_order_item.delivered_qty − 全部未作废退货`。
   理由：一张订单可能分多次发货，按订单行统一算会让「A 单已退满」把 B 单的可退量也吃掉。
2. **不变量里必须排除「本单自己」**：提交 / 待过账前重跑超退判定时传 `exclude_return = Some(本单 id)`。
   不排除的话，一张「10 退 10」的单自己就被算成「已退 10」⇒ 永远提交不了（e2e 第一轮实测就是这个 400）。
3. **`POSTED` 本卡显式 409**：状态机边已登记（卡面要求），但过账执行体（入库单 + `inventory_writer` 回库 + 红字入库凭证 + 冲应收 + 红字发票 + 回写 `delivered_qty`）是 **SO-30 的一个事务**。
   本卡不假装「状态过账、账没动」，409 文案点名 SO-30。SO-30 落地时把这段闸删掉换实现（与 SO-09 → SO-10 同一套分工）。
4. **只有已过账（`POSTED`）的发货单能退**：货还没出库就没有「退回来」这回事；未过账发货单 409。
5. **`PENDING_POST → CANCELLED` 故意没有边**：稿面 §15.2.4 只画到 `APPROVED` 的作废箭头；待过账要撤先回批准（本卡也未开放回退边 ⇒ 待过账的单只能继续过账，由 SO-30 的过账卡决定撤回语义）。
6. **`return_type` 不设默认值**：漏传宁可 400，也不静默记成 `OTHER`（与 SO-28 的列口径一致）。
7. **无原单行（`OTHER`）不参与超退判定**：没有原单就没有「已发 / 已退」可算，只校验 `qty > 0` 与金额非负 —— 但必须自带 `material_id`（明细表 NOT NULL）。
8. **审批流不接**（本卡卡面只要求 CRUD + 状态机）：`SUBMITTED → APPROVED` 走人工 `PATCH /status`；`approval_instance_id` 列留空，接审批流是后续卡。

## 3. 门禁实测

### 3.1 编译 / 单测

```
cargo check --all-targets        → Finished（零警告）
cargo fmt -p cloudpivot -- --check → exit 0
cargo test --lib                 → 2875 passed; 0 failed（含本卡 19 条）
```

单测覆盖：6 值状态域 / 3 值退货类型 / 7 条边（跳步・回退・终态全拒）/ 超退公式（边界 7/7.0001、已退>已发、0 与负数）/
金额四舍五入 + 合计恒等 / 单号拼解析（7 个非法形态）/ 序列号一物一码 / 重复订单行拒绝 / 权限码字面量 / 闸门状态集 / 分页归一。

> `cargo clippy --lib -- -D warnings` 在本工作区 **红 11 处**，全部指向
> `backend/src/business/erp/receipt.rs`（`collapsible_if`）—— 该文件是**邻居未提交的 WIP**（本卡开工前 `git status` 就已是 `M`，
> 本卡未纳入提交），本卡改动的 7 个文件 **零 clippy 诊断**（`grep -c sales_return clippy.log = 0`）。

### 3.2 迁移（空库全量重放，`apply_clean_db.txt`）

```
数据库迁移完成: 应用 231 个迁移
303 | sales_return             | versioned | executed | applied_seq=220
304 | sales_return_permissions | versioned | executed | applied_seq=221
applied_via 分组：executed=231
biz_sales_return 22 列 / biz_sales_return_item 14 列；fk_biz_sreturn_item_return 等 8 条约束
biz_seq: SR | SR | yyyyMMdd | step=1
```

已迁移库重启：`应用 0 个迁移` + `/health 200`（`boot_and_migration.txt`）。

### 3.3 V0304 回滚演练（`v0304_drill.txt`）

```
基线 3 行 → down 后 0 行 → up 后 3 行 → 二次 up 仍 3 行
明细：menu_type=2 页面（parent=NULL, visible=false）+ 2 个 menu_type=3 按钮
```

### 3.4 真实 token curl（`e2e_curl.txt`，79 断言全绿）

| 组 | 断言 |
|---|---|
| 迁移记账 | V0303/V0304 kind=versioned + applied_via=executed、`applied_via` 口径、两张表、3 个权限码承载行、CASCADE 外键 |
| 可退明细 | SH-0001 → 已发 10 / 已退 0 / 可退 10 / 单价 100 / 税率 13 |
| 建单（自动带出） | 200 + `SR-20261107-0000NN` + DRAFT + 明细 1 行 qty=10 + 1000/130/1130 + 原订单号 / 原发货单号 / 客户名 |
| 详情 / 分页 | 单号一致、明细带 `so_item_id`、列表行明细为空、total=1 |
| 状态机主链 | SUBMITTED → APPROVED → PENDING_POST 全 200；POSTED **409 且文案点名 SO-30**；`biz_status_log` 4 条（建单 + 3 次流转）且 `entity_no` 唯一 |
| 非法边 / 原因 | DRAFT→APPROVED 409（文案给去向）；SUBMITTED→CANCELLED 缺 reason 400；带 reason 200；终态再流转 409；轨迹 3 条 |
| 超退 | SH-0002 已发 5：退 3 → 200（678）；再退 3 → **400「超退」且不落库**；作废前一张后重退 5 → 200（1130） |
| 原单闸 / 参数 | 未过账发货单 409（文案含 POSTED 要求）；`return_type=REFUND` 400；无 items 且无 shipment 400；仓库不存在 400 |
| 按订单退货 | 第 3 行（delivered_qty=3，无发货单）退 3 → 200（169.5）；再退 0.0001 → 400 |
| 无原单 OTHER | 缺 `material_id` 400；自带物料退 2×9.5 → 200（19 / 2.47） |
| 汇总 | 6 张成功、7 次被拒不落库；状态分布 `CANCELLED=2,DRAFT=3,PENDING_POST=1` |

### 3.5 前端

```
pnpm typecheck → 通过（vue-tsc --noEmit，无输出）
pnpm build     → 通过（dist/index.html 已生成；首次与 cargo test 并发时 node OOM，
                 NODE_OPTIONS=--max-old-space-size=6144 重跑通过 —— 环境内存竞争，非代码问题）
```

## 4. 复跑方式

```bash
bash scripts/verify/so29_db_setup.sh                    # 建空库 cloudpivot_so29
bash scripts/verify/so29_instance.sh up                 # :18229（空库 ⇒ 应用 231 个迁移）
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_so29 \
  -f scripts/verify/so29_sales_return_fixture.sql       # 夹具（可反复执行）
bash scripts/verify/so29_sales_return_e2e.sh            # 79 断言
bash scripts/verify/so29_v0304_drill.sh                 # V0304 down→up→up
bash scripts/verify/so29_instance.sh down               # 收尾
```
