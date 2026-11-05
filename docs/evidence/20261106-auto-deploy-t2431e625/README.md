# 2026-11-06 生产部署证据（卡 t_2431e625 · AUTO-DEPLOY）

**结论：生产已生效并能自证。** 最终发布提交 `81b89441`（= origin/main），库内 190 条版本化迁移全部记账，
二进制与 dist 都比各自的最近源码改动新，服务 active、`/health` 200、进程 == 磁盘二进制，
不变量 68/68、快照巡检 BLOCKER 0、真机抽查 11/11。

---

## 0. 这张卡的数字是**旧快照**，先说清

卡面写的是「生产落后 6 个提交 / 二进制过期 / dist 过期」，采样时间 00:00（`BIN_TS 1793888930`、
`BE_TS 1793893090`）。在此之前的一次运行（run 747，00:02–00:34）已经把生产送到 `b75ccc8` 并收口通过；
本卡被取消后由调度重新拉起，于是卡面数字与真机状态已经不一致。

本轮**没有相信卡面数字**，全部重新在 144 上逐文件核对；核对期间主仓还在往前推（团队其他 worker 在提交），
因此本轮实际做了 3 次发布（都是 `scripts/deploy/deploy.sh`，见下表）。

| 轮次 | 目标提交 | 做了什么 | 结果 |
|---|---|---|---|
| ① 核对 | `b75ccc8` | dry-run 体检（落后 0 / 待跑迁移 0 / 无前后端改动） | 已是当时最新，无需动作 |
| ② 发布 | `80e05a11` | ff + 跑 V0257 + **重编后端并重启** | V0257 记账 188→189，进程 == 磁盘 |
| ③ 发布 | `81b89441` | ff + 跑 V0258（`business_role_matrix` 冻结基线） | 记账 189→190，无后端/前端改动故不动服务 |

## 1. 备份（三件套，每一轮 deploy.sh 各自做一次）

```
/root/deploy-backup-autodep-t2431e625/{db.dump, cloudpivot, dist}     # 第一批
/root/deploy-backup-autodep-t2431e625b/{db.dump, cloudpivot, dist}    # 第二批（发布 80e05a11 前）
/root/deploy-backup-autodep-t2431e625c/{db.dump, cloudpivot, dist}    # 第三批（发布 81b89441 前）
```

回滚三件套（二进制 / dist / 库 dump）与命令见 `/opt/cloudpivot/scripts/deploy/README.md`「回滚」一节，
各轮 deploy.sh 日志尾部也打印了精确到 commit 的回滚命令。

## 2. 迁移（逐条跑 + 逐条 psql 对账）

| 迁移 | 内容 | 记账结果 |
|---|---|---|
| `V0257__erp_period_remind_ledger` | 结账催办写业务提醒台账（doc_type 8 值 / remind_kind 6 值自检通过） | `APPLY_OK V257 checksum=bfa82b1a… seq=205`，188 → 189 |
| `V0258__business_role_matrix` | 业务角色权限矩阵进装配路径，冻结生产基线（矩阵 450 对 / 本次新增授权 0 行 / token_version 推进 0 个账号） | `APPLY_OK V258 checksum=f942dc0d… seq=206`，189 → 190 |

**逐条核对口径**（不按版本号比大小，用「磁盘文件集 − 库内 success 记录」做集合差）：

```
磁盘 backend/migrations/**/*.up.sql          = 190
库内 schema_migrations(kind='versioned')      = 190
差集                                           = 空   ✅
```

deploy.sh 每轮都跑了三道自检：`RECON_OK`（增量 == 本次条数）、`VIA_OK`（applied_via ∈ executed/imported/baseline）、
`SET_OK`（磁盘全记账），并且重启后日志确认「应用 0 个迁移」——这条是「记账与引擎口径一致」的证明。

## 3. 二进制 / dist / 服务（时间戳逐文件核对）

见 `accept.txt`，摘录：

```
主仓 HEAD / origin/main          81b89441 / 81b89441      ✅ HEAD == origin/main
迁移：磁盘 190 ，库内 versioned success 190                ✅ 差集为空
BIN_TS=1793898821  BE_TS=1793898068                        ✅ 二进制比最近后端改动新 753s
DIST_TS=1793896074 FE_TS=1793895165                        ✅ dist 比最近前端改动新 909s
服务状态=active  /health=200  进程==磁盘二进制=yes
线上前端入口 chunk=assets/index-Cv1a5LCm.js
```

线上 dist 里确实包含本轮上线的两个新页（`grep` 命中 `assets/index-Cv1a5LCm.js`）：`报表项维护`、`应付账龄分析`。

## 4. 四项回归

| 项 | 命令 | 结果 |
|---|---|---|
| cargo test | `cargo test --features test-hooks`（生产机，**未设** `ERP_DB_URL`） | **2910 passed / 0 failed**（23 个测试二进制）；日志 `cargo-test-nodb.log` |
| cargo test（真库档） | 同上 + `ERP_DB_URL` 指向**隔离库**（198 条迁移由发布二进制首启应用） | 2676 passed / **3 failed**，见 §4.1；日志 `cargo-test-with-db.log` |
| pnpm（前端门禁） | `pnpm typecheck`（`vue-tsc --noEmit`） | 通过，0 error。**注意：`frontend/package.json` 里没有 `test` 脚本**，本仓库的前端门禁就是 typecheck |
| erp_invariants | `python3 scripts/verify/erp_invariants.py`（生产库，只读） | **68/68 通过，BLOCKER 0 / WARN 0**；日志 `invariants-68.txt` |
| snapshot_audit | `python3 scripts/verify/snapshot_audit.py`（生产库，只读，`BEGIN READ ONLY`） | **BLOCKER 0 / WARN 0 / INFO 90**（26 项：通过 22、有命中 4、失败 0）；日志 `snapshot-audit.txt` |

