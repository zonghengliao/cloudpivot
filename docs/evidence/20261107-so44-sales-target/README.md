# SO-44 销售目标与预测（表 + 达成率看板）验收证据

卡片：`t_1a134712` · ARCH-7 §15.8 · 交付日期 2026-11-07

## 1. 交付内容

| 层 | 文件 | 说明 |
|----|------|------|
| 迁移 | `backend/migrations/business/V0318__sales_target_forecast.{up,down}.sql` | 建 `biz_sales_target`（14 列）/ `biz_sales_forecast`（14 列）+ **9 条 CHECK**（目标 5 + 预测 4）+ 4 个部分唯一索引 + `biz_seq` 取号（STG/SFC）+ 页面与 8 个按钮菜单 + 4 角色授权矩阵 + **末尾 DO 块自检**（8 条行为探针必须被 CHECK 拒、正样本默认值、唯一索引、权限码承载、页面 path/component/父级） |
| DTO | `backend/src/business/erp/sales_target_dto.rs` | 域值常量（与 CHECK 逐字一致）、`Period`（`yyyy-MM` / `yyyy-QN` ⇒ 左闭右开区间）、达成率 / 缺口 / 金额精度、归一化与校验；**17 条单测** |
| model | `backend/src/business/erp/sales_target_model.rs` | 两张表的读写、取号（`biz_seq` 按 CST 天滚动）、**达成率两个口径的取数 SQL**；**6 条单测**（含「uuid 参数必须带 `::uuid`」的绑定回归） |
| handler | `backend/src/business/erp/sales_target_handler.rs` | 9 个接口（目标 4 + 看板 1 + 预测 4）、两个口径一次算全、业务逻辑为可测 `async fn`；**18 条单测** |
| 前端 | `frontend/src/api/business/erp/sales-target.ts`、`frontend/src/views/business/erp/sales-target/index.vue` | 三页签：达成率看板（进度条 + 缺口 + 汇总卡）/ 目标维护 / 销售预测 |
| 路由 | `frontend/src/router/index.ts` | `business/erp/sales-target` → `BusinessErpSalesTarget`，`meta.permission = business:erp:sales-target:list` |
| 模块声明 | `backend/src/business/erp/mod.rs` | 三个模块 + 模块表文档行 |
| 验收脚本 | `scripts/verify/so44_start.sh` / `so44_seed.sql` / `so44_sales_target_e2e.py` / `so44_curl_evidence.sh` / `so44_apply_fresh.sh` | 隔离实例 + 夹具 + 77 项断言 + 原始 curl 取证 + 干净库 APPLY_OK 复证 |

## 2. 达成率口径（改之前先读）

```text
订单口径 ORDER   ：期间内 order_date 落在 [from, to)、status ∈ APPROVED/PARTIAL_OUT/OUTBOUND/CLOSED
                   金额 = total_with_tax（含税，与销售业绩口径一致）
收款口径 RECEIPT ：期间内 receipt_date 的收款核销额 − 期间内 refund_date 的退款核销额（净额）
                   收款单无销售员列 ⇒ 顺链反查：收款核销 → 应收 → 出库(source_type='SALES') → 发货 → 订单
归属：SALESPERSON 按 salesperson_id；DEPT 经 sys_user.dept_id；MATERIAL_CATEGORY 按明细 material_id
      （收款口径按「该物料行含税额 / 整单含税额」分摊）；链断的收款只进 ALL 口径
achievement_rate = actual / target × 100（2 位，四舍五入；target = 0 ⇒ 0）
gap              = target − actual（正 = 还差多少，负 = 超额）
```

两个口径**每次都一起算、一起返回**：`actual_amount` / `achievement_rate` / `gap` 是按 `basis`
选中的那一份，`by_order` / `by_receipt` 两个都在 —— 前端切口径不必重新请求，
也不会出现「两个口径各算一次、数字对不上」。

## 3. 验收结论

