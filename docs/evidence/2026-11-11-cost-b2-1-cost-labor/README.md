# COST-B2-1【成本】工时台账后端 —— 实测证据

卡：`t_2e11875e`（拆卡清单 ARCH-9-3 §8.1 的 **B2-1**）
依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
§3.1 ④（`biz_cost_labor` DDL + 6 条 CHECK）/ §4.5.3（8 列录入口径）/ §4.5.4（费率来源）/
§5.1（出参口径）/ §5.2 #10–#14（5 个接口）/ §5.3（5 个权限码）/ D-18（数据权限）

## 交付物（四件套 + 三处登记）

| 文件 | 内容 |
|---|---|
| `backend/src/business/erp/cost_labor_dto.rs` | 契约：列表筛选 / 建改入参（三态部分更新）/ 出参 / **金额算法 `expected_amount`** / `66058` 错误映射 / 确认幂等判定 `confirm_action` |
| `backend/src/business/erp/cost_labor_model.rs` | 数据层：单头 + 4 个 join 补名 + 分页（`{FILTER}` + `scope::paginate`）+ 建改删确认（**金额由 SQL 的 `round(hours*rate,4)` 求值**）+ 成本对象 / 部门引用查询 |
| `backend/src/business/erp/cost_labor_handler.rs` | 接口层：5 个接口（list / create / update / delete / confirm）+ 引用校验 + 确认幂等编排 |
| `backend/src/business/erp/cost_labor_db_tests.rs` | 真库用例 10 条（`ERP_DB_URL` 开关） |
| `backend/src/business/shared/scope.rs` | `ScopeTarget::BIZ_COST_LABOR`（`dept_col = dept_id`）登记进 `SCOPED_TABLES`（设计 D-18） |
| `backend/src/business/permission/scope_model.rs` | `TABLE_WIRING` 补 `biz_cost_labor`（D8 覆盖率闸要求行数 = 登记表数） |
| `backend/src/business/shared/guard.rs` | 启动自检清单计数 13 → 14 |
| `backend/src/shared/error_code.rs` | 新业务码 `ERP_COST_LABOR_AMOUNT_MISMATCH = 66058`（进 66xxx 稳定性断言） |

## 实测输出

### ⓿ HTTP 冒烟（真实例 `127.0.0.1:18041`，空库首启 + admin 真登录，**19 PASS / 0 FAIL**）

见 `04_http_smoke.out` / `04_http_smoke.sh`（可复跑：空库 + 实例起来后直接 `bash` 本脚本）。

```
1)  登录 admin                                  token 长度 461
1b) PUT /system/auth/password（首登必须改初始口令）HTTP 200 code=0
3)  POST /business/erp/cost-labor               code=0 status=DRAFT source_type=MANUAL
                                                hours=8.00 machine_hours=2.00 rate=50.000000
                                                amount=400.0000  ← 金额服务端算（客户端不填）
                                                cost_object_no=CB-20261111-000001
                                                product_name=成品E（种子） dept_name=总部
4)  GET  /business/erp/cost-labor?period_code=2026-11&cost_object_id=…  code=0 total=1 page=1 page_size=10
5)  PUT  /business/erp/cost-labor/{id}  {"hours":"9.5"}  code=0 hours=9.50 amount=475.0000 ← 金额重算
6)  POST /business/erp/cost-labor/{id}/confirm   code=0 status=CONFIRMED amount=475.0000
7)  重复确认                                     code=0 status=CONFIRMED amount=475.0000（不翻倍）
                                                updated_at 与第一次**完全相同**（没进写路径）
                                                列表 total=1（仍一行）
8)  已确认后 PUT / DELETE                        HTTP 409 code=10003「只有草稿（DRAFT）状态的工时才能修改/删除」
9)  POST 带 amount=401（8×50 应为 400）          HTTP 400 code=66058
                                                msg=工时×费率与金额不符：8 小时 × 50 元/小时 = 400 元…但金额是 401 元
10) POST 跨月（period=2026-11，work_date=2026-12-01）HTTP 400 code=10001 work_date 不在所选期间内…
11) DELETE 草稿行                                HTTP 200 code=0（软删，列表回到 1 行）
12) 关批对象后 POST 工时                         HTTP 409 code=66057「…当前是「已关批」（CLOSED），不能录工时…」
13) 未鉴权 GET / 未知路径                        HTTP 401 / 404（路由确实挂在 router 上）
14) 清场                                         残留行数=0
```

