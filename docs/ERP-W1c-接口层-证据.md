# W1c 证据：11 张 ERP 表的 handler + 权限码 + 单测（接口层）

> 云枢任务 **TSK-202610-000677** · 批次 D · W1c
> 前置：W1a（V0137 13 张表 DDL）✅ / W1b（model + dto，36 单测）✅
> 本文所有数字都来自**真机实跑**（144，`/opt/cloudpivot`），原始输出见同目录
> `ERP-W1c-e2e-原始输出.txt`。

## 一、交付清单

| 文件 | 内容 |
|------|------|
| `backend/src/business/erp/handler.rs` | 新增 3079 行：44 个接口 + 6 个写路径不变量 + 28 条单测（含真库用例） |
| `backend/src/business/erp/mod.rs` | `pub mod handler;` + 模块文档补接口层说明 |
| `backend/src/shared/error_code.rs` | 新增 **66xxx 段**（`66001`–`66006`）ERP 错误码 + 段内自检单测 |
| `backend/migrations/business/V0141__erp_permissions.{up,down}.sql` | 32 个权限码（`sys_menu.auth_code`）+ 2 目录 / 13 菜单 / 19 按钮 + 授权 |
| `scripts/verify/w1c_erp_api.sh` | 真机验收脚本：自建库 + 起隔离实例 + **78 项断言**（接口 / 权限 / EXPLAIN / 残留） |

## 二、接口清单（44 个）

### ① 5 张可写主数据表（25 个接口）

每张表都是 `page` / `{id}` 详情 / `POST` / `PUT` / `DELETE`：

| 表 | 路径前缀 | 权限码 |
|----|----------|--------|
| `biz_warehouse` | `/business/erp/warehouse` | `business:erp:warehouse:{list,detail,create,update,delete}` |
| `biz_location` | `/business/erp/location` | `business:erp:location:{list,detail,create,update,delete}` |
| `biz_material` | `/business/erp/material` | `business:erp:material:{list,detail,create,update,delete}` |
| `biz_unit` | `/business/erp/unit` | `business:erp:unit:{list,create,update}`（3 码制） |
| `biz_supplier` | `/business/erp/supplier` | `business:erp:supplier:{list,create,update}`（3 码制） |

### ② 3 个特殊查询

| 方法 | 路径 | 说明 |
|------|------|------|
| GET | `/business/erp/warehouse/tree?root_id=` | 仓库树：`warehouse_path ~>=~ $1 AND ~<~ $2` 走 `text_pattern_ops`，组装 1–3 层 |
| GET | `/business/erp/location/list?warehouse_id=&include_disabled=` | 按仓库取库位（下拉） |
| GET | `/business/erp/material/list?keyword=&status=` | 物料搜索（编码/名称模糊，上限 100 条） |

### ③ 8 张只读台账（16 个接口）

`account` / `period` / `seq` / `purchase-order` / `purchase-order-item` / `inventory` /
`inventory-location` / `inventory-txn`，每张 `page` + `{id}`，权限码
`business:erp:{entity}:list`（列表与详情共用）。

**为什么这 8 张只读**：库存 / 库存流水 / 采购单是事务与账务表，V0137 §4 头注释写明
「库存与成本的唯一真相来自库存流水」，直接 `POST/PUT` 会让账实不符；写路径属于后续批次的单据编排。

## 三、与卡面的差异（四条，全部以真实 schema / 真实框架行为为准）

1. **卡面点名的 11 张表里 6 张在 V0137 里根本不存在**：`biz_warehouse_location`
   （真实表名 **`biz_location`**）、`biz_material_category`、`biz_customer`、`biz_storage_area`、
   `biz_warehouse_type`（是 `biz_warehouse.warehouse_type` **列**，不是表）、`biz_material_attribute`、
   `biz_material_barcode`。W1a 按架构文档落地的是 13 张表；本卡按**真实存在的 13 张**给接口
   （5 张可写 + 8 张只读），没有伪造不存在的表。
2. **没有 `sys_permission` 表**：`system/menu/model.rs` 文件头明写「`auth_code` 是权限码的唯一来源」，
   登录装载权限读的是 `sys_menu.auth_code`（经 `sys_role_menu`）。V0141 因此只种 `sys_menu` + `sys_role_menu`。
