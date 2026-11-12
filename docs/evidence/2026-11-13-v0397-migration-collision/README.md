# V0397 迁移号段撞车 —— 实测证据（卡 t_83977c43）

> 结论与处置方案见 **`docs/architecture/P0-V0397-迁移号段撞车-决策.md`**。
> 本目录只放**原始输出**：每一条都是真跑出来的，不是推演。

## 一、事故一句话

`main` 上 `system/V0397__preference_palette_ids_gallery77`（主题配色，`2609c7f6`）与
`business/V0397__hr_salary_period_after_reverse`（HR-40，`83a4cfab`）抢同一个版本号。
nik 的迁移加载器把 `migrations/{system,support,business}` 合成**一张槽位表**，重号 ⇒
加载阶段 `duplicate up migration for version 397` + `FATAL`。
**这条失败与库状态无关：空库炸，已迁满的库重启也炸。**

## 二、证据清单（每条都标了「证明了什么」）

| 文件 | 场景 | 实测结论 |
|---|---|---|
| `01_empty_db_duplicated_FATAL.out` | 空库 + 两条 V0397（main 原样） | `rc=1`，`duplicate … version 397` + `FATAL`；库里 `schema_migrations` **根本没被建**（`to_regclass` = false）⇒ 一条迁移都没执行 |
| `02_relocate_system_v0397_then_migrate_full.out` | 把 system/V0397 临时移出 → 空库首启 | `应用 329 个迁移`（318 版本化 + 11 种子），`/health` 200（4s）——**用来造一个「已迁满的库」** |
| `03_migrated_db_restart_FATAL.out` | 把 system/V0397 放回（= 重号复原）→ **对上面那个已迁满库重启** | `rc=1`，同一条 `duplicate up migration for version 397` + `FATAL`；`schema_migrations` 重启前后都是 329 行（**未变**）⇒ 失败发生在读迁移记录之前。**⇒ 生产 144 只要发布 main，`systemctl restart` 就起不来** |
| `04_renumbered_empty_db_start.out` | 改号（system/V0397 → **V0399**）后空库首启 | `应用 330 个迁移`（319 版本化 + 11 种子），`/health` 200（4s） |
| `05_renumbered_restart_0_pending.out` | 改号后二次启动（已迁满库） | `应用 0 个迁移`（稳态快路径），`/health` 200 |
| `06_stale_old_number_record_start.out` | 已应用旧号（记录仍是 `preference_palette_ids_gallery77@397`）的库启动 | **正常启动**，`应用 0 个迁移`。记录表主键是 `(kind, name)`、**不含 version** ⇒ 新号不会被重跑 —— 因为本迁移内容与旧号逐字节相同，「不重跑」恰好等于「已经跑过」，**无需任何指纹处置** |
| `07_content_changed_checksum_FATAL.out` | 对照组：改号时**顺手往 V0399 里加一行注释**，同一库启动 | `rc=1`，`migration version 399 … was modified after it was applied: recorded checksum … != current …` + `FATAL`（默认 `divergent_policy = abort`）⇒ **改号可以，改内容（哪怕只加一行注释）不行** |
| `11_prod_144_migration_state.out` | 生产 144 的真实迁移状态 | `version = '397'` 的记录 **0 行**；`max_applied_version = 391`；部署树 `HEAD = 6807bb5e`、磁盘迁移上限 `V0391` ⇒ **两条 V0397 都没上过生产**（改号的判据成立） |
| `12_repo_tree_empty_db_start.out` | 改号落地后，从**真实仓库树**（`/opt/cloudpivot`）空库首启 | `应用 330 个迁移` + `/health` 200 |
| `13_check_sh_l0_on_repo.out` | `bash scripts/check.sh --l0`（真实仓库树） | `全部检查通过（L0）`，其中新门禁：`649 个文件 / 319 对 up-down / 399 个版本，最大 V0399`，`下一个可用版本号：V0400` |
| `08/09/10_gate_l0_*.out` | 新门禁的有牙演练 | 干净树 `rc=0` → **注入**一个重复 `V0399`（`business/V0399__dup_probe`）后 `rc=1` 且指名两个文件 → 撤掉后 `rc=0` |
| `run.sh` | 上面所有条目的复现脚本 | `bash run.sh t1 / t3a / t3b / t4 / t4b / t5 / t5c / t6`（子命令见脚本头注释） |

## 三、复现（本机 = 172.18.10.99，库 = 172.18.10.145）

```bash
# 前置：root@172.18.10.145 免密可读 /etc/cloudpivot/cloudpivot-dev.env；后端已 cargo build --release
WT=/root/arch-v0399/wt  bash run.sh t1     # 空库 + 重号 → FATAL（复现）
WT=/root/arch-v0399/wt  bash run.sh t3a    # 移出一条 → 迁满（造「已迁移库」）
WT=/root/arch-v0399/wt  bash run.sh t3b    # 重号复原 → 已迁移库重启也 FATAL
WT=/root/arch-v0399/wt  bash run.sh t4     # 改号后空库首启 → 绿
bash run.sh clean                          # 清掉 v0399_* 演练库
```

> 所有演练库名都带 `v0399_` 前后缀、端口落在 18000–18999 隔离段，跑完由 `clean` 子命令 DROP；
> 全程不碰生产 144 的库，也不重启现网进程。

## 四、这起事故为什么此前没被任何卡发现

1. 各卡的真库回放脚本（`docs/evidence/*/0*_db_tests.sh`）是自己
   `ls migrations/*/*.up.sql | sort -n` **逐条 psql 回放**，**不经过 nik 加载器** ——
   「319 条回放全绿」只能证明 SQL 能跑，证明不了版本号唯一。
2. 唯一一个名字看起来在管这件事的测试
   `backend/src/business/hr/hr_invariants_tests.rs::selfcheck_migration_has_a_down_and_a_unique_version`
   **只扫 `V0398__` 这一个前缀**，对别号的重复完全无感（名字承诺了「跨目录全局唯一」，实现没做到）。
3. 静态门禁里没有这一项，`scripts/check.sh --fast` 全绿也拦不住。

⇒ 处置里补的 `scripts/lint/migration_version_unique.py` 是**唯一**能提前拦住它的位置（详见决策文档 §5）。