### ① 真库用例（10/10，库 = `mig_replay_costb18`，空库全链重放后的库）

见 `01_real_db_tests.out`：

```
test confirm_twice_does_not_double ... ok                      ← 验收项：确认幂等（不翻倍）
test db_check_maps_amount_mismatch_to_66058 ... ok             ← 验收项：金额 CHECK → 「工时×费率与金额不符」
test crud_confirm_flow_end_to_end ... ok                       ← C/R/U/确认全通 + *_id 都带 *_name
test declared_amount_mismatch_is_rejected_at_entry ... ok      ← 入口核对值不符 → 400/66058（不落库）
test list_is_scoped_by_dept ... ok                             ← 数据权限：别的车间看不见（D-18）
test object_must_exist_and_be_open ... ok                      ← 不存在 400/66002；已关批 409/66057
test period_and_work_date_must_agree ... ok                    ← 日期不在所选期间 → 400
test confirmed_row_is_readonly ... ok                          ← 已确认 改/删 → 409
test repeating_rate_rounds_to_four_decimals ... ok             ← 真实工资率 33.333333 → 33.3333（4 位）
test soft_deleted_draft_disappears_from_list ... ok            ← 软删后列表查不到
test result: ok. 10 passed; 0 failed
```

### ② 纯逻辑单测（不连库）

见 `02_pure_unit_tests.out`：`cargo test --lib cost_labor` → **47 passed / 0 failed**。

其中直接对应卡面验收项的三条：

| 用例 | 验收项 |
|---|---|
| `cost_labor_dto::tests::declared_amount_mismatch_is_66058` | 金额不符 → 400 + 66058 + 「工时×费率与金额不符」 |
| `cost_labor_dto::tests::amount_check_violation_maps_to_400_66058` | 库 `ck_biz_cost_labor_amount` 的 `CheckViolation` 翻译成同一条码同一条文案 |
| `cost_labor_dto::tests::confirm_action_is_idempotent` + `cost_labor_handler::tests::confirm_on_confirmed_row_never_writes` + `confirm_sql_only_flips_status` | 确认幂等（判定层 / 写路径层 / SQL 层三层） |

其余关键断言：金额算法真值（含 `1 × 33.333333 = 33.3333` 与 .5 边界 `0.00005 → 0.0001`）、
精度超限**不静默舍入**、工时 > 0、费率缺失**报错并指向 B2-3**、`{FILTER}` 标记位置、
金额一律由 SQL 求值（不许直接绑定）、改/删/确认三条 SQL 都带 `status='DRAFT'` 闸。

### ③ 全量 `cargo test` + clippy + fmt

```
cargo test                          31 个测试目标全 ok（4541 passed / 0 failed，EXIT=0）
cargo clippy --all-targets -D warnings   0 warning / 0 error
cargo fmt --check                    干净
```

（原始输出见 `05_full_test_summary.out`）

### ④ 被本卡改到的三处公共登记（回归）

见 `03_touched_guards.out`：

```
cargo test --lib scope        → 93 passed（SCOPED_TABLES 14 张自洽 / 全部 scoped）
cargo test --lib guard        → 146 passed（启动自检清单计数 14 = 原 13 + 工时台账）
cargo test --lib error_code   → 11 passed（66xxx 段码值稳定 + 66058 不重复）
cargo fmt -- --check          → clean
```

## 实现要点（为什么这么做）