3. **卡面给 unit / supplier 各 3 个码** → 按 3 码制落地：`:list`（含详情）、`:create`、
   `:update`（含删除）。其余 3 张主力表 5 码制。合计仍是卡面的 **24 个码**；`category` 的 3 个码
   作为「物料」菜单下的按钮**预留**（表落地即用，不必再补迁移）。
4. **卡面 24 码覆盖不到真实 13 张表里的 8 张** → 补 **8 个只读码**，合计 **32 码**。
   码值清单在 `handler::CARD_PERMISSIONS` / `handler::READONLY_PERMISSIONS`，
   与 V0141 的 `VALUES` 列表逐字一致（不一致时 V0141 自检与 e2e 都会红）。

另：**迁移编号用 V0141**（卡面要求）。V0138–V0140 仍空着留给 W2/W3；
若它们晚于 V0141 落地，启动日志会出现一条 `待应用迁移的版本低于已应用最大值（乱序补迁）`
**WARN**——不阻断，应用成功后重启即 `应用 0 个迁移`（批次 C · C8 已记录同一现象）。

## 四、写路径的六条不变量（每条都有单测或 e2e 断言）

1. 编码重复 → `409 + 66001`（判据是 `DbError::UniqueViolation` **变体**，
   不是文案 —— `DbError` 的 `Display` 是 `unique constraint violated constraint=…`，
   不含 `duplicate key`，按文案匹配会把唯一冲突判成 500，真库单测当场抓到并改掉）；
2. 弱引用缺失（上级仓库 / 基准单位 / 所属仓库 / 计量单位 / 默认仓库 / 存货科目 / 应付科目）→ `400 + 66002`；
3. 仓库层级：顶级 = 1，下级 = 上级 + 1；超 3 层 → `400 + 66004`；`level` 与上级推导不一致 → `400 + 10001`；
4. 库位只能挂 `is_leaf = 1` 的仓库 → `400 + 66005`；每仓默认库位唯一（置默认时先清同仓其它默认位）；
5. 删除一律软删（`deleted_at`），有下级仓库 / 有库位 / 有物料在用的单位 → `409`；
6. 所有筛选里的 UUID **非法即 400 + 66006**，绝不静默当「没传」（否则「按仓库筛库位」会变成「返回全部库位」）。

## 五、验收原始输出

### ① 全量单测（不连库 / 连库各一遍）

```
===== 3) 全量 lib 单测（不连库）
test result: ok. 1005 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.63s

===== 4) 全量 lib 单测（连 cloudpivot_w1c）
test result: ok. 1005 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 16.05s
```

基线 977（T8-A 交付时）→ 本卡 **+28** = 1005：

- `business::erp::handler::tests` **27 条**（11 条纯逻辑 + 16 条真库 CRUD / 树 / 权限口径）；
- `shared::error_code::tests::erp_error_codes_are_stable` **1 条**（66xxx 段码值稳定）。

真库用例全部实跑（`ERP_DB_URL` 指向一次性库 `cloudpivot_w1c`，跑完 `hard_delete` 清场）：

```
test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 978 filtered out; finished in 5.34s
```

### ② clippy + 门禁

```
===== 2) clippy --all-targets -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 32.99s

===== 5) scripts/check.sh
check.sh exit=0
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）
==> 批次B 门禁：全部通过
==> C7 门禁汇总：G1–G6 共 6 条，未通过 0 条
==> C7 门禁：全部通过
==> 全部检查通过
```

- **闸①**：本文件分页全部走 `model::*_page` → `scope::paginate_unscoped`，没有一处
  `paginate_with_params`（0 处）；
- **闸②**：`handler.rs` 文件头写明 `SCOPE-EXEMPT` 理由（13 张表无 `dept_id`/归属人列，
  数据范围表达不出来），且分页确实走 `scope::`；
- **C7 G1–G6**：ERP 不碰 `sys_change_log`，六条全过。

### ③ 真机 e2e（`bash scripts/verify/w1c_erp_api.sh`）

```
 W1c 验收：PASS=78  FAIL=0
 全部通过
```

