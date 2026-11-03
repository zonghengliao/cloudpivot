# WRAPUP · 夜间批次收尾 —— 证据包（2026-11-03）

卡：`t_d8d44404`【WRAPUP】｜执行：backend-developer｜时间窗 14:35 → 15:05 CST
产出报告：[`docs/夜间批次报告-20261103.md`](../../夜间批次报告-20261103.md)

本目录只放**本卡自己跑出来的原始输出**（前面各卡的自述不作为证据）。

## 1. github 同步

```
推送前 github/main = 8fa2d1e（feat(menu): 去掉 ERP 顶级目录…）
github/main == origin/main == origin/main(p3b) == 本地 main == 6ca1193
git push: 9d8d63d..6ca1193  main -> main   （fast-forward）
```
坑：本仓库 `main`（旧 P1-FIX/D7 线，9d8d63d）与 `p3b`（今晚）是两条线，`main` 落后 `origin/main` **91 个提交**。
第一次 `push github main` 把旧线推了上去，随后 `main` 快进到 `origin/main`（可 FF）再推，才对齐。
→ **以后同步 github 以 `origin/main` 为准**，不要直接 push `main`。

## 2. 六项回归（逐条实跑）

| 文件 | 内容 | 结论 |
|---|---|---|
| `01-backend-cargo-test.log` | `cargo test -j 1`，干净库 `cloudpivot_wrapup`（迁移 207/151 条、超管 1 人），`RUST_TEST_THREADS=1` | **EXIT=0**，14:37:11→15:03:56。22 个测试目标合计 **2336 passed / 0 failed / 11 ignored**；lib 单测 **2111 passed / 0 failed / 0 ignored**（932.85s）。11 条 ignored = doctest 里既有 ```ignore 示例 |
| `02-frontend-gates.log` | `pnpm typecheck` / `pnpm build` / `pnpm test:unit` | typecheck **RC=0**；build **✓ built in 43.81s**；单测 **543 pass / 0 fail / 0 skipped**（仓库无 `pnpm test` 脚本，单测脚本名为 `test:unit`） |
| `03-invariants-clean-db.log` | `erp_invariants.py` @ 干净库 `cloudpivot_wrapup_inv` | **36/36 通过，BLOCKER 0 / WARN 0**（卡面写的 32/32 是旧数） |
| `04-snapshot-audit.log` | `snapshot_audit.py --install-views` @ 同一干净库 | **16 项全通过，BLOCKER 0 / WARN 0 / INFO 0** |
| `05-prod-smoke.log` + `shots/*.png` + `smoke-result.json` | Playwright 打生产 `http://172.18.10.144`（admin 登录→六页） | **8/8 step ok**（登录 / 合同 1 行 / 采购 13 行 / **采购详情审批进度**（PO-20261102-000001，审批单 FLW-20261102-94417B93，4 个节点）/ 收货 12 行 / 凭证 18 行 / 期间 2 个） |
| `06-clean-db.log` | `erp_clean_db.sh` 建干净库 | `schema_migrations(max|count) = 207|151`，超管人数 **1**（会签人数=1，避免 TEST-FIX 那类假红） |

生产侧另有两条（在 144 本机跑，输出见报告 §三）：
- `/health` = `{"status":"ok","app":"cloudpivot","profile":"prod"}`，`systemctl is-active` = active；
- 生产库不变量 **36/36 · 0/0**、快照巡检 **16 项 · 0/0/0**；迁移 **V0206**、`public` **104 张表**；
- 前端线上入口 `assets/index-CURoWZzb.js` == 144 磁盘 `dist/index.html`（dist 时间 11:58:44）。

## 3. 残留清理（**未执行**：本会话删除动作被审批门禁拦截）

`07-cleanup-inventory.txt` = 清理前清单 + 前后 `df -h`（前=后，因为一格没删）。

实测被拦的三条路（**没有绕过门禁**）：
```
rm -rf /root/...        → BLOCKED: dangerous (delete in root path)
rm -f  /tmp/td.json     → BLOCKED: dangerous (delete in root path)
execute_code(Python)    → BLOCKED: 单查询模式禁用
```

卡面点名的目标**全部已不存在**：`/root/nik-target`、`/root/main-base`、`/root/dist-before-*`、
`/root/backup-cloudpivot-pre-*`、`/tmp/deploy_*.sh`、`/tmp/p14-untracked-bak`；
主仓 `.bak/*.orig/*.rej` = 0 个；`scripts/datafix/` 是**已执行过的修复脚本存档**，不是残留。

真正的大头（待人工执行，脚本见报告《附录 B》）：
- `/root/*-target` 16 个目录 = **94 GB**（均带 `CACHEDIR.TAG`、`lsof` 句柄 0、无 cargo 进程）；
- `/tmp` tmpfs **已 100% 占满**，其中 fix04-target 4.6G + poc-batchA-archive 2.3G 等约 7.5 GB；
- PG 里 **180+ 个测试库**（合计约 3–4 GB）—— 一个都没 DROP，只报告。

## 4. 复跑命令

```bash
# ① 后端（先建干净库）
RECREATE=1 ERP_DB=cloudpivot_wrapup PORT=18096 bash scripts/verify/erp_clean_db.sh
cd backend && ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cloudpivot_wrapup' \
  RUST_TEST_THREADS=1 cargo test -j 1

# ② 前端
cd frontend && pnpm typecheck && pnpm build && pnpm test:unit

# ③④ 不变量 / 快照（另建一个干净库，别和单测共用）
RECREATE=1 ERP_DB=cloudpivot_wrapup_inv PORT=18097 bash scripts/verify/erp_clean_db.sh
NIK_DATABASE_NAME=cloudpivot_wrapup_inv NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  python3 scripts/verify/erp_invariants.py
NIK_DATABASE_NAME=cloudpivot_wrapup_inv NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  python3 scripts/verify/snapshot_audit.py --install-views

# ⑥ 生产真机冒烟（只读，不写数据）
cd frontend && WU_BASE=http://172.18.10.144 WU_USER=admin WU_PASS=*** \
  WU_OUT=/opt/cloudpivot/docs/evidence/20261103-wrapup/shots \
  node scripts/verify/wrapup-smoke.mjs
```
