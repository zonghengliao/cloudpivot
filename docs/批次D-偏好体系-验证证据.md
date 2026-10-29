# 批次 D · 用户偏好体系落库 —— 验证证据

> 卡片：`【偏好体系】通用KV+字典表落库：配色/明暗/侧边栏/表格密度/分页/语言/通知（V0090+V0091）`
> 交付形态：**通用 KV 主表 + 字典元数据表**（新增偏好 = 往字典插一行，不改表结构、不改后端代码）
> 验证机：`172.18.10.144`（Ubuntu 26.04 / PG 18 / rustc 1.99.1 / node 24）

## 0. 交付清单

| 文件 | 行数 | 说明 |
|------|------|------|
| `backend/migrations/system/V0090__create_sys_user_preference.{up,down}.sql` | 62 / 3 | 两张表 + 注释 + 3 条约束 |
| `backend/migrations/system/V0091__seed_preference_def.{up,down}.sql` | 38 / 12 | 8 项预置字典（`ON CONFLICT DO UPDATE` 幂等） |
| `backend/src/system/preference/mod.rs` | 44 | 模块文档（两级结构 / 加偏好的步骤 / 数据权限口径） |
| `backend/src/system/preference/dto.rs` | 184 | 两代请求形态（扁平三字段 / `items` KV）+ 响应体 |
| `backend/src/system/preference/model.rs` | 726 | 字典与用户值读写 + **唯一**校验实现（`oneof` / `range` / 类型） |
| `backend/src/system/preference/handler.rs` | 123 | `GET` / `PUT /system/auth/preference` |
| `backend/tests/pref_preference.rs` | 280 | 真库集成测试（6 个场景，缺 `PREF_DB_URL` 自动 SKIP） |
| `frontend/src/api/system/preference-api.ts` | 133 | 接口封装 + `savePreferenceQuietly`（fail-safe） |
| `frontend/src/stores/preference.ts` | 133 | **通用偏好 store**（KV → 有类型读接口，其余偏好的统一入口） |
| `frontend/src/composables/usePalette.ts` | +81 | 本地即时渲染 + 登录后按后端校正 + 切换异步落库 |
| `frontend/src/stores/setting.ts` | +69 | 明暗 / 侧边栏落库（`applyRemote*` 与写库路径分开，避免回环） |
| `frontend/src/layouts/BasicLayout.vue` | +15 | 侧边栏折叠改绑 store（原来钉在组件内 `ref`，刷新即丢） |
| `frontend/src/stores/user.ts` | +8 | `loadProfile` 末尾触发一次偏好同步 |
| `scripts/lint/palette_def_sync.py` + `scripts/check.sh` | 79 / +14 | 门禁：字典 `ui.palette` 的 oneof ↔ 22 套色板逐字一致 |
| `backend/src/shared/error_code.rs` | +38 | 62020–62024 五个偏好码（码表自检同步扩到 24 个） |

红线核对：**未改** `frontend/src/style/palettes.ts`（22 套色板与色阶算法）、**未改** `frontend/src/style/theme.less`。

## 1. 表结构（空库冷启动后 `\d` 实测）

```
                   Table "public.sys_preference_def"
    Column     |           Type           | Nullable |         Default
---------------+--------------------------+----------+-------------------------
 pref_key      | character varying(64)    | not null |
 pref_name     | character varying(100)   | not null |
 value_type    | character varying(16)    | not null |
 default_value | jsonb                    | not null |
 scope         | character varying(16)    | not null | 'ui'::character varying
 validator     | character varying(300)   |          |
 sort_order    | integer                  | not null | 0
 status        | smallint                 | not null | 1
 remark        | character varying(255)   |          |
 created_at    | timestamp with time zone | not null | now()
 updated_at    | timestamp with time zone | not null | now()
Indexes: "sys_preference_def_pkey" PRIMARY KEY, btree (pref_key)
Check constraints: value_type IN ('string','number','boolean','json')

                   Table "public.sys_user_preference"
   Column   |           Type           | Nullable | Default
------------+--------------------------+----------+---------
 user_id    | uuid                     | not null |
 pref_key   | character varying(64)    | not null |
 pref_value | jsonb                    | not null |
 updated_at | timestamp with time zone | not null | now()
Indexes: "pk_sys_user_preference" PRIMARY KEY, btree (user_id, pref_key)
Check constraints: CHECK (pref_key ~ '^[a-z][a-z0-9_]*\.[a-z][a-z0-9_]*$')
Foreign-key constraints: FOREIGN KEY (user_id) REFERENCES sys_user(id) ON DELETE CASCADE
```

`PK(user_id, pref_key)` 的 user_id 在最左 → 「查某用户全部偏好」走主键索引，无需额外单列索引。

