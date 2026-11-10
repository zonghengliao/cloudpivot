# COST-B1-11【成本】前端成本对象页 —— 实测证据

卡：`t_d49c483f`（拆卡清单 ARCH-9-3 §8.1 的 **B1-11**）
依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
§6.1 #1（页面 / 路由 / 菜单）、§6.2.1（表单字段 + 按钮态）、§5.2 #1–#5（接口与权限码）、
§4.1（状态机 + 关批 5 条前置）

## 交付物

| 文件 | 内容 |
|---|---|
| `frontend/src/api/business/erp/cost-object-kinds.ts` | 词表 + 契约类型（**不 import 请求层**，纯逻辑单测可直接引用）：状态 / 投料方式 / 关批拦截分类与指引 / 66055 / 列表行 / 详情 / 建改关批入参 |
| `frontend/src/api/business/erp/cost-object.ts` | 5 个接口：`fetchCostObjectPage` / `fetchCostObjectDetail` / `createCostObject` / `updateCostObject` / `closeCostObject`，`export *` 出词表 |
| `frontend/src/views/business/erp/cost-object/cost-object-logic.ts` | 纯逻辑：状态译名/主题、`monthRange`（月→开工日期区间）、表单校验、`toCreatePayload`、`toUpdatePayload`（部分更新三态）、按钮态、`closeSummaryOf` / `requiresForceWip` / `toClosePayload` / `closeErrorText` |
| `frontend/src/views/business/erp/cost-object/index.vue` | 页面：列表（分页 + 状态/开工期间/车间/关键字筛选）+ 建/改表单 + 详情抽屉 + 关批二次确认 |
| `frontend/src/router/index.ts` | 注册 `business/erp/cost-object` → `BusinessErpCostObject`（菜单 V0378 `…c001` 的 path/component/权限码逐字对应） |
| `frontend/tests/unit/erp-cost-object.test.ts` | 27 条纯逻辑单测（跑在 `pnpm test:unit` 里，零新依赖） |

## 验收对照（卡面四条）

| 卡面要求 | 落点 | 实测 |
|---|---|---|
| 列表分页、筛选（状态/期间/车间） | `index.vue` 工具栏 + `fetchCostObjectPage` | 状态=已关批 → 1 条；开工期间=2026-10 → 0 条、=2026-11 → 2 条；车间=总部 → 2 条。**请求 URL 实测带参**（见下） |
| 表单校验必填项 | `validateCostObjectForm` + 标红 | 清空计划数量点「创建」→ 红框 + `计划数量必填` + 顶部「还有必填项没填对」；补齐后创建成功 |
| 关批按钮二次确认 + 错误提示 | 关批弹窗 → `DialogPlugin.confirm` 终确认 → `closeErrorText` | 未勾确认 → 拦；在制 12 未勾「带在制关批」→ 拦；勾两项后弹「最后确认」，确认后关批成功；有未归集领料 → 409/66055 原文 + 「怎么修」指引 |
| `pnpm typecheck && build` | — | 均 exit 0（见下） |

## 实测输出

### ① 隔离实例（真实例 + 真浏览器，不是 mock）

```
空库 costb111_web（首启全链重放迁移）→ 后端 127.0.0.1:18030（profile=test）
前端 vite dev 127.0.0.1:5199（BACKEND_ORIGIN=http://127.0.0.1:18030）
admin 初始口令经 NIK_ADMIN_PASSWORD 注入 → 首登改密 → 真令牌进 localStorage
```

### ② 列表筛选打到后端的真实 URL

```
/api/business/erp/cost-object?page=1&page_size=20
/api/business/erp/cost-object?page=1&page_size=20&start_from=2026-10-01&start_to=2026-10-31   → 共 0 个
/api/business/erp/cost-object?page=1&page_size=20&start_from=2026-11-01&start_to=2026-11-30   → 共 2 个
/api/business/erp/cost-object?page=1&page_size=20&workshop_dept_id=20000000-…-0000000f0366&start_from=2026-11-01&start_to=2026-11-30 → 共 2 个
```

> 列表接口**没有** `period_code`，页面上的「开工期间」= 开工日期区间（`monthRange` 折算，闰年 2 月有单测），
> 这是设计 §5.2 #1 的入参口径，不是自创。

