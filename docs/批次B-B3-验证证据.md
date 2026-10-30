# 批次 B · B3「底座接入」验证证据

> 交付步：`B3 · 底座接入（报批项 #3–#11）`（架构文档 §7-B3 / §5.3 / §2.4 / §2.6）
> 验证日期：2026-10-28（CST）
> 验证机：`172.18.10.144`（`/opt/cloudpivot`，rustc 1.99.1 / edition 2024）
> 验收实例：`127.0.0.1:8099`，库 `cloudpivot_b3v`（专用库，新建后跑满 **43** 个迁移，
> 含 B1 的 V0060–V0064/V0067 与 B4 的 V0065），Redis db6，`NIK_CACHE_TYPE=redis`
> 门禁：闸① **20 → 19** 处、闸② **21 → 18** 处（下降部分逐条列在 §4，门禁脚本一行未改）
> 单测：**305 lib passed / 0 failed**（B2 基线 289 + B4 10 + 本卡 6）+ 集成测试 1/7/2 全绿

---

## 1. 交付物（逐项对应 §5.3 报批清单）

| # | 文件 | diff | 内容 |
|---|---|---|---|
| #3 | `backend/src/system/role/model.rs` | +50 | `SysRoleModel.data_scope: i16`（注释说明「不加这一列 → 整行 update 会写坏范围」）+ `find_role_holder_ids()`（范围缓存失效的持有者反查） |
| #4 | `backend/src/system/role/dto.rs` | +19 | `RoleRes.data_scope: i16`（**必下发**，前端回显 / §5.6 R5）+ `RoleSaveReq.data_scope: Option<i16>`（**`None` = 保留现值**，架构点名的坑） |
| #5 | `backend/src/system/role/handler.rs` | +455 | `create`/`update` 透传 `data_scope`；`resolve_data_scope()`（唯一转换口）；`scope_class()` / `scope_width()` + `ensure_data_scope_grantable[_with]()`（范围上限，403/61002）；`data_scope` 变更后 `invalidate_role_scope_cache()`；6 个单测 |
| #7 | `backend/src/system/user/model.rs` + `handler.rs` | +80 | `page_users` 改走 `scope::paginate`（签名加 `&ScopedPrincipal`，`{FILTER}` 标记 + `ORDER BY` 独立传参）；`handler::page` 解析 principal；`update`（换部门）/`batch_update_dept`/`assign_roles` 补 `invalidate_user_scope(s)` |
| #8 | `backend/src/system/dept/model.rs` + `handler.rs` | +94 | `find_dept_list` 按 `scope::filter_for(SYS_DEPT)` 裁剪（主体表统一别名 `d`）；`tree` 裁剪 + **不可见上级提升为根**；全量树缓存**只在 `is_noop()` 时读写**；create/update/delete/migrate 4 个写点各加 `bump_epoch()` |
| #9 | `backend/src/bootstrap.rs` | +56 | `tokio::join!` 第 5 分支 `preflight_permission_guard()`：prod 自检失败 `exit(1)`，其它 profile 记 error/warn 继续 |
| #10 | `backend/src/shared/error_code.rs` | +64 | 6100x 段补满 **61005–61012**（61001/61002 由 B2、61003/61004 由 B4 落地，本卡与 B4 的措辞合并保留）+ 码值稳定性单测 |
| #11 | `backend/src/shared/constants.rs` | +20 | `CACHE_KEY_PREFIX_USER_SCOPE` / `CACHE_KEY_USER_SCOPE_EPOCH` / `CACHE_KEY_PREFIX_USER_MASK`；`business/shared/principal.rs` 改为 `pub use` 转出（键常量集中一处） |
| 报批尾项 | `backend/Cargo.toml` + `Cargo.lock` | +6 / +1 | 显式声明 `regex = "1"`（1.13.1 本就在依赖树里，`nik-utils` 的依赖；**不引入新 crate**，与 `tokio-util`/`futures-util`/`redis` 三个先例同口径） |

