# DEPLOY dep-v217 —— 生产补跑 V0212–V0217 + 二进制/dist 重发（2026-11-04）

> 触发：`deploy_check_v2.sh` 报「生产未生效」（v2 检查的是**生产是否真的生效**，四个信号：
> 迁移 / 二进制 / 前端 dist / 服务）。卡 t_ab5e9912。
> 结论：**已全部补齐并验收**；顺带把 `deploy.sh` 的两个假绿缺陷修掉（否则同样的漂移会再来一次）。

## 0. 一句话结论

| 信号 | 部署前 | 部署后 | 判据 |
|---|---|---|---|
| 库迁移号 | `V0211` | **`V0217`** | `max(version::int)`，且 6 条 checksum 与磁盘逐条相等 |
| 后端二进制 | 2026-11-03 21:14（`1793711656`） | **2026-11-04 12:23（`1793766183`）** | 晚于最近后端提交 11:46:26（`1793763986`） |
| 前端 dist | 2026-11-03 22:40（`1793716806`） | **2026-11-04 12:25（`1793766327`）** | 晚于最近前端提交 12:14:39（`1793765679`） |
| 服务 | active | **active** | `/health` ok、`/proc/<pid>/exe` md5 == 磁盘 md5 |

发布提交：`c003ab1`（含父卡 SAFARI-FLOW 的 `d496158`、检测脚本修复 `7fc5125`）。
备份：`/root/deploy-backup-dep-v217/{db.dump,cloudpivot,dist/}`。
日志：`/var/log/cloudpivot-deploy-dep-v217.log`。

## 1. 为什么会出现「工作区已到 V0217、库停在 V0211」

`deploy.sh`（DEPLOY-C 起入库）原先两个判据都只看 **git diff**：

1. **待跑迁移** = `git diff --diff-filter=A OLD..NEW -- backend/migrations/` —— 只认「这次同步新加的 up.sql」。
   实操里主仓一旦先同步了代码（迁移文件已落盘）却没对库跑，待跑迁移**不在 diff 里** ⇒ 脚本报「本次无新迁移」。
2. **后端/前端要不要重做** = diff 有没有碰 `backend/` / `frontend/` —— 二进制/产物比代码旧时同样跳过。

于是 v1 检查脚本看 git 落后数 = 0 → 显示"已部署"；`deploy.sh` 看 diff = 空 → "无改动"。**两边同时假绿**，
库与代码漂了 6 个迁移、二进制落后 1 天、dist 落后 15 小时。

## 2. 对 deploy.sh 的两处修改（已推 origin/main）

`7fc5125` 判据改成「**生产实况 vs 磁盘**」：

- 待跑迁移 = `git ls-tree $NEW_HEAD backend/migrations/business/` 里**版本号 > `max(version::int)`** 的 up 脚本；
- 后端需重做 = `backend/` 有 diff **或** `二进制 mtime < git log -1 --format=%ct -- backend/`；
- 前端需重做 = `frontend/` 有 diff **或** `dist mtime < git log -1 --format=%ct -- frontend/`。

`c003ab1` 调整步骤顺序（8 步）：**[4] 只编译不重启 → [5] 逐条迁移 → [6] 换二进制+重启**。
原来「迁移 → 编译(5–10min) → 重启」之间，仍在服务的旧二进制会引用已删列（V0217 删 `received_qty`），
这段窗口里单据接口会 500。改序后「旧二进制 × 新 schema」窗口 = 迁移耗时（实测 6 条共 **217 ms**，
`APPLY_OK` 单条 26–66 ms）。
顺带修掉 `health` 判据被转义成 `\"status\"` 匹配不上的问题（`grep -q '"status":"ok"'`）。

脚本对本次部署的实际判定（部署日志 [1/8]）：

```
[12:18:21] 二进制过期：线上二进制 2026-11-03 21:14:16 早于最近后端提交 2026-11-04 11:46:26 ⇒ 重编译
[12:18:21] 后端改动：有　　前端改动：有　　待应用迁移：6 个
```

