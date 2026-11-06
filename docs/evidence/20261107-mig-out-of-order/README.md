# 【MIG-OUT-OF-ORDER】空库重放迁移在 V0267 必败 —— 修复与证据

> 卡：`t_d403a60a`｜父卡：`t_524de96e`（SO-26）｜日期：2026-11-07
> 结论：**空库按版本号重放整条迁移链已修好**（219/219 通过 → 起实例「应用 229 个迁移」→
> `/health` 200），修法是**新增一条号段更小的前置迁移 `V0266`**，
> **没有改动任何已应用的迁移文件**（生产 / 144 / 各卡副本的 `checksum` 校验照旧通过）。

## 1. 现象（复现）

空库（`CREATE DATABASE … OWNER alun`）逐条 apply `backend/migrations/**/V*__*.up.sql`（版本升序）：

```
FAIL v0267 V0267__sales_menu.up.sql
  ERROR:  V0267 自检 6.1 失败：以下销售权限码在 sys_menu 里没有承载 →
    business:erp:shipment:* / receivable:* / receipt:* / invoice:*（共 17 个）
== applied OK: 197, first failure at v0267
```

原始输出：`raw/01_replay_without_v0266_FAIL.txt`（用 `scripts/verify/mig_replay_fresh.sh` 复跑，退出码 1）。

应用侧同样失败（日志只有 `数据库迁移失败: query failed: : db error`，看不出是哪条自检）。

## 2. 根因

`V0267__sales_menu` 的授权矩阵有 47 对 `(角色, 权限码)`，它做了三处**自检**：

| 自检 | 要求 | 依赖谁 |
|---|---|---|
| 6.1 | 矩阵里的 25 个销售码必须在 `sys_menu` 有承载行 | 其中 17 个码由 **V0284 / V0288 / V0293 / V0296**（号段 284–296）才种下 |
| 6.3 | 6 个销售页面都已存在并归位到「销售管理」目录 | 4 个页面同上（发货 / 应收 / 收款 / 发票） |
| 6.4 | 矩阵里的 5 个角色码必须存在 | `SALES_SUPERVISOR` 由 **V0281** 才种下 |

⇒ 空库按版本号升序跑到 267 时，这些行**都还不存在**，自检必 RAISE。
V0267 当年能过，是因为 SO-25 是在 `cloudpivot_so24`（已含 287–301）的副本上应用的
—— **它从没在空库上跑过**，所以这个乱序依赖一直没被发现。

## 3. 影响面：整条迁移链只有这一个乱序依赖（实测）

把 V0267 在两个副本里挪到末尾（`V0302`）再重放：

```
== applied OK: 217, FAIL: 0
```

原始输出：`raw/03_blast_radius_v0267_last.txt`。⇒ 修好这一处，空库重放就通；
不需要「宣布空库重放不受支持」，也不需要维护 baseline dump。

## 4. 修法：新增 `V0266__sales_menu_prereq`（前置承载）

**不动历史文件**是硬约束（`schema_migrations.checksum` 与文件内容绑定，改一个注释都会让
所有已应用库启动即 `migration … was modified after it was applied`）。
所以唯一合法解是「把 V0267 依赖的行提前种下」：

| 内容 | 来源（逐字镜像） | 幂等保证 |
|---|---|---|
| 发货单页 …2841 + 2 按钮（…2842/2843） | `V0284` | `ON CONFLICT (id) DO NOTHING` |
| 应收台账页 …2880 + 4 按钮（…2881–2884） | `V0288` | 同上 |
| 收款单页 …2890 + 4 按钮（…2891–2894） | `V0293` | 同上 |
| 销售发票页 …2895 + 3 按钮（…2896–2898） | `V0296` | 同上 |
| 角色 `SALES_SUPERVISOR`（…0f0007） | `V0281` | `ON CONFLICT (role_code) WHERE deleted_at IS NULL DO NOTHING` |

那 5 条迁移随后执行时 0 行插入，其自带自检（码有承载 / 按钮挂在页面下 /
`is_visible` 仍为 FALSE）依旧成立。

**本迁移故意不做的事**：不写 `sys_role_menu`（角色矩阵只有一处真相源 = V0267）、
不改 `path`/`component`/`is_visible`（归 V0286/291/294/297 收尾，提前填会让
`system::menu::tests::n08` 在空库上判红）、不种 `so_approve` 流程（V0281 的职责）。

**down 是 no-op**：这 17 行菜单 + 1 个角色的生命周期属于 V0281/284/288/293/296，
在本迁移里删行会连带删掉存量库在用的菜单。

### 4.1 为什么不是另外两条路

| 方案 | 为什么没走 |
|---|---|
| **改 V0267**（拆授权段 + 自检宽容 + 新增 V03xx 补授权） | 必须改已应用文件 ⇒ 生产/144 要在一个窗口里做 checksum 重写（`repair`），风险与协调成本都落在生产上；而实测表明**只需要一条新增迁移**就能达到同样效果 |
| **宣布「全新安装 = baseline dump」** | 会让 `scripts/verify/erp_clean_db.sh` / skill 里的「建空库 → 起实例 → 全量单测」干净库口径长期失效，且 baseline dump 要维护一份**业务表排除清单**，漂移风险更高。本卡把这条路留作「如果将来再出现更深的乱序依赖」的兜底，不再是必需 |