**未动**：`business/` 下任何业务模块（storage/impexp/notice 是 B5）、`sys_field_mask` 相关、
`sys_role_mutex` 相关、`scripts/check.sh` 门禁脚本、前端。

### 关于 §5.3-#6（`assign_roles` 插 SoD 校验）——**有意不在本卡做**

- #6 的内容是 `sod::ensure_no_mutex(&id, &role_ids)`，而 `business/permission/sod/` 由 **B7** 创建（依赖图 `B3 → B7`）；
- 本卡做的是 #6 的**前置**：在 `assign_roles` 的既有失效点补上数据范围缓存失效（`scope_guard::invalidate_user_scope(&id)`）；
- B7 落地时只需在该函数里加 **1 行** SoD 调用，不必再动缓存逻辑。

### 关于与 B4 的并发提交（rebase 记录）

本卡开发期间 B4 并发落地并先推到 `main`（`05ef6f4`：`business/permission/` + V0065 + error_code 61003/61004）。
本卡提交已 rebase 到 B4 之上，**唯一冲突**在 `backend/src/shared/error_code.rs`（两卡都加了 61003/61004）：

- 保留 B4 的措辞与常量定义（61003 = 自己持有角色的范围锁定、61004 = 部门不存在/已软删）；
- 本卡补齐 61005–61012 与码值稳定性单测；
- 段头注释合并为「B2 用 61001/61002，B4 补 61003/61004，B3 补满 61005–61012，B6/B7 直接使用」；
- rebase 后重跑 fmt / clippy / `cargo test`（**305 passed**）与全部真机验收，结果与 rebase 前一致。

B4 的「部门树裁剪」实现在 `business/permission/scope_model.rs`（配置页面用的部门树），
与本卡改的 `system/dept`（**人员/权限体系里真正的部门树接口**）不冲突、不重复：两者共用同一份
`principal` 与 `scope::filter_for`，裁剪语义一致（都做「父不可见则提升为根」）。

---

## 2. 复现命令

```bash
# ① 静态检查 + 单测（顺序固定）
cd /opt/cloudpivot/backend
cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo test

# ② 起验收实例（独立运行目录 + 独立 target，避免与主 target / 其它卡串用）
su postgres -c "createdb -O cloudpivot cloudpivot_b3v"          # 只需一次
CARGO_TARGET_DIR=/var/tmp/b3-target cargo build
setsid nohup scripts/batchb/b3_run.sh > /var/tmp/b3-run.log 2>&1 &
#   脚本做的事：拷二进制到 /var/tmp/b3-run、按白名单逐 key 读 cloudpivot-test.env、
#   覆盖 NIK_DATABASE_NAME=cloudpivot_b3v / NIK_SERVER_LISTEN=127.0.0.1:8099 /
#   NIK_CACHE_TYPE=redis / NIK_REDIS_URL=redis://127.0.0.1:6379/6

# ③ 夹具 + 逐条验收（脚本随本卡提交：scripts/batchb/b3_accept.py）
python3 scripts/batchb/b3_accept.py all        # = seed + verify②③④⑤⑥⑦

# ④ 门禁（口径与 scripts/check.sh 的 batchb_gates 逐字相同）
cd /opt/cloudpivot/backend
grep -rn paginate_with_params src --include=*.rs | grep -v business/shared/scope.rs \
  | grep -v -E "^[^:]+:[0-9]+: *(//|\*)" | wc -l     # → 19
for t in sys_user sys_dept sys_import_task sys_export_task sys_notice sys_file \
         operation_logs audit_logs auth_logs security_logs; do
  for f in $(grep -rl "FROM $t" src --include=*.rs); do
    grep -q -e "scope::" -e "SCOPE-EXEMPT" "$f" || echo "FAIL: ${f#src/} 查询 $t"
  done
done | wc -l                                          # → 18
```

