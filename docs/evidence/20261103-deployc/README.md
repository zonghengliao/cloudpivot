# DEPLOY-C 证据包（2026-11-03 21:08–21:5x）

把 **42f671e → 24655b0** 的 11 个提交上线到生产 144，并留下 `scripts/deploy/deploy.sh` 一键部署脚本。
**本卡全程用这个脚本完成的真实部署**（不是先手工做再补脚本）。

## 1. 待上线提交清单（全量，`git log 42f671e..24655b0`）

```
24655b0 chore(deploy): DEPLOY-C 一键部署脚本（备份/ff/迁移/按改动编译重启/入口指纹/失败回滚）   ← 本卡新增
1827a89 feat(erp): P1-3 盘点单（账实差异有单据：建单→审批→过账→盘盈盘亏凭证→作废红冲）
e89d9d3 feat(notice): P1-2 业务提醒任务包（应付到期/采购超期/合同到期/低库存，幂等防轰炸）
a360243 feat(erp): P1-1 合同审批流（合同现在没人审就生效）+ 审批终态→单据必须前进巡检
00d3753 chore(evidence): YEAR-END 补齐门禁证据（全量 2125/0、不变量 36/36、前端 553/0、真机 9 图 5 phase）
d6424e8 feat(erp): YEAR-END 年结检查清单——列明细 + 标处理人 + 一键催办
bda2476 docs: WRAPUP 报告补 github 对齐的完整时间线（推错旧 main 线 → 纠正 → 收尾提交后仍三方一致）
7a042f6 chore: WRAPUP 夜间批次收尾——github 对齐 6ca1193 + 六项回归实跑 + 夜间批次报告
6ca1193 chore(evidence): PERIOD-CLOSE 补齐门禁证据（全量 2111/0、不变量 36/36、前端 543/0、真机 9 图 4 phase）
c528bf6 feat(erp): PERIOD-CLOSE 结账检查清单——列明细 + 标处理人 + 一键催办
a64e8b8 fix(test): 夹具批到终态以适配「会签 N 人」，修收货作废链路 57 条假红 (t_6d815eda)
```

部署前：主仓/生产 `42f671e`、bare main `24655b0`、生产库迁移最大 `V0206`。
部署后：主仓 HEAD == origin/main == `24655b0`、生产库最大 `V0211`。

## 2. 备份三件套（`/root/deploy-backup-20261103-210832/`）

| 项 | 大小 |
|---|---|
| `db.dump`（`pg_dump -Fc`，库 29 MB / 104 表） | 1,774,206 字节 |
| `cloudpivot`（部署前的 release 二进制） | 51,846,488 字节 |
| `dist/`（部署前的前端） | 14 M |

## 3. 新迁移：5 条，逐条跑 + 按引擎口径记账（**本次有新迁移**）

| 版本 | 名称 | checksum(前 16) | 耗时 | 自检 NOTICE |
|---|---|---|---|---|
| V0207 | erp_period_remind | 0a4fa0dee91c9fed | 37 ms | 结账催办 1 个权限节点 |
| V0208 | erp_year_end_remind | 07c0dc5b7ad6a657 | 31 ms | 年结催办 1 个权限节点 |
| V0209 | erp_contract_approval | 747f0d381c79e712 | 59 ms | 合同审批流（金额两档分级）就绪；状态白名单扩到 6 值 |
| V0210 | erp_business_reminder | f6c973f5d26673cd | 74 ms | 台账 9 列 + 幂等唯一索引 + 4 模板 + 4 任务 + 2 参数 + 1 权限节点 |
| V0211 | erp_stock_take | 139e49ddc6b08d03 | 88 ms | 盘点两表 / 8 索引 / 7 约束 / 6 权限码 / 1 取号规则 / 1 审批流程 |

重启后日志出现 **「数据库迁移完成: 应用 0 个迁移」** ⇒ 记账与引擎一致（引擎只校验 checksum）。

## 4. 后端 / 前端

