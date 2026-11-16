# M2-17 安全库存：前端设置页（批量设置 + 导入）

- 卡：`t_58a23185`（M2-17【M2·仓库】安全库存：前端设置页 —— 批量设置 + 导入）
- 上游：M2-13 表 + 单格 upsert（V0428）、M2-14 水位检查（V0429）、M2-15 补货建议、M2-16 单测
- 本卡**只做「设置页 + 批量设置 + 导入」**，不改任何水位判定口径

## 1. 交付清单

| # | 交付物 | 说明 |
|:--:|---|---|
| ① | 前端页 `/business/erp/stock-policy` | 列表（物料 × 仓库 一格一行）+ 单格新建/编辑 + 删除 + **批量设置** + **导入** |
| ② | 批量设置接口 `PUT /business/erp/stock-policy/batch` | `material_ids × warehouse_ids` 笛卡尔积，**一个事务全有或全无** |
| ③ | Excel 导入定义 `business.erp.stock-policy` | 走通用导入向导（模板 → 上传预检 → 确认入库），不自己解析 xlsx |
| ④ | 迁移 `V0431__erp_stock_policy_page` | 只种 1 个页面显形码 + 菜单页面行 + 授权 + 祖先闭包 |
| ⑤ | 页面显形码 `business:erp:stock-policy:page` | 后端零引用（登记 `permission_audit_tests::ROUTE_LEVEL_ONLY`） |

### 为什么批量设置要有后端接口（不是前端循环发 N 次 PUT）

1. **原子性**：循环发 200 次，第 137 次失败时前 136 格已写进去 —— 用户看到「设了一部分」，
   而他没有任何办法知道是哪一部分；
2. **往返**：2 000 格 = 2 000 次 HTTP + 2 000 次事务，比一条 `jsonb_to_recordset` 语句慢两个量级；
3. **回执**：一次请求能给出「新建 N 格 / 覆盖 M 格」，页面直接告诉用户发生了什么。

权限码**复用** `business:erp:stock-policy:save`（它就是「设置水位」，只是这次设置多格）——
单独发 `:batch` 码会在授权树上造出「能改一格、不能改十格」这种没人想要的组合。

### 为什么导入走 impexp 通用向导

模板生成、表头结构校验、逐格类型/长度/枚举/外键校验、文件内**组合**唯一、错误明细 xlsx、
确认入库这套能力 impexp 已经全有（物料 / 仓库 / 员工档案 / 考勤都在用）。页面再写一份 xlsx
解析就是第二套口径，而这类批量写数据的场景里，两套口径迟早会出现「页面能导的、向导导不了」。
所以页面只挂 `ImportEntryButton`（下载模板 + 字段说明 + 进向导），`biz-type` 与后端常量逐字一致。

## 2. 改动文件

后端：

```
backend/app/src/business/erp/stock_policy/dto.rs            +批量 DTO / checked_write 抽取 / 6 条纯逻辑用例
backend/app/src/business/erp/stock_policy/model.rs          +missing_materials / missing_warehouses /
                                                            existing_pair_count / policy_upsert_batch（一条 SQL）
backend/app/src/business/erp/stock_policy/handler.rs        +save_policy_batch_of + PUT …/batch + 5 条真库用例
backend/app/src/business/erp/stock_policy/policy_db_tests.rs +5 条真库用例（含导入定义 persist）
backend/app/src/business/erp/stock_policy/mod.rs            模块地图 / 导出
backend/app/src/business/impexp/builtin/erp_setting.rs     新增：StockPolicyImport（6 列，组合唯一）+ 5 条用例
backend/app/src/business/impexp/builtin/mod.rs              注册（register_import）
backend/app/src/system/menu/permission_audit_tests.rs       页面码登记进 ROUTE_LEVEL_ONLY
backend/app/migrations/business/V0431__erp_stock_policy_page.{up,down}.sql  新增
```

前端：

```
frontend/src/api/business/erp/stock-policy.ts                      新增（四个面）
frontend/src/views/business/erp/stock-policy/stock-policy-logic.ts  新增（纯逻辑 + 可单测）
frontend/src/views/business/erp/stock-policy/index.vue              新增（页面）
frontend/src/router/index.ts                                        注册路由（页面显形码）
frontend/tests/unit/m217-stock-policy-page.test.ts                  新增 41 条用例
```

