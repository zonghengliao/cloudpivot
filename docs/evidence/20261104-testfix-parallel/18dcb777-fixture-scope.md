# t_18dcb777 · masking / system::user / system::role 夹具清扫「按用例身份、按 id 集合」改造

> 现象、取证、归因见同目录 `9a6e3d35-full-suite-2runs.md`（§6 分类表 / §7 诊断矩阵 / §8 归因）。
> 本文件只记**本卡的改动与自证**，所有数字都来自下面的原始日志。

## 1. 缺陷（本卡要修的三件事）

| # | 位置 | 缺陷 | 实测症状 |
|---|---|---|---|
| 1 | `business/masking/tests.rs` | `sweep()` 按 `tc_m%` **整批前缀**删用户/角色/规则；每个用例开头都调 | `--test-threads=4` 8/10（m05、m07 红）→ `--test-threads=1` 10/10 |
| 2 | `system/user/tests.rs` | 同上（`tc_u%`），且每个用例现造一个**临时超管**操作者、末尾删掉 | `--test-threads=4` 7/12（u03/u05/u07/u08/u09 红）→ 12/12 |
| 3 | `system/role/tests.rs` | 同上（`tc_r%`），同样现造/删临时超管 | `--test-threads=4` 4/8（r02/r03/r05/r07 红）→ 8/8 |

根因不是产品逻辑：**A 用例的 sweep 把 B 用例正在使用的操作者/角色/规则删掉了**。
典型报错 `30001 操作者账号不存在，拒绝操作`、`sys_user_role_user_id_fkey` 违约、
`规则写入成功但回读失败`。

另有跨模块共因（同一全量里各红一条）：

- `payable_bridge::p17_seeded_flow_graph_is_consistent`
  `DELETE FROM sys_user ... biz_flow_task_assignee_id_fkey` 违约；
- `payable_payment::d5_cancel_payable_revokes_pending_payment`
  `发起付款审批单: biz_flow_task_assignee_id_fkey` 违约。

两者单跑都绿 ⇒ 纯并发：`assignee_type='ADMIN'` 的流程节点在运行时展开「全部超管」，
而这些「临时超管」正在被别的用例删除/新建。

## 2. 改动

### 2.1 `src/system/tests_support.rs`（共用基建）

新增**用例级夹具身份** `ts::Fixture`：

- `Fixture::new(family, case)` → 唯一身份 `<family>_<case>_<毫秒+进程内序号>`
  （如 `tc_m_m05_T1793800000000001`），并在进程内登记「在途身份」；
- `fx.name("holder")` → 该身份下的夹具标识（username / role_code / menu.name /
  dept_code / `sys_field_mask.reason` 共用一串）；
- `fx.sweep()` → **两段式**：`SELECT id FROM <表> WHERE <身份列> LIKE '<身份>%'` →
  `DELETE FROM <表> WHERE id IN (<那批 id>)`。引用行（`sys_user_role` / `sys_role_menu` /
  `sys_role_dept` / `sys_role_mutex` / `sys_user_preference` / `sys_field_mask`）按外键
  依赖顺序先清；实体行按 id 集合删，整批失败时退化逐行，仍被 `biz_*` 钉住的行软删。
- `Fixture` 在 `Drop` 时注销身份 ⇒ `x00` 残留自检只排除**正在跑的那一刻**的行，
  用例一结束（含 panic 展开）它留下的行立刻重新纳入判据（`active_identity_regex`）。

`count_fixture_residue` 增加 `AND <列> !~ '^(在途身份1|在途身份2…)'`，**其余判据一字未改**：
`created_at < now-5s`、夹具前缀白名单、种子号段排除、`kept_by_business` 精确对账都照旧。

### 2.2 `src/system/user/tests.rs` / `src/system/role/tests.rs`

- 每个用例 `let fx = ts::Fixture::new("tc_u", "u03")`，所有夹具（用户/角色/菜单/部门）
  用 `fx.name(...)`；收尾 `fx.sweep()`。
- 删除「每个用例现造一个临时超管操作者再删掉」：操作者直接用**内置管理员 `admin`**
  （`guard::ensure_role_grant_allowed` 判定的是库里的角色绑定，admin 由 bootstrap 保证持有
  `SUPER_ADMIN`，claims 仍是 `*:*:*`）。这样本模块**不再增删超管**，
  跨模块的 `biz_flow_task.assignee_id` 竞态从源头消失。
- 断言一条未改（u01–u12 / r01–r08 全部保留原判据与文案）。

### 2.3 `src/business/masking/tests.rs`

- `setup(case)` 改为返回 `ts::Fixture`：角色/用户/规则 `reason` 全用 `fx.name(...)`；
  收尾 `fx.sweep()`。删掉开头那次全前缀 `sweep()`。
- `m10` 重写为**并发口径的回归闸**（断言更强，不是更弱）：
  ① 被 `sys_role_mutex` / `sys_user_role` / `sys_field_mask` 钉住的四类夹具必须被收干；
  ② 反向对照改成「**同族前缀（`tc_m`）但不同身份**的兄弟用例夹具一行不少」——
  这正是并行时「别人正在用的行」。

### 2.4 `src/business/erp/payable_bridge.rs`（p17 夹具）

