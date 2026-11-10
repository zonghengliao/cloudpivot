# COST-B1-8【成本】后端成本对象四件套 —— 实测证据

卡：`t_d52fc991`（拆卡清单 ARCH-9-3 §8.1 的 **B1-8**）
依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
§3.1 ②（`biz_cost_object`）/ §4.1（状态机 + 关批 5 条前置）/ §5.1（出参口径）/ §5.2（API 清单）

## 交付物

| 文件 | 内容 |
|---|---|
| `backend/src/business/erp/cost_object_dto.rs` | 契约：列表筛选 / 建改入参（部分更新三态）/ 详情与列表出参 / `CloseFacts` + `evaluate_close`（关批判定的**纯函数**） |
| `backend/src/business/erp/cost_object_model.rs` | 数据层：单头 + 5 个 join 补名 + 归集汇总 + 计算单 + 建改落库 + 关批事实查询 + 边表常量 |
| `backend/src/business/erp/cost_object_handler.rs` | 接口层：5 个接口（list / detail / create / update / close）+ 引用校验 + 关批事务编排 |
| `backend/src/business/erp/cost_object_db_tests.rs` | 真库用例 8 条（`ERP_DB_URL` 开关） |
| `backend/src/business/shared/state_machine.rs` | 注册 `Entity::CostObject`（`COST_OBJECT_EDGES` 2 条 / BASE / REASON / LOCKED） |
| `backend/src/business/shared/doc_kind.rs` | `DocKind::CostObject`（`COST_OBJECT` / 「成本对象」/ `biz_cost_object`）+ `DOC_KIND_VALUES` |
| `backend/src/business/shared/scope.rs` | `ScopeTarget::BIZ_COST_OBJECT`（`dept_col = workshop_dept_id`）并登记进 `SCOPED_TABLES` |
| `backend/src/business/permission/scope_model.rs` | `TABLE_WIRING` 补 `biz_cost_object`（D8 覆盖率闸） |
| `backend/src/business/shared/guard.rs` | 启动自检清单计数 12 → 13 |
| `backend/src/business/flow/linkage/spec.rs` | `TRIGGER_EXEMPT` 登记 `COST_OBJECT`（关批不调联动引擎，理由写在代码里） |
| `backend/src/shared/error_code.rs` | 新业务码 `ERP_COST_OBJECT_CLOSE_BLOCKED = 66055`（进 66xxx 稳定性断言） |
| `frontend/src/api/business/erp/doc-kinds.ts` | 词表镜像（`DOC_KINDS` / `DOC_KIND_LABELS` / `DOC_KIND_TABLES`） |
| `docs/architecture/snapshot-drift/10_checks.sql` | `STATUS_ENTITY` 登记 `COST_OBJECT → biz_cost_object` |

## 实测输出

### ⓿ HTTP 冒烟（真实例 `127.0.0.1:18024`，空库首启 308 个迁移，admin 真登录）

见 `03_http_smoke.out` —— **5 个接口全通，且是 HTTP 层、带鉴权**：

```
2) POST   /business/erp/cost-object            code=0  object_no=CB-20261110-000001 status=OPEN
                                                  plan_qty=100.0000 product_name=成品E（种子）
                                                  dept=总部 next=['CLOSED','CANCELLED']
3) GET    /business/erp/cost-object?keyword=…   code=0  total=1 page=1 page_size=10
4) GET    /business/erp/cost-object/{id}        code=0  collect_summary={mat,labor,oh,total}
                                                  legal_edges=[['OPEN','CLOSED'],['OPEN','CANCELLED']]
5) PUT    /business/erp/cost-object/{id}        code=0  plan_qty=150.0000
6) POST   /business/erp/cost-object/{id}/close   HTTP 409  code=66055
      msg=… [UNCOLLECTED_STOCK_OUT] 还有 1 张领料出库单已过账但没有归集到本成本对象…
7) POST   …/close（补归集后）                    code=0  status=CLOSED closed_at=… next=[]
8a) 重复关批                                     HTTP 409 code=10003
8b) 已关批改单                                   HTTP 409 code=10003
```

