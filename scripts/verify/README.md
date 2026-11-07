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
```

5 条断言（任一不过 → 打印前 10 条明细 + exit 1）：

| # | 断言 | 判据 |
|---|---|---|
| ① | 孤儿 parent | `parent_id` 指向不存在或已软删的记录 ⇒ 0 |
| ② | /system/ 归属 | `path ~ '/system/%'` 的页面/按钮（menu_type 2/3）必须挂在 `system` 目录（`10000000-…-0001`）下 |
| ③ | /business/ 归属 | `path ~ '/business/%'` 沿 parent 链上溯到的**根**必须落在 /business 命名空间 |
| ④ | path 唯一性 | `menu_type=2` 且未软删的记录中同一 `path` 不得出现多次 |
| ⑤ | 菜单总数 | 未软删总数 ∈ [400, 420]（`MENU_TOTAL_MIN`/`MENU_TOTAL_MAX` 可覆盖） |

**③ 的口径与卡面字面写法不同，这是故意的**：本仓库 ERP 各模块目录（`/business/erp/{finance,sales,warehouse,purchase}-mgmt`、`master-data`、`contract`）本身就是一级根，`/business` 通用底座并没有把它们挂在下面——按「必须找到 `path='/business'` 的祖先」的字面口径，既有基线就有 63 条"违规"，脚本会永远红。所以 ③ 判的是**「/business 子树不能挂到非 /business 命名空间的根上」**（例如误挂到 `/system`、`/project` 下），这才是 MENU-01 那类错位的回归面。
白名单 `MENU_BIZ_EXTRA_ROOTS`（默认 `/notice-center`）：公告中心根历史上承载 `/business/notice/*`、`/business/announcement` 4 条，属既有基线，不在此卡收敛范围。

**挂进流程**（`/root/test_deploy.sh` 与 `/root/.hermes/profiles/laoliu/scripts/deploy_check_v5.sh`，均在仓库外）：
- `test_deploy.sh` ⑦ 步：迁移 + 重启 + 登录冒烟之后跑本脚本，**只报警不阻断**（测试环境要能进去排查）；
- `deploy_check_v5.sh` ⑦ 步：对 145 测试库与 144 生产库各跑一次，违规/执行失败 → 建（或追加到）`【MENU-STRUCT】` 卡。

**自证脚本有牙**（测试库，改完立刻复原）：

```bash
psql ... -c "update sys_menu set parent_id=null where path='/system/user';"   # 制造 ②
bash scripts/verify/menu_invariants.sh <db>; echo $?                          # 期望 1、② 报 1
psql ... -c "update sys_menu set parent_id='10000000-0000-0000-0000-000000000001'
             where path='/system/user';"
bash scripts/verify/menu_invariants.sh <db>; echo $?                          # 期望 0
```
