# scripts/verify —— 云枢接口验证套件

一套**可直接上 CI** 的接口验证脚本：统一入口 `verify.py`，公共模块 `common.py`，
6 个子模块按批次/功能拆分。有 FAIL 时进程退出码为 `1`。

```bash
python3 verify.py all              # 跑全部：smoke + a + b + c + task + flow
python3 verify.py smoke            # 只读冒烟（A/B/C/D，39 项）
python3 verify.py task             # C+ 任务管理（55 项）
python3 verify.py flow             # 批次 D 审批流读写（28 项）
python3 verify.py a                # A 批次写操作（存储/通知/导入导出，17 项）
python3 verify.py b                # B 批次写操作（数据权限/脱敏/互斥，23 项）
python3 verify.py c                # C 批次写操作（公告/帮助/变更日志，23 项）
python3 verify.py a b c            # 多个子命令组合
python3 verify.py smoke --batches a,c     # 冒烟只跑指定批次
python3 verify.py task --modules t1,t2    # 任务管理只跑指定模块
python3 verify.py --list           # 列出全部子命令
```

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `BASE_URL` | `http://127.0.0.1:8023` | 被测实例 |
| `TEST_USER` | `laoliu_test` | 登录账号（需超管或全权限） |
| `TEST_PASS` | `Test@12345` | 登录密码 |
| `DB_ENV_FILE` | `/etc/cloudpivot/cloudpivot.env` | 从服务 env 文件读 `ALUN_DATABASE_*` |
| `DB_HOST` / `DB_PORT` / `DB_NAME` / `DB_USER` / `DB_PASS` | 取自上面文件 | 逐项覆盖，指向被测实例的库 |
| `DB_ENABLED` | `1` | `0` = 跳过所有 DB 断言（无 psql 时用，纯接口校验） |

`DB_NAME` 必须与 `BASE_URL` 指向的实例是**同一个库**，否则库断言会误报。

```bash
# CI 里跑隔离实例
BASE_URL=http://127.0.0.1:8033 DB_NAME=cloudpivot_verify \
DB_PASS=xxx python3 scripts/verify/verify.py all
```

## 文件结构

| 文件 | 作用 |
|---|---|
| `common.py` | 登录 / 请求 / 断言 / DB 查询 / 清理登记 / **字段契约构造器** / 扫尾 `sweep()` |
| `verify.py` | 统一入口：子命令分发、汇总、退出码 |
| `v_smoke.py` | 只读冒烟（A/B/C/D 四组 GET） |
| `v_task.py` | C+ 任务管理（T1/T2/T3/T4/T5/T5b/T6b/L1/T7a） |
| `v_flow.py` | 批次 D 审批流读写（单签/驳回/会签/非法图） |
| `v_a.py` / `v_b.py` / `v_c.py` | A / B / C 批次写操作 |

## 字段契约（踩过的坑，已固化成构造函数）

用例里**不要手写请求体**，用 `common.py` 的构造器，坑就不会再踩：

| 场景 | 正确写法 | 常见错误 |
|---|---|---|
| 流程节点 | `node(key, "APPROVAL", ...)` → `{key, type, ...}` | `node_key` |
| 流程连线 | `edge("e1", "s", "n1")` → `{key, from, to}` | 缺 `key` / 用 `source`、`target` |
| 整图 | `graph(nodes, edges)`（自动校验 key/from/to） | 直接拼 dict |
| 审批人 | `assignee(uid)` → `{type, ref_id}` | `{kind, id}` |
| 公告 | `content_md` | `content` |
| 通知 | `content`（**和公告相反**） | `content_md` |
| 帮助分类 | `code` 必填 | 只给 `name` |
| 帮助文档 | `slug` + `content_md` 必填；PUT 是**全量更新**，四个字段都要带 | 只传 `content_md` |
| 变更日志（库） | `sys_change_log.resource_id` | `target_id`（库里没这列） |
| 变更日志（接口） | `GET /changelog/timeline?table=x&target_id=y` | 以为参数也叫 `resource_id` |
| 文件删除 | `delete_file(ctx, fid)` → `DELETE /business/storage/{fid}` | `/storage/file/{fid}` |
| 通知已读 | `notice_read_all(ctx)` → `PUT /business/notice/read-all`，**body=None** | `POST`、带 `{}` |
| 单条已读 | `PUT /business/notice/read/{receiver_id}`（收件人 id） | 传通知 id |
| 导出 | `biz_type` | `code` / `def_code` |
| 列级脱敏 | `column_name` / `mask_type`(int 1..7) / `reason` 必填 / 单个 `role_id` | `field_name`、`"EMAIL"`、`role_ids` |
| 角色互斥 | `role_a_id` / `role_b_id` / `reason` | 少 `reason` |
| 流程实例终态 | 库约束 `ck_biz_flow_instance_end`：终态必须同时写 `ended_at` | 只改 `status` 被约束拒绝 |

## 怎么加新用例

**1) 加到已有子模块**（多数情况）：在对应 `v_*.py` 里加一个函数，用 `ctx` 断言：

```python
def d_xxx(ctx):
    print("\n【D-x】我的新场景")
    st, resp = ctx.post("/api/business/flow/def", {"flow_code": code, "flow_name": "x", "biz_type": "OTHER"})
    ctx.ok("建流程定义", resp)                      # 断言 code==0
    did = (ctx.data(resp) or {}).get("id")
    ctx.chk("拿到 id", bool(did), resp)             # 任意布尔断言
    # 建的东西一律登记清理，跑完自动删
    ctx.on_cleanup("删流程定义", lambda: ctx.delete("/api/business/flow/def/%s" % did))
```

然后把函数挂进该模块 `run()` 的步骤列表（`v_task.py` 则是加进 `MODULES` + `handlers`）。

**2) 加子命令**（新的一类验证）：

1. 新建 `v_xxx.py`，实现 `def run(ctx)`；
2. `verify.py` 的 `COMMANDS` 加 `"xxx": "v_xxx"`；
3. 想让 `all` 也跑，就把它加进 `ALL_ORDER`。

**3) 加字段契约**：先写进 `common.py` 顶部"字段契约"docstring，再补一个构造函数
（如 `xxx_body()`）；用例只用构造函数 —— 下次契约变了只改一处。

## 清理与扫尾

- 每条用例建的对象都要 `ctx.on_cleanup(...)` 登记，`verify.py` 跑完每个子命令都自动执行（逆序，单条失败不影响其余）。
- 有些残留 API 删不掉，由 `common.sweep()` 在**每次运行结束时**按「本次运行开始时间」窗口兜底清理：
  1. 任务/流程联动留下的在跑审批单（任务进 REVIEW 会拉起 `task_accept` 流程，任务删了审批单还挂着）；
  2. 孤儿 AI 任务令牌 `biz_task_ai_token`；
  3. 运行期产生的通知（`TODO` / `SYSTEM` / `CIRCULAR`）。
- 两个库约束必须先知道，否则「取消」会被静默拒绝（`psql` 只往 stderr 报错）：
  `ck_biz_flow_instance_end`：`(status='PENDING') = (ended_at IS NULL)`；
  `ck_biz_flow_task_done`：`(status='PENDING') = (completed_at IS NULL)`。
  → 改终态必须同时写 `ended_at` / `completed_at`。
- `sweep()` 只动 `created_at >= 本次运行开始` 的行，不碰别人的数据；`DB_ENABLED=0` 时跳过。

## 两个环境坑

1. **多实例共用同一个 Redis**：缓存键会串（例如 `/system/role/list` 返回另一个实例缓存的角色 id）。
   脚本已改为「角色 id 从库取」，但排查问题时别把缓存串味当成接口 bug。
2. **自审跳过**：节点默认 `self_approval=SKIP`，发起人自己就是审批人时待办直接 `SKIPPED` 并自动前进，
   实例一步到 `APPROVED`、测不到「待办 → 同意/驳回」。`v_flow.py` 因此现建临时审批人
   （授超管角色拿 `business:flow:approve` 权限）用**别人的 token** 去批，跑完删掉。

## 基线

干净库（迁移全跑 + 种子流程 `task_accept`、仅 `admin`/`laoliu_test` 两个用户）实测：

```
smoke=39  a=17  b=23  c=23  task=55  flow=28   →  PASS=185  FAIL=0  SKIP=0   退出码 0
```

## ERP 账务类脚本（不走 verify.py 的分发，单独跑）

