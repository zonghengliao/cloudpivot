# 2026-10-31 11:34 prod 夹具事故 · 取证结论（172.18.10.7 是谁）

> 事故本身（B1 BLOCKER + C3 WARN 变红、P11 验收 30/32）与后端侧的处置见
> `docs/datafix-20261031-p11-m-w1a-004-recur.md`。本文只回答**源头**问题：
> 11:34:11 打 `POST /api/business/erp/serial/batch-generate` 的 `172.18.10.7` 是谁，
> 以及仓库侧为此落地的硬判据（E2E 不得直连 prod）。

## 1. 结论（一句话）

**不是本团队的自动化 E2E，也不是仓库里的 `scripts/verify/*`。**
是一条**人在 Mac 上用真实 Chrome 打开 prod 页面手工走查**的业务闭环：
客户端 `172.18.10.7`（macOS + Chrome 156），账号 **`admin`（超级管理员）**，
11:34:11 点「批量生成」造出 10 个零成本序列号，11:35:03 建调拨单，11:35:50–11:37:47
三次**换账号**（admin → `henry.liao` → admin → `henry.liao`）演示审批。

## 2. 取证命令与原始证据

### 2.1 客户端到底是什么（nginx access log 有 UA 和 Referer）

在 prod（`172.18.10.144`）上：

```bash
grep "^172.18.10.7 " /var/log/nginx/access.log | grep "batch-generate"
```

```
172.18.10.7 - - [31/Oct/2026:11:34:11 +0800] "POST /api/business/erp/serial/batch-generate HTTP/1.1" 200 489
  "http://172.18.10.144/business/erp/serial"
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36"
```

- UA 是**桌面版 Chrome for macOS**，不含 `HeadlessChrome`、不含 `Playwright/x`；
- Referer 是真实页面 `http://172.18.10.144/business/erp/serial`（人从菜单点进来的，不是脚本拼 URL）；
- 该 IP 当天 **1502** 条请求，**UA 只有这一种**（`sort | uniq -c` 结果 1502 条同 UA），
  时间跨度 00:55 → 12:08，形态是浏览（`/dashboard`、`/task/list?view=board`、`/flow/def/.../edit`、
  `/business/erp/{serial,transfer,warehouse,material}`）+ 60s 一次的 `auth/refresh` + `/notice/stream` SSE。

对比本团队 Playwright 的 UA（同一份 `auth_logs` 里，来自 `172.18.10.99`）：

```
Playwright/1.64.1 (arm64; ubuntu 26.04) node/22.23
Mozilla/5.0 (X11; Linux x86_64) ... HeadlessChrome/156.0.8078.4 Safari/537.36   ← 172.18.10.144 上跑的一次
```

⇒ 事故请求的客户端与团队的 E2E 运行器**不是同一类客户端**。

### 2.2 机器类型：Mac（不是 Linux 服务器、不是容器）

从开发机 `172.18.10.99` 侧扫（`nc -z`）：

```bash
for p in 22 80 443 5900 5000 7000 445 3283 3000 8080 5173 8023; do
  timeout 1 nc -z -n 172.18.10.7 $p && echo "open $p"; done
```

```
open 22      # sshd
open 5000    # macOS AirPlay 接收器（ControlCenter）
open 7000    # macOS AirPlay（RAOP）
```

- `5000/7000` 是 macOS 特有的 AirPlay 端口组合，Windows/Linux 上没有；
- 没有 445（SMB）、没有 3389，`ssh` 返回 `Permission denied (publickey,password,keyboard-interactive)`（macOS 的 sshd banner 行为一致）；
- MAC `3e:ca:29:ad:9a:05` 是**本地管理地址**（虚拟机/私有 MAC），与 144/99 的 Parallels `00:1c:42:*` 不同 ⇒ 另有其人自己的机器，不是 prod/dev 两台服务器。

### 2.3 是谁在操作（`auth_logs` 直接记了账号 + IP + UA）

prod 库：

```bash
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h "$NIK_DATABASE_HOST" -p "$NIK_DATABASE_PORT" \
  -U "$NIK_DATABASE_USER" -d "$NIK_DATABASE_NAME" -c \
  "select username, auth_type, auth_status, ip, browser_type, os_type, created_at
     from auth_logs
    where ip='172.18.10.7' and created_at between '2026-10-31 11:00' and '2026-10-31 12:00'
    order by created_at"
```

```
 admin       | LOGOUT       | SUCCESS | 172.18.10.7 | Chrome | macOS | 11:35:50.084
 henry.liao  | LOGIN        | SUCCESS | 172.18.10.7 | Chrome | macOS | 11:35:52.962
 henry.liao  | LOGOUT       | SUCCESS | 172.18.10.7 | Chrome | macOS | 11:37:00.156
 admin       | LOGIN        | SUCCESS | 172.18.10.7 | Chrome | macOS | 11:37:03.501
 admin       | LOGOUT       | SUCCESS | 172.18.10.7 | Chrome | macOS | 11:37:43.905
 henry.liao  | LOGIN        | SUCCESS | 172.18.10.7 | Chrome | macOS | 11:37:47.296
```

