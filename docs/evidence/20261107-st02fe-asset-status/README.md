# FIN-ASSET-ST02-FE 取证：资产页「停用 / 启用」按钮 + 原因弹窗

卡：`t_55cb43fd`（frontend-developer）。上游后端卡 `t_4201b539`（commit `e089d77`）提供
`POST /api/business/erp/asset/{id}/status`；本卡只动前端（api / 纯逻辑 / 页面 / 单测）。

## 1. 改了什么（4 个文件）

| 文件 | 改动 |
|------|------|
| `frontend/src/api/business/erp/asset.ts` | `AssetStatusChangePayload` + `changeAssetStatus(id, payload)` → `` `${BASE}/asset/${id}/status` `` |
| `frontend/src/views/business/erp/asset/asset-logic.ts` | `canIdleAsset`（`status==='IN_USE'`）/ `canEnableAsset`（`status==='IDLE'`）；`ASSET_ERROR_HINTS[66046]` |
| `frontend/src/views/business/erp/asset/index.vue` | 操作列「停用 / 启用」（换人之后、处置之前）+ 一个弹窗两种语义（标题 / alert / 确认按钮主题随目标态切换） |
| `frontend/tests/unit/fin2-asset.test.ts` | 真值表 + 66046 提示 + `changeSummary` 状态留痕 + 源码护栏（api 路径 / 两个 testid / 不许新增权限码 / 页面不许出现 66046） |

**零新增权限码**：按钮沿用 `PERM.update = business:erp:asset:update`（与后端 `#[nik::permission]` 一致）；
单测里有一条 `!/PERM\.(idle|enable|status)/i` 的反向护栏。
**未改 `changeSummary`**：留痕行部门 / 使用人四组为 `NULL`，现有实现按 `from_status/to_status` 自动输出
「在役 → 闲置」/「闲置 → 在役」（见 §4 时间线证据）。
**66046 只在 `asset-logic.ts`**：页面失败路径统一走 `errorText(error)`，单测钉住页面源码不含 `66046`。

## 2. 门禁结果（本机实测）

```
pnpm typecheck        → 通过（0 错；本机默认堆会 OOM，需 NODE_OPTIONS=--max-old-space-size=6144）
pnpm check:palettes   → 色阶断言 330 条 / 失败 0，「全部通过 ✓」
pnpm test:unit        → 1033 条：1031 pass / 2 fail（两条 fail 是 main 上既有红，见 §5）
```

> 注：`frontend/package.json` **没有 `lint` 脚本**（`grep -n '"lint"'` 零命中），本卡按既有门禁跑
> `typecheck + check:palettes + test:unit`，没有另造一套 linter。

`fin2-asset.test.ts` 单独跑：**28 passed / 0 failed**（新增 4 条：
`停用 / 启用按钮可见性真值表` / `66046 提示` / `changeSummary 在役 → 闲置` / `停用启用接线源码护栏`）。
逐条输出见 `unit-tests-summary.txt`。

## 3. 真机环境（隔离，不碰生产）

| 组件 | 值 |
|------|-----|
| 后端 | HEAD `e6594bf` 源码 `git archive` 到 `/root/st02fe-tree`，`cargo +1.94.1 build`（CARGO_TARGET_DIR 复用热目录），监听 `127.0.0.1:18151` |
| 库 | `pg_dump cloudpivot_fin2asset \| psql → cloudpivot_st02fe`（自带上线前 4 张资产卡；admin 口令用 argon2 重算后置 `Admin@123456`） |
| 前端 | `BACKEND_ORIGIN=http://127.0.0.1:18151 vite --port 5183`（**源码即产物**，不 build、不碰生产 dist） |
| 浏览器 | Playwright（`@playwright/test` 的 chromium），脚本 `verify.mjs`（本目录 `.txt` 备份） |

夹具（`fixture_status.sql`，每轮跑前重置）：`FA-20261105-000001` 在役（走真链路）、
`FA-20261105-000003` 置 `IDLE`、`FA-20261105-000004` 置 `DISPOSED`（需同时写 `disposal_type/date`，
否则撞 `ck_biz_fa_disposed`）。A1 的停用 / 启用**全部走 HTTP 真接口**，不是夹具改状态。

## 4. 真机链路（一次跑通，`ok: true`，consoleErrors `[]`，failedRequests `[]`）

