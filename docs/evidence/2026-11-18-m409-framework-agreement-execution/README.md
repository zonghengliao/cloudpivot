# M4-09 框架协议：执行量统计（已用 / 剩余）· 证据

卡：`t_645358e9`（M4-09【M4·采购】框架协议：执行量统计）
上游：`t_06b8c65b`（M4-08 订单引用：校验价格 / 数量 + 执行量重算回写）
交付：后端（迁移 + 四件套里的 dto/model/handler + 真库用例 + 演练脚本），**零前端**

## 交了什么

| 层 | 落点 | 说明 |
|---|---|---|
| 迁移 | `V0452__framework_agreement_execution.{up,down}.sql` | 两个视图 + 2 行菜单（页面 …4480 / 执行量按钮 …4481）+ 授权；up 自带探针自检，down 先删授权再删菜单再删视图 |
| 视图 | `v_framework_agreement_item_execution` | 物料行级：一行 = 协议的一格（物料）——已用 / 剩余 / 执行率 / 行状态 / 单据数 / 最近下单日 |
| 视图 | `v_framework_agreement_execution` | 单头级：一张存活协议一行——合计 + 有效封顶 + 剩余 + 率 + 行状态计数 + 有效期状态 |
| 接口 | `GET /business/erp/framework-agreement/execution/page` | 跨协议执行量总览（关键字 / 供应商 / 状态 / 有效期状态 / 只看超限 + 分页） |
| 接口 | `GET /business/erp/framework-agreement/{id}/execution` | 单协议执行量：单头 + 逐物料行 + 下钻来源（含作废 / 软删单，标 `counted=false` + 原因） |
| 权限码 | `business:erp:framework-agreement:execution` | 两个只读接口共用（ERP 尚未解耦页面码/接口码，过渡期条款） |
| 菜单 | `business:erp:framework-agreement:page`（…4480，页面）+ `…:execution`（…4481，按钮） | 页面 Vue 文件属 **M4-11**；页面行必须先落，否则按钮行 = 孤儿 parent（menu_invariants ①） |

**本卡不包含**：前端页面（M4-11 `t_da2f0a9b`）、整模块单测收口（M4-10 `t_4da4c498`）、协议 CRUD 接口（计划里没有单独卡，M4-11「协议维护」需要它时另开卡）。
`frontend/src` 下 `framework-agreement` 命中数 = **0**。

## 口径（写死在 V0452 头注释里，代码有纯函数逐字对应）

1. **已用量现算，不读回写列**：`Σ 采购明细 qty`，且采购单 `deleted_at IS NULL AND status <> 'CANCELLED'`。
   草稿算（量已占住，与合同额度守卫 `committed_amount` 同口径）；作废 / 软删不算（链已断）。
   M4-08 的回写列（`booked_qty` / `booked_total_qty`）**并排返回**用于对账：差非 0 就是分叉，
   报表直接显形（`items[].booked_drift`）。
2. **金额口径跟协议价走**：协议价含税 → 比 `Σ with_tax_amount`；不含税 → 比 `Σ amount`。
   与 M4-08 `amount_for_cap` 同一口径（换口径比金额等于白送 / 白扣一个税率）。
3. **剩余是 `null` 而不是 0**：封顶 `NULL` = 不限 ⇒ 剩余 / 执行率都是 `null`；负数 = 超上限（必须显形，不许截成 0）。
4. **封顶优先级**：单头 `total_max_qty` → 没写则「明细上限合计」，但**只有明细全部设了上限**（`unlimited_items = 0`）时它才有效；有一行不限，整体就不封顶。金额封顶只认单头 `total_max_amount`。
5. **有效期是时间的函数**：`APPROVED AND start <= 当日 <= end`（与 `framework_agreement_model::is_effective_on` 一致）；`validity_state`（DRAFT / SUBMITTED / NOT_STARTED / ACTIVE / EXPIRED）只是人话翻译，不落库。
6. **空物料范围返 200 不返 404**：协议单头是合法实体，0 行是「拟稿还没编物料」的正常中间态；只有协议不存在 / 已软删才 404（M4-04 计划口径不同：空计划 404 —— 那是计划不是协议）。

