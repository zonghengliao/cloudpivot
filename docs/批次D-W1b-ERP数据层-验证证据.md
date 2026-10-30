# 批次 D · W1b：13 张 ERP 表的 Rust model + dto（数据层）——验证证据

> 云枢任务：TSK-202610-000676（W1b）；父任务 W1（TSK-000673）；前置 W1a（V0137 已部署生产）
> 交付范围：`backend/src/business/erp/{mod,model,dto}.rs`，**不含 handler**（W1c）
> 验收脚本：`scripts/verify/w1b_erp_data_layer.sh`（REPO 参数化，自建/自毁一次性库，15 项断言）

## 0. 一句话结论

13 张表的行结构 / 列清单 / 基础查询 / DTO 契约全部落地，**36 条单测**（21 条真库 + 15 条纯逻辑）真跑全绿，
`cargo test --lib` 931 → **967**，`clippy -D warnings` / `cargo fmt` / `scripts/check.sh`（退出码 0）全过；
仓库子树查询用 `~>=~ / ~<~` 命中 `idx_biz_warehouse_path_pattern`（EXPLAIN 实证，参数化 `LIKE` 用不上这个索引）。

## 1. 交付物

| 文件 | 行数 | 内容 |
|------|------|------|
| `backend/src/business/erp/model.rs` | 2908 | 13 个 Row 结构 + 13 个 `*_COLUMNS` + 30 个查询函数（含 `warehouse_tree` / `inventory_balance` 两级账）+ 22 条真库单测 |
| `backend/src/business/erp/dto.rs` | 3419 | 13 张表的建 / 改 / 筛选 / 卡片 / 详情（64 个 DTO）+ `validate` 校验 + 14 条纯逻辑单测 |
| `backend/src/business/erp/mod.rs` | 27 | 模块文档（W1c 接手须知） |
| `backend/Cargo.toml` / `Cargo.lock` | +7 行 | 打开 `nik` 的 `rust_decimal` feature + 直接声明 `rust_decimal 1.43`（只为开 serde） |
| `backend/src/business/mod.rs` | +2 行 | 注册 `pub mod erp;` + 模块表一行 |
| `scripts/verify/w1b_erp_data_layer.sh` | 175 | 验收脚本（建库 → 迁移 → 36 条单测 → EXPLAIN → 门禁复查 → 删库） |

## 2. 与卡面文字的四处差异（全部以 `information_schema` / 迁移实测为准）

| # | 卡面写的 | 库里实际 | 处置 |
|---|----------|----------|------|
| 1 | 仓库路径列 `path`，索引 `idx_biz_warehouse_path` | 列是 **`warehouse_path`**；`text_pattern_ops` 索引是 **`idx_biz_warehouse_path_pattern`**（卡面那个名字是默认 collation 那条，前缀匹配用不上） | 代码用真名；`warehouse_tree` 改走 `~>=~ / ~<~` 才吃得到该索引（见 §4.3） |
| 2 | `biz_material` 含 `serial_required` / `external_sn`，可按它筛选 | V0137 文件头写明「`serial_required` 属 **V0138**」，`information_schema` 实测无此二列 | `MaterialRow` 不含该字段；`material_page` 的 `serial_required` 筛选**先探测列是否存在**：不存在 → `400 + 10001` 并点名 `serial_required`（不静默返回全量）；存在 → 追加 `AND serial_required = ?`，V0138 落地后零改动生效（两条分支都有用例覆盖） |
| 3 | `unit_find_by_id(ex: &Transaction, …)` / `unit_page(ex: &Transaction, …)` | 本仓约定：**读走全局 `db()`，写才吃 `&Transaction`**（`business/task/model.rs`、`business/project/model.rs` 同款） | 照仓内约定实现；否则每个 GET 都得开事务，W1c 反而更难写。差异写进 `model.rs` 模块文档第 3 条 |
| 4 | `XxxFilter` 的 id 字段类型未指明 | DTO 的 id 一律 `String`（`Uuid` 没开 serde feature） | `model.rs` 里对 `String → Uuid` 做显式解析（`parse_id`），非法 UUID 直接 `400` 而不是**静默丢掉筛选条件**（丢掉会让「按仓库筛库位」退化成「全部库位」） |

## 3. 红线逐条