### 夹具（`b3_accept.py seed`，幂等；已存在的用户会被重置为脚本推导的口令）

```
B3-HQ(B3 总部) ├── B3-RD(B3 研发部) ──── B3-RD-BE(B3 后端组)
               └── B3-SALES(B3 销售部)

角色：B3_ALL(data_scope=1)  B3_DEPT(=2)  B3_CHILD(=3)  B3_SELF(=4)
用户：b3_mgr     → B3-RD    / B3_DEPT     （部门主管，验收②④的主角）
      b3_childmgr → B3-RD   / B3_CHILD    （验收⑤，看研发部及以下）
      b3_dev1 / b3_selfonly → B3-RD / B3_SELF
      b3_be1 / b3_be2 → B3-RD-BE / B3_SELF
      b3_sales1 → B3-SALES / B3_SELF      （验收⑤里被搬迁的部门下的人）
      b3_hq1    → B3-HQ / B3_SELF
```

> 口令不落盘：脚本从 `/etc/cloudpivot/cloudpivot-test.env` 的 `NIK_ADMIN_PASSWORD` 推导
> （`<初始>-b3admin` / `<初始>-b3user`），避免验收脚本本身成为仓库里的凭据明文。

---

## 3. 验收逐条证据（`b3_accept.py all` 的原始输出，未改写）

### ③ 前置：`cargo test` 全绿（含既有 289 个，一个不许少）

```
$ cd /opt/cloudpivot/backend && cargo test
running 305 tests
test result: ok. 305 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
running 0 tests   → ok. 0 passed
running 1 test    → ok. 1 passed        (tests/batchB_b2_scope.rs，B2 真库用例未回归)
running 7 tests   → ok. 7 passed        (tests/impexp_worker.rs)
running 2 tests   → ok. 2 passed        (tests/s3_minio.rs)
doc-tests         → 5 ignored
```

其中本卡新增 6 个：`shared::error_code::tests::batch_b_error_codes_are_stable`、
`system::role::handler::tests::{data_scope_resolution_keeps_fallback, scope_class_ordering,
scope_width_ordering, non_super_cannot_escalate_scope, invalid_scope_rejected_by_grant_check}`。
`cargo fmt -- --check` 干净、`cargo clippy --all-targets -- -D warnings` 零告警。

### ② 部门主管（`data_scope=2`）登录 → `GET /api/system/user/page` 只回本部门行

```
==============================================================================
验收 ②：部门主管（data_scope=2）登录 → GET /api/system/user/page 只回本部门行
==============================================================================
  token(b3_mgr) = eyJ0eX...iOiI…（长度 551）
  GET /system/user/page: HTTP 200 code=0 total=4 rows=4
    部门分布: ['B3 研发部']
    用户名: ['b3_childmgr', 'b3_dev1', 'b3_mgr', 'b3_selfonly']
  DB 侧部门人数: B3 后端组=2; B3 总部=1; B3 研发部=4; B3 销售部=1
  ✅ 只回「B3 研发部」的 4 行（b3_mgr/b3_dev1/b3_childmgr/b3_selfonly），无后端组/销售部/总部行
```

**令牌声明**（`jti`/`sub`/权限逐字可见；签名段由运行环境的密钥打码策略遮蔽，不可用作凭据）：

```json
{"jti":"a5298d19-8154-4082-af74-73325a69d457",
 "sub":"01a1834f-6e41-759e-8dcf-0015d94b3f37",
 "username":"b3_mgr",
 "roles":["B3_DEPT"],
 "permissions":["system:dept:list","system:role:list","system:role:update","system:user:list"],
 "token_type":"access","exp":1793205291,"iat":1793204391,"ver":0,
 "sid":"ce013910-4ed4-46c9-bb73-7a99ce3dbc41"}
```

即：**该账号只有 4 个权限点、角色 `B3_DEPT`（`data_scope=2`），`total=4` 恰为研发部在册用户数；
后端组（2 人）/销售部（1 人）/总部（1 人）与 admin 全部不可见。**

