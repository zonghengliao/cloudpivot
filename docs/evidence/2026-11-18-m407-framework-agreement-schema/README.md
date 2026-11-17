# M4-07 框架协议表结构（有效期 + 价格条款 + 数量上限）· 证据包

- 卡：`t_7eb1c440`（M4-07【M4·采购】框架协议：表结构（有效期 + 价格条款 + 数量上限））
- 分支/提交：`main`（提交号见卡内汇报）
- 本卡范围：**只交付表结构**（含供应商与物料范围）。CRUD/审批、订单引用校验（M4-08）、
  执行量统计（M4-09）、单测（M4-10）、前端（M4-11）都不在本卡。

## 1. 交付物

| 文件 | 说明 |
|:--|:--|
| `backend/app/migrations/business/V0450__framework_agreement.up.sql` | 两张表 + 取号行 FWA + 7 索引 + 末尾 DO 自检（533 行） |
| `backend/app/migrations/business/V0450__framework_agreement.down.sql` | 回滚：**有数据就拒绝**，再删表与取号行 |
| `backend/app/src/business/erp/purchase/framework_agreement_model.rs` | 表名/枚举常量 + 行结构 + 取号 `next_agreement_no` + 读入口 + 派生合计重算 + 有效期/容差纯函数 + 8 条纯逻辑单测 |
| `backend/app/src/business/erp/purchase/framework_agreement_db_tests.rs` | 真库用例 12 条（约束行为，逐条对应卡面口径） |
| `backend/app/src/business/erp/purchase/mod.rs` | 注册上面两个模块（+10 行） |
| `scripts/verify/m407_framework_agreement_schema_drill.sh` | 演练脚本：克隆库 → 推到 V0449 → 上 V0450 两遍 → 结构/约束/范围/全链重放/回滚/用例/门禁/残留 |

## 2. 表设计（卡面逐条落点 + 理由）

`biz_framework_agreement`（单头，29 列）· `biz_framework_agreement_item`（物料范围，13 列）

| 卡面原话 | 落点 | 理由 |
|:--|:--|:--|
| **有效期** | `start_date` / `end_date`（`ck_biz_fwa_period` 保证起 ≤ 止）+ `sign_date`（签订日，可≠生效日） | **不造 `EXPIRED` / `TERMINATED` 状态**：到期是时间的函数，不是人的动作。落成状态列就要有人去刷，刷漏一格协议「永久有效」，刷早了又把续签挡掉。判据统一走 `model::is_effective_on()`：`APPROVED 且 start ≤ 当日 ≤ end`（含边界） |
| **价格条款** | 单头：`price_mode`(FIXED/FLOATING) + `price_adjust_rule`（FLOATING 必填）+ `price_includes_tax` + `price_currency` + `price_tolerance`(0–1)；明细：`agreed_price` + `tax_rate` | 条款分两层：单头是「怎么定价」（合同条款），明细是「这个物料多少钱」（数字）。调价规则塞进明细会被抄 200 遍、改一次漏一行。`price_tolerance` 落库而不是写死 0：允许「5% 以内浮动不重签」是常规条款 |
| **数量上限** | 明细 `max_qty`；单头 `total_max_qty` / `total_max_amount`。三者 **NULL = 不限**，写了必须 > 0 | 两级都存：逐物料限量最常用，一揽子协议常只给总额。**0 不等于不限**（0 上限 = 禁止采购），想不限就写 NULL —— 语义只有一种。**超上限不在库层拦**（与 V0446 头注释 4 同口径）：加急放量现实中会发生，拦在插行只会 500，露头应在 M4-08 的下单校验与 M4-09 的执行量报表 |
| **含供应商** | 单头 `supplier_id` + `supplier_name`（快照）+ 展示侧 join 现名 | 一份协议对一家供应商；**同供应商可有多份协议**（不同类目/期间），所以库层**不做** supplier 唯一 —— 拦了就没法「先签新协议、旧协议到期自然失效」地平滑过渡。「同供应商同物料同期间不该有两份生效协议」是跨行 + 时间窗语义，归 M4-08 |
| **物料范围** | 明细 `material_id` + 唯一索引 `uk_biz_fwai_material (agreement_id, material_id)` | 同一协议同物料只能一行：两行就是两个协议价，校验时取哪行都说不清。同物料换协议可以是另一个价（允许） |
| 状态 | 只有 `DRAFT / SUBMITTED / APPROVED`；`SUBMITTED` 必有审批实例，`APPROVED` 不强求（免审路径） | 与采购计划（V0446）逐字同口径；`EXPIRED/TERMINATED/CLOSED` **都没有入口**，造了就是死状态。真要归档时**追加**迁移扩 CHECK |
| 执行量（卡面未写） | 明细 `executed_qty`/`executed_amount` 回写列 + 单头两个派生合计 | M4-08 下单要在**行锁内**判「还剩多少量」，视图算不出 `FOR UPDATE`；M4-09 再用视图按单据链现算一列 `ordered_qty` 与回写列并排对账（与 V0449 的 `booked_qty`/`req_qty` 同一套做法） |

