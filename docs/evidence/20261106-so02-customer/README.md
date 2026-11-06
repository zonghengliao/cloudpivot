# SO-02 客户档案后端 API（t_1cc778e9）· 验收证据

日期：2026-11-06 · 机器：`172.18.10.99:/opt/cloudpivot`（工作仓）
隔离库：`cloudpivot_v0260`（SO-01 建，已被本卡应用 V0261）· 实例端口 `127.0.0.1:18097`

## 0. 交付物

| 类型 | 文件 |
|------|------|
| 数据层 | `backend/src/business/erp/customer_model.rs`（行结构 / 查询 / 写入 / 纯逻辑单测） |
| 接口层 | `backend/src/business/erp/customer_handler.rs`（5 个 HTTP 入口 + 业务函数 + 真库单测） |
| 注册 | `backend/src/business/erp/mod.rs`（`pub mod customer_handler; pub mod customer_model;`） |
| 迁移 | `backend/migrations/business/V0261__sales_customer_permissions.{up,down}.sql` |
| 验收脚本 | `scripts/verify/so02_customer_api_e2e.sh`（真 token curl 24 断言）、`scripts/verify/so02_start.sh`（隔离实例） |

接口（`/api` 前缀由 `[router].prefix` 注入）：

```
POST   /api/business/erp/customer           business:erp:customer:create
PUT    /api/business/erp/customer/{id}      business:erp:customer:update
DELETE /api/business/erp/customer/{id}      business:erp:customer:delete   软删
GET    /api/business/erp/customer/{id}      business:erp:customer:list
GET    /api/business/erp/customer/page      business:erp:customer:list      code/name 模糊 + status 精确
```

## 1. cargo test（连真库，连跑两轮）

```
$ cd backend && ERP_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_v0260' \
    cargo test --lib business::erp::customer
running 11 tests
test business::erp::customer_handler::tests::create_customer_persists_all_fields ... ok
test business::erp::customer_handler::tests::duplicate_code_rejected_and_reusable_after_soft_delete ... ok
test business::erp::customer_handler::tests::negative_credit_limit_rejected_zero_allowed ... ok
test business::erp::customer_handler::tests::page_filters_by_code_name_and_status ... ok
test business::erp::customer_handler::tests::soft_delete_hides_row_but_keeps_it ... ok
test business::erp::customer_handler::tests::update_is_partial_and_checks_code_uniqueness ... ok
test business::erp::customer_handler::tests::permission_codes_are_unique_and_prefixed ... ok
test business::erp::customer_model::tests::status_domain_matches_check_constraint ... ok
test business::erp::customer_model::tests::normalize_code_only_trims ... ok
test business::erp::customer_model::tests::credit_limit_guard_boundary ... ok
test business::erp::customer_model::tests::columns_cover_every_v0260_field ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 2681 filtered out; finished in 2.74s
```

两轮一致（第二轮的日志：`cargo test --lib business::erp::customer` → 同样 11 passed / 0 failed）。
`ERP_DB_URL` **设了**，所以真库断言真的执行了（不是 SKIP 假绿：`ensure_db` 返回 false 时会打印
`SKIP: 未设置 ERP_DB_URL`，日志里没有这一行）。

## 2. 编译门禁

```
$ cargo check                    → Finished dev profile（0 error 0 warning）
$ cargo clippy --all-targets -- -D warnings → Finished（含测试目标，零告警）
$ cargo fmt -- --check           → FMT_OK（无 diff）
```

## 3. 迁移 V0261（引擎真实应用，不是手工 psql）

```
2026-11-06T01:20:13.928407Z  INFO nik_web::app: 数据库迁移完成: 应用 1 个迁移

$ select max(version::int), count(*) from schema_migrations;
 261 | 203

-[ RECORD 1 ]-----------------------------------------------
kind        | versioned
name        | sales_customer_permissions
version     | 261
applied_via | executed
success     | t
source      | migrations/business/V0261__sales_customer_permissions.up.sql
ck_len      | 64

db  = a623a42aef61bbada596d8abf55206ecb5a8f46625a8570158e340fa00e1711e
file= a623a42aef61bbada596d8abf55206ecb5a8f46625a8570158e340fa00e1711e
MATCH ✓
```

