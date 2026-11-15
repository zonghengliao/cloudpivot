# 证据 · P2-64 耗时预算告警（单测 > 5000 ms 时提示）

> 卡 `t_03abd8fa`　父卡 `t_0406d7e8`（P2-63 JUnit 报告）　机器 **99** → PG `172.18.10.145`（模板库 `cloudpivot_test_full`）
> 设计说明：`docs/architecture/P2-64-耗时预算告警.md`

| 文件 | 是什么 | 关键数字 |
|---|---|---|
| `ab-parallel.txt` | **改前/改后 A/B**：19 域并行 + JUnit，只差「预算告警开/关」，各 2 轮 | 改前 38.45 / 38.03 s；改后 37.90 / 38.29 s；四轮都是 5699 pass / **12 红**（失败名单 diff 为空） |
| `single-ab.txt` | **字面口径 A/B**：一次单进程全量 `cargo test -p cloudpivot --lib -- --test-threads=1`，各 2 轮 | 改前 169.89 / 169.60 s；改后 172.20 / 169.81 s（差异属主机抖动，见 `alarm-cost.txt`） |
| `alarm-cost.txt` | 告警本身的微基准（对真实 5712 条产物，n=7 取 min/median） | 读 XML+判定 **10.8 ms** + 拼文本 0.095 ms + 写 5712 行 TSV 1.9 ms = **12.8 ms（0.03%）**；CLI 端到端 57 ms |
| `budget-real-parallel.txt` | 真实产物上的告警输出（19 域合并的 `junit.xml`） | 5712 条已量、**1 条超预算 `n14…` 9016.8 ms**、`rc=0`（只提示不阻断） |
| `budget-real-single.txt` | 同上，单进程全量产物的告警输出 | 5453 条已量、同样命中 `n14…` 8874.7 ms |
| `budget-top30.tsv` | 告警产出的机器可读清单前 30 行（改动前 `--tsv` 全文 5712 行） | 第 1 名 9017 ms、第 2 名 3060 ms、第 30 名 322 ms |
| `budget-unmeasured.txt` | **反证**：`from-logs` 回填的真实 XML（每条 `time=0.000`） | 报「0 条已量 / 5712 条未量 ⇒ **预算未判定**」，`--require-measured` ⇒ `rc=2`（不静默合格） |
| `selftest.txt` | `python3 scripts/junit_report.py selftest` | **27 条断言全过**（新增 ⑰–㉒：命中 / 降阈值 / 边界 5000 不算超 / XML 往返 / 未量不算合格 / 接线） |
| `gate-pass.txt` | `bash scripts/verify/test_budget_gate.sh` | 九组断言全过 → `rc=0`（夹具边界 + 2 处反证 + 接线） |
| `single-pipe-probe.txt` | 支线探针：单进程全量 stdout 接 `tee` 管道（不经 Python） | 仍是 **12 红**、164.53 s ⇒ 12↔10 的红集合差异不是管道造成的（详见设计 §7.3 注） |
| `failures-diff.txt` | 改前/改后失败名单逐条 diff | **空**（12 条既有红逐条相同：本卡 0 新增 0 消失） |

## 三条结论（都能在这些文件里复核）

1. **告警不拖慢跑批**：同并行度关/开告警 38.45/38.03 vs 37.90/38.29 s（噪声级）；告警本身是**纯后处理**，
   实测 **12.8 ms**（对 38 s 是 0.03%），CLI 端到端 57 ms。
2. **改前/改后全量测试仍全绿（= 12 条既有红一条不多）**：分域并行四轮失败名单逐条相同、`diff` 为空；
   字面单进程口径改前 169.6~169.9 s、改后 169.8~172.2 s（见上面「主机抖动」说明）。
3. **告警上线首日就抓到真东西**：5711 条里 1 条超预算 ——
   `system::menu::permission_audit_tests::n14_menu_auth_codes_have_source_action_point`（9.0~9.3 s），
   机制是「每个菜单权限码都在整份源码文本上重扫」⇒ O(码数 × 文件数)，建议单独开卡改成一次索引。
