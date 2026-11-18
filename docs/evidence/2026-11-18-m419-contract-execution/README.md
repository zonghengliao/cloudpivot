# M4-19 合同执行量统计（已执行 / 合同额）· 证据

卡：`t_08ae874c`（M4-19【M4·采购】合同执行量统计 —— 已执行/合同额）
上游：`t_b90d4761`（M4-18 采购单合同额度守卫）—— 本卡口径必须与它**同源**。
交付：后端（迁移 + 视图两枚 + dto/model/handler + 12 条真库用例 + 演练脚本 + 证据），**零前端**。

## 交了什么

| 层 | 落点 | 说明 |
|---|---|---|
| 迁移 | `V0458__erp_contract_execution.{up,down}.sql` | 两个视图 + 1 行菜单（按钮 `…4498`）+ 3 角色授权；up 自带探针自检，down 先删授权再删菜单再删视图 |
| 视图 | `v_contract_execution_order` | 单据级：一行 = 一张挂在合同上的单据（采购单 / 销售单，含作废 / 软删，`counted` 标记 + 币种标记）；12 列 |
| 视图 | `v_contract_execution` | 单头级：一张存活合同一行——已执行额 / 净额 / 剩余 / 执行率 / 执行状态 / 单据数 / 币种串行数 / 到期信息；37 列 |
| 接口 | `GET /business/erp/contract/execution/page` | 跨合同执行量总览（关键字 / 类型 / 状态 / 执行状态 / 只看超限 / 只看过期 / 只看币种串 + 分页） |
| 接口 | `GET /business/erp/contract/{id}/execution` | 单合同执行量：单头汇总 + 下钻单据（含作废 / 软删，`counted=false` + 原因） |
| 权限码 | `business:erp:contract:execution` | 两个只读接口共用；`contract_handler::PERMISSIONS` 由 5 条扩到 6 条 |
| 菜单 | 按钮 `…4498`（`menu_type=3`，sort 50） | 挂在**已存在**的「合同管理」页面行 `…001100`（V0197 建 / V0200 放显形 / V0241 改前缀）下 |

**本卡不包含**：前端页面（ERP 合同页的「执行量」tab / 视图属后续收口卡；M4-09 也是「后端先行、页面后补」的同款分工），
`frontend/src` 下 `contract/execution` 命中数 = **0**（见 `04_scope_no_frontend.txt`）。

## 口径（写死在 V0458 头注释里，代码有纯函数逐字对应）

1. **已执行额现算，不缓存**：Σ 单据 `total_with_tax`，判据
   `deleted_at IS NULL AND status <> 'CANCELLED'`。
   与 `purchase_model::committed_amount` / `sales_order_model::committed_amount`
   **逐字同口径**（草稿也算、作废/软删不算）—— 换口径就等于制造「报表说还剩 10、下单却被拒」的争议。
   `V0195` 头注释早已定死「合同表不许有 `used_amount` 缓存列」。
2. **下游按合同类型分派**：`PURCHASE → 采购单`、`SALES → 销售单`、`SERVICE / LEASE → 无下游`。
   不按「谁挂了 contract_id」猜：两个 handler 各自校验类型，视图仍按类型分派，
   是为了让「绕过 handler 写进库的错挂行」不进统计（宁可少算不混算）。
3. **剩余额可为负**：`amount − executed`。守卫只拦**新单**，合同额被改小 / 单据被恢复都会越界，
   报表必须显形，绝不截成 0。
4. **执行状态五态**：`NO_DOWNSTREAM`（服务/租赁，没有可执行的单据）/ `NOT_STARTED` / `PARTIAL` /
   `DONE`（恰好用满）/ `OVER`（> 合同额）。边界 `executed == amount` 是 **DONE 不是 OVER**。
5. **币种只显形不改口径**：`currency_mismatch_doc_count` = 计入单据里币种 ≠ 合同币种的行数。
   守卫也不看币种（总额照加），所以报表也不剔除 —— 但让「USD 单挂 CNY 合同」看得见，
   否则执行率被算歪而界面上没有任何异常。
6. **软删合同不进报表**，详情 404（`66014`）。

## 验收（本目录原始输出）

