# DEPLOY-A 部署与真机验收记录（2026-11-02）

把 A1–A6 六张卡（合同表 / 合同 CRUD / 采购单绑合同 / 入库单改绑 / 合同页面 / 采购入库前端配合）
**真正落到生产**：跑完 V0193–V0200 迁移、编译并替换后端二进制、构建并原子替换前端 dist，
再在生产实例上做 10 项真机验证。

- 工作仓：`99:/opt/cloudpivot`（HEAD `6cfdc4d`，与 bare main 一致）
- 生产机：`144:/opt/cloudpivot`（`systemctl restart cloudpivot`，端口 8023，nginx :80 → `frontend/dist`）
- 数据库：PostgreSQL 18.6，库 `cloudpivot`，应用账号 `cloudpivot`（非 superuser）

## 0. 结论（卡面验收清单）

| 验收项 | 结果 |
|---|---|
| 备份文件存在且 > 0 字节 | ✅ `backup-cloudpivot-pre-V0195-20261102-195536.dump` 1,683,223 B（1375 个归档项） |
| 迁移全部跑完，`schema_migrations` ≥0195 有 6 条 | ✅ V0195–V0200 共 6 条（另 V0193/V0194 也在本次落地，见 §2.1） |
| `biz_contract` 23 列 / 9 CHECK / 4 索引 | ✅ 23 列（22 个约束中 9 个 CHECK）/ 4 业务索引 + 主键 |
| 采购单 4 个新列都在 | ✅ `contract_id, contract_no_snapshot, purchase_type, supplier_name_snapshot` |
| 存量采购单 `purchase_type` 全是 SPOT | ✅ 迁移前 15 张全部 SPOT（迁移后新单才有 CONTRACT） |
| 后端编译成功、`/health` 正常 | ✅ `md5=8ad2905f…` 进程与磁盘一致，`profile=prod`，日志「应用 0 个迁移」 |
| 前端 typecheck 0 错 / build 成功 / 线上 entry 与 dist 一致 | ✅ 新 entry `assets/index-C7HCCblS.js` = 线上 entry |
| 真机 10 项全过，≥8 张截图 | ✅ 36 条断言全 PASS，15 张截图（`docs/evidence/deployA-*.png`） |
| 不变量 32/32 | ✅ 32/32，BLOCKER 0，WARN 0 |
| `cargo test` + 前端单测全绿 | ⚠️ 前端 487/487 全绿；后端 lib 1976/1976 + 12 个集成 target 全绿，仅 `tests/testd` 3 条**守卫型**用例红（缺陷已修好导致的报告口径过期，另开卡，见 §7.1） |
| git commit | ✅ `chore(deploy): DEPLOY-A …` |
| 卡里汇报真实输出 | ✅ 即本文档 + 看板评论 |

## 1. 迁移前备份（第 1 步）

```console
$ pg_dump -h 127.0.0.1 -U cloudpivot -d cloudpivot -Fc -f /root/backup-cloudpivot-pre-V0195-20261102-195536.dump
$ ls -lh /root/backup-cloudpivot-pre-V0195-*.dump
-rw-r--r-- 1 root root 1.7M Nov  2 19:55 /root/backup-cloudpivot-pre-V0195-20261102-195536.dump
size_bytes=1683223
==> 校验：能列出归档目录（pg_restore -l 计数）
1375
```

## 2. 逐条迁移（第 2 步）

### 2.1 迁移前的真实状态（与卡面假设有两处出入）

```console
schema_migrations: 行数=136 max_version=192
biz_contract 表=0   采购单合同 4 列=0
业务行数: 采购单=15 入库单=12 菜单=251
服务状态: active pid=3722036
```

- **卡面写「`schema_migrations` 里 0 条记录」不准确**：表里已有 136 条，最大版本 V0192；
  真正没落库的是 **V0193 起**的迁移。