### 3.1 迁移 APPLY_OK

```
versioned|sales_target_forecast|318|e2acdbb2bb0148fd2c814ee0acd3dda66dc3848a29e1efaaa7f8a7d2bc21f3f1|executed|...|success=t
```

- `kind = versioned`、`applied_via = executed`、`success = t`（自检 DO 块通过，NOTICE「SO-44: 销售目标 / 预测两表 + 8 个权限码 + 菜单 + 取号规则自检通过」）。
- 一个隔离库上重启实例日志出现「应用 N 个迁移」后，再次启动为「应用 0 个迁移」（checksum 校验通过）。

**干净库首启复证**（`scripts/verify/so44_apply_fresh.sh`，输出见 `apply_fresh.txt`）：

```
== 空库已建：cloudpivot_so44_apply（owner=alun，无 schema_migrations）
2026-11-07T03:37:51 INFO nik_web::app: 数据库迁移完成: 应用 250 个迁移
== V0318 记录
318|sales_target_forecast|versioned|executed|t
== 表结构
target_cols=14 forecast_cols=14 part_uk=4 seq=2 menus=11
NOTICE: SO-44: 销售目标 / 预测两表 + 8 个权限码 + 菜单 + 取号规则自检通过
```

CHECK 约束实测 9 条：`biz_sales_target` 5 条（scope / scope_id / period / amount / status）、
`biz_sales_forecast` 4 条（period / amount / confidence / version）。

### 3.2 达成率计算正确（夹具 → 期望 → 实测）

夹具 `scripts/verify/so44_seed.sql`：销售员甲（销售一部）订单 SO-SO44-0001 含税 11300
（原料A 6780 + 原料B 4520）；出库 → 应收 11300 → 收款 5000(POSTED) → 退款 1000(POSTED)；
干扰单：CANCELLED 9999（同月，不计）、2026-10 单 8888（月度不计 / **季度计入**）。

| 口径 | 目标 | 期望实际 | 实测 | 达成率 | 缺口 |
|------|------|----------|------|--------|------|
| SALESPERSON / ORDER | 12500.50（e2e ⑥ 把目标从 10000 编辑成该值并重新启用后取的数，curl 取证同值） | 11300.00 | 11300.00 | 90.40 | 1200.50 |
| SALESPERSON / RECEIPT | 同上 | 4000.00 | 4000.00 | 32.00 | 8500.50 |
| DEPT / ORDER | 20000.00 | 11300.00 | 11300.00 | 56.50 | 8700.00 |
| MATERIAL(原料A) / ORDER | 5000.00 | 6780.00 | 6780.00 | 135.60 | −1780.00 |
| MATERIAL(原料A) / RECEIPT | 5000.00 | 2400.00（4000×6780/11300） | 2400.00 | 48.00 | 2600.00 |
| ALL / ORDER（2026-11） | 11300.00 | 11300.00 | 11300.00 | 100 | 0.00 |
| ALL / RECEIPT（2026-11） | 11300.00 | 4000.00 | 4000.00 | 35.40 | 7300.00 |
| ALL / ORDER（2026-Q4） | 50000.00 | 20188.00（11300 + 10 月 8888） | 20188.00 | 40.38 | 29812.00 |

`2026-10` 期间无目标 ⇒ 看板返回空列表（不报错）。

### 3.3 e2e（真实 token + 真实 HTTP）

```
bash scripts/verify/so44_start.sh                       # 隔离实例 127.0.0.1:18044
psql -f scripts/verify/so44_seed.sql                    # 夹具
python3 scripts/verify/so44_sales_target_e2e.py         # 77 断言
→ PASS = 77 / FAIL = 0
```