### ③ 改角色名后 `data_scope` 不变（改前改后各查一次 DB）

```
==============================================================================
验收 ③：改角色名后 data_scope 不变（PUT 不带 data_scope 字段，模拟老前端）
==============================================================================
  改前 DB: id=01a1834f-6d13-726d-834b-2c0d28742abf role_name=B3 本部门（验收②） data_scope=2
  PUT /system/role/01a1834f-6d13-726d-834b-2c0d28742abf  body 里**没有** data_scope 字段 → HTTP 200 code=0
  响应体 data_scope = 2
  改后 DB: id=01a1834f-6d13-726d-834b-2c0d28742abf role_name=B3 本部门（改名验证） data_scope=2
  ✅ 角色名 B3 本部门（验收②） → B3 本部门（改名验证），data_scope 仍为 2（未被重置为默认 1）
  复原角色名 → HTTP 200 code=0 data_scope=2
```

请求体（刻意**不带** `data_scope`，模拟老前端）：

```json
{"role_code":"B3_DEPT","role_name":"B3 本部门（改名验证）","description":"",
 "status":1,"sort_order":0,"remark":""}
```

实现要点：`update` 里 `resolve_data_scope(req.data_scope, existing.data_scope)` —— `None` 取**库中现值**，
不是取默认值 `1`；单测 `data_scope_resolution_keeps_fallback` 把这条钉住。

### ④ 越权用例：把范围改成超出自身可见集合 → **403 / 61002**

```
==============================================================================
验收 ④：越权用例 —— 部门主管把自己的角色范围改成超出自身可见集合 → 403 / 61002
==============================================================================
  预检 b3_mgr 有 system:role:list → GET /system/role/page HTTP 200 code=0
  PUT data_scope=1（全部数据 ALL（远超其「本部门」可见集合））→ HTTP 403 code=61002 msg=不能把数据范围设为「全部数据」：超出你自身的可见范围（当前为「本部门」）
  PUT data_scope=3（本部门及以下 DEPT_AND_CHILD（对持有者本人就是放宽））→ HTTP 403 code=61002 msg=不能把数据范围从「本部门」调整为「本部门及以下」：非超管只能收紧或保持不变（放宽由超管执行）
  DB 复核：B3_DEPT.data_scope = 2（未被越权写入）
  对照：收紧为 data_scope=4（仅本人）→ HTTP 200 code=0 data_scope=4
  超管复原 data_scope=2 → HTTP 200 data_scope=2
  ✅ 越权（放宽）403/61002 且库未变；收紧允许；超管不受限
```

**这里比架构文档多做了一档判定（详见 §5-①）**：除「不得越过自身可见范围」外，
还禁止**非超管把任何角色的范围放宽**（含同级的 `DEPT → DEPT_AND_CHILD`）。

### ⑤ `parent_id` 变更后另一位用户的列表在 **1 次请求内**变为新范围（epoch 生效）

```
==============================================================================
验收 ⑤：部门 parent_id 变更后，另一位用户的列表在 1 次请求内变为新范围（epoch 生效）
==============================================================================
  b3_childmgr 范围=3（本部门及以下，部门=B3 研发部）
  ① 变更前 GET /system/user/page: HTTP 200 code=0 total=6 rows=6
    部门分布: ['B3 后端组', 'B3 研发部']
    用户名: ['b3_be1', 'b3_be2', 'b3_childmgr', 'b3_dev1', 'b3_mgr', 'b3_selfonly']
  Redis: epoch=4  perm:user_scope:* 键数=6                       ← 缓存已预热（前提成立）
  搬迁前 B3-SALES.parent_id = 01a1834f-6ceb-70d6-811f-23bd54f33192
  PUT /system/dept/01a1834f-6cfb-76ea-8cc6-485290c9ed2a parent_id → B3 后端组：HTTP 200 code=0
  Redis: epoch=5（+1 = bump_epoch 已执行）
  ② 变更后第一次 GET（同一 token，未等待 TTL）: HTTP 200 code=0 total=7 rows=7
    部门分布: ['B3 后端组', 'B3 研发部', 'B3 销售部']
    用户名: ['b3_be1', 'b3_be2', 'b3_childmgr', 'b3_dev1', 'b3_mgr', 'b3_sales1', 'b3_selfonly']
  新增可见：['b3_sales1']；失去可见：[]
  ✅ parent_id 变更后，同一 token 的下一次请求即包含新子部门（销售部 1 人）——epoch 全量失效生效
  复原 parent_id → B3 总部：HTTP 200 code=0；epoch=6
```

