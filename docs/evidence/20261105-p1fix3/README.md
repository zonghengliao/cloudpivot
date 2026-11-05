# P1-FIX-3 授权后权限缓存不失效（必须重启才生效）— 证据与结论

卡号 `t_e14b6920`。隔离实例：`127.0.0.1:18116`，库 `cloudpivot_p1fix3_a`（**不碰生产 144**），
起停脚本 `p1fix3_instance.sh`，验收脚本 `p1fix3_cache_e2e.py`（同一份脚本跑 `fix` / `baseline` 两种模式）。

## 1. 根因（两条独立的「旧值残留」通道）

| # | 通道 | 旧行为 | 现象 |
|---|------|--------|------|
| A | **签发令牌时读缓存**：`issue_token` 用 `load_role_codes()` + `load_permissions()`，两者都先查 `perm:user_perms:{uid}` / `perm:user_roles:{uid}`（TTL 300 s） | 授权写库的路径不是本进程（运维直连 SQL、别的实例、脚本补授权）时，本进程缓存**没人删** | 重新登录拿到的仍是旧权限码 ⇒ 接口继续 403；单机是进程内缓存，清它的唯一手段就是**重启** |
| B | **菜单写点只推进 `token_version`，不清主体缓存**：`menu::handler::update/delete` | 旧令牌被踢掉（401 逼重登），但重登时读的还是缓存里的旧权限码 | 重登后最长 300 s 继续 403；删菜单同理 |

评审报告 §14「授权后权限缓存不失效（需重启）」对应的就是 A：隔离库两轮对比 `05_chains_run2.txt`(403) → 重启后 `run3`(200)。

## 2. 改法（本次落地）

1. **签发路径直读库 + 写穿缓存**（`system/auth/model.rs::load_principal_snapshot`，
   `system/auth/handler.rs::issue_token`）：库是唯一真源，登录 / 刷新签发的权限快照不再读缓存；
   读到后把真值写回缓存，保证随后 `me` / `menus` 与令牌快照一致。
   超管的通配码 `*:*:*` **不入缓存**（同一缓存键，摘掉超管角色后会被非超管分支读到 = 提权）。
2. **按持有者精确批量失效**（`invalidate_role_holders` / 新增 `invalidate_menu_holders`）：
   `sys_user_role` / `sys_role_menu ⋈ sys_user_role` 反查「谁受影响」，逐个删三个键
   （角色 / 权限码 / 可见菜单，`principal_keys()` 单一真值源）。
3. **反查失败退化为整命名空间清理**（`invalidate_all_principals` → `Cache::delete_pattern` 三条前缀）：
   宁可多清不可漏清 —— 反查一失败就退回「旧权限码留到 TTL」正是本卡的现象。
   反查成功但**确实没有持有者**时返回 `Some(vec![])`，不触发 `KEYS` 扫描。
4. **菜单写点配对**（`system/menu/handler.rs::update` 调 `invalidate_menu_holders`；
   `delete` 在**事务前**反查持有者、提交后清理 —— 绑定在事务里删掉后就查不到了）。

## 3. 真机验收（隔离实例，全程**不重启**，同一实例内完成）

| 场景 | 基线（改动前二进制） | 修复后 |
|------|--------------------|--------|
| A1 授权前探测 | 403 | 403 |
| **A2 直连 SQL 补授权 → 重新登录 → 立即访问** | **403（复现 bug）** | **200 ✅** |
| A3 直连 SQL 撤销 → 重新登录 → 立即拒绝 | 403 | 403 |
| B3 接口授权后旧令牌 | 403（旧令牌无新码） | 403 |
| B4 接口授权 → 重新登录 → 立即访问 | 200 | 200 |
| B6 接口撤销后旧令牌 | 401 权限已变更 | 401 权限已变更 |
| B7 接口撤销 → 重新登录 → 立即拒绝 | 403 | 403 |
| C1 / C2 `me` 权限码与令牌一致 | 一致 | 一致 |

- `before_run3_baselinebin.txt`：基线二进制一轮，`PASS=24 FAIL=0`（其中 A2 断言「期望 403」= bug 复现成功）
- `after_run3_newbin.txt` / `after_run4_newbin.txt`：**修复后新二进制连跑两轮**，`PASS=24 FAIL=0`（A2 期望 200 实际 200）
- `before_run1/2.txt` + `after_run1/2.txt`：本卡第一轮改动时的同样的两两对比（baseline 复现 / fix 通过）

## 4. 单测（7 条，`backend/src/system/auth/cache_tests.rs`）

```
cargo test --lib system::auth::cache_tests   → 7 passed（带 SYS_DB_URL，真库，连跑两轮皆绿）
C01 三个主体缓存键与 constants 逐字一致且互不相同
C02 三条通配模式覆盖键空间且互不重叠
C03 无缓存（启动降级）时三个失效入口都是安全 no-op
C04 角色持有者反查精确（1 中 0 漏；无持有者返回 Some(空) 而非 None）
C05 菜单持有者反查跨两跳（用户 → 角色 → 菜单）
C06 签发快照直读库：SQL 侧的增权 / 撤销在下一份快照立刻反映
C07 超管快照为通配码；角色投影照常入缓存（供 me 用）
```

受影响模块回归：`system::role::` 13 passed、`system::authz_tests` 15 passed（各两轮）；
`system::menu::` 20 passed（**须 `--test-threads=1`**，见下）。

