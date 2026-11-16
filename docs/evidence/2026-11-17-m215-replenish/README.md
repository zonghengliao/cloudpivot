# M2-15 补货建议（安全库存 + 在途 + 未清订单）

- 卡：`t_527ceaa3`（M2-15【M2·仓库】补货建议：安全库存 + 在途 + 未清订单）
- 上游：M2-13 水位设置 + 生效水位口径（V0428）、M2-14 水位检查（V0429）、M2-04 在途账（V0420）
- 代码：`backend/app/src/business/erp/replenish/{dto,model,handler}.rs`（+ `replenish_db_tests.rs`）
- 迁移：`V0430__erp_replenish_suggestion.{up,down}.sql`
- 接口：`GET /business/erp/replenish/page`（权限码 `business:erp:replenish:list`）
- 验收脚本：`scripts/verify/m215_replenish_fixture.sql` + `scripts/verify/m215_replenish_api_check.py`

## 1. 算的是什么（唯一口径）

```text
可用结存   = biz_inventory.qty（按 物料 × 仓库 合计）− 发货在途（status='IN_TRANSIT' 的 qty_in_transit）
预计可用   = 可用结存 + 未清采购订单未收量
               未收量 = Σ max(0, 项.qty − 已收)，已收 = purchase_model::POSTED_RECEIVED_SQL
               （主单 status ∈ SUBMITTED / APPROVED / PARTIAL_RECEIVED，未软删）
目标水位   = 策略行 max_qty（有则补到最高水位），否则生效最低水位 min_qty
触发条件   = 预计可用 < 生效最低水位（严格小于，与 M2-14 的 `qty < min` 同一刀）
建议采购量 = 触发 ? (目标水位 − 预计可用) : 0
```

三条**不要改**的边界：

| 边界 | 取值 | 为什么 |
|:--|:--|:--|
| 「在途」指哪个 | 发货在途 `biz_inventory_in_transit`（已发未签收） | 货已离仓、账面还没扣（M2-02 两段过账）；只用账面会把「已发走」当仓里有的，建议量偏小 |
| 采购未清订单算不算供给 | 算（且只算 SUBMITTED/APPROVED/PARTIAL_RECEIVED） | 不算 → 同一批货重复下单；算 DRAFT → 把「还没提交、可随时删改」的量当供给，真断料 |
| 已收数量从哪来 | `purchase_model::POSTED_RECEIVED_SQL`（现算） | `biz_purchase_order_item.received_qty` 已由 V0217 删除；自己再算一份就是 P2-3 收敛掉的「双真相」 |

另外三条口径取舍（都有出处）：

- 结存**按 物料 × 仓库 合计**（`biz_inventory` 唯一键含 `location_id`，按库位分成本桶时一格会有多行）——
  「补货是按仓补的、不按库位补」（`docs/ERP-摆设字段口径.md` §2）；
- 负结存 / 负可用**照实算**，不 `GREATEST(qty,0)`（账实不符比「低一点」更该补）；
- 在途仓（`warehouse_type = 'IN_TRANSIT'`）、停用 / 删除的物料与仓库**不出现**（与 M2-14、`reminder_stock` 同口径）；
- 调拨在途**不计**：调拨过账在一个事务里同时写调出仓与调入仓，中途没有可观测的半途量。

## 2. 三种结论

| verdict | 条件 | 默认返回 |
|:--|:--|:--:|
| `PURCHASE` | 建议采购量 > 0 | ✅ |
| `COVERED` | 账面已跌破最低水位，但未清订单 + 在途把缺口盖住（建议 0） | 仅 `include_covered=true` |
| `OK` | 没跌破再订货点（或该格没有水位设置） | ❌ |

`COVERED` 解决的是采购最常问的那句「我刚下完单，为什么清单里没了 / 为什么还让我买」。

## 3. 实测证据（2026-11-17，99 开发机 · 库在 145）

```bash
# ① 空库首启（335+ 条迁移，V0430 由服务自己应用并写 schema_migrations）
createdb -h 172.18.10.145 -U cloudpivot cp_m215_drill
bash /tmp/m215_start.sh            # NIK_SERVER_LISTEN=127.0.0.1:18215，库 cp_m215_drill

# ② 夹具 5 格（表见 fixture 文件头）
psql … -f scripts/verify/m215_replenish_fixture.sql
#   NOTICE: M215 夹具就绪：5 物料 / 5 库存行 / 4 水位 / 3 采购单 / 1 在途

# ③ 真接口
python3 scripts/verify/m215_replenish_api_check.py
#   pass=48 fail=0  ALL PASS

# ④ 模块用例（12 条纯逻辑 + 19 条真库）
ERP_DB_URL="postgres://…@172.18.10.145:5432/cp_m215_drill" \
  cargo test --lib business::erp::replenish -- --test-threads=1
#   test result: ok. 31 passed; 0 failed

# ⑤ 迁移往返（重放幂等 → down → up）
psql … -f V0430__erp_replenish_suggestion.up.sql   # 重放：NOTICE 通过
psql … -f V0430__erp_replenish_suggestion.down.sql # 菜单/授权清 0，V0420/V0428 仍在
psql … -f V0430__erp_replenish_suggestion.up.sql   # 再种回

# ⑥ 门禁
bash scripts/verify/menu_invariants.sh cp_m215_drill          # 8/8 全绿（菜单 525）
cargo test --lib system::menu::permission_audit_tests          # 8 passed
cargo test --lib system::authz_tests                           # 14 passed / 1 既有基线红（o13 · M1-14 现金流 check 用读码）
```

真接口断言里几个关键数字（F1 一格）：结存 30、发货在途 5、未清订单 10（草稿 100 不计）→
可用 25 / 预计 35；最低水位 50（来源 POLICY）、最高 100 → 目标 100 → **建议采购量 65**。

## 4. 下游 / 后续

- 前端页（列表 + 「复制到新采购订单」预填建议量）不在本卡；契约字段已在
  `ReplenishRowRes`（`suggested_qty` / `expected_qty` / `min_source` / `verdict` / `hints`）。
- 单测扩充归 M2-16、收口 E2E 归 M2-18。
- 本卡**不生成采购单**、**不建待办**、**不改水位**：建议量是给人做决定的（订多少、给谁、分几批是采购的判断）。

## 5. 复现注意

- 演练库只建在 145，库名必须含 `drill`/`e2e`/`test`；用完 `dropdb`。
- 隔离实例要显式注入三把密钥（`NIK_MIDDLEWARE_AUTH_JWT_SECRET` / `CLOUDPIVOT_STORAGE_MASTER_KEY` /
  `X_CONFIG_SECRET_KEY`），否则启动预检直接拒绝。
- `M215-` 前缀是本卡夹具专用，测试清场只删这个前缀（不碰生产号段）。