| 红线 | 结果 | 证据 |
|------|------|------|
| ① 不写 handler | ✅ | `erp/` 下只有 `mod.rs` / `model.rs` / `dto.rs`，无 `handler.rs`、无 `axum` 依赖 |
| ② 不改 V0137 迁移 | ✅ | `git status` 里 `migrations/` 无改动 |
| ③ 不改现有模块 | ✅ | 仅 `business/mod.rs` 加 2 行（注册模块，红线允许的最小接线段）；`task/` `flow/` `system/` 零改动 |
| ④ `cargo test --lib` 全绿 | ✅ | `967 passed; 0 failed`（931 基线 + 36 条新用例） |
| ⑤ `clippy --all-targets -- -D warnings` | ✅ | `Finished dev profile`，零 warning |
| ⑥ `cargo fmt` 干净 | ✅ | `cargo fmt -- --check` 通过（`check.sh` 第一步） |
| ⑦ `bash scripts/check.sh` 退出码 0 | ✅ | `CHECK_SH_EXIT=0`（fmt / clippy / check / pnpm typecheck / 闸①闸② / C7 G1–G6 / 色板全过） |
| ⑧ `biz_` 查询接 `scope::paginate` 或 `SCOPE-EXEMPT` | ✅ | 13 个列表统一走 `page_of` → `scope::paginate_unscoped`（唯一入口 1 处）；模块头写了 `SCOPE-EXEMPT` 理由（13 张表都是全局主数据 / 仓储结构，V0137 无 `dept_id` / 归属人列，注入范围只会静默少数据） |

## 4. 原始输出

### 4.1 全量 lib 单测（`ERP_DB_URL` 指向一次性库 `cloudpivot_w1b`）

```
$ cd /opt/cloudpivot/backend && ERP_DB_URL=postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_w1b \
  CARGO_TARGET_DIR=/root/w1b-target cargo test --lib -j 1
test result: ok. 967 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 11.31s
EXIT=0
```

其中 `business::erp` 36 条（`grep -cE "^test business::erp" → 36`），21 条真库用例全部实跑、无 SKIP：

```
$ ERP_DB_URL=… cargo test --lib business::erp -- --test-threads=1
test business::erp::model::tests::unit_row_maps_key_field ... ok
test business::erp::model::tests::warehouse_row_maps_level_and_path ... ok
test business::erp::model::tests::location_row_maps_warehouse_and_default ... ok
test business::erp::model::tests::material_row_maps_type_and_tax ... ok
test business::erp::model::tests::supplier_row_maps_credit_limit ... ok
test business::erp::model::tests::account_row_maps_direction_and_leaf ... ok
test business::erp::model::tests::period_row_maps_code_and_status ... ok
test business::erp::model::tests::seq_row_maps_current_no_and_step ... ok
test business::erp::model::tests::purchase_order_row_maps_amounts_and_currency ... ok
test business::erp::model::tests::purchase_order_item_row_maps_qty_and_received ... ok
test business::erp::model::tests::inventory_row_maps_avg_cost ... ok
test business::erp::model::tests::inventory_location_row_maps_qty_per_location ... ok
test business::erp::model::tests::inventory_txn_row_maps_direction_and_qty ... ok
test business::erp::model::tests::warehouse_tree_returns_three_levels_in_path_order ... ok
test business::erp::model::tests::warehouse_subtree_excludes_sibling_root ... ok
test business::erp::model::tests::warehouse_child_counts_and_children ... ok
test business::erp::model::tests::path_prefix_upper_bounds_the_subtree ... ok
test business::erp::model::tests::material_page_filters_and_pages ... ok
test business::erp::model::tests::material_page_serial_required_filter_tracks_column_existence ... ok
test business::erp::model::tests::inventory_two_level_ledger_is_consistent ... ok
test business::erp::model::tests::inventory_balance_consistency_rules ... ok
test business::erp::model::tests::decimal_12345_6789_roundtrips_without_loss ... ok
test business::erp::model::tests::normalize_page_clamps_bounds ... ok
test business::erp::model::tests::list_pages_respect_filters ... ok
test business::erp::dto::tests::validate_rejects_blank_code ... ok
test business::erp::dto::tests::validate_rejects_overlong_name ... ok
test business::erp::dto::tests::validate_rejects_negative_qty ... ok
test business::erp::dto::tests::validate_unit_base_consistency ... ok
test business::erp::dto::tests::validate_warehouse_level_consistency ... ok
test business::erp::dto::tests::validate_rejects_unknown_enum_values ... ok
test business::erp::dto::tests::validate_period_bounds_and_code ... ok
test business::erp::dto::tests::purchase_order_item_amounts_are_consistent ... ok
test business::erp::dto::tests::decimal_json_roundtrip_keeps_scale ... ok
test business::erp::dto::tests::update_absent_means_unchanged ... ok
test business::erp::dto::tests::filters_have_sane_defaults ... ok
test business::erp::dto::tests::validate_txn_direction_and_qty ... ok
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 931 filtered out; finished in 11.63s
```

