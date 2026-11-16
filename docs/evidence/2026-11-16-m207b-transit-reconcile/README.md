# M2-07b 在途对账页 — 证据

> 卡：`t_4e93fa53`（M2-07b【M2·仓库】在途对账：前端页面（差异行 + 汇总））
> 结论：在既有页面 `/business/erp/inventory-transit` 上**加第三个 Tab「在途对账」**，
> 消费 M2-05 早已落地、此前**没有任何页面消费**的 `GET /business/erp/inventory-transit/reconcile`。
> **零迁移、零后端改动、零新权限码**：Tab 显隐直接用 V0422 的对账码 `business:erp:inventory-transit:audit`，
> 所以走的是卡面「入口 1」而不是另开独立页（另开页要再种页面显形码 + 菜单行 + 授权成对 + 台账登记）。

## 1. 交付物

| 文件 | 作用 |
|------|------|
| `frontend/src/views/business/erp/inventory-transit/index.vue` | 第三个 Tab（筛选行 / 结论横幅 / KPI / 差异表 / 三种空态）；行内「发货单号」可点进发货单 |
| `frontend/src/views/business/erp/inventory-transit/transit-logic.ts` | 对账纯逻辑：拼参（`include_matched` 只在勾上时发 `'true'`）、差异码主题、结论文案（**只取后端 `issue_labels`**）、三态空态、结论横幅、截断提示、403 人话 |
| `frontend/src/api/business/erp/inventory-transit.ts` | `TransitReconcile` / `TransitReconcileRow` / `TransitReconcileTotals`（逐字抄 DTO）+ `fetchTransitReconcile` |
| `frontend/src/views/business/erp/shipment/index.vue` | 接深链 `?shipment_id=<uuid>` → `openDetailById`（对账页点发货单号直接开那张单的抽屉）；列表行为不变 |
| `frontend/tests/unit/m207b-transit-reconcile.test.ts` | **36 条**单测：纯逻辑 21 条 + 源码契约 15 条（含「页面零 reduce / 不自己相减」） |
| `frontend/tests/unit/m207-transit-page.test.ts` | 2 处契约随本卡更新（对账码由「本页不消费」改为「只用来判第三个 Tab 显隐」；Tab 声明改为 `TRANSIT_TABS` 2 个常驻 + `RECONCILE_TAB`） |
| `scripts/verify/m207b_reconcile_fixture.sql` | 真机夹具：**六种对账形态**一次造齐（对平 / QTY_MISMATCH / MISSING_TRANSIT / CANCELLED_ONLY / ORPHAN_TRANSIT / MULTI_WAREHOUSE） |
| `scripts/verify/m207b_reconcile_noaudit_user.sql` | 只读角色（有页面码 + 取数码、**没有对账码**）+ 账号，用于证明「Tab 按码显隐、不整页 403」 |
| `scripts/verify/m207b_reconcile_drill.sh` | 库侧 21 条断言：权限形状未被本卡动过 + 夹具口径 + 只读角色授权面 |
| `scripts/verify/m207b_api_check.py` | 真接口 44 条断言（含 `include_matched`、筛选、4 例非法参数、匿名 401、只读角色 403） |

## 2. 权限口径：对账码只判 Tab 显隐，不判整页

| 码 | 挂在哪 | 管什么 | 本卡动作 |
|----|--------|--------|----------|
| `business:erp:inventory-transit:page` | V0423 页面行 `…4230` | 侧栏显形 + 路由 `meta.permission` | 不动 |
| `business:erp:inventory-transit:list` | V0421 按钮行 `…4210`（父=即时库存） | 明细 / 汇总接口 | 不动 |
| `business:erp:inventory-transit:audit` | V0422 按钮行 `…4220`（父=即时库存） | 对账接口 **+ 本卡第三个 Tab 的显隐** | 不动（只在前端消费） |

