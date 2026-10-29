# 批次 C · C8「种子与配置」验证证据

- 提交：`415d8c7 feat(batchC): C8 种子与配置 —— 变更日志权限 + announcement 存储策略 + 5 个 sys_config + 3 个帮助分类`
- 基线：`0ffd9a1`（C3 证据提交），`HEAD == origin/main == 415d8c7`
- 环境：`172.18.10.144`，PG `cloudpivot_test`，隔离实例 `127.0.0.1:18088`（`ALUN_REDIS_URL=.../9`，验收后已 kill）
- 改动：**只加 4 个迁移文件（507 行），零业务代码改动**

| 文件 | 内容 |
|---|---|
| `V0085__seed_changelog_permissions.{up,down}.sql` | 变更日志 3 码 + 公告管理 6 码 = 11 个菜单节点 + 授权 + 自检 |
| `V0086__seed_announcement_storage_and_configs.{up,down}.sql` | `announcement` 存储策略 + 5 个 `sys_config` + 3 个默认帮助分类 |

---

## 1. 启动迁移器应用（真实运行时）

```
2026-10-29T02:18:51.290083Z  WARN alun_db::migrate: 待应用迁移的版本低于已应用最大值（乱序补迁）；确认无误后可设 out_of_order = true 消除告警 migration=seed_changelog_permissions version=85 latest_applied=88
2026-10-29T02:18:51.290716Z  WARN alun_db::migrate: 待应用迁移的版本低于已应用最大值（乱序补迁）；确认无误后可设 out_of_order = true 消除告警 migration=seed_announcement_storage_and_configs version=86 latest_applied=88
2026-10-29T02:18:51.316319Z  INFO alun_web::app: 数据库迁移完成: 应用 2 个迁移
2026-10-29T02:18:51.342971Z  INFO alun_web::app: cloudpivot 启动 -> http://127.0.0.1:18088
```

「乱序补迁」告警是**预期**：V0087（C2 公告读）/ V0088（C4 帮助）在并卡交付中先占了号，
V0085/V0086 是收尾补种（原因写在两个迁移的文件头）。

重启实例（迁移记录已在，只校验 checksum）：

```
2026-10-29T02:21:32.143414Z  INFO alun_web::app: 数据库迁移完成: 应用 0 个迁移
2026-10-29T02:21:32.165963Z  INFO alun_web::app: cloudpivot 启动完成, 耗时 116ms
```

`schema_migrations`：

```
version=85 kind=versioned name=seed_changelog_permissions success=true checksum=92d0a90e1608
version=86 kind=versioned name=seed_announcement_storage_and_configs success=true checksum=ef1f60f4a222
```

---

## 2. 验收①：权限可见性（真机 HTTP，22 项断言全 PASS）

```
================================================================================
A. 权限可见性
================================================================================
[PASS] A-1 超管登录  http=200 code=0
[PASS] A-2 角色-菜单树可见本迁移 11 个节点  http=200 树内 business 节点=82 缺=无
[PASS] A-2b 变更日志节点字段（path/auth_code/component）  name=变更日志 auth=system:change-log:list path=/system/change-log component=system/change-log/index children=3
[PASS] A-2c 公告管理节点字段 + 6 个按钮子节点  name=公告管理 auth=business:announcement:list path=/business/announcement children=['business:announcement:list', 'business:announcement:create', 'business:announcement:update', 'business:announcement:delete', 'business:announcement:publish', 'business:announcement:offline']
[PASS] A-3a 取到 SUPER_ADMIN 角色 id  id=bfcb95a2-5a88-4010-9da1-b193a5b08e8e 角色数=11
[PASS] A-3b SUPER_ADMIN 角色菜单勾选含 11 个新节点  http=200 该角色勾选总数=85 缺=无
[PASS] A-4 TEST_VIEWER 无变更日志节点、有公告/帮助读入口  http=200 变更日志泄漏=无 读入口命中=4/4 勾选总数=28
[PASS] A-4b TEST_VIEWER 无公告管理节点（管理码不授业务角色）  公告管理泄漏=无
[PASS] A-5 建只读用户（TEST_VIEWER）  http=200 code=30010
[PASS] A-5b 只读用户登录  http=200 code=0
[PASS] A-6 权限集：无 system:change-log:*、无公告管理码  http=200 roles=['TEST_VIEWER'] 权限数=20 change-log 码=无 公告管理码=无
[PASS] A-6b 权限集：有 business:announcement:read 与 system:help:read  权限集全文=['business:impexp:export:cancel', 'system:preference:view', 'business:impexp:import:download', 'business:impexp:import:template', 'business:impexp:export:retry', 'business:storage:list', 'business:announcement:read', 'business:impexp:export:list', 'business:impexp:import:cancel', 'business:impexp:import:upload', 'business:impexp:import:retry', 'business:storage:download', 'business:impexp:export:create', 'business:impexp:export:download', 'business:notice:mine', 'business:impexp:import:confirm', 'business:impexp:def:list', 'business:impexp:import:list', 'system:help:read', 'business:storage:upload']
[PASS] A-6c 非超管自己的菜单树：无变更日志/公告管理，有公告中心/帮助中心  http=200 我的菜单节点=9 变更日志泄漏=无 公告管理在本树=False 公告中心=True 帮助中心=True
[PASS] A-6d 对照组：超管导航菜单含「变更日志」「公告管理」页  http=200 超管导航节点=30 变更日志页在=True 公告管理页在=True
       非超管导航 auth_code=['', 'business:announcement:read', 'business:impexp:export:list', 'business:impexp:import:list', 'business:impexp:import:upload', 'business:notice:mine', 'business:storage:list', 'system:help:read', 'system:preference:view']
[PASS] A-7 非超管能进「公告中心」  http=200 code=0 total=0
[PASS] A-8 非超管能进「帮助中心」  http=200 code=0 total=0
[PASS] A-9 非超管进不了「公告管理」→ 403  http=403 code=403 msg=无权限访问，需要权限: business:announcement:list
[PASS] A-10 非超管进不了「变更日志」（权限集无该码；路由层 C6 在途）  viewer http=404、admin http=404（C6 未落地则两端都无路由）、change-log 权限码=无、菜单节点存在于树=True
```