- 因此待应用不是 6 条而是 **8 条**：`V0193 flow_def_is_system`、`V0194 flow_templates_menu`
  也一直没跑（它们属于流程图卡，在 main 里但生产没落地）。**只要重启服务，这两条也会被一起应用**，
  所以本次一并按同一流程逐条落地并验证，这样重启后才是「应用 0 个迁移」的干净状态。

### 2.2 先在「生产库副本」演练一遍

```console
$ createdb -O cloudpivot cloudpivot_dply_reh && pg_restore --no-owner --no-privileges -d cloudpivot_dply_reh <dump>
恢复后基线: schema_migrations 行数=136 max=192 / 采购单 15 / 入库单 12 / 菜单 251
...
===== 汇总: PASS=32 FAIL=0 =====
```

演练库上 8 条迁移逐条应用 + 32 条结构断言全绿后，才碰生产。

### 2.3 V0198 之前的手工存量体检（卡面要求，生产真实行数）

```console
S2-① 非采购入库带供应商|0
S2-② 采购入库无供应商|0
S2-③ 入库单与采购单供应商不一致|0
```

### 2.4 生产逐条应用（每条一个事务，失败即停）

跑法说明：本仓库没有 `migrate` CLI，应用的迁移入口只有「启动时一次性应用」，做不到逐条。
因此用 `psql -v ON_ERROR_STOP=1 --single-transaction -f V0xxx.up.sql` 逐条执行（等价于迁移引擎的
`execute_script` + 独立事务），成功后**按 `vendor/nik-db/src/migrate/{exec,history}.rs` 的口径**补
`schema_migrations` 记录（`kind='versioned'`、`applied_via='imported'`、`checksum=sha256(up 脚本)`、
`applied_seq=max+1`、`source='migrations/business/V0xxx__*.up.sql'`）。工具已入库：
`scripts/verify/deploya_migrate.sh`。

指纹口径先做了对账（拿库中已存在的三条记录与 `sha256sum` 比对）：

```console
MATCH  V190  V0190__erp_year_end.up.sql
MATCH  V191  V0191__pflow3b_finance_cancel_reverse_approval.up.sql
MATCH  V192  V0192__pflow3b_grant_new_codes_by_old_matrix.up.sql
```

生产逐条输出（节选，完整见 `/root/deploya-logs/apply-prod.log`）：

```console
==> [V0193] NOTICE:  V0193 自检通过：is_system 列 + 预置流程分类归一
APPLY_OK V193 checksum=f1401a149c36ed10… seq=143 ms=41 记录数=1
==> [V0194] NOTICE:  V0194 自检通过：流程模板库菜单节点（/flow/templates）
APPLY_OK V194 checksum=6b7ab3ba8c0e7c7a… seq=144 ms=29 记录数=1
==> [V0195] NOTICE:  V0195 自检通过：cols=23 / checks=9 / indexes=5 / CT 取号=1
APPLY_OK V195 checksum=267b3e51cdd48a73… seq=145 ms=45 记录数=1
==> [V0196] APPLY_OK V196 checksum=f5203d9a4ca748b7… seq=146 ms=35 记录数=1
==> [V0197] NOTICE:  V0197 自检通过：5 个 erp:contract:* 权限码 + 合同菜单节点
APPLY_OK V197 checksum=a9c539f8d21fdda9… seq=147 ms=39 记录数=1
==> [V0198] NOTICE:  S2 存量体检：①=0 行；②=0 行；③=0 行（③只报告不修改）
    NOTICE:  S6 自检通过：4 列 / 3 约束 / 1 部分索引；双向钉死违反=0；存量采购单 15 张（CONTRACT=0 / 其余 SPOT）
APPLY_OK V198 checksum=f15729c09aa3645e… seq=148 ms=41 记录数=1
==> [V0199] NOTICE:  S2 存量体检：无源入库单=5 张（其中无备注=4 张，只报告）；采购入库缺供应商=0 张
    NOTICE:  S6 自检通过：1 列 / 2 约束 / 1 部分索引；钉死违反=0；存量入库单 11 张（有源=6 / 无源标记=5）
APPLY_OK V199 checksum=09f4eb65e15f1e8c… seq=149 ms=38 记录数=1
==> [V0200] NOTICE:  V0200 自检通过：合同管理菜单已上侧边栏（根级，sort 75）
APPLY_OK V200 checksum=8cde08a8ce68c627… seq=150 ms=29 记录数=1
```