| 脚本 | 作用 | 退出码 |
|---|---|---|
| `erp_invariants.py` | 65 条不变量审计（会计恒等 / 账账 / 账实 / 单据链 / 状态 / 期间 / 闭环 / 审批闭环 / 标准标签订正 / 任务 / 权限 N1 / E2E 标记 O1）。**只认 `NIK_DATABASE_*`**，跑前必须 `set -a; . /etc/cloudpivot/cloudpivot.env; set +a` | 有 BLOCKER → 1 |
| `e2e_fixture_residue.py` | 「批量生成零成本夹具」残留检查（只读）。这类行会让 B1（BLOCKER）/ C3（WARN）同时红，e2e 写完数据库建议挂一个后置检查。**判据见下面「语义变化」一节，ADR 落地后不再命中普通批量生成** | 有残留 → 1；连库失败 → 2 |
| `e2e_marker.py` | **e2e 夹具标记的唯一判据源**：`[E2E]` 任务 / `E2E-` 仓库 / `e2e*` 上传件导出件 / 通知公告标题 / 挂着标记任务的评论，外加 6 个已知测试账号是否还在用。三处复用它：`erp_invariants.py` 的 **O1**、`datafix/e2e_residue3/apply.sh` 的收尾交叉断言、`frontend/e2e/target-guard.mjs` 的 prod 后置清单。`--self-test` 钉住「误伤反例」（正文提到 e2e 的真实汇报/通知）+ 前端 `E2E_MARK` 一致性 + 两处接线是否还在 | 有残留 → 1；连库失败 → 2 |
| `p11f_fixture_residue_drill.sh` | 注入式演练：在 prod 副本库上重放「批量生成夹具」→ 检出 → 清理 → 全绿 → 幂等 → 回滚（31 条断言） | 有 FAIL → 1 |
| `snapshot_audit.py` | **快照一致性巡检（16 项 `di.c_di01..c_di16`，只报不修）**：把 `docs/architecture/快照一致性巡检与告警设计.md` §2.4 的巡检项落地。合同链 / 实物链 / 资金链 / 主档 / 护栏五组；`--json` / `--group` / `--dry-run` / `--install-views`。只读事务（`BEGIN READ ONLY` + 会话级 `default_transaction_read_only=on`），**没有一行 DML**；发现漂移只写日志+JSON，绝不回写业务数据（AP-8） | 有 BLOCKER → 1；环境/用法错 → 2；上一轮在跑 → 3 |

```bash
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
python3 scripts/verify/erp_invariants.py          # 账是否干净
python3 scripts/verify/e2e_fixture_residue.py     # 有没有人往库里造夹具（exit 1 = 有）
```

### 快照一致性巡检（B1）怎么跑

```bash
# 0) 首次（或 di 视图被 drop 后）：幂等装巡检视图（只建 di schema 的视图，回滚 DROP SCHEMA di CASCADE）
python3 scripts/verify/snapshot_audit.py --install-views

# 1) 人看：五组一节，每条一行 ✓/⚠/✗，末尾 BLOCKER x / WARN y / INFO z
python3 scripts/verify/snapshot_audit.py

# 2) 机看：stdout 纯 JSON（人看格式转 stderr），可 --out 落文件
python3 scripts/verify/snapshot_audit.py --json --out /var/log/cloudpivot/snapshot-audit.latest.json

# 3) 只跑一组 / 只跑一条 / 只看计划
python3 scripts/verify/snapshot_audit.py --group 资金链
python3 scripts/verify/snapshot_audit.py --group DI-06
python3 scripts/verify/snapshot_audit.py --dry-run

# 4) cron 档（每日 02:00，已在 crontab 里；可手工触发一次验证通路）
bash scripts/cron/snapshot_audit.sh
```

判读铁律（设计文档 §3.5）：**`⊘ 跳过` 不是通过** —— 合同 schema 未就位时 DI-01…05/13/15
记 SKIPPED，因为「0 行」既可能是干净，也可能是根本没查。看到跳过就说明这组没人看着。

**e2e 跑完请务必**：① 不要直连 prod（8023）造数；② 跑完调一次
`e2e_fixture_residue.py`，有残留就按它打印的三条命令清（备份 → `--check` →
`P10_GENERIC_APPLY=1 apply.sh`）。留痕见 `docs/datafix-20261031-p11-m-w1a-004-recur.md`。

## 语义变化（2026-10-31 ADR 拍板）：批量生成 = 无来源入库 ⇒ 库存不再是「零价值夹具」

**判据没改，改的是它抓到的东西。** ADR
`docs/adr-20261031-erp-serial-batch-generate-semantics.md` §3（+§6 R1/R2）落地后：

| | 旧行为（2026-10-31 11:34 以前） | ADR 之后（`68b9d02` 起） |
|---|---|---|
| 请求 | 无 `unit_cost` | 必录 `unit_cost`（缺/≤0/非数字 ⇒ `400 + 10001`，零落库） |
| 成本账 | `qty>0 / amount=0 / avg_cost=0` | `amount = N × unit_cost`、`avg_cost` 走移动加权 |
| 库位账 | **不写** | 同事务写（`apply_location_qty`） |
| 凭证 | 无 | 生成 借 1405 / 贷 1901；出库 借 6401 / 贷 1405；报废 借 1901 / 贷 1405 |
| 夹具判据第 ④ 道「库存零价值」 | **命中** → 检出 | **不命中** → 不再算夹具 |
| `erp_invariants.py` | B1（BLOCKER）+ C3（WARN）+ B2 红 | 全绿（通过数=总条数），B1/B2/C3 全绿 |

**结论（对处置流程的硬约束）**：

1. 此后「批量生成」写进去的库存是**真实入库**（有成本、有两本账、有凭证）。
   走查点出来的这类数据**撤销要走红冲 / 盘点**，**不能指望 P10 清** ——
   `20_residue_report.up.sql` 第 ④ 道「有价值就一律不碰」会（正确地）跳过它。
2. **第 ④ 道判据不许改**：删有价值库存 = 毁账。要清只能走业务单据。
3. e2e 收尾不再依赖「跑完 residue 检出 → P10 清」这条路；隔离实例的收尾方式是
   **换一份新副本库**（`E2E_ISO_FRESH=1`），别在副本库里手工删账。

### 双向自测（证明判据两个方向都有牙）

同一条物料、同一条 `SERIAL_IN/SERIAL_BATCH` 流水，**只差「有没有成本」**：

```bash
# A. 新形态：隔离实例上真跑 spec 12（批量生成 10 件 @12.34），落到副本库
set -a; . /tmp/e2e-iso-t995/iso.env; set +a          # NIK_DATABASE_* → 副本库
python3 scripts/verify/e2e_fixture_residue.py        # → exit 0（不误判）
python3 scripts/verify/erp_invariants.py             # → 全绿  BLOCKER 0  WARN 0

# B. 旧形态：把同一批数据打回 11:34 的落库形态（零成本 + 不写库位账）
psql -c "update biz_inventory i set amount=0, avg_cost=0 from biz_material m
          where m.id=i.material_id and m.material_code='M-W1A-004';
         delete from biz_inventory_location l using biz_material m
          where m.id=l.material_id and m.material_code='M-W1A-004';"
python3 scripts/verify/e2e_fixture_residue.py        # → exit 1（命中 M-W1A-004）
python3 scripts/verify/erp_invariants.py             # → 29/32  B1+B2 🚨  C3 ⚠️
```

2026-10-31 实测（副本库 `cloudpivot_e2e_iso_t995` / 演练库 `cloudpivot_t995_residue`）：

```
# A（新形态，带成本）
  ✅ 无残留：没有「无单据、零价值、批量生成」的物料 —— B1/C3 不会被这类夹具打红
  ✅ [账账] B1 库位账合计 = 成本账 qty
  ✅ [账实] C3 有库存则成本价非零
  结果：N/N 通过   🚨 BLOCKER 0   ⚠️ WARN 0

# B（旧形态，零成本 + 无库位账）
  ❌ 命中 M-W1A-004：库存 1 行 / 数量 10.0000；流水 1 条；存活序列号 10（全部 31 条）
  🚨 [账账] B1 库位账合计 = 成本账 qty
  🚨 [账账] B2 库存金额 = 存货科目 1405 余额   — 库存模块 435.0000 / 总账 1405 558.4000 / 差 -123.4000
  ⚠️ [账实] C3 有库存则成本价非零
  结果：29/32 通过   🚨 BLOCKER 2   ⚠️ WARN 1
```

判据本身（四道同时成立才算夹具）见 `e2e_fixture_residue.py` 文件头与
`scripts/datafix/p10/20_residue_report.up.sql`，**两处必须同改**。

### 已知盲区（2026-10-31 16:17 实测发现，未改判据，登记待评估）

第 ③ 道把「有真实来源」判成 `source_type NOT IN ('', 'PURCHASE')`，于是
**`source_type='PRODUCE'` 且 `source_id IS NULL`** 的零成本批量生成会被当成「有来源」→ **漏检**：

```
prod 16:17:24（henry.liao / 172.18.10.7，旧代码）：批量生成 10 件 @0，source_type=PRODUCE
  e2e_fixture_residue.py  → ✅ 无残留（漏检，假绿）
  erp_invariants.py       → B1 🚨 BLOCKER + C3 ⚠️（30/32）
```

