# D3 证据索引（2026-11-03）

卡面：t_61baf976 —— 接管财务联动 `GENERATE_VOUCHER`（先抽公共函数 → SHADOW 并行对比 → TAKEOVER）
说明文档：`docs/跨单据联动动作接管-D3.md`

| 文件 | 内容 | 结果 |
|---|---|---|
| `01-unit-tests.txt` | 联动模块单测（真库 `cloudpivot_d3u`，`ERP_DB_URL` 已设，非假绿） | **35 passed / 0 failed**（D2 的 28 + 本卡 7） |
| `02-e2e.txt` | 真机 e2e（隔离实例 18096 / 库 `cloudpivot_d3`）：无规则基线 → SHADOW 对比 → TAKEOVER → 脏配置 / 模板停用 / 期间已结账 | **PASS=56 FAIL=0** |
| `03-invariants-e2e-db.txt` | ERP 不变量，在 **e2e 库**上跑（库里有配置生成的 5 张凭证 + 库存 + 应付） | **36/36，BLOCKER 0 / WARN 0** |
| `04-invariants-clean-db.txt` | ERP 不变量，干净库 `cloudpivot_d3inv2` | **36/36，BLOCKER 0 / WARN 0** |
| `05-full-suite.txt` | 全量后端单测（干净库 `cloudpivot_d3inv`，`--test-threads=1`） | 见文件尾 `test result` / `CARGO_EXIT` |
| `06-stockin-regression.txt` | 收货过账用例回归（**零改动**，证明抽公共函数后行为不变） | **133 passed / 0 failed** |
| `07-voucher-regression.txt` | 凭证用例回归（**零改动**） | **41 passed / 0 failed** |
| `08-fmt-clippy.txt` | `rustfmt +1.94.1 --edition 2024 --check`（本卡 7 个文件）+ `cargo clippy --lib -D warnings` | 无差异 / 通过 |
| `09-scope-gate.txt` | 数据权限闸②（本卡文件） | PASS（全库基线 27 处均为存量） |
| `10-git.txt` | `git show --stat HEAD`（提交与改动清单） | — |

复跑方式（隔离库与实例已在跑时）：

```bash
bash    scripts/verify/d3_instance.sh start
python3 scripts/verify/d3_generate_voucher_e2e.py
bash    scripts/verify/d3_invariants.sh cloudpivot_d3      # e2e 库
bash    scripts/verify/d3_invariants.sh                    # 干净库
bash    scripts/verify/d3_unit_tests.sh                    # 联动模块
bash    scripts/verify/d3_full_suite.sh                    # 全量
```

> 本卡的库命名约定：`cloudpivot_d3`（e2e 实例）/ `cloudpivot_d3u`（模块单测）/
> `cloudpivot_d3inv`（全量套件）/ `cloudpivot_d3inv2`（干净库不变量）。生产库零触碰。