覆盖：5 张可写表的建 / 详情 / 分页 / 编辑 / 软删 / 删后 404；3 个特殊查询；
8 张只读台账的 page + 不存在 → 404；重复编码 `409+66001`；第 4 层 `400+66004`；
库位挂非叶子 `400+66005`；物料引用坏单位 `400+66002`；默认库位唯一；
**无权限账号 → 403**（`{"code":403,"msg":"无权限访问，需要权限: business:erp:unit:create"}`）；
V0141 落库 32 码 / 34 节点；V0141 重放幂等 + 超管授权补齐 34 节点；夹具残留 0 行。

### ④ EXPLAIN：仓库树必须走 `text_pattern_ops`

20 万行压测数据（`W1C-BULK-*`，挂在 E2E 总仓下），`PREPARE` + `EXPLAIN (ANALYZE, BUFFERS)`
复刻 `model::warehouse_tree` 的参数化形态（参数化 `LIKE` 拿不到前缀字面量，会退化成全表扫，
所以代码用的是 `~>=~` / `~<~` 这两个 pattern ops 算子）：

```
 Sort  (cost=8.45..8.46 rows=1 width=112) (actual time=0.035..0.036 rows=1.00 loops=1)
   Sort Key: warehouse_path, sort_order
   Sort Method: quicksort  Memory: 25kB
   Buffers: shared hit=10
   ->  Index Scan using idx_biz_warehouse_path_pattern on biz_warehouse
         (cost=0.42..8.44 rows=1 width=112) (actual time=0.022..0.023 rows=1.00 loops=1)
         Index Cond: (((warehouse_path)::text ~>=~ '…/f1000001-…-000000000001/'::text)
                  AND ((warehouse_path)::text ~<~ '…/f1000001-…-0000000000010'::text))
         Filter: (deleted_at IS NULL)
         Index Searches: 1
```

- 命中 `idx_biz_warehouse_path_pattern` ✅
- 计划里**没有** `Seq Scan on biz_warehouse` ✅
- 20 万行下 HTTP `GET /business/erp/warehouse/tree` 仍 200，耗时 14ms

## 六、复跑方法

```bash
# 全量单测（不连库）
cd /opt/cloudpivot/backend && cargo test --lib -j 1

# 真库单测（一次性库）
su - postgres -c 'createdb -O cloudpivot cloudpivot_w1c'
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export NIK_DATABASE_NAME=cloudpivot_w1c NIK_SERVER_LISTEN=127.0.0.1:18091
cd /opt/cloudpivot/backend && setsid nohup ./target/debug/cloudpivot >/tmp/w1c.log 2>&1 </dev/null &
export ERP_DB_URL="postgres://$NIK_DATABASE_USER:$NIK_DATABASE_PASSWORD@127.0.0.1:5432/cloudpivot_w1c"
cargo test --lib -j 1 business::erp

# 真机 e2e（自建库 + 起实例 + 78 项断言；W1C_KEEP=0 跑完删库）
cd /opt/cloudpivot && bash scripts/verify/w1c_erp_api.sh
```

## 七、实测踩到的三个坑（写进代码注释，后续卡不用再踩）

1. **`utils::build_tree` 会静默丢节点**：子树查询时根节点的 `parent_id` 指向集合外，
   节点被塞进 `children_map` 而那个父永不来认领 → 子树返回空数组（真库单测
   `warehouse_tree_returns_all_levels` 当场抓到）。修法：组装前把「父不在集合内」的
   `parent_id` 视为 `None`（提升为根），单测 `subtree_root_with_invisible_parent_is_promoted` 钉住。
2. **`DbError` 的唯一约束不是文案**：`Display` 形如
   `unique constraint violated constraint=uk_biz_unit_code`，按 `duplicate key` 匹配会把
   唯一冲突判成 500。改成 `matches!(err, DbError::UniqueViolation { .. })`，
   顺手把 `CheckViolation`（`23514`）也映射成 `400`（比 500 有用）。
3. **迁移里的 `sys_menu` ID 段要先探**：09xx 段的 `0900/0901/0902` 已被任务看板/列表/模板占掉，
   我第一版撞车 → `ON CONFLICT DO NOTHING` 静默跳过 → 少 2 个权限码。
   ERP 顺延到 **0940–0978**；V0141 的自检块现在会把「应有 32 个码」写成硬断言。
   另：派生表 `JOIN (… UNION …) src` 的列名取自 UNION 第一支（`role_id`），
   写成 `src.id` 会直接报 `column r.id does not exist`。
