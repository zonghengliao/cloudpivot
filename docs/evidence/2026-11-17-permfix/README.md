# FIX-权限闸：M1-23 账龄页面码死码 + 4 对孤儿按钮（卡 t_fa41e0e1）

> 一句话：本卡收口 M2-04 施工时实测到的两道权限闸既有红。**N14（死权限码）已由 M2-07 转绿**，
> 本卡补上余下两件 —— **孤儿按钮 4 对** 与 **N20 六码零授予**（体检时新发现的、同属「权限闸」家族的红），
> 交付物是**一条迁移 V0442** + 一个真库演练脚本，零菜单新增、零新权限码、零 Rust 改动。

- 施工时间：2026-11-17
- 提交：见卡内汇报（`backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.{up,down}.sql`）
- 演练脚本：`scripts/verify/perm_gate_fix_drill.sh`（自建库、自 DROP，库名 `cp_permfix_drill_<pid>`，只建在 145）

---

## 一、开工前的实测基线（真库，不是推测）

克隆模板库 `cloudpivot_test`（停在 V0404）→ 补齐 V0405…V0441 → 跑四道门禁：

| 闸 | 基线读数 | 结论 |
|:--|:--|:--|
| `perm_menu_gate.py`（死码闸，判「注解码能否授出去」） | `missing_in_menu = 0` | ✅ 已绿 —— **缺陷 1（N14）已被 M2-07 修掉**（见 §三） |
| `orphan_button_gate.py`（孤儿按钮） | `孤儿 (角色,码) = 4` | ❌ 复现，见 §二 |
| `permission_audit_tests`（8 条真库用例） | **7 passed / 1 failed**（N20） | ❌ 新红：6 个报价/返利核心码零授予，见 §四 |
| `menu_invariants.sh`（8 条结构不变量） | 全绿（活跃菜单 540 行） | ✅ |

原始输出：[`01_orphan_before.out`](01_orphan_before.out)、[`02_permission_audit_before.out`](02_permission_audit_before.out)、
[`03_perm_menu_gate.out`](03_perm_menu_gate.out)、[`04_menu_invariants.out`](04_menu_invariants.out)

（`permission_audit_tests` 读 **`SYS_DB_URL`**，不设就是 SKIP 假绿 —— 本次两次运行 `grep -c '^SKIP'` 都是 **0**。）

---

## 二、缺陷 2 · 孤儿按钮 4 对：根因与修法

闸的判据：对每个（角色,码），把该角色被授的**按钮节点**逐层向上走祖先链，链上只要有一个
「带码祖先」的码没授给该角色 ⇒ 该节点不可达；若该码的**全部**按钮节点都不可达 ⇒ 判红。

| # | 角色 | 码 | 不可达父页 | 根因 | 本卡修法 |
|:--:|:--|:--|:--|:--|:--|
| 1 | `DEV_MONITOR_VIEWER` | `business:notice:mine` | `/business/notice/template`（`business:notice:list`） | 按钮 …414「我的通知 / 未读数 / 已读」错挂在「消息通知」模板管理页下；它真正所属的「消息中心」页（…0010）挂的**就是** `business:notice:mine` | **父页归位**（…414 → …0010），零授权变化 |
| 2 | `TEST_UADMIN` | `business:notice:mine` | 同上 | 同上 | 同上 |
| 3 | `FINANCE_DIRECTOR` | `business:erp:receivable:aging` | `/business/erp/receivable`（`business:erp:receivable:list`） | V0418（M1-23）给财务总监授了「应收账龄分析」报表页 + 账龄取数码，却没给承载该取数码的「应收台账」页 ⇒ 调得动接口、看不见挂载点 | **补授父页码**（数据驱动：凡持有 `receivable:aging` 的角色都补 `receivable:list`） |
| 4 | `TEST_UADMIN` | `system:role-mutex:conflict` | `/system/role-mutex`（`system:role-mutex:list`） | 装配库遗留角色持有「冲突用户预检」按钮，但没持父页；角色互斥只有一个页面，按钮没有第二个可归位的父页 | **补授父页码**（数据驱动：凡持有 `role-mutex:conflict` 的角色都补 `role-mutex:list`） |

**为什么第 1/2 对不选「补授 `notice:list`」**：那是「消息通知」模板管理页（含新建/修改/删除模板三个按钮），
送给只读监控角色明显过头。归位更干净：`business:notice:mine` 的按钮与页面共用同一个码，归位后
祖先链上的带码祖先就是它自己 ⇒ 7 个持有者**一律**可达（自检 b 用结构断言钉死，不写角色名单）。