`source_id IS NULL` 才是「没有来源单据」的硬判据；`source_type` 非空不代表有来源单。
本次**没有动判据**（卡面要求判据不改，且改判据要连 `20_residue_report.up.sql` 一起改）——
但**组合闸门仍然有效**：`e2e-postrun-check.sh` 第 ③ 步的不变量会把这种形态抓住，
所以「① 干净」不等于「全绿」，两步必须都跑。


## e2e 隔离实例（E2E 唯一允许的目标）

2026-10-31 11:34 prod 被写进一批零成本序列号夹具（源头取证见
`docs/e2e-prod-fixture-incident-20261031-172.18.10.7.md`）。仓库侧的收口是
「硬判据 + 一条命令起隔离 + 跑完必查」，两处落点：

| 脚本 | 作用 | 退出码 |
|---|---|---|
| `e2e_isolated_instance.sh` | 起 e2e 隔离实例：prod `pg_dump` → 本机副本库 `cloudpivot_e2e_iso`（独立 owner 角色）+ profile=test 后端 `127.0.0.1:18089`（Redis db 9）+ vite `127.0.0.1:5189`；端口段不合法 / 被占用直接拒绝，起完自证副本库行数、反代探测 | 失败 → 非 0 |
| `../e2e-postrun-check.sh`（`scripts/`） | **打过 prod 就跑它**：只读检出零成本夹具 → 有残留走 `00_backup.sh` + `P10_GENERIC_APPLY=1 apply.sh` → 复检 → `erp_invariants.py` 必须全绿 / BLOCKER 0 / **WARN 0**（不变量脚本对 WARN 不退出非 0，本脚本显式解析） | 达标 → 0；仍有残留/不达标 → 1；环境问题 → 2 |

```bash
# ① 起隔离实例（默认从 prod 复制一份库，只读 dump）
bash scripts/verify/e2e_isolated_instance.sh up
#   ✅ 端口判据：后端 18089 / 前端 5189 都在隔离段且空闲
#   ✅ 副本库就绪：sys_user=… 行 / ✅ 后端就绪 / ✅ 前端就绪
export E2E_BASE_URL=http://127.0.0.1:5189
cd frontend && pnpm test:e2e        # target-guard 会在配置加载期拒绝任何非隔离目标

#   18089/5189 被别人占着时用 env 覆盖（脚本仍按隔离段判据自查；别去抢别人的实例）
#   E2E_ISO_DB=cloudpivot_e2e_iso_t995 E2E_ISO_BACKEND_PORT=18094 E2E_ISO_FRONTEND_PORT=5194 \
#   E2E_ISO_RUN_DIR=/tmp/e2e-iso-t995 E2E_ISO_FRESH=1 \
#     bash scripts/verify/e2e_isolated_instance.sh up

#   流程 12（批量生成 → 调拨 → 审批）单跑：ADR 之后批量生成**必录成本单价**，
#   想连三账不变量一起断，再给 E2E_RUN_INVARIANTS=1 + NIK_DATABASE_*（**必须指副本库**）
E2E_BASE_URL=http://127.0.0.1:5194 E2E_RUN_INVARIANTS=1 \
NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_NAME=cloudpivot_e2e_iso_t995 \
NIK_DATABASE_USER=e2erole NIK_DATABASE_PASSWORD=… \
  npx playwright test e2e/12-erp-serial-batch-walkthrough.spec.ts --reporter=list
#   ① 断「不填单价被拦」（UI 本地拦 + 后端 400/10001）+ 零落库
#   ② 断 avg_cost = unit_cost / amount = N × unit_cost / qty = N / 库位账合计 = N / 流水写实成本
#   ⑤ 跑 erp_invariants.py，断全绿（通过数=总条数）/ BLOCKER 0 / WARN 0，并单独核 B1 / B2 / C3 三行

# ② 线上验收确需打 prod 时（E2E_ALLOW_PROD=1 + E2E_PROD_REASON），跑完必须：
bash scripts/e2e-postrun-check.sh            # 检出 → 清理 → 全绿，exit 0 才算跑完
bash scripts/e2e-postrun-check.sh --check-only   # 只看不治（巡检 / 告警用）
```

判据实现是 `frontend/e2e/target-guard.mjs`（单测 `frontend/tests/unit/target-guard.test.ts`），
回环 + 18000-18999 / 5100-5999 才算隔离；`5173`、`8023`、`80/443`、`172.18.10.144`、
`172.18.10.7` 一律拒跑。**不要**把它写成「文档约定」——三道闸口（config / global-setup / support）
都是加载期抛错。

---

## 真库 lib 单测：**按模块分库 + 分进程**跑（`run_lib_tests_by_module.sh`）

`cargo test --lib` 里连真库的那批用例（`ERP_DB_URL` / `TASK_DB_URL` / `P0FIX_DB_URL`）**不能把几个模块
串在同一个库上跑**：模块之间会互相踩（flow 段写的流程定义/实例/待办会被 erp 段"全库口径"的断言判死；
`internal_control` 的 `P0FIX_DB_URL` 还会因为进程里 `set_db()` 先到先得被静默忽略）。
根因、复现命令与证据见 [`docs/测试卫生-真库单测分库跑法.md`](../../docs/测试卫生-真库单测分库跑法.md)。

```bash
# 4 个模块（changelog / flow_p0fix / flow / erp）各一个干净库、各一个进程，跑完汇总
bash scripts/verify/run_lib_tests_by_module.sh

# 交卡 / 发版前的守门：同一个库连跑两遍都必须 0 failed，且两轮 passed 数一致
RUNS=2 bash scripts/verify/run_lib_tests_by_module.sh

# 只跑某几个模块 / 自定义条目
MODULES="erp" RUNS=2 bash scripts/verify/run_lib_tests_by_module.sh
MODULES="mymod=business::erp::contract_" bash scripts/verify/run_lib_tests_by_module.sh
```

| 做了什么 | 为什么 |
|---|---|
| 每个模块 `createdb -T 模板库`（`cp_libtest_<模块>`，另配 `…_p0fix`） | 库里不许有别的模块的行 |
| 每模块单独一个 `cargo test` 进程 + `--test-threads=1` | 一个进程只有一个全局 `db()`，天然免疫 `set_db` 先到先得 |
| preflight 断言库是干净的（实例/待办/采购单/`p0fix_*` 定义全 0） | 有人在脏库上跑时立刻红，而不是产出 60 多条假红 |
| `flow_p0fix` 单列模块 + 泄漏校验（P0FIX 库 ≥1 条 `p0fix_*`、模块库 0 条） | 守住 `P0FIX_DB_URL` 被静默忽略这个坑 |
| `flow` 模块显式 `--skip business::flow::internal_control::db_tests` | 那 7 条要求「连的必须是 P0FIX 库」，`ensure_db()` 现在会因此显式失败（卡 `t_f38d1f45`）；它们归 `flow_p0fix` 模块独占进程跑。`flow` 338 + `flow_p0fix` 7 = 原来那 345 条，一条没少 |
| 第 2 轮 passed 数必须等于第 1 轮 | 幂等性：防"某轮 SKIP 变多"式的假绿 |
| 任一模块任一轮失败 → `exit 1` | 可直接给 CI 用 |

模板库 `cloudpivot_libtest_template` 不存在时，脚本会用后端二进制跑一次启动迁移 + 种子自动建出来
（二进制不在就先 `cargo build --bin cloudpivot`）。`check.sh` 只跑 fmt/clippy/check/typecheck，
**不跑**真库用例，两者互不替代。

---

## 真库全量的「隔离性」验收（`erp_full_suite_twice.sh` / `erp_module_concurrency_check.sh`）

单跑一遍绿 ≠ 用隔离干净：真正的验收口径是**同一个库连跑两轮都 0 failed**（第二轮是在第一轮的残留上跑），
以及**受影响模块在 `--test-threads=4` 下重复跑**。偶发假红的复现率约 1/2 轮，跑一遍看不见。

```bash
# ① 建干净库 + 全量两轮（任一轮非 0 failed → exit 1）
ERP_DB=cloudpivot_ftwice bash scripts/verify/erp_full_suite_twice.sh
ROUNDS=3 ERP_DB=... bash scripts/verify/erp_full_suite_twice.sh

# ② 受影响模块并发自检（7 个付款/凭证/期次/合同模块 × 3 轮）
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_ftwice \
  bash scripts/verify/erp_module_concurrency_check.sh 3

# ③ 库侧复查（纯 psql，只读；夹具号段 / 夹具用户 / 状态日志孤儿 / 种子流程行 / 测试期次）
DB=cloudpivot_ftwice bash scripts/verify/erp_fixture_residue_check.sh
```

口径与实证（含一条真跑的对照：改动前 2323 passed / 1 failed → 改动后两轮 0 failed）见
[`docs/evidence/20261104-testfix-parallel/README.md`](../../docs/evidence/20261104-testfix-parallel/README.md)。
铁律：**同一批全局行 = 同一把锁**；清场只许动「本用例造的行」；整库型断言必须能容忍别人正在删的瞬时窗口。

