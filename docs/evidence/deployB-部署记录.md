# DEPLOY-B 部署记录：把 `d04054b` 的 35 个提交真正上线

- 执行时间：2026-11-03 10:49 → 11:45 CST（144 生产机）
- 目标：主仓 / 生产二进制 / 生产 dist / 生产库迁移从 **DEPLOY-A 的 7044423 + V0200** 推进到 **`d04054b` + V0206**
- 结论：**6 个迁移全部上线、后端/前端均换新、真机 10 项里 9 项通过**（第 ⑧ 项的**前端入口**未交付，见 §7）
- 回归：`erp_invariants` 36/36 全绿、`snapshot_audit` 16/16 全绿、`pnpm test:unit` 521/522、`cargo test` 2030/2087（57 条旧夹具与新种子不符，见 §10）

---

## 1. 备份（三份，均可回滚）

| 产物 | 路径 | 大小 / 指纹 |
|---|---|---|
| 全库 | `/root/backup-pre-V0201-20261103-105033.dump` | 1,712,094 B（pg_dump -Fc，库 29 MB） |
| 旧二进制 | `/root/cloudpivot-bin-pre-DEPLOYB` | 49,887,648 B，md5 `8ad2905f4d3527bd458b8caf714e8624` |
| 旧 dist | `/root/dist-pre-DEPLOYB` | 14 MB，入口 `assets/index-C7HCCblS.js` |

证据：`deployB-01-backup.txt`

## 2. 主仓同步（ff-only，无冲突）

```
HEAD 前  7044423 chore(deploy): DEPLOY-A …
HEAD 后  d04054b739e7fa48e41556c99c1b1070b4011a8e
origin/main == bare /opt/git/cloudpivot.git main == d04054b  ✅ 一致
6 个新迁移 + stockout_{base,dto,handler,model,tests}.rs + frontend stock-out/{index.vue,stock-out-logic.ts} 全部到位
```

证据：`deployB-02-sync.txt`
说明：卡面写的前端路径是 `views/business/erp/stockout/`，**实际是 `stock-out/`（带连字符）**，与 `sys_menu.path` 同口径。

## 3. 六个迁移：先在生产库副本上演一遍，再逐个上生产

因为「应用迁移」只有入口没有 CLI（服务启动时一次性应用），且卡面要求「逐个跑、逐个验」，做法分两段：

1. **预演**：`createdb cloudpivot_rehearsal_b` + 用当日全库 dump 恢复 → 用 `scripts/verify/deploya_migrate.sh` 逐条应用并记账 → 逐项结构校验通过；
2. **上生产**：同一条命令对 `cloudpivot` 库逐条应用（每跑一条立刻校验该条的对象），T+0 备份已在 §1。

生产逐个应用结果（`applied_via=imported`，引擎重启时校验 checksum 并输出「应用 0 个迁移」）：

| 版本 | 迁移 | 耗时 | checksum 前 16 位 | 自检结论（迁移内 NOTICE） |
|---|---|---|---|---|
| V0201 | erp_material_costing_moving_avg_only | 60 ms | `414f854db3f59110…` | 存量 0 行非法；约束收敛为单值 MOVING_AVG；同表另 3 条 CHECK 完好 |
| V0202 | erp_approval_flow_health | 50 ms | `8ff21e1c34734b85…` | 2 个权限码 + 8 个审批流程全部 PUBLISHED |
| V0203 | erp_stock_out | 82 ms | `2222dd873161691f…` | 出库两表 / 11 索引 / 7 约束 / 6 权限码 / 1 取号规则 |
| V0204 | flow_state_edge | 36 ms | `9f7e904ed338ac0c…` | 建表 + 唯一约束，初始 0 行（全用 BASE_EDGES） |
| V0205 | flow_linkage_rule | 38 ms | `416e2656c20b9805…` | 白名单 CHECK + 禁 SQL 键 CHECK，初始 0 行 |
| V0206 | flow_edge_config_admin | 41 ms | `5c89cc3a50dd96e6…` | 2 权限码 + `flow_edge_config` 种子流程（s→n_cfg→e） |

迁移后结构自检（`scripts/verify/deployB_schema_check.sh cloudpivot`，证据 `deployB-03-migrations.txt`）：

