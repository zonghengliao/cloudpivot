# HR-33 证据：考勤「确认导入」成功后不再补客户端 recalc（真库 + 真后端 + 真浏览器）

- 卡：t_04c1e92f（HR-33，frontend-developer）
- 改动：只动前端 2 个文件 + 1 个单测
  - `frontend/src/views/business/hr/attendance/index.vue`：删掉导入成功后的 `recalcPeriod(...)` 补偿（含 catch/warning 分支），并作废旧合计缓存
  - `frontend/src/api/business/hr/attendance.ts`：文件头把「通用路径不重算 / 需要页面补 recalc」改成「确认入库即重算」
  - `frontend/tests/unit/hr-attendance-page.test.ts`：把「导入后调 recalc」的断言反过来（断言不调）
- 后端未改（`/period/{id}/recalc` 与期间 tab 的「重算」按钮都保留）

## 1. 环境（一次性，跑完即删）

| 项 | 值 |
|---|---|
| 库 | 99 本机 PG 18.6 `cloudpivot_hr33`（`create database … owner alun`，evidence 末段有清理命令） |
| 后端 | `/opt/cloudpivot/backend/target/debug/cloudpivot`，`NIK_PROFILE=test`，`NIK_SERVER_LISTEN=127.0.0.1:18033`（首启迁移 393 + bootstrap） |
| 前端 | `frontend` 的 `vite dev`（`127.0.0.1:5199`，`BACKEND_ORIGIN=http://127.0.0.1:18033`）—— 用**改动后的源码**跑，不是旧 dist |
| 夹具 | `biz_employee` 插 2 名在职员工（EMP-0001 张三 / EMP-0002 李四，org_id=全零哨兵）；`POST /business/hr/attendance/period` 建 `2029-03`（DRAFT，初始 0/0.00/0.00） |
| 导入文件 | 手工构的 xlsx（15 列表头 + 2 行），生成脚本见 §5 |

## 2. 场景一：空期间 → 上传 2 行 → 确认导入 → 请求流水

清空 `performance` 计时后点「确认导入（2 行）」，浏览器实际发出的 `/api` 请求（`performance.getEntriesByType('resource')`，真网络层）：

```
/api/business/impexp/import/01a1d129-8ed4-720e-ae21-642eeb0ad867/confirm
/api/business/hr/attendance/period/page?status=DRAFT,SUBMITTED&page=1&page_size=200
/api/business/hr/attendance/record/page?period_id=01a1d128-ec8e-7768-944a-b041f52a7386&page=1&page_size=20
/api/business/hr/attendance/period/page?page=1&page_size=20
```

**没有 `/attendance/period/{id}/recalc`**。回执卡片 `共 1 批，入库 2 行（校验失败 0 行已跳过）（已入库 2 行）`（截图 `02-import-done-no-recalc.png`），背后台账底栏已是 `共 2 人 · 应出勤 44 天 · 实际 40 天 · 加班 0 小时`。

切回「考勤期间」tab（截图 `03-period-tab-totals-after-import.png`），期间行：

```
2029-03 | 2029-03-01 ~ 2029-03-31 | 草稿 | 2 | 44 | 40 | 0 | … | 台账 编辑 提交 重算 删除
```

人数 / 合计已经是新值，**没有点过重算**。行上的「重算」链接仍在（人工补偿入口保留）。

## 3. 场景二（回归本卡顺手修的缓存口径）：先点过「重算」再导入

原来导入后会 `totalsCache[periodId] = recalc.after` 覆盖缓存；删掉 recalc 后如果不清缓存，
「导入前点过重算 / 改过单元格」留下的旧合计会盖住后端新算出来的值。实测：

1. 期间 tab 点「重算」（此时 totalsCache = 2 / 44 / 40，请求 `/attendance/period/{id}/recalc` 出现一次 —— 人工入口）；
2. 台账 tab 底栏合计 `共 2 人 · 应出勤 44 天 · 实际 40 天`；
3. 上传 v2 文件（EMP-0001 实际出勤 20 → 21）→ 确认导入；
4. 确认窗口请求流水同样只有 confirm + 3 次分页读，**无 recalc**；
5. 底栏合计变成 `共 2 人 · 应出勤 44 天 · 实际 41 天`（截图 `04-reimport-after-stale-cache.png`）。

库里的真值（旁证）：

```
2029-03|DRAFT|2|44.00|41.00
EMP-0001|22.00|21.00|IMPORT
EMP-0002|22.00|20.00|IMPORT
```

## 4. 门禁

```
pnpm typecheck   → 通过（vue-tsc --noEmit，无输出）
pnpm test:unit   → 1722 tests / 1722 pass / 0 fail
```

单测新增 `confirm import sends upload + confirm only, never a compensating recalc`：
断言 `handleConfirmImport` 函数体内没有 `recalcPeriod(`、有 `delete totalsCache[`、有 `await loadLedgerPeriods()`；
断言全页 `await recalcPeriod(` 只有 1 处（期间 tab 的「重算」按钮）；断言页面头 / API 文件头已无「后端不重算」旧说法。

## 5. 复现（夹具与脚本）

```bash
# 库 + 后端（profile=test，空库首启带 NIK_ADMIN_PASSWORD，否则随机口令 + must_change_password）
PGPASSWORD=pflow3a psql -h 127.0.0.1 -U pflow3a -d postgres -c "create database cloudpivot_hr33 owner alun"
# 夹具 SQL / xlsx 生成脚本 / env 在卡内 scratch：
#   fixture_employees.sql、make_att_xlsx.py、env.sh
python3 make_att_xlsx.py att_202903.xlsx 2029-03 EMP-0001 EMP-0002      # 22/20 + 22/20
python3 make_att_xlsx.py att_202903_v2.xlsx 2029-03 EMP-0001 EMP-0002 21 # 22/21 + 22/20
BACKEND_ORIGIN=http://127.0.0.1:18033 ./node_modules/.bin/vite --port 5199 --strictPort
```

## 6. 清理

```bash
# 停实例（按端口取 pid，别用 pkill -f target/debug/cloudpivot）
PID=$(ss -ltnp | grep ':18033' | grep -o 'pid=[0-9]*' | cut -d= -f2); kill $PID
PID=$(ss -ltnp | grep ':5199'  | grep -o 'pid=[0-9]*' | cut -d= -f2); kill $PID
PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d postgres -tAc "select count(*) from pg_database where datname='cloudpivot_hr33'"
PGPASSWORD=pflow3a dropdb -h 127.0.0.1 -U pflow3a cloudpivot_hr33
```