---

## 菜单结构不变量（`menu_invariants.sh`，卡 MENU-02）

防止「迁移/datafix 把 `sys_menu` 结构改坏」的机械闸门。纯只读（只 SELECT），任何环境都能直接跑。

```bash
# 连接参数取 NIK_DATABASE_*（或 PG*），库名可显式给
bash scripts/verify/menu_invariants.sh cloudpivot_dev
echo $?     # 0=全绿 ｜ 1=有违规（明细已打印）｜ 2=连库/查询失败
# 注意：⑧ 在「找不到图标库」的机器上会打印 ⏭ 跳过并**仍然 rc=0**（细节见下面 ⑧ 一节）——
# 跳过 ≠ 通过，结论行会写出「有跳过」，要严格校验的环境设 MENU_ICON_LIB_REQUIRED=1（缺库 rc=2）。
```

8 条断言（任一不过 → 打印前 10 条明细 + exit 1）：

| # | 断言 | 判据 |
|---|---|---|
| ① | 孤儿 parent | `parent_id` 指向不存在或已软删的记录 ⇒ 0 |
| ② | /system/ 归属 | `path ~ '/system/%'` 的页面/按钮（menu_type 2/3）必须挂在 `system` 目录（`10000000-…-0001`）下 |
| ③ | /business/ 归属 | `path ~ '/business/%'` 沿 parent 链上溯到的**根**必须落在 /business 命名空间（两个显式豁免见下） |
| ④ | path 唯一性 | `menu_type=2` 且未软删的记录中同一 `path` 不得出现多次 |
| ⑤ | 菜单总数 | 未软删总数 ∈ [400, 520]（`MENU_TOTAL_MIN`/`MENU_TOTAL_MAX` 可覆盖）。上限只在实测基数上留 ~5~8% 余量（420→460 于 2026-11-08，因 P2-01/02/03 三个采购闭环新增 13 条菜单，410→423；460→520 于 2026-11-11，成本核算 + 人力资源两个模块同批落地 36 条，459→495）；它防的是批量误插/误删，不是功能增长闸门 |
| ⑥ | `title` 长度 | `menu_type IN (1,2)`（侧栏真正显示的目录/页面两层）的 `title` 长度 ≤ 12 字（`MENU_TITLE_MAX` 可覆盖）。`title` 是侧栏显示名，写成功能说明（`即时 / 月末汇率维护 + 按月批量录入（ARCH-7 §15.6.3 ②）`，38 字）就换行/截断 |
| ⑦ | 同父 sort 唯一 | 同一 `parent_id` 下 `menu_type=2` 的 `sort_order` 不得重复（重复 ⇒ 侧栏顺序依赖返回行序，刷新可能变一次）；`parent_id IS NULL` 的顶级页面算一组 |
| ⑧ | `icon` 名合法 | `menu_type IN (1,2)` 且 `icon` 非空的记录，`icon` 必须是 **TDesign 图标库里真实存在的名字**（且只含小写字母/数字/连字符）。名字不存在时 `<t-icon :name>` 渲染空节点、侧栏那项**没有图标**（`icon` 非空所以组件里的 `folder`/`app` 兜底也不触发） |

### ⑥⑦ 的口径与已知边界（卡 MENU-FIX-2）

**为什么加这两条**：145/全新库的实测缺陷不是「个别手滑」——`sys_menu.title` 被当作功能说明列用了，最长 38 字并带 `ARCH-7 §15.6.3` 这类开发文档引用；`sort_order` 有 5 组同父撞车。数据修复走 `V0335__menu_title_shorten_and_sort.up.sql`（卡 MENU-FIX-1），**规范**进 `docs/编码规范.md` §1.6.1，**闸门**就是这里的 ⑥⑦。两者缺一不可：只修数据，下次迁移照样写长句。

| 边界 | 为什么这么划 | 实测 |
|---|---|---|
| ⑥ 只管 `menu_type IN (1,2)` | `menu_type=3` 是按钮权限点，不进侧栏，是「授权树里的一句话」而不是菜单名；把它一起判红会逼着为 155 行改文案 | 145：mt=3 里 155 条 >12 字、最长 73 字（`新建销售退货单（取号 SR-yyyyMMdd-000001；…）`） |
| 12 是硬顶不是目标值 | 规范要求 2-6 字，硬顶留余量给少量正常偏长的业务名 | 145 基线 `menu_type 1/2` 最长 10 字、共 108 行 |
| ⑦ 只管 `menu_type=2` | 照规范校验 SQL 的字面口径；跨类型（目录 vs 页面）同 `sort_order` 属排序噪声，判红要动历史数据 | 145 顶级 1 组：`通用底座`(mt=1) / `合同管理`(mt=2) 同为 75 —— 已登记待评估，不在断言内 |

**牙齿验证（145 临时库 `menu_inv_teeth2`，验完已删库，不碰业务库）**：

```bash
# 402 行基线：7 条全绿 rc=0
# 注入 ①「演示页面」title 改成 38 字长句 + ② 同父再插一条 sort_order=10 的页面
bash menu_invariants.sh menu_inv_teeth2
# ⇒ ⑥=1 ❌（打印那行 38 字 title）、⑦=1 ❌（打印父/ sort / 2 个兄弟名）、rc=1
```

顺带验证了另一条硬规矩：临时表没给 `cloudpivot` 授权时，14 次查询全 `permission denied`，脚本**rc=2 且明确说「基础设施问题，不是结构违规」** —— 没有把连库/权限故障降级成结构违规去骗迁移。

### ⑧ 的口径与已知边界（卡 t_595098fe，2026-11-09）

**为什么加这条**：`sys_menu.icon` 存的是 **TDesign 图标组件名**，名字不存在时
`SideMenuItem.vue` 的 `<t-icon :name="item.icon" />` 渲染**空节点** —— 侧栏那一项就没有图标；
而且 `icon` 是非空串，连组件里的兜底 `props.item.icon || (hasChildren ? 'folder' : 'app')` 也不会触发。
这一类问题 DB 侧完全「合规」（除了这个断言没人查），只能靠人眼看出来，**已经复发两次**：

1. `V0170` 把 `/business/erp/purchase-order` 的 `shopping-cart` 改成 `cart`；
2. 生产(144) 那条同一逻辑菜单的 path 是 `/business/erp/purchase`（V0319 头注释记的 prod/dev 分叉），
   **没被 V0170 覆盖** ⇒ 老大在生产上看到「采购订单没有图标」，`V0353` 才补上（卡 MENU-FIX-3）。

**判据**：`menu_type IN (1,2)` 且 `icon` 非空的记录，`icon` 必须

1. 只含 `^[a-z0-9][a-z0-9-]*$`（TDesign 名字的实际字符集；换行/空格/大写一律非法），**且**
2. 能在图标库名字清单里找到同名项。

第 1 条不是洁癖：icon 里若塞了换行，`'bill\nmoney'` 这种值在「一行一条」的差集里会被拆成
`bill` + `money` 两行，而两段**各自都是合法名字** ⇒ 假绿。所以形状单独判，明细里用
`\n` / `\t` 可见化打印（这一条由牙齿脚本的 S3b 场景守着）。

**图标库从哪来**（`icon_lib_load`，先到先用）：

| 序 | 来源 | 说明 |
|---|---|---|
| ① | `MENU_ICON_LIB=<路径>` | 显式指定：`tdesign-icons-vue-next/esm/components` 目录，或一行一个名字的文本清单，或 `esm/manifest.js`、`IconSelect/icons.ts`（自动识别三种格式） |
| ② | `<repo>/frontend/node_modules/tdesign-icons-vue-next/esm/components` | **真实安装版本，最权威**（`components/*.js` 与 `manifest.js` 的 `stem` 实测一一对应，各 2356 个） |
| ③ | `<repo>/frontend/src/components/IconSelect/icons.ts` | 仓库内清单（`ICON_NAMES`，2356 条，由已安装版本枚举生成）。只在 ① ② 都拿不到时兜底——升级依赖后它可能落后，那时新图标会被误报红，所以有 node_modules 就永远不用它 |

**找不到图标库时**：打印 `⏭ 跳过（找不到图标库 — 试过：…）` + 「这是没校验，不是校验通过」，
结论行改写成 `结果: 全绿 ✅，但**有跳过**：⑧icon名(无图标库)`，**rc 仍为 0**（prod 机 / CI /
瘦容器不一定装过前端依赖，不能因此报红）。要「必须校验」的环境设 `MENU_ICON_LIB_REQUIRED=1`
⇒ 缺库按基础设施问题 **rc=2**（本仓库对假绿敏感，别静默绿）。

**一次 SQL + 一次列举 + 内存差集**：`select distinct icon, path …`（形状合法的部分）配
`awk 'NR==FNR{ok[$1]=1;…}'` 做差集，**不每行 fork**（60 个 icon / 400+ 行菜单）。

