# M5-02 · 分批法：按生产订单/批次归集表 — 证据包

> 卡：`t_b9375705`（M5 成本批第 2 张，接 M5-01 成本方法配置）· 日期：2026-11-19 · 主机：99（开发机，PG 在 145）
> 验证库：`cloudpivot_test_m502`（`scripts/verify/make_full_suite_db.sh` 全新构建：全量迁移 + 演示组织种子 + 21 项自检全过）
> 临时库：`cp_m502_dev`（模板库克隆，仅用于早期跑通；模板库 schema 落后，正式证据一律用上面那份种子库）

## 一、交付了什么（两个接口 + 一层契约/取数 + 9 条真库用例）

| 层 | 文件 | 说明 |
|---|---|---|
| 契约 | `backend/app/src/business/erp/cost/cost_collect_sheet_dto.rs` | 期间必填校验 / 状态与排序值域 / 分页夹取 / 出参（行 = 一个批次）/ 标签映射；**复用** `business:erp:cost-object:list` 权限码 |
| 取数 | `backend/app/src/business/erp/cost/cost_collect_sheet_model.rs` | 按批次聚合本期料/工/费（派生表 + `FILTER`）/ 整表合计 / 明细分笔；数据范围取 `scope::filter_for(OBJECT_TARGET)`；参数顺序手工构造并**逐次校验占位符数** |
| 接口 | `backend/app/src/business/erp/cost/cost_collect_sheet_handler.rs` | `GET /business/erp/cost-collect-sheet`（分页 + 整表合计）、`GET /business/erp/cost-collect-sheet/{object_id}`（逐笔明细）；无写接口 |
| 接线 | `backend/app/src/business/erp/cost/mod.rs` | 4 个 `pub mod` + 1 个 `#[cfg(test)]` 模块声明 |
| 用例 | `backend/app/src/business/erp/cost/cost_collect_sheet_db_tests.rs` | 9 条真库用例（`ERP_DB_URL` 开关）+ 16 条纯函数用例（分散在 dto/model/handler） |

**没有**：新迁移（表 `biz_cost_object` / `biz_cost_collect` 是 V0370/V0371 已有的）、新权限码、新菜单、前端页面（前端卡是 M5-07「前端：批次成本页」）。

## 二、口径（决定，不是待办）

1. **行 = 一个批次（成本对象）**，列 = 该批在**指定期间**的 直接材料 / 直接人工 / 制造费用 / 合计 + 批次身份列（生产订单号 `source_doc_no` / 产出批号 / 产出品 / 车间 / 计划·完工·在制数量 / 状态 / 归集行数 / 最后一笔归集日期）。表尾给**整张表**（不是当前页）的合计。
2. **「按生产订单」= 批次行上的订单号列**。ARCH-9 D-3/D-4 定稿一期不引入工单，一单一批 ⇒ 生产订单号就是 `source_doc_no`；`keyword` 可按订单号搜、`sort=order_no` 可按订单号排。要「一单多批合并汇总」得等三期 `object_type='ORDER'`，现在加 `group_by` 参数只有一个取值是死旋钮。
3. **没有归集的批次也出现在表里**（三行 0 + `line_count=0`）：期初在产/本月没投料的批是**缺口**，不是噪声。所以不提供 `include_empty` 开关。
4. **期间必填**（不默认本月）：归集额是「期间 × 批次」的量；默认本月会让 1 月 1 日打开页面的人以为数据丢了。
5. **费用池行（`cost_object_id IS NULL`）不进批表、也不进合计**：它们还没有归集对象，「未摊完的费用池」由 B2-12 未归集清单（`GET /business/erp/cost-period/uncollected`）负责，本表不重复一遍。
6. **合计按数据范围裁剪**：部门范围的角色看到的是本部门车间的合计，不是全公司金额（反证见 §四 A）。
7. **权限码复用 `business:erp:cost-object:list`**：本表每一行都是一个成本对象、每一列在 `GET /cost-object` 与详情里本来就对同一个码开放（不多露一列）；新开码要种菜单，而页面要到 M5-07 才落地 —— 先插一条指向不存在组件的菜单行会让侧栏点开即 404。与 `cost_uncollected` / `cost_period_sheet` 复用既有码同口径。
8. **明细不可见 ⇒ 404（不是 403）**：403 等于承认「这个 id 真实存在」，把别的车间的批次号变成探测口。

