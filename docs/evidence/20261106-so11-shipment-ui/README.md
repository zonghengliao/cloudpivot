# SO-11 发货单前端页面（t_b143cd85）证据包

承接 SO-10（`t_83e23c1d`：发货过账 → 销售出库 → 库存扣减）。本卡只交付**前端页面 + 路由 + 菜单收尾迁移**，
后端接口一行未改（发现 1 处后端缺口，已开缺陷卡，见 §5）。

## 1. 交付物（改了什么）

| 文件 | 说明 |
|------|------|
| `frontend/src/api/business/erp/shipment.ts`（新增） | 5 个接口 + 全部契约类型（`Shipment` / `ShipmentItem` / `ShippableItem` / `CreateShipmentReq` / `ShipmentStatusReq`），字段逐字抄 `shipment_handler.rs` |
| `frontend/src/views/business/erp/shipment/shipment-query.ts`（新增 466 行） | 纯逻辑：4 态状态域/中文/配色、`SHIPMENT_EDGES`（4 条边）+ `SHIPMENT_REASON_EDGES`（1 条）、按钮矩阵、筛选→请求参数、`linesFromShippable`（只带未发完）、表单校验、`buildCreatePayload`、过账/出库单文案 |
| `frontend/src/views/business/erp/shipment/index.vue`（新增，~1250 行） | 列表（服务端分页 + 单号/状态/客户筛选）· 新建弹窗（选订单 → 自动带出未发完明细 → 改数量/选库位/删行）· 详情抽屉（4 态状态线 / 明细 / 关联出库单号）· 提交待过账 · **过账确认弹窗** · 作废（原因按边表必填） |
| `frontend/src/router/index.ts` | 注册 `business/erp/shipment` → `BusinessErpShipment`（`meta.permission = business:erp:shipment:list`） |
| `backend/migrations/business/V0286__sales_shipment_menu_page.{up,down}.sql`（新增） | 把 V0284 故意留空的菜单 `path/component/is_visible` 补成终态、`title` 收敛成「发货单」；自检 + 幂等 + 回滚断言（按钮父级不动） |
| `frontend/tests/unit/erp-shipment.test.ts`（新增） | 27 条单测：状态表/边表**直接读后端源码比对**、按钮矩阵、带出规则、校验、请求体形状、文案 |

## 2. 三条硬口径（都在代码注释里写明）

1. **过账不是状态标记**：`PENDING_POST → POSTED` 同事务生成销售出库单（`source_type=SALES`）、扣三本账、
   生成成本结转凭证、回写订单 `delivered_qty` 与状态 —— 确认弹窗只陈述这三件事，不多说一句。
2. **`DRAFT` 不能直接过账**（边表里没有这条边）⇒ 列表与详情都是「提交」和「过账」两个按钮，不拼假动作。
3. **发货单没有审批流**（`ShipmentRes` 没有 `approval_instance_id`）⇒ 卡面写的「复用 FlowProgress（如需）」
   在这里**用不上**：审批链在销售订单（`so_approve`）那一层。页面画的是单据自己的 4 态状态线
   （`DRAFT → PENDING_POST → POSTED`，`CANCELLED` 用红色 tag 表达，不进主链）。

## 3. 门禁结果（每条都有落盘文件）

| 门禁 | 命令 | 结果 |
|------|------|------|
| 类型检查 | `frontend: npx vue-tsc --noEmit` | **rc=0**（零错误） |
| 前端构建 | `vite build --outDir <仓库外>/so11-dist --emptyOutDir`（等价 `pnpm build`，**不覆盖生产 dist**） | **成功**，`assets/index-*.js` 里能命中「新建发货单」「关联出库单」 |
| 前端单测 | `npx tsx --test tests/unit/erp-shipment.test.ts` | **27 passed / 0 failed** |
| 真实 token curl（页面消费的 5 个接口） | `bash curl-e2e.sh`，跑两轮 | **两轮各 67 条断言全通过**（`curl-round1.txt` / `curl-round2.txt`） |
| 真机 UI（Playwright + vite dev + 隔离后端） | `bash ui-prepare.sh && node ui-verify.mjs`，跑两轮 | **两轮各 27/27 通过**（`ui-round1.txt` / `ui-round2.txt`），截图见 `shots/` |
| 菜单 ↔ 路由（n08 同口径复算） | `python3 n08_check.py` | 路由 87 条 / 菜单页 78 条 / **MISSING=0**（`n08-recheck.txt`） |
| 迁移 | 起后端自动迁移 | `version=286 name=sales_shipment_menu_page kind=versioned applied_via=executed` |

### 3.1 curl 覆盖（`curl-e2e.sh`，每轮 67 条）

页面真正会打的 5 个接口 + 3 个反路径：

1. `GET /sales-order/page?status=APPROVED`（订单下拉数据源）；
2. `GET /shipment/shippable/{so_id}`：只带未发完的行，`remaining = ordered − delivered − reserved`；
3. `POST /shipment`（**前端 payload 形状**：显式 `items`、`qty` 字符串、行备注；`material_id` 不传）
   → 200 DRAFT、`SH-yyyyMMdd-nnnnnn`、`total_qty`；
