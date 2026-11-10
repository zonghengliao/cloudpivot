# COST-B1-12 前端批次管理页 + 领料单「成本对象」选择器

任务卡：`t_3d4dd487`（ARCH-9 §8.1 B1-12）。前置：B1-9（批次四件套）、B1-10（领料挂对象）、B1-11（成本对象页）。

## 一、本卡交付

| # | 内容 | 落点 |
|---|---|---|
| 1 | **批次管理页**（列表分页 + 关键字 / 物料 / 来源 / 质检 / 状态筛选、新建、编辑、停用切换） | `frontend/src/views/business/erp/lot/index.vue` + `lot-logic.ts` |
| 2 | 批次 API 层（page / options / create / update，**无 delete**） | `frontend/src/api/business/erp/lot.ts` + `lot-kinds.ts` |
| 3 | 路由注册 `business/erp/lot`（菜单种子 V0378 `…c006` 指过来，漏注册 = 点菜单 404） | `frontend/src/router/index.ts` |
| 4 | **领料单（出库单）表单加「成本对象」下拉**，**只列 OPEN**，选完借方科目提示实时变 | `frontend/src/views/business/erp/stock-out/{index.vue,stock-out-logic.ts}` |
| 5 | 出库单详情显示「成本对象 / 过账借方」，过账确认框借方取后端下发值 | 同上 |
| 6 | **新增后端下拉接口** `GET /business/erp/stock-out/cost-object-options`（只 OPEN，权限 `stock-out:list`） | `stockout_{dto,model,handler}.rs` |

## 二、验收对照（卡面三条）

| 卡面验收 | 证据 | 结果 |
|---|---|---|
| 批次列表能按物料筛 | `01_lot_filter_by_material.png`（共 2 条，只剩 B112-M1 的两条批）+ `09_http_smoke.out` §3 | ✅ |
| 出库单表单加「成本对象」下拉（只列 OPEN 的） | `03_picker_only_open.png`（下拉只有 `CB-B112-000001`，CLOSED 的 `CB-B112-000002` 不出现）+ `09_http_smoke.out` §4/§5 | ✅ |
| 选完后借方科目提示会变 | `02`（6401 主营业务成本）→ `04`（4001.01 生产成本-直接材料）；切「报废」→ `05`（1901，且选择器消失） | ✅ |

补充实证：
- `07_ui_create_lot.png`：UI 新建批次成功（列表出现 `LOT-B112-UI`，提示「批次已创建」）。
- `08_ui_duplicate_409_66056.png`：重复批号 → 红条原样显示「批号『LOT-B112-A』在物料『B112-M1 B112 钢板』下已存在…（错误码 66056）」，不含 PG 约束名。
- `06_detail_cost_object_and_debit.png`：出库单详情显示「成本对象 CB-B112-000001 / 过账借方 4001.01 生产成本-直接材料」。
- `09_http_smoke.out` §6/§7：建单挂对象 ⇒ 借方 4001.01、不挂 ⇒ 6401；挂已关批对象 ⇒ 409 + 66057；§8 无 token ⇒ 401。

## 三、为什么新增 `/stock-out/cost-object-options` 接口（不是越权改别的模块）

领料单由**仓管**（`WAREHOUSE_KEEPER` 等）开具，V0378 的授权矩阵把 `business:erp:cost-object:list`
**只授给财务角色**。若下拉直接查成本对象页的 `GET /cost-object?status=OPEN`，仓管一开单就 403
→ 下拉永远空（"只列 OPEN" 的口径根本走不到）。所以本卡在**出库模块内**加一条最小投影查询：

- 权限码复用 `business:erp:stock-out:list`（已种在 `sys_menu`，`perm_menu_gate` 不会红；
  若为此单开新码，迁移已应用不可改 ⇒ 闸⑥ 必红）；
- SQL：`biz_cost_object WHERE status='OPEN' AND deleted_at IS NULL` + `biz_material` join 取产品名，
  上限 200（与 `lot_dto::OPTIONS_LIMIT` 同口径），关键字命中对象号 / 产品编码 / 产品名称；
- 非 OPEN 对象不出现：挂了会被 `ensure_cost_object_usable` 409+66057 拦下，列出来只是让用户白点。

