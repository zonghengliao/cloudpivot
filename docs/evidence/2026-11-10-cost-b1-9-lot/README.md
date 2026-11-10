# COST-B1-9【成本】后端批次主数据四件套 —— 实测证据

卡：`t_28562c40`（拆卡清单 ARCH-9-3 §8.1 的 **B1-9**）
依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
§3.1 ①（`biz_lot` DDL + 4 条 CHECK + 唯一索引 `uk_biz_lot_material_no`）、
§5.1（出参口径）、§5.2「批次主数据」（#7 GET / #8 POST / #9 PUT）、
§5.3（权限码 `business:erp:lot:{list,create,update}`）、
§6.1（页面 `/business/erp/lot`，菜单种子已在 B1-7 的 V0378 落地）。

## 交付物

| 文件 | 内容 |
|---|---|
| `backend/src/business/erp/lot_dto.rs` | 契约：值域（与 V0370 的 CHECK 逐字一致）/ 列表筛选 / 建改入参（部分更新三态）/ 列表行 / 下拉选项 / 纯逻辑校验 + **唯一键冲突的友好文案**（`duplicate_message` / `map_write_error`） |
| `backend/src/business/erp/lot_model.rs` | 数据层：单头 + 2 个 join 补名（物料 / 供应商）+ 列表分页（unscoped）+ **按物料取批号**（只要启用批）+ 物料/供应商存在性 + 批号占用查询 + 建改落库 |
| `backend/src/business/erp/lot_handler.rs` | 接口层：4 个 HTTP 入口（list / options / create / update）+ 引用校验 + 重复预检 |
| `backend/src/business/erp/lot_db_tests.rs` | 真库用例 7 条（`ERP_DB_URL` 开关） |
| `backend/src/business/erp/mod.rs` | 注册 `lot_dto` / `lot_model` / `lot_handler` / `lot_db_tests` |
| `backend/src/shared/error_code.rs` | 新业务码 `ERP_LOT_NO_DUPLICATE = 66056`（进 66xxx 稳定性断言 + 段注释） |
| `docs/evidence/2026-11-10-cost-b1-9-lot/` | 本 README + 4 份实测输出 + 冒烟脚本 |

**没有新增迁移**：`biz_lot` 表、索引、CHECK 全在 B1-1 的 `V0370__cost_lot_and_cost_object.up.sql` 里
（卡面「迁移幂等 + 配套 .down.sql」对本卡是 N/A —— 本卡不改表结构）。
权限码的 `sys_menu` 承载行在 B1-7 的 `V0378__cost_menu_and_perm.up.sql`
（`business:erp:lot:list` 页面 + `lot:create` / `lot:update` 两个按钮）。

## 接口清单（真跑过的 4 条）

| # | 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|---|
| 7 | GET | `/business/erp/lot` | `business:erp:lot:list` | 分页 `{list,total,page,page_size}`；筛选 `keyword`（批号/物料编码/物料名称）、`material_id`、`source_type`、`qc_status`、`status` |
| — | GET | `/business/erp/lot/options` | `business:erp:lot:list` | **「按物料取批号」**：`material_id` 必填，只返回启用批（`status=1`），上限 200，支持 `keyword` |
| 8 | POST | `/business/erp/lot` | `business:erp:lot:create` | 建批（`lot_no`/`material_id` 必填；`source_type` 缺省 MANUAL；`qc_status` 缺省 NA；`status` 缺省 1） |
| 9 | PUT | `/business/erp/lot/{id}` | `business:erp:lot:update` | 部分更新：缺失不改 / `null` 清空 / 有值覆盖；**`material_id` 不可改**（业务键） |

## 实测输出

### ① 纯逻辑单测（不连库）—— 30 条全过

见 `01_pure_unit_tests.out`：

```
cargo test --lib lot_dto      15 passed
cargo test --lib lot_model     8 passed
cargo test --lib lot_handler   7 passed
cargo test --lib error_code   11 passed   ← 66xxx 段码值稳定性断言（含新增 66056）
```

### ② 真库用例（7/7）—— 库 = `mig_replay_costb18`

见 `02_real_db_tests.out`：

```
test create_page_update_and_options_flow ... ok                            ← C/R/U 全通 + 停用后从下拉消失
test duplicate_lot_no_on_same_material_is_friendly_409 ... ok              ← 唯一键冲突 → 409 + 66056 + 人话
test same_lot_no_allowed_on_another_material ... ok                        ← 唯一键是 (material_id, lot_no)
test update_into_existing_lot_no_is_friendly_409 ... ok                    ← 改单撞键同样 409；改回自己原批号不算冲突
test db_unique_index_still_catches_concurrent_insert ... ok                ← 绕过预检直插 ⇒ 库抛 UniqueViolation ⇒ 翻译成 409/66056
test refs_and_dates_are_rejected_with_field_names ... ok                   ← 物料/供应商不存在 400 指名；日期倒挂 400
test options_require_material_and_reject_unknown ... ok                    ← 下拉缺物料 400；物料不存在 400（不是空列表）
test result: ok. 7 passed; 0 failed
```

### ③ HTTP 冒烟（真实例 `127.0.0.1:18025`，空库首启 300+ 迁移，admin 真登录）

见 `03_http_smoke.out` / 脚本 `03_http_smoke.sh`：