**为什么第 3 对不选「把按钮挪到新报表页」**：取数码 …2882 是「应收台账」页里的账龄看板 tab
（V0418 头注释的既有口径），台账页就是它的家；挪过去会让**只持有台账页**的角色
（`SALES_SUPERVISOR`）变成新孤儿 —— 等于把缺陷挪个位置。

**祖先闭包**：`build_menu_tree` 用 `utils::build_tree`，父节点不在授权集合里的子节点会被**静默丢弃**。
实测装配库里「系统管理」（…0001）补授前**授权行 0** —— `TEST_UADMIN` 早就持有的「用户管理」页
（`system:user:list`）因此一直没在侧栏出现过。所以补授页面码时必须连带补父目录（本迁移 ④ 段，通用写法）。

**`TEST_UADMIN` 是否该进装配数据**：**不该，但本卡不删**。它是装配库里的遗留角色
（`docs/architecture/P2-新装配库权限矩阵-决策.md` §2.2 已列明「生产不存在的历史角色码」，生产 12 个角色里没有它），
装配库有 1 个账号绑着它、它持有 87 个权限码。删角色会连带账号、越出「修闸」范围 ⇒ 本卡只按闸的建议
补授父页码，**是否从装配数据下线建议另开卡**。正因为不想把非生产角色码写进迁移（`role_code_gate`
会判红：写不存在的角色码 = 授权静默 0 行），第 2/3/4 对的补授全部做成**数据驱动**（「谁持有按钮码就给谁补父页码」），
迁移里因此**没有任何非生产角色字面量**。

---

## 三、缺陷 1 · N14 死权限码：已在 M2-07 转绿（本卡只复核）

`business:erp:report:receivable-aging`（V0418 的页面显形码，后端零引用）在 M2-07（`t_10d54ac1`，提交 `51afe645`）
被登记进 `permission_audit_tests::ROUTE_LEVEL_ONLY` 例外台账（附理由：页面码只给前端路由，取数走
`business:erp:receivable:aging`）。本卡在真库上复核：`n14 ... ok`、`n18`（台账活性）`ok`、
`perm_menu_gate` `missing_in_menu = 0` ⇒ **无需再动手**。

---

## 四、体检时新发现 · N20 六码零授予（同属「权限闸」家族，本卡一并收口）

`permission_audit_tests::n20` 的判据：`business:erp:<资源>:(create|list|status)` 形态的核心码
**必须至少授予 1 个非超管角色**（超管 `*:*:*` 短路，授了等于没授）。基线上它红，点名的正是 M3 三卡的码：

```
business:erp:quotation:{create,list,status}   ← V0433（M3-02 报价单）
business:erp:rebate-policy:{create,list}      ← V0438（M3-15 返利政策）
business:erp:rebate-settlement:list           ← V0439（M3-16 返利结算）
```

三条迁移都**刻意不写** `sys_role_menu`（原文：「授给哪些角色是管理决策，不静默扩权」），与 N20 直接冲突。
冲突总得有人收口 —— 本卡只补 N20 点名的 **6 个码**（update / delete / convert 一概不动），口径：

| 码 | 角色 | 依据 |
|:--|:--|:--|
| `quotation:list` | `SALES_CLERK` / `SALES_SUPERVISOR` / `FINANCE_MANAGER` / `GENERAL_MANAGER` | 同型单据销售订单（V0265）的 `sale-order:list` 给了 5 个角色；报价单是成交前的单 ⇒ 去掉仓库。`FINANCE_MANAGER` 是 V0435 审批流「财务经理复核」节点的指派人（看不见单就审不了） |
| `quotation:create` | `SALES_CLERK` / `SALES_SUPERVISOR` | 与 `sale-order:create` 完全同款 |
| `quotation:status` | `SALES_CLERK` / `SALES_SUPERVISOR` | 与 `sale-order:status` 同款。**审批动作不靠这个码**：审批走 `business:flow:approve`（V0435 绑 `SALES_SUPERVISOR` + `FINANCE_MANAGER`），`SALES_CLERK` 一个 approve 都没有（迁移自检 + 演练各钉一次）⇒ 建单与审批分属不同角色 |
| `rebate-policy:list` | `SALES_SUPERVISOR` / `SALES_CLERK` / `FINANCE_MANAGER` | V0438 头注释原话「销售经理建政策 / 销售员只读」；财务要算返利，只读政策是前提 |
| `rebate-policy:create` | `SALES_SUPERVISOR` | 政策由销售经理维护 |
| `rebate-settlement:list` | `SALES_SUPERVISOR` / `FINANCE_MANAGER` | 结算单 = 期间净销售 × 命中档算出的应返金额，销售经理与财务经理都要看；**不给** `SALES_CLERK`（返利金额不该下到业务员） |

