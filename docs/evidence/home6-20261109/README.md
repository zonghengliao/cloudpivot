# HOME-6 验收证据 — 顶栏铃铛 / 登录未读弹窗「按权限发请求」

实例：隔离后端 `127.0.0.1:18094`（debug，`NIK_PROFILE=test`）+ 库 `cloudpivot_home4_v1`（HOME-4 留的，复用）
+ Vite dev `127.0.0.1:5400`（`BACKEND_ORIGIN=http://127.0.0.1:18094`）+ Playwright Chromium。

账号：`admin / Home4@2026Pass`（SUPER_ADMIN）、`h4viewer / Viewer@2026Pass`（EMPLOYEE，**已删掉**
`business:notice:mine` / `business:announcement:read` 的 4 条 `sys_role_menu` 授权，见第四节)。

---

## 一、问题与结论

改前（HOME-4 留证 `docs/evidence/home4-20261109/api/report-viewer-noperm.json` 的 `viewer.forbidden`）：

```
403 /api/business/notice/stream
403 /api/business/notice/unread-count      ← 铃铛 + 登录弹窗各一次
403 /api/business/announcement/unread-count
```

这三条都是**无权限账号一登录就硬发**的长连接 / 未读数请求（`BasicLayout` → `noticeStore.connect()`、
`LoginUnreadDialog.open()`）。后端注解里它们分别是 `business:notice:mine` / `business:notice:mine` /
`business:announcement:read`（`backend/src/business/notice/handler.rs:617`、
`backend/src/business/announcement/handler.rs:169`），所以对 EMPLOYEE 角色**发出去就是 403**。

改后：`EMPLOYEE` 登录 → 三条请求一条都不发、铃铛整只不渲染、控制台 0 条 error；
`admin` 登录 → 角标 / SSE 实时推送 / 登录未读弹窗四行数字**逐字不变**（见第三节）。

---

## 二、改动清单

| 文件 | 改什么 | 为什么 |
|------|--------|--------|
| `frontend/src/stores/notice.ts` | 新增 `canReadNotice()`（`userStore.hasPermission(PERM.noticeMine)`）；`connect()` / `refreshUnread()` / `pullLatest()` 三处入口都先过这道闸；`connect()` 无权限时顺手清掉残留角标与浮层批次 | 站内信的 SSE / 未读数 / 列表接口**共用同一个权限码**，闸放在 store 里，顶栏、消息中心页、浮层就都收口了（不用每个组件各写一遍） |
| `frontend/src/layouts/components/NoticeBell.vue` | 根节点 `<t-popup v-if="canReadNotice">` | 无权限时铃铛点开必 403：留着只是把红色报错从首屏挪到点击后。直接不渲染，角标自然也不会出现 |
| `frontend/src/layouts/components/NoticeToast.vue` | 浮层 `v-if` 增加 `canReadNotice` | 浮层本身不请求接口（数据来自 store），但换账号/退出重登时 store 可能残留上一账号的消息，加同一道闸兜住 |
| `frontend/src/utils/login-unread.ts` | `UNREAD_SOURCES` 每类加 `permission` 字段；新增 `permittedUnreadKeys()` / `pickUnreadLoaders()`；`summarizeUnread(counts, keys?)` 支持只渲染有权限的类别 | 登录弹窗的四路未读数各有自己的权限码，集中成一张表，组件不再各写一套判据 |
| `frontend/src/layouts/components/LoginUnreadDialog.vue` | `open()` 先算 `allowedKeys`，为空直接返回（不发请求、不弹空框）；只挂有权限类别的 loader；行也只用有权限类别汇总 | ③「四类都无权限 → 整框不弹」与 ②「无权限行不出现」的唯一实现点 |
| `frontend/tests/unit/login-unread.test.ts` | +4 条用例（权限码逐字对应 / `permittedUnreadKeys` / `pickUnreadLoaders` 剔除后不调用 / `summarizeUnread` 只出行） | 卡面 ③ 的分支（`allowedKeys.length === 0` → 不请求不弹）在单测里锁死，防止以后有人把闸拆了 |

> 权限码全部走 `frontend/src/constants/permissions.ts` 的 `PERM.*`，与后端 `#[nik::permission]` 逐字一致，
> 组件与 store 里**不出现**权限码字面量。

---

## 三、验收逐条对照（`22 passed / 0 failed`，`logs/verify-ui.txt`）

| 卡面验收 | 结果 | 证据 |
|---|---|---|
| ① 无 `business:notice:mine` → 不请求 `/notice/stream`、`/notice/unread-count`，铃铛不显示角标 | ✅ | 浏览器 network：`stream=0`、`unread-count=0`、`notice/page=0`；DOM：`.notice-bell__trigger` 数量 = **0** |
| ② 无 `business:announcement:read` → 不请求 `/announcement/unread-count`，弹窗该行不出现 | ✅ | `announcement/unread-count=0`；弹窗行 keys = `[]`（连行都没有） |
| ③ 四类都无权限 → 整框不弹（无权限按 0 计） | ✅ | 分支单测：`permittedUnreadKeys(() => false)` = `[]`，`open()` 为空即 return（不发请求、不弹框、不打「已弹过」标记）；E2E：viewer 本环境 task/approval 无未读 → `login-unread-dialog` 未出现 |
| ④ 超管行为不变 | ✅ | 铃铛 1 个、角标 `2`；`/notice/stream` 建立 **1** 条长连接且 **0 个 4xx**；`/notice/unread-count` 4 次、`/announcement/unread-count` 1 次全部 200；弹窗四行数字与接口值逐字相等（notice 2=2、announcement 2=2、task 3=3、approval 0=0）；发一条站内信给自己 → **浮层 12s 内弹出**，标题「新消息 1 条」（SSE 实时链路没被闸误伤） |
| EMPLOYEE 登录 → console error = 0，network 无任何 403 | ✅ | `badResponses=[]`、`consoleErrors=[]`（整页，不止首页） |
| `pnpm typecheck` / `pnpm build` | ✅ | `logs/typecheck.txt` `TYPECHECK_EXIT=0`、`logs/build.txt` `BUILD_EXIT=0` |
| 单测 | ✅ | `logs/unit-login-unread.txt`：12 tests / 12 pass / 0 fail |