```
schema_migrations ≥201 : 201..206 六条，全部 imported + success
V0201 ck_biz_material_costing : CHECK ((((costing_method)::text = 'MOVING_AVG'::text) IS TRUE))
V0201 违反行数 0 ; 同表 ck_biz_material_{type,safety,tax} 仍在 3/3
V0202 权限码 : business:erp:approval-flow:health / business:erp:skip-approval
V0203 列数  : biz_stock_out 21 列 / biz_stock_out_item 13 列；6 个 stock-out 权限码；取号 STOCK_OUT|OUT|0
V0204       : biz_flow_state_edge 存在，UNIQUE (entity, from_status, to_status)
V0205       : biz_flow_linkage_rule 存在，ck_flow_linkage_action_code 11 个白名单动作 + ck_flow_linkage_no_sql_key
V0206       : business:flow:state-edge:list / :manage；角色 FLOW_CONFIG_APPROVER；flow_edge_config|PUBLISHED|FLOW_EDGE_CFG|1，3 节点
四张新表合计 = 4  ✅
```

▶ 卡面第 3 步「V0201 存量体检」实测：`select costing_method,count(*) from biz_material group by 1` → `MOVING_AVG|7`，即 **0 行需要回填**，迁移里的 S1 NOTICE 也是「0 行（其中有效 0 行）」。

## 4. 后端编译 + 重启

```
cargo build --release  10:51:10 → 10:53:39（2 分 29 秒，4 核，依赖已热）
新二进制 target/release/cloudpivot  51,846,488 B  md5 ac855ac87e82a33e3f988ba5733dad16
重启后 MainPID 4188039：proc_md5 == disk_md5 == ac855ac8…  ✅（进程 == 磁盘）
systemctl: active (running)
/health: {"code":0,"msg":"ok",…,"profile":"prod"}
启动日志: 数据库迁移完成: 应用 0 个迁移   ✅（引擎认可 6 条 imported 记录与 checksum）
```

证据：`deployB-04-build-restart.txt`
编译器只报 1 条**既有**警告（`stockout_tests.rs:29 use nik::prelude::*` 未使用），属被上线代码自身的 warning，未改。

## 5. 前端构建 + 原子替换

```
pnpm install --frozen-lockfile  → Already up to date (rc=0)
pnpm typecheck (vue-tsc)        → rc=0（0 错）
pnpm exec vite build --outDir dist-new → rc=0，271 个 chunk，14 MB
替换前 dist 入口 : assets/index-C7HCCblS.js
替换后 dist 入口 : assets/index-COHYtSHL.js
线上 http://127.0.0.1/ : assets/index-COHYtSHL.js   ✅ 与 dist 一致
入口 chunk  : 200，115,562 B
出库单页面 chunk 存在 : assets/index-DbuYH0X2.js（含 stockout-page）
```

做法：**先 build 到 `dist-new` 成功，再 `mv dist dist-prev-<ts>; mv dist-new dist`**，全程 nginx root 不指向空目录（两次 rename，毫秒级）。
证据：`deployB-05-frontend.txt`

## 6. 真机 10 项（Playwright 打 nginx → 线上 dist → 新二进制 → 生产库）

脚本 `scripts/verify/deployB_e2e.mjs`，完整日志 `deployB-e2e.log`，截图 `deployB-01…16-*.png`（16 张）。
最后一轮（TAG=DEPLOYB-20261103034751）：**PASS 29 / FAIL 1 / SKIP 0**。

| # | 项目 | 结果 | 取证 |
|---|---|---|---|
| ① | 登录能进、不白屏 | ✅ | 页面文本 506 字 + localStorage 有令牌；`deployB-01-login-home.png` |
| ② | 出库单页不 404 / 建领用·报废·盘亏 / 过账后库存减 + 成本结转凭证 | ✅ | `/business/erp/stock-out` 打开；UI 建 `OUT-20261103-000007` 领用单（2 kg）→ 过账 → **成本账 391.0000 → 389.0000**，`total_amount=4.0000`，凭证 `voucher_id=2ca98111-b416-4c91-86ff-9dde8236c637`；报废 `OUT-…000002` / 盘亏 `OUT-…000003` 草稿可建；截图 02–07 |
| ③ | 成本法只剩「移动加权」 | ✅ | 物料编辑弹窗成本法控件值 = `移动加权平均（MOVING_AVG）` 且 `disabled=true`（按设计置灰），灰字「本版本仅支持移动加权平均…FIFO/标准成本属路线图能力，暂不可选」，展开无第二项；`deployB-08-material-costing-disabled-single.png` |
| ④ | 非基本单位被拒 | ✅ | API `POST /material`（单位「箱」）→ **400 + 66030**「物料的计量单位必须是基本单位…」；UI 表单选「箱」保存 → 同一文案；`deployB-09-material-unit-rejected.png` |
| ⑤ | 已取消采购单收货被拒 | ✅ | 取 `PO-20261031-000005`（CANCELLED）建收货 → **400 + 10001**「当前是「已取消」（CANCELLED），不能收货」 |
| ⑥ | 审批流未配置 → 显式 409 | ✅ | `scripts/verify/deployB_flowgate.sh`：临时把 `po_approve` 置 DISABLED（**10 秒窗口，脚本 trap 自动恢复**）→ 提交 DRAFT 采购单 `PO-20261102-000002` → **409 + 66033**，报文点名 `po_approve`；单据仍是 DRAFT（一分没动）；恢复后 `approval-flow/health` = 8/8 healthy。证据 `deployB-06-flowgate.txt` |
| ⑦ | 四单审批进度（审批人 + 时间） | ✅ | 采购/收货/应付/凭证/调拨五个详情都能看到审批信息；调拨单时间线实测「超级管理员 已同意 2026-10-31 11:35:21 / Henry.Liao 已同意 … / 老六 已同意 2026-11-02 11:33…」；截图 10–14 |
| ⑧ | 流程定义页能看到「边配置」入口 | ❌ **未通过** | 后端 `GET /business/flow/state-edge?entity=STOCK_OUT` → 200 + 6 条 BASE 边（HTTP 面可用）；前端 `/flow/def` 能打开（888 字、9 个流程），但**页面上「边配置」按钮/链接数 = 0**（见 §7） |
| ⑨ | 快照巡检能出结果 | ✅ | `python3 scripts/verify/snapshot_audit.py`（先 `--install-views` 建只读视图）→ 16 项全过，BLOCKER 0 / WARN 0 / INFO 0，退出码 0 |
| ⑩ | 控制台无报错、无意外 4xx/5xx | ✅ | 控制台 0 条意外 error（唯一一条是 ④ 负向断言触发的 `400 Bad Request` 资源日志，已单列）；API 无意外 4xx/5xx |

