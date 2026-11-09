# E2E-FIX-3 销售退货单权限码零授予（P2 内控红线）—— 证据与口径

- 卡：`t_c7038fa7`（E2E-FIX-3），来源 E2E-02 销售域演练问题清单第 4 条
- 结论：**已修**（迁移 `V0356`）+ **已上常驻闸**（`scripts/verify/core_perm_grants.py` 与真库用例 `N20`）
- 环境：99 工作仓 + 145 隔离库 `cp_e2efix3` / 隔离端口 `18319`（**不碰 144 生产**）

---

## 一、问题（现象 → 根因 → 后果）

```
business:erp:sales-return:{create,list,status}
  sys_menu 里有承载行 ✔（V0304 种的 1 页面 + 2 按钮）
  后端有 #[nik::permission] 注解 ✔
  sys_role_menu 里一行授权都没有 ✘

⇒ 除超管（`*:*:*` 短路）外**所有人 403**
⇒ 退货单的「审批」只能超管自己批（自建自批）→ 职责分离形同虚设
⇒ `perm_menu_gate.py` 挡不住：它比的是「注解码 − 菜单 auth_code」，两边都有，闸是绿的
```

根因不是「忘了授」，而是 V0304 / V0295 的迁移头注释写着「本迁移**不写** `sys_role_menu`：
授给哪些角色是管理决策」——授权面留白，而**没有人回头补**。留白本身没错，缺的是一道
「留白是不是变成了死锁」的检查。

---

## 二、同类扫描（卡面 ②）——零授予清单

口径：`sys_menu`（未软删）里所有 `{create,list,status}` 权限码 × **非超管角色**的授予情况；
同一码挂在页面行与按钮行时按 `auth_code` 聚合（任一节点被持有即算有授权）。

| 文件 | 内容 |
|---|---|
| `zero_grant_scan_before.txt` | 库 `cp_e2efix1`（全新库 355 条迁移，**V0356 未应用** = 演练当天状态）：**25 个码零授予** |
| `zero_grant_scan_after.txt` | 库 `cp_e2efix3`（同上 + V0356）：**剩 18 个**，全部是 ERP 之外的平台治理/配置面 |

**ERP 域（本卡修）——7 个码，三组**：

| 权限码 | 影响 | 本卡处置 |
|---|---|---|
| `business:erp:sales-return:{create,list,status}` | 销售退货单整页不可用，「审批」只能超管自批 | 授 `SALES_CLERK`(create,list) + `SALES_SUPERVISOR`(list,status) |
| `business:erp:receipt-refund:{list,detail,create,allocate,status}` | 收款退款单（销售链「冲收款」唯一入口）财务一条都用不了 | 授 `FINANCE_MANAGER` 全 5 码 |
| `business:erp:unit:create` | 计量单位只能超管建 ⇒ 上不了新单位就上不了新物料 | 授 `PURCHASE_MANAGER`（该角色已有 `unit:list`） |

**ERP 之外的 18 个零授予码（本卡不判红，只报告）**：
`business:flow:{create,linkage:list}`、`business:impexp:{def:list,export:create,export:list,import:list}`、
`business:storage:{config:create,config:list,policy:list}`、
`system:{change-log:list,field-mask:create,field-mask:list,help:category:create,help:category:list,help:doc:create,help:doc:list,role-mutex:create,role-mutex:list}`。
这些都是**平台治理/配置面**（存储配置、字段脱敏、角色互斥、帮助中心、导入导出定义、流程配置），
「只有超管能动」是站得住的口径；把它们判红只会逼人往白名单里塞东西。
常驻闸对它们**打印但不判红**，避免「静默」本身成为下一个死锁的藏身处。

---

## 三、修复（迁移 `V0356__doc_core_perm_grants`）

10 对授权，矩阵与理由写在迁移头注释里：

| 角色 | 权限码 | 依据 |
|---|---|---|
| `SALES_CLERK` | `sales-return:list`, `sales-return:create` | 销售员建单 |
| `SALES_SUPERVISOR` | `sales-return:list`, `sales-return:status` | 审批侧（提交/批准/待过账/作废） |
| `FINANCE_MANAGER` | `receipt-refund:{list,detail,create,allocate,status}` | 退款登记 → 核销 → 过账 |
| `PURCHASE_MANAGER` | `unit:create` | 计量单位维护 |