- `cleanup_fixtures()` 不再 `DELETE FROM sys_user`，改为
  `sys_user_role` 解绑 + `UPDATE sys_user SET deleted_at = now()` **软删**；
  并按 `flow/assignee.rs::admins()` 的真实展开条件（`u.deleted_at IS NULL`）保证
  「摘出超管集合」。
- `setup()` 用 `ON CONFLICT (id) DO UPDATE SET deleted_at = NULL` **复活**这三行。
- 顺带去掉 `DELETE FROM biz_flow_task WHERE assignee_id IN (夹具用户)` 与
  `biz_flow_cc`：那是兄弟模块**正在跑**的实例的行，删掉等于把互删从 `sys_user` 搬到
  `biz_flow_task`。我方实例的待办由 `DELETE FROM biz_flow_instance`（子表 CASCADE）负责。
- `p17_fixture_users_are_removed_by_cleanup` 的判据从「行数为 0」改成
  「**活的**行数为 0 + 角色绑定为 0」——这正是「不再被当成审批人」的定义；
  物理行保留是为了外键完整性（文档写明理由）。

## 3. 自证（缺一不可，全部为真实执行输出）

环境：本机 4 核；隔离库 `cloudpivot_18dcb777`（`scripts/verify/erp_clean_db.sh` 新建，
`schema_migrations = 219|163`、超管 1）；**测试线程数一律用 libtest 默认值（= 4）**，
`-j 1` 只限编译并行度。

### 3.1 三个模块 × `--test-threads=4` × 4 轮（第 1 轮干净库，2–4 轮不清库 = 残留场景）

命令：`bash scripts/verify/t18_fixture_scope_rounds.sh`（原始日志 `18dcb777/module-rounds.txt`）

```
round=1 module=business::masking::tests -> test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 2321 filtered out; finished in 0.25s (SKIP 行=0)
round=1 module=system::user::tests      -> test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 2319 filtered out; finished in 2.41s (SKIP 行=0)
round=1 module=system::role::tests      -> test result: ok.  8 passed; 0 failed; 0 ignored; 0 measured; 2323 filtered out; finished in 0.84s (SKIP 行=0)
round=2 … round=4（同一库、不清库，三轮 × 三个模块全部 0 failed，SKIP=0）
=== OVERALL: ALL_GREEN
```

原始日志：`18dcb777/module-rounds.txt`

关键点：`SKIP 行=0` —— `system::*` 的真库用例只认 `SYS_DB_URL`，缺它就会打印 `SKIP`
后 `return`（假绿），所以自证脚本同时导出 `ERP_DB_URL` 与 `SYS_DB_URL`。

### 3.2 全库 `cargo test -j 1` 连跑 2 轮（第 2 轮在同一库的残留上）

命令：`bash scripts/verify/t18_fixture_scope_full_twice.sh`（原始日志 `18dcb777/full/`）

```
round=1 exit=0 耗时=1441s  lib: test result: ok. 2331 passed; 0 failed; … finished in 1075.64s
round=2 exit=0 耗时=1401s  lib: test result: ok. 2331 passed; 0 failed; … finished in 1025.79s
=== PASS：2 轮 0 failed
```

两条门禁在**两轮里都是 ok**（`--test-threads=4`，即 libtest 默认线程数）：

```
test system::tests_support::self_check::x00_suite_leaves_zero_fixture_residue ... ok
test business::masking::tests::m10_sweep_drains_fk_pinned_fixture_and_stays_scope_scoped ... ok
test business::erp::payable_bridge::tests::p17_fixture_users_are_removed_by_cleanup ... ok
test system::menu::tests::n08_menu_paths_all_resolve_in_frontend_router ... ok   ← 卡 t_71b01775 的前端路由已落地
```

跑完（第 2 轮结束）在库上直接数残留：

```
sys_user（tc_%）   = 0      sys_role（tc_%）       = 0
sys_menu（tc_%）   = 0      sys_field_mask（tc_%） = 0
p17 夹具用户：活的 3（按设计常驻，下次 setup 摘净）/ 软删 0
超管人数 = 3（p17 两个夹具超管 + 内置 admin，属「上一轮 p17 用例的窗口」）
```

### 3.3 跨模块那一对（p17 / d5）同进程并发验证

```
ERP_DB_URL=… SYS_DB_URL=… cargo test --lib payable -- --test-threads=4
→ test result: ok. 88 passed; 0 failed; 0 ignored; 0 measured; 2243 filtered out; finished in 83.90s
```

（该过滤器同时命中 `payable_bridge` 的 p17 系列、`payable_payment` 的 P7/D5 系列与
`voucher` 系列，共 88 条，同进程 4 线程跑。）

### 3.4 静态检查

```
cargo clippy --lib --tests -- -D warnings     → 无任何 error/warning
rustfmt +1.94.1 --edition 2024 --check <本卡 5 个文件>  → FMT_OK
```

## 4. 不做的事（对照卡面）

- 没有用 `--test-threads=1` / `-j 1` 当修复手段：全部自证都是默认 4 线程；
- 没有放宽/删除断言：`x00`（只多了一个「排除在途用例身份」的边界，判据其余全同）、
  `m10`（反向对照从「非 tc_m 前缀」加强为「同族前缀不同身份」）、`r0x` / `u0x` /
  `m0x` 全部保留；
- 没有为了变绿改业务代码语义：改的全是 `#[cfg(test)]` 夹具与测试基建，
  唯一落在 `payable_bridge.rs` 的改动也在 `#[cfg(test)] mod tests` 内。
