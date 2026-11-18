# M4-11b 框架协议维护链路（CRUD + 提交审批）· 交付证据

卡：`t_f358da98`（M4-11b【M4·采购】框架协议维护（CRUD）后端接口 + 页面写入口）
提交：`3d256f92`（origin/main + github/main）

## 一、这一卡补的是什么洞

M4-11（`t_da2f0a9b`）交付的 `/business/erp/framework-agreement` 页面**只能读**：
框架协议域当时有表（M4-07）、有下单引用校验（M4-08）、有两条只读统计接口（M4-09），
**没有任何写接口**，库里也没有 `…:create/update/delete/submit` 四个权限码。
协议只能靠导入 / 数据修复建立 —— M4-21（协议 + 合同关联 E2E）与 M4-22（收口）都卡在这。

本卡把它打通成一条真正能走的路：**建 → 提交 → 审批（采购经理 → 财务经理）→ 下单 → 执行量涨**。

## 二、后端接口（6 条，5 个新权限码 + 1 个列表码）

| 方法 | 路径（前缀 `/api` 由 `[router].prefix` 注入） | 权限码 | 说明 |
|:--|:--|:--|:--|
| GET | `/business/erp/framework-agreement/page` | `…:list` | 台账列表（与执行量总览同一张视图，码不同：台账 Tab 判 `…:list`） |
| GET | `/business/erp/framework-agreement/{id}` | `…:list` | 详情（单头 + 物料范围，**含行备注**，改单表单靠它回填） |
| POST | `/business/erp/framework-agreement` | `…:create` | 建单（`FWA-yyyyMMdd-000001`，DRAFT） |
| PUT | `/business/erp/framework-agreement/{id}` | `…:update` | 整单替换（只有 DRAFT） |
| DELETE | `/business/erp/framework-agreement/{id}` | `…:delete` | 软删（只有 DRAFT） |
| POST | `/business/erp/framework-agreement/{id}/submit` | `…:submit` | 提交审批（`fw_agreement` 流程） |

执行量两条只读接口（`…:execution`）保持 M4-09 原样不动。

## 三、校验口径（与 M4-08 同一份判据）

| 判据 | 库层 | 请求层 |
|:--|:--|:--|
| `start_date <= end_date` | `ck_biz_fwa_period` | `check_period` |
| `FLOATING ⇒ price_adjust_rule` 非空 | `ck_biz_fwa_adjust` | `check_adjust_rule` |
| `(agreement_id, material_id)` 唯一 | `uk_biz_fwai_material` | `check_materials_unique`（报文点出「第 3 行与第 1 行」） |
| `agreed_price >= 0` | `ck_biz_fwai_price` | `check_agreed_price` |
| `max_qty / total_max_*` NULL 或 > 0 | `ck_biz_fwai_cap` / `ck_biz_fwa_caps` | `check_cap`（报文教「想不限就留空」） |
| **改单不许把上限改到低于已执行量** | 库层做不到（跨行） | `check_cap_shrink` + `guard_item_shrink`（取「链现算」与「回写」的较大值） |
| **不许删掉已有执行量的物料行** | 弱引用无外键 | `check_missing_material` + 库层兜底 `AND executed_qty = 0` |

## 四、三个关键设计决定（都会被后人踩到）

1. **改单明细按物料就地 UPDATE，不删光重建。**
   `biz_purchase_order_item.framework_agreement_item_id` 是弱引用（V0451 刻意不加外键）。
   删行重建 → 行 id 悬空 → M4-09 的执行量视图（从采购单链现算）**静默少掉**那段用量，
   而回写列还在 → `booked_drift` 非 0，谁也说不出用了多少。所以：同物料原地改、新物料插入、
   消失的物料只在 `executed_qty = 0` 时删。行号先 `+1000000` 挪开再落定（`uk_biz_fwai_line` 是即时唯一索引，两行对调会撞键）。
2. **审批线是新的 `FWA_APPROVE` / `fw_agreement`，不是复用 `PP_APPROVE`。**
   `biz_type` 是单号级的（一个实例只服务一种单据），复用会让 `purchase_plan_bridge` 收到不属于它的实例。
   接线点：`FlowBizType` 27→28、`ErpDoc` 20→21、`GOVERNED_CODES` 19→20、前端 `doc-kinds.ts` 三张表。
3. **`GET /page` 与 `GET /execution/page` 返回同一份行、两个权限码。**
   AGENTS 约定 14：页面显形码与接口码解耦，两个 Tab 是两个入口（台账看单据 / 执行量看用量）。
   `/page` 委派 `execution_model` 取数，不复制 SQL。

## 五、迁移 V0456（只追加、幂等、有回滚）

- 5 行按钮菜单（`…004490–004494`，全部挂在 V0452 的页面行 `…004480` 下 —— `menu_invariants ①` 查孤儿 parent）
- 角色授权：`PURCHASE_MANAGER` 全 5 行；`FINANCE_MANAGER` 只 `…:list`（改条款是采购的活，职责分离）
- 流程四层种子 `fw_agreement`：START → 采购经理 → 财务经理 → END（4 节点 / 3 边 / 2 ROLE 审批人）
  `graph_hash = de47937e30aa0fa1b56b007b1f4e3b7774cd3dd848e5bde4f35eb82d67885629`
  （**实测回填**：先落 64 个 0 → 跑 `seeded_flow_hash_matches_rows` 拿到 `load_graph()` 重算值 → 改迁移 → 重建库复验）