1. **金额由服务端算、且由 SQL 算**。`amount` 列有 `CHECK (amount = round(hours*rate, 4))`。
   若让前端传金额，就变成两道口径赛跑；若由 Rust 算好再传，则 `.5` 边界上
   「rust_decimal 舍入 vs PG numeric 舍入」可能差一位 → 行插不进去。
   所以写库时 `amount = round(?::numeric * ?::numeric, 4)`（与 CHECK **同引擎**），
   Rust 的 `expected_amount` 只用于「客户端核对值」与单测期望（舍入策略同为
   `MidpointAwayFromZero`，与 PG numeric 的半数远离零一致）。
2. **工时 / 费率的超精度拒绝，不静默舍入**。列是 `NUMERIC(10,2)` / `NUMERIC(18,6)`：
   用户填 `hours=8.555` 时库会把**列值**舍成 8.56 再参与 CHECK，金额对得上但工时是错的，
   而且没人会发现。所以 `>2` / `>6` 位小数直接 400 指名（设计 §4.5.2「不猜」）。
3. **确认只翻状态、不写金额、不做累加**，并叠两道幂等：判定层（`confirm_action`
   对 `CONFIRMED` 直接返回、不进写路径 ⇒ `updated_at` 都不动）+ SQL 层
   （`WHERE ... AND status='DRAFT'`，并发下第二条命中 0 行、再读一次照样返回成功）。
4. **确认复用 `cost-labor:update` 权限码**（设计 §5.2 #14 明文），不新开码 ——
   已应用的菜单种子（V0378）里没有承载行，新开码会让 `perm_menu_gate` 红。
5. **数据权限按工作中心（`dept_id`）**，并且 SQL 里带 `{FILTER}` 注入位
   （否则「非超管 + 带业务筛选」会因参数顺序契约直接报错，见下「已知偏差 / 发现」）。

## 已知偏差（如实记录）

1. **导入 / 模板两个接口不在本卡**（设计 §5.2 #15/#16 = B2-2）。本卡只声明
   `business:erp:cost-labor:import` 权限码（菜单种子已种）。
2. **费率自动取数不在本卡**（§4.5.4 的对象 > 车间 > 公司 = B2-3）。所以 `rate` 是
   **必填**：缺失时 400 并在报文里点名「自动取数属 B2-3」，**不会默认 0**
   （设计 D-7「不猜」）。
3. **期间锁定（已结算期间只读）本卡不做**：成本核算批次表与结算流程属 B3；
   本卡的 `DRAFT/CONFIRMED` 闸只拦「已确认的行被改 / 删」。
4. **图省事的接口未提供**：设计 §5.2 工时台账没有「详情」接口（列表行即全量字段），
   本卡按设计不额外造 `GET /{id}`。
5. **出参金额是库标度字符串**（`"400.0000"` / hours `"8.00"` / rate `"50.000000"`），
   与 `cost_object` 的 `"100.0000"` 同口径；前端按字符串展示，不做算术。

## 发现（不在本卡范围，未动手）

- **`cost_object_model::page` 缺 `{FILTER}` 标记**（B1-8 遗留）：它的分页 SQL 带业务
  过滤参数却没有 `{FILTER}` 注入位。`scope::paginate` 的参数顺序契约是「片段参数恒前置」，
  当数据范围**真注入**（非超管 + Dept/DeptAndChild/Custom/Self 范围）且同时带业务筛选
  （关键字 / 状态 / 车间…）时，`ensure_injection_point` 会直接返回
  `InvalidValue` → handler 变成 **500「服务器内部错误」**。超管（All）范围不触发，
  B1-8 的真库用例与冒烟都是超管身份，所以当时没暴露。
  本卡在 `cost_labor_model::page` 里按正确形态实现（`WHERE … {FILTER} AND 业务条件`），
  并有单测 `page_base_sql_has_single_filter_marker_before_business_params` 兜底；
  `cost_object_model.rs` 属 B1-8 的文件，**未改**，建议单独立卡修（同批可顺带排查
  其它 `scope::paginate` 调用点是否都带标记）。