## 四、实测输出

### 后端

```
cargo check --all-targets                      EXIT=0（1m52s）
cargo test --lib                               4224 passed / 0 failed（EXIT=0）
cargo test --lib -- cost_object_tests (真库)    8 passed / 0 failed（含新增 ⑦⑧ 两条下拉用例）
cargo test --lib -- stockout_dto stockout_model::option_tests   14 passed
cargo fmt                                      已执行
cargo clippy --all-targets -- -D warnings       见 10_clippy.out（EXIT=0）
```

真库用 `mig_replay_b110cost`（已迁移到 V0381）；新增用例：
- `options_list_only_open_objects_with_product_name`：只 OPEN + 产品名 join + 计划量/开工日；
- `options_filter_by_keyword_on_object_no_and_product`：关键字三列命中，**关批对象号搜出来是空**。

### 前端

```
pnpm typecheck     EXIT=0
pnpm build         EXIT=0
pnpm test:unit     1374 pass / 0 fail（新增 erp-lot 15 条 + 出库单 4 条成本对象用例）
```

### 端到端（真实例 18032 + vite 5199 + 真浏览器）

`09_http_smoke.out` 全文，覆盖：登录 → 批次列表（3 条）→ 按物料筛（2 条）→ 成本对象下拉（只 1 条 OPEN）
→ 关键字三命中 → 建单借方 4001.01 / 6401 对照 → 关批对象 66057 → 无 token 401。

## 五、已知偏差

1. **没有删除按钮**：设计 §5.3 只给 `lot:{list,create,update}` 三个码，后端无 DELETE 路由；
   挂一个必 404 的按钮比没有更糟（批次作废属下游卡）。
2. **物料建单后只读**：`(material_id, lot_no)` 是业务键，后端 `LotUpdateReq` 里没有 `material_id` 字段。
3. **供应商 / 来源单据字段未进表单**：后端支持（`supplier_id` / `source_doc_type` / `source_doc_no`），
   但本卡验收不涉及；页面只展示来源类型。留给采购 / 入库联动的卡。
4. **批次下拉（`/lot/options`）本卡未接入出库明细的 `lot_no`**：现有明细 `lot_no` 是自由文本，
   接成「按物料取批号」的联动属 B2 领料体验卡，本卡范围外。
5. **本卡无迁移**：`biz_lot` 表（V0370）、成本对象（V0370）、列 `cost_object_id`（V0375）、
   科目 `4001.01`（V0377）、菜单权限（V0378）全部前置卡已落地 ⇒ 卡面「迁移幂等 + down.sql」N/A。
6. **环境残留**（本机，不影响生产）：e2e 库 `b112e2e`（首启未注入 admin 口令，admin 口令随机不可登录的废库）
   与 `b112e2e2`（本卡验收用库）。`DROP DATABASE` 被无人值守安全闸拦，未自动清理。
7. **借方科目预览是前端镜像**：表单里未保存时的「过账借方」由 `debitAccountHint` 复算（与后端
   `stockout_base::debit_account_of_doc` 同一张表，4 条单测钉住）；保存后一律显示后端下发的
   `debit_account_code/name`（详情 / 过账确认框都是）。

## 六、复现命令

```bash
# 后端真库用例（需已迁移到 V0381 的库）
ERP_DB_URL="postgres://alun:***@127.0.0.1:5432/mig_replay_b110cost" \
  cargo test --lib -- cost_object_tests -- --test-threads=1

# 前端
cd frontend && pnpm test:unit && pnpm typecheck && pnpm build

# 端到端（真实例 + 浏览器）
cd backend && NIK_DATABASE_NAME=b112e2e2 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_SERVER_LISTEN=127.0.0.1:18032 NIK_ADMIN_PASSWORD='Admin@123456' \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) X_CONFIG_SECRET_KEY=$(openssl rand -hex 24) \
  ./target/debug/cloudpivot
# 夹具见 /tmp/b112_fixtures.sql（同内容已复制到本目录 b112_fixtures.sql）
# BASE=http://127.0.0.1:18032 DB=b112e2e2 bash b112_http_smoke.sh
# cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18032 pnpm dev --port 5199
```
