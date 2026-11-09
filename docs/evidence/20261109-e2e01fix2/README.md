# E2E01-FIX-2【P4】采购明细列表加关键字搜索 —— 真机证据（t_627c49bc）

> 仓库：`/opt/cloudpivot`（99 = 工作仓）· 真机：`172.18.10.145`（`cloudpivot-test.service` · `:8033` · 库 `cloudpivot_dev`）
> 代码 HEAD / 部署 HEAD = **41b6a205**（`git ls-remote origin refs/heads/main` 与本地一致）
> 采集时间：2026-11-09 16:16–16:30（CST）· 原始输出见 `raw-145/`、`raw-99/`

---

## 0. 一句话结论

`GET /business/erp/purchase-order-item/page` 新增 `keyword`：**一次 LEFT JOIN 单头 + 物料**，
`ILIKE '%…%'` 命中 **单号 / 物料编码 / 物料名称** 任一列；空 / 空白与不传**完全等价**（全量）。
前端采购明细页 toolbar 加搜索框（回车或「查询」触发，清空即恢复全量）。
145 真机验收脚本 **PASS=24 / FAIL=0**，卡面三个字面关键词（`PO-2026` / `DEMO-P1` / `物料P1`）
接口 total 与「同谓词 SQL 真值」逐一对上（11 / 8 / 5）。

| # | 卡面验收 | 结论 | 证据 |
|---|---|---|---|
| ① | `?keyword=PO-2026` 只返回该单明细 | ✅ 11 行 / 10 张单，SQL 真值 11 | §2 `card_po2026.json` |
| ② | `?keyword=DEMO-P1` 按物料编码命中 | ✅ 8 行，SQL 真值 8 | §2 `card_demop1.json` |
| ③ | `?keyword=物料P1` 按名称命中 | ✅ 5 行 / 1 个物料，SQL 真值 5 | §2 `card_wuliao.json` |
| ④ | 大小写混合（`demo-p1`）也命中 | ✅ `demo-p101` 与 `DEMO-P101` 结果一致（5 行） | §3 `kw_lower.json` |
| ⑤ | 空 keyword = 全量 | ✅ 不传 / 空串 / 3 空格 都是 15 行 | §3 |
| ⑥ | 前端搜索框可用 | ✅ 构建产物 + 145 dist 已换，入口 chunk 引用新页面 chunk | §4 |
| ⑦ | cargo test + 前端 build | ✅ 3928 passed（干净 HEAD）/ 真库 26 passed / typecheck+build 通过 | §5 |

---

## 1. 后端改法（`model_order.rs::purchase_order_item_page`）

```sql
SELECT i.id, … , (…POSTED_RECEIVED_SQL…) AS received_qty
  FROM biz_purchase_order_item i
  LEFT JOIN biz_purchase_order o ON o.id = i.order_id     -- 只在带 keyword 时才 join
  LEFT JOIN biz_material       m ON m.id = i.material_id
 WHERE 1 = 1
   [AND i.order_id = ?] [AND i.material_id = ?]
   AND (o.order_no ILIKE ? OR m.material_code ILIKE ? OR m.material_name ILIKE ?)
 ORDER BY o.order_no ASC, i.line_no ASC                    -- 带 keyword 时；不带则保持 i.line_no
```

四个决定（都在函数 doc 里写清了理由）：

1. **一次 join 完**，不逐行回查 —— 两张表都按主键 join，一对多不存在，
   `scope::paginate` 的 `SELECT COUNT(*) FROM (…base_sql…)` 口径不变（count 不虚高）。
2. **`LEFT JOIN` 不是 `INNER`** —— 单头 / 物料被软删时明细行仍要出现在「不带关键词」的全量里，
   关键词只是多一种筛选，不该改变「哪些明细可见」。
3. **`ILIKE` + `%kw%`** —— 大小写不敏感；模式串走 `utils::like_pattern`，
   空白 / `None` 返回 `None` ⇒ **一个条件都不加**（与加参数前逐字一致，不是 `ILIKE '%%'`）。
4. **排序**：带 keyword 时 `ORDER BY o.order_no, i.line_no`（跨单搜索要成组可读）；
   不带时维持 `ORDER BY i.line_no`（老口径、翻页顺序不变）。

DTO：`PurchaseOrderItemFilter` 加 `keyword: Option<String>`，`Default` 同步（`None`），
`#[serde(default)]` 保证老客户端不传也能解析。

---

## 2. 卡面字面关键词实测（145 · 真实 token）

原始输出：`raw-145/card_kw.txt`，报文：`raw-145/card_*.json`

```text
== 卡面字面关键词实测（145 · cloudpivot_dev）
po2026         kw=PO-2026    http=200 total=11 行=11 单号种类=10 物料种类=3
demop1         kw=DEMO-P1    http=200 total=8  行=8  单号种类=7  物料种类=2
wuliao         kw=物料P1    http=200 total=5  行=5  单号种类=5  物料种类=1

== 同一关键词的 SQL 真值（ILIKE 同谓词）
 kw      | count
---------+-------
 PO-2026 |    11
 DEMO-P1 |     8
 物料P1  |     5
```