## 3. 上生产前先在「生产库副本」预演（照 DEPLOY-B 的做法）

```bash
pg_dump -Fc → /root/reh-pre-V0212.dump                        # 1.8 MB
su - postgres -c "createdb -O cloudpivot cp_reh_212217"
su - postgres -c "pg_restore -d cp_reh_212217 --no-owner --role=cloudpivot" < /root/reh-pre-V0212.dump
bash scripts/verify/deploya_migrate.sh cp_reh_212217 212:erp_doc_attachment ... 217:erp_p2_3_received_qty_derived
```

演练库：107 表 / `maxver=211` / `biz_purchase_order` 17 行 / `biz_purchase_order_item` 17 行 /
`biz_stock_in` 15 行 / `received_qty` 列存在 —— **6 条迁移全部 APPLY_OK**，随后逐项结构校验通过
（`biz_doc_attachment` 1 表 4 索引、`received_qty` 消失、部分索引 `idx_biz_stock_in_source_posted` 出现、
采购单新增三列、7 条新权限码、`po_approve` v2 自检 hash 一致）。预演库与生产库同源，所以生产直接跑同一命令。

## 4. 生产执行（一条命令，8 步）

```bash
cp scripts/deploy/deploy.sh /root/deploy-run.sh     # 跑副本：ff 会覆盖仓库里的同一路径
bash /root/deploy-run.sh --tag dep-v217
```

| 步 | 结果 |
|---|---|
| [2] 备份 | `db.dump` 1 823 509 B / `cloudpivot` 53 377 720 B / `dist/` 14 M |
| [3] 同步 | `61a78ea → c003ab1`（Fast-forward，3 个提交） |
| [4] 编译 | `cargo build --release` jobs=1，**281 s**，新二进制 md5 `08dcf33b…` 53 532 744 B |
| [5] 迁移 | 6 条逐条 APPLY_OK（seq 162–167），`V0211 → V0217` |
| [6] 启用 | `/health` ok、`/proc/<pid>/exe` == 磁盘、**启动日志「数据库迁移完成: 应用 0 个迁移」** |
| [7] 前端 | `pnpm typecheck` 0 错 → `vite build --outDir dist-dep-v217` → 双 rename → 线上入口 == dist 入口 `assets/index-bjgrlE7E.js` |
| [8] 收口 | HEAD == origin/main == `c003ab1`、迁移 V0217、服务 active、入口一致 |

**幂等性证据**：部署完再跑一次 `--dry-run` → `待应用迁移：0 个 / 无后端改动 / 无前端改动`。

## 5. 验收（六项）

### 5.1 迁移：库 == 磁盘，且记账口径与引擎一致

```
max(version::int) = 217       磁盘最新 = V0217__erp_p2_3_received_qty_derived
V0212 a43a87b2fa46…  OK      V0215 ac6c0c02e19c…  OK
V0213 55673c0277ab…  OK      V0216 f348c91f184b…  OK
V0214 3b8179b13c3c…  OK      V0217 646c813c41ca…  OK      （disk sha256 == schema_migrations.checksum）
```

重启日志「应用 0 个迁移」= 引擎认这 6 条账，不会重放；checksum 逐条与磁盘相同 = 不会因指纹不符回滚重建。
另有一条独立交叉验证：在 99 用**全新空库**启动同一份代码，引擎自己把 1..217 全部应用（102 表、admin 1 行、
`maxver=217`），说明这些迁移在全新库上也成立，不依赖生产存量。

### 5.2 结构（生产库实测）

| 项 | 结果 |
|---|---|
| `biz_doc_attachment` 表（V0212） | 1 表 / 4 索引 / 2 约束 / 3 权限节点（迁移自检 NOTICE） |
| `biz_purchase_order_item.received_qty`（V0217 删列） | **0 命中**（列已不存在） |
| `idx_biz_stock_in_source_posted` 部分索引 | 1 条 |
| `biz_purchase_order` 新三列 `over_credit_limit / credit_exposure / credit_limit_snapshot`（V0215） | 3 列齐 |
| 新权限码（V0212 3 个 + V0213 2 个×3 业务线 + V0214 1 个） | 7 行命中 |
| `po_approve` 流程（V0216） | `PUBLISHED` / `current_version_no=2` / **9 节点 / 11 边** |
| `schema_migrations` 212–217 | 6 行，`success=t` 全绿 |