另一侧的直接佐证（`api/viewer-endpoints-status.txt`）：这三个接口**直连后端对该账号就是 403** ——
说明「不发请求」是收口而不是隐藏结果：

```
/api/business/notice/stream                -> 403
/api/business/notice/unread-count          -> 403
/api/business/announcement/unread-count    -> 403
```

---

## 四、复跑方法

```bash
# 1) 起隔离后端（DB 复用 HOME-4 的 cloudpivot_home4_v1）
cd backend && NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18094 \
  NIK_DATABASE_NAME=cloudpivot_home4_v1 NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  NIK_ADMIN_PASSWORD='Admin@123456' \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) ./target/debug/cloudpivot

# 2) 让 h4viewer 真的「无权限」（本卡就是这么造的；恢复语句见下）
psql -U alun -h 127.0.0.1 -d cloudpivot_home4_v1 -c "
  delete from sys_role_menu
   where role_id=(select id from sys_role where role_code='EMPLOYEE')
     and menu_id in ('20000000-0000-0000-0000-000000000700',
                     '20000000-0000-0000-0000-000000000701',
                     '23000000-0000-0000-0000-000000000010',
                     '20000000-0000-0000-0000-000000000414');"

# 3) 前端 dev 指向隔离后端
cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18094 pnpm dev --port 5400 --strictPort

# 4) 跑验收（22 条断言 + 2 张截图 + report-home6.json）
node docs/evidence/home6-20261109/scripts/verify-ui.mjs
```

> **恢复授权**（把 h4viewer 变回「有站内信/公告权限」）：
> 把上面 4 条 `sys_role_menu` 行重新插回即可（`id` 可用 `gen_random_uuid()`，
> `role_id = 20000000-0000-4000-8000-0000000f0009`）。本卡复跑后的库**保持无权限状态**，
> 因为「无权限账号」才是本卡要压的场景。

### 环境搭建时踩到的两个坑（记录以免下一位重踩）

1. **`CLOUDPIVOT_STORAGE_MASTER_KEY` 必须是 64 位十六进制**（32 字节）。随便写 37 个字符会在启动守卫
   直接 `FATAL: 安全守卫拒绝启动`。用 `openssl rand -hex 32`。
2. **复用旧库会被迁移指纹校验拦下**：`seed_fix_flow_is_system_and_admin_dept`（version `366`）在
   `3193fdbe` / `91525642` 两个提交里被改过，旧库记录的是老 checksum（`99a23e50…`），当前文件是
   `9f221373…`，启动即 `FATAL: 数据库迁移失败: ... has been tampered with`。
   复用旧库时按当前文件内容对齐一次指纹即可（只影响这个测试库）：
   ```sql
   update schema_migrations
      set checksum='9f221373a17b884cf3a05c077c40673856d40b3ed90187002f95cae4dcffc13f'
    where version='366';
   ```

---

## 五、偏差与取舍（明写，不藏）

1. **铃铛对无权限账号「整只不渲染」**，而不是「渲染但角标 0」。卡面①写「铃铛不显示角标」；
   但铃铛面板的数据源 `GET /notice/page` 同样是 `business:notice:mine`，渲染出来点开必 403。
   留一个点开就报错的入口比隐藏它更糟，所以整只隐藏。**有权限账号零变化**（E2E 已断言铃铛数量 = 1）。
2. **闸放在 `stores/notice.ts` 而不是 `BasicLayout`**。`connect()` 有三个调用方（`BasicLayout`、
   消息中心页、页面刷新），放在 store 里只有一处判据；同时把 `refreshUnread()` / `pullLatest()`
   也一起闸住，避免「连接被挡了、30 秒轮询还在偷偷请求」这种半收口。
3. **`/notice/unread-count` 在超管下出现 4 次**是既有行为（首屏 1 次 + SSE 握手后 `handleFrame` 刷新 +
   两条推送各 1 次），本卡没动这条路径；数字与接口值逐字一致已断言。
4. **无权限时 `connect()` 会清 `unreadCount` 与浮层批次**：换账号（超管 → EMPLOYEE）时不残留上一个账号的角标。
5. **未改后端、未改权限种子**：`EMPLOYEE` 是否该有 `business:notice:mine` 是产品口径问题
   （种子里这两条授权其实存在，本卡只是把「删掉授权后前端别硬发」这件事补上）。若产品后来决定给
   EMPLOYEE 授权，前端无需再改 —— 有权限就自动恢复请求与角标。

---

## 六、文件清单

```
ui/01-viewer-dashboard.png        EMPLOYEE 首页：顶栏无铃铛、无 403
ui/02-admin-dashboard.png         超管首页：铃铛 + 角标、SSE 推送浮层
api/report-home6.json             22 条断言结果 + 请求清单 + 弹窗/接口数字对照
api/viewer-endpoints-status.txt   三个接口对该账号直连 403（证明「不发」= 收口）
logs/verify-ui.txt                Playwright 验收输出（22 passed / 0 failed）
logs/unit-login-unread.txt        tsx 单测（12 passed / 0 failed）
logs/typecheck.txt                pnpm typecheck（exit 0）
logs/build.txt                    pnpm build（exit 0）
scripts/verify-ui.mjs             验收脚本（可复跑）
```