## 2. 迁移：空库冷启动 + up→down→up 可重放

### 2.1 空库冷启动（全新库 `cloudpivot_pref_pv`，0 张表 → 起服务）

```
INFO alun_web::app: 数据库迁移完成: 应用 57 个迁移
schema_migrations: 91|seed_preference_def|versioned|applied_seq=53
tables: sys_preference_def, sys_user_preference
```

8 条字典（真库）：

```
     pref_key     |  pref_name   | value_type | default_value | scope  | sort_order | validator长度
------------------+--------------+------------+---------------+--------+------------+--------------
 ui.palette       | 配色方案     | string     | "mist-blue"   | ui     |         10 | 232
 ui.theme_mode    | 明暗模式     | string     | "light"       | ui     |         20 |  16
 ui.side_compact  | 侧边栏折叠   | boolean    | false         | ui     |         30 |
 ui.table_density | 表格密度     | string     | "default"     | table  |         40 |  21
 ui.page_size     | 默认分页条数 | number     | 10            | table  |         50 |  11
 ui.language      | 界面语言     | string     | "zh-CN"       | ui     |         60 |  17
 notify.inbox     | 站内信开关   | boolean    | true          | notify |         70 |
 notify.email     | 邮件通知开关 | boolean    | false         | notify |         80 |
(8 rows)
```

（`validator长度=232` 就是 22 套色板的 `oneof:`，见 §7 门禁。）

### 2.2 up→（重复 up 幂等）→down→up（独立空库 `cloudpivot_pref_replay`）

```
== 0. 准备独立空库 cloudpivot_pref_replay
  ✔ 空库 + sys_user 桩表（仅 id / username / deleted_at，供外键落点）
== 1. 首次 up
  ✔ V0090 up（建两表）      ✔ V0091 up（种 8 条字典）
  表=2（应 2）  字典行=8（应 8）  索引=2  约束=3（应 3）
== 2. 重放 up（幂等）
  ✔ V0091 up 再来一次（ON CONFLICT）   字典行=8（应仍是 8）
== 3. 约束真的在起作用（脏键被 CHECK 拒绝）
  ✔ 脏键 'Bad..Key' 被 ck_sys_user_preference_key 拒绝
== 4. down（V0091 → V0090）
  ✔ V0091 down（删 8 条字典）  字典行=0   ✔ V0090 down（删两表）  剩余表=0
== 5. 再次 up（可重放）
  ✔ V0090 up 第二次  ✔ V0091 up 第二次  表=2  字典行=8  约束=3
== 结论：V0090 / V0091 up→（重复 up 幂等）→down→up 全部通过（0 残留、0 报错）
```

## 3. API 真机实测（`127.0.0.1:8026` + 空库 `cloudpivot_pref_pv`）

脚本 `verify_http.py`（真登录真令牌真落库），全绿 `EXIT=0`：

```
== 1. 管理员登录 + GET 默认值（无记录也要返默认，不返 404）
  PASS  POST /api/system/auth/login — http=200 code=0
     GET 响应 = {"palette": "mist-blue", "theme_mode": "light", "side_compact": false,
                 "items": {"notify.email": false, "notify.inbox": true, "ui.language": "zh-CN",
                           "ui.page_size": 10, "ui.palette": "mist-blue",
                           "ui.side_compact": false, "ui.table_density": "default",
                           "ui.theme_mode": "light"}}
  PASS  GET 首次（数据库无记录）返回默认值 — http=200 code=0
  PASS  GET items 全量 8 项 — items 键数 = 8
  PASS  GET 兼容字段 palette/theme_mode/side_compact 齐全
== 2. PUT 旧形态（扁平三字段）→ 落库 → GET 回读
  PASS  PUT 旧形态 200/code=0
  PASS  PUT 返回体含 items 全量 + 兼容字段 — palette=deep-sea theme_mode=dark side_compact=True
  PASS  GET 回读 = 刚保存的值
== 3. PUT 新形态（items KV）+ 局部更新语义
  PASS  PUT items 200/code=0
  PASS  局部更新：未传的 ui.theme_mode 保持 dark
  PASS  局部更新：ui.page_size=50 已生效
== 4. 字典校验 4 种拒绝（+ 空请求）
  PASS  拒绝：ui.palette 非法值（oneof） — http=400 code=62022 msg=偏好项「配色方案」的取值只能是 mist-blue / … （当前："not-a-palette"）
  PASS  拒绝：字典外的键（evil.key） — http=400 code=62020 msg=未知的偏好项：evil.key
  PASS  拒绝：ui.page_size 超 range（999） — http=400 code=62022 msg=偏好项「默认分页条数」必须是 5–200 之间的整数（当前：999）
  PASS  拒绝：ui.side_compact 类型错（字符串） — http=400 code=62021 msg=偏好项「侧边栏折叠」要求布尔类型的值（当前："yes"）
  PASS  拒绝：空请求体（{}） — http=400 code=62023 msg=请求体里没有任何偏好项
== 5. 拒绝之后库里的值没被改动（校验先于写库）   PASS
== 6. 越权：A 不能读写 B 的偏好（PUT 带 user_id 被忽略）
     B 首次 GET 拿到的是默认值（看不到 A 的 jade） — B.palette=mist-blue
     PASS  B 的 PUT 里带 A 的 user_id：仍写入 B 自己 — http=200 B.palette=rouge（请求体 user_id=01a185c9-…）
     PASS  A 的偏好未被 B 篡改（仍是 jade）
== 7. 未登录访问被拒绝
  PASS  无令牌 GET → 401      PASS  无令牌 PUT → 401
== 结论：全部通过
```