**关于 A-10 的诚实说明**：`GET /api/system/change-log/page` 目前**两端都 404**，
因为变更日志的查询接口属 C6（并行卡，`src/business/changelog` 此刻只有 `diff/mask/model/dto/mod`，
无 `handler.rs`，且 C6 在 `/opt/cloudpivot` 有在途改动尚未提交）。所以「非超管进不了变更日志」
在**当前代码基线**上由三层证据共同成立：

1. 登录后权限集（`/system/auth/me`）里没有 `system:change-log:*`；
2. 自己的导航菜单（`/system/auth/menus`，只返回目录+菜单）里没有「变更日志」页（超管有，对照组）；
3. 服务端根本没有该路由（C6 未落地），因此无任何入口可进。

C6 落地后应补一次「非超管 403 / 审计员 200」的回归（已在下方「遗留」列出）。

**`sys_menu` 落库原文**（11 个节点）：

```
20000000-0000-0000-0000-000000000710 | 公告管理 | type=2 | auth=business:announcement:list | path=/business/announcement | component=business/announcement/admin | visible=true
20000000-0000-0000-0000-000000000711 | business:announcement:list | type=3 | auth=business:announcement:list | path=- | component=- | visible=false
20000000-0000-0000-0000-000000000712 | business:announcement:create | type=3 | auth=business:announcement:create | path=- | component=- | visible=false
20000000-0000-0000-0000-000000000713 | business:announcement:update | type=3 | auth=business:announcement:update | path=- | component=- | visible=false
20000000-0000-0000-0000-000000000714 | business:announcement:delete | type=3 | auth=business:announcement:delete | path=- | component=- | visible=false
20000000-0000-0000-0000-000000000715 | business:announcement:publish | type=3 | auth=business:announcement:publish | path=- | component=- | visible=false
20000000-0000-0000-0000-000000000716 | business:announcement:offline | type=3 | auth=business:announcement:offline | path=- | component=- | visible=false
20000000-0000-0000-0000-000000000730 | 变更日志 | type=2 | auth=system:change-log:list | path=/system/change-log | component=system/change-log/index | visible=true
20000000-0000-0000-0000-000000000731 | system:change-log:list | type=3 | auth=system:change-log:list | path=- | component=- | visible=false
20000000-0000-0000-0000-000000000732 | system:change-log:export | type=3 | auth=system:change-log:export | path=- | component=- | visible=false
20000000-0000-0000-0000-000000000733 | system:change-log:clean | type=3 | auth=system:change-log:clean | path=- | component=- | visible=false
```

各角色持有数（本迁移 11 节点）：

```
A5_ROLE_1 | 0        A5_ROLE_2 | 0        A5_ROLE_3 | 0        A5_ROLE_4 | 0
A5_ROLE_5 | 0        B5_DEPT_CHILD | 0    B5_DEPT | 0         B5_SELF | 0
SUPER_ADMIN | 11     TEST_USER_ADMIN | 0  TEST_VIEWER | 0
```

---

## 3. 验收②③④：存储策略 / 配置项 / 默认分类

### 验收② `sys_storage_policy`（`category='announcement'`）

