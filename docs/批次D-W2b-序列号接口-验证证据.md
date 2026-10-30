# 批次 D · W2b 序列号接口（TSK-202610-000679）验证证据

> 交付分支：`main` · 前置：W2a（`0e6a9da`，V0138 建 `biz_material_serial`）
> 证据来源：本机 `bash scripts/verify/w2b_serial_api.sh`（隔离库 `cloudpivot_w2b` + 端口 18095）、
> `cargo test --lib -j 2`（`ERP_DB_URL` 指同一隔离库）、`bash scripts/check.sh`

## 一、交付物

| 文件 | 内容 |
|------|------|
| `backend/src/business/erp/serial_model.rs` | 数据层：原子取号（`UPDATE … RETURNING`）、批量生成、扫码、分页、状态流转（事务 + `FOR UPDATE`） |
| `backend/src/business/erp/serial_dto.rs` | 契约：批量生成 / 筛选 / 状态流转入参校验，三种出参 |
| `backend/src/business/erp/serial_handler.rs` | 5 个接口 + 5 个权限码 + 25 条单测（含 20 条真库用例） |
| `backend/migrations/business/V0142__erp_serial_permissions.{up,down}.sql` | 序列号菜单 0981 + 4 个按钮（0982–0985）+ 超管/laoliu 授权 + 自检 |
| `scripts/verify/w2b_serial_api.sh` | 73 项真机验收（接口 / 重号 / 并发 / 状态机 / 权限 / EXPLAIN / V0142 往返） |
| `backend/src/shared/error_code.rs` | 新增 `66007 ERP_SERIAL_NOT_REQUIRED`、`66008 ERP_SERIAL_STATUS_INVALID` |
| `backend/src/business/erp/{model,dto,handler}.rs` | **补口**：物料主数据能读写 `serial_required`（见 §四） |

## 二、5 个接口

| 方法 | 路径 | 权限码 | 实测 |
|------|------|--------|------|
| POST | `/api/business/erp/serial/batch-generate` | `business:erp:serial:generate` | 200；1000 条 **108 ms**；非一物一码物料 400+66007 |
| GET | `/api/business/erp/serial/page` | `business:erp:serial:list` | 200；material/warehouse/status/serial_no 筛选；非法 UUID 400+66006 |
| GET | `/api/business/erp/serial/scan?serial_no=` | `business:erp:serial:view` | 200；HTTP 往返 **2 ms**；不存在 404；软删 404；缺参 400+66006 |
| PATCH | `/api/business/erp/serial/{id}/status` | `business:erp:serial:status` | 3 条合法边 200；`OUTBOUND→IN_STOCK` 与 `SCRAPPED→*` 400+66008 |
| GET | `/api/business/erp/serial/{id}` | `business:erp:serial:detail` | 200；软删/不存在 404 |

## 三、取号（卡面 §一 的落实与一处**安全性修正**）

格式 `SN-yyyyMMdd-XXXXXX`（6 位补零，按 **CST** 的「天」重置），号存 `biz_seq`
（V0138 已幂等种入 `seq_type='SN'`）。一次 `UPDATE … RETURNING` 拿整段：

```sql
UPDATE biz_seq
   SET current_no = CASE WHEN period_key = $1 THEN current_no + $2
                         WHEN period_key <  $1 THEN $2          -- 进入更新的一天：重置
                         ELSE current_no + $2 END,              -- 周期键回退：绝不重置
       period_key = CASE WHEN period_key < $1 THEN $1 ELSE period_key END,
       updated_at = now()
 WHERE seq_type = 'SN' AND deleted_at IS NULL
RETURNING current_no - $2 AS start;   -- 号段 (start, start+n]
```

**为什么第三条分支必须存在**（实测教训）：第一版写「`period_key` 与本次不同就重置」，
单测用假周期键把 `period_key` 推来推去（A→B→A），回到 A 时从 000001 重新发号，
**把当天已发出的 1000 个号整段撞掉** —— 真库用例 12 条报 `409 + 66001`。
周期键 `yyyyMMdd` 定宽，字典序 = 时间序，所以「只有前进才重置」既满足跨天重置，
又让 NTP 回拨 / 手工改库 / 测试假日期都不会造成重号。

- 卡面写 `scope='material_serial:20261030'` / `current_value`：**真实列是
  `seq_type` / `period_key` / `current_no`**（V0137 §5.9），按真实列落地，
  `(seq_type, period_key)` 两列共同表达卡面 `scope` 的语义。