库侧留证（同一时刻的真实行，`join sys_user` 看归属）：

```
 username  |               user_id                |    pref_key     | pref_value
-----------+--------------------------------------+-----------------+------------
 admin     | 01a185c9-e5e6-7004-a5b2-120cc6cd1d77 | ui.page_size    | 50
 admin     | 01a185c9-e5e6-7004-a5b2-120cc6cd1d77 | ui.palette      | "jade"
 admin     | 01a185c9-e5e6-7004-a5b2-120cc6cd1d77 | ui.side_compact | true
 admin     | 01a185c9-e5e6-7004-a5b2-120cc6cd1d77 | ui.theme_mode   | "dark"
 pref_e2e_b| 01a185cb-1fe0-74e0-b4b8-a38a264d0ae9 | ui.palette      | "rouge"
(5 rows)
```

B 的请求体里带的是 **A 的 user_id**，落库行仍是 B 的 `user_id` —— 这就是「body 里的 user_id 一律忽略」的硬证据。

## 4. 前端验收（真浏览器，独立实例 `127.0.0.1:5174` → 后端 8026）

> 浏览器与服务器不同机，走 SSH 隧道（`-L 15174:127.0.0.1:5174`）；登录用令牌注入（不输密码）。
> 起始状态（库里 admin）：**palette = rouge / theme_mode = dark / side_compact = false / page_size = 50**。

### ⭐ 验收 1：删 localStorage → 刷新 → 仍从数据库恢复

```
clear: {"p":null,"s":null}                       ← localStorage.removeItem('cloudpivot-palette'/'cloudpivot-setting')
after-clear-reload: {"dataPalette":"rouge",      ← <html data-palette> 仍是从库里取回的 rouge
                     "dataPaletteMode":"dark",
                     "themeModeAttr":"dark",
                     "lsPalette":"{\"palette\":\"rouge\",\"mode\":\"dark\",…}",   ← 校正后回写本地
                     "lsSetting":"{\"themeMode\":\"dark\",\"sideCompact\":false}"}
```

### 验收 2：界面改配色 → 刷新 → 保持（并落库）

```
before: ["jade"]                                  ← 进设置页时选中项 = 库里的 jade
click: clicked                                    ← 点「胭脂 rouge」色卡
after-click: {"palette":"rouge","ls":"{\"palette\":\"rouge\",…}"}
after-reload: {"palette":"rouge","mode":"dark",…}  ← 刷新后仍是 rouge
DB: ui.palette = "rouge"  updated_at=2026-10-29 11:56:59+08
```

### 验收 3：全新浏览器（独立 session = 空 localStorage）同账号 → 同一配色

```
fresh-storage: {"p":null,"s":null}                ← 新浏览器一开始什么都没有
second-browser: {"dataPalette":"rouge", "dataPaletteMode":"dark",
                 "asideWidth":232,                ← side_compact=false（侧边栏展开）也从库里来
                 "lsPalette":"{\"palette\":\"rouge\",…}"}
```

### 验收 4：后端不可用时界面仍正常（回退 localStorage）

用「只有偏好接口 500」的方式验（把表改名，接口 `500 + 10004`），比整站停服更严格 ——
整站停服时路由守卫本来就会跳登录页（既有语义，与偏好无关），那样验不出偏好的 fail-safe。

```
api-status: {"status":500,"body":"{\"code\":10004,\"msg\":\"服务器内部错误\"}"}
after-reload: {"url":"…/dashboard","dataPalette":"rouge","dataPaletteMode":"dark",
               "dashboardRendered":true,"bodyTextLength":582}     ← 界面照常，配色来自 localStorage
click-amber: clicked
after-click: {"palette":"amber","ls":"{\"palette\":\"amber\",…}"}  ← 写库失败也不阻塞 UI
```