关键点：**同一个 token、没有重新登录、没有等 TTL**，第 6 行到第 7 行是 `bump_epoch()` 的
`INCR cloudpivot:perm:user_scope_epoch`（`4 → 5`）换掉了缓存键的 `{epoch}` 段，旧键即刻不可达。
（`epoch` 从 4 起是因为灌夹具时 4 次部门写操作各 `INCR` 一次。）

> ⚠️ **与架构文档的偏差（实测产物）**：文档写键为 `perm:user_scope:…`，实际框架会在所有缓存键前
> 再加应用前缀 `cloudpivot:`（`alun_cache` 用 `cfg().app_name` 做命名空间）。运维排查 / 手工清缓存
> 必须用 `cloudpivot:perm:user_scope:*`，否则 `--scan` 一条都扫不到。

### ⑥ 清空 / 停掉 Redis 后列表仍正确

```
==============================================================================
验收 ⑥：清空 Redis 缓存后列表仍正确（回退查库）
==============================================================================
  ① 清空前的列表（应有 scope 缓存）: HTTP 200 code=0 total=4 rows=4
    部门分布: ['B3 研发部']
    用户名: ['b3_childmgr', 'b3_dev1', 'b3_mgr', 'b3_selfonly']
  Redis cloudpivot:perm:user_scope:* 键数 = 8：['cloudpivot:perm:user_scope:4:01a1834f-747e-…',
      '…:6:01a1834f-6e41-…','…:1:01a1834f-0cd0-…','…:3:01a1834f-0cd0-…','…:4:01a1834f-0cd0-…',
      '…:5:01a1834f-747e-…','…:2:01a1834f-0cd0-…','…:0:01a1834f-0cd0-…']
  逐个 DEL scope 键 → ['8']
  再 DEL epoch 键 → 1
  清空后 cloudpivot:perm:user_scope* 键数 = 0
  ② scope 缓存清空后同一 token 的列表: HTTP 200 code=0 total=4 rows=4
    部门分布: ['B3 研发部']
    用户名: ['b3_childmgr', 'b3_dev1', 'b3_mgr', 'b3_selfonly']
  行数 清空前=4 清空后=4
  请求后回填的 scope 键数 = 1：['cloudpivot:perm:user_scope:0:01a1834f-6e41-…']
  ✅ scope 缓存清空后仍回本部门 4 行（cache-aside 未命中 → 查库重算 → 回填）

  redis-cli -n 6 FLUSHDB → OK
  清空后 db6 dbsize = 0
  ③ 整库清空后同一 token 的列表: HTTP 200 code=0 total=4 rows=4
    部门分布: ['B3 研发部']
    用户名: ['b3_childmgr', 'b3_dev1', 'b3_mgr', 'b3_selfonly']
  ✅ Redis 整库清空后列表仍为同 4 行（数据权限不依赖缓存在不在）
```