- 末尾 DO 块自检：菜单 5 行 / 同父 sort 唯一 / title ≤12 / 授权 / 流程四层 / 快照条数 / 3 条串行边
- 回滚 `V0456__framework_agreement_crud.down.sql`：**有协议单就拒绝**（撤掉入口会让在途协议没人能推进）

## 六、前端（页面写入口）

`frontend/src/views/business/erp/framework-agreement/index.vue`：
- 台账 Tab 接上「新建协议 / 编辑 / 提交 / 删除」，**逐条 gate**（canCreate / canUpdate / canDelete / canSubmit），
  且只有 `status === 'DRAFT'` 可点（与后端 `editable` 同一判据）
- 表单抽屉：单头 12 个字段 + 物料范围表（物料 / 协议价 / 税率（%）/ 数量上限 / **已用（只读）**/ 备注）
- 页脚只读说明改成真实能力（`data-testid="fwa-ledger-note"` 保留，e2e 依赖）
- **保留**的 testid：`fwa-ledger-note` / `fwa-ledger-table` / `fwa-detail-drawer`；新增 `fwa-new` / `fwa-form-dialog` /
  `fwa-ledger-edit|submit|delete` / `fwa-line-*` / `fwa-add-line` / `fwa-lines-table`
- 编辑用 `GET /{id}`（维护口径，含行备注）回填，**不用** `/{id}/execution`（那份没备注，整单替换会抹掉）

## 七、验收

```
bash scripts/verify/m411b_framework_agreement_crud_drill.sh        → PASS=28 FAIL=0 RESULT: PASS
M411B_BAD_URL=1 ... -- SKIP_GATES=1                                → PASS=24 FAIL=1 RESULT: FAIL（**预期红**：真库标记=0 时不许按 passed 数放行）
cargo fmt / cargo clippy --lib -- -D warnings                      → rc=0
pnpm typecheck && pnpm build                                        → rc=0
真库用例 framework_agreement_crud_db_tests                          → 12 passed / 0 failed / 0 SKIP
回归 business::erp::purchase::framework_agreement                   → 101 条全绿（M4-07/08/09/10 + 本卡）
回归 business::flow::doc_reader_tests / erp::doc_reader_impl_tests  → 18 / 16 条全绿（审批线接线对账）
门禁 permission_audit_tests + menu_invariants.sh                    → 全绿
```

真库用例清单（12 条）：

| 用例 | 钉住什么 |
|:--|:--|
| `create_draft_shapes` | 单号 `FWA-yyyyMMdd-NNNNNN` / DRAFT / 供应商与经办人姓名快照 / 行号 / 派生合计 0 / `editable` |
| `create_rejects_each_bad_branch` | 11 个反向分支各一条：日期倒序、FLOATING 无规则、同物料两行、负价、明细上限 0、单头上限 0、容差越界、供应商不存在、物料不存在、缺 start_date；末尾断言「被拒的一条都没落库」 |
| `update_replaces_items_in_place` | 同物料原地保留行 id、改价改上限、加物料、删零执行量的行、行号重排 |
| `update_rejects_non_draft` | 提交后改 / 删 → 409 |
| `update_rejects_shrinking_below_executed` | 上限 10 < 已用 30 → 409；== 30 放行；删有执行量的行 → 409；单头总量收紧 → 409 |
| `delete_only_draft` | 软删后查不到（明细保留供审计）；重复删 404 |
| `submit_goes_through_flow` | SUBMITTED + `FWA_APPROVE` 实例 + 两节点真批 → APPROVED + 实例 id 保留；重复提交 409 |
| `reject_returns_to_draft` | 驳回 → DRAFT + 意见进备注 + 实例清空；改完能再提 |
| `submit_rejects_empty_scope` | 空物料范围提交 → 400 |
| `full_chain_order_raises_execution` | 审批后走 M4-08 真实下单入口 → 明细回写 40 / 单头派生合计 40 / 详情剩余 60；再改单收紧 → 409 |
| `seeded_flow_hash_matches_rows` | V0456 的 graph_hash 与节点·边行重算逐字节一致 |
| `zz_teardown_purges_fixtures` | `M411B-` 夹具 0 残留 |

## 八、遗留 / 交接

- **页面没有 e2e**：`frontend/e2e/m411-framework-agreement.spec.ts` 仍只覆盖只读台账；
  新增的写入口（`fwa-new` / `fwa-form-dialog` / `fwa-ledger-submit` 等 testid 已就位）留给 M4-21 的 E2E 卡。
- **`GET /page` 与 `GET /execution/page` 出参完全同形**：若后续给台账做窄列（去掉执行量列），
  在 `framework_agreement_dto` 加一个 `AgreementRowRes` 版列表即可，不动执行量那条。
- **全仓还有「明细删光重建」的改单写法**（如采购计划 `replace_plan`）：凡是明细被弱引用的单据都有
  同款悬空风险，见到 `DELETE FROM ..._item WHERE plan_id` 可顺手按本卡的「按业务键就地 UPDATE」改。
