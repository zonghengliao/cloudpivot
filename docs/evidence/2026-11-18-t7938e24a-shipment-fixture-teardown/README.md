# t_7938e24a 证据：shipment_posting / shipment_receipt_tests / receivable_bridge 收尾清场（发货单 + 在途行）

卡面：`shipment_posting` / `shipment_receipt_tests` / `receivable_bridge` 造完发货单与在途行后不收回，
同一库连跑第二轮全量时 `inventory_transit::transit_query_tests` 8 条全表口径断言被别人的残留在途行打红
（`left: 2 / right: 1`）。上游 `t_9c3551b0` 只在 `inventory_transit` 侧兜了底（造数前清「非本模块」在途行），
源头没修。

本卡把源头改成**每个套件在自己的收尾用例里清干并断言残留 0**，不依赖下游替它们收拾、也不依赖下一轮开头。

## 1. 根因（三条，都不只是「忘了 DELETE」）

| # | 根因 | 证据 |
|:--|:--|:--|
| ① | 三套件的 `cleanup()` 只在 `setup()` **开头**跑 ⇒ **最后一条用例**造的发货单 / 在途行 / 出库单 / 应收 / 凭证全部留在库里 | §2 反证：跳过收尾用例再跑一遍，`biz_shipment` 3→6、`biz_inventory_in_transit` 3→5，多出来的正是 `SO10T-SH-…/IN_TRANSIT` 与 `SO10R-SH-…/RECEIVED`（与卡面查库结果同形） |
| ② | `receivable_bridge` 的清场顺着**客户链**反查（`customer_id IN (…)`），漏掉幽灵客户夹具（`missing_customer_rolls_back_stock_and_receivable` 用随机 uuid 当客户）⇒ 这类订单 / 发货单 / 出库单 / 凭证一辈子清不掉 | 修前收尾用例：`清场后 SO14T- 夹具必须 0 残留 left: 6 / right: 0` |
| ③ | `shipment_receipt_tests` 的状态日志只删 `STOCK_OUT`/`SHIPMENT`/`SALE_ORDER` 三类，**漏了 `RECEIVABLE`**；三套件都**没有**删 `VOUCHER` 类日志（签收确认 / 出库过账都会发成本结转凭证）⇒ 每跑一遍多一批孤儿 | 修前收尾用例：`状态日志必须按单据 id 先删 left: 1 / right: 0`；修前在脏库上按「引用已失效」扫 `biz_status_log`，一次列出 84 行孤儿（`VOUCHER`/`RECEIVABLE` 两类占绝大多数） |

## 2. 改法（三个文件，只动 `#[cfg(test)]` 代码）

1. **收尾用真实 `cleanup()`，不另写一份 SQL**：每个套件补 `zz_teardown_purges_fixtures`
   （libtest 按用例全名排序，`zz_` 保证本模块最后跑）：走真实入口造一张全链单 → 断言清场前 > 0（防空跑）→
   调 `cleanup()` → 按**单号前缀**断言残留 0 → 按**清场前抓到的 id** 断言在途 / 出库 / 应收 / 凭证 / 日志都没了。
2. **清场判据改成本模块自己的单号前缀**（`SO14T-` / `SO10R-` / `SO10T-`），不再顺着客户链反查 —— 覆盖幽灵客户夹具。
3. **状态日志按 `ENTITY_TABLES` 逐类对齐**：补 `RECEIVABLE` 与 `VOUCHER` 两支，且**日志先删、子单据后删**
   （子查询依赖那些行还在；顺序反了会静默留孤儿）。
4. **手工领用出库单打夹具标记**：`receivable_bridge` 的 `NewStockOut` 现在把 `remark = "SO14T-MANUAL-STOCK-OUT"`，
   清场按这个标记反查（这张单 `source_type`/`source_id` 都是 NULL、单号又是生产号段 `OUT-YYYYMMDD-NNNNNN`，
   按号段既清不到又可能误删真数据）。手工建单逻辑抽成 `manual_stock_out()`，用例与收尾共用一份。
5. `shipment_receipt_tests` 收尾额外用 `test_seed::purge_customers("SO10R-CUST")` 清掉本文件独占的客户行
   （物料 / 仓库是 `test_seed` 的跨模块共用夹具，不删）。

## 3. 验收（全部真跑，原始输出在本目录）

