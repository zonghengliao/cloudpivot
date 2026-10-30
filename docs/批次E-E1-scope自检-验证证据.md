# 批次E · E1：scope 自检误报修复 + 11 个豁免逐条核对 —— 验证证据

- 卡：`t_68bea742`【整合-E1】修 scope 自检误报 + 逐条核对 11 个豁免
- 提交：`backend/src/business/permission/{scope_model.rs, dto.rs, scope_handler.rs}`（3 文件 +322/-42）
- 迁移：**0**　接口：**1 个**（`GET /api/business/permission/scope/uncovered-tables`，返回值扩展、向后兼容）
- 结论：**`uncovered: 0`**（生产实测）；11 张登记表逐条核对，**11 条理由全部成立**；另给出 2 条"待修清单"（本卡不修）

## 验收对照

| 验收项 | 结果 | 证据 |
|---|---|---|
| `uncovered-tables` 返回 `uncovered: 0` | ✅ | §七 生产实测 `uncovered: 0, uncovered_tables: []` |
| 每个表有 `status` + 非空 `reason` | ✅ | §七 响应样例；单测 `d8_coverage_matches_registry` 硬断言 reason 非空 |
| 逐条核对表每条都有判定 | ✅ | §三（11 行，判定 = 成立） |
| 不成立的理由 → 待修清单 | ✅（无"理由不成立"项，但有 2 条独立待修） | §四 |
| 现有 780+ 测试不回归 | ✅ | §六 `cargo test --lib` → **812 passed / 0 failed** |
| §6.2 部署验收 | ✅ | §七 |

---

## 一、根因（比卡面写的更彻底一层）

自检接口的实现是"登记表 → `wired: bool`"一档投影，而**运行时镜子清单本身是空的**：

```rust
// 改动前 backend/src/business/permission/scope_model.rs
pub const WIRED_TABLES: &[&str] = &[];          // ← 11 张表一张都没登记
pub fn is_wired(table: &str) -> bool { WIRED_TABLES.contains(&table) }
```

所以 `GET …/uncovered-tables` 对 11 张登记表**一律**返回 `wired=false`。卡面判断的"判定逻辑只认 `scope::paginate`"是第二层原因：即使把表名填进去，这套 `bool` 也无法表达另外两种合法形式 ——

1. `scope::paginate_unscoped(…ScopeTarget::unscoped(t))` + 一行 `// SCOPE-EXEMPT: <理由>`（显式豁免，B5 定的口径）；
2. **不分页**的表（部门树 `sys_dept`：一次取全量再组树，没有 `{FILTER}` 位置契约，裁剪走 `scope::filter_for`）。

自检一旦把这两类打成"未接入"，11 张全绿的表就变成了 11 条常年告警 —— **自检本身成了噪音，下次真漏报就没人看了**，这是本卡要修的东西。

## 二、改动（3 文件）

| 文件 | 改动 |
|---|---|
| `scope_model.rs` | 新增 `WiringStatus{Wired,Exempted,Uncovered}` + `TableWiring{table,status,no_paginate,anchor,reason}` + `TABLE_WIRING`（11 行登记表）；`table_coverage()` 改为按登记项投影，每行带 `status`/`no_paginate`/`reason`；新增 `exempted_tables()`；`is_wired()` 语义改为 `status != uncovered`（exempted 也算已处理）；抽出 `coverage_row()` 让 exempted 分支可被单测直覆盖 |
| `dto.rs` | `TableCoverage` 增 `status` / `no_paginate` / `reason`（`wired` 保留）；`UncoveredTablesRes` 增 `exempted` / `exempted_tables`（`registered`/`wired`/`uncovered`/`uncovered_tables`/`tables` 全部保留） |
| `scope_handler.rs` | 只改 `uncovered_tables()` 的处理体：按 `status` 分流计数（`wired` / `exempted` / `uncovered`），不再用 `!wired` 一档 |

**兼容性**：老字段一个没删，只加字段（`status=exempted` 行在老口径下也是 `wired=true`，即"不告警"语义不变）。前端无消费方：`grep -rn "uncovered" frontend/src` → **0 命中**（实测）。

**防漂移**：新增单测 `d8c_wiring_anchors_exist_in_source` —— 每张登记为 `wired` 的表，其 `ScopeTarget::<anchor>` 常量必须真的出现在某个**含 `scope::` 调用**的 `src/**.rs` 里。以后谁把 scope 调用删了却留着登记项，`cargo test` 直接红。

## 三、逐条核对（每一条都在代码里核过）

