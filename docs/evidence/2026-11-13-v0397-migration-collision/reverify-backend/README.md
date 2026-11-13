# V0397 撞号事故 · 后端独立复核证据（backend-developer · 卡 t_b4cc9e22）

复核对象：`main`（`16cbad90` 改名提交，本目录复核时工作区 `HEAD=12d20c7d`，`16cbad90` 为其祖先）。
结论：**撞号已解除，改动形态正确（纯改名、内容零改动），空库/已迁满库/已应用旧号的库三种形态都能启动，门禁有牙。**
本目录是独立复跑产物，不引用卡 t_83977c43 的 `docs/evidence/2026-11-13-v0397-migration-collision/` 输出。

## 改动形态（静态）

| 事实 | 证据 |
|---|---|
| `system/V0397__preference_palette_ids_gallery77` → `system/V0399__…`，up+down 成对 | `git show --stat -M 16cbad90`；`ls backend/migrations/system/V0399__*` |
| 改名**未动内容**（git blob hash 相同 ⇒ 一字节未改） | 已应用旧号的库不会因指纹不一致而中止（见 C2），另 `git rev-parse 16cbad90^:…V0397…up.sql` 与 `HEAD:…V0399…up.sql` 同为 `b82e4ab1…` |
| `business/V0397__hr_salary_period_after_reverse`（HR-40）保持不动 | `git log -- backend/migrations/business/V0397__*.up.sql` 只有 `83a4cfab` |
| 版本号全局唯一（按**数值**，跨 system/support/business） | `E1_independent_version_uniqueness.out`（独立脚本，未复用仓库 lint 工具）：`files=638 versions=319 max=V0399`，重复号 0、同名跨版本 0、up/down 配平 0 缺失 |

## 运行时复核（每步都是真起进程 + 真库）

脚本：`mig_drill.sh`（本目录，可原样复跑）。库：本机 PG `alun/alun`，端口 18073/18074，profile=test，三个密钥显式注入。

| 步骤 | 做什么 | 期望 | 实测（见同名 `.out` / `.log`） |
|---|---|---|---|
| `R` | 把 drill 库重置为空 | 无 `schema_migrations` | `schema_migrations 存在吗：f` |
| `A` | **空库首启**（仓库树，system/V0399 + business/V0397） | health 200 + 全量迁移 | `health=200 after 6s`、`数据库迁移完成: 应用 330 个迁移`、`palette 记录@399`、`ui.palette 白名单 id=99`、真实失败行数 **0**（`A1_…trimmed`） |
| `B` | 二次启动（已迁满） | 0 待应用、行数不变 | `应用 0 个迁移`、330→330（`B_restart_0_pending.log`） |
| `N` | **负向对照**：把文件改回 `system/V0397`（复原撞号） | 启动即 FATAL、库内零表 | `EXITED rc=1`、`ERROR nik_web::app: 数据库迁移失败: migration error: duplicate up migration for version 397: '…business/V0397__hr_salary_period_after_reverse.up.sql' conflicts with '…system/V0397__preference_palette_ids_gallery77.up.sql'; identities must be globally unique across all sources`、`schema_migrations 存在吗：f`（`N_duplicate_number_FATAL.log`） |
| `C1` | 基线：145 `cloudpivot_dev` 的真实 dump 副本（本地 `cp_mig_devcopy`） | 记录仍是旧号 397 + 旧指纹 | `schema_migrations 行数=333`、`preference_palette_ids_gallery77@397`、白名单 99 |
| `C2` | **已应用旧号 397 的库 + 仓库树（V0399）启动** | 能起、不重跑、不改记录 | `health=200 after 4s`、`应用 0 个迁移`、`FATAL 行数=0`、无 checksum/divergent 报错、`palette 记录` 仍 `@397` 且只 1 条、白名单仍 99（`C2_oldnumber_db_start.log`） |
| `D1/D2/D3` | 门禁三态（在 scratch 副本树上，不动仓库） | 绿 → 红 → 绿 | `rc=0` / `rc=1`（点名两文件）/ `rc=0` |
| `F1` | 真实仓库树 `bash scripts/check.sh --l0` | 全绿 | `==> 全部检查通过（L0）`、迁移段 `649 个文件 / 319 对 / 399 个版本，最大 V0399`、`下一个可用版本号：V0400` |
| `G1` | 生产 144 只读核对 | 397/398/399 一条都没上 | `schema_migrations` 中 version ∈ {397,398,399} **0 行**、`max_applied_version=391`、`palette_name_rows=0`、部署树 `HEAD=6807bb5e`、磁盘迁移上限 V0391、systemd `cloudpivot` active（pid 1655，旧二进制） |

## 卡面 4 条要求的落点

1. **重新编号** —— 已完成（`system/V0399`，up+down 成对）；号段依据：全局最大 `V0398`（HR-45 自检）→ `V0399`，下一个可用 `V0400`（门禁会打印）。
2. **指纹处理** —— 实测**无需处理**：`schema_migrations` 主键是 `(kind, name)`、不含 version，纯改名后「已应用旧号」的库按 `(kind,name)` 命中 ⇒ 不重跑、无 checksum 报错（C1→C2 为证）。反例（改号时顺手改内容）会 `modified after it was applied` 中止，见 architect 卡 t_83977c43 的 `07_content_changed_checksum_FATAL.out`。
3. **门禁** —— `scripts/lint/migration_version_unique.py` 挂在 `scripts/check.sh` 的 L0（`--fast`/pre-push 经 `.githooks/pre-push → check.sh --fast` 覆盖）；判据 A–E + `--selftest` 8 条语料；D1–D3 证明有牙。
4. **验收** —— 空库首启 ✓（A/B）、`cargo test` 见 `H1_wt_head_cargo_test.log`、生产/演练库启动演练 ✓（C2 用 145 真实 dump 副本；G1 生产只读核对，未重启现网）。

## 复跑方式

```bash
# 需要本机 PG（alun/alun）+ backend/target/debug/cloudpivot
bash mig_drill.sh R && bash mig_drill.sh A && bash mig_drill.sh B     # 空库首启 + 二次启动
bash mig_drill.sh N                                                   # 负向：复原撞号 → FATAL
bash mig_drill.sh C1 && bash mig_drill.sh C2                          # 已应用旧号的库 + 新号树
bash mig_drill.sh D1 && bash mig_drill.sh D2 && bash mig_drill.sh D3   # 门禁三态
```

> 注意：`N` 会 `DROP SCHEMA public CASCADE` 掉 `cp_mig_b4cc_neg`（本卡专用 drill 库），跑完 A 之前先跑 `R`。