> V0199 的「无源入库单=5 张」与 `UPDATE 6` 的差异：`UPDATE 6` 含 1 张软删行，`NOTICE` 里的 5 是
> `deleted_at IS NULL` 口径（存量 12 张里 11 张存活）。

## 3. 迁移结果验收（第 3 步，真实 psql 输出）

```console
① biz_contract 列数: 23  / 约束总数: 22  / CHECK 数: 9
   索引: biz_contract_pkey, idx_biz_contract_end, idx_biz_contract_party_b, idx_biz_contract_status, uk_biz_contract_no
② 采购单合同列: contract_id, contract_no_snapshot, purchase_type, supplier_name_snapshot
③ schema_migrations ≥0195: 6 条；清单:
   193:flow_def_is_system 194:flow_templates_menu 195:erp_contract 196:erp_separation_of_duties_config
   197:erp_contract_permissions 198:erp_po_contract_bridge 199:erp_stock_in_sourceless 200:erp_contract_menu_visible
④ 存量采购单 purchase_type 分布: SPOT=15
   业务行数: 采购单=15 入库单=12 菜单=257
```

完整断言（`scripts/verify/deploya_schema_check.sh cloudpivot`，32 条）：

```console
===== 结构验收 @ cloudpivot =====
  PASS  biz_flow_def.is_system 列                     = 1
  PASS  菜单 2000…07a5 存在且可见                     = 1
  PASS  biz_contract 表存在 / 列数 23 / CHECK 9       = 1 / 23 / 9
  PASS  biz_contract 业务索引 4 / 总数 5 / COMMENT 19 = 4 / 5 / 19
  PASS  biz_seq 的 CT 取号规则                        = 1
  PASS  sys_config 内控参数 4 条 / 全 BOOLEAN          = 4 / 4
  PASS  默认值 enabled=false, post=false              = false / false
  PASS  默认值 approve=true, pay=true                 = true / true
  PASS  字典项 erp_internal_control                   = 1
  PASS  erp:contract:* 权限码 5 个 / 菜单 component   = 5 / 1
  PASS  采购单 4 个新列 / 3 个新约束 / idx_biz_po_contract = 4 / 3 / 1
  PASS  存量(迁移前建单) purchase_type 全 SPOT        = 1:SPOT
  PASS  违反 ck_biz_po_contract_pair 行数             = 0
  PASS  is_sourceless 列 / 约束 / 索引                = 1 / 1 / 1
  PASS  无源标记与 source_order_id 一致(违反数)       = 0
  PASS  合同菜单 is_visible=TRUE 且根级               = 1
  PASS  schema_migrations ≥0195 = 6 / (193,194) = 2   = 6 / 2
  PASS  存量采购单/入库单行数（>= 基线）              = OK / OK
===== 汇总: PASS=32 FAIL=0 =====
```

## 4. 后端编译 + 重启（第 4 步）

```console
$ cd backend && CARGO_BUILD_JOBS=1 cargo build --release
    Finished `release` profile [optimized] target(s) in 4m 52s     # exit=0

旧二进制（备份）: /root/cloudpivot-bin-before-deployA-20261102-195619   md5=328376db78a11429191b25c2e3eb137d
新二进制        : /opt/cloudpivot/backend/target/release/cloudpivot     md5=8ad2905f4d3527bd458b8caf714e8624

$ systemctl restart cloudpivot   → /health 2s 内就绪
{"code":0,"msg":"ok","data":{"status":"ok","app":"cloudpivot","profile":"prod",…}}

服务 pid=3963026  进程 exe md5=8ad2905f…  磁盘 md5=8ad2905f…   MATCH: 进程 == 磁盘

journalctl -u cloudpivot:
  数据库迁移完成: 应用 0 个迁移          ← 引擎认可手工记账（checksum 校验通过、无漂移）
  启动引导完成 app=cloudpivot profile=prod db_driver=postgres
  启动守卫通过：缓存后端满足吊销耐久性要求 cache_type=redis
  cloudpivot 启动完成, 耗时 52ms
```

