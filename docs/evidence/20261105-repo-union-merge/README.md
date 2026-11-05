# 证据 · 仓库口径收口：99 本地 main ↔ 144 bare 历史分叉（union merge）

> 卡：`t_72c90ae4`【仓库口径】99 本地 main 与 144 bare 历史分叉，push 被 non-fast-forward 拒绝
> 上游发现：`t_5f9f0ca2`（P2-5B 库存写入口门禁，只报告未动远端）
> 执行人：manong（开发总管）· 日期：2026-11-05 · 工作仓 `/opt/cloudpivot`（99）

## 0. 一页结论

| 问题 | 裁定 |
|---|---|
| 哪条血缘权威 | **99 本地 main** 为权威主干（144 bare main 是「分支 tip rebase 重放」的副本，见 §1） |
| 怎么统一 | **union merge（双方都不丢）**：本地 main 合入 `origin/main`，产生一个 merge commit，再以**快进**推回 144 bare —— 没有 force push、没有 rebase、没有任何提交被抹掉 |
| 144 bare 上那 19 条独有提交 | **保留在主干**（P2-6 收口线 / ARCH-6 / FLOW-PANEL-UX / flow 修复 / P2-6b 等），它们是真实工作量，不能丢 |
| 撞号 | 双方各有 `DI-23`：144 侧是「单据附件悬空」（P2-6b，已在生产口径与证据里）⇒ 本地侧「供应商名映射」**让到 DI-26** |
| `post-commit` 钩子 | **不改造成 ssh 自动推**（理由见 §5），改的是「发现机制」：新增 `scripts/git/main_drift_check.sh` |
| 生产影响 | 无迁移、无数据变更；只影响 git 历史与巡检项编号（DI-26 是新增项，DI-23/24/25 语义未动） |

## 1. 分叉是怎么来的（不是「本地落后」）

```
merge-base           7928b63  2026-11-04 16:55:07  fix(data): 清掉生产库 37 行 E2E 残留 + 三层环境守卫
99 本地 main          4f8230a  2026-11-05 07:59     (702 commits)
144 bare main        9ed4bf8  2026-11-04 22:50     (653 commits)
git status -sb →     main...origin/main [ahead 79, behind 30]
```

**关键证据：同标题提交两侧 hash 不同，且 144 侧的 committer 时间晚 3–13 秒，author 时间完全一致。**

| 标题 | 99 本地（author/committer） | 144 bare（author/committer） |
|---|---|---|
| 盘点单页面 + 路由 | `73f2097` 22:45:48 / 22:45:48 | `6c5354d` 22:45:48 / **22:45:51** |
| P0-FIX-4B 凭证审核链前端 | `d37302d` 22:11:29 / 22:11:29 | `7f243d4` 22:11:29 / **22:26:14** |
| AUDIT-ALL 评分 | `b42778b` 21:25:04 / 21:25:04 | `8a633a6` 21:25:04 / **21:25:25** |
| 登录限流放行 | `ed5b746` 18:25:19 / 18:25:19 | `180f709` 18:25:19 / **18:25:25** |
| VUE-FLOW-3 默认画布 | `fd48648` 17:18:06 / 17:18:06 | `1eb8523` 17:18:06 / **17:18:17** |

`git log --cherry-mark origin/main...main` 给出 **10 对 patch-id 完全相同、hash 不同**的提交。
结合 99 仓里还留着 `4b-push` / `4b-push2` / `4b-push3` / `t71b01775-push` / `t91-push` / `arch6-finance-design` 这些
「*-push」分支（它们的 tip 恰好等于 144 main 上的重写 hash）⇒ 结论：

> **144 bare main = 一批分支 tip 依次 rebase 到 bare main 上再推回去的线性副本**；
> 99 本地 main 才是「原始提交 + 其他人后续提交」的那条线。两边都在推进，所以**各有对方没有的提交**。

## 2. 两侧各自独有的真实工作（所以只能 union，不能二选一）

### 99 本地独有（79 条，节选）

P2-5A 库存三本账收口 `8f0a161`、P2-5B 库存写入口闸门加固 `4f8230a`、P2-4 合同权限码统一 `565d296`、
V0240 预算映射可落库 `280cf1a`、审批角色补码 `7165350`、P2-9 单据元数据、P2-8 采购单挂项目、
P2-7-F1 供应商名、P2-3 单测 +61、FIN-2 固定资产前端、预算三页路由、巡检结果落库/看板（P2-7）等。

### 144 bare 独有（20 条，其中 **19 条内容在本地完全不存在**）

用 `git apply --check --cached -R` 逐条验证「本地是否已含该改动」：