## 3. 口径决策（改代码前先看这里）

| 决策 | 值 | 理由 |
|---|---|---|
| 批量上限 | `MAX_BATCH_CELLS = 2000`（前后端同值，单测对拍） | 全客户「上百物料 × 十来个仓库」也就上千格；再大该走导入（单文件 20 000 行） |
| 批量语义 | 按格 **upsert**（覆盖），不叠加、不做差量 | 与单格 `PUT` 同一动作铺开；四列留空 = 覆盖成 NULL（不是「保留旧值」） |
| 入参重复 | 去重并计数（回执里 `duplicate_materials/warehouses`） | 同一格传两次会撞 `uk_biz_msp_mat_wh`，整批失败 → 页面与服务端都先去掉 |
| 非法 UUID / 空串 | 400 且点名**第几项**（`material_ids[3]`） | 静默跳过 = 用户以为设了 10 格，实际 9 格且没人告诉他 |
| 物料 / 仓库不存在 | 404 + 整批回滚（文案列前 5 个 id） | 水位是设置，不顺手建主数据 |
| 导入幂等键 | `(material_id, warehouse_id) WHERE deleted_at IS NULL` | 与表的部分唯一索引同口径；软删过的格「重导 = 新建一行」 |
| 导入跨列规则 | `max_qty > min_qty`（逐行判，带 Excel 行号） | 库层 `ck_biz_msp_max` 只报「约束被违反」，用户看不出是哪一行、哪个框 |
| 编辑时能否换格 | **不能**（物料 / 仓库下拉禁用） | 键是 `WHERE` 条件：换格 = 删一格 + 设另一格，混成一个动作会让旧格悄悄回退物料级 |
| 页面码 vs 接口码 | 页面码 `…:page` 只判前端；四个接口码各自判 | AGENTS.md 约定 14 / PERM-01；V0428/V0429 已把接口码形状钉死 |

## 4. 实测（2026-11-17，99 开发机 · 库在 145:5432）

### 4.1 空库/模板库首启 + 迁移

```bash
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m217_drill
NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18217 \
NIK_DATABASE_HOST=172.18.10.145 NIK_DATABASE_NAME=cp_m217_drill ... \
  ./target/debug/cloudpivot profile=test
#   schema_migrations 366 条（含 431），服务自检通过
```

V0431 落地实测：

```sql
select auth_code, path, component, sort_order, icon, title, is_visible
  from sys_menu where auth_code like 'business:erp:stock-policy:%' order by menu_type;
-- business:erp:stock-policy:page | /business/erp/stock-policy | business/erp/stock-policy/index | 47 | setting | 安全库存 | t
-- list/save/delete/check 四条仍是 menu_type=3、path NULL（V0428/V0429 形状未被动）
-- 授权 5 个角色（模板库里持有任一水位码的角色）
```

**幂等**：把 `V0431…up.sql` 用 psql 连跑 2 次，两次自检均通过且计数不变（5 个节点 / 5 条授权）。

### 4.2 真实 HTTP 冒烟（`NIK_SERVER_LISTEN=127.0.0.1:18217`，`admin` 登录）

| # | 请求 | 结果 |
|:--:|---|---|
| ① | `GET /api/business/erp/stock-policy/page` | `{"code":0,...,"total":0}` |
| ② | `PUT /api/business/erp/stock-policy`（甲×甲仓，min 100 / max 500） | `code:0`，回读 `min_qty:"100.0000"`、`max_qty:"500.0000"`、`remark:"冒烟单格"` |
| ③ | `PUT /api/business/erp/stock-policy/batch`（甲、乙 × 甲仓，min 250） | `{"cells":2,"created":1,"updated":1,...,"message":"已设置 2 格水位：新建 1 格，覆盖 1 格"}` |
| ④ | `GET …/page?keyword=M217` | `total:2`，两行 `min_qty:"250.0000"`、`max_qty:null`、`remark:"冒烟批量"`（留空覆盖成 NULL ✓） |
| ⑤ | `PUT …/batch`（2001 格） | `400` `"一次最多批量设置 2000 格（物料 2001 × 仓库 1 = 2001 格）：请减少物料 / 仓库，或改用「导入」按 Excel 逐行设置"` |
| ⑥ | `GET /api/business/impexp/import/template?biz_type=business.erp.stock-policy` | `HTTP 200`，`application/vnd.openxmlformats-…sheet`，10 784 字节，`file` 识别为 Excel 2007+（**定义已注册**） |
| ⑦ | `GET /api/business/impexp/def/columns?biz_type=…` | 6 列：物料编码*(必填) / 仓库编码*(必填) / 最低水位*(必填) / 最高水位 / 状态 / 备注 |
| ⑧ | `DELETE /api/business/erp/stock-policy/{id}` | `code:0`；存活行数 2 → 1（软删 = 该格回退物料级） |