| 步骤 | 实测 |
|------|------|
| 三种状态按钮可见性 | `FA-…000001` 在役 → `idle:true, enable:false`；`FA-…000003` 闲置 → `idle:false, enable:true`；`FA-…000004` 已处置 → 两个都 `false` |
| 点「停用」弹窗 | 标题 `停用资产`；alert 含「仍按月计提折旧」+「恢复使用点「启用」」；`原因`（textarea）+ `变动日期`（date-picker）都在；确认按钮 class 含 `t-button--theme-warning` |
| 空原因前置校验 | 不发请求（POST 计数 0），表单项报「停用原因必填（留痕要能回答「为什么」）」 |
| 停用提交 | `POST /api/business/erp/asset/{id}/status` **200**，body `target_status=IDLE`、`reason` 28 字、`change_date=2026-11-07`；回包 `asset.status=IDLE`、`changes` 长度增长 |
| 停用后列表 | 行 Tag 变「闲置」，按钮翻转为 `enable:true / idle:false`（图 `03-list-after-idle.png`） |
| 详情时间线 | 首行「停用 / 在役 → 闲置」（图 `04-detail-timeline.png`；DOM 断言 `timeline.includes('在役 → 闲置')`） |
| 再点「启用」 | 标题 `启用资产`；alert「启用后资产回到「在役」，继续按月计提折旧。」；确认按钮 `t-button--theme-primary` |
| 启用提交 | **200**，`target_status=IN_USE`，回包 `asset.status=IN_USE`；列表 Tag 回「在役」，按钮翻转回 `idle:true / enable:false` |
| 同态重复（对在役卡发 `IN_USE`） | **409 / code 66046**「资产 FA-20261105-000001 当前已经是「在役」：重复启用不是状态变更（不会重复通知）」→ 页面走 `errorText` 自动补这句「怎么办」 |

截图：`01-list-before.png`（三种状态同屏）→ `02-idle-dialog.png` → `03-list-after-idle.png` →
`04-detail-timeline.png` → `05-enable-dialog.png` → `06-list-after-enable.png`。

### 一个与卡面不同的小事实（不影响前端）

卡面 / 设计 §6 写「已处置的卡片发状态变更 → 400 指向 `/dispose`」，真机在 **409 + 66046** 上返回
「…只有「在役 / 闲置」的卡片能停用 / 启用；已处置的请先作废处置」。前端两条都走同一个 `errorText` +
`ASSET_ERROR_HINTS[66046]`，行为不变；此处只做记录，供后端卡后续收敛文案口径。

## 5. 两条既有红（不是本卡引入，已用干净树反证）

```
not ok 358 - create payload keeps dates and folds empty remark to null   (tests/unit/erp-sales-order.test.ts)
      actual 多出 currency_code: null（SO-42 币种字段进了 payload，测试期望没同步）
not ok 588 - 后端 FlowBizType 的落库字面量 = 前端 FLOW_BIZ_TYPES（15 个）
      解析到 16 个：…,ASSET_DISPOSAL,RECEIPT_REFUND → 16 !== 15
```

反证方式：`git archive HEAD | tar -x` 到干净目录、只软链 `node_modules`，跑这两个 spec
**同样 2 条红**（27 pass / 2 fail）→ 与工作区改动无关，属 main 既有债（SO-42 与 RECEIPT_REFUND 那两卡留下的）。
本卡改动只落在资产页 4 个文件，不碰销售 / 流程模块。

## 6. 复现步骤

```bash
# ① 后端（HEAD 源码 + 隔离库）
git -C /opt/cloudpivot archive HEAD | tar -x -C /root/st02fe-tree
cp /var/tmp/t_573bfe5b-run/config/config-test.toml /root/st02fe-tree/backend/config/
su postgres -c "createdb -O alun cloudpivot_st02fe"
su postgres -c "pg_dump cloudpivot_fin2asset" | su postgres -c "psql -q -d cloudpivot_st02fe"
cd /root/st02fe-tree/backend && CARGO_TARGET_DIR=/root/cargo-target-shared cargo +1.94.1 build
NIK_PROFILE=test NIK_SERVER_LISTEN=127.0.0.1:18151 NIK_DATABASE_NAME=cloudpivot_st02fe \
NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) NIK_ADMIN_PASSWORD=Admin@123456 \
  /root/cargo-target-shared/debug/cloudpivot
# ② 前端 dev（源码即产物）
cd /opt/cloudpivot/frontend && BACKEND_ORIGIN=http://127.0.0.1:18151 ./node_modules/.bin/vite --port 5183 --strictPort
# ③ 夹具 + 真机脚本
su postgres -c "psql -q -d cloudpivot_st02fe -f /var/tmp/st02fe_fixture.sql"
cd /root/st02fe-verify && node verify.mjs /root/st02fe-verify/evidence    # 期望 exit 0 且 "ok": true
```

坑（下一次直接照做）：
- 库 admin 口令未知时**不要**用 `su postgres -c "psql -v pw=\"$(cat hash)\""` 传 argon2 串 ——
  `$argon2id$v=19$…` 会被 `su -c` 的二次 shell 当变量展开，落库成 `=19=19456,t=…`，
  登录报 500 `Unknown hash format`。用 `uv run --with argon2-cffi` 现算 PHC 串、写进 `.sql` 文件再 `psql -f`。
- `DISPOSED` 直置必须同时写 `disposal_type` + `disposal_date`（`ck_biz_fa_disposed`）。
- 详情抽屉 `:footer="false"` + `header` 属性不渲染关闭按钮；`.t-drawer__mask` 点击偶发不生效 →
  点抽屉外坐标 + 轮询 `classList.contains('t-drawer--open')` 判关闭。
- 抽屉是滑入动画：截图前 `sleep(1200)` + `scrollIntoViewIfNeeded()`，否则拍到半截。
- `pnpm typecheck` 本机默认堆 1.5G 会 `FATAL ERROR: Reached heap limit` → `NODE_OPTIONS=--max-old-space-size=6144`。