* `canAudit = hasPermission('business:erp:inventory-transit:audit')` **只挂在第三个 `t-tab-panel` 的 `v-if` 上**
  （单测断言整页模板里 `canAudit` 只出现 1 次、且不存在 `v-if="!canAudit"` 的整页分支）。
* 没有该码的角色：**看不到 Tab，但页面照常能开、明细与汇总照常能查**（真机 `04_no_audit_tab_hidden.png`；
  HTTP `06_api_check.out` 末段：只读角色的 `me.permissions` 有 `:page`/`:list`、无 `:audit`，`/page` 200、`/reconcile` 403）。
* 为什么不做整页 403：对账是「出结论」的授权面（V0422 文件头写的就是「看数的人不一定该出结论」），
  它不该把整个在途页锁死。

## 3. 页面口径：**页面不算账**

| 数 | 来源 | 页面做什么 |
|----|------|-----------|
| 发货总量 `item_qty` | 发货单明细 `Σqty`（SSOT，后端 `reconcile_sql` 的 items 侧） | 只格式化 |
| 已出 `received_qty` / 在途 `qty_in_transit` | 在途账（`qty_in_transit` 是 PG 生成列） | 只格式化 |
| 差异量 `diff` | 后端 `ComboRow::diff()` = `item_qty − (received + in_transit)` | 只格式化（`diffText`） |
| 汇总 `totals.*`（checked / matched / mismatched / cancelled / untracked / 三侧合计 / diff） | 后端 `reconcile::summarize` | 只渲染 |
| 结论中文 | 后端 `issue_labels`（`issue_label` 逐字） | 只拼接（`；`），前端**不内置任何差异文案** |
| 判定（对平 / 不平 / 整批作废） | 由 `matched` + `issues`（`CANCELLED_ONLY` 单独算「整批作废」，与后端 `mismatched` 口径一致） | 上色 |

单测里三条硬断言（照 M2-07 的路子）：页面源码**无 `.reduce(`**、**无 `item_qty - …` 之类的自减**、
结论文案里**不出现后端 `issue_label` 的字面量**（抄了就会漂）。
另外一条反面用例：`totals` 故意不自洽（`item_qty=999`、`diff=997`）时横幅仍显示 999 / 997 —— 证明它在读字段，而不是在算。

## 4. 三种空态分开写（卡面要求 2）

| 形态 | 标题 | 判据 |
|------|------|------|
| 还没跑过 | 「还没有对账结果」 | `report === null` |
| 筛选下无差异 | 「当前筛选下没有差异组（N 组全对平）」/「当前筛选下没有可对账的组」 | 有筛选条件 |
| 全部对平 | 「全部对平（N 组）」+ 「这一页空着是好消息，不是没数据」 | 无筛选、`checked > 0`、`mismatched === 0` |
| 没有可对账的单 | 「还没有可对账的发货单」（说明只覆盖 POSTED 单） | 无筛选、`checked === 0` |

真机 `02_reconcile_filtered_matched.png` 是「筛选到一张全对平的单」时的样子（绿色横幅 + KPI 1/1/0/0/0 + 空态）。

## 5. 门禁与实测（全部真实输出）

### 5.1 门禁（`08_gates.out`）

| 门禁 | 结果 |
|------|------|
| `cd frontend && pnpm typecheck` | rc=0 |
| `cd frontend && pnpm typecheck:unit` | rc=0 |
| `cd frontend && pnpm build` | 成功（rc=0） |
| `cd frontend && pnpm test:unit`（全量） | **2043 / 2043 pass**（本卡新增 36 条） |
| 本卡两份契约单测 | 68 / 68 pass |
| `node scripts/shell-variant-report.cjs --baseline …` | **无新增页面壳类声明**（按 M2-07 的教训：工具行复用 `shell.less` 的 `.tool-row`，新样式一律 `transit-*` 自有命名） |
| `cd backend && cargo clippy --lib --all-targets -- -D warnings` | rc=0（本卡 backend **0 行改动**） |
| `cd backend && cargo fmt -- --check` | **rc=1，但 `Diff in` 全部落在 `inventory_aging/*` 与 `erp/mod.rs`** —— 那是**另一张卡（M2-08 库龄）在途的未提交文件**，本卡 `git diff` 里 `backend/` 为空。不越界去格式化别人的在途代码 |