> V0258 动了权限矩阵，所以不变量与真机抽查在它落库**之后**又各跑了一遍（都还是全绿）。

### 4.1 真库档那 3 条失败：**不是代码缺陷，是测试脏数据**

失败全是 `business::flow::edge_admin_tests::batch_task_{invalid_uuid_only_fails_that_item,
repeated_approval_keeps_single_row, writes_edge_after_approval}`。定位过程：

1. 库里的遗留实例 `01a1acab-8287-767a-a68e-1de359708143` 显示 `n_cfg` 节点有 **3 条待办**：
   `超级管理员(APPROVED) / P17 p17_ap1(PENDING) / P17 p17_ap2(PENDING)`；
2. 共用的已发布流程图 `flow_edge_config`（`biz_type='FLOW_EDGE_CFG'`）节点已被前面的 p17 用例改成
   `["e","n_cfg","s"]` 且 `n_cfg` 挂 3 个 assignee（`sys_user` 里 `p17_*` 账号仍在用）；
3. 于是「同意 1 条待办」只批掉 1/3，实例停在 `PENDING` → 用例断言必红。

**反证**：换第二个全新隔离库，只跑该模块 → `9 passed / 0 failed`（`cargo-test-3cases-fresh-db.log`）。

已为此开卡 **t_977c5189**（assignee `test-enginner`）：p17 夹具改写共用已发布图后不恢复、
默认门禁（不设 `ERP_DB_URL`）看不见这类缺陷。

## 5. 真机抽查（生产，走 nginx:80 → 后端 :8023，只读）

11/11 PASS（`spotcheck.txt`）：

```
/health 200 · 登录 200（拿到 461 字符 access_token）· SPA 入口 200 + 入口 chunk 200
user/page 200(total=9) · menu/tree 200(顶层 16)
report-item/catalog 200 · report-item/list 200(41 行) · report/payable-aging 200
   （buckets = NOT_DUE / D0_30 / D31_60 / D61_90 / OVER_90）
notice/stream?token=… 200（P1-FIX-6 的 query token 不再 401）
菜单树里已挂两个新页：
  应付账龄分析 → /business/erp/report/payable-aging
  报表项维护   → /business/erp/report/report-item
```

截图：`shots/01-prod-login.png` —— 生产入口页真实渲染（品牌名、TDesign 登录卡、版本行 `Rust + nik · Vue 3 + TDesign`）。

> **诚实边界**：登录后的页面截图没有做。浏览器工具不允许代输口令（策略），而生产 admin 口令只存在于
> 144 的 `/etc/cloudpivot/cloudpivot.env`（`NIK_ADMIN_PASSWORD`），不在仓里；所以「登录态」的证据是
> curl 拿 token 后的 8 条鉴权接口断言（含新页在菜单树上），不是截图。

## 6. 本轮没做的事（以及为什么）

- **没改任何代码**：本轮只做发布与验证。
- **没把证据写进 `docs/evidence/` 并提交**：144 工作区的 99 副本（/opt/cloudpivot）当前被其他 worker 改着
  （23 个未提交文件，含 `backend/src/business/erp/period_remind.rs` 等正在被提交的文件），此时 commit 会与
  origin/main 分叉、pull 又会被脏文件挡住。证据随卡附件保存（README + 原始日志），等脏树落定后再归档进仓。
- **没跑 `cargo fmt/clippy`**：`check.sh` 的这两道门需要 `--all-targets` 全量重建，7GB 机器上本轮已两次
  触发 OOM（详见 §7），风险收益不划算；本轮发布走的是 release build + 真机/真库验证。
- **没 push 别人的提交**：origin/main 由各 worker 自己推，本轮只 ff 生产工作仓。

## 7. 环境坑（本轮实测，供后来者省时间）

1. **非交互 ssh 没有 `cargo`**：`PATH` 里没有 `/root/.cargo/bin`，后台任务要显式 `export PATH`，
   否则 `setsid nohup cargo …` 会静默产出 0 字节日志。
2. **7GB 机器跑 `cargo test` 会 OOM**（`Out of memory: Killed process rustc … anon-rss 3.4GB`）。
   可用组合：`CARGO_BUILD_JOBS=1 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0`
   （`debug=0` 会让依赖重编一遍，但峰值内存掉到 1.5GB 级，不再杀进程）。
3. **真库用例要显式给 `ERP_DB_URL`**，否则 `SKIP` 假绿；隔离库用发布二进制首启即可自动套用全部迁移
   （`NIK_DATABASE_NAME=cloudpivot_xxx NIK_SERVER_LISTEN=127.0.0.1:18099 ./target/release/cloudpivot`）。
4. **主仓一直在动**：本轮 60 分钟里 origin/main 前进了 3 次（`b75ccc8` → `80e05a11` → `81b89441`）。
   deploy.sh 每轮都重新 fetch 并对账，所以「落后 N 个提交」这类卡面数字随时会过期，必须复核。