**职责分离的口径（本卡核心）**：`sales-return:status` 是一个**粗码**，一把盖住
`DRAFT→SUBMITTED`（提交）/ `SUBMITTED→APPROVED`（批准）/ `PENDING_POST→POSTED`（过账）；
`sales_return_handler::change_return_status` 只做权限码一道闸、**没有按目标态分角色鉴权**。
所以给建单人 `status` 就等于「自己建、自己批」——SoD 只能在授权层切：**CLERK 不给 status**，
迁移末尾用断言把这条钉死（施工后 `SALES_CLERK` 若持有 `sales-return:status` → 迁移失败）。

代价（写清楚，不藏）：销售员建完草稿**不能自己提交**，由主管提交/批准/过账。
要拆成 `:submit` / `:approve` 得新增权限码 + handler 按目标态校验 + 前端按钮同步，
属后续卡（本迁移不擅自造码，AGENTS.md 约定 7）。

**为什么 `SALES_SUPERVISOR` 必须同时拿 `:list`**：`status` 按钮（menu_type=3）父节点是页面节点，
父节点 `auth_code` 就是 `:list`。只授按钮不授父页 ⇒ 权限树上按钮不可达（`orphan_button_gate` 判孤儿）、
侧边栏整棵子树对非超管不可见。`receipt-refund` 的 4 个按钮同理。

**幂等/回滚（实测）**：

```
up    → 新增授权 10 行
up 重放 → 新增授权 0 行，token_version 不推进（不踢在线会话）
down  → 撤销 10 行（其它迁移授的同码行 id 不同，一行不碰）
up    → 再来 10 行
```

---

## 四、常驻闸（卡面 ③）

1. `scripts/verify/core_perm_grants.py`（新增）
   - 判据：`business:erp:*:{create,list,status}` 每个码必须至少授予 **1 个非超管角色**；
   - 超管排除（`*:*:*` 短路，一行超管授权等于没授）；
   - 两档：ERP 域判红（rc=1）；ERP 之外只报告（INFO）；
   - `--self-test` 5 组红/绿语料（零授予必报、超管不算、白名单必须带理由、正则口径防前缀伪装）；
   - 基数下限 60（低于它=连错库/迁移没跑，rc=2，不假装绿）；`--json` / `--menu-grants-file` / `--dump`。
2. `backend/src/system/menu/permission_audit_tests.rs::n20_erp_core_permission_codes_are_granted_to_non_super_role`
   （真库用例，`SYS_DB_URL` 门控；与 N14 同一套「种子行」收窄，并发夹具不算）+ 文件头表格登记。
3. `scripts/verify/run_all.sh` ⑤ 不变量阶段新增一条 stage（只读，rc=1 报警，不自动改授权）。

**闸的自证（证明有牙）**：

| 场景 | 结果 |
|---|---|
| 库 `cp_e2efix1`（修复前，演练当天状态） | `rc=1`，点名 7 个零授予码（sales-return ×3 / receipt-refund ×3 / unit:create） |
| 库 `cp_e2efix3`（修复后） | `rc=0`，98/98 个 ERP 核心码都有非超管授权 |
| 人工删掉 `SALES_SUPERVISOR` 的 `sales-return:status` | `rc=1`，点名该码；重跑 up 恢复 → `rc=0` |
| `--self-test` | 全部 PASS |

---

## 五、145 实测（真实 token curl）

脚本：`live-145.sh`（隔离库 `cp_e2efix3` + 隔离端口 `18319`，跑完即停实例，**不碰 :8023 / 144**）
输出：`live-145.txt`

覆盖：

| 验收 | 做法 | 期望 |
|---|---|---|
| ① 销售主管能 PATCH 退货单状态 | `e2e02_appr`（SALES_SUPERVISOR+财务+总经理+仓管+财务总监）`PATCH /business/erp/sales-return/{id}/status` | 修复前 403 → 修复后 200 |
| ② 销售员能建单/查列表 | `e2e02_sp`（SALES_CLERK）`POST /business/erp/sales-return` + `GET .../page` + `GET .../{id}` | 修复前 403 → 修复后 200/201 |
| ③ 零授予清单完整 | 脚本 phase A 把 V0356 `down` 掉复现现场，闸输出 7 个码 | 与本 README 第二节一致 |
| ④ 不变量闸能发现缺失 | phase A 闸 `rc=1`；phase B 闸 `rc=0` | 一致 |
| ⑤ push | 见卡内汇报 commit hash | `origin/main` 已含 |