**现状基线（2026-11-09 实测）**：

| 环境 | distinct icon | ⑧ 结果 |
|---|---|---|
| 145 dev `cloudpivot_dev`（432 行） | 60 | **0 ✅ rc=0**（8 条全绿） |
| 144 prod `cloudpivot`（431 行，V0353 未发布） | 61 | **1 ❌ rc=1**：`shopping-cart /business/erp/purchase` —— 其余 7 条全绿，只有 ⑧ 抓到真问题 |
| 本地 prod 形状副本（`cloudpivot_r2`，path 是 `/business/erp/purchase`） | — | 1 ❌ 同一行；证明闸门与 path 分叉无关 |
| 老库 `cloudpivot_test_iso2` | — | 报 `invoice /business/erp/expense`（历史错名，已在新种子里改成 `bill`） |

⚠ **144 会在下一次发布前一直 ⑧ 红**（`V0353` 需要发布才应用，发布卡见 t_881cfdfb）。
`deploy_check_v5` 夜里跑到它会照实报 `【MENU-STRUCT】`，这是**正确行为**，别当成新事故。

复现命令（只读，任何装了 pg 的机器）：

```bash
PGPASSWORD=... PGUSER=... bash scripts/verify/menu_invariants.sh <库>; echo $?    # ⑧ 有问题 ⇒ rc=1
MENU_REPO_ROOT=/opt/cloudpivot bash /tmp/menu_invariants.sh <库>                   # 脚本被拷到仓库外时要指仓库根
MENU_ICON_LIB=/path/to/esm/components bash scripts/verify/menu_invariants.sh <库>  # 指定图标库
```

**牙齿（注入 → 报警 → 复原）**，一条命令跑完 40 项自检：

```bash
# 只在一次性副本库上跑（脚本会写 icon，别指业务库/生产库）：
createdb -O alun cloudpivot_icon8_xxx   # 或 su postgres -s /bin/sh -c 'createdb …'
pg_dump -t public.sys_menu cloudpivot_dev | psql -d cloudpivot_icon8_xxx
DB=cloudpivot_icon8_xxx PGPASSWORD=... PGUSER=... bash scripts/verify/icon_invariant_teeth.sh
# ⇒ PASS=40 FAIL=0；证据（每场景原始输出）落在 $OUT，默认 mktemp 目录
```

覆盖的场景：S1 基线全绿 → S2 注入 `shopping-cart`（rc=1、打印坏名+path）→ S3 再注入
`invoice`（两条都报）→ S3b `bill\nmoney`（形状非法，防假绿）→ S4 按原值复原（rc=0）→
S5 无图标库（⏭ 跳过、rc=0、结论写明有跳过）→ S6 无图标库 + `MENU_ICON_LIB_REQUIRED=1`（rc=2）→
S7 `MENU_ICON_LIB` 给残缺清单（rc=1，证明覆盖生效）→ S8 无 node_modules 但有 `icons.ts`（兜底全绿）→
S9 `MENU_ICON_LIB` 指 `manifest.js`（解析出 2356 个名字）→ S10 收尾复核。

**③ 的口径与卡面字面写法不同，这是故意的**：本仓库 ERP 各模块目录（`/business/erp/{finance,sales,warehouse,purchase}-mgmt`、`master-data`、`contract`）本身就是一级根，`/business` 通用底座并没有把它们挂在下面——按「必须找到 `path='/business'` 的祖先」的字面口径，既有基线就有 63 条"违规"，脚本会永远红。所以 ③ 判的是**「/business 子树不能挂到非 /business 命名空间的根上」**（例如误挂到 `/system`、`/project` 下），这才是 MENU-01 那类错位的回归面。

### ③ 的两个显式豁免（卡 t_197ecb8e 评估结论：**不动**）

`/notice-center`（公告与消息）是一级根，其下 4 条页面 `path` 仍在 `/business/` 命名空间：

| id | path | name | 出处 |
|---|---|---|---|
| `20000000-…-0710` | `/business/announcement` | 公告管理 | V0085 |
| `20000000-…-0410` | `/business/notice/template` | 消息通知 | V0065 |
| `23000000-…-0010` | `/business/notice/message` | 消息中心 | V0065 |
| `23000000-…-0020` | `/business/notice/send` | 发送通知 | V0065（410 的子节点） |

**为什么不收敛（改 path 或改挂）**：
1. 根是产品决策 —— `V0133` 是老大 2026-10-30 明确要求「公告与消息」目录，把 4 个菜单收进来；`path` 是菜单被搬进该目录**之前**的历史值，不是搬家时改错了。
2. 改名不是只改数据库：前端 `router/index.ts` 的静态路由 `path` 必须与 `sys_menu.path` **逐字一致**（否则侧栏点了 404），另有 `NoticeBell.vue` / `NoticeToast.vue` / `stores/badge.ts` / `utils/login-unread.ts` / 通知模板页与发送页之间的 `router.push` 硬编码及文档，属净做工作量、零功能收益的回归面。
3. 结构闸门的真实回归面是「误挂到别的命名空间」，本例外是**产品分组**而非错位；显式登记比改数据更安全。

两个环境变量（默认值即下表，置空＝关闭该豁免）：

| 变量 | 默认 | 含义 |
|---|---|---|
| `MENU_BIZ_EXTRA_ROOTS` | 空（不启用） | 根级白名单：这些根下**任意** `/business/*` 都放过。默认不启用——它会把未来的新错位一起放过 |
| `MENU_BIZ_EXTRA_IDS` | 上表 4 个 id | 行级例外表：**只有这 4 行**豁免，新错位照样报红 |

行级例外表每次运行都做体检：例外 id 缺失（被软删 / 已改名出 `/business/*`）或已合规（父链已落在 /business 命名空间）时打印 `⚠ 失配 N` 要求清理；**只提示不改退出码**（例外过期不等于结构坏了）。

**复现判据（注意）**：旧版写 `${MENU_BIZ_EXTRA_ROOTS:-…}`，空串会回落默认值，`MENU_BIZ_EXTRA_ROOTS=` 根本关不掉——MENU-02 卡面「把白名单置空即会报这 4 条」是不成立的。现已改 `${VAR-default}`：`MENU_BIZ_EXTRA_IDS= bash scripts/verify/menu_invariants.sh <db>` 才真能复现 ③=4 / rc=1（145 测试库与 144 生产库实测一致）。

**挂进流程**（`/root/test_deploy.sh` 与 `/root/.hermes/profiles/laoliu/scripts/deploy_check_v5.sh`，均在仓库外）：
- `test_deploy.sh` ⑦ 步：迁移 + 重启 + 登录冒烟之后跑本脚本，**只报警不阻断**（测试环境要能进去排查）；
- `deploy_check_v5.sh` ⑦ 步：对 145 测试库与 144 生产库各跑一次，违规/执行失败 → 建（或追加到）`【MENU-STRUCT】` 卡。

### rc=2 的两种成因，别再互相冒充（2026-11-08 假告警复盘）

**事故**：`deploy_check_v5` 00:00 那轮，145 测试与 144 生产同时 `rc=2「⛔ 连不上库」`，建了 `【MENU-STRUCT】` 卡但卡面明细为空。
实测两库 5 条断言全绿（菜单总数 410），**没有任何结构违规**，是脚本自身把「客户端噪声」当成了连库失败。

**根因链**（两处叠加，缺一不犯）：
1. 本机 `psql` 是 Debian 的 `pg_wrapper`（`/usr/bin/psql -> ../share/postgresql-common/pg_wrapper`，**Perl 实现**）；
2. 调用方环境带着目标机没装的 locale —— cron/ssh 的 `SendEnv LANG LC_*`（`/etc/ssh/ssh_config:50`）会把开发机的 `LANG=zh_CN.UTF-8` 带进 145/144，而两台机都没装 zh_CN；
   ⇒ Perl 往 **stderr** 打十几行 `perl: warning: Setting locale failed…`，**而 psql 本身仍以 0 退出**。
3. 旧版 `sql()` 用 `2>&1` 把 stderr 并进返回值：`select 1` 的 `"1"` 变成「警告串 + 1」⇒ 连库自检判 `!= "1"` ⇒ 打印「⛔ 连不上库」并 `exit 2`。

**修法**（`menu_invariants.sh` 内，共四处）：
- 钉死 `LC_ALL=C` / `LANG=C` / `unset LANGUAGE` / `PERL_BADLANG=0`，并 `PGCLIENTENCODING=UTF8` 保住中文菜单名不被 `SQL_ASCII` 走样；
- `psql` 加 `-X` 忽略 `~/.psqlrc`（谁在里面写 `\timing on` 会犯同一类病）；
- `sql()` 改成 **stderr 永不并入返回值**：只把 stdout 当值，成功时的警告单独转本脚本 stderr；
- `sql()` 失败不再 `exit 2`（调用点都在 `$(sql …)` 子 shell 里，`exit` 只杀子 shell，主流程会把空串误判成结构违规 rc=1）：改为写共享错误日志，**结论前统一 `-s` 判定 → rc=2**，并打印「基础设施问题，不是结构违规，别照着建迁移」。