## 5. 验收证据

| 验收项 | 结果 | 证据 |
|---|---|---|
| 空库逐条重放全部迁移 | **219/219 通过** | `raw/02_replay_with_v0266_PASS.txt` |
| 空库起实例（`erp_clean_db.sh`，真引擎） | `数据库迁移完成: 应用 229 个迁移` → `启动完成` 1868ms | `raw/04_clean_db_startup.txt` |
| 空库 `/health` | **200**；库指纹 `max(version)=302`/`229 条`、`admin=1`、已发布流程 15 | 同上 |
| 重启幂等 | `应用 0 个迁移` + `/health` 200 | 同上 |
| **存量库**（`TEMPLATE cloudpivot_so26`：227 条 / max=301） | WARN「乱序补迁」（不阻断）→ `应用 2 个迁移`；`sys_menu/sys_role/sys_role_menu` 行数 **362/10/755 前后完全一致**（纯 no-op）；**0 条** `was modified` 引擎告警 | `raw/05_gapfill_existing_db.txt` |
| 存量库再重启 | `应用 0 个迁移` + `/health` 200 | `raw/06_gapfill_restart.txt` |
| 结束态与「V0267 放最后」的参考库等价 | `sys_menu`(315 行)/`sys_role_menu`(624 行) 按 `(menu_type, auth_code, 父码, sort_order, is_visible, path, component)` 逐行比对 **0 行差异** | 见 §7 复现命令 |
| 门禁闸⑥⑦⑧ | 全 PASS / 0 孤儿 / 无未知角色码 | `raw/08_gates_on_clean_db.txt` |

## 6. 修复带来的直接收益：7 条「假红」消失

SO-26 报的 7 条真库用例假红，根因是**拿不到干净库**（只能跑在带旧业务数据的模板库副本上）。
在空库重放出来的干净库（`cloudpivot_td403a60b`）上跑这三组用例：

| 用例组 | 模板库副本（改前） | 干净库（改后） |
|---|---|---|
| `receivable_aging::`（含 3 条 `db_*`） | 3 条红（被 `biz_invoice` / `biz_receipt_allocation` 外键挡住 `DELETE FROM biz_receivable`） | **9 passed / 0 failed** |
| `payable_bridge::`（含 `p17_skip_approval_payment_logs_are_purged_by_id`） | 1 条红（撞库里已发布的 `payable_pay`） | **23 passed / 0 failed** |
| `sales_approval_bridge::db_tests::` | 2 条红（待办 `node_key` 为空） | **4 passed / 0 failed** |

原始输出：`raw/07_clean_db_tests.txt`（`ERP_DB_URL`/`SYS_DB_URL`/`TASK_DB_URL` 指向干净库，
无 SKIP，`--test-threads=1 --nocapture`）。

## 7. 复现命令

```bash
# ① 30 秒探针：空库按版本号重放整条迁移链（发版/改迁移前先跑）
bash scripts/verify/mig_replay_fresh.sh mycard            # PASS = 219/219
MROOT=<不含 V0266 的迁移树> bash scripts/verify/mig_replay_fresh.sh neg   # 负样本：FAIL at v0267

# ② 真引擎口径：建干净库（一次性跑完迁移 + bootstrap）
ERP_DB=cloudpivot_mycard bash scripts/verify/erp_clean_db.sh
#   → 期望「数据库迁移完成: 应用 229 个迁移」+「启动完成」；库指纹 admin=1

# ③ 存量库 no-op 证明（用任意已迁移库的副本，不碰源库）
su - postgres -c "psql -c 'CREATE DATABASE cp_noop OWNER alun TEMPLATE cloudpivot_so26'"
#   起一次实例（profile=test + 三把密钥）→ 期望 WARN 乱序补迁 / 应用 1 个迁移 / 菜单三表行数不变

# ④ 结束态等价性（把 V0267 挪到最后的参考库 vs 本修法的库）
#    比 sys_menu / sys_role_menu 的 (menu_type, auth_code, 父码, sort_order, is_visible, path, component)
```

## 8. 边界与遗留

- 本卡**没有**改动 `V0267` 及任何已应用迁移；`schema_migrations.checksum` 对存量库全部照旧通过（实测）。
- 新增 `V0266` 在存量库上会触发一次 `WARN 待应用迁移的版本低于已应用最大值（乱序补迁）`。
  这是引擎**设计内**的行为（集合语义补迁，`out_of_order=false` 只告警不拒绝）；
  发版脚本 `scripts/verify/deploya_migrate.sh --assert-pending` 的集合差会自动把它算进待跑清单。
- `V0295__sales_invoice.up.sql:8` 有一句「V0266 已被销售订单菜单卡占用」的历史注释 ——
  **实测全仓无任何 `V0266` 文件、`git log --all` 也无新增记录**，各库 `schema_migrations` 里同样没有 266 行，
  故本卡复用了这个空号段（紧邻 267，语义最清楚）。历史注释按「已应用迁移不可改」保持原样。
- 前端无关；`cargo` 无改动（本卡只加 SQL + 一个 verify 脚本 + 本文档）。
