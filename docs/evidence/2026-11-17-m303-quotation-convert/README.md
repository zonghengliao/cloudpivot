# M3-03 报价单 → 销售订单转换（带行）· 交付证据

- **卡**：t_de446ead（M3-03【M3·销售】报价单：→ 销售订单转换（带行））
- **上游**：M3-01（V0432 建表）/ M3-02（V0433 五个权限码 + 状态机，提交 a6cbf9c2）
- **日期**：2026-11-17
- **一句话**：`POST /business/erp/quotation/{id}/convert` —— 一键把「客户已接受」的报价单转成
  `DRAFT` 销售订单，明细整列搬、金额不重算、来源双向留痕、重复点击幂等、全程一个事务。

## 1 交付清单

| 类型 | 文件 | 说明 |
|------|------|------|
| 接口 | `backend/app/src/business/erp/sales/quotation_handler.rs` | `convert_quotation` + DTO（三段式 `header/items/meta`）+ 纯逻辑 `plan_order_items` / `ensure_convertible` / `parse_convert_body` / `compose_order_remark` + 3 条真库用例 |
| 数据访问 | `backend/app/src/business/erp/sales/quotation_model.rs` | `lock_quotation`（`FOR UPDATE` 锁单）/ `lines_of_in`（锁内读明细）/ `mark_converted`（CAS 回填） |
| 迁移 | `backend/app/migrations/business/V0434__quotation_convert_permission.{up,down}.sql` | 种 `business:erp:quotation:convert`（按钮挂报价单页下 sort=50），不写任何角色授权 |
| 联动登记 | `backend/app/src/business/erp/linkage/spec.rs` | `TRIGGER_EXEMPT` 补 `QUOTATION`（理由见文件内注释）—— 补上游 M3-02 注册实体后漏登记导致的 `triggers_cover_state_machine_entities` 红 |
| 验收脚本 | `scripts/verify/m303_quotation_convert_e2e.sh` + `.py` | 空库首启 → 434 迁移 → 77 条真接口断言 → 落库形态 8 条 → 菜单不变量 8 条 |
| 证据 | 本目录 | 原始输出见 `raw/` |

## 2 口径（决定，不是待办）

| 事项 | 口径 | 为什么 |
|------|------|--------|
| 可转状态 | **只有 `ACCEPTED`** | 销售订单 = 已成交（占信用、出库、开票、生成凭证）。`SENT` 只代表「我方报了价」，客户没表态；转单接口给 409 并要求先走 `PATCH /status` 标接受（那条边自带「未过期」守卫，不需要转单再拦一次） |
| 已接受但今天过期 | **仍可转** | 有效期的约束对象是「客户接受那一刻」；接受发生在期内即成交，事后补单据不该被过期挡（`meta.quotation_expired` 会带出事实，前端可提示） |
| 明细 | 逐行整列搬（物料 / 数量 / 单价 / 税率 / 行金额 / 税额），**不重算金额** | 报价是客户认过的价；重算会引入第二种舍入口径 |
| 金额守卫 | 每行 `amount == round(price×qty,4)`；明细汇总 == 单头三项合计；不一致一律 409 | 库里被绕过写入口改脏时，直接搬过去 = 把脏金额洗成一张要出库、要开票的订单 |
| 来源（结构化） | `biz_quotation.converted_so_id` + `converted_at`（CAS 回填） | V0432 的部分唯一索引保证一张订单只被一张报价认领；反向按 `converted_so_id` 可索引查 |
| 来源（人可读） | 订单备注固定追加「由报价单 QT-… 转换」 | 订单单独导出 / 打印后，只有备注能回答「这单是哪张报价换的」 |
| 来源（轨迹） | `biz_status_log` 一行 `ACCEPTED→ACCEPTED, action=CONVERT, biz_ref_type=SALES_ORDER, biz_ref_no=SO-…` | 状态没变，但这是业务事件；走 `state_machine::log_event`（不是 `transition`，避免伪造边） |
| 幂等 | 已转过再点 → **200 + `meta.already_converted=true` + 原订单**，绝不建第二张 | 双击 / 网络重试是常态；409 会让用户以为没转成 |
| 事务 | 锁单 → 建订单 → CAS 回填 → 写轨迹 **同一个事务** | 与「采购申请 → 采购订单」的两段事务形成对照：任何一步失败整笔回滚，不留孤订单、不留半截回链（约定 11） |
| 并发 | 转单前 `SELECT … FOR UPDATE` | 两个并发请求只有一个能建单；第二个在锁上排队后读到「已转过」，走幂等路径返回同一张订单 |
| 币种 | 账套本位币，`exchange_rate = 1` | 报价单没有币种列（V0432 口径）；外币报价转外币订单要先给报价单补币种列，是另一个决定 |
| 合同 / 销售员 | 都不从报价单继承；销售员可在转单时指定（须启用），合同留空 | 报价单没有这两列；合同额度守卫属于订单侧建单流程，不在这里猜 |
| 订单状态 | 新建即 `DRAFT` | 转单只搬「成交价」；提交审批 / 占信用 / 出库仍走订单自己的流程 |
| 权限码 | 新增 `business:erp:quotation:convert`，**不复用** `:status` / `:create` | 销售员可以自己报价、自己标「客户已接受」；「把报价变成订单」是报价域里权限最重的动作（要占信用、要出库），必须能单独授权。迁移不写任何角色授权（不静默扩权） |
| 请求体 | 空 body = 全默认；有 body 必须合法 JSON（非法 400） | 前端「一键转单」不必先拼 `{}`；`Option<Json<…>>` 在空 body 上会报 `EOF while parsing`（实测），所以用 `Bytes` 手动解析 |