**实机验收（145 全量 8/8，144 只读 3/3）**：

| 用例 | 期望 | 145 | 144 |
|---|---|---|---|
| 旧版 + `LC_ALL=zh_CN.UTF-8`（复现 00:00 输入） | rc=2「连不上库」 | ✅ 逐字复现 | ✅ 逐字复现 |
| 新版 + `LC_ALL=zh_CN.UTF-8` | rc=0 全绿 | ✅ | ✅ |
| 新版 + 默认 locale | rc=0 全绿 | ✅ | ✅ |
| 错密码 | rc=2 + psql 报错明文 | ✅ | ✅ |
| 空库（自检过、断言查不到 `sys_menu`） | **rc=2 而不是 rc=1** | ✅ | 未做（生产不建库） |
| 副本库基线 / `MENU_BIZ_EXTRA_IDS=` / 注入 ② / 复原 | 0 / 1(③=4) / 1(②=1) / 0 | ✅ 全过 | 未做（生产不建库） |

复现命令（任何装了 pg 的机器）：

```bash
LC_ALL=zh_CN.UTF-8 bash scripts/verify/menu_invariants.sh <库>; echo $?   # 旧版=2、新版=0
```

**自证脚本有牙**（测试库，改完立刻复原）：

```bash
psql ... -c "update sys_menu set parent_id=null where path='/system/user';"   # 制造 ②
bash scripts/verify/menu_invariants.sh <db>; echo $?                          # 期望 1、② 报 1
psql ... -c "update sys_menu set parent_id='10000000-0000-0000-0000-000000000001'
             where path='/system/user';"
bash scripts/verify/menu_invariants.sh <db>; echo $?                          # 期望 0
```

③ 的牙（基线 4 条例外不会被误报，新错位照样报红）：

```bash
# ① 基线：关掉例外表 → 只剩那 4 条，rc=1
MENU_BIZ_EXTRA_IDS= bash scripts/verify/menu_invariants.sh <db>; echo $?      # ③=4、rc=1
# ② 新错位：把 /business/storage/file 误挂到 /notice-center 根（例外表在，仍须报红）
psql ... -c "update sys_menu set parent_id='20000000-0000-0000-0000-000000000880'
             where id='21000000-0000-0000-0000-000000000020';"
bash scripts/verify/menu_invariants.sh <db>; echo $?                          # ③=1（文件管理）、rc=1
psql ... -c "update sys_menu set parent_id='21000000-0000-0000-0000-000000000001'
             where id='21000000-0000-0000-0000-000000000020';"
# ③ 例外过期：把例外行改成合规 → 不报红，但打印「失配 1」
psql ... -c "update sys_menu set parent_id='21000000-0000-0000-0000-000000000001'
             where id='20000000-0000-0000-0000-000000000710';"
bash scripts/verify/menu_invariants.sh <db>; echo $?                          # ③=0、rc=0、⚠ 失配 1
```

以上 ①②③ 在 145 `cloudpivot_dev` 实测：注入前/复原后整表 md5 一致（`c8530451…`），复原后 rc=0。

## 菜单元数据对不对 ≠ 界面上看得见（`menu_render_check.mjs`，卡 MENU-FIX-3）

`menu_invariants.sh` 查的是 `sys_menu` 的**结构**（parent / path / sort / title）。它查不出
「`icon` 写了一个 TDesign 图标库里不存在的名字」这类问题 —— 那种情况 DB 完全合规，
但 `SideMenuItem.vue` 的 `<t-icon :name="item.icon" />` 渲染空节点，侧栏那一项就是**空白**，
而且 `icon` 非空串时连组件里的 `folder` / `app` 兜底也不会触发。

实测（2026-11-09）：`tdesign-icons-vue-next/esm/components/` 有 `cart.js`，**没有** `shopping-cart.js`；
145 dev 的 60 个 distinct icon 全部存在，144 生产只有 `shopping-cart` 一个不存在
（`/business/erp/purchase`，即「采购订单」，V0170 只修了 dev 那条 `purchase-order` 路径——
两条是同一逻辑菜单在 prod/dev 的不同 path，V0319 头注释有记载，V0353 才补齐）。

所以这类问题**必须渲染出来看 DOM**：

```bash
FE_PASS="$(ssh root@172.18.10.145 'grep -m1 ^NIK_ADMIN_PASSWORD= /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-')" \
  node scripts/verify/menu_render_check.mjs
# 退出码：0 = 没有空白图标 ｜ 1 = 有空白（明细里打印 [目录] 菜单项）
# 环境变量：FE_BASE（默认 http://172.18.10.145）/ FE_USER（默认 admin）/ OUT_DIR（截图目录）
```

做法：API 登录拿 `/api/system/auth/menus` → 开 chromium → 逐个**展开顶级目录**
（TDesign 侧栏是手风琴，展开一个会收起别的，所以一次只验一个）→ 逐项断言 `li` 里存在
`svg.t-icon`，并打印「目录 → 子项 [渲染出的 svg class]」清单。

基线（145 `cloudpivot_dev`，2026-11-09 V0353 之后）：**82 个菜单项全部渲染出图标，rc=0**。

DOM 事实（写选择器前先看，否则会假红）：图标是 `li` 内**直接**的 `svg.t-icon t-icon-<name>`，
**不**在 `.t-menu__icon` 里；子项在 `li.t-submenu > ul` 内，层级 = 祖先 `li.t-submenu` 个数；
登录后「您有 N 件待处理」弹层会挡住侧栏点击，必须先点「知道了」；
`networkidle` 永远等不到（通知流是 SSE 长连接），用 `domcontentloaded` + 等 `.t-menu`。


## 菜单结构导出 / 跨环境比对 / 新环境初始化（卡 MENU-03）

三个脚本一条链：**导出**（归一化）→ **比对**（含等价规则）→ **新库从零重建并自检**。
配套文档：`docs/新机器部署-纯迁移重建.md`。

```bash
# ① 归一化导出（每行 path|parent_path|menu_type，只含未软删记录）
bash scripts/verify/menu_structure_dump.sh cloudpivot_fresh            # stdout
bash scripts/verify/menu_structure_dump.sh cloudpivot_fresh out.txt    # 写文件

# ② 两个来源比对（第二个参数可以是库名，也可以是文件）
bash scripts/verify/menu_struct_compare.sh cloudpivot_fresh \
     scripts/verify/baseline/menu_structure_baseline.txt
bash scripts/verify/menu_struct_compare.sh cloudpivot_fresh cloudpivot_dev --strict

# ③ 全新库：建库 → 纯跑全部迁移 → 不变量 → 树摘要 → 双向结构比对
bash scripts/setup_new_env.sh cloudpivot_fresh          # 库已存在则拒绝
RECREATE=1 bash scripts/setup_new_env.sh cloudpivot_fresh
```

退出码统一：`0` 通过 ｜ `1` 违规/差异 ｜ `2` 连库或参数问题。

### 口径：只比 path / parent_path / menu_type

`name` / `title` / `sort_order` / `icon` / `is_visible` 是业务数据或审美差异，改它们不会让侧栏"乱"；
层级与类型才会。三种特殊标注保证缺陷不被 `coalesce` 抹平：

| 标注 | 含义 |
|---|---|
| `-ROOT-` | 节点自身是顶级（`parent_id IS NULL`） |
| `-MISSING-` | 父节点不存在或已软删（孤儿 ⇒ 缺陷可见） |
| `~NULLPATH~` / `~NULL~` | 父节点存在但 path 为空 / 节点自身 path 为空（例如生产里 `/receipt-refund` 这类只种了页面行的菜单） |

### 等价表示登记表（不是白名单）

`scripts/verify/baseline/menu_struct_equiv.tsv`：`path` / `parent` 两类规则，每条带**理由与出处**。
比对前对两侧同时归一，输出永远同时打印「原始差异」和「归一后差异」，并统计每条规则命中几行：

```
已备案的等价规则（…/menu_struct_equiv.tsv）：
  · path   /business/erp/purchase → /business/erp/purchase-order（命中 8 行）：…
  · parent /business/erp/seq：/business/erp/finance-mgmt → /business/erp/master-data（命中 1 行）：…
原始差异: 仅 A 9 ｜ 仅 B 9
归一后差异: 仅 A 0 ｜ 仅 B 0
```

判据：**规则只能登记"同一菜单的另一种合法写法"**（如前端 `router` alias），
"有人手工改过库"必须补幂等迁移，不许往表里塞规则。`--strict` 要求原始差异也为 0，
用来审计旧规则是否已收敛；规则不再命中时脚本打 `⚠ 未被命中的规则…建议清理`。
关掉全部规则：`MENU_STRUCT_EQUIV=/dev/null`。