| 文件 | 是什么 | 关键数 |
|---|---|---|
| `01_drill_full.out` | 演练全跑（克隆 145 模板库 → 回放 V0405…V0458 → 结构 → **SQL 直查口径 26 条** → 幂等/回滚 → 12 条真库用例 → 合同模块回归 109 条 → fmt/clippy → 残留） | **PASS=52 FAIL=0**，回放 54 条迁移 0 失败 |
| `02_teeth_skip_guard.out` | 有牙自检：`M419_BAD_URL=1`（连接串故意写错） | rc=101，**12 failed**（真库用例连不上必须红，不许静默 SKIP 记 ok） |
| `03_schema_and_scope.txt` | 两个视图完整定义 + 合同域 6 条菜单 + 3 角色授权 + 模板库存量 11 张合同/4 张单据的报表直查 | 视图列 37 / 12 |
| `04_scope_no_frontend.txt` | 范围守卫：2 条路由 / 2 处权限注解 / 前端 0 命中 / 号段核对（最大 V0458） | — |

复跑：

```bash
# 演练（在 99 上跑，连 145 的 PG；库名含 drill，脚本自己 DROP）
bash scripts/verify/m419_contract_execution_drill.sh
# 有牙自检（期望 rc=1）
M419_BAD_URL=1 SKIP_GATES=1 bash scripts/verify/m419_contract_execution_drill.sh

# 单卡真库用例
HOST=172.18.10.145
PGPASSWORD=*** createdb -h $HOST -U cloudpivot -T cloudpivot_test cp_m419_exec
# 模板库停在 V0404，先回放 V0405…V0458（否则用例会在「列不存在」上假红）
ERP_DB_URL="postgres://cloudpivot:***@$HOST:5432/cp_m419_exec" \
  cargo test --lib business::erp::contract::contract_execution_db_tests -- --test-threads=1
```

## 用例覆盖面（12 真库 + 14 纯逻辑）

真库：新合同全量剩余 / 两张单累加（含税额与净额都对）/ 作废释放 + 下钻原因 / 软删释放（真实删除入口）/
边界正好用满 = DONE / 超合同额（负剩余、率 113%）/ 销售合同只看销售单 / 服务租赁无下游 /
软删合同不可见 + 详情 404 / 币种串了照计但显形 / 总览六类筛选 + OVER 优先排序 / 清场 0 残留。

纯逻辑：剩余保留负数 / 率需要正分母（0 与负数都 None）/ 百分比两位小数 / 执行状态五态与边界 /
下游分派（SERVICE / LEASE 为 None）/ 单据判定先作废后软删 / 标签未知值原样返回 /
入参非法枚举一律 400 / 入参归一化（页码、上限 200、大小写）/ 视图名与迁移逐字一致 / 列清单覆盖 reader ×2。

## 踩过的坑（已写进 skill `cloudpivot-backend-workflow`）

- **`ensure_db()` 静默 SKIP = 假绿**：本仓真库用例的通用写法是连不上就 `return`，cargo 记 ok。
  连接串写错时 `12 passed` 照样出现 —— 必须**数用例打印的标记**（演练第 4 段）或把「配了 URL 却连不上」改成 panic。
  本卡两条都做了（`ensure_db_or_panic` + 标记计数）。
- **夹具清场不能按单号前缀**：本卡的单据走真实入口，单号是生产号段 `PO-…` / `SO-…`；
  清场只能按**夹具合同 id** 反查（合同是固定 UUID），否则每轮残留、用例互顶。
- **物料必须挂基本单位**：`test_seed::ensure_unit()` 挑「第一个启用单位」，模板库里可能是「箱」；
  采购建单会被 `unit_guard` 拒（66030）。用 `ensure_material_base_unit`，且因为
  `upsert_material` 是 `ON CONFLICT DO NOTHING`，早先失败留下的非基本单位要显式 UPDATE 校正。
- **模板库停在 V0404**：不先回放 V0405…V0458，用例会在「列不存在」上红成假故障。
- 视图里的列名要与 model 的 `HEAD_COLUMNS` 逐字一致：`party_b_display_name` 是 handler 算的展示字段，
  **视图里没有**（关键字筛选一开始写了它 → 42703）。

## 给后续卡的事实

1. **合同执行量报表的 UI 还没落地**（本卡零前端）。合同页要展示「执行量」需要另开卡：
   后端接口已就绪（两条），前端只需 `api/business/erp/contract.ts` 加两个调用 + 页面 tab。
2. **守卫与报表必须继续同源**：`committed_amount` 与 `v_contract_execution_order.counted`
   是同一判据的两处实现（一处 Rust、一处 SQL）。改任何一处都必须同时改另一处，
   否则报表与下单校验会分叉 —— 本卡在两侧都写了指针注释。
3. **跨币种是个已知口子**（守卫也不看币种）：本卡只做显形（`currency_mismatch_doc_count`），
   要真正堵住得在守卫里按合同币种折算 —— 那是另一张卡的事，别在报表侧偷偷改口径。