4. `GET /shipment/page`：单号模糊 + 状态精确命中；**列表行 `items=[]`、`stock_out_no=null`**；
5. `GET /shipment/{id}`：`so_no` / 客户 / 仓库 / 明细齐全，未过账时 `stock_out_no=null`；
6. `PATCH /status → PENDING_POST`：不动库存、不出出库单；
7. `PATCH /status → POSTED`：`stock_out_no = OUT-yyyyMMdd-nnnnnn`，库内三账 10→0、出库单 `source_type=SALES`、
   成本结转凭证生成、订单行 `delivered_qty=10`、订单 `OUTBOUND`；
8. 反路径：重复过账 409（库存不再变）、`PENDING_POST→CANCELLED` 缺 reason 400 / 带 reason 200、
   `DRAFT→CANCELLED` 免原因、超发 `qty=11>10` 400、零库存过账 409（三账不动）、草稿订单建单 409、
   指定库位有账 200 / 指定库位无账 409（**绝不静默改走 FIFO**）。

### 3.2 真机 UI 覆盖（`ui-verify.mjs`，每轮 27 条）

列表页打开 → 新建弹窗选订单**自动带出 1 行且数量默认 10** → 选仓库 → 创建（详情抽屉自动打开，状态「草稿」，
来源订单号/关联出库单号「未过账」可见）→ 提交待过账（库存仍 10）→ 过账确认弹窗（点名单号）→ 确认 →
状态「已过账」且**详情显示 `OUT-20261106-000037`** → 库内核对（成本账/库位账 0、出库流水 10、出库单来源 SALES、
凭证已生成、`delivered_qty=10`、订单 `OUTBOUND`）→ 关抽屉刷新列表，行状态「已过账」。

## 4. 复现环境（隔离实例，不动共享后端/生产）

```bash
# 1) 克隆库（cloudpivot_so10 已到 V0285）+ 起一台 18099
su postgres -c "createdb -T cloudpivot_so10 -O alun cloudpivot_so11"
bash <scratch>/start-so11-backend.sh          # NIK_* / 64hex JWT+STORAGE_KEY，自动迁到 V0286
# 2) curl 证据（两轮）
BASE=http://127.0.0.1:18099 DB=cloudpivot_so11 bash curl-e2e.sh
# 3) 真机 UI：vite dev 指到 18099（**绝不在 /opt/cloudpivot/frontend 里 build 生产 dist**）
cd /opt/cloudpivot/frontend && BACKEND_ORIGIN=http://127.0.0.1:18099 ./node_modules/.bin/vite --port 5191 --strictPort
bash ui-prepare.sh && node ui-verify.mjs
```

## 5. 实测发现的后端缺口（已开缺陷卡 → backend-developer）

`shipment_handler::page_shipment` 批量补齐了 `customer_*` / `warehouse_*`，**漏了 `so_no` 与 `stock_out_no`**：

```
GET /api/business/erp/shipment/page → data.list[0].so_no = null,  stock_out_no = null
GET /api/business/erp/shipment/{id}  → data.so_no = "SO-20261106-000016", stock_out_no = "OUT-….000001"
```

而 `ShipmentRes` 的文档注释写的是「列表行 `so_no` / `customer_code` / `warehouse_code` 会批量补齐」。
影响：列表的「销售订单」列与「关联出库单」列在列表里只能兜底显示（「见详情」/「未过账」），
详情页数据是完全正确的（截图 `04-detail-posted.png` 可见出库单号）。

前端**没有**为了绕开这个缺口去改后端，也没有按 `null` 判「订单已删除 / 出库单已删除」——
那种写法会把每张正常单据都说成坏数据。兜底文案 + 缺陷卡是本卡的选择。

## 6. 其他实测坑（写进代码注释，供后续卡片省时间）

- **`shipment_model::status_label(CANCELLED)` 的中文是「已取消」**（不是「已作废」）：动作叫「作废」、
  状态叫「已取消」，前端兜底表逐字对齐后端（单测读源码比对）。
- **指定库位要两本账同时就位**：`biz_inventory_location.location_id` = **库位字典 id**，
  而 `biz_inventory.location_id` = **库位账行 id**（`stockout_model::fifo_location_row` 的注释写明）。
  夹具只写一本 → 过账 409「该库位桶上没有库存行」。
- **`t-input-number` 的 `decimal-places` 会改显示值**：`10` 显示成 `10.0000`，作业员会以为填错
  （实测第一轮 UI 断言就红在这条）→ 去掉该 prop，靠 `validateShipmentForm` 兜。
- **列表 `SUM` 空表回 `0`（无标度）**：断言 `'0.0000'` 会挂，需 `::numeric(18,4)` 或数值比较。
- **登录限流 10 次/60s 且不自解封**：脚本改为复用同一个 `ACCESS_TOKEN`（prepare + verify 共用），
  否则连跑第三轮必 `10005`（本次踩到，重启隔离后端才清零）。