**数据权限**：与采购计划同口径 **SCOPE-EXEMPT**（公司级采购文件，`dept_id` 是「经办部门」快照语义，
不是数据范围列），不登记 `SCOPED_TABLES` —— 可见性交给 M4-11 的权限码。

**命名避让**：`biz_fixed_asset`（固定资产）已占用 `idx_biz_fa_*` / `ck_biz_fa_*`，所以本卡索引与约束
统一用 **fwa / fwai** 前缀；单据号前缀也从 `FA` 改成 **`FWA`**（`FWA-yyyyMMdd-000001`，
`biz_seq.seq_type='FWA'`）。第一次跑演练就撞了 `idx_biz_fa_status` 已存在 —— 记在这里免得后来人重犯。

## 3. 两个真实修掉的坑

1. **`150%` 写进 `RAISE EXCEPTION` ⇒ 迁移直接编译失败**：PL/pgSQL 把 `%` 当占位符，
   `RAISE EXCEPTION '… 税率 150% 被接受'`（无参数）报 `too few parameters specified for RAISE`。
   改成「税率 1.5 被接受（合法域 0-1）」（或写 `%%`）。
2. **「有 ERROR 就算拒了」的假绿**：演练脚本第一版把反证 SQL 写成
   `'gen_random_uuid()'::uuid`（把函数名当字符串），10 条约束反证全部因为
   `invalid input syntax for type uuid` 报错而「通过」—— 一条约束都没验到。
   改成 `rejects_c <sql> <说明> <约束名>`：**必须命中指定约束名**，并显式排除
   `invalid input syntax / does not exist / cannot cast` 三类「SQL 自己写错」的报错。

## 4. 验收（原始输出在编号文件里）

| 文件 | 内容 | 结果 |
|:--|:--|:--|
| `01_drill_full.out` | 完整演练（含空库全链重放 370/370） | **PASS=63 FAIL=0，rc=0** |
| `02_teeth_skip_guard.out` | 有牙自检：`M407_BAD_URL=1`（连接串故意写错） | **PASS=60 FAIL=2，rc=1**（SKIP>0 + 缺正向标记双判据生效） |
| `03_schema_dump.txt` | 两表 `\d+`、列（含注释）、索引、约束、`biz_seq(FWA)` 行（实库导出） | 223 行 |
| `04_scope_no_routes_menus_frontend.txt` | 范围守卫：路由 0 / 权限码 0 / `INSERT INTO sys_menu` 0 / 订单表新列 0 / 前端 0 | 全部 0 |
| `05_gates.out` | `cargo fmt --check` / `clippy --lib -D warnings` / `clippy --lib --tests -D warnings` | 全绿 0 告警 |

演练脚本的 63 条断言摘要：

1. 模板可克隆 → 45 条 V0405…V0449 迁移推到 V0449（**上 V0450 前框架协议表确实不存在**）
2. V0450 连跑两遍无 ERROR（幂等）；上迁移前后 `sys_menu` 总数不变（**本卡一条菜单都没加**）
3. 结构：2 表 / 29+13 列 / 外键 CASCADE / 7 索引 / 协议号部分唯一索引 / 取号行 FWA 1 行 /
   三个单头查询索引都挂在框架协议表上（不撞固定资产命名）