夹具（单位 / 2 物料 / 1 仓库）用完即删，冒烟脚本留在 `~/.hermes/profiles/backend-developer/cache/scratch/m217_http_smoke.sh`。

### 4.3 测试与门禁

```bash
ERP_DB_URL=postgres://…@172.18.10.145:5432/cp_m217_drill \
  cargo test --lib business::erp::stock_policy -- --test-threads=1
#   ok. 71 passed; 0 failed        （含本卡新增 9 条：4 纯逻辑 + 5 真库）

ERP_DB_URL=… cargo test --lib business::impexp -- --test-threads=1
#   ok. 226 passed; 0 failed       （含 erp_setting 5 条）

ERP_DB_URL=… cargo test --lib system::menu::permission_audit_tests  # ok. 8 passed（N14/N18 页面码台账）
ERP_DB_URL=… cargo test --lib system::menu::tests                   # ok. 14 passed（N08 路由↔菜单 / N09 图标 / N10 注解码→菜单）

PGHOST=172.18.10.145 … bash scripts/verify/menu_invariants.sh cp_m217_drill
#   ① 孤儿 0 ② /system/ 0 ③ /business/ 0 ④ path 唯一 0 ⑤ 总数 526（400-540） ⑥ title 0 ⑦ 同父 sort 0 ⑧ icon 0 → 全绿

cd backend && cargo fmt && cargo clippy --lib -- -D warnings        # clean
cd backend && cargo clippy --all-targets -- -D warnings             # clean

cd frontend && pnpm typecheck                                        # clean（vue-tsc --noEmit）
cd frontend && node ./node_modules/tsx/dist/cli.mjs --test tests/unit/m217-stock-policy-page.test.ts
#   41 tests / 41 pass
cd frontend && vite build                                           # 成功（dist 494 assets）
```

### 4.4 新增用例清单（14 条后端 + 41 条前端）

后端纯逻辑（`dto::tests`）：批量笛卡尔积与去重 / 非法列表点名字段 / 2000 格上限 / 四列校验复用 /
回执文案。真库（`policy_db_tests`）：4 格新建→再批全覆盖（created/updated 分开计数、留空覆盖成 NULL）/
物料或仓库不存在整批回滚 / 入参去重 + 软删格重设算新建 / 超上限碰库前拦住 / 导入定义 persist
（编码→id JOIN、max 空落 NULL、重导覆盖 id 不变）。

前端（`m217-stock-policy-page.test.ts`）：拼参空筛选整键缺席 / 四列校验刀口 / 批量展开与上限 /
提交载荷去重 / 格式化（`null` → `—`、`0` → 「0（不预警）」）/ **源码即契约**（`/api` 前缀、
`biz-type` 与后端常量一致、路由 path 与迁移 `sys_menu.path` 一致、页面不自己解析 xlsx、
编辑时键不可改、按码置灰按钮）。

## 5. 未做 / 边界

- **HTTP 端的「上传 → 预检 → 确认入库」整链未做**：本卡用真库用例直接调导入定义的 `persist`
  （框架在事务里调的就是它）+ HTTP 验证模板与字段说明可用。整链归 M2-18 收口 E2E。
- 冒烟用的 `admin` 口令是模板库里既有 hash（本机无 argon2 工具，未改库内口令）；
  生产不受影响，演练库 `cp_m217_drill` 已 drop。
- 页面未挂「水位检查」（`POST …/check`）按钮：那是运维 / 定时动作（M2-14 已有小时级任务）。
