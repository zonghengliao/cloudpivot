# t_255fb28e 证据：`fx_report_tests` 跑完全量后常驻 6 张 M109 夹具凭证

卡面：`business::erp::report::fx_report_tests`（M1-09 报表取数按币种）的夹具清场只在 `setup_fixture()`
**开头**跑，所以本模块最后一条用例造的 6 张凭证（`9001~9005 POSTED` + `9006 DRAFT`，
`voucher_word='ERPFX9'`、`summary='M109 夹具'`、`source_no` 全 NULL）一直留在库里。
后果不是「下游断言变红」，而是「全量跑完净增量 = 0」这类统计永远非 0，且这些凭证的状态日志会在
下一次清场删掉凭证之后成为**孤儿**。

## 1. 根因（两条，都在同一个 `setup_fixture()` 里）

| # | 根因 | 证据 |
|:--|:--|:--|
| ① | 清场只在 `setup()` 开头跑 ⇒ **最后一条用例**造的 6 张凭证留给下一轮 | §3 反证 `07_skip_zz_repro.out`：跳过收尾用例再跑一遍本模块，库里立刻回到 `9001:POSTED … 9006:DRAFT` 六张（与卡面查库结果逐字一致） |
| ② | 同一个 `setup_fixture()` 还会整体重建 **2064-09 / 2064-10 期间**与 **2064 年 USD 牌价**，而它们同样没有收尾 ⇒ 本文件常驻 2 + 2 行（卡面没提，一并收） | 修前实测（跳过收尾）：`2064期间=2 2064牌价=2`；修后 `0 / 0` |

**卡面第 3 条推断需要更正**：这 6 张凭证是**裸 SQL** 灌进 `biz_voucher` 的，它们**自己不写**
`biz_status_log`（该表没有触发器）。所以修前库里的 `VOUCHER` 孤儿数**就是 0**，
「这 6 张的日志变成孤儿」并没有发生。但清场顺序仍然按「**先删日志、再删凭证**」写
（业务入口造的凭证一定带日志，顺序反了会静默留孤儿），并由收尾用例**显式补一条日志**
把这条腿钉住（见 §2 第 3 点），否则「日志 0 条」的断言等于断言空集合（假绿）。

## 2. 改法（单文件 `backend/app/src/business/erp/report/fx_report_tests.rs`，只动 `#[cfg(test)]` 代码）

1. **`cleanup()` 抽成 `purge_fixtures()`**：判据沿用 `voucher_word = 'ERPFX9'`（本文件独占，不按单号前缀猜），
   顺序固定「先按凭证 id 删 `VOUCHER` 状态日志 → 再删凭证」，返回删掉的凭证行数。
   `setup_fixture()` 里原来那行 `DELETE FROM biz_voucher …` 改调它（行为不变，清场判据只加不减）。
2. **补 `zz_teardown_purges_fixtures`**（libtest 按用例全名排序，`zz_` 在本模块最后跑）：
   造 6 张 → 断言清场前 = 6（防空跑）→ 给这 6 张补一条真实链路会写的 `VOUCHER` 状态日志
   （`entity_no = voucher_no`、`to_status = 凭证状态`）→ 断言清场前日志 = 6 → 调 `purge_fixtures()` →
   断言删掉 6 张、按 `voucher_word` 数 0 张、**按清场前抓到的 6 个 id** 断言日志 0 条。
3. **收尾兜历史孤儿**：`fixture_hygiene::purge_orphans(&["VOUCHER"])` + `orphans(&["VOUCHER"])`
   断言 0（修前每一轮 `setup` 删凭证都会把上一轮的日志变孤儿，没人收）。
4. **顺带收本文件独占的 2064 期间 / 牌价**并断言残留 0（根因 ②）——不收的话「净增量 = 0」仍然非 0。

## 3. 验收（全部真跑，原始输出在本目录）