4. 约束行为（22 条断言）：旧前缀 `FA-…`、空白协议名、起止颠倒、FLOATING 无规则、FLOATING 空白规则、
   小写币种、容差 1.5、总量上限 0、`EXPIRED` 死状态、SUBMITTED 无实例、同协议同物料第二行、
   负协议价、物料上限 0、税率 1.5、负已执行量 —— **全部被库层按指定约束拒**；
   正样本（FLOATING + 5% 容差 + 总额上限 + 两行物料）能插、同供应商第二份协议能插、
   **已执行量 1200 > 上限 1000 能写进去且原样落库**
5. 空库全链重放 `mig_replay_fresh.sh`：**370/370 条版本化迁移按版本号升序全过**
   （V0450 不依赖更晚的迁移、没撞号）
6. 回滚三段：有数据 → 拒（0 行也不允许「顺手抹掉真协议」）；清空 → down 成功（两表 + 取号行）；
   再 up → 复原（29 列）
7. Rust 用例 20 条（8 纯逻辑 + 12 真库），SKIP=0，正向标记 `[M4-07] 表形状/有效期/数量上限/物料范围` 在位
8. 门禁：`permission_audit_tests` 全绿、`menu_invariants.sh` 8 条全绿（菜单总数 548，本卡零新增）
9. 夹具残留：`M407-` 协议 0 / 物料范围 0 / 探针号段 `FWA-19990101-9*` 0

复跑：

```bash
bash scripts/verify/m407_framework_agreement_schema_drill.sh                    # 99 → 145（默认）
PG_HOST=127.0.0.1 bash scripts/verify/m407_framework_agreement_schema_drill.sh  # 在 145 上跑
SKIP_REPLAY=1 bash scripts/verify/m407_framework_agreement_schema_drill.sh      # 跳过空库全链重放
M407_BAD_URL=1 bash scripts/verify/m407_framework_agreement_schema_drill.sh     # 期望报红（有牙自检）
```

## 5. 交接给 M4-08（订单引用：校验价格/数量）

1. **model 已就位**：`framework_agreement_model::{AGREEMENT_TABLE, ITEM_TABLE, SEQ_TYPE, PRICE_MODE_*,
   STATUS_*, PRICE_MODES, STATUSES, AgreementRow, AgreementItemRow, next_agreement_no,
   find_agreement, find_agreement_by_no, items_of_agreement, find_item_by_material,
   lock_agreement, recalc_totals, add_executed, is_effective_on, max_allowed_price, today_cst}`。
   建单/校验不用再猜列名。
2. **两条判据直接用现成的**：
   - 价格：`order_price <= max_allowed_price(item.agreed_price, head.price_tolerance)`
     （`price_tolerance = 0` 就是「必须 ≤ 协议价」）；
   - 有效期：`is_effective_on(head.status, head.start_date, head.end_date, today_cst(now))`。
   数量：`item.max_qty` 为 `None` = 不限；否则 `executed_qty + 本次量 <= max_qty`
   （`AgreementItemRow::remaining_qty()` 已给「剩余可下单」，超限后是**负数**，别截断成 0）。
3. **必须加锁**：校验在 `lock_agreement(id)` 的行锁内做（否则并发下单超卖），
   回写用 `add_executed`（`+=`，不是读改写），写完调 `recalc_totals` 滚单头合计。
4. **订单侧字段由 M4-08 自己加迁移**：`biz_purchase_order(_item)` 上的
   `framework_agreement_id` / `framework_agreement_item_id` 属 M4-08，本卡**没有**动订单表
   （演练第 4 节实测 0 列）。
5. **库层已知不兜的三件事**（要在 handler 里判）：① 同供应商同物料在有效期内不应有两份生效协议
   （库层不做 supplier 唯一）；② 超数量上限；③ 下单价超容差。库层只保证「数据形状合法」。
6. **取号**：`biz_seq(seq_type='FWA')` 已由迁移种下，`next_agreement_no(now)` 按 CST 日滚动
   （`FWA-yyyyMMdd-000001`）。建单前可用 `find_agreement_by_no` 探重，比撞唯一索引报 500 友好。