回滚脚本在事务里试跑（`begin` → down.sql → 断言 → `rollback`）：

```
DELETE 3      -- 3 个按钮
DELETE 1      -- 页面节点
 left_in_tx | 0
ROLLBACK
回滚后实际库：4 行仍在（ROLLBACK 生效）
```

菜单结构（4 个码全部有承载，页面隐藏、`path` 留空待 SO-03）：

```
20000000-…-000000002601 | 客户档案                      | 2 | business:erp:customer:list   | path=NULL | is_visible=f
20000000-…-000000002602 | business:erp:customer:create  | 3 | business:erp:customer:create |           | f
20000000-…-000000002603 | business:erp:customer:update  | 3 | business:erp:customer:update |           | f
20000000-…-000000002604 | business:erp:customer:delete  | 3 | business:erp:customer:delete |           | f
```

> `path`/`component` 故意留 NULL：`system::menu::tests::n08`（真库档）要求
> menu_type=2 的 path 能在 `frontend/src/router/index.ts` 解析，前端页属 SO-03。
> 本卡实测过「先填上」的后果（n08 当场红：`客户档案 (/business/erp/customer) 前端未注册路由`），
> 故改为「页面节点就位 + 隐藏 + 待补 path」，一条 UPDATE 收尾（写法见迁移文件头）。

## 4. 真 token curl 五个接口（连跑两轮，各 24 PASS / 0 FAIL）

`bash scripts/verify/so02_customer_api_e2e.sh`（`BASE=http://127.0.0.1:18097`）：

| 轮次 | 结果 | 日志 |
|------|------|------|
| 第 1 轮 | `PASS=24 FAIL=0`（rc=0） | `round-1.txt` / `e2e-round-1.log` |
| 第 2 轮 | `PASS=24 FAIL=0`（rc=0） | `round-2.txt` / `e2e-round-2.log` |

### 五个接口各一次的实测报文（第 2 轮原文）

```jsonc
// ① POST /api/business/erp/customer  → 200
{"code":0,"msg":"ok","data":{"id":"01a1ae71-6ffc-75bd-b4d5-eab9b70b21d4","code":"SO2E2E-092032",
 "name":"e2e 客户甲","tax_no":"91110108E2E0001XY","contact_name":"张伟","contact_phone":"010-88886666",
 "address":"北京市海淀区 1 号","credit_limit":"123456.7890","status":"ACTIVE","remark":"SO-02 e2e",
 "created_at":"2026-11-06T01:20:32.252856Z","updated_at":"2026-11-06T01:20:32.252856Z"}}

// ② GET /api/business/erp/customer/{id} → 200
{"code":0,"msg":"ok","data":{"id":"01a1ae71-6ffc-75bd-b4d5-eab9b70b21d4","code":"SO2E2E-092032", ... }}

// ③ PUT /api/business/erp/customer/{id} → 200（部分更新：只改名/额度/状态，code 未传不变）
{"code":0,"msg":"ok","data":{"id":"01a1ae71-6ffc-75bd-b4d5-eab9b70b21d4","code":"SO2E2E-092032",
 "name":"e2e 客户甲（改名）","credit_limit":"200000.5000","status":"FROZEN", ... }}

// ④ GET /api/business/erp/customer/page?code=SO2E2E-092032&status=FROZEN → 200
{"code":0,"msg":"ok","data":{"list":[{"id":"01a1ae71-6ffc-75bd-b4d5-eab9b70b21d4",
 "code":"SO2E2E-092032","name":"e2e 客户甲（改名）","credit_limit":"200000.5000","status":"FROZEN", ... }],
 "total":1,"page":1,"page_size":10}}

// ⑤ DELETE /api/business/erp/customer/{id} → 200
{"code":0,"msg":"ok","data":null}
// 软删后 ⑥ GET 同一 id → 404 {"code":10002,"msg":"客户不存在或已删除"}
// 软删后 ⑦ page?code=… → total=0
// 软删后 ⑧ POST 同编码 → 200（部分唯一索引 uk_biz_customer_code 释放编码，实测复用成功）
```