无库环境（CI）：`unset ERP_DB_URL` 时同一批用例**跳过而不是失败**，全量仍然全绿：

```
$ unset ERP_DB_URL; cargo test --lib -j 1
test result: ok. 967 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.55s
```

### 4.2 `scripts/check.sh`

```
$ cd /opt/cloudpivot && CARGO_TARGET_DIR=/root/w1b-target bash scripts/check.sh
==> 后端：cargo fmt --check
==> 后端：cargo clippy -- -D warnings
==> 后端：cargo check
==> 前端：pnpm typecheck
==> 批次B 闸①：分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）
  PASS：0 处（分页全部走 scope::paginate）
==> 批次B 闸②：登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
  PASS：0 处
==> C7 G1..G6：全部通过
==> 偏好字典与色板一致性
  PASS：22 套色板与 V0091 的 ui.palette oneof 逐字一致
==> 全部检查通过
CHECK_SH_EXIT=0
```

### 4.3 仓库子树为什么不用 `LIKE`（EXPLAIN 实证）

夹具里造了 3 层树（总仓 → 分仓 → 库房）+ 另一棵树的顶级仓。`SET enable_seqscan = off` 下：

```
--- 写法 A：参数化 LIKE（直觉写法）
  Index Scan using uk_biz_warehouse_code on biz_warehouse
    Filter: ((warehouse_path)::text ~~ '/b1b…0001/%'::text)
    ↑ 走的是**编码唯一索引**整扫 + Filter，text_pattern_ops 索引完全没用上

--- 写法 B：~>=~ / ~<~（W1b 落地口径，model::warehouse_tree）
  Index Scan using idx_biz_warehouse_path_pattern on biz_warehouse
    Index Cond: (((warehouse_path)::text ~>=~ '/b1b…0001/'::text) AND ((warehouse_path)::text ~<~ '/b1b…00010'::text))
    Filter: (deleted_at IS NULL)
```

两种写法返回**同一集合**（`WH-W1B-ROOT,WH-W1B-SUB,WH-W1B-LEAF`），语义等价。

> 上界细节（踩过）：上界必须是「**去掉末尾斜杠**再补 `0`」⇒ `/uuid` → `/uuid0`。
> 写成 `/uuid/` + `0`（即 `/uuid/0`）会漏掉全部后代（uuid 以 `b`/`1`… 开头都 `> '0'`），
> 实测只回总仓一行。`path_prefix_upper()` 有单测钉这个边界。

### 4.4 验收脚本（可复跑）

```
$ REPO=/opt/cloudpivot CARGO_TARGET_DIR=/root/w1b-target bash scripts/verify/w1b_erp_data_layer.sh
== 1) 建一次性库 cloudpivot_w1b_verify
  [PASS] createdb -O cloudpivot cloudpivot_w1b_verify
== 2) 起隔离实例跑迁移（127.0.0.1:18089，redis db 11）
  [PASS] 迁移完成：数据库迁移完成: 应用 94 个迁移
== 3) 13 张表 + 迁移版本
  [PASS] 13 张 biz_* 表齐备
  [PASS] 迁移版本 = 137（V0137 已应用）
== 4) cargo test --lib business::erp（真库，36 条）
  test result: ok. 36 passed; 0 failed; …
  [PASS] erp 单测全绿：36 条
  [PASS] 没有用例跳过（真库路径确实跑了）
== 5) 仓库子树：~>=~ / ~<~ 走 text_pattern_ops 索引
  [PASS] 子树查询命中 idx_biz_warehouse_path_pattern
  [PASS] 参数化 LIKE 用不上该索引（这就是不用 LIKE 的理由）
  [PASS] 两种写法返回同一集合：WH-W1B-ROOT,WH-W1B-SUB,WH-W1B-LEAF
== 6) 门禁复查（分页入口 / SCOPE 口径）
  [PASS] 新模块 0 处 paginate_with_params
  [PASS] 13 个列表统一走 page_of 助手（13 处调用）
  [PASS] paginate_unscoped 只在 page_of 里出现 1 处（唯一分页入口）
  [PASS] 模块头写明了 SCOPE-EXEMPT 理由
  [PASS] business/mod.rs 已注册 pub mod erp;
  [PASS] 交付 3 个文件：mod.rs / model.rs / dto.rs
== 收尾
  已删库 cloudpivot_w1b_verify
== 汇总：PASS=15 FAIL=0
$ echo $?  → 0
```

## 5. 测试覆盖对照卡面七条