额外断言：
- **职责分离**：`e2e02_sp`（建单人）PATCH 状态 → **403**（没有 `status` 码，自建自批被堵死）；
- 收款退款单：`FINANCE_MANAGER`（`e2e02_appr`）`GET /business/erp/receipt-refund/page` → 200（原 403）；
- 计量单位：`PURCHASE_MANAGER`（`e2efix3_buyer`）`POST /business/erp/unit` → 200（原 403）。

最终一轮实测关键行（`live-145.txt` 逐字）：

```
本迁移授权行：10（期望 10）                         ← 迁移随实例启动自动应用
修复前：SALES_CLERK 查列表 403 / 销售主管 PATCH 403   ← 缺陷复现
修复前：core_perm_grants.py rc=1（点名 7 码）
V0356 完成：…本次新增授权 10 行，推进 token_version 3 人
销售员建单 http=200 return_no=SR-20261109-000005
销售员查列表 200（total=5）/ 详情 200
销售主管 PATCH 状态 → 200（status=CANCELLED）        ← 验收 ①
销售员 PATCH 状态 403（自建自批被堵死）               ← 职责分离
FINANCE_MANAGER 收款退款单列表 200 / PURCHASE_MANAGER 建计量单位 200
修复后：core_perm_grants.py rc=0（ERP 核心码 98/98）
汇总：PASS=17 FAIL=0
```

---

## 六、代码/测试（实测输出见 `verification.txt`）

| 项 | 结果 |
|---|---|
| `cargo test --lib system::menu::permission_audit_tests`（SYS_DB_URL=cp_e2efix3） | **7 passed / 0 failed**（N14–N20；N20 为本卡新增） |
| N20 打「修复前」库 cp_e2efix1 | **FAILED**，点名 7 个零授予码（证明用例有牙，不是永远绿） |
| `python3 scripts/verify/core_perm_grants.py --self-test` | **PASS**（5 组红/绿语料） |
| 静态闸打 cp_e2efix1 / cp_e2efix3 | `rc=1`（7 码）/ `rc=0`（98/98） |
| 迁移 down→up→up 重放 | 撤 10 行 → 补 10 行 → 重放新增 **0 行**（幂等），闸 rc 1→0 |
| 前端 `pnpm typecheck && pnpm build` | 通过（exit 0；本卡无前端改动，跑一遍证明未破） |
| 全量 `cargo test` | **未跑全量**：单线程约 90min 超出本卡 2h 预算（与上一张卡同一口径）；改动面 = 迁移 + 一个新用例，定向用例与静态闸已覆盖 |

顺手修掉 main 上一条**既有红**：`GET /business/erp/master-data/code-policy`（CODE-08 落地时
漏登记 `NO_PERMISSION_ROUTES`）让 N16 一直红；本卡登记白名单（带理由）后 N16 转绿。

---

## 七、遗留与后续（不藏问题）

1. **粗粒度 `status` 码**：`sales-return:status` 同时包含提交/批准/过账，SoD 只能在授权层切。
   拆成 `:submit` / `:approve` / `:post` 才能表达「主管提交、经理批准、财务过账」这类三段权限，
   属独立卡（要动 handler 鉴权 + 菜单 + 前端按钮 + 迁移）。
2. **`SALES_CLERK` 的角色矩阵不完整**（**不是零授予，是覆盖面**）：实测
   `GET /business/erp/material/page` 与 `/warehouse/page` 对销售员都是 403 ⇒ 退货页前端
   `Promise.allSettled` 吞掉 403，页面能开但物料/仓库下拉为空、明细只显示 id。
   同一个洞在销售订单页也存在（先于本卡），要改角色矩阵，需管理决策，不在本卡范围。
3. **ERP 之外的 18 个零授予码**：当前口径是「平台治理/配置面只给超管」。若日后要让
   `FLOW_CONFIG_APPROVER` 之类角色进流程配置页，得另开卡补授权，别直接加白名单。
