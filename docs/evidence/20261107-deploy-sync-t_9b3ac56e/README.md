# DEPLOY-SYNC t_9b3ac56e —— 真分叉合仓 + 上线证据

卡：`【DEPLOY-SYNC】三方不一致：99=a677354 bare=bdd3a29b prod=bdd3a29b`（deploy_check_v4 2026-11-07 00:00:02 自动建卡）

## 1. 起点真值（卡面数字已过期，实测）

| 端 | HEAD | 说明 |
|---|---|---|
| ① 99 开发机 `/opt/cloudpivot` | `311f13a` | 相对 bare **领先 9 / 落后 5** → **真分叉**（不是卡面写的「只有未推送」）|
| ② bare `/opt/git/cloudpivot.git` | `bdd3a29b` | 上一轮 DEPLOY-SYNC t_c3087afe 的合并结果 |
| ③ 144 生产 | `bdd3a29b` | 与 bare 一致，无部署积压 |

卡面写「99=a677354 未推送 7」是 00:00 的快照；00:10 又落了 `311f13a`（SO-23 followup，V0301）。

**根因**：上一轮 t_c3087afe 的合并在独立 worktree 里完成（分支 `deploy-sync/t_c3087afe`），
只推了 bare，**没有把 99 的 `main` 前进到合并结果** ⇒ 99 main 停在分叉前的 `07dfb67` 血统，
auto_push 每小时判「分叉，不自动推」（`/var/log/cloudpivot/auto-push.log` 23:40 起连续 4 条）。
本轮沿用同一手法，并把「99 main 未前进」写进遗留事项（见第 6 节）。

## 2. 合仓

独立 worktree `/opt/cloudpivot-merge-t_9b3ac56e`，分支 `deploy-sync/t_9b3ac56e`，
`git merge --no-ff origin/main`（base = `07dfb67`）。

合并结果 **`4a25bb7`**（merge commit），入仓内容：

- 99 侧：SO-22 收入确认凭证 / SO-23 不变量巡检 + followup（V0301）/ SO-24 端到端十步 / SO-25 销售菜单树（V0267）
- bare 侧：SO-14 遗留退款审批（V0294/95/96/97 + `receipt_refund*.rs`）

### 冲突处理（1 处文本冲突 + 2 处语义冲突）

| 文件 | 类型 | 处理 |
|---|---|---|
| `backend/src/business/flow/linkage/spec.rs` | 文本冲突 `TRIGGER_EXEMPT` | 并集：`RECEIPT` 取 99 侧注释（多带 SO-20 发票说明）+ 追加 bare 侧 `RECEIPT_REFUND` 豁免项 |
| `backend/src/business/erp/receivable.rs` | 语义冲突（测试）| bare 侧 V0294 已把 `RECEIVABLE_EDGES` 加到 9 条（退款反向边 `SETTLED→PARTIAL→OPEN`）但测试仍断言 6 条且把 `SETTLED` 当终态 —— **该测试在 bare 上本来就是红的**。断言改 9，补 3 条反向边，`SETTLED` 移出终态循环并显式钉住「只允许回退到 OPEN/PARTIAL」 |
| `backend/src/business/shared/state_machine.rs` | 语义冲突（常量断言）| 两侧各新增一个实体（`RECEIPT_REFUND` / `INVOICE`），`Entity::ALL = 20`，断言 19 → 20 并补演进注释 |

自动合并（无冲突但已核对语义）：`invoice.rs`、`receivable.rs`、`linkage/spec.rs` 的其它 hunk、
`state_machine.rs` 的七处注册 match 分支（`Invoice` / `ReceiptRefund` 四处分支齐全）。

### 迁移号段（全段连续无重复）

bare 在上一轮合仓时已顺延：发票 → `V0298/99/300`、退款 → `V0294/95/97`、收款菜单 → `V0296`。
本树新增 `V0301`（SO-23 followup）⇒ `V0294…V0301` 连续，无编号碰撞，无「同号不同内容」。

## 3. 本地验证（合并树）

```
cargo check --all-targets   → Finished dev profile in 2m52s，0 error / 0 warning
cargo test --lib            → test result: ok. 2868 passed; 0 failed; 0 ignored
```

> `shared::startup_guard::tests::merge_semantics_or_base_and_profile` 首轮红，原因是 worktree 缺
> `backend/config/config-dev.toml|config-prod.toml|config-test.toml`（gitignore，本地不入库；
> 99 主仓 `backend/config/` 有）。从主仓 cp 过去后转绿 ⇒ **环境性**，非合并引入。
> `cargo fmt --check` 对 `backend/src/business/erp/voucher.rs`（SO-22 代码，`311f13a` 提交时未 fmt）
> 有 2 处 diff，属既有格式债，本卡未动（合并卡不夹带格式化，避免掩盖真改动）。

## 4. 部署（144 `scripts/deploy/deploy.sh --tag t_9b3ac56e`）

