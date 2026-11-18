# t_60c193ef 证据：`report::tests`（ERPR1）跑完全量后常驻 1 张凭证 + 2 条分录 + 2020-01 期间

卡面：`business::erp::report::tests`（ERP-R1 资产负债表，`backend/app/src/business/erp/report/mod.rs`，
`FIXTURE_WORD = "ERPR1"`）的 `setup_fixture()` 只在**开头**清场，所以本模块 libtest 排序最后一条跑
（`r1_real_previous_period_comparison` ⑭，造 `0201`）留下的东西一直在库里：

```text
ERPR1凭证=1（0201 POSTED，summary='ERPR1 夹具'、期间 2026-08）
ERPR1分录=2
2020期间=1（period_code='2020-01'，setup 里 DELETE + INSERT 的）
2019期间=0
```

后果不是「下游断言变红」，而是「全量跑完净增量 = 0」这类统计永远非 0（姊妹病灶 `t_255fb28e`
已修 `fx_report_tests`，本卡收同一目录的 `report::tests`）。

## 1. 根因

| # | 根因 | 证据 |
|:--|:--|:--|
| ① | 清场只在 `setup_fixture()` 开头跑 ⇒ **本模块最后一条用例**（⑭ 造 `0201`）造的凭证留给下一轮 | §3 `07b_skip_zz_residue.txt`：跳过收尾用例再跑本模块，库里立刻回到 `ERPR1凭证=1 ERPR1分录=2 2020期间=1`（与卡面查库结果逐字一致，明细 `0201|ERPR1|2026-08|POSTED|ERPR1 夹具`） |
| ② | 同一个 `setup_fixture()` 会重建 `2019-12` / `2020-01` 期间，同样没有收尾 ⇒ 本模块独占的 `2020-01` 常驻 | 同上：`2020期间=1`；修后两轮全量后 `2020期间=0` |

**卡面第 2 条「日志必须先删」的口径需要说明**：`report::tests` 的夹具是**裸 SQL** 灌 `biz_voucher` 的，
它们自己不写 `biz_status_log`（该表没有触发器），所以修前库里的 `VOUCHER` 孤儿数本来就是 0 ——
「删日志」那条腿在真实数据上**没有被测到**。因此收尾用例**显式补一条真实链路（`state_machine::transition`）
会写的建单日志**当牙齿（见 §2 第 3 点），否则「日志 0 条」的断言等于断言空集合（假绿）。

## 2. 改法（单文件 `backend/app/src/business/erp/report/mod.rs`，只动 `#[cfg(test)]` 代码）

1. **`setup_fixture()` 开头的清场抽成 `purge_fixtures()`**，顺序固定
   「**先按凭证 id 删 `VOUCHER` 状态日志 → 再删凭证 → 最后删本模块独占的两个期间**」，
   判据沿用 `voucher_word = 'ERPR1'`（本模块独占；不按单号前缀猜 —— 生产号段是 `PZ-YYYYMMDD-*`），
   返回删掉的凭证行数。`setup_fixture()` 里原来那两条 `DELETE`（凭证 + `2019-12`/`2020-01` 期间）改调它
   （行为不变，清场判据只加不减）。
2. **⑫ / ⑬ 里两处内联 `DELETE FROM biz_voucher WHERE voucher_word = 'ERPR1'` 改调 `purge_fixtures()`**
   —— 语义相同，但连带把状态日志与夹具期间一起收，不再各写一份 SQL。
3. **补 `zz_teardown_purges_fixtures`**（libtest 按用例全名排序，`zz_` 在本模块最后跑）：
   造 `0201` → 断言清场前凭证数 = 1（防空跑）→ 抓 id → **给这张凭证补一条 `VOUCHER` 状态日志**
   （`entity_no = voucher_no`、`to_status = 凭证状态`）→ 断言清场前日志 = 1 → 调 `purge_fixtures()` →
   断言删掉 1 张、按 `voucher_word` 数 0 张、**按清场前抓到的 id** 断言日志 0 条 →
   断言 `2019-12 / 2020-01` 期间残留 0。
4. **收尾兜历史孤儿**：`fixture_hygiene::purge_orphans(&["VOUCHER"])` + `orphans(&["VOUCHER"])` 断言 0。

**没动的东西（边界）**：

- 7 条真库用例的断言与夹具口径（`2026-09/10/11`、`2000-12`、`2020-01`、三张凭证金额）一字未动；
- `2000-12` 期间**不收**：`income_statement`（R2-⑦b）/ `cash_flow`（R3-⑳）也用它做「空期间全 0」夹具，
  不是本文件独占，单方面删只会把别人的前提变成「靠我这轮没跑干净」；
