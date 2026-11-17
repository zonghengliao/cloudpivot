# V0442 指纹修复（一次性 datafix）

## 一句话

V0442 为修「空库首启必失败」（t_08e1816a）被**原地改写**，因此凡是在 2026-11-17 之前
跑过旧版 V0442 的库，必须把 `schema_migrations` 里那条记录的 `checksum` 更新成新指纹，
否则实例启动直接 `MigrationChecksum` 失败。

```bash
export ERP_DB_URL='postgres://cloudpivot:***@127.0.0.1:5432/cloudpivot_dev'
bash scripts/datafix/v0442-checksum/apply.sh            # 预演
bash scripts/datafix/v0442-checksum/apply.sh --apply    # 真改（幂等）
```

## 为什么会有这一步

| 事实 | 依据 |
|---|---|
| 内核执行次序是 **baseline(0) → versioned(1) → repeatable(2) → seed(3)** | `backend/vendor/nik-db/src/migrate/kind.rs::execution_rank` |
| V0442 的前置护栏断言了「系统管理」目录行存在，而该行**只**由种子 `S__menu_data.sql` 建出 | `grep -rln 10000000-0000-0000-0000-000000000001 backend/app/migrations/`（版本化侧只有 V0319 的 `UPDATE parent_id` 与 V0322 的 `parent_id` 引用，都不建行） |
| ⇒ 空库首启跑到 V0442 必然 `RAISE EXCEPTION`，`make_full_suite_db.sh` / 新环境部署全被挡 | 实测：失败那一刻 `schema_migrations` 里 `versioned=361 / seed=0` |
| 新迁移救不了：V0443 永远轮不到执行（442 先炸） | 内核按版本升序执行 |
| 框架对已应用迁移是**强校验**：`up` 内容一变即中止启动 | `vendor/nik-db/src/migrate/history.rs`（`Divergent::Abort` 是默认策略，仓库 config 未改） |

所以只能原地改 V0442 —— 代价就是这条路必须同步修指纹。

## 本脚本做什么 / 不做什么

- **做**：`UPDATE schema_migrations SET checksum = <新版指纹> WHERE kind='versioned' AND version='442'`
- **不做**：不重跑迁移、不碰业务表、不动 `applied_at` / `applied_seq` / `success` / `source`
  （因此不影响回滚顺序与 `status` 视图）

## 三层安全阀

1. **只认已知的旧指纹** `2e6fbee6a98c6ca081ada172f91623b922bad2ea459f431db50cce98b11cdafe`
   （未修改的 V0442 的框架口径指纹，2026-11-17 实测与 `cloudpivot_dev` 记录逐字节一致）。
   库内值是任何其它值 ⇒ 中止，交人工判断。
2. **默认 dry-run**：不带 `--apply` 只打印将执行的 SQL 与前后值。
3. **改后回读**：执行完立刻重查 `checksum`，与新指纹不一致即退 1。

## 新指纹怎么来的（不是拍脑袋）

`recompute_checksum.py` 复刻框架的 `content_checksum`：**逐行 + `\n` 后做 SHA-256**
（`sql.lines()` 语义 ⇒ 文件末尾换行数量与 CRLF/LF 不影响结果）。
不要用 `sha256sum`：那是对文件字节做哈希，写进库之后启动仍然报 `MigrationChecksum`。

自证：用本脚本对**未修改**的 V0442 算，得到的就是上面那个旧指纹
（`proof_checksum_calibration.txt` 里有逐字节输出）。

## 幂等

- 已在 442 之前停住的库（`cloudpivot_test` / `cloudpivot_test_full` …）→ 没有 442 记录 ⇒ 输出 no-op、退 0。
- 已经修过的库 → 现值 = 新指纹 ⇒ 输出 no-op、退 0。
- 全新库（424 之后正常迁移到 442）→ 记录里本来就是新指纹 ⇒ no-op。