### 负向断言（同一轮内）

```
POST 负额度      → 400 · {"code":10001,"msg":"credit_limit 不能为负数（当前 -1）；0 表示不控制额度"}
POST 重复编码    → 409 · {"code":66001,"msg":"客户编码 `SO2E2E-092032` 已存在（已软删的编码可以复用）"}
GET  非法 id     → 400（parse_uuid → 66006，单测覆盖）
PUT  改成别人的 code → 409 · 66001（单测 `update_is_partial_and_checks_code_uniqueness`）
```

### 软删语义的库侧证据

```
code           | status | deleted
SO2E2E-092032  | FROZEN | t      ← 原行：软删，物理行仍在（biz_customer_contact 的 ON DELETE RESTRICT 才挡得住硬删）
SO2E2E-092032  | ACTIVE | t      ← 复用同编码重建的行，也被本轮收尾软删
```

（`SO2E2E-0918xx` 是前几轮调脚本时的残留，同样全是 `deleted=t`。单测夹具前缀 `SO2T-` 的行由
`cleanup()` 物理删除，复查为 0 行。）

## 5. 仓内常驻闸（连隔离库实测）

```
$ NIK_DATABASE_*=cloudpivot_v0260 python3 scripts/verify/perm_menu_gate.py
代码侧：382 个非测试 .rs → 注解码 282 个（506 处）
菜单侧：294 个活跃 auth_code（db=cloudpivot_v0260）
✓ missing_in_menu = 0   （代码在用的码全部有菜单承载，可被授予）
==> 闸：PASS

$ NIK_DATABASE_*=cloudpivot_v0260 python3 scripts/verify/orphan_button_gate.py
扫描面：活跃 sys_menu 339 行 / 带码父页的按钮节点 245 个 / 按钮授权 431 行
✓ 孤儿 (角色,码) = 0

$ SYS_DB_URL=…cargo test --lib system::menu
test result: ok. 20 passed; 0 failed   ← 含 n08（菜单路径能在前端路由解析）/ n10（注解码有菜单行）/
                                           n16（每条路由有权限闸）/ n18（异常台账）
```

## 6. 与卡面/其它卡的已知关系

- **不改 `biz_customer_contact`**：联系人子表本卡只读不写（卡面只批主档 CRUD），
  后续卡再给它接口；`ON DELETE RESTRICT` 是本卡坚持软删的硬理由之一。
- **不写凭证 / 库存三本账 / 流程引擎**：客户档案是主数据，不是业务单据，
  没有 `LEGAL_EDGES` 状态机、不走 `biz_voucher`、不碰 `inventory_writer`。
  （卡面「必须遵守 1」在本卡的实际落点是：**没有**任何直写三本账的 SQL —— 见 §0 文件清单。）
- **`status = 'ACTIVE'` 字符串域**：`contract_model.rs:31` 注释里的 `status = 1` 是旧占位，
  客户分支必须写 `'ACTIVE'`（V0260 已定稿 `VARCHAR` 域值，本卡 `STATUS_ACTIVE` 常量钉住）。
- **SO-03 交接**：V0261 的页面节点 `path`/`component` 留 NULL、`is_visible=false`；
  前端页落地后一条 UPDATE 补全（语句见迁移文件头）。这样 `n08` 在 SO-03 之前不会红，
  按钮也已经挂在正确的父页下（不需要 SO-03 再做 V0259 式的 reparent）。