**两分钟内三登三出、两个账号来回换**——这是人在演示「申请人提交 → 审批人同意」，
自动化用例不会这么干（E2E 用 `laoliu_test` 一个账号，靠 `storageState` 复用登录态）。

### 2.4 造的那批数据是谁写的

```sql
select serial_no, m.material_code, s.status, s.source_type, s.source_id, s.create_by, s.created_at, s.deleted_at
  from biz_material_serial s join biz_material m on m.id = s.material_id
 where s.serial_no like 'SN-20261031-0000%' order by s.serial_no;
```

```
SN-20261031-000001..000010 | M-W1A-004 | IN_STOCK | PURCHASE | (null) |
  create_by = 01a17bb9-7d99-76c3-b362-58d3a284c270（= admin） | created_at 11:34:11.113 |
  deleted_at 12:12:58.237（P10 清理，非本次事故的处置）
```

⚠️ 注意 `deleted_at` 有值但 `status` 仍是 `IN_STOCK`、`source_id` 仍为空：**清理是软删**，
所以「库里还能查到 `SN-…`」不等于「还有残留」。判据以 `e2e_fixture_residue.py`
（形态特征：`SERIAL_IN + SERIAL_BATCH` 流水、无来源、零成本）为准，而不是按序列号搜索。

### 2.5 事故之后那台机器还在打 prod（说明是人在用，不是一次性脚本）

```
12:08:26 GET /api/business/flow/def/2000...d157/versions 200  "http://172.18.10.144/flow/def/20000000-.../edit"
```

—— 12:08 还在流程设计器里翻版本。团队当天的 Playwright 运行（`12:12:53`、`12:13`）
都在 `172.18.10.99`，UA 是 `Playwright/1.64.1`，与 `.7` 完全无关。

## 3. 仓库侧的真问题（与「谁点的」无关，本次一并收口）

即便 `.7` 是人，**仓库自己也曾默认指向 prod**：`playwright.config.ts` / `e2e/global-setup.ts` /
`e2e/support.ts` 三处 `BASE_URL` 兜底都是 `http://172.18.10.144`，任何人一句
`pnpm test:e2e` 就在生产上造数（`08-t8b-task-tree.spec.ts` 里还自带一处同样的兜底）。
11:34 不是仓库脚本干的，但这个坑是同源的，已按「硬判据 + 隔离实例 + 后置检查」收口：

| 措施 | 落点 | 机制（不是文档约定） |
| --- | --- | --- |
| 目标硬判据 | `frontend/e2e/target-guard.mjs` | 纯函数 `classifyTarget()`：非回环 / 非隔离端口 / 已知共享主机一律拒绝；`playwright.config.ts`、`global-setup.ts`、`support.ts` 三处调用 `assertIsolatedTarget()`，**配置加载期**就抛错 |
| 默认目标改隔离 | `playwright.config.ts` 等 | 默认 `http://127.0.0.1:5189`（`e2e_isolated_instance.sh` 起的前端），prod 必须显式 + 双条件逃生口 |
| 逃生口留痕 | `e2e/.auth/prod-target.flag` | 线上验收确需打 prod 时 `E2E_ALLOW_PROD=1 E2E_PROD_REASON='…'`（缺一个照样拒），写标记触发后置检查 |
| 隔离实例一条命令 | `scripts/verify/e2e_isolated_instance.sh` | prod `pg_dump` → 本地副本库 `cloudpivot_e2e_iso`（owner 独立角色）+ profile=test 后端 `127.0.0.1:18089` + Redis db 9 + vite `127.0.0.1:5189`；脚本**自检端口段**、自证副本库行数、反代探测 |
| 后置检查 | `scripts/e2e-postrun-check.sh` | 只读检出 `e2e_fixture_residue.py` → 有残留就走 `00_backup.sh` + `P10_GENERIC_APPLY=1 apply.sh` → 复检 → `erp_invariants.py` 必须 32/32 / BLOCKER 0 / **WARN 0**（脚本解析 WARN 计数，因为不变量脚本对 WARN 不退出非 0） |
| 走查回归 | `frontend/e2e/12-erp-serial-batch-walkthrough.spec.ts` | 把 11:34 那条业务闭环写成用例（批量生成 → 调拨建单提交 → 待办同意），开头自检 BASE_URL 必须是回环隔离端口 |
| 判据回归网 | `frontend/tests/unit/target-guard.test.ts` | 判据被改松（删 `172.18.10.144`、把 5173 放回隔离段）立刻红 |

## 4. 复跑验证（本次实际执行）