## 7. 未通过项：⑧ 的前端「边配置」入口不存在（如实报告，未擅自修改）

> 取证过程更正一处：第一版脚本把流程定义页的 URL 写成 `/business/flow/def`，那是 **404 页**
> （`frontend/src/router/index.ts` 里该路由是挂在 `/` 下的子路由 `flow/def`，与
> `sys_menu.path` 同口径，**不带 `business/` 前缀**）。改用 `/flow/def` 复查后页面正常渲染
> （9 个流程），结论不变：**页面上没有「边配置」入口**。`deployB-e2e.log` 是修正后重跑的一轮。

- 后端：`backend/src/business/flow/edge_admin.rs`（V0206 + D3）路由 `GET /business/flow/state-edge`、
  `POST /business/flow/state-edge/change` 可用，实测 200 / code 0 / STOCK_OUT 6 条边（含 mandatory 与 requires_reason 标记）。
- 前端：`grep -rl "state-edge\|stateEdge" frontend/src` → **0 个文件**；
  `/flow/def` 页面按钮实测只有「查询 / 重置 / 新建流程 / 刷新」+ 每行「编辑 / 发布 / 停用 / 版本历史 / 删除」，
  `button:has-text("边配置")` 计数 **0**。
- 提交核对：D3 的功能提交 `2e1f90d` 的文件清单里**没有任何 `frontend/` 文件**
  （`git show --stat 2e1f90d` 只动 backend + 迁移 + 脚本 + 证据）。卡面写的「前端按钮联动」实际落的是
  **出库单动作按钮由 `next_statuses` 驱动**，不是「流程定义页的边配置入口」。
- 结论：这是**交付缺口**（后端有面、前端无入口），需另开卡补前端；本卡按「发现问题只报告」处理。

## 8. 回归

| 项 | 结果 | 说明 |
|---|---|---|
| `python3 scripts/verify/erp_invariants.py` | **36/36 通过，BLOCKER 0 / WARN 0** | 含 H1/H2/H3 审批闭环、F1/F2 期间控制 |
| `python3 scripts/verify/snapshot_audit.py` | **16 项全过，BLOCKER 0 / WARN 0** | 首次需 `--install-views`（建 `di` 只读视图，回滚 `DROP SCHEMA di CASCADE`） |
| `pnpm test:unit`（= `tsx --test tests/unit/**`） | **522 条：521 pass / 1 fail** | 唯一红：`tests/unit/task-root-only.test.ts`「model.rs 的 TaskFilter 必须有归一化后的 root_only」——TD-2 把 `task/model.rs` 拆成 142 行门面后，该字段搬到 `task_model_query.rs`，用例仍按**文件路径**断言 ⇒ 属 TD-2 的用例口径问题，本卡未改 |
| `cargo test -j 1`（`ERP_DB_URL` → 干净库 `cloudpivot_deployb_test`，已迁到 V0206） | **2030 passed / 57 failed / 0 ignored** | 57 条全部落在「收货单作废 / 过账审批」链路 + 3 条 teardown，见 §10 |

## 9. 生产数据变动（本次真机验收造的单据）