**关于「停掉 Redis」这一形态的诚实说明**：本栈在 Redis 不可用时，请求会先在
`middleware::auth_guard`（项目自建的 fail-closed 复检；其模块头注释：*「查询失败 / 缓存缺失 /
令牌无 jti → 401」*）被拒 —— 也就是说 **Redis 停掉时任何受保护接口都返回 401，与数据权限无关**。
这是既有取向（吊销状态就在缓存里，缺了就是配置错误），本卡不该也不能改。
因此本条取「**缓存数据全丢**」（`FLUSHDB`）与「**scope 缓存整族删除**」两种口径：
两者都逼数据权限层回源查库，结果仍逐行正确 —— 这才是「数据权限不依赖缓存」的可验证命题。

### 交付物 3 的补充验收（部门树裁剪 / 越权过滤 / `RoleRes` 回显）

```
==============================================================================
验收 ⑦（交付物 3）：部门树按 scope 裁剪 + 越权部门过滤不穿透
==============================================================================
  超管（范围=ALL，走全量树缓存）: ['B3 总部', '  B3 研发部', '    B3 后端组', '  B3 销售部']
  b3_mgr（范围=本部门，按 scope 裁剪）: ['B3 研发部']
  ✅ 超管看到 4 个部门；部门主管只看到「B3 研发部」一行，且被提升为树根（上级不可见）
  b3_mgr 带 dept_id=01a1834f…（销售部，看不见）: HTTP 200 code=0 total=0 rows=0
    部门分布: []
    用户名: []
  ✅ 显式传越权部门 id → total=0（scope 片段与业务条件是 AND，过滤条件不能绕过范围）
  GET /system/role/page → role_code=B3_DEPT data_scope=2（RoleRes 已下发该字段）
```

### 闸 3 启动自检（`bootstrap` 接线）——正反两面

正常启动（`/var/tmp/b3-run.log`）：

```
INFO cloudpivot::business::shared::guard: 数据权限自检通过：10 张登记表的部门列/归属列均存在，SUPER_ADMIN 角色存在
```

把 `sys_file.uploaded_by` 故意改名后重启（模拟「后续批次改了列名」）：

```
ERROR cloudpivot::business::shared::guard: 数据权限自检失败：登记表 10 张，缺失项 1 个（sys_file.uploaded_by）
 WARN cloudpivot::bootstrap: 数据权限自检不通过（非 prod，继续启动）：数据权限自检失败：…
```

即：自检**真的会失败**（不是永远打印「通过」的哑门禁）；非 prod 记 WARN 继续（便于施工），
prod 路径为 `eprintln!("FATAL: …") + exit(1)`（与 `bootstrap::ensure_admin_user` 缺初始口令的
既有 `exit(1)` 同口径，见 `preflight_permission_guard`）。还原列名后自检恢复「通过」。

---

## 4. 门禁数字：20 → 19 / 21 → 18（逐条说明为什么不更多）

**闸①（`paginate_with_params` 必须走 `scope::paginate`）**

- 消失的 1 处：`system/user/model.rs` → 改为 `scope::paginate`（本卡 §5.3-#7）。
- 仍剩 19 处：`system/role/model.rs` 2 处属 **B5**（附录 C #7/#8）；其余
  `business/{impexp,notice,storage}`、`support/{config,dict,file,job,log}` 全是 **B5** 的接入点。
  本卡按红线「不动 `business/`」，因此**不应**再降。

**闸②（登记表查询必须 `scope::` 或 `SCOPE-EXEMPT`）**

- 消失的 4 条（file×table）：`system/user/model.rs`（`sys_user`）、`system/dept/model.rs`
  （`sys_user` + `sys_dept`）、`system/role/model.rs`（`sys_user`）——前三个是真接入。
- **`system/role/model.rs` 那条是「用注释蹭过门禁」的反例，本卡刻意把它还原成 FAIL**：
  第一版我在 `SysRoleModel.data_scope` 的文档注释里写了 `` `scope::ScopeKind::from_i16` ``，
  `grep "scope::"` 因此把整个文件算成「已接入」——但该文件的 `page_role_users` **并没有**注入
  scope（附录 C #7，属 B5）。门禁被一句文档注释骗过，等于门禁哑了。已改写注释措辞，
  让它老老实实算 18 而不是 17。
