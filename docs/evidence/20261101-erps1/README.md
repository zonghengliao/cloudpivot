# ERP-S1 证据：标准基础数据预置（V0187）

> 卡：`t_eee6774a`【ERP-S1】预置标准基础数据（24 个计量单位 + 物料分类 + 结算方式，幂等）
> 提交：`84fb290` feat(erp)…V0187；`a0aff53` test(verify)…真机验收脚本
> 生产：172.18.10.144 / `cloudpivot` 库 / `cloudpivot.service`（重启后自动迁移）
> 状态：已部署、已验收（迁移自检 PASS、干跑 38/38、真机 15/15、ERP 不变量 31/32 BLOCKER 0）

---

## 一、动手前实测的现状（卡面是草图，表结构以实测为准）

| # | 卡面假设 | 实测 | 处理 |
|---|---|---|---|
| 1 | `biz_unit` 有 `precision` 列 | **没有** `precision`，也**没有** `is_system` | 迁移第 ① 步补两列（硬红线 4：先查结构再写迁移） |
| 2 | `on conflict (unit_code) do nothing` | 唯一索引是**部分索引** `uk_biz_unit_code … WHERE deleted_at IS NULL` | 必须写 `on conflict (unit_code) where deleted_at is null do nothing`；裸写法报 `no unique or exclusion constraint matching the ON CONFLICT specification` |
| 3 | 有 `biz_material_category` 表 | **没有**该表，`biz_material` 也没有 `category` 列（`pg_tables like '%categ%'` 零行） | 物料分类的真身是 `material_type`（4 值 CHECK）→ 扩到 7 值 |
| 4 | 结算方式「有独立表或字典」 | 没有独立表；`biz_payment.pay_method` 是**自由文本**，无枚举无字典 | 种字典类型 `pay_method`（7 项，取值用中文原串与老数据同形） |
| 5 | 仓库类型「看要不要补」 | `warehouse_type` 枚举已 5 值（NORMAL/RAW/FINISHED/SCRAP/IN_TRANSIT），生产已有 WH01/WH02/WH-TRANSIT | 不重复造仓库、不造死枚举（造了就是没消费方的假数据） |

原始查询（生产库，改代码前）：

```
biz_unit 列：id, unit_code, unit_name, is_base, base_unit_id, rate, status, sort_order,
             remark, created_at, updated_at, create_by, update_by, deleted_at
biz_unit 行：PCS 个(base,rate=1) / KG 千克(base,rate=1) / BOX 箱(base=0 → PCS, rate=12)
字典类型    ：config_category, config_type, job_status, log_*, menu_type, user_status（10 个，无 pay/settle）
物料分类    ：SEMI×1, RAW×4, FINISHED×2（共 5 行未删；枚举 4 值）
```

---

## 二、迁移 `V0187__erp_standard_master_data`

```
① biz_unit ADD COLUMN is_system BOOLEAN NOT NULL DEFAULT false
                 precision  SMALLINT NOT NULL DEFAULT 0  + ck_biz_unit_precision(0..6)
                 + 部分索引 idx_biz_unit_is_system（只索引预置行）
② 28 个标准单位编码（原有 PCS/KG/BOX 3 个 → 实际新增 25 个）
   ON CONFLICT (unit_code) WHERE deleted_at IS NULL DO NOTHING
③ 历史 3 行回填 is_system / precision：只在「编码 + 名称仍与 V0137 种子逐字一致」时生效
④ ck_biz_material_type：4 值 → 7 值（新增 PACKAGING / CONSUMABLE / OFFICE）
⑤ 字典 pay_method 7 项（现金为默认项，item_value 用中文原串）
⑥ 自检 DO 块：单位数 ≥24、无重复、28 编码逐个在位、精度分组、分类 7 值、字典 7 项
```

单位清单（生产实测 28 行，`precision` 分组见第四列）：