| 结论 | 提交 |
|---|---|
| ALREADY-IN-LOCAL（1 条） | `7b827cf` ARCH-6 口径校正（本地等价内容已在） |
| **MISSING（19 条）** | `9ed4bf8` c2 用例对齐 approvalProgress、`dae4353` ARCH-6 财务扩展设计（3312 行）、`2d8fd9b`+`8eeddaf` FLOW-PANEL-UX 属性面板、`cf44429`/`0709e1b`/`ff40b9a`/`ae5dd7a`/`aaaae4e`/`1882860` P2-6 收口证据包、`e721a78` 144 快照巡检 BLOCKER 飞书通知钩子、`8d54734`/`0ca5beb` DATAFIX-P14 台账 + 数据修复 SOP、`7f0fe5c` DI-19 按状态分级 + DI-18 精确豁免、`1be4ed7` 清 36 条软删测试定义、`83741fb`/`11a4bdf` 审批详情关联业务按 biz_type 路由、`6543949` P2-6b 死代码收口、`261e487` P2-6b 序列号映射 + 附件/提醒/抄送三类多态引用巡检 |

`git diff --diff-filter=A --name-only main origin/main | wc -l` = **111 个文件**只存在于 144 侧。

> 顺带纠正卡面的一条推测：**「同标题 hash 不同」不等于「远端被恶意重写」**，也不等于远端那些提交是废的——
> 重写的是**本地已有提交的副本**，同时远端还长了 19 条本地没有的提交。

## 3. 合并结果

- 合并基点（父）：`main 4f8230a` + `origin/main 9ed4bf8`
- 冲突 7 个文件，逐条裁定：

| 文件 | 冲突类型 | 裁定 | 原因 |
|---|---|---|---|
| `docs/architecture/ARCH-6-财务扩展设计.md` | add/add | 取本地 | 本地版含 V0240 修正（`ref_code` 是目标、`ref_id` 恒空），远端是旧口径 |
| `docs/评审报告-现有功能评分-20261104.md` | add/add | 取本地 | 本地版已把 P2-6 决策 A（「标准流程」术语收口 / 菜单并为一个）写进去，远端是旧措辞 |
| `scripts/verify/audit_features_20261104/chains_e2e.py` | add/add | 取本地 | 本地用统一后的权限码 `business:erp:contract:*`；远端还是旧码 `erp:contract:*` |
| `frontend/src/router/index.ts` | content | 取本地 | 本地是超集（盘点单/资产/预算共 6 条路由），远端的盘点单路由文本一字不差地已含在内 |
| `frontend/tests/unit/c2-approval-unified.test.ts` | content | 取远端 | 远端同题更严：4 条行为断言 + 逐入口断言（`fnSource`），本地只做全文件 count |
| `docs/architecture/snapshot-drift/10_checks.sql` | content | 取远端 + 追加本地 DI-26 | 远端含 P2-6b 的 DI-21 目标表补全 + DI-23/24/25 + `di.v_poly_ref_waiver`；本地只多一段供应商巡检 |
| `docs/architecture/快照一致性巡检与告警设计.md` | content | 取远端 + 追加 §2.4d | 同上 |

### 3.1 DI 编号撞号（必须记一笔）

- 144 侧：`DI-23` 单据附件 / `DI-24` 提醒台账 / `DI-25` 流程抄送（P2-6b，已在生产口径与 `docs/evidence/20261104-p2-6*` 证据里）。
- 99 侧：`DI-23` 供应商 ID→名字映射唯一性（P2-7）。
- **裁定：本地侧让号 → `DI-26`**（`di.c_di26` / `issue_code='DI-26'` / `CHECKS` 同号）。
  同步改到的文件：`10_checks.sql`、`快照一致性巡检与告警设计.md`（§2.4d + 总表 + 条数）、
  `scripts/verify/snapshot_audit.py`、`backend/tests/invariant_registry.rs`、
  `docs/architecture/{ARCH-3,P2-7-供应商名口径盘点,编码规范}.md`、`00_install_compat.sql` 注释。
- `docs/evidence/20261105-p2-7-supplier-name/` 下的**原始运行输出不改**（那是当次真实记录，仍写 DI-23），
  在 README 顶部标明「原编号 DI-23 → 现 DI-26」。

### 3.2 一致性自检（脚本可复跑）

```bash
# 视图编号集合必须 == CHECKS 登记集合（backend/tests/invariant_registry.rs 的同一判据）
diff <(grep -oE 'VIEW di\.c_di[0-9]+' docs/architecture/snapshot-drift/10_checks.sql \
        | sed 's/.*c_di//' | sort -u) \
     <(grep -oE 'dict\(id="DI-[0-9]+"' scripts/verify/snapshot_audit.py \
        | grep -oE '[0-9]+' | sort -u)
```
两侧均为 `01…26`，无重复、无缺口。

## 4. 执行实录（含退路）

1. **留退路（144 bare，先做）**：
   ```
   git -C /opt/git/cloudpivot.git tag -f backup/20261105-pre-union 9ed4bf8
   git -C /opt/git/cloudpivot.git branch -f backup/main-9ed4bf8 9ed4bf8
   ```
   两条 ref 都指向合并前的远端 main，随时可回滚。
2. 在**独立 worktree**（`git worktree add ... -b merge/union-20261105`）里完成合并、解冲突、自检
   —— 全程不碰共享工作树 `/opt/cloudpivot` 上其他人的未提交改动。
