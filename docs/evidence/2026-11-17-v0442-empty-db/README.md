# 证据：V0442 让空库首启直接失败（P0 · 卡 t_08e1816a）

结论与决策见 **[`docs/architecture/P0-V0442-空库首启与迁移不许依赖种子行-决策.md`](../../architecture/P0-V0442-空库首启与迁移不许依赖种子行-决策.md)**。
本目录只放**原始输出**（可复跑，脚本在 [`scripts/`](./scripts/)）。

所有命令都在 `172.18.10.145:5432` 上跑（库名以 `cloudpivot_test_v0442*` 命名，跑完 dropdb 清场）。

## 索引

| 文件 | 是什么 | 关键读数 |
|---|---|---|
| `01_manual_apply_v0442_real_error.txt` | 在「只跑完版本化、没跑种子」的库上手工 psql 应用 V0442 | `versioned:361 / seed:0`、`系统管理=0 行`、`销售管理=1 行`、`sys_role=10`；报 `ERROR: V0442: 祖先目录「系统管理」(1000…0001) 不存在` |
| `02_grants_baseline_dev_vs_fresh.txt` | `cloudpivot_dev`（已种子）vs 全新库的 V0442 授权面 | dev 21 行 / 新库 0 行（修前）；N20 六码在新库 0 持有者 |
| `03_drift_probe_fresh_vs_dev.txt` | 「缺祖先的授权对」（`build_tree` 会静默丢节点） | 两库都是 **0**（⇒ V0442 跳过闭包不会留隐患） |
| `04_cfg_category_fresh_vs_dev.txt` | `sys_config.category` vs `config_category` 字典 | 441 状态库 7/7 覆盖；dev 9/9；**`erp_fx` 已不存在**（V0441 合并） |
| `05_static_gate_measure_before.txt` | 静态判据定型前的度量（口径演进留痕） | 版本化迁移以 `id` 列 INSERT 的 uuid 903 / seed 49；硬护栏命中 2 处（1 处为 V0442，1 处为哨兵误报） |
| `06_before_fix_kernel_log.txt` | **修前**空库首启的内核日志 | `数据库迁移失败: query failed: : db error` + `FATAL`（真因被吞） |
| `07_make_full_suite_db_after_fix.txt` | **修后** `make_full_suite_db.sh` 全量输出 | 自检 21 项全 ✓、`sys_menu=540`、`max version=443` |
| `08_fresh_db_grants_after_fix.txt` | 修后新库的 V0442 授权面 | 16 行确定性 id 授权、N20 六码全有非超管持有者、`missing_ancestor_pairs=0`、`notice:mine` 按钮已归位 |
| `09_checksum_calibration_and_datafix.txt` | 指纹口径校准 + datafix 双向取证 | 旧版实算 = dev 记录（逐字节）；不修指纹 ⇒ `migration version 442 … was modified`；修后 ⇒ `数据库迁移完成: 应用 0 个迁移`；再跑 no-op |
| `10_gate_mutation_proof.txt` | 门禁变异反证 | 修后 0 命中；把 V0442 换回旧版 ⇒ **1 命中** |
| `11_check_l0.txt` | `bash scripts/check.sh --l0`（含新闸） | `EXIT=0`；含新区块「种子行依赖门禁 · 判据自检」4 条语料全绿 + 扫描 |
| `12_v0442_idempotent_and_rollback.txt` | up→up→down→up 手工重放 | 16 → 16 → 0 → 16 行；父节点 `…0010 ↔ …0410` 往返正确 |
| `13_gates_on_fresh_db.txt` | 新库上的孤儿按钮闸 + 菜单不变量 | 孤儿 (角色,码) = **0**；`menu_invariants` 8/8（总数 540 未涨） |
| `14_cfg_category_gate_fresh.txt` | 新库上的分类闸 | 9 个分类 / 字典覆盖 9 个 ⇒ PASS |
| `15_diagnostics_before_after.txt` | 同一句错误在「修前二进制 / 修后二进制」下的两次输出 | 修前 `query failed: : db error`；修后 `query failed: : V0442: 祖先目录…[P0001]` |

## 复跑（脚本原样拷入）

```bash
cd docs/evidence/2026-11-17-v0442-empty-db/scripts
bash proof_roundtrip.sh          # 12：up/up/down/up 幂等与回滚
bash verify_fresh_grants.sh <新库>  # 08：新库授权面
bash proof_gate_mutation.sh      # 10：门禁变异反证
bash proof_checksum_fix.sh       # 09：指纹校准 + datafix 双向
bash proof_diagnostics.sh        # 15：诊断力（需旧版迁移副本，脚本自建临时 profile）
python3 measure_seed_dep4.py     # 05：静态判据的度量口径
```

> 脚本里的库名前缀 `cloudpivot_test_v0442*` 与 `172.18.10.145` 是本机实测环境；换环境时改
> 脚本头部的 `H/U/PGPASSWORD`，库名换成自己独占的空库（**不要**指向 `cloudpivot_dev`）。