- `2026-09/10/11` 期间同理（与 `income_statement` / `cash_flow` 共用，且 `ON CONFLICT DO NOTHING` 幂等共存）；
- 未碰任何生产代码。

## 3. 验收（全部真跑，原始输出在本目录）

| 判据 | 结果 | 文件 |
|:--|:--|:--|
| 全新种子库 `cloudpivot_test_erpr1`（迁移最新版本 460 / 自检 21 项全 ✓ / `SEED_RC=0`） | 通过 | `00b_dbseed_final.out` |
| 同一库连跑**两轮全量**（`--test-threads=1`，CWD=`backend/app`） | 第一轮 **6566 passed / 0 failed / 1 ignored**（214.9s）；第二轮 **6566 passed / 0 failed / 1 ignored**（217.8s）→ 两轮一致 | `01_round1_summary.out` / `03_round2_summary.out` |
| 两轮后残留（`biz_voucher` / 期间 / 孤儿日志） | 两轮后均 `ERPR1凭证=0 ERPR1分录=0 2019期间=0 2020期间=0 VOUCHER孤儿日志=0` | `02_residue_after_round1.txt` / `05_residue_after_round2.txt` |
| 本模块单独跑 | `18 passed / 0 failed`（原 17 条 + 新 zz 收尾） | `06_module_green.out` |
| `psql … select count(*) from biz_voucher where voucher_word='ERPR1'` | `0` | `04_erpr1_vouchers_after_module.txt` |
| `cargo fmt -p cloudpivot -- --check` | `FMT_RC=0` | `12_fmt_check.out` |
| `cargo clippy -p cloudpivot --lib --tests -- -D warnings` | `CLIPPY_RC=0`（零告警） | `13_clippy.out` |

### 反证（断言必须有牙）

| # | 做法 | 结果 | 文件 |
|:--|:--|:--|:--|
| ① 跳过收尾用例 | `cargo test … business::erp::report::tests:: --skip zz_teardown` 再查库 | **修前症状原样复现**：`ERPR1凭证=1 ERPR1分录=2 2020期间=1 2019期间=0`，明细 `0201\|ERPR1\|2026-08\|POSTED\|ERPR1 夹具` | `07_skip_zz_module.out` / `07b_skip_zz_residue.txt` |
| ② 删日志腿改永不匹配 | `purge_fixtures` 里 `entity_type = 'VOUCHER'` → `'VOUCHER-NEVER'` | **RED**：`清场后这 1 个 id 上的 VOUCHER 状态日志必须 0 条（日志没有外键，必须先删）left: 1 / right: 0` | `09_teeth_a_logs_leg.out` |
| ③ 删凭证腿改永不匹配 | `voucher_word = '{FIXTURE_WORD}'` → `'NEVER-{FIXTURE_WORD}'` | **RED**：`清场必须删掉 1 张夹具凭证 left: 0 / right: 1` | `10_teeth_b_voucher_leg.out` |

还原后 `sha256sum backend/app/src/business/erp/report/mod.rs = 0124f30ad23e28511034cd194b1eba20c3f9e112e6b04e928a986903c018457e`
（与改动完成后、变异之前的备份**逐字节一致**），随后本模块再跑一次 18 passed 复绿（`11_after_restore_module_green.out`）。
变异脚本 `mutate.py` 的等价物在本目录 `repro_two_rounds.sh` 同级的操作记录见 §2（变异只用于反证，未入提交）。

## 4. 复现方式

```bash
cd /opt/cloudpivot
bash docs/evidence/2026-11-18-t60c193ef-report-erpr1-fixture-teardown/repro_two_rounds.sh
```

（脚本自带：全新种子库 → 两轮全量 → 两轮后查残留；`ERP_DB_URL` 指向 145 的 `cloudpivot_test_erpr1`。）

## 5. 附注

- 两轮之间 `biz_voucher` 总数 12 → 13：多出来那 1 张**不是 ERPR1**（`voucher_word` 逐条核过），属别的模块的
  已知残留面，与 `t_255fb28e` 的「26 张表两轮增长」清单同族，不在本卡范围。
- 本卡的 `zz_teardown_purges_fixtures` 与同目录 `fx_report_tests.rs`、`shipment`、`stock_policy`、`hr::leave_*`
  等模块的同名收尾用例**互不影响**（判据各自按自己的 `voucher_word` / 表收敛）。