```bash
bash scripts/verify/e2e_isolated_instance.sh up       # 副本库 + 18089 + 5189
cd frontend && E2E_BASE_URL=http://127.0.0.1:5189 \
  pnpm exec playwright test e2e/12-erp-serial-batch-walkthrough.spec.ts
bash scripts/e2e-postrun-check.sh --check-only        # prod：exit 0
```

实测结果：

| 步骤 | 实测 |
| --- | --- |
| 隔离实例 | `✅ 端口判据 18089/5189` → `✅ 副本库就绪：sys_user=25 行` → `✅ 后端就绪` → `✅ 前端就绪（/api 反代探测码 401）` → `✅ 数据判据 biz_material=7 行` |
| 同一条走查（隔离） | `3 passed (9.1s)`：① 批量生成 10 个序列号（序列号总数 40→50）② 调拨单 `TK-20261031-000009` 提交即 PENDING + 自动起审批实例 ③ 待我审批点同意 → 我的待办消失，实例/单据状态一致（PENDING/PENDING，会签节点只剩其他超管的票） |
| 夹具落在哪 | 副本库 `cloudpivot_e2e_iso`：`e2e_fixture_residue.py` **命中 M-W1A-004（库存 50 件 / 流水 5 条 / 存活序列号 50）exit 1**；同库 `erp_invariants.py` = **30/32，BLOCKER 1（B1）+ WARN 1（C3）** —— 与 11:34 prod 的红法完全一致 |
| prod 有没有被碰 | 同一时刻 prod `cloudpivot`：`e2e_fixture_residue.py` **exit 0（无残留）**，`erp_invariants.py` **32/32 / BLOCKER 0 / WARN 0** |
| 清理分支（在副本库上演练） | `e2e-postrun-check.sh --local` → 检出 → 备份 `/root/datafix-backup-p10-20261031-124437` → `10_` 清掉 50 个号/5 条流水 → `20_` 无残留 → `21_` 跳过 0 个物料 → 复检 exit 0 → **32/32 / BLOCKER 0 / WARN 0** |
| 判据自证 | `node e2e/target-guard.mjs --check http://172.18.10.144` → `REFUSED … exit 1`；`E2E_BASE_URL=prod pnpm exec playwright test --list` → 配置加载期抛错 exit 1；`run-full-rounds.sh` 打 prod → exit 2；`E2E_ALLOW_PROD=1` 无 reason → 仍拒 |

### 4.1 复跑顺手抓到的缺陷：残留判据对 `source_type=''` 假绿（已修）

第一次复跑后，副本库里明明有 50 个零成本序列号，`e2e_fixture_residue.py` 却报 **exit 0**。根因：

- `serial_model::batch_generate` 的 `source_type` 取 `params.source_type.unwrap_or_default()`，
  **请求不带该字段就落空串**（`''`），请求带 `PURCHASE` 才落 `PURCHASE`（prod 11:34 那批就是后者）；
- 旧判据 ③ 写的是「全部 `source_type='PURCHASE' AND source_id IS NULL`」，
  用 `source_type IS DISTINCT FROM 'PURCHASE'` 取反 —— 空串形态直接落在反例里 ⇒ **整批夹具漏检**。

已改成「**没有任何一条带真实来源**」：`source_type ∈ {NULL,'','PURCHASE'} 且 source_id IS NULL` 才算夹具
（`scripts/verify/e2e_fixture_residue.py` 与 `scripts/datafix/p10/20_residue_report.up.sql` 同步改，口径一致）。
改后副本库 exit 1、prod 仍 exit 0 —— 两个方向都实测过。

> 教训：`e2e_fixture_residue.py exit 0` 只有在**判据本身被验证过能命中**时才有意义。
> 本次是因为先复跑出夹具、再看判据才发现假绿；以后改这四条判据，至少要在副本库上重放一次 11:34。

## 5. 遗留（需要拍板，不在本卡改动范围）

1. **`batch-generate` 的语义缺陷**（见 `docs/datafix-20261031-p11-m-w1a-004-recur.md` §6）：
   「取号」同时落 `IN_STOCK` + 重算库存、却没有任何成本来源 ⇒ 合法用户在 prod 点一次，
   B1 + C3 必然变红。仓库侧只能「不让 E2E 点」，挡不住**真人**点。
2. **`henry.liao`（Henry.Liao，SUPER_ADMIN，账号 2026-10-28 建）在 prod 手工走查的合规性**：
   客户/业务方在线上验收是业务需要，但「在 prod 上点会写库的按钮」需要有约定
   （录成本 / 只给隔离环境开入口 / 走查后跑 `scripts/e2e-postrun-check.sh`）。
   归属与后续动作已回填 t_d75b02e7 并升级给总管。
3. 事故那次留下的 `biz_stock_transfer` **TK-20261031-000007（PENDING，申请人 admin）**
   仍停在 prod：它不是零成本夹具（不触发 B1/C3），P10 也不清它，属业务单据，
   处置需业务确认（撤销或继续审批）。