这 6 个码 0 行授权时，除超管外**全体 403**：报价单建不了、审批人看不见单、返利政策没人能建
—— 不是「少配几个勾」，是功能上线即死锁（N20 头注释记的 E2E-02 现场同类）。

---

## 五、验收实测（全部真库 / 真进程）

| 项 | 命令 | 结果 |
|:--|:--|:--|
| L0 静态闸 | `bash scripts/check.sh --l0` | **全部检查通过（L0）**（含迁移版本号全局唯一、闸⑧ 迁移授权段角色码、库存写入口 22/22、测试分域登记表） |
| fmt / clippy | `cargo fmt --check`、`cargo clippy -p cloudpivot --lib -- -D warnings` | 0 diff / 0 告警 |
| 孤儿闸 | `python3 scripts/verify/orphan_button_gate.py` | **孤儿 = 0**，退出码 0（[`05_orphan_after.out`](05_orphan_after.out)） |
| 死码闸 | `python3 scripts/verify/perm_menu_gate.py` | **PASS**，`missing_in_menu = 0` |
| 权限审计 8 条 | `SYS_DB_URL=… cargo test --lib system::menu::permission_audit_tests -- --test-threads=1` | **8 passed / 0 failed**，`SKIP = 0`（[`06_permission_audit_after.out`](06_permission_audit_after.out)） |
| 菜单结构 8 条 | `bash scripts/verify/menu_invariants.sh cp_permfix_drill_<pid>` | **全绿**，菜单总数 540 **未涨**（本迁移不新增菜单行） |
| 幂等 | `up` 连跑两次 | 第二次 `UPDATE 0 / INSERT 0 / INSERT 0`，`sys_role_menu` 总行数不变 |
| 回滚 | `down` → `down` 再跑 → `up` 恢复 | down 后回到基线（孤儿 **4**、授权行清零、…414 回到「消息通知」页 sort 40）；down 重放 0 行；再 up 恢复（孤儿 **0**） |
| 整体演练 | `bash scripts/verify/perm_gate_fix_drill.sh` | **PASS=47 FAIL=0 / ALL PASS**（[`00_drill.out`](00_drill.out)） |

迁移形状（基线上）：**授权行 17**（N20 矩阵 14 + 账龄/互斥两对补授 3）+ **祖先闭包行 4**，
零新增 `sys_menu` 行（540 → 540）。

---

## 六、回滚

```bash
psql "$DSN" -f backend/app/migrations/business/V0442__perm_gate_orphans_and_n20.down.sql
```

down 按**确定性 id**（`v0442.<role_id>:<menu_id>` / `v0442c.<role_id>:<dir_id>`）精确删除本迁移插入的行，
角色侧不按名单枚举（数据驱动的两段也能完整回滚），并把 …414 放回「消息通知」页（sort 40）。
回滚**不是关功能**：菜单行、权限码、接口全部还在；变的是「谁能勾到它」——
4 对孤儿与 N20 六红回到迁移前形态。

---

## 七、只报不改的两条发现

1. **`系统管理`（…0001）目录在装配库里零角色持有** —— 所有非超管角色的 `/system/*` 页面
   （含 `TEST_UADMIN` 独占的 `system:user:list`）在侧栏都渲染不出来（`build_tree` 丢弃孤儿子树）。
   本迁移给 `TEST_UADMIN` 补了该目录，但这只是「谁持有页面行谁补」的数据驱动自愈，
   **是否让非超管看见系统管理子树是个产品决定**，需要单开卡。
2. **`TEST_UADMIN` 这类「生产不存在的历史角色码」仍留在装配数据里** —— 它在装配库里持 87 个码、
   有 1 个账号绑定；每个引用它的老迁移都只能靠 `role_code_gate_baseline.json` 冻结豁免。
   建议单开卡决定「装配库是否清退这批角色」，否则闸会持续把它们算进孤儿/矩阵统计。