- 后端：`cargo build --release`（`CARGO_BUILD_JOBS=1`）5m42s → 新二进制 **md5 68e9c40c301b6d324d4986425ef77f1c**、53,377,720 字节
  → `systemctl restart cloudpivot` → `/health` 200；**进程 md5 == 磁盘 md5**（PID 195288）
- 前端：`pnpm typecheck` **0 错** → `vite build --outDir dist-20261103-210832` → 双 rename（旧 `dist` 留 `dist-prev-20261103-210832`）
  → 新入口 `assets/index-CE6Ojb_S.js` == `curl http://127.0.0.1/` 的线上入口 ⇒ **入口指纹一致**
  （部署前线上入口是 `assets/index-CURoWZzb.js`）

## 5. 真机验收（生产实例 `http://127.0.0.1`，`scripts/verify/deployc_e2e.mjs`）

**24 项断言全 PASS，18 张截图**（`shots/`，原图在 144 `/root/dc-evidence/run5/`；`result.json` 是原始结论）。

- ① 会计期间 2026-10 结账弹窗：预检「不通过」+ **挡路单据明细表格 7 行**，每行都有「该谁处理 / 去处理 / 催办」
  （原文见 `page-text.json`：`PO-20261031-000002 采购 ¥200.00 老六 超级管理员 3 天 [去收货][催办]` …）
- ② 年结 2026：五项检查（❌12 个月全部结账 / ❌无未完成单据 8 张 / ✅试算平衡 / ✅不变量全绿 / ❌下一年度已开账）
  + **明细表 8 行**（含 2026-11 的 `PO-20261102-000002 草稿（未提交）`）
- ③ 流程定义页「边配置」：面板打开、**10 条边**列出；提交一条变更 → 生成审批单 `FLW-20261103-B8005F58`
  （toast「已提交审批 … 审批通过后才会生效」）→ **验收完已驳回**（实例状态 `REJECTED`，`biz_flow_state_edge` 仍 0 行 = BASE 未变）
- ④ 出库单：领用单 `OUT-20261103-000008`（M-W1A-001 × 1）保存草稿 → **过账成功（状态=已过账）**，
  分仓库存 `389/778 → 388/776`，生成凭证 `PZ-20261103-000006`（借 2.00 / 贷 2.00，来源 OUT-20261103-000008）
- ⑤ 登录不白屏（`/dashboard`，body 799 字）+ **控制台 0 条 error、0 个 4xx/5xx**

### 本卡在生产上产生的数据（可回滚）

| 对象 | 说明 |
|---|---|
| `OUT-20261103-000008`（领用单，已过账） | 验收建单；分仓 M-W1A-001 扣 1 件 / 扣 ¥2.00；走「作废」可红冲回滚 |
| `PZ-20261103-000006`（成本结转凭证） | 该单自动生成，借 6401 2.00 / 贷 1405 2.00 |
| `FLW-20261103-B8005F58`（边配置变更审批） | 探针单，**已驳回**，配置未生效 |

## 6. 四项回归

| 项 | 结果 |
|---|---|
| `erp_invariants.py`（生产库） | **40/40 通过，BLOCKER 0 / WARN 0** → `invariants.txt` |
| `snapshot_audit.py`（生产库） | **16 项全通过，BLOCKER 0 / WARN 0**，退出码 0 → `snapshot.txt` |
| `pnpm test:unit`（生产树 frontend） | **552 通过 / 1 失败** ← 真缺陷，见第 7 节（已开卡） |
| `cargo test -j 1`（干净隔离库 `cloudpivot_deployc` + `ERP_DB_URL`） | **2187 通过 / 2 失败 / 0 ignored**（631.8 s）← 两条都是**并发互踩**，单跑即绿，见下 |

隔离库用**新 release 二进制**自举：`数据库迁移完成: 应用 155 个迁移` → 迁移最大 `V211` / 101 表 / admin 已种，
`SKIP: 未设置 ERP_DB_URL` 计数 **0**，所以真库用例不是「SKIP 假绿」。原始日志：`cargo-test-clean.log`
（另一份同口径首轮日志 `cargo-test-parallel.log`：2188/1，失败项是同一族并发互踩）。