## 5. 诚实边界（没做到的 / 需要知道的）

1. `me`（`GET /api/system/auth/me`）仍走缓存（TTL 300 s）。经接口的授权 / 撤销都会清缓存，所以两侧一致；
   但**绕过接口**（直连 SQL）改授权时，`me` 最长 300 s 可能仍显示旧码 —— 接口鉴权以令牌快照为准，不受影响。
   要彻底对齐可把 `me` 也改成直读库（多一次 JOIN），本卡按「不扩大改动面」未做。
2. 反查持有者失败时的整命名空间清理会扫 `KEYS`，只在低频管理动作 + 库抖动时触发。
3. `system::menu::` 的 20 条用例在**并行跑**时会偶发 `n14_menu_auth_codes_have_source_action_point` 变红：
   该用例扫描「库里有、源码里 0 落地」的权限码，而兄弟用例（n13/n14 的 `..._writes_succeed_without_cache`）
   在并行线程里**正持有**临时菜单行（`N13-xxx:btn`）⇒ 被当成死码。串行（`--test-threads=1`）稳定 20/20。
   与本次改动无关（改动不涉及菜单权限码），已在证据里留痕，未在本卡修（属测试隔离问题，另开卡）。
4. 未部署生产 144（本卡只做代码 + 隔离实例验证）。
## 6. 本卡复验（run 709，HEAD=`8def5d8`，2026-11-05 19:04–19:06）

本卡首轮验证在 15:0x–15:33（commit `9daa73b`）；之后 main 前进到 `8def5d8`，
本次在**当前 HEAD 上重新构建的二进制**上复验，不依赖首轮结论。

### 6.1 代码层面：本卡补丁仍在 HEAD，且 HEAD 相对构建点无 Rust 改动

```
$ git merge-base --is-ancestor 9daa73b HEAD   → 是（ancestor）
$ git diff --stat 78a8a8f..HEAD -- backend/src backend/tests backend/Cargo.toml
（空 → 无 Rust 改动）
$ ls -l backend/target/debug/cloudpivot        → 2026-11-05 18:21:31（构建于 78a8a8f）
```

### 6.2 真机验收（三分对比，全程同一实例不重启）

| 轮次 | 二进制 | 日志 | 结果 |
|------|--------|------|------|
| run7（本轮） | HEAD `8def5d8` | `after_run7_head.txt` | **PASS=24 FAIL=0** |
| run6（19:02） | HEAD 同二进制 | `after_run6_head.txt` | PASS=24 FAIL=0 |
| run5（19:01） | HEAD 同二进制 | `after_run5_head.txt` | PASS=24 FAIL=0 |
| baseline（19:02） | 改动前二进制 `/root/p1fix3-baseline-bin` | `before_run4_baselinebin.txt` | PASS=24 FAIL=0，**其中 A2 断言「期望 403」实际 403 = bug 复现** |

复现场景 A2 的前后状态码：

```
基线二进制：  A2 授权后重新登录即可访问（baseline 期望 403）  → 实际 403   ← 授权后仍 403
HEAD 二进制： A2 授权后重新登录即可访问（fix 期望 200）       → 实际 200   ← 立即生效，未重启
撤销：        A3 撤销后重新登录立即被拒（两种模式都应 403）    → 实际 403   ← 撤销同样立即生效
接口侧撤销：  B6 撤销后旧令牌立刻失效                        → 401 您的权限已变更
              B7 撤销后重新登录立即被拒                      → 403
```

### 6.3 单测（HEAD 上重跑，连跑两轮）

用**已迁移到最新**（193 条迁移，含 V0251）的库 `cloudpivot_p1fix3_a`：

```
round 1/2  system::auth::cache_tests   7 passed / 0 failed   （C01–C07）
round 1/2  system::role::             13 passed / 0 failed
round 1/2  system::authz_tests        15 passed / 0 failed
menu（串行）                          19 passed / 1 failed ← 见 §6.4，与本卡无关
```

日志：`02_unit_cache_tests_two_rounds.txt`（cloudpivot_test 库上也各跑两轮全绿）、
`03_unit_role_authz_two_rounds.txt`、`05_unit_all_current_db.txt`。

### 6.4 本轮撞到的两条红（均与本卡无关，已归属）

1. `system::menu::tests::n09_menu_icons_exist_in_tdesign` —— **真实数据缺陷但属 V0242**：
   `backend/migrations/business/V0242__erp_expense_tables.up.sql:263` 给「我的报销」种了
   `icon='invoice'`，TDesign 图标集里没有该名 ⇒ 侧栏渲染空白（与本仓「图标渲染空白」已知坑同类）。
   修法是换成 `invoice-list`/`file-pdf` 之类存在的名字，属另一张卡的范围（本卡不改别人的菜单种子）。
2. `cloudpivot_test` 库是**陈旧库**（152 条迁移，缺 V0239 之后的种子）⇒ 用它跑
   `n19_approval_node_role_assignees_have_approve_permission` / `n10_..._menu_code` 必红，
   而同一份代码在最新库 `cloudpivot_p1fix3_a` 上两条都过（实测：7 个业务角色均持有
   `business:flow:approve`，`business:erp:stock-take:list` / `contract:list` 菜单存在）。
   ⇒ 这两条红是**测试库未迁移**导致的假红，不是代码回归。