```
category            | announcement
category_name       | 公告附件
max_size_mb         | 50
allowed_extensions  | jpg,jpeg,png,pdf,docx,xlsx,zip
allowed_mimes       | （空 ⇒ 不校验 MIME）
direct_upload       | t
allow_overwrite     | f
status              | 1
sort_order          | 60
remark              | 公告附件：50 MB 上限；白名单 jpg/jpeg/png/pdf/docx/xlsx/zip；可执行扩展名（exe/sh/bat/js/vbs/msi/ps1/jar/so/dll…）一律不在白名单内，等同禁用；MIME 不校验（allowed_mimes 为空）——docx/xlsx 常被客户端报成 octet-stream
```

**「禁可执行」怎么落地的**：表里没有黑名单列，只有扩展名白名单。白名单语义天然
fail-closed（不在表内一律拒绝），所以「禁可执行」= 白名单不含任何可执行扩展名；
迁移里的 `DO $$ … $$` 自检会逐个断言白名单项不在
`exe/sh/bat/cmd/com/scr/msi/js/vbs/ps1/jar/dll/so/…` 黑名单里且全小写无点，
将来谁手工往白名单塞 `exe`，下一次 up 重放会直接炸在迁移期。

**真实上传验证**（不只是查表）：

```
[PASS] B-1 策略页可查 announcement 行（50MB / jpg,jpeg,png,pdf,docx,xlsx,zip）  http=200 row={"id": "24000000-0000-4000-8000-000000000006", "category": "announcement", ... "max_size_mb": 50, "allowed_extensions": "jpg,jpeg,png,pdf,docx,xlsx,zip", "allowed_mimes": "", ...}
[PASS] B-2 上传 evil.exe → 400 + 类型不允许（禁可执行）  http=400 code=60004 msg=不支持的文件类型 .exe，允许的类型：jpg,jpeg,png,pdf,docx,xlsx,zip
[PASS] B-3 上传 c8-ok.png → 200 且落在 announcement 分类  http=200 code=0 id=01a18576-06b4-70c6-8a1e-bac444c7a454 size=70 category=announcement
[PASS] B-3b 回收测试文件（软删）  http=200 code=0
```

### 验收③ `sys_config` 5 个键

```
announcement.default_expire_days = 30  [NUMBER]  cat=announcement  sort=10
announcement.fanout_enabled = true  [BOOLEAN]  cat=announcement  sort=20
announcement.scope_refresh_on_dept_change = false  [BOOLEAN]  cat=announcement  sort=30
changelog.retention_months = 36  [NUMBER]  cat=changelog  sort=40
changelog.big_field_bytes = 2048  [NUMBER]  cat=changelog  sort=50
```

键名与默认值逐字照架构 §5.3 的「V0086 的 sys_config 参数」表。
`config_type` 用 `BOOLEAN/NUMBER`（`support/config/model.rs::is_valid_config_type` 的白名单口径），
**不沿用**历史种子 `S__config_data.sql` 里的 `BOOL`（那是 `shared/enums.rs` 记录的
「三重不一致」历史债；改历史行超出本卡「只加种子」的红线，故只在迁移头说明）。

### 验收④ `sys_help_category` 3 条

```
getting-started | 快速开始 | parent=NULL | sort=10 | status=1
faq             | 常见问题 | parent=NULL | sort=20 | status=1
features        | 功能说明 | parent=NULL | sort=30 | status=1
```

---

## 4. 验收⑤：迁移 `up → down → up` 可重放（+ 幂等复跑）

```
########## 0) 基线（V0085/V0086 已由启动迁移器应用） ##########
  菜单节点(710-716,730-733) = 11
  授权行(同段)               = 11
  announcement 策略行        = 1
  5 个 sys_config            = 5
  3 个帮助分类               = 3
########## 1) down：V0086 → V0085 ##########
  菜单节点(710-716,730-733) = 0
  授权行(同段)               = 0
  announcement 策略行        = 0
  5 个 sys_config            = 0
  3 个帮助分类               = 0
########## 2) up：V0085 → V0086 ##########
  菜单节点(710-716,730-733) = 11
  授权行(同段)               = 11
  announcement 策略行        = 1
  5 个 sys_config            = 5
  3 个帮助分类               = 3
########## 3) 幂等：再跑一次 up ##########
  菜单节点(710-716,730-733) = 11
  授权行(同段)               = 11
  announcement 策略行        = 1
  5 个 sys_config            = 5
  3 个帮助分类               = 3
```

down 用**应用账号**执行（不是 `su - postgres`），与启动迁移器同口径，避免表 owner 变 postgres。

重启后 `应用 0 个迁移` + 4 项数据仍在（见 §1）。

---

## 5. 验收⑥：`cargo test` 不回归