`应用 0 个迁移` 是对第 2 步的关键反证：如果手工补的记录字段/指纹有一处不对，引擎会在启动时
报 checksum 不符或 migration failed，而不是 0。

## 5. 前端构建 + 原子替换（第 5 步）

```console
1) pnpm install --frozen-lockfile   install_exit=0
2) pnpm typecheck                   typecheck_exit=0
3) pnpm exec vite build --outDir dist-deploya    build_exit=0
4) 新包入口: assets/index-C7HCCblS.js（270 个文件 / 14M）
5) 旧 dist 备份: /root/dist-before-deployA-200720（入口 assets/index-Cw3isBDp.js）
6) 原子替换: mv dist dist-old-200720 && mv dist-deploya dist
7) 回读: curl -s http://172.18.10.144/ | grep -o 'assets/index-[^"]*\.js'
   served=assets/index-C7HCCblS.js   MATCH: 线上 entry == 新构建 entry
   入口 chunk HTTP 200 (115195B)；/business/erp/contract 走 SPA fallback 200
```

**为什么不是 `pnpm build` 直接写 `dist/`**：`dist` 就是 nginx 的 root，vite 默认会先清空 outDir，
构建那几分钟线上就是白屏（正是卡面提醒过的坑）。改成「构建到 `dist-deploya` → 两次 `mv` 替换」，
替换窗口是微秒级的 rename。

## 6. 真机验证（第 6 步，36 条断言 + 15 张截图）

脚本 `scripts/verify/deploya_e2e.mjs`（Playwright，打 `http://127.0.0.1/` → nginx → dist → 8023），
日志 `/root/deploya-logs/e2e.log`：

| # | 卡面要求 | 结果 |
|---|---|---|
| ① | 合同管理页能打开、菜单有「合同管理」、列表加载 | ✅ 侧边栏命中；路由 `/business/erp/contract` 未重定向；列表 table 渲染、无 load-error |
| ② | 新建合同（乙方 / 金额 / 日期）→ 保存 → 列表可见 | ✅ 生成 `HT-20261102-000001`（DRAFT），列表 DOM 命中 |
| ③ | 状态流转 DRAFT → 生效，徽标变绿 | ✅ DRAFT→SIGNED（已签订）→ACTIVE（已生效）；接口回读 `status=ACTIVE` |
| ④ | 采购单选「合同采购」→ 合同选择器列出 ACTIVE 合同 → 供应商自动带出且灰掉 | ✅ 选择器只列出 1 个选项即该合同；供应商字段值=「示例供应商（种子）」且带 `readonly`；无手填下拉 |
| ⑤ | 采购单保存 → 列表显示「合同采购」+ 合同号 | ✅ `PO-20261102-000001`（CONTRACT，`contract_no_snapshot=HT-20261102-000001`） |
| ⑥ | 详情：合同号链接 + 审批单 FLW-xxx 链接（两个都在） | ✅ 合同号链接 `HT-20261102-000001`；审批 `审批中 FLW-20261102-94417B93` |
| ⑦ | 生成收货单 → 供应商只读 + 显示采购单号 | ✅ 收货页供应商只读带出；页面显示 `PO-20261102-000001` |
| ⑧ | 收货单保存 → 过账 → 三账都对 | ✅ `IN-20261102-000002` 过账成功（113 元走小额免审分支）；三账见下表 |
| ⑨ | 现货采购单供应商能手填 | ✅ 现货下供应商是可编辑下拉、无合同选择器；`PO-20261102-000002`（SPOT）保存成功 |
| ⑩ | 控制台无报错、无 4xx/5xx | ✅ console error 0（已滤掉告示 SSE）；接口 4xx/5xx 0 |