## 验收（本目录原始输出）

| 文件 | 是什么 | 关键数 |
|---|---|---|
| `01_drill_full.out` | 演练全跑（克隆 145 模板库 → V0405…V0451 → V0452 两遍 → 结构 → 口径 SQL 直查 → 空库全链重放 → 回滚三段 → 真库用例 → 门禁 → 残留） | **PASS=60 FAIL=0**，空库重放 372/372 |
| `02_teeth_skip_guard.out` | 有牙自检：`M409_BAD_URL=1`（连接串故意写错） | **rc=1 / FAIL=13**（SKIP 守卫 + 12 条正向标记缺失） |
| `03_schema_dump.txt` | 两个视图的完整定义 + 菜单两行 + 4 条授权 | 视图列 28 / 48 |
| `04_scope_no_frontend.txt` | 范围守卫：2 条路由 / 2 条权限注解 / 前端 0 命中 / 号段核对 | — |
| `05_gates.out` | rustfmt --check / clippy（--lib 与 --lib --tests）/ 本卡 24 条用例 / permission_audit 8 条 / menu_invariants 8 条 | 24 passed；8+8 全绿；菜单总数 550 |

复跑：

```bash
# 演练（在 99 上跑，连 145 的 PG；库名含 drill，脚本自己 DROP）
bash scripts/verify/m409_framework_agreement_execution_drill.sh
# 有牙自检（期望 rc=1）
M409_BAD_URL=1 SKIP_REPLAY=1 bash scripts/verify/m409_framework_agreement_execution_drill.sh

# 单卡真库用例
HOST=172.18.10.145
PGPASSWORD=*** createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_m409_exec
ERP_DB_URL="postgres://cloudpivot:***@$HOST:5432/cp_m409_exec" \
  cargo test --lib business::erp::purchase::framework_agreement_execution -- --test-threads=1
```

## 用例覆盖面（12 真库 + 12 纯逻辑）

真库：新协议全量剩余 / 两张单累加 / 作废回落 + 下钻原因 / 软删回落 / 超上限（负剩余）/ 不限量（null 剩余）/ 单头封顶优先 / 金额口径随含税 / 单头合计与 `summary_of` 对拍 / 有效期五态 + `is_effective` / 总览筛选 + 软删不进总览 + 详情 404 / 清场 0 残留。

纯逻辑：剩余保留 null 与负数 / 率需要正分母 / 行状态五态边界 / 有效期先看状态再看日期 / 下钻三态判定 / 汇总的明细合计兜底 / 百分比两位小数 / 筛选非法输入一律 400 / 视图名与迁移逐字一致 / 两个列清单覆盖 reader / 总览目标是 SCOPE-EXEMPT。

## 踩过的坑（已写进 skill `cloudpivot-backend-workflow/references/framework-agreement-cards.md` §11）

- `biz_purchase_order_item` 的 `ck_biz_po_item_tax` 要求 `with_tax_amount = amount + tax_amount` —— 探针只写 `with_tax_amount` 会被拒（迁移自检直接炸在第 89 行）。
- `ck_biz_fwa_no_fmt` 是**严格 6 位数字流水**：`FWA-19990101-94090a`（字母）和 7 位号都会被拒；夹具号段要按 `940901…940910` 递推。
- 协议夹具的物料上限会**反向卡住**本卡的用例：用「上限 40」的协议去下 30 + 20 会先被 M4-08 拒（409/66098），报表用例要先算清上限预算。
- 视图里的「不限」必须一路传到 DTO 的 `Option`：任何一处 `unwrap_or(0)` 都会把「不限量」显示成「一点都不剩」。