- 仍剩 18 处：`business/{notice,impexp,storage}`、`support/{dashboard,log,file}`、`system/auth`、
  `system/role` —— 全是 B5 的接入点与 B5 才写的 `SCOPE-EXEMPT` 豁免行。

> 门禁脚本 `scripts/check.sh` 一行未改（红线）。

---

## 5. 本卡遇到的坑与做出的决策（架构文档没写到、实施时必须定的）

① **`DEPT → DEPT_AND_CHILD` 不是「同级」，而是自我扩权**。
架构 §2.7 只说「新范围的部门集合必须 ⊆ 操作者自己的可见部门集合」。但 B3 阶段没有
`custom_dept_ids` 入参，部门类范围的最终集合取决于**持有者**所在部门，无法静态判定。
第一版按「等级比较」（SELF < 部门类 < ALL）实现，结果 `b3_mgr` 把自己角色从「本部门」改成
「本部门及以下」**被放行** —— 他立刻能看到原先看不见的后端组用户。实测发现后改成两条判定：
`scope_class`（对操作者自身：3 档）+ `scope_width`（对角色现值：4 档，
`SELF < DEPT < CUSTOM≈DEPT_AND_CHILD < ALL`），**非超管不得放宽任何角色**（只能收紧或不变）。
B4 的专用接口已按「集合包含」做了更细的一层，两者叠加，本处结论不会放宽。

② **部门树裁剪后必须把「上级不可见」的节点提升为根**。
`utils::build_tree` 按「父节点存在」挂载，不可见的父节点会让整棵子树**静默消失** ——
`data_scope=2` 的主管会看到一棵空树。已在 `tree` 里先算可见 id 集合，把 `parent_id`
指向不可见父节点的节点置为根（语义：可见范围外的上层结构对本用户不存在）。

③ **全量部门树缓存只在 `filter_for(...).is_noop()` 时读写**。
`CACHE_KEY_DEPT_TREE` 是全局键，一个按范围裁剪过的树写进去就是「把别人的范围发给所有人」。
现在：超管 / `ALL` 范围照旧读写缓存（行为不变），带范围的树既不读也不写。

④ **缓存键前面还有应用命名空间**：真实键是 `cloudpivot:perm:user_scope:{epoch}:{uid}`（见 §3-⑤）。
写运维手册 / 排查脚本时用错前缀会「一条都扫不到」。

⑤ **`config-test.toml` 没有 `[cache]` 段 → 默认 `local`**，第一次起验收实例时
`cache_type=local`，`perm:user_scope:*` 一条都不在 Redis 里，验收⑤的前提直接不成立。
起实例必须显式 `NIK_CACHE_TYPE=redis`（`nik-config/src/env.rs` 的映射表只认
`NIK_CACHE_TYPE` / `NIK_REDIS_URL`，`NIK_DATABASE_PORT` 之类并不在表里）。

⑥ **角色写操作会 `force_logout_all_except(["admin"])`**（既有语义）。验收脚本因此每段都重新登录；
也意味着「验收③改角色名」之后 `b3_mgr` 的旧 token 会 401 —— 不是数据权限问题，别误判。

⑦ **`batch_update_dept` 原本没有任何缓存失效**（连 `invalidate_user_principal` 都没有，属既有缺口，
架构 §2.8 失效矩阵里写的是「已有」）。本卡只补数据范围缓存那一层（`invalidate_user_scopes`），
权限快照那一层留给后续卡，避免在 B3 顺手改出行为差异。

---

## 6. 同步核对

```
$ cd /opt/cloudpivot && git rev-parse HEAD
$ git rev-parse origin/main
$ git --git-dir=/opt/git/cloudpivot.git rev-parse main
（三者一致，见 kanban 卡的 completion metadata）
$ git status --short          # 空
```