覆盖：目标 4 种范围 + 季度、编号形状、回读派生列（scope_name）、分页 / 关键字、
看板两个口径 × 4 种范围、编辑后重算、停用后不看板、删除 200 → 重复删 404、
预测 4 接口 + 同维度同版本 409 + 全局预测（客户/物料都空）、负向（ALL 带 scope_id、
非法期间、负目标、3 位小数、非法 scope_type、范围对象不存在、非法 basis、缺 period_code、
非法季度、非法信心度、版本号 0、非法 UUID、无 token 401）。

### 3.4 原始 curl 取证

`docs/evidence/20261107-so44-sales-target/curl_evidence.txt`（`scripts/verify/so44_curl_evidence.sh` 生成），
含两个口径的看板 JSON、目标 / 预测的创建与分页、5 条负向 HTTP 码、干净退出前的清理 DELETE。

### 3.5 静态检查

| 项 | 结果 |
|----|------|
| `rustfmt +1.94.1 --edition 2024 --check`（本卡 3 文件） | 无 diff |
| `cargo clippy --lib` | 本卡 0 警告；**唯一警告是既有** `sales_order_model.rs:699 too_many_arguments (8/7)`（SO-40 遗留，非本卡文件） |
| `cargo test --lib business::erp::sales_target` | 41 passed / 0 failed |
| `pnpm typecheck` | exit 0 |
| `pnpm build` | exit 0 |
| `perm_menu_gate.py`（云 cloudpivot_so44） | `missing_in_menu = 0`（8 个新码都有菜单承载行），`unused_in_code = 12` 全为既有行级 / 纯前端码 |

## 4. 修掉的两个真缺陷（前两次崩溃留下的半成品）

1. **`create_by` / `update_by` 裸 `?` 绑定 ⇒ 新建目标 / 预测全 500**
   症状：`POST /sales-target` 500，日志 `error serializing parameter 9`（0 起算 = create_by）。
   原因：SQL 里这两个 uuid 列写成裸 `?, ?`，PG 把参数推断成 `uuid`，而驱动拿到的是
   `Value::Text("uuid 串")` ⇒ `type_mismatch`（`PgParam::Text` 只接受文本家族）。
   修法：改成 `NULLIF(?::text, '')::uuid`（与文件内其它 uuid 位同口径），
   并加**绑定回归单测** `insert_target_sql_binds_uuid_columns_with_cast` /
   `insert_forecast_sql_binds_uuid_columns_with_cast` 钉住「uuid 列必须带 `::uuid`」。
2. **`version_no = 0` 走库侧 CHECK ⇒ 500**
   症状：`POST /sales-forecast {"version_no":0}` 500，日志
   `check constraint violated (constraint ck_biz_sf_version)`。
   修法：入口 `check_version_no()` 校验 `>= 1` ⇒ 400（新建与编辑两条路径都接），并加单测。

顺带修掉的口径不一致：`receipt_actual_total`（ALL 口径）原先是「期间内收款 − 其名下全部退款
（不限退款日期 / 状态）」，与按范围口径的「事件日期落期间 + 退款状态白名单」不一致 ——
11 月收款 12 月退款时，ALL 会凭空少一笔而按人不会。已改成同构的事件式（收款按 `receipt_date`、
退款按 `refund_date`，都受状态白名单约束）。

## 5. 残留 / 已知边界

- 金额按**原币**汇总（与 SO-43 对账单一致），多币种混收不换算；要做本位币口径需接 SO-40 的 `base_total_*`。
- `MATERIAL_CATEGORY` 的 `scope_id` 存 **`biz_material.id`**：基础版没有 `biz_material_category` 表
  （与 V0309 佣金规则同口径），接分类表后由数据迁移换 id，看板取数逻辑不变。
- 收款口径靠单据链反查销售员：手工挂账 / 未关联发货的收款只进 `ALL` 口径。
- 达成率 JSON 里是 `Decimal` 字符串，整数值可能序列化成 `"100"` 而不是 `"100.00"`（前端 `formatRate` 统一补零）。
- 本机保留验收库 `cloudpivot_so44`（实例按 pid 停止）可复跑。