## 3 验证（全部真跑，原始输出在 `raw/`）

### 3.1 端到端演练（空库首启 + 真接口 + 真库 + 真权限）

```bash
bash scripts/verify/m303_quotation_convert_e2e.sh      # 99 开发机 → PG 145
```

`raw/01_e2e_full.out` —— **PASS=24 FAIL=0**，退出码 0：

- 空库首启：`schema_migrations.success = 366`，最大版本 **434**（本卡 V0434 是最后一号）；
- V0434 落地形态 6 条：6 个 quotation 权限码 / 转单码唯一 / 5 个按钮挂在报价单页下 / sort=50 / 零角色授权；
- 真接口断言 **77 条全绿**：转单带行、金额逐项相等、回链、轨迹、幂等（订单数仍 1）、
  DRAFT/SENT/不存在/非法 uuid 四种守卫、转单后作废 409、带参（下单日 / 交期 / 备注 / 非法销售员 / 交期倒挂）；
- 落库形态 8 条：3 张报价单回填 `converted_so_id`、`converted_at` 成对、订单数 == 3（重复点没多建）、
  3 行 CONVERT 轨迹且都指向订单号、订单金额与报价单逐项相等、订单行数 == 报价行数、订单都是 DRAFT；
- 菜单不变量 `menu_invariants.sh` 8 条全绿。

### 3.2 真库单测

```bash
ERP_DB_URL=postgres://…@172.18.10.145:5432/cp_m303_unit_test_<pid> \
  cargo test -p cloudpivot --lib business::erp::sales::quotation -- --test-threads=1
```

`raw/02_cargo_test_real_db.out`：**29 passed / 0 failed**（输出里 `SKIP` 出现 0 次 ⇒ 真库用例真的跑了，
不是「打印 SKIP 记 passed」）。新增 8 条：4 条纯逻辑（转单守卫 / 逐行搬运 / 脏数据四类拒绝 / 备注带来源）、
1 条请求体解析、3 条真库（主链 + 幂等 + 守卫与回滚）。

`raw/03_cargo_test_sales_module.out`：整个 `business::erp::sales` 域 **151 passed / 0 failed**（真库，回归）。

### 3.3 全库单测

`raw/04_cargo_test_full_lib.out`（修联动登记前）/ `raw/05_cargo_test_full_lib_after_fix.out`（修后）：
**5905 passed / 1 failed**，唯一失败是**存量**问题（与本卡无关，见 §4）：

```
system::authz_tests::o13_every_write_route_requires_permission
POST /business/erp/report/cash-flow/worksheet/check → business:erp:cashflow-worksheet:list
```

### 3.4 机械门禁

```bash
bash scripts/check.sh --fast        # L0 + L1：全部检查通过
```

另在**已迁移到 434 的空库**上跑：闸⑥ 死权限码 **PASS**、闸⑦ 孤儿按钮（唯一 1 条是存量，见 §4）。

## 4 已知存量问题（只报不改，不在本卡范围）

1. **`o13` 写接口用读权限码**：`POST /business/erp/report/cash-flow/worksheet/check` 挂
   `business:erp:cashflow-worksheet:list`。该接口按设计「只算不落库」，但 O13 的语义判据只认
   `/preview|/export|/search|/statistics` 后缀。`cashflow_worksheet.rs` 本卡未改动
   （`git status` 为空），失败在 HEAD 即存在。修法二选一：补 `:check` 权限码（要迁移）/
   在 O13 的语义读豁免里显式登记（要评审）。**已另开卡处理**。
2. **闸⑦ 孤儿按钮 1 条**：`FINANCE_DIRECTOR business:erp:receivable:aging` 不可达父页
   （V0418/V0425 存量种的数据，本卡迁移不写任何授权，不可能造成孤儿）。
3. `scripts/verify/README.md` 的脚本清单行未加（该文件正被其它卡改动，避免冲突；合并后补一行）。

## 5 复现步骤

```bash
# 99 开发机（PG 在 145）
bash scripts/verify/m303_quotation_convert_e2e.sh

# 145 测试机（本机 PG loopback）
PG_HOST=127.0.0.1 bash scripts/verify/m303_quotation_convert_e2e.sh

# 真库单测（库名须含 test/drill/e2e，脚本自己 DROP）
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test cp_m303_u_test
ERP_DB_URL=postgres://cloudpivot:***@172.18.10.145:5432/cp_m303_u_test \
  cargo test -p cloudpivot --lib business::erp::sales::quotation -- --test-threads=1
```

> 注意：`cloudpivot_test` 模板库只到 V0404、没有 `biz_quotation`，
> 真库单测的库要**先让服务跑一遍迁移**（空库首启或 `cp_m303_convert_e2e.sh`），
> 否则 `biz_quotation` 不存在会直接报错。