## 三、真跑的验收（每项都可复核）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_fmt.out` | 5 个文件的 `rustfmt --check` | `rc=0` |
| `02_clippy.out` | `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | 零告警（`rc=0`） |
| `03_tests_and_regression.out` | ① 本卡 25 条（16 纯函数 + 9 真库）② 成本域全量 `business::erp::cost::` ③ 数据范围接线守卫 `d8c_wiring_anchors_exist_in_source` | ① **25 passed** ② **695 passed / 0 failed** ③ **1 passed** |
| `04_permission_audit.out` | `system::menu::permission_audit_tests`（N14 死码 / N15 码形状 / N16 裸路由 / N18 例外台账 / N20 非超管授权 / N21 模板遮蔽） | **8 passed** |
| `05_http_probe.out` | 起隔离实例（`:18023`）→ admin 登录 → 打 4 种请求 | 缺 `period_code` ⇒ **400**（报文说清为什么必填）；`2026-08` ⇒ **200**（完整 JSON：`totals` + 每行 `mat/labor/oh/total/line_count/last_occur_date`）；不存在批次 ⇒ **404**；`sort=amount desc;drop table x` ⇒ **400**（回显收到什么） |
| `06_http_probe_with_data.out` | 造一个批次（料 100+50、工 30、费 20）→ 归集表 + 明细 → 清场 | 表行 `mat=150.0000 / labor=30.0000 / oh=20.0000 / total=200.0000 / line_count=4 / last_occur_date=2031-07-20`；明细 4 笔（料 2 笔带数量、工/费 `qty=null`），**明细三项和 == 表行**；清场后残留计数 **0** |
| `07_teeth_a_scope_totals.out` | 反证 A：把合计的数据范围片段改成恒真 | 红：`合计也必须按范围裁剪` left `1000.0000` / right `100.0000`（100 本部门 + 900 别部门被算进来） |
| `08_teeth_b_bind_guard.out` | 反证 B：`page_rows` 少推一个期间参数 | 红且**拒绝执行**：`SQL 占位符与参数数量不一致（?=2，参数=1）：参数错位会静默绑错值，拒绝执行` |
| `09_model_sha_after_restore.txt` / `00_model_sha_before.txt` | 反证后还原 | `ded092b4…3ca9f` 逐字节一致 |
| `10_after_restore_green.out` | 还原后重跑 | 25 passed / 0 failed |

> 反证模板：`mutate_scope.py`（scratch，仅本机）；A 换 `totals` 的范围片段为 `allow_all()`，B 删掉 `page_rows` 里的期间参数推送。

### 真跑实例的两个前置（排障用，不是缺陷）

1. **成本模块开关**：`sys_config.erp.cost.enabled` 默认 `false` ⇒ 所有 `/business/erp/cost*` 路由统一 `403 + 66079`（框架级守卫，本卡的两条路由同样被守）。真跑前置 `true`。
2. **首登强制改密**：`admin` 的 `must_change_password=true` 时除改密/登出外一律 403；真跑前置 `false`（生产环境当然要留着，改成走一次改密流程）。


## 四、9 条真库用例覆盖（`cost_collect_sheet_db_tests.rs`）

| # | 用例 | 钉住什么 |
|---|---|---|
| 1 | `sheet_row_per_batch_with_three_elements_and_totals` | 一行一批；料 100+50 / 工 30 / 费 20 各归各位；`line_count=4`；`last_occur_date=2031-07-20`；整表合计 = 行之和；未归集批次也在表里且 `line_count=0` |
| 2 | `sheet_period_isolation_and_pool_rows_are_excluded` | 别期 999 不进本期；费用池行 777 既不成为行也不进合计 |
| 3 | `sheet_filters_status_dept_and_keyword` | 状态 / 车间 / 关键字（订单号、产品名、批次号）各自生效；查不到 = 0 行 0 元而不是报错；**合计随筛选走** |
| 4 | `sheet_pagination_keeps_totals_whole` | `page_size=1` 两页不重叠、总数仍是筛选集；**两页合计都是 125**（合计不随翻页变） |
| 5 | `sheet_sorts_by_total_and_order_no` | 金额倒序（默认）/ 订单号倒序 / 批次号倒序；注入式 `sort` ⇒ 400 |
| 6 | `sheet_scope_limits_rows_totals_and_detail` | 部门范围：1 行、合计 100（不是 1000）；本部门明细可看、别部门 404；超管两条都在（证明 404 来自范围） |
| 7 | `detail_lines_add_up_to_the_sheet_row` | 明细只含本期三笔、元素顺序 料→工→费、中文标签由后端给、工行 `qty=None`；**明细三项和 == 归集表该行** |
| 8 | `detail_rejects_bad_period_and_unknown_batch` | 期间非法/缺失 400；批次不存在 404；路径参数非 uuid 404；软删批次 404 且不进表也不进合计；`status=SETTLED` 400 |
| 9 | `zz_teardown_leaves_zero_residue` | 先断言清场前看得到 → 清场 → 归集行 0 / 池行 0 / 批次 0 / 部门 0 / 表回到 0 行 |

## 五、边界

- 未改 `biz_cost_collect` / `biz_cost_object` 的任何列与约束，未动 calc / settle / reverse / report 任何一行。
- 未新增权限码与菜单 ⇒ `menu_invariants.sh` 的 8 条判据面不变（菜单总数、同父 sort、title 长度都没被触碰）。
- 未写前端（M5-07 的活）；本卡只保证接口契约与取数口径。
- 早期在模板库 `cp_m502_dev`（`cloudpivot_test` 克隆）上跑成本域全量时 52 条红，原因是**模板库 schema 落后**（`biz_voucher.currency_code` 列不存在，迁移只到 339 条），与本卡无关；改用 `make_full_suite_db.sh` 新建的 `cloudpivot_test_m502` 后 695 passed / 0 failed。该现象已在本卡评论里记录。