未经鉴权请求同一批路径返回 **401**，未知路径返回 **404** —— 说明 5 条路由确实注册到了
router（不是「文件写了但没挂上」）。

### ① 真库用例（8/8，库 = `mig_replay_costb18`，空库全链重放 295/295 后的库）

见 `01_real_db_tests.out`：

```
test close_blocked_by_draft_labor ... ok                              ← 前置 ①
test close_blocked_by_locked_period ... ok                            ← 前置 ③
test close_blocked_by_uncollected_stock_out ... ok                    ← 前置 ②（卡面点名）
test close_blocked_by_unposted_stock_out ... ok                       ← 前置 ④
test close_succeeds_when_five_checks_pass ... ok                      ← 5 条全过 + 状态日志 2 行
test create_page_detail_update_flow ... ok                            ← 5 个接口全通
test update_rejects_after_close_and_product_change_with_collect ... ok ← 改单守卫
test wip_requires_force_confirmation ... ok                           ← 前置 ⑤
test result: ok. 8 passed; 0 failed
```

### ② 纯逻辑单测（不连库）

见 `02_pure_unit_tests.out`：

```
cost_object（dto + model + handler 纯逻辑）:     41 passed / 0 failed
doc_kind + state_machine + scope + error_code
  + linkage 触发点守卫:                         115 passed / 0 failed
```

关批 5 条前置**各一个用例**（`cost_object_dto::tests`）：

| 用例 | 前置 |
|---|---|
| `close_blocked_by_draft_labor` | ① 草稿工时台账 |
| `close_blocked_by_uncollected_stock_out` | ② 已过账未归集的领料 |
| `close_blocked_by_locked_period` | ③ 当期批次已锁定 |
| `close_blocked_by_unposted_stock_out` | ④ 未过账的领料单 |
| `close_blocked_by_wip_unless_forced` | ⑤ 在制不为 0（`force_wip` 后放行） |
| `all_blockers_are_reported_together` | 五条同时命中时**全部**报出（不是只报第一条） |

### ③ 全量 `cargo test` + clippy + 前端

```
cargo test                     30 个测试目标全 ok（0 FAILED / 0 error）
cargo clippy --all-targets -D warnings   0 error
cargo fmt --check              干净
pnpm typecheck                 exit 0
pnpm build                     exit 0
```

## 已知偏差（如实记录）

1. **卡面 5 个接口 vs 设计 6 个**：设计 §5.2 还有 `DELETE /business/erp/cost-object/{id}`（软删）。
   本卡按卡面验收只实现 5 个（list / detail / create / update / close）；
   `PERM_DELETE` 权限码已声明（菜单种子与前端按钮要共用），接口本身留给后续卡
   —— 卡的范围纪律要求「只做本卡写的事」。
2. **成本对象作废（`OPEN → CANCELLED`）本卡不提供入口**：边表已按设计 §4.1 注册
   （`COST_OBJECT_EDGES` 两条），`next_statuses` 也会如实返回 `CANCELLED`，
   但作废接口属「作废 + 前置校验」另一张卡；本卡只落关批那条边。
3. **依赖 `biz_stock_out.cost_object_id`**：该列由 B1-5 的迁移 `V0375` 添加（已在本工作区
   且已在重放库中）。若只部署本卡而不部署 V0375，关批的前置 ②④ 会因缺列报错 ——
   两张卡在同一批次里，顺序上 B1-5 在前。
4. **权限码还没有 `sys_menu` 承载行**：设计 §5.3 的 5 个码已声明为常量，
   菜单 / 按钮种子迁移属 B1-7（`V0379` 那一段）。`perm_menu_gate.py` 属 L2 层（需库），
   在菜单种子落地前会红 —— 与本卡无关，但记在这里以免误判。
5. **`cost_total` 的小数位**：无归集时是 `"0"`（`COALESCE(sum(...),0)` 的 numeric 标度），
   有归集时是 `"100.0000"`（`NUMERIC(18,4)`）。前端按字符串展示，不做算术。
