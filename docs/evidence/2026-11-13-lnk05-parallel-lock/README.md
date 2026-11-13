# LNK-05（`expense_linkage_tests`）清红：并行档假红的真因与修法

卡：`t_cc1e65a4`（P1-01【清红】lnk05 费用联动全链）。证据目录：`raw/`。

## 结论一句话

**不是接线缺失（A），也不是断言过期（B）** —— 是 **C：六条用例共用一批「全库唯一」的
联动规则行，却各自按前缀整片删，并行档（libtest 默认 = 核数线程）互踩**。修法 = 给这批
全局行加一把模块级互斥锁 `expense_linkage_tests::LNK05_LOCK`（与 `fixture_hygiene`
的 `FINANCE_SEED_LOCK` / `SEED_ROLE_LOCK` 同一条铁律「同一批全局行 = 同一把锁」）。

产品代码、断言、迁移**一行未改**。

## 判定依据

| 判据 | 结论 |
|---|---|
| `expense_linkage::run_in_tx` / `run_deferred_after_commit` 存在，且 5 个触发点（提交 / 通过 / 付清 / 作废 / 打回）都在调 | 接线在 ⇒ 排除 A |
| 同库**串行**跑 6 条全绿（`raw/11_before_serial.txt`） | 接线与断言都成立 ⇒ 排除 B |
| 并行档 3 红全部红在「通知必须真的发出一条通知」left: 0 / right: 1 | 是「规则没了」，不是「文案变了」 |
| `biz_flow_linkage_rule` 唯一键 `(trigger_entity, trigger_status, action_code)` | 一行配置对全库生效，**换前缀解决不了**（`PAID/SEND_NOTICE` 这一行 `pay_*` 与 `repeat_*` 都要用） |

## 前后对照（干净库，当前树迁移 V0403 + 演示种子，4 核机器）

| 时机 | 命令 | 结果 | 文件 |
|---|---|---|---|
| 修前 | `cargo test --lib business::erp::expense_linkage_tests`（**默认并行**） | **3 passed / 3 failed** | `raw/10_before_parallel.txt` |
| 修前 | 同库 `-- --test-threads=1` | 6 passed / 0 failed | `raw/11_before_serial.txt` |
| 修后 | 并行（首轮） | 6 passed / 0 failed | `raw/20_after_parallel.txt` |
| 修后 | 并行（复跑 ×2） | 6 passed / 0 failed ×2 | `raw/21_…` / `raw/22_…` |
| 修后 | 串行（卡面验收命令同口径） | 6 passed / 0 failed | `raw/23_after_serial.txt` |

> 修前并行档红了 3 条而不是 6 条：并发交错是概率性的（先跑完的那条可能抢到干净窗口）。
> `docs/evidence/2026-11-13-erp-debt49/18_business_erp_tests.txt` 那次全模块并行档红了 5 条，
> 红点位置完全相同（`expense_linkage_tests.rs:603/662/712/784/837`，全是「通知 0 条」）。
> **红几条取决于交错，红在哪一处不变** —— 这正是夹具竞态、而不是业务断言的形状。

## 复现方法

```bash
# 1) 干净库（当前树迁移 + bootstrap + 演示组织）
psql -h 172.18.10.145 -U cloudpivot -c "create database lnk05_repro owner cloudpivot"
NIK_PROFILE=test NIK_DATABASE_HOST=172.18.10.145 NIK_DATABASE_NAME=lnk05_repro \
NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD=*** \
NIK_SERVER_LISTEN=127.0.0.1:18299 NIK_SEED_DEMO_USERS=true \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) X_CONFIG_SECRET_KEY=$(openssl rand -hex 32) \
  ./backend/target/debug/cloudpivot &   # 等 schema_migrations 到 V0403 且 demo_gm 落库后 kill

# 2) 并行档（修前必红）
ERP_DB_URL=postgres://…/lnk05_repro cargo test --lib business::erp::expense_linkage_tests
# 3) 串行档（修前也绿 —— 这就是「接线在、断言对」的反证）
ERP_DB_URL=postgres://…/lnk05_repro cargo test --lib business::erp::expense_linkage_tests -- --test-threads=1
```

## 为什么不是「把 `--test-threads=1` 写进脚注」了事

`docs/evidence/2026-11-13-erp-debt49/README.md` §9 已经写明「本模块必须 `--test-threads=1`」，
但**全量回归默认并行**，于是这 6 条在 `cargo test --lib` 里恒红 —— 靠调用方约定守不住。
锁把这条约定搬进用例自己身上：并行跑也是「结果与串行一致」，并且不妨碍任何人继续用
`--test-threads=1`（串行时锁永远无人竞争，零代价）。

## 库的处置

`cloudpivot_lnk05_p101` / `cloudpivot_lnk05_p102` 是本次演练库（145），跑完即删，不在 99/145
留库。