恢复接口后（表名改回）再刷新 → **后端值盖回本地值**，证明优先级「后端 > localStorage > 默认」：

```
priority: {"palette":"rouge", "ls":"{\"palette\":\"rouge\",…}", "note":"本地是 amber，后端是 rouge"}
```

### 附：明暗 / 侧边栏也落库

```
mode-before: {"attr":"dark"}  click-light: clicked  mode-after: {"attr":"light"}   → DB ui.theme_mode="light"
aside-before: 64px  toggle: clicked  → DB ui.side_compact=false；禁掉 transition 后实测 asideWidth=232px
（无头浏览器 visibility=hidden 时 CSS transition 不推进，rect 会停在旧值 —— 已知环境现象，非功能缺陷）
```

## 5. 单测与集成测试

```
$ cargo test                       # 后端全量
running 521 tests
test result: ok. 521 passed; 0 failed; 0 ignored          ← 本卡新增 15 个纯逻辑单测（基线 506）
（其余套件：11 / 8 / 7 / 7 / 4 / 2 / 1 … 全部 ok；pref_preference 无 PREF_DB_URL 时打印 SKIP）

$ PREF_DB_URL=…/cloudpivot_pref_pv cargo test --test pref_preference -- --test-threads=1
① upsert 幂等：1 行，deep-sea → jade ✓
② 局部更新：palette 保持 jade，page_size=50 ✓
③ 越权隔离：A=jade / B=rouge，互不可见 ✓
④ 真库字典：8 项预置 + ui.palette oneof 22 套色板 ✓
⑤ 外键级联：删用户后偏好行清零 ✓
⑥ 无记录用户 GET 默认值：8 项全部命中 ✓
test result: ok. 1 passed
```

纯逻辑单测覆盖（`model.rs` 12 条 + `dto.rs` 5 条）：未知键 62020 / 类型错 62021 /
oneof 越界 62022 / range 越界与小数 62022 / 边界值 5·200 通过 / null 全类型拒绝 /
GET 合并默认值 / 字典外脏键不下发 / 字典损坏 62024 / validator 解析 /
两代请求形态 / items 优先 / 空请求 62023 / body 里 `user_id` 被忽略。

## 6. 门禁

```
$ bash scripts/check.sh
==> 后端：cargo fmt --check            （通过）
==> 后端：cargo clippy -- -D warnings  （通过）
==> 后端：cargo check                  （通过）
==> 前端：pnpm typecheck               （通过，vue-tsc 零错误）
==> 批次B 闸①：0 处    闸②：0 处       （通过）
==> C7 G1–G6：6 条全部 PASS            （通过）
==> 偏好字典与色板一致性：PASS：22 套色板与 V0091 的 ui.palette oneof 逐字一致
==> 全部检查通过                       CHECK_EXIT=0

$ pnpm build                           成功（dist 产物正常，含 preference-*.js chunk）
```

一致性门禁的反向验证（注入一个前端有、字典没有的色板）：

```
FAIL：色板 23 套 / 字典 oneof 22 项
    字典缺少（界面能选、后端会拒绝保存）：brand-new-palette
DRIFT_EXIT=1
```

> 闸② 之所以没被新代码判红：`model.rs` 的 `FROM sys_user_preference` 会命中
> `sys_user` 的子串匹配，文件里写明 `SCOPE-EXEMPT`（偏好是「用户对自己的设置」，
> 行级隔离就是 `user_id = AuthClaims.sub`，注入数据范围反而会让用户读不到自己的配色）。

## 7. 口径与边界（如实说明）

1. **审计**：偏好变更**不写** `audit_logs`（量级高、无安全语义，会淹掉真正的敏感操作）；
   成功路径打 `tracing::info!(user_id, keys)`，库里 `updated_at` 可查。
2. **不缓存字典**：8 行表、主键顺序扫描 <1ms；缓存会引入「改字典要等过期」的运维成本。
   偏好项上百条时再加 `Cache` + 显式失效。
3. **本卡范围**：`ui.table_density` / `ui.page_size` / `ui.language` / 两个通知开关
   **只打通读存**（字典 + 接口 + `stores/preference.ts`），业务表格消费是后续增量。
4. **前端 dist 变更**：`pnpm build` 会更新 `frontend/dist`（prod nginx 的 root）。本卡**没有**
   重建/重启 prod 的 release 后端，因此在后端重新部署前，prod 页面调新接口会拿到 404，
   前端按 fail-safe 回退 localStorage（不报错、不白屏）——部署后端后自动生效。
5. **无头浏览器现象**：`document.visibilityState === 'hidden'` 时 CSS transition 不推进，
   侧边栏宽度读数会停在旧值；禁 transition 复测得 232px，功能本身正常。