| 判据 | 结果 | 文件 |
|:--|:--|:--|
| 全新种子库（`cloudpivot_test_fxm109`，392 条迁移 / V0460 / 自检 21 项全过） | `RC=0` | `00_dbseed.out` |
| 同一库连跑**两轮全量**（`--test-threads=1`，CWD=`backend/app`） | 第一轮 **6565 passed / 0 failed / 1 ignored**（214.0s）；第二轮同样 **6565 passed / 0 failed / 1 ignored**（214.6s） | `01_round1_summary.out` / `03_round2_summary.out` |
| 两轮后本卡判据（卡面验收 SQL） | `M109夹具凭证=0`、`ERPFX9凭证=0`、`VOUCHER孤儿日志=0`、`2064期间=0`、`2064牌价=0`（**两轮都一样**） | `02b_residue_after_round1.txt` / `05_residue_after_round2.txt` |
| 模块单跑 7 条全绿 + 清场打印 | `7 passed`；`[M1-09] 清场：删夹具凭证 6 张、其 VOUCHER 状态日志 6 条` → `M109 夹具凭证 0 张、其状态日志 0 条、历史孤儿已收 0 条、2064 期间与牌价 0 行 ✓` | `06_module_green.out` |
| **反证①（复现修前症状）**：跳过收尾用例再跑本模块 | 残留立刻回来：`9001:POSTED,9002:POSTED,9003:POSTED,9004:POSTED,9005:POSTED,9006:DRAFT` + `2064期间=2 2064牌价=2`；把收尾用例放回来再跑一次 → 全部 0 | `07_skip_zz_repro.out` / `07b_after_repro_clean.out` |
| **反证②（断言有牙 · 日志腿）**：把 `purge_fixtures()` 删日志那条腿的定位词改成永不匹配 | 收尾用例 RED：`清场后这 6 个 id 上的 VOUCHER 状态日志必须 0 条 left: 6 / right: 0` | `09_teeth_a_logs_leg.out` |
| **反证③（断言有牙 · 凭证腿）**：把删凭证的 `voucher_word` 改成永不匹配 | 收尾用例 RED：`清场必须删掉 6 张夹具凭证 left: 0 / right: 6` | `10_teeth_b_voucher_leg.out` |
| 变异还原（`cp` 备份 + `sha256sum` 逐字节校验 `630f818d…`）后复绿 | 7 passed；`M109夹具凭证=0`、`VOUCHER孤儿日志=0`；`cargo fmt -p cloudpivot -- --check` rc=0；`cargo clippy -p cloudpivot --lib --tests -- -D warnings` 零告警 | `08_teeth_precheck.out` / `11_after_restore_green.out` |

### 两轮后仍然增长的表（**不是本卡**，只是把口径写清）

`02_counts_after_round1.txt` vs `04_counts_after_round2.txt`：194 张表里 26 张两轮之间有增量
（`sys_notice` 303→604、`biz_doc_attachment` 20→40、`biz_flow_instance` 17→22、
`biz_inventory_txn` 38→70、`biz_voucher` 13→14 …），逐条核对**没有一条属于本模块**：
`biz_voucher` 的 +1 来自 `P2C2/P2D1/记/ERPR1` 一类别的夹具（见 `biz_voucher` 的 `voucher_word`），
而 `ERPFX9 / M109` 两轮后都是 0 张。这些表的增量属于各自的卡，本卡不动。

## 4. 复现命令

```bash
# 全新种子库（145）
cd /opt/cloudpivot
ERP_DB=cloudpivot_test_fxm109 PORT=18255 bash scripts/verify/make_full_suite_db.sh
export ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cloudpivot_test_fxm109'
export SYS_DB_URL="$ERP_DB_URL" TASK_DB_URL="$ERP_DB_URL" P0FIX_DB_URL="$ERP_DB_URL"

# 同一库连跑两轮全量（CWD 必须是 backend/app）
cd backend/app
cargo test -p cloudpivot --lib -- --test-threads=1
cargo test -p cloudpivot --lib -- --test-threads=1

# 卡面验收 SQL（期望 0，修前 6）
psql "$ERP_DB_URL" -Atc "select voucher_no, status from biz_voucher where summary='M109 夹具' order by 1"
psql "$ERP_DB_URL" -Atc "select count(*) from biz_status_log where entity_type='VOUCHER' and entity_id not in (select id from biz_voucher)"

# 反证：跳过收尾用例 = 修前行为（6 张立刻回来）
cargo test -p cloudpivot --lib -- --test-threads=1 business::erp::report::fx_report_tests \
  --skip zz_teardown_purges_fixtures
```

## 5. 边界与发现

- **没改任何断言期望值**：7 条既有用例的断言、夹具口径（2064 / USD / EUR / 六张凭证）一字未动；
  新增的只有清场函数抽取 + 一条收尾用例。
- **判据只加不减**：`purge_fixtures()` 的定位判据仍是 `voucher_word='ERPFX9'`；新增的孤儿收干与
  2064 期间/牌价清理各自带 0 残留断言，不删任何别人的行（`purge_orphans` 只删「引用已失效」的日志）。
- **发现的姐妹病灶（本卡不修，已开卡）**：`business::erp::report::tests`（`report/mod.rs`，
  `FIXTURE_WORD="ERPR1"`）同样只在 `setup_fixture()` 开头清场，两轮后常驻
  `1 张凭证（0201 POSTED）+ 2 条分录 + 1 个 2020-01 期间`。
- **已知遗留（本卡未动）**：`setup_fixture()` 会整体删掉 USD/EUR/HKD→CNY 的**全部**牌价再种 2064 的
  两行（注释声明这三对本用例独占）⇒ 2026 年的模板牌价行会被第一次运行抹掉。这与本卡症状无关，
  改它要重新评估其它外币用例的牌价前置，留给后续卡。