### 基线文件（随仓库提交）

| 文件 | 内容 | 何时更新 |
|---|---|---|
| `baseline/menu_structure_baseline.txt` | 纯迁移重建得到的 canonical 结构（408 行） | 迁移**故意**改导航结构时重新生成，commit 写清原因 |
| `baseline/prod_menu_structure.txt` | 144 生产快照（408 行，md5 `acb46924…`） | 生产菜单变更后重新导出 |
| `baseline/menu_struct_equiv.tsv` | 等价表示登记表 | 见上，只在论证后新增 |

### 自证（145 实测，2026-11-07）

| 用例 | 期望 | 实测 |
|---|---|---|
| 新库 vs 迁移链基线（`--strict`） | rc=0 | rc=0（408/408，原始与归一后均 0 差异） |
| 生产快照里插一条幽灵菜单 | 规则不得掩盖 ⇒ rc=1 | 归一后报 1 处（`/business/erp/ghost-page`），rc=1 |
| `MENU_STRUCT_EQUIV=/dev/null`（关规则） | 露出原始 18 处 ⇒ rc=1 | 归一后 18 处，rc=1 |
| 新库 vs 生产快照 `--strict` | 规则仍在生效 ⇒ rc=1 | `--strict：归一后虽一致，但原始差异 18 处`，rc=1 |
| 规则文件路径不存在 | 不得静默放过 ⇒ rc=1 | 18 处差异，rc=1（规则文件不存在时该节不打印） |

## 全员会签（ALL）节点的「永久卡死」不变量（`flow_countersign_invariants.sh`，卡 E2E07-FIX-1）

```bash
PGPASSWORD=... PGUSER=cloudpivot bash scripts/verify/flow_countersign_invariants.sh cloudpivot_dev
echo $?    # 0=4 条全绿 ｜ 1=有违规 ｜ 2=连库/查询失败（基础设施问题，别当违规修）
```

**刚建好的空库**（角色还没配人）④ 必然命中（6 个会签节点候选人为 0）——那不是配置错误，
是「还没配人」。这种场合用 `FLOW_CS_ALLOW_EMPTY=1` 把 ④ 降级成警告（rc 仍为 0，但结论会写明
「有降级警告 —— 降级 ≠ 通过」）。在用环境不要开这个开关：`FALLBACK_ADMIN` 会把「全员会签」
悄悄变成「全部超管会签」，多一个超管夹具账号就重演同一条死锁。

**为什么要有这条闸门**：`approval_mode='ALL'` 的推进条件是「所有候选人都签过」。
候选人里只要有一个**永远签不了**（离职未摘角色 / 停用 / 口令是占位符 `'x'`），
节点就永远凑不齐票 —— 没有待办可点、没有报错、界面上也没有按钮，单据停在
`PENDING_APPROVAL`，最后只能改库。E2E-06（预算 `n_dept`，`p3a_0002` 口令是占位符）与
E2E-07（合同 `n_pm` / `n_pm_hi`，`p3a_0003` 同病）各实测一次。

| # | 断言 | 判据 |
|---|---|---|
| ① | ALL 节点必须配兜底 | `timeout_hours > 0` 或 配了 `fallback_user_id` / `fallback_role_id`。**只填 `timeout_action`、不填 `timeout_hours` 也算违规**（`flow::timeout` 的扫描谓词是 `timeout_hours IS NOT NULL`） |
| ② | 角色型审批不得写成全员会签 | `APPROVAL` + `ROLE` + `ALL` = 违规（`APPROVAL` 语义是「岗位任一人签字即可」；跨角色共识要建 `COUNTERSIGN`）。V0339 按「候选人数 ≥2 ⇒ ALL」冻结出的错配由迁移 V0359 全系统修正 |
| ③ | ALL 节点的角色里不得有坏号 | 角色成员出现 软删 / 停用 / 授权过期 / 口令占位符（`'x'`/`''`/`CHANGE_ME` 或 `length < 20`）⇒ 违规 |
| ④ | ALL 节点可用候选人不得为 0 | 为 0 时只能靠 `empty_assignee` 兜：`FALLBACK_ADMIN` = 退化成「全部超管会签」（同一条死锁换个角色重演），`SKIP` = 审批被静默跳过 |

与发布校验 **R28**（`backend/src/business/flow/graph.rs::all_countersign_issues`，错误码 `65048`）
同一口径：发布期挡**新图**，本脚本管**存量库**。①③④ 是发布期查不出来的（候选人是否可用是运行期数据）。

**有牙齿的证明**（145 实测，见 `docs/evidence/e2e07-fix1/invariants_145_teeth.txt`）：
把 `contract_approve.n_gm` 改成 ALL 并清空超时、把 `GENERAL_MANAGER` 成员 `p3a_0004` 口令改成 `'x'`
→ ①②③ 同时报红（rc=1）并逐条点名；还原后 rc=0。

**清理不可用账号**（本卡 ⑤，只摘角色授权、不动用户、不碰 SUPER_ADMIN、先留档再删）：

```bash
bash scripts/datafix/flow_broken_role_accounts.sh                # dry-run：列出停用/软删/口令占位符的角色授权
bash scripts/datafix/flow_broken_role_accounts.sh --apply        # 先写 biz_data_fix_backup(fix_code=E2E07-FIX-1) 再摘除
```

还原（留档是整行 JSONB）：

```sql
insert into sys_user_role select id, user_id, role_id, expire_time, created_at
  from jsonb_populate_record(null::sys_user_role, (select row_data from biz_data_fix_backup
        where fix_code='E2E07-FIX-1' and table_name='sys_user_role' and row_id = '<授权行 id>'));
```

## 联动规则的「接线≠有规则」不变量（`linkage_rule_invariants.sh`，卡 E2E07-FIX-2）

```bash
bash scripts/verify/linkage_rule_invariants.sh                     # 读 /etc/cloudpivot/cloudpivot-dev.env
bash scripts/verify/linkage_rule_invariants.sh cloudpivot_dev      # 指定库
echo $?    # 0=4 条全绿 ｜ 1=有违规（逐条点名）｜ 2=连不上库 / 表不存在（基础设施问题，别当违规修）
```

**为什么要有这条闸门**：`biz_flow_linkage_rule` 是「单据状态变了以后还要做什么」的唯一载体。
`spec::TRIGGER_SPECS` 里 `wired = true` 只说明「代码在那个状态变更点真的调了引擎」，
**不**说明「配好了规则」——两者缺一，管理员看到的「已接线」就是一句空话：

- 接线了但**零 enabled 规则** ⇒ 引擎每次都被调用、每次都读到 0 条规则 ⇒ 一条通知都不发，
  而且没有任何报错（E2E-07 的 `CONTRACT`、FIX-3 的 `FIXED_ASSET`、TST-01 的 `PURCHASE_RETURN` 都是这个病）；
- 规则挂在**未接线**触发点 / 状态写错 / 动作是占位或被结构性禁止 ⇒ 同样「配了不跑」。

| # | 断言 | 判据 |
|---|---|---|
| ① | 每个 wired 触发点至少 1 条 enabled 规则 | 16 个 wired 触发点逐个查 `enabled AND deleted_at IS NULL`，为 0 即点名（「接线了但能力不存在」） |
| ② | enabled 规则必须挂在已接线触发点 | `trigger_entity` 不在 wired 清单里 ⇒ 违规（未接线实体见 `spec::TRIGGER_EXEMPT`：SO / SHIPMENT / RECEIVABLE / RECEIPT / INVOICE / SALES_RETURN / RECEIPT_REFUND / BUDGET） |
| ③ | `(触发点, 状态)` 必须是真实状态 | 与 `spec::TRIGGER_SPECS[].status_labels` 逐字对拍（状态改名 / 手抖 / 迁移漏改 ⇒ 规则永远等不到触发） |
| ④ | 动作必须已接管、且不是结构性被禁 | 占位动作（`implemented = false`）配得上但执行时 `Skipped`；合同出凭证 / 凭证生凭证 / 审批实例落账 = 配置期就该 400 的脏行，运行时一律被排除 |

脚本里写死三张表（`WIRED_TRIGGERS` / `STATUS_TABLE` / `IMPLEMENTED_ACTIONS` / `FORBIDDEN_PAIRS`），
与代码同口径由单测 `linkage::tests_presets_coverage::invariants_script_*` **双向**钉住
（表过期 ⇒ 单测红，不会静默放行）。

**① 的落地**：迁移 `V0254` / `V0323` / `V0351` / `V0360` 是预置规则的落地点；
`V0360` 把 12 个「接线了但零规则」的触发点一次补齐（`CONTRACT×2` / `STOCK_OUT` /
`STOCK_TRANSFER` / `PURCHASE_ORDER` / `PAYABLE` / `VOUCHER` / `ACCOUNTING_PERIOD` /
`ACCOUNTING_YEAR` / `STOCK_TAKE` / `DEPRECIATION` / `EXPENSE_REPORT` / `TASK` / `FLOW_INSTANCE×2`），
「以后新接一个触发点忘了种规则」由单测 `every_wired_trigger_has_a_preset` 在代码侧先红。