汇总：`===== 真机验收汇总: PASS=36 FAIL=0 =====`

**⑧ 的三账（psql，生产库）**：

```console
库存（成本账）：M-W1A-001 @ WH01  qty=21.0000  avg_cost=7.238095  amount=152.0000
库存流水      ：IN | PURCHASE_IN | IN-20261102-000002 | qty=10.0000 | amount=100.0000
凭证          ：PZ-20261102-000002 | 2026-11-02 | 状态 POSTED | 3 条分录
                借 100.0000（存货）+ 借 13.0000（进项税额）= 贷 113.0000（应付）← 借贷平衡
勾稽          ：库存合计 420.0000 = 流水净额 420.0000（17 行）
采购单回写    ：PO-20261102-000001 明细 line 1  qty=10.0000  received_qty=10.0000
```

截图（`docs/evidence/`，15 张）：

```
deployA-01-contract-list.png              deployA-09-purchase-list.png
deployA-02-contract-form.png              deployA-10-purchase-detail-links.png
deployA-03-contract-created.png           deployA-11-stockin-from-po.png
deployA-04-contract-status-SIGNED-confirm.png   deployA-12-stockin-post-dialog.png
deployA-05-contract-status-ACTIVE-confirm.png   deployA-13-stockin-after-post.png
deployA-06-contract-active.png            deployA-14-purchase-spot-form.png
deployA-07-purchase-contract-form.png     deployA-15-purchase-spot-saved.png
deployA-08-purchase-form-filled.png
```

## 7. 不变量 + 回归（第 7 步）

```console
# 生产库不变量（144，sourced /etc/cloudpivot/cloudpivot.env）
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。

# 前端单测（144）
$ pnpm test:unit
# tests 487 / # pass 487 / # fail 0 / # skipped 0 / # todo 0

# 后端全量（99，干净库 cloudpivot_dpla：起一次实例迁移到 V0200 后跑）
$ ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_dpla \
  SYS_DB_URL=$ERP_DB_URL NIK_DATABASE_NAME=cloudpivot_dpla NIK_PROFILE=test \
  CARGO_BUILD_JOBS=1 cargo test -- --test-threads=1
...
test result: ok. 1976 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out  ← lib 单测
test result: ok. 62 passed; 0 failed; 0 ignored                               ← tests/linkage（本卡补 is_sourceless 后）
test result: ok. 8 / 4 / 1 / 5 / 6 / 9 / 7 / 12 / 14 / 12 / 1 / 2 passed      ← 其余 12 个集成 target 全绿
test result: FAILED. 64 passed; 3 failed; 0 ignored                            ← tests/testd（3 条「报告口径守卫」，见下）
error: test failed, to rerun pass `--test testd`
```

### 7.1 tests/testd 的 3 条 FAILED：是「缺陷已修好」而不是本卡改坏（带归因）

这 3 条是**守卫型用例**（断言某个缺陷仍然存在），缺陷被修掉之后它们必然变红，需要更新报告口径：

| 用例 | 断言原文 | 归因（git 证据） |
|---|---|---|
| `testd::announcement::g03_delete_announcement_keeps_read_receipts` | `实现已变更：公告下线会软删关联通知，请更新 BUG-D-05 与报告口径` | 断言「通知不随公告下线软删」这个旧缺陷；通知下线改造（FIX-04）先于 A 批次落地，该守卫自此必红 |
| `testd::linkage::k08_mutex_erp_maker_checker_gap` | `ERP 已实现「制单人不得审核」校验（…8 处 SELF_APPROVAL…）：请把本用例改成正向断言并更新 BUG-D-14` | `git log -S SELF_APPROVAL -- backend/src/business/erp/purchase_handler.rs` → `ac1a34d`（P-FLOW-3A），**早于 A1** |
| `testd::storage::zz_no_fixture_residue_after_full_suite` | `cloudpivot_test 存在本卡夹具残留（sys_user=12, sys_role=12, …）` | 其它 testd 用例的夹具没自清理（测试卫生问题），与本卡改动无关 |