| 表名 | 当前形式 | 理由 | 判定 | 待修? |
|---|---|---|---|---|
| `sys_user` | `scope::paginate(SYS_USER)`（`system/user/model.rs:261`）+ 批量可见性 `scope::filter_for`（`:553`） | 人员列表是最核心的数据权限场景，SELF = 本人这一行 | **成立** | 否 |
| `sys_dept` | **不分页**（`no_paginate: true`）：`scope::filter_for(SYS_DEPT)`（`system/dept/model.rs:119`），写点 `guard::bump_epoch` | 部门树一次取全量再组树，没有分页就没有 `{FILTER}` 契约；裁剪在 SQL 层做，不做必泄漏「部门名/负责人/电话」 | **成立** | 否 |
| `sys_import_task` | `scope::paginate(SYS_IMPORT_TASK)`，归属列 `create_by`（`business/impexp/model.rs:470`） | 此前 `only_mine=false` 时全站可见（含他人文件名/行数/错误摘要），B5 已按归属列收口 | **成立** | 否 |
| `sys_export_task` | 同上（`business/impexp/model.rs:981`） | 同导入 | **成立** | 否 |
| `operation_logs` | `scope::paginate(OPERATION_LOGS)`，`user_id`（`support/log/model.rs:352`） | 操作日志按操作人收口 | **成立** | 否 |
| `audit_logs` | `scope::paginate(scope_target())`，`user_id`（`support/log/model.rs:479 ← 284-290`） | 审计日志按操作人 | **成立** | 否 |
| `auth_logs` | 同上 | 登录日志含 IP / 设备，属个人信息 | **成立** | 否 |
| `security_logs` | 同上 | 安全日志同上 | **成立** | 否 |
| `password_change_logs` | 同上 | 密码变更日志按 `user_id`（`V0017` 实测列名） | **成立** | 否 |
| `sys_notice` | 管理视角 `page_notices` → `scope::paginate(SYS_NOTICE)`，`sender_id`（`business/notice/model.rs:563`） | 公告管理列表必须按发送人收口；「我的通知」是**另一张表** `sys_notice_receiver`（按 `r.user_id` 硬绑定，见 §四） | **成立** | 否 |
| `sys_file` | `scope::paginate(SYS_FILE)`，`uploaded_by`（`business/storage/model.rs:1072`） | 文件按上传人收口；无部门列 → 部门类范围 fail-closed 降级「仅本人」（§四 #2） | **成立** | 否（降级项见待修 #2） |

> 结论：**11 张登记表无一落在 `uncovered`，也无一条豁免理由站不住**。`exempted`（`paginate_unscoped` + `SCOPE-EXEMPT`）通道已实现并单测覆盖（`d8b` 合成行），当前计数为 0 是**事实**而不是缺陷：存量的 `SCOPE-EXEMPT` 都写在**未登记表**上（`sys_notice_receiver`、`sys_import_task_error`、`sys_storage_config`、`sys_storage_policy`、`sys_notice_template`、审批流定义侧等），它们不参与这 11 行的判定。

### 卡面证据的两处更正（都复核过）

1. **`sys_file`**：卡面写「`paginate_unscoped`（超管专属），`business/storage/model.rs:221`」。实际 221 行是 `ScopeTarget::unscoped("sys_storage_config")` 的分页（**另一张表**，未登记）；`sys_file` 本体在 **1072 行**走 `scope::paginate(SYS_FILE)`。判定仍是"已接"，但形式是 `wired` 而非 `exempted`。
2. **`sys_notice`**：卡面写「按 `user_id = 当前用户` 硬绑定（`business/notice/model.rs:664`）」。664 行那条 `SCOPE-EXEMPT` 属于 `sys_notice_receiver`（「我的通知」）；登记表里的 `sys_notice` 是**管理视角** `page_notices`（563 行，`paginate` + `sender_id`）。两张表、两种处理，都不构成 `sys_notice` 的豁免。

## 四、待修清单（本卡不修，供另开卡）

| # | 位置 | 问题 | 为什么本卡不动 | 建议 |
|---|---|---|---|---|
| 1 | `backend/src/business/flow/task_bridge.rs` | 查询 `biz_flow_task` / `biz_task`，**既无 `scope::` 也无 `SCOPE-EXEMPT`** → `bash scripts/check.sh` 闸② 实测 **FAIL 1 处**（EXIT=1）。D3b（`0d8fa8e`）引入，非本卡改动 | 属 D3b 的接线口径问题，改它要动审批门闸，超出 E1 范围 | 接 `scope::filter_for` 或补显式豁免+理由；建议单独一张卡，附 `scripts/check.sh` 全绿为验收 |
| 2 | `sys_file` / `sys_notice` / `sys_import_task` / `sys_export_task` / 5 张日志表 | 这些表**没有部门列** → 部门类范围（`DEPT` / `DEPT_AND_CHILD` / `CUSTOM`）在 `scope.rs:462-468` **fail-closed 降级为「仅本人」**并置 `is_degraded()` | 不是"豁免理由不成立"（降级是安全方向），是**覆盖面已知上限**，补它要 join `sys_user` | 另开卡评估「部门形态补齐」（架构 §2.5 的 P1） |