3. 自检（真实输出见本目录 `sql-install-check.txt`）：
   - `cargo check --all-targets`：**通过**（1m07s，0 error）；
   - `cargo test --test invariant_registry`：**1 passed**
     （`n01_check_view_and_registry_are_bijective`，视图 ↔ CHECKS 双向一一对应）；
   - `snapshot_audit.py --dry-run`：合计 **26 项**；`py_compile` 通过；
   - **SQL 真机安装**：在 144 上建一次性库 `cp_union_check_20261105`（`createdb -O cloudpivot`），
     `pg_dump --schema-only` 灌生产表结构，再 `psql -v ON_ERROR_STOP=1` 装
     `00_install_compat.sql` + `10_checks.sql` → **两文件均 OK**；`di.c_di01…c_di26` 共 **26 个视图**，
     `c_di23/24/25/26` 均可查询、`c_di26` 输出 **11 列**、`di.v_poly_ref_waiver` 返回
     `DI-18|biz_inventory_txn|manong|2027-02-04`。验证后已 `dropdb` 清理，生产库未动。
4. 推 144：`git push origin merge/union-20261105:main` → `9ed4bf8..a9bdcd1`（**快进**，不是 force push）。
5. **合并期间本地 main 又落了 1 条**（`97440ca` docs(manual) 菜单路径）⇒ 立刻做第二次 union merge
   `merge a9bdcd1 ← 97440ca` → `37e0549`，同样快进推送 `a9bdcd1..37e0549`。
   （这就是分叉的复发机制：只要有人在本地提交、有人往 bare 推，就会再次 ahead/behind。）
6. 共享工作树 `/opt/cloudpivot`：`HEAD` 已指向 `37e0549`，工作树用
   `git checkout 37e0549 -- .` 物化成合并树（合并无文件删除，故无需 `reset --hard`），
   再**逐个恢复**他人未提交的改动 —— 覆盖前先 `git stash create` 存了一份整树快照
   (`631331a`)，并把 17 个 WIP 文件按原样备份。
   - 与合并**有交叠**的只有 2 个文件：
     - `backend/src/business/erp/serial_handler.rs`：做 3-way merge（base=`97440ca` 原版、
       ours=WIP、theirs=合并版）→ **0 冲突**，两边改动都在；
     - `docs/architecture/snapshot-drift/10_checks.sql`：WIP 是「P2-6 收口（DI-19 分级 + DI-18 豁免）」
       的**并行另一份实现**，与已提交的 `7f0fe5c` 内容等价（只差一句注释措辞）⇒ **以已提交版为准**，
       WIP 那份不再恢复（已在卡面 comment 里告知）。
   - 6 个 flow 前端文件 + `snapshot_audit.py` 的 WIP 与合并树**逐字节相同**（本来就是同一条 FLOW-PANEL-UX /
     P2-6 工作的未提交副本）⇒ 自然变为 clean，无需恢复。
   - 其余 9 个 WIP 文件（bootstrap / payable_payment / flow.handler / internal_control / startup_guard /
     2 份 p2-8 证据时间戳 / verify README）+ 7 个 untracked 全部原样保留。
7. 校验：`git status -sb` → `## main...origin/main`（无 ahead/behind）；
   144 侧 `git log --oneline -3 main` = `37e0549 ← 97440ca ← a9bdcd1 ← 4f8230a/9ed4bf8`；
   `git merge-base --is-ancestor` 对 `4f8230a` 与 `9ed4bf8` 均 OK；备份 ref 仍在。
   `git ls-remote origin main` = `37e0549…`。

## 5. `post-commit` 钩子：**不做 ssh 自动推**

理由：
1. 共享工作树上随时有多个 worker 的半成品改动；每提交一次就自动推共享 bare = 把未验证的中间态发布给所有人（144 部署直接从 bare 拉）。
2. 之前的故障恰恰来自「各自把分支重放到 bare main 再推」，自动推会放大这个模式。
3. 钩子只认本机路径（`/opt/git/*.git`）是**刻意**的：本机镜像可随时重建，不承载协作语义。

替代方案（已落地）：`scripts/git/main_drift_check.sh` —— 只读检查本地 main 与 `origin/main` 的关系，
分叉（既 ahead 又 behind）时**非零退出并打印合并指引入口**，提醒「别 push、别 rebase、先开卡定口径」。
不接进 `scripts/check.sh`（CI 不该依赖网络可达性）。

## 6. 没做的事（诚实清单）

- 没有 `push -f`、没有 `rebase` 任何一条已有历史、没有删除 144 上的任何分支或提交。
- 没有跑后端全量 `cargo test` / 前端全量 `vitest`（合并的冲突裁定集中在文档 / 巡检 SQL / 路由 / 单测文件；
  做的是 `cargo check --all-targets` + `invariant_registry` 单测 + 巡检 SQL 真机安装 + 视图/登记集合比对，
  见 §4.3。全量回归仍应在下一条功能卡之前补跑一次）。
- 没有把 `github` 远端（`zonghengliao/cloudpivot`，main 同为 `9ed4bf8`）一起同步——本次口径只在 144；
  github 镜像是否跟进另开卡决定。
