# M5-20【M5·成本】在制品：约当产量表 · 证据

卡片：`t_d9c1c697`（父卡 `t_876fb4bb` M5-19 差异分析页）
交付物：**一条只读报表接口** `GET /api/business/erp/cost-wip-equiv`

---

## 1. 这张表回答什么

期间内**逐成本对象（生产批）**：本期完工多少、期末在制多少、完工率多少、
按约当产量法折出来两个分母（材料 / 加工）—— 一分钱都不分，只出**分母**。

```
① 本期完工   = 成本对象累计完工 − 前期已结算计算单的完工合计     （跨月批次不能用累计数）
② 材料约当   = 本期完工 + 期末在制 × 100%                        （一期只实现「开工一次投入」）
③ 加工约当   = 本期完工 + 期末在制 × 完工率%
```

②③ 直接复用 calc 第 6 段的 `cost_period_item_dto::equivalents`（**同一段代码**，
不是抄公式），所以「表值 vs 计算单」永远同一口径；`equiv_*` 落库 4 位、这里也按 4 位出。

## 2. 交付清单

| 文件 | 内容 |
|---|---|
| `backend/app/src/business/erp/cost/cost_wip_equiv_dto.rs`（新增 1367 行） | 契约 + 纯逻辑：`equiv_of` 一行算尽（三种行状态 / 五类问题码 / 三条只提示不拦的提醒 / 与计算单逐字段对拍）、`EquivTotals` 整表合计、出参装配、23 条单测 |
| `backend/app/src/business/erp/cost/cost_wip_equiv_model.rs`（新增 381 行） | 取数层：一条 SQL 取回对象 + 前期已结算汇总 + 本期归集 + 本期计算单快照 + 产出品 / 车间名（无 N+1）；数据范围复用 `ScopeTarget::BIZ_COST_OBJECT`；占位符/参数一致性硬校验；5 条单测 |
| `backend/app/src/business/erp/cost/cost_wip_equiv_handler.rs`（新增 273 行） | 接口层：入参归一化（期间必填 + 关键字 + 五档状态 + 分页上限 200）、编排（取数 → 算 → 筛 → 分页 → 整表合计 → 页脚说明）、`#[nik::permission]` + `#[nik::get]`；5 条单测 |
| `backend/app/src/business/erp/cost/cost_wip_equiv_db_tests.rs`（新增 786 行） | 10 条**真库**用例（`ERP_DB_URL` 开关）：算数 / 跨月扣减 / 与真计算单对拍 / 改完工率漂移 / 缺完工率 / 有投入无产量 / 无批次试算 / 数据范围 / 整表合计与四档筛选 / 清场零残留 |
| `backend/app/src/business/erp/cost/mod.rs`（+16 行） | 挂三个 `pub mod` + 一个 `#[cfg(test)] mod`，每个都写明为什么在这里 |
| `docs/evidence/2026-11-19-t_d9c1c697-cost-wip-equiv/smoke_wip_equiv.py` | HTTP 冒烟脚本（真库自造数 + 自清场，54 条断言） |

**零迁移、零新增权限码、零后端其它模块改动、零前端改动**（前端页是 M5-25）。

## 3. 权限与数据范围

| 项 | 决定 | 理由 |
|---|---|---|
| 权限码 | **复用** `business:erp:cost-object:list` | 读的是同一批 `biz_cost_object`，与 `cost-collect-sheet`（M5-02）/ `wip-balance`（M5-05）/ `cost-standard-compare` 同码。新开 `cost-wip:list` 没有菜单行承载 ⇒ 死码（`permission_audit_tests::n14` 判红）；页面码/接口码解耦（PERM-01）只在 HR 试点 |
| 数据范围 | 行由 `biz_cost_object.workshop_dept_id` 裁剪（`ScopeTarget::BIZ_COST_OBJECT`） | 与归集表同一锚点，不手写范围谓词。**表别名必须写成 `t`**：锚点里写死 `alias = "t"`，换个字母就只有非超管 500（超管走 `AND 1 = 1` 看不出来）—— 已加单测 `table_alias_matches_the_scope_anchor` 钉死 |
| 看不见的对象 | 不出现在表里，不报 403 | 列表没有「探测口」问题；单条入口的 404 语义属于 M5-21/M5-23 |

## 4. 三个不算账的决定（都不是 bug）