> 说明：`uncovered` 类（"应该过滤但没做"）**为空**；上表 #1 是**门禁红**（不是本接口的 `uncovered`），#2 是**覆盖降级**，两者都在本接口之外，如实列出不代表 E1 未达成。

## 五、登记表怎么维护（写给后来人）

接入一张表 = 改查询 + 在 `scope_model::TABLE_WIRING` 里加一行，四个字段都要填：

```
status      Wired（paginate / 不分页的 filter_for）| Exempted（paginate_unscoped + SCOPE-EXEMPT）| Uncovered（必须 0 行）
no_paginate true 时 reason 里必须写清"为什么不分页"
anchor      ScopeTarget 的常量名（漂移守卫单测据此 grep 源码）
reason      判定依据（恒非空；Exempted 必须以 `SCOPE-EXEMPT:` 开头）
```

漏登记 → 接口报 `uncovered` 且 `d8_coverage_matches_registry` 红（`TABLE_WIRING.len()` 必须 == `SCOPED_TABLES.len()`）。

## 六、单测与门禁

```
$ cargo test --lib
test result: ok. 812 passed; 0 failed; 0 ignored        # E1 前 809，+3（d8 / d8b / d8c）

$ cargo test --lib d8 -- --nocapture
[D8b] exempted 投影：status=exempted wired=true ✓
[D8]  登记 11 张：wired=11 exempted=[] uncovered=0 ✓
[D8c] 11 张 wired 表的锚点在源码中可查 ✓
test result: ok. 3 passed

$ bash scripts/check.sh
==> cargo fmt --check                     PASS（无输出）
==> cargo clippy --all-targets -D warnings PASS（Finished dev profile）
==> cargo check                           PASS
==> pnpm typecheck（vue-tsc --noEmit）     PASS
==> 闸① 0 处                              PASS
==> 闸② 1 处 → business/flow/task_bridge.rs（存量，见 §四 #1）
```

E1 **没有改任何 SQL / 没有任何迁移**（只改自检投影与响应结构），因此不触碰数据面。

## 七、§6.2 部署验收（生产 8023 / 库 `cloudpivot`）

```
① 迁移一致性
prod max version = 127      dev max version = 127        （本卡 0 迁移）

② release 二进制
before sha256 = 3af71c9d284f127fb631836d2f6b36a3396a053b78d25ef4852bac4876a2b2ff  (mtime 2026-10-30 08:40)
after  sha256 = 2dee2c52162f22b07556798c363eea77e2ecc58a9cdfd58be1f057264083df20  (mtime 2026-10-30 08:51)
cargo build --release → Finished in 1m 01s

③ systemctl restart cloudpivot → active（MainPID 1594120）
   /health → {"code":0,"msg":"ok","data":{"status":"ok","app":"cloudpivot","profile":"prod",…}}

④ 启动日志（数据权限自检仍然通过，登记表仍是 11 张）
   数据权限自检通过：11 张登记表的部门列/归属列均存在，SUPER_ADMIN 角色存在

⑤ 路由存在性（注意前缀是 /api）
   GET /api/business/permission/scope/uncovered-tables   未登录 → 401 {"code":401,…}   ✅ 不是 404
   真实登录 admin（/api/system/auth/login → 200）        带令牌 → 200 code=0
```

### 生产实测响应（节选）

```json
{"code":0,"msg":"ok","data":{
  "registered": 11, "wired": 11, "exempted": 0, "uncovered": 0,
  "uncovered_tables": [], "exempted_tables": [],
  "tables": [
    {"table":"sys_dept","dept_col":"id","owner_col":null,"wired":true,
     "status":"wired","no_paginate":true,
     "reason":"部门树不分页（无 {FILTER} 契约）：scope::filter_for(SYS_DEPT) 把部门行按范围裁剪（system/dept/model.rs:119）；写点走 guard::bump_epoch 失效"},
    …（其余 10 行同结构，reason 见 §三）
  ]}}
```

断言脚本（`/root/e1_verify.py`，真实登录后逐字段核对）输出：

```
registered = 11 / wired = 11 / exempted = 0 [] / uncovered = 0 []
uncovered == 0                     : True
tables 行数 == registered          : True
每行 status 合法 + reason 非空      : True
wired + exempted + uncovered == registered : True
EXIT=0
```

## 八、遗留与建议

- §四 #1（闸② 存量红 `task_bridge.rs`）建议尽快单开卡，否则 `scripts/check.sh` 会一直是红的；
- §四 #2（部门形态降级）是 P1 覆盖面问题，量级比本卡大，建议与架构 §2.5 一起排。