- `PO-2026` 是**单号**子串（`PO-20261107-000001` 那一批）→ 命中 10 张单 11 行明细；
- `DEMO-P1` 是**物料编码**子串（`DEMO-P101`）→ 8 行；同名前缀的 `DEMO-P102` 不含 `P1`，正确地没带出来；
- `物料P1` 是**物料名称**子串（`采购演练物料P1`）→ 5 行，且 `material_id` 只有 1 种。

三条的接口 `total` 与库上同谓词 SQL **一模一样**，说明不是「碰巧行数对上」。

---

## 3. 六条口径的正规验收脚本（`scripts/verify/e2e01fix2_keyword.sh`）

设计要点：**期望值全部用与接口同一个 ILIKE 谓词从库里现算**（psql），
不写死数字 —— 这样数据一变脚本仍有效，也不会出现「脚本与接口一起错」的假绿。

```text
== E2E01-FIX-2 采购明细 keyword 搜索 · 145 真机（2026-11-09 16:16:57）
   BASE=http://127.0.0.1:8033/api   DB=cloudpivot_dev   HEAD=41b6a205
   登录成功（admin · http=200）

-- ⑤ 无 keyword = 全量（与库里 15 行一致）
  PASS  http(无 keyword) = 200
  PASS  total(无 keyword) = 15
  PASS  total(keyword=空串) = 15
  PASS  total(keyword=3 空格) = 15

-- ① keyword=P1FIX-PO-1（该单 2 行；ILIKE 真值 2 行 / 1 张单）
  PASS  total(单号) = SQL 真值 = 2
  PASS  命中行的 order_id 种类数 = 1
  PASS  命中行的 order_id = d2a30001-0000-4000-8000-000000000021

-- ② keyword=DEMO-P101（该物料 5 行；ILIKE 真值 5 行 / 1 个物料）
  PASS  total(编码) = SQL 真值 = 5
  PASS  命中行的 material_id 种类数 = 1

-- ③ keyword=采购演练物料P1（该名称 5 行；ILIKE 真值 5 行 / 1 个物料）
  PASS  total(名称) = SQL 真值 = 5

-- ④ keyword=demo-p101（小写编码，ILIKE 必须同样命中）
  PASS  total(小写) 与 ② 一致 = 5
  PASS  用例有效：小写 != 原编码（demo-p101 vs DEMO-P101）

-- ⑥ keyword=P1FIX-PO-1 + order_id=（另一张单）→ 0 行
  PASS  total(AND 冲突) = 0
  PASS  list 长度(AND 冲突) = 0

-- 反向：keyword=zz-no-such-material → 0 行（不是全量、不是 500）
  PASS  total(未命中) = 0

-- 回归：?order_id=<原单> 仍按原口径（总数 = 该单明细数）
  PASS  total(order_id) = 2

== PASS=24 FAIL=0
```

完整汇总：`raw-145/summary.txt`；报文：`raw-145/kw_*.json`。

---

## 4. 前端：搜索框 + 构建产物

改动：

- `views/business/erp/purchase-order-item/index.vue`：toolbar 加
  `t-input`（placeholder「搜索单号 / 物料编码 / 物料名称」，`clearable`，`@enter` 触发查询，
  `@clear` 走 `handleKeywordClear` 显式归零后重查 —— 不依赖 `clear` 与 `update:modelValue` 的先后顺序）；
  空态文案改成「换个关键词、采购单或物料再试」。
- `purchase-order-item-logic.ts`：`ItemFilterState` 加 `keyword`，`buildItemQuery` 对 keyword 做
  **trim 后为空则不发送**（与后端 `like_pattern` 同口径）；`isItemFiltered` 把 keyword 计入。
- `api/business/erp/index.ts`：`fetchPurchaseOrderItemPage` 参数加 `keyword?`。

构建与部署（145 nginx `root /opt/cloudpivot/frontend/dist`）：

```text
入口 chunk           entry=index-CStkxsjl.js
新页面 chunk         dist/assets/index-C3SwNuON.js
含 item-filter-keyword 次数                      1
含「搜索单号 / 物料编码 / 物料名称」次数           1
入口是否引用该 chunk                              2（manifest + import）
nginx 取到的入口                                 src="/assets/index-CStkxsjl.js"   ← 与 dist/index.html 一致
```

**未做**：浏览器点击走查。本环境禁止用自动化工具输入登录口令，所以没有「登录 → 点搜索」的截图。
替代证据：单测锁定 `buildItemQuery`（trim / 空白 / 与 id 组合）、构建产物里确有该输入框与
`data-testid="item-filter-keyword"`、145 已换 dist 且入口引用新 chunk、接口侧用真实 token 全量验过。

---

## 5. 测试与检查链