| 段 | 编码 | 小数位 |
|---|---|---|
| 数量 | PCS 个 / SET 套 / BOX 箱 / PACK 包 / PAIR 双 / DOZEN 打 | 0 |
| 重量 | KG 千克 / G 克 / T 吨 / LB 磅 | 3 |
| 长度 | M 米 / CM 厘米 / MM 毫米 / KM 千米 / FT 英尺 | 3 |
| 面积体积 | M2 平方米 / M3 立方米 / L 升 / ML 毫升 / GAL 加仑 | 3 |
| 包装 | ROLL 卷 / BAG 袋 / BOTTLE 瓶 / CAN 罐 / TUBE 支 | 0 |
| 时间 | HOUR 小时 / DAY 天 / MONTH 月 | 0 |

> 卡面写「补 24 个」，实际列了 **28 个编码**；减去原有 3 个 = 本次新增 **25 个**。按编码逐个种，不复用卡面的数字口径。

---

## 三、干跑（生产 schema 副本，不动生产）

`bash scripts/verify/erps1_standard_data.sh` —— 自建 `cloudpivot_s1v`（`pg_dump 生产 → 新库`），跑完自毁：

```
PASS  迁移 up 执行成功
PASS  迁移内置自检块通过（NOTICE: V0187 自检通过：单位 28 个（其中标准 28 个、系统预置 28 个）、物料分类 7 值、结算方式 7 项）
PASS  单位总数 28 ≥ 24
PASS  标准单位数量（28）
PASS  本次新增单位数量（28 - 原有 3）（25）
PASS  关键单位（千克 / 米 / 箱）都在，物料下拉能选到（BOX,KG,M）
PASS  单位编码无重复（0）
PASS  重跑 up 不报错（幂等）           ← 卡面单测②
PASS  重跑后单位总数不变（28）
PASS  重跑后结算方式字典项仍是 7 个（7）
PASS  客户改的名字（公斤）没被迁移改回「千克」（公斤）   ← 卡面单测③
PASS  改过名的行不会被标成系统预置（仍是客户自建 false）
PASS  标准单位全部标了 is_system = true（28）          ← 卡面单测④
PASS  客户自建单位（列默认值）is_system = false（f）
PASS  清掉自建单位后回到 28（28）
PASS  数量 / 包装 / 时间类单位 precision = 0（0）
PASS  重量 / 长度 / 面积体积类单位 precision = 3（0）
PASS  3 位小数的单位 14 个（14）
PASS  precision = 9 被 ck_biz_unit_precision 拦住
PASS  分类 RAW / SEMI / FINISHED / PACKAGING / CONSUMABLE / OFFICE / SERVICE 都在 CHECK 里（7 条）
PASS  已有物料的分类值全部仍在枚举内（零影响）（5）
PASS  非法分类被 ck_biz_material_type 拦住
PASS  字典类型 pay_method 唯一（1）
PASS  字典项 7 个 / 取值正好是卡面 7 种 / 现金是默认项
PASS  down 执行成功 / 只剩 3 个单位 / 字典已删 / 两列已删 / PACKAGING 被拒（CHECK 收回 4 值）
PASS  重放后单位回到 28
PASS=38 FAIL=0
```

---

## 四、生产部署

```
prod HEAD: 5444da7 → 84fb290（ff 对齐）
二进制：/root/rel-0095-target/release/cloudpivot md5 70b1d3d5b3da43e4bb22d2b7c8dbc4e5
        /proc/<MainPID>/exe == target/release/cloudpivot（进程==磁盘，md5 一致）
journal：NOTICE: V0187 自检通过：单位 28 个（其中标准 28 个、系统预置 28 个）、物料分类 7 值、结算方式 7 项
         数据库迁移完成: 应用 1 个迁移
health ：HTTP 200
```

行数前后（同一条 SQL，部署前 → 部署后）：

```
biz_unit          3 → 28        （+25，本卡种下）
sys_dict_type    10 → 11        （+pay_method）
sys_dict_item    71 → 78        （+7 结算方式）
biz_material      5 → 5         （未动）
biz_account      56 → 56        （未动）
biz_voucher      11 → 11        （未动）
biz_inventory     3 → 3   biz_inventory_txn 15 → 15   （未动）
biz_payable       8 → 8   biz_payment 0 → 0           （未动）
biz_purchase_order 11 → 11  biz_stock_in 10 → 10      （未动）
schema_migrations max(version) 186 → 187
```

---

## 五、真机验收（`scripts/verify/erps1_prod_e2e.py`，15/15 PASS）