### 那 2 条失败是什么（都证伪了「业务回归」）

| 用例 | 报错 | 单跑 |
|---|---|---|
| `payable_bridge::tests::zz_teardown_purges_payment_status_logs` | `10001 审批流程 payable_pay 已发布：有流程时必须走审批，不能跳过` | `1 passed` |
| `period_items::tests::can_close_is_unaffected_by_items` | `assertion failed: !blocked.can_close`（period_items_tests.rs:459） | `1 passed`（整模块 13 passed） |

根因（都是**用例夹具身份不唯一**，不是这次上线引入的业务变化）：

1. 该用例先 `UPDATE biz_flow_def SET status='DISABLED' WHERE flow_code='payable_pay'` 再直付；
   并行跑的别的流程用例把 `payable_pay` 又发布回来 ⇒ 直付被审批闸拦。
2. `period_items` 用例固定用期次 `2033-10`，并 `UPDATE biz_voucher SET status='POSTED' WHERE period='2033-10'`
   （按整期改，无夹具前缀）⇒ 并行时把兄弟用例刚种下的草稿凭证一并过账了。

串行（`--test-threads=1`）在同一库上反而红了 `contract_approval_tests` 2 条（顺序污染），
说明**换线程数治不了，得让夹具身份唯一**。已开卡给 `test-enginner`：`t_0d7c51f8`。

顺带核过生产：`biz_flow_def` 里 `contract_approve`（V0209 种）**PUBLISHED**、`payable_pay` 仍 PUBLISHED，
即「合同要走审批、应付付款要走审批」的线上口径与上线前一致，没有被这次部署改动。


## 7. 已知缺陷（本卡只报告，不改业务代码）

`frontend/tests/unit/erp-contract.test.ts` ⑦ 红：
**前端 `CONTRACT_EDGES` 还是 P1-1 之前的 5 条边**（`DRAFT→SIGNED` 直签），而后端已扩到 7 条
（`DRAFT→PENDING_APPROVAL→SIGNED`、`PENDING_APPROVAL→DRAFT`）。影响：

- 详情抽屉的按钮走后端 `next_statuses`，**真实动作不受影响**；
- 但前端回落表 / `needsReason`（驳回边是否必填原因）/ `TRANSITION_LABELS` 仍是旧的 ⇒ 列表行快捷动作、
  驳回原因校验会与后端状态机不一致；
- 守卫用例红（553 → 552/1）。

已开跟进卡给 `frontend-developer`（同步 `CONTRACT_EDGES` + `CONTRACT_REASON_EDGES` + 标签）：`t_87588b45`。

另有一处**编译告警**（非本卡引入、也不影响运行，但违「零警告」约定）：
`src/business/erp/stockout_tests.rs:29 use nik::prelude::*;` 未被使用 → `warning: unused import`。

## 8. 一键部署脚本的第二次运行（幂等性证据）

`deploy-rerun-idempotent.log`：在同一台机上再跑一次 `scripts/deploy/deploy.sh`（此时 HEAD == origin/main）：

```
✅ 本次无新迁移（已应用最大版本 V0211）
[5/7] 无后端改动，跳过编译与重启
[6/7] 无前端改动，跳过 dist 构建
✅ 主仓 HEAD == origin/main ／ 服务 active ／ /health ok ／ 进程==磁盘 yes ／ 入口指纹一致
================= 部署完成 =================
```

## 9. 本次复用的文件

- `scripts/deploy/deploy.sh`（本卡新增，**这次部署就是它跑的**）+ `scripts/deploy/README.md`
- `scripts/verify/deployc_e2e.mjs`（24 项断言 + 截图）、`scripts/verify/deployc_dump.mjs`（页面文本取证）
- `scripts/verify/deploya_migrate.sh`（逐条迁移 + 引擎口径记账，DEPLOY-A 留下）