1. **算不出来就给 `null`，绝不给 0**。写 0 会被读成「没有在制 / 都完工了」，而真相是
   「缺一个输入」。问题码稳定：`WIP_PCT_MISSING`（在制>0 但完工率 0，与 calc 66067 同一判据）、
   `NO_OUTPUT`（本期既无完工也无在制）、`COMPLETED_NEGATIVE`（累计完工 < 前期已结算）、
   `PCT_OUT_OF_RANGE`、`MATERIAL_MODE_UNSUPPORTED`；每条都带「怎么修」。
2. **合计分两套口径**：数量合计（本期完工 / 期末在制）**算所有行**（它们是事实）；
   约当合计**整行跳过 blocked 行** —— 只加材料侧会给出一个「半个分母」，读的人分不清
   这是不是定数。合计旁边就写着「当前有 N 行算不出来」。
3. **行集不漏两类**：`期末在制>0` ∪ `本期完工>0` ∪ `上期已结算还挂着期末在制` ∪ `本期有归集行`。
   后两类正是「有投入没产量 / 跨月继续生产」的对象 —— 漏了它们等于「这行的成本没人管」。

## 5. 与计算单对拍（漂移）

有当期活跃批次时，把 `biz_cost_period_item` 的快照逐字段比：本期完工 / 期末在制 / 完工率 /
材料约当 / 加工约当。任一不等 ⇒ `recalc_required = true` + `drift[]` 逐字段给「表里 X / 计算单 Y」，
`state = drift` 可一屏筛出。算完又改数是最常见的人工事故，这张表当场指出来。

## 6. 验收（全部真跑）

| 项 | 命令 | 结果 | 证据 |
|---|---|---|---|
| 单测（含 10 条真库） | `ERP_DB_URL=…/cp_m520_dev cargo test --lib cost_wip_equiv -- --test-threads=1` | **43 passed / 0 failed**（10 条真库全跑） | `01_unit_tests.out` |
| HTTP 冒烟 | `WIP_BASE=http://127.0.0.1:18250 python3 smoke_wip_equiv.py` | **54 PASS / 0 FAIL** | `02_http_smoke.out` |
| 空库首启迁移 | 空库 `cp_m520_smoke` 起实例 | 应用 **398** 个迁移，202 张表，4s 内 ready | `04_migrations.out` |
| 成本域回归 | `cargo test --lib business::erp::cost` | 1165 passed / 0 failed | 见日志 |
| 格式化 / lint / 门禁 | `cargo fmt --check`、`cargo clippy --lib -- -D warnings`、`bash scripts/check.sh --fast` | 全绿 | `03_gates.out` |

冒烟覆盖的 13 组断言（每一条都是真接口 + 真库）：

1. 路由挂上了（200 + data，不是 404 / 不是 SPA HTML）
2. 缺期间 / 期间 `2026-13` / 状态拼错 `wipp` 三种 400 的报文
3. 完工率 × 数量：本期完工 800、在制折材料 200、材料约当 1000、在制折加工 100、加工约当 900
4. 跨月：前期已结算完工 300 ⇒ 本期完工 700（不是累计 1000）
5. 与真计算单逐字段一致 ⇒ `calc.agrees = true`、`recalc_required = false`
6. 在制>0 但完工率 0 ⇒ `blocked` + 加工约当 `null` + `WIP_PCT_MISSING` + 怎么修
7. 合计：`row_count = 3`、wip/finished/blocked = 1/1/1、数量合计含 blocked 行、约当合计跳过它
8. 四档筛选 wip / finished / blocked / drift（各命中 1/1/1/0）+ 关键字收窄
9. 改完工率 50→60 ⇒ `recalc_required = true`、drift 命中「完工率」60.00 vs 50.00、加工约当变 920
10. 没批次期间 ⇒ `cost_ready = false`、`batch = null`、页脚「试算视图」、行照出
11. `page_size = 9999` ⇒ 回 `page_size = 200`
12. 权限闸：EMPLOYEE 角色用户 ⇒ `403 无权限访问，需要权限: business:erp:cost-object:list`
13. 清场零残留

## 7. 边界（本卡不做）

- **钻取到工序级完工率**（M5-21 完工率录入）
- **料工费在完工与在制之间怎么分**（M5-22；本卡只出分母，一分钱不分）
- **期末在制余额**（M5-23）
- **前端在制品页**（M5-25）；本卡 API 的 `state_options` / `formula_*` / `totals.note`
  就是给那一页现成吃的
- **在制品单测大卡**（M5-24）：本卡的 10 条真库用例可直接作它的起点