| 卡面要求 | 用例 |
|----------|------|
| ① 13 张表各验一个关键字段 | `unit_row_maps_key_field` … `inventory_txn_row_maps_direction_and_qty`（13 条） |
| ② 仓库树 3 层子树 | `warehouse_tree_returns_three_levels_in_path_order` / `warehouse_subtree_excludes_sibling_root` / `warehouse_child_counts_and_children` / `path_prefix_upper_bounds_the_subtree` |
| ③ 物料分页 + `serial_required` 筛选 | `material_page_filters_and_pages` / `material_page_serial_required_filter_tracks_column_existence`（含「列不存在 → 报错」与「造列 → 筛选生效 → 还原」两分支） |
| ④ 库存两级账 | `inventory_two_level_ledger_is_consistent`（成本账 5 = 库位 3 + 2）/ `inventory_balance_consistency_rules` |
| ⑤ DTO validate：空 code / 超长 name / 负数量 | `validate_rejects_blank_code` / `validate_rejects_overlong_name`（含「50 字符合法上界」「中文按 char 不按 byte」）/ `validate_rejects_negative_qty` |
| ⑥ Decimal 精度 12345.6789 | `decimal_12345_6789_roundtrips_without_loss`（写 `12345.6789` → 读回逐位一致；再写 `0.0001` 最小标度）+ `decimal_json_roundtrip_keeps_scale`（JSON 侧是字符串） |
| 剩下 10 条 | 枚举白名单 / 期间边界 / 金额三件套自洽 / 编辑语义 `Option<Option<T>>` / 筛选缺省值 / 流水方向 / 分页 clamp / 两级账规则 / 路径上界 / 9 个列表的筛选与分页 |

## 6. 踩过并固化的三个坑（已写进代码注释与 skill）

1. **`#[tokio::test]` 的独立 runtime × 进程级 `db()` 池**：池里的空闲连接绑在上一个 runtime 上，
   runtime 结束即死，但 `client.is_closed()` 不会立刻变 true ⇒ 下一次借出拿到死连接，
   报 `query failed … : connection closed`（实测 36 条挂 5–7 条）。对策：测试里 `Db::connect_with`
   + `pool.max_lifetime = ZERO`，逼池每次新建连接；真库用例再用一把 `DB_LOCK` 串行化
   （其中一个用例会 `ALTER TABLE`，并发会把别人的查询挂断）。
2. **`MaterialFilter` 一类的「可选 id 筛选」不能静默丢条件**：`String` 解析失败必须 400
   （见差异表 #4）。
3. **迁移种子与夹具抢唯一键**：V0137 已种 `1001` / `100101`（科目）与 `2026-10`（期间）。
   夹具改 `W1B` 前缀 + `2025-12`，并用 `ON CONFLICT DO NOTHING`（无目标）兜住任何唯一键冲突。

## 7. 交给 W1c 的接口面（`handler.rs` 只需要这些）

- 列表：`unit_page` / `warehouse_page` / `location_page` / `material_page` / `supplier_page` /
  `account_page` / `period_page` / `seq_page` / `purchase_order_page` / `purchase_order_item_page` /
  `inventory_page` / `inventory_location_page` / `inventory_txn_page` → 都返回 `(Vec<Row>, i64)`
- 详情：13 个 `*_find_by_id` + `warehouse_find_by_code` / `material_find_by_code` /
  `account_find_by_code` / `period_find_by_code` / `seq_find_by_type` / `purchase_order_find_by_no`
- 结构：`warehouse_tree`（树接口）、`warehouse_children` + `warehouse_child_counts`（卡片 `child_count`）、
  `location_by_warehouse`（库位下拉）、`account_children`（科目树）
- 账：`inventory_find` / `inventory_locations` / `inventory_balance`（两级账快照 + `is_consistent()`）
- DTO：每个 Create 的 `validate()` 已经给出 400 文案；`UpdateXxxReq` 的「缺席 ≠ 清空」语义已由
  `double_option` 定死；金额全是 `Decimal`（JSON 形态是字符串）

## 8. 遗留（不是本卡范围）

- **V0138**：`biz_material.serial_required` / `biz_material_serial` 落地后，`material_page` 的筛选与
  `MaterialCard` 需要补一个字段（`model.rs` 的 `SERIAL_COLUMN` 探测会自动启用 WHERE 分支）。
- **W1c**：路由 / 权限码 / 事务编排 / 单号取号（`biz_seq` 的 `UPDATE … RETURNING` 路径）/ 主子单级联写。
- **展示字段**：列表卡片里的 `supplier_name` / `warehouse_name` / `material_name` 目前只给 id，
  join 起来变成 `*_CardRow` 是 W1c 与 W2 的取值口径问题，本卡刻意不做（避免 model 层出现两套行结构）。