### ③ 建单 / 校验 / 改单（浏览器真实点击）

```
选产出品下拉 → M-W1A-004 成品D / M-W1A-005 成品E / M-W1A-003 半成品C
   ← FINISHED + SEMI 两次请求合并（物料接口一次只认一个 material_type）
选车间（DeptTreeSelect：总部）、目标仓（总仓（种子），树拍平带缩进）
计划数量清空 → 创建 ⇒ 顶部 alert「还有必填项没填对，请看下方标红项」+ 计划数量红字「计划数量必填」
计划数量 100 → 创建 ⇒ 列表出现 CB-20261110-000001 在产 详情/编辑/关批
编辑该单（计划数量 50→60 + 备注）⇒ 列表当行变 60（PUT 只发改过的字段）
```

### ④ 关批（二次确认 + 错误提示）

```
① 在制=0 的对象：勾「我已确认」→ 确认关批 → 弹「最后确认」→ 确认 ⇒ 成功
   （同一对象后续：行状态 已关批，操作列只剩「详情 / 已结项」）
② 在制=12（SQL 造）：
   未勾任何确认 → 「请先勾选确认「关批后不可回在产」」
   只勾第一项   → 「期末在制 12 不为 0：请勾选「带在制关批」，或先结算掉在制数量」
   勾两项 → 终确认 → 关批成功
③ 有 1 张已过账未归集领料（SQL 造 biz_stock_out）→ 后端口径：
   成本对象 CB-20261110-000002 不能关批：[UNCOLLECTED_STOCK_OUT] 还有 1 张领料出库单
   已过账但**没有归集**到本成本对象：先归集再关批，否则这批材料成本永远进不了成本计算单
   （请求 ID：3d73daa0-…）（怎么修：去「出库单」把领料归集到本对象（或重跑成本计算））
   ⇒ 对象仍是「在产」，可以继续改数据后重试（不留死单）
```

### ⑤ 前端闸

```
pnpm typecheck  ⇒ exit 0（vue-tsc --noEmit）
pnpm build      ⇒ exit 0（内含 typecheck；产物 dist/index.html + assets/*）
pnpm test:unit  ⇒ # tests 1356 / # pass 1356 / # fail 0（本卡新增 27 条）
```

## 已知偏差（都在卡内说明，未擅自动别的模块）

1. **列表没有「开工日期」列**：后端 `CostObjectListRes` 按 §5.2 #1 只给 15 个字段，不含
   `start_date` / `due_date` / `completion_pct`。列表用「产出品 / 批号 · 订单」副行给线索，
   日期在详情抽屉看。要加列需先改后端出参（另一张卡）。
2. **删除按钮不做**：后端 B1-8 只实现了 5 个接口，`DELETE /business/erp/cost-object/{id}` 无路由
   （权限码 `cost-object:delete` 已种但无 handler）⇒ 前端不挂必 404 的按钮。
3. **作废按钮不做**：`OPEN→CANCELLED` 边已在状态机注册，但作废接口属另一张卡。
4. **「领料」快捷入口不做**：§6.2.1 要求「跳转新建出库单并带 `cost_object_id`」，
   但出库单页目前**完全不认** `cost_object_id`（`grep` 前端 stock-out 目录零命中）；
   该能力正是下游卡 **COST-B1-12**（`t_3d4dd487`「领料单加成本对象选择器」）的范围，本卡不越界。
5. **「开工期间」是月份选择器**：设计 §5.2 #1 给的是 `start_from` / `start_to` 日期区间，
   页面用「月」粒度换取一次点击，折算逻辑与闰年边界有单测（`monthRange`）。
6. **下拉主数据一次拉 200 条**：产出品（FINISHED+SEMI 各 200）、客户（ACTIVE 200）、仓库树（全量）。
   超过后靠下拉 filterable 搜素；供应商式远程搜索不在本卡范围。
7. 详情抽屉未加「关闭」按钮（与仓库既有 `asset` 页同款写法，点遮罩 / ESC 关闭）。
   本卡实测遮罩点击可关（`t-drawer--open` 消失）。