| 判据 | 结果 | 文件 |
|:--|:--|:--|
| 全新种子库 + 同一库连跑两轮全量 | 第一轮 `6464 passed; 1 failed`，第二轮 `6449 passed; 16 failed`；**本卡相关族 0 条红** | `01_round1_full.txt` / `02_round2_full.txt` |
| 卡面症状（第二轮 `transit_query_tests` 8 条全红） | 两轮 8/8 **全绿** | 01 §「逐条结果」、02 §「逐条结果」 |
| 两轮后本套件夹具残留 | `SO14T-/SO10R-/SO10T-` 的发货单 / 订单 / 客户 **全部 0**；在途行只剩 `inventory_transit` 自己的 3 行 M204 夹具（两轮计数不变，不是增长） | `03_residue_after_two_rounds.txt` |
| 反证：跳过三个收尾用例 = 复现修前行为 | 三套件各跑一遍后 `ship=3→6 transit=3→5 out=0→3 ar=21→23 vou=20→22 log=81→99`，多出来的在途行是 `SO10T-SH-…/IN_TRANSIT` 与 `SO10R-SH-…/RECEIVED`（与卡面一致） | `04_baseline_without_teardown.txt` |
| 断言有牙（RED 反证） | 把 `shipment_receipt_tests::cleanup()` 里「删 `SO10R-` 订单」改成永不匹配前缀 → 收尾用例红在 `清场后 SO10R- 夹具必须 0 残留 left: 8 / right: 0`；还原后转绿 | `05_red_proof.txt` |
| 目标模块两轮（同一库，未重造） | 四套件两轮全绿；`SO*` 残留 0 且两轮之间各表计数不变（ship 3 / transit 3 / out 0 / ar 21 / vou 20 都是别人的 M204 等夹具） | `06_targeted_two_rounds.txt` |

**第二轮 16 条红的归因（都不是本卡）**

- `system::menu::permission_audit_tests::n18_route_level_exception_ledger_is_live`：HEAD 上就红 ——
  `git show HEAD:backend/app/src/business/erp/purchase/framework_agreement_dto.rs` 第 21/569 行写着
  `business:erp:framework-agreement:page`（M4-11b 遗留，已开卡 `t_23aad8fd`）。
- `cost_period_approval_db_tests` 5 条 + `period::tests` 10 条：`FINANCE_MANAGER` 被别家夹具绑上
  `tst02b.finance_manager`（单据夹具整表清角色绑定）+ 成本批次 `CS-203109-0028` 停在「已提交待审批」挡住
  2031-09 结账 —— 与上游 `t_85e005d9` 第二轮同族，已由它开卡 `t_18b222b9`。

## 4. 卡面第 2 条（衍生行核查）的结论

- `biz_stock_out` / `biz_receivable` / `biz_voucher` / 三本账：**确实漏清**，本卡已纳入三套件的 `cleanup()`
  并在收尾用例里按 id 断言（见 §1 ③ 与 §2）。
- 卡面提到的「`biz_voucher` 里 1 张 `source_no` 为空的 `9006` 草稿」：**不是这条链**。它是
  `business::erp::report::fx_report_tests`（M109 夹具）的 `9006` 张，跑完全量后常驻 6 张
  （`summary='M109 夹具'`，`9001~9005 POSTED` + `9006 DRAFT`），数量稳定不随轮次增长。已另开后续卡。

## 5. 复现命令

```bash
# 全新种子库
ERP_DB=cloudpivot_test_shipclean bash scripts/verify/make_full_suite_db.sh
export ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cloudpivot_test_shipclean'
export SYS_DB_URL="$ERP_DB_URL" TASK_DB_URL="$ERP_DB_URL" P0FIX_DB_URL="$ERP_DB_URL"
cd backend/app

# ① 两轮全量（同一库，不重造）
cargo test -p cloudpivot --lib -- --test-threads=1
cargo test -p cloudpivot --lib -- --test-threads=1

# ② 卡面验收 SQL（两轮后应为 0）
psql "$ERP_DB_URL" -Atc "select count(*) from biz_shipment where ship_no like 'SO14T-%' or ship_no like 'SO10R-%' or ship_no like 'SO10T-%'"
psql "$ERP_DB_URL" -Atc "select count(*) from biz_inventory_in_transit t join biz_shipment s on s.id=t.shipment_id where s.ship_no like 'SO14T-%' or s.ship_no like 'SO10R-%' or s.ship_no like 'SO10T-%'"

# ③ 反证：跳过收尾用例（= 修前行为），残留必须重新出现
cargo test -p cloudpivot --lib -- --test-threads=1 business::erp::receivable::receivable_bridge --skip zz_teardown_purges_fixtures
cargo test -p cloudpivot --lib -- --test-threads=1 business::erp::shipment::shipment_posting --skip zz_teardown_purges_fixtures
cargo test -p cloudpivot --lib -- --test-threads=1 business::erp::shipment::shipment_receipt_tests --skip zz_teardown_purges_fixtures
```

## 6. 一个跑法坑（卡面「复现」里的 `-- shipment_` 过滤不可用）

`cargo test -- <filter>` 过滤的是**用例全名**，`shipment_` 会连带命中 `receivable_bridge` 里两个名字含
`shipment_` 的用例（如 `post_shipment_creates_receivable_in_same_transaction`），但**不会**命中它们的
`zz_teardown_purges_fixtures` ⇒ 跑完仍会留 1 张 `SO14T-` 发货单 + 在途行，看着像没修。
按模块跑要用全路径：`business::erp::receivable::receivable_bridge` / `business::erp::shipment::shipment_posting`
/ `business::erp::shipment::shipment_receipt_tests`。

## 7. 影响面

零迁移、零权限码、零菜单行、零生产代码 —— 改动全在三个 `#[cfg(test)]` 测试文件里
（`receivable_bridge.rs` 的 `mod tests`、`shipment_posting.rs` 的 `mod tests`、`shipment_receipt_tests.rs` 整文件）。