计划（`--dry-run`）：后端改动 有 / 前端改动 无 / 待应用迁移 2 条（`V0267` 乱序补迁 + `V0301`）。

| 判据 | 结果 |
|---|---|
| ff 同步 | `HEAD → 4a25bb7b`（`git merge --ff-only`）|
| 编译 | `Finished release`，二进制 md5 `ca00824166eb002144fbf081b4e8ea37`，mtime `2026-11-07 00:31:36` |
| 迁移对账 | `RECON_OK applied 增量 2 == 本次条数 2（218 → 220）`；`V0267 seq=235`、`V0301 seq=236` |
| V0301 自检 | `NOTICE: so_approve 打标 1 行，快照 canonical 重排 1 行（da3c2eed → 97fe9377）` |
| applied_via | 全部 ∈ `executed/imported/baseline` |
| 集合复核 | 磁盘版本化迁移全部已记账（220 条）|
| 服务 | `active`，`/health` → `{"code":0,...,"status":"ok","profile":"prod"}` |
| 进程 == 磁盘 | yes（md5 同上，PID 1727248）|
| 启动日志 | 「应用 0 个迁移」（记账与引擎一致）|
| 前端 | 无前端改动，跳过（入口仍 `assets/index-aUBeUlnV.js`，与 `dist/index.html` 一致）|
| 收口 | 主仓 HEAD == origin/main == `4a25bb7b`；迁移 220 条 / 最大 V0301 |

## 5. 生产独立核验（不信 deploy.sh 自报）

1. **库内账**：`schema_migrations` 230 行 / `maxver=301`；`V0294–V0297` `applied_via=executed`，
   `V0298–V0301` `imported`（见 `raw/` 内 psql 输出）。
2. **V0301 声称对象**：`biz_flow_def.so_approve.is_system = true`、
   当前版本 `version_no=1 / hash16=97fe9377`（与迁移 NOTICE 逐字一致）；快照 `schema_version=1 / nodes=7 / edges=8`。
   `receipt_refund` 也 `is_system = true`。
3. **新表**：`biz_invoice` / `biz_receipt_refund` / `biz_receipt_refund_allocation` 三表存在且 `count(*)=0`（干净建表）。
4. **号段**：`biz_seq` 有 `RR`（收款退款单号）与 `IV`（销售发票号）各 1 行，`current_no=0`。
5. **菜单**：`sys_menu` 销售管理容器 `…2701` 下 6 个页面（客户档案/销售订单/发货单/应收账款/收款单/销售发票），
   `sort_order` 10–60，`is_visible=t`，`auth_code` 齐全（V0267 + V0300 落地）。
6. **二进制内路由**：`strings` 命中 `business/erp/invoice`(2) / `receipt-refund`(2) / `sales-order`(1) / `contract`(8)。
7. **未认证探针**：`invoice` / `receipt-refund` / `sales-order` / `receivable` / `receipt` 的 `/page` 全 `401`；
   对照组 `/api/business/erp/zzz-nope/page` → `404`（证明 401 是路由真注册，不是 SPA fallback 兜底）。
8. **deploy_check_v4**（带 cron 的 SSHPASS）：`① 99: 311f13a(未推送 0) ② bare: 4a25bb7b ③ 144: 4a25bb7b` → ✅ 三方一致。

## 6. 遗留 / 交回

- **99 的 `main` 仍停在 `311f13a`（落后合并结果，但无任何独有提交）**：当时 `SO-26` 卡正在
  同一工作区写 `contract_*.rs` / `receipt.rs`（4 个未提交文件 + 2 个未跟踪脚本），
  硬 ff 会把它的工作区打成「大量已删除」。**待 SO-26 提交完成后**在 99 执行：
  ```bash
  cd /opt/cloudpivot && git fetch origin && git merge --ff-only origin/main
  ```
  在它完成前 `deploy_check_v4` 已判「未推送 0 个」，不会重复建卡。
- `bare` 侧原先红的 `receivable::tests::legal_edges_match_arch7_state_graph` 已在本合并修复
  （属 SO-14 遗留卡漏改测试，不是本卡引入的红）。
- `voucher.rs` 的 `cargo fmt` 债（SO-22）与 `receipt.rs` 既有 clippy 债留给后续代码卡。

## 原始记录（`raw/`）

| 文件 | 内容 |
|---|---|
| `deploy-log.txt` | 144 上 8 步部署完整日志（510 行，`❌` 计数 0）|
| `deploy-check-v4.txt` | 收口三方比对（带 SSHPASS 手工跑）|
| `cargo-check.txt` | 合并树 `cargo check --all-targets` |
| `cargo-check-summary.txt` | 同上，仅 `Finished` 行 |
| `cargo-test-lib-summary.txt` | 合并树 `cargo test --lib` 结果行（2868 passed / 0 failed）|