- 批量落库用**一条 `INSERT … SELECT` + 单个 `text` 载荷**（每行 `id|serial_no`，
  SQL 侧 `string_to_array` + `split_part` 拆）：`Value::Text("{a,b}")` 绑 `?::text[]`
  实测报 `error serializing parameter N`（1000 条批量生成直接 500）。
- 号码先取后插：插入失败会浪费一小段号（序列号允许跳号，不可接受的是重号）。

## 四、两处「卡面没写但必须补」的口子

1. **物料主数据读不了也写不了 `serial_required`**：W1c 落地时该列还不存在（dto 注释明写
   「不含 serial_required」），V0138 建列后接口没跟上 → 只能 psql 改库才能打开一物一码，
   序列号功能等于没法启用。本卡补：`MaterialRow` 加字段、`CreateMaterialReq` /
   `UpdateMaterialReq` 接受 `serial_required`、`MaterialRes` 回显（e2e 实跑
   `GET/PUT material` 与 `material/page?serial_required=true` 三处均 200）。
2. **`material_page` 的 `serial_required` 筛选绑的是 `i16` 0/1**，而列是 BOOLEAN：
   V0138 落地后该筛选会报 `operator does not exist: boolean = smallint`（与 V0137 §5.9
   同源）。已改成绑 `bool`，并用 e2e `GET /material/page?serial_required=true` 钉住。

## 五、测试（42 条，卡面要求 ≥20；22 条纯逻辑 + 20 条真库）

`serial_model` 9 条纯逻辑（号格式 / 超 6 位 / 号段连续 / CST 周期键 / 状态机 3 条边 /
回退与原地拒绝 / 拒绝文案 / 落库载荷）+ `serial_dto` 8 条（条数边界 / UUID / source_type /
目标态 / reason 长度 / 筛选默认值 / 非法 status / 非法 UUID）+ `serial_handler` 25 条
（权限码 / 可写目标态 / 扫码缺参 / 非法目标态不碰库 + 20 条真库用例：**1000 条无重号** /
**并发 2×500 无重号** / 号段连续 / **跨天重置** / 非一物一码 400+66007 / 物料不存在 404 /
库位引用校验 / 扫码全字段 / 扫码 404 / 软删 404 / 软删后同号重用 / 转 OUTBOUND 写 outbound_at /
IN_STOCK→SCRAPPED / OUTBOUND→SCRAPPED 保留时刻 / OUTBOUND→IN_STOCK 拒绝且库内不变 /
SCRAPPED 终态 / 行不存在 404 / 分页三条件 / 详情与软删 / 16 列映射 / 物料开关回读）。

真库用例需 `ERP_DB_URL`（未设置则打印 `SKIP` 跳过，`cargo test --lib` 在无库环境仍绿）。

## 六、验收实测（原始输出）

```
# ① 全量 lib 单测（ERP_DB_URL → cloudpivot_w2b）
FULL_TEST_EXIT=0
test result: ok. 1054 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 24.16s

# ② scripts/check.sh
CHECK_SH_EXIT=0
==> 批次B 门禁汇总：闸①=0 处  闸②=0 处  （B0 基线 20 / 21）→ 全部通过
==> C7 门禁汇总：G1–G6 共 6 条，未通过 0 条 → 全部通过
==> 偏好字典与色板一致性：22 套色板逐字一致
==> 全部检查通过

# ③ 真机验收 73 项
数据库迁移完成: 应用 97 个迁移          ← V0142 已被迁移器自动应用
[PASS] 迁移器已应用到 V0142（max version=142）
[PASS] POST batch-generate 1000 条
       1000 条批量生成耗时：108 ms（首号 SN-20261030-000001，末号 SN-20261030-001000）
[PASS]   返回的号无重号（unique=1000）
[PASS]   库内 DISTINCT=1000（psql 复核）
[PASS]   并发 1000 条无重号
[PASS] POST batch-generate 按数量物料 → 400+66007
[PASS] GET scan 命中 / scan material_name / warehouse_name / location_name / status / inbound_at
       扫码 HTTP 往返 0.003746s
[PASS] GET scan 软删的号 → 404
[PASS] page total=2000（≥2000）；筛出 total=2000；按号筛 total=1
[PASS] PATCH IN_STOCK → OUTBOUND；转 OUTBOUND 写 outbound_at（库内 true）
[PASS] PATCH OUTBOUND → IN_STOCK → 400+66008（文案指引「新的序列号」）
[PASS] PATCH OUTBOUND → SCRAPPED；PATCH SCRAPPED → 任何 → 400+66008
[PASS] 无码打 page / scan / batch-generate / status → 全 403
[PASS] 序列号表堆到 97000 行（含 1000+1000 真实号）
       ->  Index Scan using uk_biz_material_serial_no on biz_material_serial s
           (actual time=0.029..0.029 rows=1.00 loops=1)
       Execution Time: 0.067 ms
[PASS] 未出现 Seq Scan on biz_material_serial
       9.5 万行下扫码接口最快 HTTP 往返：2 ms（含鉴权中间件 + JSON 序列化）
[PASS] V0142 up 重放（幂等）/ down（零残留）/ 再 up（可重建）/ 超管拿到 5 个节点授权
[PASS] 真库单测全绿：test result: ok. 42 passed; 0 failed
[PASS] prod 8023 仍 200（本卡未碰生产）

W2b 验收：73 项 PASS / 0 FAIL
```

