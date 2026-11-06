# SO-25 销售菜单种子 + 角色授权（V0267）— 验收证据

卡：`t_30f9d01b`　｜　施工时间：2026-11-06 23:5x CST
隔离库：`cloudpivot_so25`（从 `cloudpivot_so24` 逻辑复制：**225 条迁移、不含 sales_menu**）
后端：`backend/target/debug/cloudpivot`（sha256 `5a608eaa8a7333b8…`）@ `127.0.0.1:18225`
（迁移是**运行时**从 `backend/migrations` 目录读的 —— alun-db `migrate/loader.rs` 走 `read_dir`，
所以新增迁移文件无需重编译二进制）

---

## 一、改了什么

| 文件 | 状态 | 内容 |
|---|---|---|
| `backend/migrations/business/V0267__sales_menu.up.sql` | 新增 | ① 目录「销售管理」（`menu_type=1`，`path=/business/erp/sales-mgmt`，无 auth_code，sort=64）② 六个销售页 reparent 到该目录（10/20/30/40/50/60）③ 角色 `SALES_CLERK`（销售员）④ 五角色 × 47 对授权（含无码祖先目录闭包）⑤ 6 组自检 |
| `backend/migrations/business/V0267__sales_menu.down.sql` | 新增 | 按确定性 id 公式精确删授权 → 页面还原 `parent_id=NULL` + `sort_order=76…85` → 删目录 → `SALES_CLERK` 仅无绑定时删 |
| `scripts/verify/so25_menu_apply.sh` | 新增 | 复制隔离库 + 起后端（真跑迁移）+ 断言 `schema_migrations` 记账 |
| `scripts/verify/so25_menu_verify.sh` | 新增 | 真 token curl 验收：菜单树 / 5 角色菜单子集 / 有权 200 + 越权 403 / 前端路由比对 |
| `scripts/verify/so25_migration_roundtrip.sh` | 新增 | up 重放幂等 + down/up 往返 |
| `docs/evidence/20261106-so25-sales-menu/**` | 新增 | 本目录证据 |

**前端代码零改动**：六个页面路由（`/business/erp/{customer,sales-order,shipment,receivable,receipt,invoice}`）
在 SO-03/07/11/15/18/21 落地时已全部注册进 `router/index.ts`（见 `round-*/`… 与验收脚本第 5 段逐条 grep 断言），
本卡只做「把六个页面收进目录 + 授权」，`path` 一个字符都没动 ⇒ `system::menu::tests::n08` 不受影响。
`pnpm typecheck` / `pnpm build` 仍全绿（`frontend-typecheck.txt` / `frontend-build.txt`）。

---

## 二、验收结果（真输出，非描述）

### 1) 迁移 APPLY_OK
`round-1-apply.txt` / `round-2-apply.txt`：

```
✅ 库 cloudpivot_so25 已从 cloudpivot_so24 逻辑复制（225 条迁移）
health=200 pid=…
--- 迁移记账（最近 6 条）---
301|sales_order_flow_is_system_and_snapshot|versioned|executed|t
267|sales_menu|versioned|executed|t
…
--- applied_via 分布（必须只有 executed/imported）---
executed|227
```

- `kind=versioned`、`applied_via=executed`、`success=true`（卡面要求 4：只允许 executed/imported，实测非该两类 = 0）
- 225 → 227（V0267 + 邻居卡的 V0301 一起被应用；V0267 是本次首装）
- 启动日志有一条**预期内**的乱序告警（237 之后的号段本来就是给后续卡预留的缺口）：
  `WARN 待应用迁移的版本低于已应用最大值（乱序补迁）… migration=sales_menu version=267 latest_applied=297`
  —— alun-db 只告警不阻断（`migrate/mod.rs` 里 pending 照常入列），V0267 正常执行。

### 2) 两轮（每轮重建库）结果一致
`round-1-console.txt` / `round-2-console.txt`：**PASS=60 FAIL=0**（两轮同）
两轮 `raw/` 响应体（各 23 份）归一化（UUID/时间戳抹平）后 **DIFF-EMPTY**。

### 3) 用 laoliu 账号看到销售菜单
`round-1/menus_sup.json`（laoliu = `SALES_SUPERVISOR` 销售经理）：
「销售管理」子页 = `客户档案 · 销售订单 · 发货单 · 应收台账`（4 个）
**不是 6 个，这是对的** —— ARCH-7 §10.2 里销售经理的「收款 / 发票」两列是「—」。

### 4) 各角色权限正确（菜单子集 + 接口 200/403 双向）
| 角色 | 菜单里看得到的销售页 | 销售码数 |
|---|---|---|
| `SALES_SUPERVISOR`（销售经理，复用 V0281 角色） | 客户档案 · 销售订单 · 发货单 · 应收台账 | 12 |
| `SALES_CLERK`（销售员，本卡新建） | 客户档案 · 销售订单 | 4 |
| `WAREHOUSE_MANAGER` | 销售订单 · 发货单 | 4 |
| `FINANCE_MANAGER` | 全部 6 | 17 |
| `GENERAL_MANAGER` | 全部 6 | 10 |