```
PASS  管理员登录成功（access_token 已取到）
PASS  ① 计量单位页 total = 28（≥ 20）                       ← 卡面验收①
PASS  ① 每条出参都带 is_system / precision（28 条）
PASS  ① 系统预置单位 28 个（is_system = true）
PASS  ② 单位下拉能选到「千克」（KG，precision=3）              ← 卡面验收②
PASS  ② 单位下拉能选到「米」（M，precision=3）
PASS  ② 单位下拉能选到「箱」（BOX，precision=0）
PASS  ② 千克 precision = 3（重量类 3 位小数）
PASS  ③ 系统预置单位（千克）删除被拒 409：「千克」是系统预置单位，不能删除；不用了就把它停用（status = 0）
PASS  ③ 客户自建单位 is_system = false、precision = 2（接口不开放 is_system 开关）
PASS  ③ 客户自建单位可以删除（200）
PASS  ④ 物料分类新增值 PACKAGING（包装物）建得动
PASS  ④ 非法分类被拒 400：material_type 必须是 RAW / SEMI / FINISHED / PACKAGING / CONSUMABLE / OFFICE / SERVICE 之一（当前 `BOGUS`）
PASS  ⑤ 结算方式字典 pay_method 7 项：现金, 银行转账, 承兑汇票, 支票, 微信, 支付宝, 赊账
PASS  清理完成：物料 / 单位无 S1-E2E 残留行
PASS=15 FAIL=0
```

前端 dist 已发布（nginx 根 `/opt/cloudpivot/frontend/dist`）：入口 chunk 旧 `BasicLayout-4msyjFRE.js`
→ 新 `BasicLayout-B5BiL3e0.js`，新包里能查到「系统预置单位不能删除」「小数位数」
「低值易耗品」「unit-source-tag」（`index-BVWjuICj.js` / `index-BLEvZFkY.js`），首页 HTTP 200。

---

## 六、测试

| 套件 | 结果 |
|---|---|
| 前端 `pnpm test:unit` | **343 passed / 0 failed**（新增 2 条 ERP-S1 用例：来源标签+不可删、小数位数进请求体+校验） |
| 前端 `pnpm typecheck` | 绿 |
| 后端 `cargo fmt -p cloudpivot -- --check` / `cargo clippy -- -D warnings` / `cargo check` | 全绿 |
| 后端 `cargo test -j 1 --lib business::erp`（本卡代码，生产副本库） | 446 passed / 53 failed |
| 后端 baseline（`5444da7` 源码，同一份生产副本） | 443 passed / 52 failed |

**失败集合逐条 diff**（`comm`）：本卡代码比 baseline **多通过 3 条**（= 新增用例
`unit_precision_bounds` / `material_types_cover_seven_categories` /
`system_unit_cannot_be_deleted_but_can_be_disabled` / `unit_precision_round_trip_and_rejects_overflow`），
仅 `cash_flow::tests::r3_real_config_covers_all_accounts` 一条在两轮里表现不同（我的那轮 FAIL、baseline 那轮 PASS）。
单点复核：在**同一份干净的生产副本**（已含 V0187）上分别用本卡源码与 baseline 源码跑这条用例 →

```
本卡代码（HEAD+s1 改动）: test result: ok. 1 passed; 0 failed
baseline 源码（origin/main）: test result: ok. 1 passed; 0 failed
```

⇒ 那一条是**跨轮次库污染**（第一轮跑在同一库上，作废类用例留下的 PENDING_CANCEL 单据影响了取数），
不是回归。53 条失败全部是 baseline 既有的（收货作废审批 / 期间数据依赖生产副本里的存量单据）。

ERP 不变量：`python3 scripts/verify/erp_invariants.py` → **31/32 通过，BLOCKER 0，WARN 1**
（`[期间] F2 凭证期间必有期间记录 — 2 张凭证的期间无记录`，与卡前基线完全相同）。

---

## 七、代码改动