```
3) POST /business/erp/lot          code=0  lot_no=COSTB19-LOT-… status=1 qc=PASSED
                                          material_name=批次冒烟物料… material_code=COSTB19-…
                                          prod=2026-11-01 expire=2027-11-01
4) GET  /business/erp/lot          code=0  total=1 page=1 page_size=20 首行=COSTB19-LOT-…
5) GET  /business/erp/lot/options  code=0  条数=1 批号=[COSTB19-LOT-…] 质检=[PASSED]
6) PUT  /business/erp/lot/{id}     code=0  lot_no=…-2 qc=REJECTED expire=None status=0
7) GET  /lot/options（停用后）      code=0  条数=0                    ← 停用批不再出现在下拉
8) POST /lot（同物料同批号）        HTTP 409 code=66056
      msg=批号「COSTB19-LOT-…-2」在物料「COSTB19-… 批次冒烟物料 …」下已存在：同一物料不能有两个相同批号。
          请改用其它批号，或到「批次管理」里编辑已有那一行（已软删的批号可以复用）
9) POST /lot（物料不存在）          HTTP 400 code=10001 msg=material_id（…）指向的物料不存在或已删除
10) 无 token：GET /lot 401 · GET /lot/options 401 · POST /lot 401 · PUT /lot/{id} 401
    GET /business/erp/lot-not-exist 404（没有被通配路由吞掉）
11) 冒烟数据已清理：残留批次数=0
```

### ④ 全量回归 —— 31 个测试目标 / 4476 passed / 0 failed

见 `04_full_test_summary.out`（无 `ERP_DB_URL` 时真库用例自动 SKIP，与本仓其它 ERP 用例同口径）。

## 关键设计点（为什么这么写）

1. **唯一键冲突分两层翻译**：写之前 `lot_no_taken` 先查（常规重复走干净 409），
   并发穿透时靠库的唯一索引抛 `DbError::UniqueViolation`，由 `lot_dto::map_write_error`
   按**变体**（不是 `Display` 文案 —— 本仓已踩过「按 duplicate key 匹配判成 500」的坑）翻译成同一条码同一条文案。
   用户永远看不到 `uk_biz_lot_material_no`。
2. **错误码单开 `66056`**：`ERP_CODE_CONFLICT`（66001）是「主数据编码重复」，
   批号是**物料级**业务键（同一批号可存在于不同物料），前端引导不同 ⇒ 不复用。
3. **`material_id` 建后只读**：它是唯一键的一半，改物料等于把档案换成另一个物料的批次，
   历史单据上的批号会指向错误物料。设计稿没逐字段写可改性，这是按「业务键不可变」的落地，已在卡内记为偏差。
4. **列表不注入数据权限**：`biz_lot` 没有部门列，§5.4 的数据权限清单里也没有它 ⇒
   `scope::paginate_unscoped(LOT_TARGET)`（与 13 张 ERP 主数据同款；门禁不允许自己拼 LIMIT/OFFSET）。
5. **下拉只要启用批**：停用的语义就是「这号别再用了」，历史单据仍保留在库里；
   列表按 `status=0` 仍能查到（前端要向用户展示「为什么这个批选不到」）。
6. **物料不存在时下拉接口报 400 而不是空列表**：空列表会让用户以为「这个料没有批」，
   而真实原因往往是物料被删了 / id 传错。

## 复跑

```bash
# 纯逻辑
cd backend && cargo test --lib lot_dto && cargo test --lib lot_model && cargo test --lib lot_handler

# 真库（需要可写的 PG；本机用 alun/alun）
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/mig_replay_costb18 \
  cargo test --lib lot_db -- --test-threads=1

# HTTP 冒烟（先起实例：NIK_ADMIN_PASSWORD 注入初始口令，否则首登随机口令 + 强制改密）
BASE=http://127.0.0.1:18025 DB=costb19_http2 bash docs/evidence/2026-11-10-cost-b1-9-lot/03_http_smoke.sh
```

## 已知偏差 / 需知会下游

| # | 项 | 说明 |
|---|---|---|
| 1 | **未实现 DELETE** | 设计 §5.2「批次主数据」只有 3 个接口、§5.3 只有 3 个权限码（无 `lot:delete`），菜单种子也无删除按钮 ⇒ 卡面「CRUD 全通」按 C/R/U 交付。软删（`deleted_at`）留给「批次作废」卡，或在增量需求里单卡开码。 |
| 2 | **`material_id` 建后不可改** | 见上「关键设计点 3」。前端编辑表单应把物料选择器置灰。 |
| 3 | **`source_doc_type` 有应用层白名单** | DDL 无 CHECK（设计如此），本模块按实际写入方收敛为 `STOCK_IN` / `COST_PERIOD` / `COST_OBJECT` / `BACKFILL` 或留空；新增写入方要先扩 `lot_dto::SOURCE_DOC_TYPES`，否则 400。 |
| 4 | **下拉接口复用 `lot:list` 权限码** | 设计只给 3 个码；为下拉单开码会让 `perm_menu_gate` 红（菜单种子的迁移已应用、不可改）。 |
| 5 | **本机遗留库** | 冒烟用 `costb19_http2`（`costb19_http` 是第一次跑时 admin 随机口令导致登录失败的废库）。两者都在本机、不进生产；`DROP DATABASE` 被安全闸拦，未能自动清理。 |
| 6 | **未新增迁移** | 卡面通用要求里的「迁移幂等 + down.sql」对本卡 N/A（不改表结构），已在上面说明。 |