| 项 | 结果 | 日志 |
|---|---|---|
| `cargo test --lib`（干净 HEAD 41b6a205 的 worktree） | **3928 passed / 0 failed** | `raw-99/cargo_test_lib_clean.txt` |
| 真库用例 `business::erp::model::tests`（`ERP_DB_URL=…/cloudpivot_e2efix8`） | **26 passed**（17.55s，夹具 + 关键词用例都真跑） | `raw-99/cargo_db_model.txt` |
| 新增用例 | `dto::tests::purchase_order_item_filter_keyword_is_optional`、`model::tests::purchase_order_item_page_keyword_matches_no_code_and_name` | 同上 |
| 前端 `tsx --test`（采购明细 + 单号序列逻辑） | **16 passed / 0 failed** | `raw-99/fe_unit.txt` |
| `pnpm typecheck` | 通过（vue-tsc 无输出） | `raw-99/fe_typecheck.txt` |
| `pnpm build` | 通过（dist 产出，见 §4） | `raw-99/fe_build.txt` |
| `rustfmt +1.94.1 --check`（三个改动 .rs） | 干净 | — |

### ⚠ 关于工作区里那条红色（不是本卡）

共享工作区 `/opt/cloudpivot` 当时有**别的 worker 未提交的附件模块改动**
（`attachment/{dto,handler,model,tests}.rs` + `V0362__doc_attachment_file_name.*`），
在**脏工作区**跑全量会出现：

```text
test business::attachment::tests::file_name_snapshot_is_read_before_sys_file_name ... FAILED
test result: FAILED. 3931 passed; 1 failed
```

该用例来自上述未提交改动（本卡 7 个文件不含 `attachment/**`），日志见
`raw-99/cargo_test_lib_dirty_workspace.txt`。为给出可复核的结论，
在 `git worktree`（detached at 41b6a205，纯 HEAD）里重跑全量 → **3928 passed / 0 failed**
（`raw-99/cargo_test_lib_clean.txt`，复用同一 `CARGO_TARGET_DIR`，只重编 `cloudpivot` 这个 crate）。

---

## 6. ③ 同类页面一起检查（不在本卡范围内改）

**结论：明细级（跨单据行）分页接口全系统只有 `purchase-order-item/page` 一个。**

```text
$ grep -rh 'nik::get("' backend/src/business/erp/*.rs | grep -i item
"/business/erp/purchase-order/{id}/pending-items"     ← 单内待收明细（按单，不是跨单列表）
"/business/erp/purchase-order-item/{id}"              ← 明细详情
"/business/erp/purchase-order-item/page"              ← 本卡改的这个
"/business/erp/report-item/{catalog,list}"            ← 报表条目，无关
```

其余单据的明细都随**单头详情**一次返回（`GET /stock-in/{id}`、`/stock-out/{id}`、
`/sales-order/{id}`、`/stock-take/{id}`、`/transfer/{id}` …），页面没有「跨单明细列表」，
所以不存在「明细列表缺关键词」这个缺口。

**已有关键字搜索的列表页（现状口径）**：

| 列表页 | 参数 | 匹配列 | 大小写 |
|---|---|---|---|
| 采购订单 `purchase-order` | `keyword` | `order_no` / `remark` | **LIKE（敏感）** |
| 采购明细 `purchase-order-item` | `keyword` | `order_no` / `material_code` / `material_name` | **ILIKE（本卡）** |
| 入库单 `stock-in` | `keyword` | `in_no` / `remark` | LIKE（敏感） |
| 出库单 `stock-out` | `keyword` | `out_no` / `reason` / `remark` | LIKE（敏感） |
| 销售订单 `sales-order` | `so_no`（专用，不叫 keyword） | `so_no` | LIKE（敏感） |
| 库存流水 `inventory-txn` | 无 keyword | 只有 `doc_no` 精确 + 物料/仓库下拉 | — |

**建议（另开卡，不在本卡改）**：

1. **统一大小写口径**：把上面四处的 `LIKE` 改成 `ILIKE`。否则同一个编码 `demo-p1` 在
   「采购明细」能搜到、在「采购订单 / 入库 / 出库」搜不到 —— 用户不会知道这是两张表的差异。
2. **`sales-order` 的参数名对齐**：`so_no` → 兼容一个 `keyword` 别名（或反过来），
   前端各页筛选字段名不一致，二开时容易照抄错。
3. **若要「某物料进过哪几批」**：现在只能进每张单详情看。真要做，建议新增
   `stock-in-item/page`（照本卡这个 join 写法）或给库存流水页加 `keyword`（物料编码/名称 + 单号），
   后者更省事（流水本来就一行一条出入库）。
4. **采购明细的关键词搜索有显示上限**：列表 DTO 只返 `order_id` / `material_id`，
   页面靠「最新 200 条主数据」建 Map 反查单号与物料名；关键词命中全表时，
   200 条之外的行的单号/物料名会显示成短 id（已在页面注释里写明）。要彻底解决，
   需要明细列表 DTO 一并下发 `order_no` / `material_code` / `material_name`。