**有牙齿的证明**（一次性库 `mig_replay_*` 实测，见 `docs/evidence/e2e07-fix2/invariants_teeth.txt`）：
停用 CONTRACT 两条规则 → ① 点名 CONTRACT 报红；在 `SALES_RETURN`（未接线）插规则 → ②③ 报红；
把 `ACCOUNTING_PERIOD` 的状态改成 `PAID` → ③ 报红；插 `STOCK_OUT/POSTED/WRITE_INVENTORY`（占位）
与 `CONTRACT/SIGNED/GENERATE_VOUCHER`（结构性被禁）→ ④ 报红；还原后 4 绿。

**契约实测**（145 真 token / 真 HTTP）：`bash scripts/verify/e2e07_fix2_contract_notice_probe.sh`
—— 合同提交 → 审批通过（SIGNED）→ `sys_notice` 落 1 条 `LNK:CONTRACT:SIGNED:<合同 id>`，
收件人 = 制单人；重复审批被 409 挡住且通知不增；再终止（TERMINATED）→ 再落 1 条；
合同名下 0 张凭证（`CONTRACT_EXCLUDED_ACTIONS` 有牙）。

## 附件文件名快照（`e2e07_fix3_attachment_name_probe.sh`，卡 E2E07-FIX-3）

```bash
bash scripts/verify/e2e07_fix3_attachment_name_probe.sh    # 145，读 /etc/cloudpivot/cloudpivot-dev.env
```

**为什么要有这条探针**：`sys_file` 按内容 hash **秒传复用**（同字节共用一条记录），
`original_name` 是「**首个**上传者的名字」。`biz_doc_attachment` 过去只存 `file_id`，
列表一律读 `sys_file.original_name` —— 于是「B 上传的扫描件显示了 A 上传时的发票名」，
审计会把材料认错。修法是给**绑定行**加文件名快照（迁移 `V0362`
`biz_doc_attachment.file_name`），读路径
`COALESCE(NULLIF(a.file_name,''), f.original_name, '')`（快照优先、历史行回落）。

| # | 断言 | 判据 |
|---|------|------|
| ① | 迁移已应用 | `schema_migrations` 有 0362；列是 `varchar(255)`；**存量空值 0 行** |
| ② | 真秒传 | 同一份字节以两个不同文件名传到两张单据：第二次 `duplicated=true`、`file_id` 相同 |
| ③ | 各自显示自己的名字 | 两张单据的 `GET /business/attachment/list` 各自返回自己的 `file_name` |
| ④ | 库内不变量 | 两条绑定 `count(distinct file_id)=1`、`count(distinct file_name)=2`；`sys_file` 该 hash 只有 1 行 |
| ⑤ | 审计同源 | 第二次上传的 `operation_logs(DOC_ATTACHMENT_UPLOAD).detail.file_name` = 本次的名字 |
| ⑥ | 回落口径 | 把某行 `file_name` 置空（模拟未回填的历史行）→ 列表回落到 `sys_file.original_name`；恢复后又是自己的名字 |
| ⑦ | 超长 400 | 256 字符文件名 → `400 + 10001`，报文点明上限 255（不静默截断、不 500） |

代码侧由两条单测兜住：`file_name_snapshot_is_read_before_sys_file_name`（**漂移守卫**：
`COLUMNS` 里快照必须排在 `f.original_name` 之前 —— 顺序换回去时真库用例在没有秒传的夹具上
依然全绿，只有这条会红）与 `migration_adds_and_backfills_file_name`（迁移少一句就红）。
真库用例 `db_file_name_snapshot_survives_seconds_upload` 覆盖列表 / 单条 / 幂等 / 历史回落四段。

## 合同路径②「登记签订」+ 归档前提示（`e2e07_fix4_probe.sh`，卡 E2E07-FIX-4）

```bash
# 145：先起隔离实例（全新库 cp_fix4_e2e + 8034，迁移链路自检）
bash scripts/verify/e2e07_fix4_setup_145.sh
# 再跑探针（真 token / 真 HTTP / 真库，33 条断言）
DB=cp_fix4_e2e BASE=http://127.0.0.1:8034/api bash scripts/verify/e2e07_fix4_probe.sh
```

**为什么要有这条探针**：`PENDING_APPROVAL → SIGNED`（审批中心之外的「登记签订」）过去只翻
合同状态，审批实例仍 `PENDING`、待办仍挂在审批人处 —— 会签没走完就出现「已签 + 待办未清」，
只剩「把剩余会签签完」一条出口（签完会把一张已签合同再判一次 `APPROVED`），没有审批权的人
只能找工程师改库。现在同事务终止实例（`engine::terminate_instance`）。

| # | 断言 | 判据 |
|---|------|------|
| ① | 合同落 `SIGNED` | `PATCH /status {target_status:SIGNED}` → `200/c0` |
| ② | 实例被终止 | `biz_flow_instance.status=CANCELLED`、`ended_at` 非空、`result_comment` 含「审批中心外登记签订」 |
| ③ | 待办清零但留痕 | `PENDING` 0 条；原 PENDING 条数 → 全 `CANCELLED` |
| ④ | 节点收口 | 无 `ACTIVE/PENDING` 节点 |
| ⑤ | 状态日志留痕 | `biz_status_log(PENDING_APPROVAL→SIGNED).reason` 含「审批实例已终止」 |
| ⑥ | 重复签订被挡 | 再 `PATCH SIGNED` → `409`，合同状态不变 |
| ⑦ | 归档字面量 | `target_status=CLOSED` → `400 + 10001`（合同没有 CLOSED） |
| ⑧ | E4 不硬拦 | 挂 1 张在途采购单仍 `ACTIVE → COMPLETED` `200`，详情 `open_order_count=1`（前端提示数据源） |

环境坑（脚本里已处理）：全新库种子账号 `must_change_password=true`，服务端缓存键带前缀
（实测 `cloudpivot:uv:<id>`），改库后要按 pattern 删键否则 `403 首登未改密`；全新库只有
`admin`，审批节点按角色展开时采购经理为空 → 提交 `422 + 65018`，探针自建 `fix4_appr`
（`PURCHASE_MANAGER`）。

## 性能基线套件（`perf/`，卡 N-8 / `t_dd421965`）

**这不是「功能是否对」的验证，而是「数据量上去会不会崩」的验证** —— 与上面的 `verify.py`
互不相关，单独一套（需要 10 万行数据集，跑一次分钟级）。

```bash
# ① 造数（幂等递增：同一个库可 1千 → 1万 → 10万 逐级长大）
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_perf -v target=100000 \
     -f scripts/verify/perf/gen_perf_data.sql

# ② 单请求计时（预热 1 次 + 3 次取中位 → CSV）
PERF_BASE_URL=http://127.0.0.1:18099 bash scripts/verify/perf/measure.sh 100000 /tmp/perf_100k.csv

# ③ 并发压测（5 个 k6 脚本，按接口卡 p95；退出码 0 = 全部达标）
K6_BASE_URL=http://127.0.0.1:18099 bash scripts/verify/perf/run_perf_suite.sh /tmp/k6_out

# ④ 慢查询取证（14 条被测 SQL 的 EXPLAIN ANALYZE, BUFFERS）
psql -h 127.0.0.1 -U cloudpivot -d cloudpivot_perf -f scripts/verify/perf/explain_100k.sql
```

| 文件 | 作用 |
|---|---|
| `perf/gen_perf_data.sql` | 5 张关键表批量造数（`generate_series`，行带 `N8PERF` 前缀，末尾 `ANALYZE`） |
| `perf/measure.sh` | 真实令牌 + HTTP 端到端计时，14 个接口 × 3 次取中位 |
| `perf/k6_lib.js` | 5 个 k6 脚本的公共库（登录、tag、断言） |
| `perf/k6_task_list.js` / `k6_voucher_list.js` / `k6_trial_balance.js` / `k6_report_bs.js` / `k6_login.js` | 分接口压测，每个自带 `thresholds`（p95 上限），不达标 k6 返回 99 |
| `perf/run_perf_suite.sh` | 一次跑完 5 个脚本，打印「退出码 / 结论 / 实测 p95」 |
| `perf/explain_100k.sql` | 把各接口的 SQL 原样提出来 `EXPLAIN (ANALYZE, BUFFERS)`，用于找 seq scan 与索引缺口 |

基线数字、容量红线、已知慢查询与处置口径全部在
**[`docs/运维规程-性能基线.md`](../../docs/运维规程-性能基线.md)**；本轮原始证据在
`docs/evidence/20261110-n8-perf-baseline/`。
