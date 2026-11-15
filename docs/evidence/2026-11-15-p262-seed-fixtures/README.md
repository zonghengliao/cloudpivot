# 证据 · P2-62 数据准备脚本化（统一 `ensure_*` 夹具）

> 卡 `t_efb5b7f1`　父卡 `t_fac8742e`（P2-61 分域）　机器 99 → PG 172.18.10.145
> 设计说明：`docs/architecture/P2-62-数据准备脚本化.md`

| 文件 | 是什么 | 关键数字 |
|---|---|---|
| `static-counts-before.txt` / `static-counts-after.txt` | 测试代码静态盘点（`scripts/verify/test_seed_inventory.py`，HEAD `803e7f7d` vs 本卡） | 测试文件 `INSERT` 380→**356**；主数据 133→**109**；`biz_material` 24→12；`biz_warehouse` 23→11 |
| `bench-fixture-prep.txt` | 数据准备微基准（`#[ignore]`，每轮 200 份夹具，4 次） | 1.787→1.456 / 1.650→1.361 / 1.568→1.301 / 1.591→1.192 ms（**−17~25%**） |
| `ab-before-erp-cost.txt` | 改前：`time cargo test --lib business::erp::cost::`，3 次，每次新克隆库 | 6.92 / 7.22 / 6.96 s |
| `ab-after-erp-cost-run1.txt` | 改后第一轮 | 7.34 / 7.44 / 7.38 s |
| `ab-after-erp-cost-run2.txt` | 改后第二轮（紧接改前跑完，排除负载漂移） | 7.02 / 6.65 / 6.66 s |
| `full-suite-before-summary.tsv` / `full-suite-after-summary.tsv` | 19 域并行（jobs=8）逐域条数与墙钟 | 5707→**5711** 条，12 红两侧相同，37.0s vs 37.3s |
| `full-suite-before-failures.txt` / `full-suite-after-failures.txt` | 失败用例名（12 条既有红） | `diff` **为空**（0 新增、0 消失） |
| `gate-pass.txt` | `bash scripts/verify/test_seed_gate.sh` | ① 41 处转发 ② 23 个导出 ③ 109 处预算 → 全过 |
| `gate-selftest.txt` | `--selftest` 反证：造一个本地 `fn uniq()` 实现 | 被抓到 ⇒ **exit=1**（门禁有效） |

## 复现命令

```bash
# 静态盘点（改前 = 在 HEAD 的干净 worktree 上跑同一条命令）
python3 scripts/verify/test_seed_inventory.py                    # 当前树
python3 scripts/verify/test_seed_inventory.py --root <别的树的 app/src>   # A/B

# 微基准（要真库）
export PGPASSWORD=***  # 见卡面
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test_full cp_p262_bench
cd backend/app && ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cp_p262_bench' \
  cargo test --lib test_seed::tests::bench_fixture -- --ignored --nocapture
dropdb -h 172.18.10.145 -U cloudpivot cp_p262_bench

# 单域 A/B（每个 DB 只跑一次，跨轮不可复用）
createdb -h 172.18.10.145 -U cloudpivot -T cloudpivot_test_full cp_p262_ab
cd backend/app && ERP_DB_URL='postgres://cloudpivot:***@172.18.10.145:5432/cp_p262_ab' \
  cargo test --lib business::erp::cost:: -- --test-threads=1

# 全量分域
cd /opt/cloudpivot && python3 scripts/test_domain.py run --package all --jobs 8

# 门禁 + 反证
bash scripts/verify/test_seed_gate.sh; echo "rc=$?"          # 期望 0
bash scripts/verify/test_seed_gate.sh --selftest; echo "rc=$?"  # 期望 1
```

## 口径说明（别误读）

* 单域墙钟的 ±5% 是**噪声**：99→145 的 PG 同一时刻还有别的卡的用例在跑（P2-61 实测同一条域
  在并行时 7.5s、单跑 7.2s）。判断「数据准备快不快」看 `bench-fixture-prep.txt`，不看端到端墙钟。
* `5707 → 5711` 是**新增 4 条夹具自测**（`test_seed::tests::*`），另 1 条 `#[ignore]` 基准不计入。
  分域门禁 `scripts/verify/test_domains_gate.sh` 已同步（`platform` 域登记 `test_seed::`）。