### 5.3 不变量 / 巡检（生产库，只读）

- `scripts/verify/erp_invariants.py`：**40/40 通过，BLOCKER 0 / WARN 0**（含 H4 审批已通过单据必须前进、
  I2 盘点差异==库存流水、E 凭证期间、G 业务闭环等）。
- `scripts/verify/snapshot_audit.py`：**16/16 通过**（含 DI-14 结构护栏自检：约束/部分唯一索引无缺失）。

### 5.4 真机抽查（`scripts/verify/dep_v217_spotcheck.mjs`，打 144 的 nginx → dist → 8023）

**20 项断言全过**（截图在本目录 `shots/`，原始结果 `shots/result.json`）：

```
PASS | 采购订单分页接口 | rows=10
PASS | P2-2 额度三列在 DTO 里 | over=false exposure=null snapshot=null
PASS | P2-3 明细「已收数量」为现算派生值（接口有字段，库无列） | rows=5 first.received_qty=0.0000 qty=100.0000
PASS | P2-3 待收明细接口可用（qty − 已收 现算） | code=0 rows=1
PASS | V0213 联动作废影响分析接口 200/code=0
PASS | V0212 单据附件列表接口 200/code=0 | total=0
PASS | V0214 辅助账接口 200/code=0 | rows=2
PASS | P2-1 按物料查辅助账（有发生额/余额行） | material=d1370003-…-000000000001 code=0
PASS | P2-2b po_approve 已发布 v2（9 节点 / 11 边）
PASS | 页面 /business/erp/purchase-order（13 行数据、无 4xx/5xx）
PASS | 页面 /business/erp/ledger（余额表）
PASS | 页面 /business/impexp/import（导入向导 = P1-6 6 个主数据定义入口）
PASS | 页面 /flow/def（11 条流程定义）
PASS | 页面 /flow/def/{po_approve}/edit（画布）
PASS | SAFARI-FLOW 画布节点位置分散（不是全叠在原点）| nodes=12 distinctPos=6
PASS | 页面 /business/erp/voucher
```

截图 `05-flow-canvas-po-approve.png` 里可见 v2 完整拓扑：`开始 → 金额判定 → 财务复核分支 →（<1万/1–10万/≥10万 三档）
→ 会签 → 结束`，节点分散、连线正常 —— 即父卡 `d496158` 改过的 `FlowCanvas` 在现网 dist 上真的渲染出来了。

> 截图里的中文显示成方框，是**取证容器缺中文字体**（headless Chromium 未装 CJK 字体），不是产品缺陷；
> DOM 文本判据（`page.innerText` 命中「采购/余额/导入/流程/凭证」）已独立证明文案正常。

### 5.5 前端单测

`pnpm test:unit`：**606/606 pass**（在 99，同一提交 `c003ab1`）。

## 6. 顺带发现 / 未做

- `deploy_check_v2.sh` 算「迁移落后 97 个」是**算法错**：它拿 `max(version)`（91，只对 system 迁移有意义）
  跟磁盘号（217）比；business 迁移的 `version` 列存的是**不补零的文件号**且与 system 分属两套版本空间。
  正确判据是「库 `max(version::int)` vs 磁盘 business 最大号」。本次未改那个脚本（属 laoliu 的巡检侧），
  但 `deploy.sh` 已改成按「磁盘 vs 库」判，两者口径可以对齐了。
- 生产上遗留两个演练/测试库待清理：`cp_reh_212217`（本次预演）、99 上的 `cp_gate_v217`（本次门禁用）。
- 生产 `frontend/` 下历史 dist 备份已累积 9 个（各 13–14 MB），未清理（不属本卡范围）。