接口侧（`round-*/perm_*.json`，12 条全是真 HTTP）：
`laoliu GET customer/page 200` / `laoliu GET receivable/aging 200` / **`laoliu GET receipt/page 403`** /
`clerk GET sales-order/page 200` / **`clerk GET receivable/page 403`** /
`wh GET shipment/page 200` / **`wh POST customer 403`** /
`fin GET invoice/page 200` / `fin GET shipment/page 200` /
`gm GET receivable/page 200` / **`gm POST customer 403`**

### 5) 三道闸全绿
- 闸⑧ `role_code_gate.py`：**EXIT=0**，白名单 14 个角色（种子自动并入 `SALES_CLERK`、`SALES_SUPERVISOR`），
  仅 2 条**策略 WARN**（新迁移里出现 `INSERT INTO sys_role_menu`，提示「授权优先走 V0258 矩阵」）→ WARN 不改退出码。见 `gate-role-code.txt`。
- 闸⑥ `perm_menu_gate.py`：`missing_in_menu = 0`（303 个注解码全部有菜单承载），`unused_in_code = 12` 只报告。见 `gate-perm-orphan.txt`。
- 闸⑦ `orphan_button_gate.py`：`孤儿 (角色,码) = 0`。见 `gate-perm-orphan.txt`。
- 迁移回归 `migration-roundtrip.txt`：up 重放 **本次新增授权 0 行**；down 删 52 行 + 六页面 `sort:parent` 全还原
  `76:NULL,…,85:NULL` + 目录删除；再 up 后计数与基线一致（`52/6/1/1`）→ **PASS=4 FAIL=0**。

---

## 三、与卡面不一致的地方（都给了理由，不是漏做）

1. **卡面列的 9 个码后端不存在，本迁移没有为它们建菜单行**：
   `customer:detail`、`sale-order:delete`、`sale-order:approve`、`shipment:update`、`shipment:delete`、
   `receipt:update`、`receipt:delete`、`invoice:update`、以及 `sales-order:*`（真实资源名是 **`sale-order`**，单数 sale，V0265 已落地）。
   依据：`grep -oE 'permission = "[^"]+"' backend/src/business/erp/*.rs` 的实测集合；
   给无注解码建行 = 权限树里放哑弹（AGENTS.md 约定 7 + `perm_menu_gate` 的 `unused_in_code` 档）。
   `sale-order:approve` 的语义由流程引擎承担（`business:flow:approve` + `so_approve` 节点审批人角色，V0258 已授 FM/GM）。
2. **`SALES_MANAGER` 没有新建**：直接复用 V0281 为 `so_approve` 种下的 `SALES_SUPERVISOR`（销售主管），
   避免「两个销售管理角色、审批流只认其中一个」的裂缝。`SALES_CLERK` 本仓确实没有，按闸的合法路径在同文件
   `INSERT INTO sys_role … VALUES` 新建（闸会自动并入白名单）。
3. **页标题保持原样**（`应收台账` 而非卡面的 `应收账款`）：那是 SO-15 的既有决定，本卡不动显示名。
4. **`账龄分析` 仍是「应收账款」页下的一个按钮码**（前端是页内双 Tab），没有单独建子页。

---

## 四、踩坑（给下一张卡）

1. **`Authorization: Bearer $token` + `$(login …)` 会 400 空响应**：`login()` 若把「✅ 登录成功」打到 stdout，
   会被 `$( )` 一起塞进 token 变量 ⇒ 头里带换行 ⇒ hyper 直接 400 且响应体为空（不是 401，非常容易误判成「后端挂了」）。
   本卡脚本改成 `login` 只写全局 `TOKEN`，调用处 `T_X=$TOKEN`。
2. `read_file`/`cat` 显示 `Bearer ***` 是**终端渲染脱敏**，文件里其实是 `$token` —— 别据此「修」脚本。
   so24_e2e.sh 里的 `Bearer ***` 需要单独核对（见卡内评论）。
3. **模板库带旧版 V0267 记账 ⇒ 启动即 checksum 失败**：别的验证实例在文件演进期自动套过一次 V0267，
   复制出来的库会报 `migration version 267 was modified after it was applied`。`so25_menu_apply.sh` 现在会前置断言源库无 `sales_menu`。
4. `CREATE DATABASE … TEMPLATE cloudpivot_so23fresh` 会因模板库有别的实例连接而失败（"is being accessed by other users"）；
   用 `pg_dump | psql` 逻辑复制，别去 kill 别人的会话。
5. `pnpm build` 默认 Node 堆会 **OOM（exit 134 / core dumped）**；本机 15G 内存，`NODE_OPTIONS=--max-old-space-size=6144` 可过。
6. `role_code_gate.py::parse_seeded_roles` 只认 `INSERT INTO sys_role … VALUES …`；
   写成 `INSERT … SELECT … WHERE NOT EXISTS` 的话，同一文件里的授权段会被判「未知角色码」而闸红。