| 单据 | 说明 |
|---|---|
| `OUT-20261103-000001` / `…000004` / `…000006` / `…000007` | 领用单，各 2 kg @ M-W1A-001 分仓，均已过账（成本 4.00 元/张，各生成一张成本结转凭证）—— 真机脚本反复调了几轮，每轮新建一张 |
| `OUT-20261103-000002` | 报废单草稿（事由：设备老化） |
| `OUT-20261103-000003` | 盘亏单草稿（事由：盘点差异） |
| 物料 M-W1A-001（分仓） | 成本账 **399 → 389**（4 张过账出库各 -2 kg，`amount` 798 → 778），已复核 `erp_invariants` 36/36 仍然全绿 |
| 其它 | 期间未改动其它单据；`po_approve` 曾临时 DISABLED 约 10 秒后由 trap 恢复（复核 PUBLISHED + health 8/8） |

## 10. cargo test 57 条失败的定位（本卡不改代码，只给证据）

现象：单据在「复核通过」后仍停在 `PENDING_CANCEL`，断言 `复核通过后单据必须真的作废` 失败。
定位（在 `cloudpivot_deployb_test` 库上直接查失败残留）：

```
单据 IN-20261103-000210  status=PENDING_CANCEL  create_by=NULL
审批实例 FLW-20261103-2D3E6287  biz_type=STOCK_IN_CANCEL  status=PENDING   ← 实例还没终态
待办：n_review  PENDING  p17_ap1      ← 还没批
      n_review  PENDING  p17_ap2      ← 还没批
      n_review  APPROVED admin  单测：同意作废
settle_guard 留痕（comment like '审批结论未落单据%'）= 0   ← 前置校验没有拦，是回调压根没被触发
种子流程节点：stock_in_cancel / n_review / APPROVAL / self_approval=FORBID
```

即：**`stock_in_cancel` 种子流程是会签（3 个审批人），而测试夹具 `approve_cancel()` 只批第一条待办**，实例仍 `PENDING` → 回调不会以 `APPROVED` 触发 → 单据停在 `PENDING_CANCEL`。
生产语义没问题（作废本来要 3 人复核）；错的是**测试夹具的假设**（旧种子是单人节点）。同类失败共 57 条（`stockin_handler_tests_cancel::*`、`*_a4::a4_11_*`、`d4_*`、以及 3 条 `zz_teardown_*` 收尾）。
另注：TD-2 卡 3 小时前在同一提交上跑出 2087/0，差异来自**它用的库与本次干净库的种子/夹具状态不同**，属环境+夹具口径问题，需单开卡修夹具，不在本卡范围。

## 11. 回滚手册（如后续发现问题）

```bash
# 后端：换回旧二进制并重启
systemctl stop cloudpivot
cp /root/cloudpivot-bin-pre-DEPLOYB /opt/cloudpivot/backend/target/release/cloudpivot.new
mv -f /opt/cloudpivot/backend/target/release/cloudpivot.new /opt/cloudpivot/backend/target/release/cloudpivot
systemctl start cloudpivot

# 前端：换回旧 dist
rm -rf /opt/cloudpivot/frontend/dist && cp -a /root/dist-pre-DEPLOYB /opt/cloudpivot/frontend/dist

# 库：整库回滚（V0201–V0206 的 down 也可单条执行，见 migrations/business/V020x__*.down.sql）
pg_restore -h 127.0.0.1 -U cloudpivot -d cloudpivot --clean --if-exists /root/backup-pre-V0201-20261103-105033.dump
```

## 12. 文件清单

```
scripts/verify/deployB_e2e.mjs           真机 10 项（Playwright）
scripts/verify/deployB_flowgate.sh       ⑥ 的取证（临时下线 → 409+66033 → 自动恢复 → 复查 8/8）
scripts/verify/deployB_flowgate.mjs      ⑥ 的 HTTP 断言
scripts/verify/deployB_schema_check.sh   迁移后结构自检（逐项对上卡面验证点）
docs/evidence/deployB-01-backup.txt      备份三件
docs/evidence/deployB-02-sync.txt        主仓同步 + 关键文件在位
docs/evidence/deployB-03-migrations.txt  6 个迁移逐项结构校验
docs/evidence/deployB-04-build-restart.txt  编译 + 进程==磁盘 + health + 0 迁移
docs/evidence/deployB-05-frontend.txt    前端 install/typecheck/build + 入口替换
docs/evidence/deployB-06-flowgate.txt    ⑥ 409+66033 取证 + 恢复复查
docs/evidence/deployB-07-invariants.txt  不变量 36/36 + 巡检 16/16
docs/evidence/deployB-e2e.log            真机 10 项完整日志（PASS 29 / FAIL 1，16 张截图路径在日志里）
docs/evidence/deployB-01…16-*.png        16 张截图
```