判断依据：

- 本卡对 `backend/tests/` 的改动**只有** `tests/linkage/{payable,voucher,stockin}_chain.rs` 各补一行
  `is_sourceless: Some(true)`（见 §8.3），这三个文件与 testd 的三个失败用例无交集；
- 两个守卫的触发条件分别是「公告删除的实现行为」与「源码里是否出现 SELF_APPROVAL」，都是确定性判据，
  与本次部署/迁移无关；
- `tests/linkage` 这个 target 修好后 **62 条全绿**，说明缺口只在 testd 的守卫口径。

已另开卡（`test-enginner`）更新这三条守卫与报告口径，不在本卡里改别人的用例。

## 8. 与卡面的差异 / 需要知道的事

1. **待应用是 8 条不是 6 条**：V0193/V0194 一直没落库（见 §2.1），本次一并应用并验收。
2. **`pnpm test` 这个脚本不存在**：`package.json` 只有 `test:unit`（`tsx --test tests/unit/**`）与
   `test:e2e`（Playwright）。本次跑的是 `pnpm test:unit`（487 全绿）。
3. **`cargo test` 一开始编不过（不是本卡引入的业务代码问题）**：`tests/linkage/{payable,voucher,stockin}_chain.rs`
   三处 `CreateStockInReq` 字面量缺 A4 新增的 `is_sourceless` 字段（E0063）。这三处夹具构造的是
   「PURCHASE + 无来源采购单」→ 按 A4 的规矩就是无源单据，故补 `is_sourceless: Some(true)`
   （`remark` 本来就有）。只动测试夹具，不动业务代码。
4. **`deploya_schema_check.sh` 的「存量全 SPOT」断言按迁移时刻收敛**：直接断言全表会在任何一张
   正常 CONTRACT 采购单诞生后变红，所以改成「`created_at` 早于 V0195 应用时刻的行全 SPOT」。
5. 迁移用 psql 逐条 + 补记账，而非启动自迁移 —— 原因见 §2.4；`应用 0 个迁移` 是等价性证明。

## 9. 回滚手册

```bash
# 数据库（整库回滚，会丢迁移后的新数据）
pg_restore -h 127.0.0.1 -U cloudpivot -d cloudpivot --clean --if-exists /root/backup-cloudpivot-pre-V0195-20261102-195536.dump

# 后端二进制
systemctl stop cloudpivot
cp -f /root/cloudpivot-bin-before-deployA-20261102-195619 /opt/cloudpivot/backend/target/release/cloudpivot.new \
  && mv -f /opt/cloudpivot/backend/target/release/cloudpivot.new /opt/cloudpivot/backend/target/release/cloudpivot
systemctl start cloudpivot

# 前端 dist
mv /opt/cloudpivot/frontend/dist /opt/cloudpivot/frontend/dist-broken && cp -a /root/dist-before-deployA-200720 /opt/cloudpivot/frontend/dist
```

## 10. 本次在生产创建的业务数据（可保留作证据，也可清理）

| 单据 | 号 | 状态 |
|---|---|---|
| 合同 | `HT-20261102-000001`（DEPLOYA-20261102120934-框架采购合同，乙方 示例供应商） | ACTIVE |
| 采购单（合同采购） | `PO-20261102-000001` | RECEIVED（审批实例 FLW-20261102-94417B93 审批中） |
| 收货单 | `IN-20261102-000002` | POSTED（库存 +10 @ WH01，凭证 PZ-20261102-000002） |
| 采购单（现货） | `PO-20261102-000002` | DRAFT |

> 过账已经产生库存/流水/凭证（真机验证第 ⑧ 项必须过账）。若需清理，走作废/红冲流程而不是直删；
> 直删会破坏不变量 D1/E4 的勾稽。
