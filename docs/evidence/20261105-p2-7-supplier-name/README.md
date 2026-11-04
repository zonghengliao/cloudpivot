# 证据 · P2-7 供应商名口径统一（ID / 全名 / 简称）

> 卡：`t_54ad4c5f`【P2-7】供应商三名不一致（盘点 + 定规则 + 补巡检）
> 日期：2026-11-05 · 主机 99（Hermes）→ 生产 144 只读实测
> 结论文档：`docs/architecture/P2-7-供应商名口径盘点.md`（盘点表 / 口径 S1–S6 / 偏离项）
> 巡检落地：`docs/architecture/snapshot-drift/10_checks.sql` **DI-23** + `scripts/verify/snapshot_audit.py`

## 0. 一页结论

| 验收项 | 结果 | 证据文件 |
|---|---|---|
| 盘点表完整（位置 / ID 还是名字 / 来源） | 17 处（4 类快照列 + 6 处实时取名 + 7 处前端显示） | `docs/architecture/P2-7-供应商名口径盘点.md` §2 |
| 规则写进 docs | S1–S6 落 `编码规范.md` 1.10（强制）+ 盘点文档 §3 | `docs/编码规范.md` |
| 新增 1 条巡检、能真跑出结果 | DI-23，6 类分支各自命中（1/1/1/1/2/3） | `fixture-branches.txt`、`audit-seeded-主档.txt` |
| grep 确认没有「用名字 join」 | `biz_supplier` 全部 JOIN 均 `ON s.id = …`（6 处），名字 `GROUP BY` 0 处 | `grep-on-id-join.txt` |
| 不变量全绿 | `erp_invariants.py` 52/52 通过、BLOCKER 0 / WARN 0（干净库） | `invariants-clean.txt` |
| 巡检 23 条全绿（含 DI-23） | 23/23 通过、退出码 0（干净库） | `audit-clean.txt` / `audit-clean.json` |
| 防回归 | `cargo test --test invariant_registry` 1 passed；**故意改号后确实红** | `cargo-test-invariant-registry.log` |

## 1. 文件说明

| 文件 | 内容 |
|---|---|
| `prod-readonly-measurements.txt` | 生产 `cloudpivot@144` 只读实测量（结构 A / 漂移计数 B / DI-23 六分支等价计数 C） |
| `p27_probe_a.sql` / `p27_probe_b.sql` / `p27_probe_di23_prod.sql` | 上表用到的三条只读 SQL（可直接在 144 重跑） |
| `p27_fixture.sql` | DI-23 夹具：**一个事务内**造齐 6 类命中 + 断言 + `ROLLBACK`（库侧零残留） |
| `fixture-branches.txt` | 夹具运行输出：6 类分支 `expected == actual` 全 `t`，回滚后残留 0 |
| `p27_seed.sql` | 同一批变异**提交**版（只灌进一次性证据库 `cloudpivot_54ad4c5f_seeded`，供 `snapshot_audit.py` 端到端跑） |
| `audit-seeded-主档.txt` / `audit-seeded-主档.json` | `snapshot_audit.py --group 主档` 在夹具库上的真实输出（DI-23 9 条：WARN 4 / INFO 5） |
| `audit-clean.txt` / `audit-clean.json` / `audit-clean.log` | 干净库 `cloudpivot_54ad4c5f_clean` 全量 23 条巡检：全绿、退出码 0 |
| `invariants-clean.txt` | 同库 `erp_invariants.py` 全量：52/52 通过 |
| `cargo-test-invariant-registry.log` | `cargo test --test invariant_registry`（视图 ↔ 登记 一一对应） |
| `grep-on-id-join.txt` | 「没有用名字 join」的取证命令与输出 |
| `committed-tree-verify.txt` | **提交树自洽复验**：从 commit `8f356ef` 里 `git show` 出三个文件（00/10/snapshot_audit.py）→ 全新空库 → 装 23 个视图 → 全量 23/23 通过。证明提交出去的内容**不依赖**工作区里别卡未提交的 WIP |

## 2. 复跑（本机 99）

```bash
# ① 造库（模板 = 干净引导库；没有就用任意当前迁移的库）
su postgres -c "createdb -T cloudpivot_p27_inv cloudpivot_54ad4c5f_clean"
su postgres -c "psql -q -d cloudpivot_54ad4c5f_clean -c 'ALTER DATABASE cloudpivot_54ad4c5f_clean OWNER TO alun'"

# ② 装视图 + 全量跑（应 23/23 通过、退出码 0）
cd /opt/cloudpivot
export NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
       NIK_DATABASE_NAME=cloudpivot_54ad4c5f_clean \
       SNAPSHOT_AUDIT_LOCK=/tmp/p27.lock SNAPSHOT_AUDIT_LOG=/tmp/p27-audit.log
python3 scripts/verify/snapshot_audit.py --install-views

# ③ 夹具：6 类分支各自命中 + 回滚零残留
psql -h 127.0.0.1 -U alun -d cloudpivot_54ad4c5f_clean -X -f p27_fixture.sql

# ④ 不变量
python3 scripts/verify/erp_invariants.py

# ⑤ 防回归（纯文件解析，不连库）
cd backend && cargo test --test invariant_registry
```

## 3. 生产只读（144）

```bash
scp p27_probe_*.sql root@172.18.10.144:/tmp/
ssh root@172.18.10.144 'set -a; . /etc/cloudpivot/snapshot-audit.env; set +a;
  PGPASSWORD=$NIK_DATABASE_PASSWORD psql -X -q -h 127.0.0.1 -U cloudpivot -d cloudpivot -f /tmp/p27_probe_di23.sql'
```

生产实测口径注意：**该库只有 1 家供应商、18 张采购单**，所以"名字唯一性"类分支天然 0；
`SNAPSHOT_MISSING = 11` 是真实命中（V0198 不回填存量快照）。
巡检"有牙"的证据只能来自夹具库（`fixture-branches.txt`）。