### 5.2 真接口（44 条断言，`06_api_check.out`）

跑法：99 上起隔离实例（`backend/target/debug/cloudpivot`，库 `cp_m207b_reconcile@172.18.10.145`，
`NIK_SERVER_LISTEN=127.0.0.1:18207`），灌夹具 + 只读角色后 `python3 scripts/verify/m207b_api_check.py`：

* 空库首启 `must_change_password=true` → 改密 → 重新登录（可重跑）；
* 菜单树含 `/business/erp/inventory-transit`（auth_code 是页面码），超管权限走 `*:*:*` 短路；
* `/reconcile` 缺省 → `checked=6 matched=1 mismatched=4`、`rows=5`（对平的 SH1 被滤掉）、`balanced=false`、
  `truncated=false`、`scan_limit=2000`；
* 六种形态逐行核对：`QTY_MISMATCH`（diff −5）/ `MISSING_TRANSIT`（warehouse_id 为 null）/ `CANCELLED_ONLY`
  （`cancelled_qty=10`）/ `ORPHAN_TRANSIT`（`item_qty=0`）/ `MULTI_WAREHOUSE`（唯一码）/ 对平组不在缺省结果里；
* `totals`：`item_qty=180`（只算已纳入在途账的组）、`untracked_item_qty=20`、`diff=-12`；
* `include_matched=true` → 6 行（含对平组），**`totals` 不变**（口径不随返回集合变）；
* 筛选：`shipment_id` 命中 1 组；日期区间外 → 0 组（200，不是报错）；
* 非法参数 4 例（`include_matched=maybe` / 非法 UUID / 非法日期 / 区间反了）→ **400 + 66006**；
* 匿名 → 401；只读角色（有 `:list` 无 `:audit`）→ `/page` 200、`/reconcile` **403**。

### 5.3 库侧（21 条断言，`07_drill.out`）

`bash scripts/verify/m207b_reconcile_drill.sh cp_m207b_reconcile` → **PASS=21 FAIL=0**：
对账码/页面码/取数码各有且仅有 1 个承载节点、类型与父节点未变（本卡没动权限表）；
持取数码的角色都持对账码（V0422 成对授予仍成立，只读验收角色除外）；
夹具六种形态的那几个关键数（组合数 6、明细 Σ210、非作废在途 152、已签 40、作废 10、SH6 两仓、SH3 无在途行）。

### 5.4 真机（Vite dev + 真后端，1440×900）

| 证据 | 内容 |
|------|------|
| `00_reconcile_diffs.png` | 第三个 Tab：筛选行（发货单/物料/仓库/日期区间/连对平的组）+ 红色结论横幅（有 4 组不平）+ 9 个 KPI + 5 行差异（含 `QTY_MISMATCH` 的 −5、`MISSING_TRANSIT` 的「—（无在途行）」、`MULTI_WAREHOUSE` 的 0 差异） |
| `01_reconcile_include_matched.png` | 勾「连对平的组一起看」→ 6 行，对平组 `M207B-SH1` 以绿色「对平」出现 |
| `02_reconcile_filtered_matched.png` | 筛到 `M207B-SH1` → 绿色「全部对平」横幅 + KPI 1/1/0/0/0 + 空态「当前筛选下没有差异组（1 组全对平）」 |
| `03_reconcile_matched_row.png` | 同筛选 + `include_matched` → 单行对平（100 = 40 + 60，差值 0） |
| `04_no_audit_tab_hidden.png` | **只读角色**登录：只有「在途明细 / 在途汇总」两个 Tab、页面正常出 5 行明细、DOM 里没有对账卡片 |
| `05_shipment_deeplink.png` | 点「M207B-SH2」→ `/business/erp/shipment?shipment_id=…` 并直接打开该单详情抽屉（状态线、三本账已扣、明细 50） |