共享工作区 `/opt/cloudpivot` 此刻有 **C6 的在途改动**（`business/changelog/{handler,query,partition}.rs`、
`business/shared/conn.rs` 等 8 个文件未提交，`cargo test` 在其中会因 C6 自己的
`changelog/handler.rs:304 expected String, found &str` 编不过——与本卡无关）。
因此按团队既有打法在**隔离 clone**（`/root/c8-build` @ `0ffd9a1` + 本卡 4 个 SQL）验证：

```
export CARGO_TARGET_DIR=/root/c4-target; cargo test -j 1
（首次 -j 4 时 ld 被 OOM killer 杀掉：机器 7 GB 内存，4 路并行链接扛不住，改 -j 1 后通过）

test result: ok. 483 passed; 0 failed; 0 ignored  (lib)
套件数= 10  通过合计= 514
exit=0
```

与 C3 证据里的基线（483 lib / 514 合计）**完全一致**，无回归。

---

## 6. 权限码「三方一致性」比对

口径：`docs/批次C-技术架构.md §5.1` 的 20 码。后端 = `backend/src/business/**` 的
`#[alun::permission(permission="…")]`；种子 = `sys_menu.auth_code`；前端 =
`frontend/src/constants/permissions.ts`。

```
后端注解码总数=63  sys_menu 码总数=109  前端字典码总数=46

business:announcement:list                 有  有  无   前后端 + 种子一致；前端字典待 C9
business:announcement:create               有  有  无   前后端 + 种子一致；前端字典待 C9
business:announcement:update               有  有  无   前后端 + 种子一致；前端字典待 C9
business:announcement:delete               有  有  无   前后端 + 种子一致；前端字典待 C9
business:announcement:publish              有  有  无   前后端 + 种子一致；前端字典待 C9
business:announcement:offline              有  有  无   前后端 + 种子一致；前端字典待 C9
business:announcement:read                 有  有  无   前后端 + 种子一致；前端字典待 C9
system:help:category:list                  有  有  无   前后端 + 种子一致；前端字典待 C9
system:help:category:create                有  有  无   前后端 + 种子一致；前端字典待 C9
system:help:category:update                有  有  无   前后端 + 种子一致；前端字典待 C9
system:help:category:delete                有  有  无   前后端 + 种子一致；前端字典待 C9
system:help:doc:list                       有  有  无   前后端 + 种子一致；前端字典待 C9
system:help:doc:create                     有  有  无   前后端 + 种子一致；前端字典待 C9
system:help:doc:update                     有  有  无   前后端 + 种子一致；前端字典待 C9
system:help:doc:delete                     有  有  无   前后端 + 种子一致；前端字典待 C9
system:help:doc:publish                    有  有  无   前后端 + 种子一致；前端字典待 C9
system:help:read                           有  有  无   前后端 + 种子一致；前端字典待 C9
system:change-log:list                     无  有  无   仅种子有（后端注解待 C6 落地）
system:change-log:export                   无  有  无   仅种子有（后端注解待 C6 落地）
system:change-log:clean                    无  有  无   仅种子有（后端注解待 C6 落地）

种子缺口（必须为 0）：无
sys_menu 里批次 C 段多出的码（应为空）：无
```

结论：
- **种子侧 20 码零缺口、零多余**（本卡验收要求）；
- 17 码「后端注解 + 种子」双向一致，前端字典是 C9 的活（`permissions.ts` 目前只到批次 B）；
- 3 个 `system:change-log:*` 的后端注解要等 C6 的 `handler.rs` 落地（此刻模块里没有 handler）。

---

## 7. 收尾

```
kill 758588
18088 已释放 ✔
无 18088 残留进程 ✔
c8-viewer 清理后残留：0
sys_file(c8-ok.png) 行数=3 已软删=3
菜单节点=11  policy=1  config=5  help_cat=3   ← 清理不影响种子
```

工作区：`/opt/cloudpivot` 只提交了本卡 4 个 SQL；C6 的 8 个在途文件原样保留未动。
`git rev-parse HEAD origin/main` → 两者均为 `415d8c78bf9536c591c2368914dffc922fbb8ac5`。

## 8. 遗留（不在本卡范围，已写入交接）

1. **C6**：`system:change-log:*` 的后端注解要与其 `handler.rs` 一起落地；落地后补一次
   「非超管 403 / AUDITOR 200 / 超管 200」的回归（当前该路由两端 404）。
2. **C9**：`frontend/src/constants/permissions.ts` 补批次 C 段（20 码）；
   公告管理页组件路径本卡种为 `business/announcement/admin`（`business/announcement/index`
   已被 V0087 的公告中心占用），C9 若换文件名只需改 `sys_menu` 一行。