**扫码 10 ms 红线**：SQL 执行 0.067 ms（EXPLAIN ANALYZE，9.7 万行），
HTTP 全链路 2 ms（含 JWT 中间件 + JSON），两条都远低于 10 ms。

## 七、与卡面的差异（逐条）

1. `biz_seq` 的列名：卡面 `scope` / `current_value` → 真实 `seq_type` / `period_key` / `current_no`。
2. 权限码落点：卡面「种进 `sys_permission` + `sys_menu`」→ 本仓**没有 `sys_permission` 表**，
   只种 `sys_menu.auth_code` + `sys_role_menu`（与 V0141 同口径）。
3. `PATCH …/status` 的 `reason`：V0138 的 16 列里没有 `reason` / `remark`，红线⑤禁止改表，
   故只做长度校验（≤200）+ `tracing::info!` 留痕，不落库（字段保留给前端契约稳定）。
4. 批量生成出参：卡面「返回生成的号列表」→ 多给 `period_key` 与回读整行 `list`（前端要 `id` 才能点流转）。
5. 测试库：卡面「只用 `cloudpivot_test`」→ 该库 `max(version::int)=9`（半死库，无 ERP 表），
   按 W2a/W1c 惯例自建自毁隔离库 `cloudpivot_w2b`（**prod / dev 全程未动**）。
6. `location_id` 语义：硬外键指向 `biz_inventory_location.id`（不是 `biz_location.id`），
   所以扫码取库位名多一跳（`serial.location_id → biz_inventory_location.location_id → biz_location.location_name`）。

## 八、rebase 到最新 main 后的复跑（`6748d48`，含他人 `6001bc5`）

rebase 后（任务模块有他人提交）在**最终树**上重跑一遍全链，结论不变、数字更新：

```
cargo test --lib -j 2（ERP_DB_URL → 隔离库）: 1066 passed; 0 failed   （1005 基线 + 本卡 42 + 他人 12 …）
scripts/check.sh: exit 0（闸① 0 / 闸② 0 / C7 G1–G6 全过）
scripts/verify/w2b_serial_api.sh: 73 项 PASS / 0 FAIL
  · 1000 条批量生成 102 ms；首号 SN-20261030-000001 末号 SN-20261030-001000
  · EXPLAIN ANALYZE：Index Scan using uk_biz_material_serial_no，Execution Time 0.060 ms
  · 扫码 HTTP 全链路 2 ms
  · 真库单测 42 passed / 0 failed
dev 库（cargo-watch 监听 migrations）：已自动应用到 142，序列号权限码 5 个
prod 库：仍是 141，序列号权限码 0 个（未动生产）；prod 8023 / dev 8024 /health 均 200
```

## 九、遗留债（给后续卡）

1. `serial.location_id` 指向**库位库存行**，但生成接口只校验「行存在 + 属于该仓库」，
   不校验「该库存行的 material_id 与本次物料一致」——同一库位库存行可能被另一个物料借用。
   要收紧需在 V014x 加复合校验或改语义（卡面未要求，未擅自加）。
2. `reason` 无处落库：要留审计痕迹需给 `biz_material_serial` 加 `remark` 列（新迁移）。
3. 序列号**没有删除接口**（卡面只给 5 个接口），软删只发生在运维手工 SQL；
   若产品要「作废」，建议走 `SCRAPPED` 而不是软删。