截图里的数都是夹具的：明细 Σqty 210、已纳入在途 180/40/152、差额 −12。

## 6. 已知形态 / 诚实缺口

1. **对账接口按 `scan_limit=2000` 组扫描**：`truncated=true` 时结论只覆盖前 2000 组，页面给明确提示
   （并说明**合计与差额同样只统计已扫描组** —— 这点与「在途汇总」Tab 的 `total_*` 是全量不同，两处口径不一样，
   所以 `reconcileTruncatedNote` 与 `truncatedNote` 是两份文案）。
2. **`M2-02` 之前在途账未上线时过账的历史单**会以 `MISSING_TRANSIT` 出现（后端 DTO 注释里写明的既有口径）；
   页面不吞这类行，但也不做「历史单白名单」——需要按发货日期区间收窄来看新账。
3. **发货单深链是新增的一小段行为**（`shipment/index.vue` 的 `openDetailById` + `route.query.shipment_id`）：
   只影响「带参数进来」的场景，不带参数的列表行为一字未变（`openDetail(row)` 仍走 `openDetailById(row.id)`）。
4. **真机验收用空库首启 + 夹具**，不是 144 生产数据；生产上的在途/对账差异由 M2-02/M2-03 的真实链路产生。
5. **演练库已 DROP**（`cp_m207b_reconcile`），复现按 §7 重建。
6. 本卡**没有**新增权限码 / 菜单行 / 迁移 —— 因此 `permission_audit_tests`、`menu_invariants` 与本卡无关，
   也没跑（M2-07 那次跑的结论仍成立于同一批菜单行）。

## 7. 复现（99 开发机；库只在 145）

```bash
# ① 空库（名字必须含 cp_/drill/e2e/test 前缀，本脚本用完自己 DROP）
dropdb --if-exists -h 172.18.10.145 -U cloudpivot cp_m207b_reconcile
createdb -h 172.18.10.145 -U cloudpivot cp_m207b_reconcile

# ② 起隔离后端（main 代码编出来的 debug 二进制；迁移与种子自己跑）
cd backend && NIK_PROFILE=test \
  NIK_DATABASE_HOST=172.18.10.145 NIK_DATABASE_NAME=cp_m207b_reconcile \
  NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD=*** \
  NIK_SERVER_LISTEN=127.0.0.1:18207 \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  NIK_ADMIN_PASSWORD='Admin@123456' ./target/debug/cloudpivot &

# ③ 夹具（六种对账形态）+ 只读角色（无对账码）
psql -h 172.18.10.145 -U cloudpivot -d cp_m207b_reconcile -v ON_ERROR_STOP=1 \
     -f scripts/verify/m207b_reconcile_fixture.sql
psql … -f scripts/verify/m207b_reconcile_noaudit_user.sql   # 须在 admin 改密之后跑（拷 admin 的口令哈希）

# ④ 真接口 / 库侧
python3 scripts/verify/m207b_api_check.py                   # 44 条，需先跑过一次登录改密
bash scripts/verify/m207b_reconcile_drill.sh cp_m207b_reconcile

# ⑤ 真机
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18207 ./node_modules/.bin/vite --port 5189 --strictPort
#    浏览器打开 http://127.0.0.1:5189/business/erp/inventory-transit → 第三个 Tab
#    admin / M207b@Reconcile1（超管）    m207b_noaudit / M207b@Reconcile1（只读，看不到第三个 Tab）

# ⑥ 清场
dropdb -h 172.18.10.145 -U cloudpivot cp_m207b_reconcile
```
