# P2-61 按域拆分测试套件 —— 实测证据

> 卡：`t_fac8742e`　机器：99（开发机）→ PG `172.18.10.145:5432`　2026-11-15
> 基线 `617c69d2`（P2-60 夹具复用模板之后）　本卡提交见卡内 / `git log --grep=P2-61`

## 结论

| 口径 | 命令 | 条数 | 墙钟 | 红 |
|---|---|--:|--:|--:|
| 改前（单进程全量） | `cd backend/app && time cargo test --lib --workspace -- --test-threads=1` | 5448（kernel 240 / doc 19 **未跑到**，cargo 在 app target 失败后中止） | **2m50.5s** | 12 |
| 改后（19 域并行，jobs=8） | `time python3 scripts/test_domain.py run --package all` | **5707** | **42.2s** | **12（逐条相同）** |
| 改后（单域） | `--package hr` / `--package cloudpivot-kernel` | 695 / 240 | 8.1s / 0.4s | 1（既有）/ 0 |

红集合 `diff` 为空 → 分域跑**没有引入新红**（12 条全是 P2-60 §8 记录的既有红）。

## 文件

| 文件 | 内容 |
|---|---|
| `01-before-single-process.txt` | 改前单进程全量的 `time` 与 `test result:` 原文 |
| `02-after-domains-summary.tsv` | 改后 19 域的逐域条数 / 通过 / 失败 / 墙钟（脚本自产） |
| `03-failures-diff.txt` | 改前 / 改后失败用例清单 `diff`（空）+ 12 条逐条名 |
| `04-gate-negative.txt` | 门禁反证：塞一条未登记前缀的用例 → `exit=1`（有牙） |
| `05-after-parallel-jobs8-console.txt` | 改后并行跑的控制台原文（调度顺序 / 逐域结果 / 合计 / `real`） |

复现：

```bash
ERP_DB=cloudpivot_test_full bash scripts/verify/make_full_suite_db.sh   # 模板库（178 表 / 336 迁移 / 演示种子）
python3 scripts/test_domain.py run --package all                        # 19 域并行
bash scripts/verify/test_domains_gate.sh                                # 用例↔域一一对应（5707 / 19 域）
```