| 文件 | 改动 |
|---|---|
| `backend/migrations/business/V0187__erp_standard_master_data.{up,down}.sql` | 新增：两列 + 25 个单位 + 分类 7 值 + 结算方式字典 + 自检；down 可回滚 |
| `backend/src/business/erp/model.rs` | `UNIT_COLUMNS` / `UnitRow` 增 `is_system`、`precision` |
| `backend/src/business/erp/dto.rs` | `MATERIAL_TYPES` 4→7；`Create/UpdateUnitReq.precision`（0–6 校验）；`UnitRes/UnitCard` 出参两字段；单测改成新口径 + 新增 2 条 |
| `backend/src/business/erp/handler.rs` | 建单位恒 `is_system=false`、`precision` 落库；编辑可改 `precision`；**删除系统预置单位 409**「不能删除，只能停用」；新增 2 条真库用例 |
| `frontend/src/api/business/erp/index.ts` | `MaterialType` 7 值；`Unit/UnitRow` 增 `is_system`/`precision`；`Create/UpdateUnitReq.precision` |
| `frontend/src/views/business/erp/unit/unit-query.ts` | 列增「来源」「小数位」；`unitSourceLabel/Theme`、`canRemoveUnit`、`precisionLabel`；表单与请求体带 `precision`（0–6 校验） |
| `frontend/src/views/business/erp/unit/index.vue` | 「系统」标签（`data-testid=unit-source-tag`）、小数位列、小数位输入；系统预置行的删除入口换成「系统预置」提示 |
| `frontend/src/views/business/erp/material/material-query.ts` | 分类下拉 7 项（原材料/半成品/产成品/包装物/低值易耗品/办公用品/服务） |
| `frontend/src/views/business/erp/payable/index.vue` | 结算方式下拉改为读字典 `pay_method`（失败回落 7 项常量），由 4 项扩到 7 项 |
| `scripts/verify/erps1_standard_data.sh` | 新增：38 项 SQL 断言（含卡面 4 条单测、幂等、down 往返、重放） |
| `scripts/verify/erps1_prod_e2e.py` | 新增：真机 15 项 HTTP 验收 + 自清理 |

---

## 八、与卡面的偏差 / 取舍（必须看）

1. **卡面「补 24 个」→ 实际 28 个编码 / 新增 25 个**：卡面列表本身就是 28 个（3 个已存在），按编码种、不按数字凑。
2. **`precision` 是新增列**：卡面假设已有；实测没有。精度口径按卡面「重量/长度 3 位、个数 0 位」，
   面积体积按同一逻辑归 3 位、时间归 0 位（迁移自检里逐组断言，写进迁移头注释）。
3. **物料分类没有独立表**：不新造 `biz_material_category`（造了没有消费方），而是把
   `material_type` 枚举由 4 值扩到 7 值，后端常量 + DB CHECK + 前端联合类型/下拉三处同步。
4. **结算方式用字典而不是新表**：`pay_method` 是自由文本列，字典取值用中文原串（老数据不用迁移），
   付款弹窗从字典取选项、拉不到时回落常量。
5. **仓库类型不补**：枚举已有 5 值且覆盖原料/成品/不良/在途，生产已有 3 个仓库（在途仓正在被调拨引用）——
   再加默认仓库会引入「没有库位/没有科目的空仓库」，属于净负担。
6. **系统预置单位删除 = 409**（卡面只说「不能删只能停用」，没说接口行为）：文案点名「不能删除，只能停用」，
   停用（`status=0`）照常允许；前端把删除入口换成「系统预置」提示，避免点了才吃 409。

## 九、已知风险 / 后续

- `precision` 目前是**主数据属性 + 录入/展示定标**，库存数量计算（`NUMERIC(18,4)`）尚未按单位小数位做舍入 ——
  真要在数量运算上生效属于库存引擎改动，不在本卡范围。
- 物料分类新增 3 值（PACKAGING/CONSUMABLE/OFFICE）已在建/改物料与列表筛选可用；若后续导入模板/报表要按分类统计，
  需要再确认口径（本次没动导入导出与报表）。
- 生产副本库 `cloudpivot_s1v` / `cloudpivot_s1b` / `cloudpivot_s1c` 已随脚本自毁（`S1_KEEP=1` 可保留）；
  测试角色 `s1role`（superuser，仅测试机内部用）仍在，可在评审后 `DROP ROLE s1role`。
