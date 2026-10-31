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
| `erp_invariants.py` | 32 条不变量审计（会计恒等 / 账账 / 账实 / 单据链 / 状态 / 期间 / 闭环 / 审批闭环）。**只认 `NIK_DATABASE_*`**，跑前必须 `set -a; . /etc/cloudpivot/cloudpivot.env; set +a` | 有 BLOCKER → 1 |
| `e2e_fixture_residue.py` | 「批量生成零成本夹具」残留检查（只读）。这类行会让 B1（BLOCKER）/ C3（WARN）同时红，e2e 写完数据库建议挂一个后置检查 | 有残留 → 1；连库失败 → 2 |
| `p11f_fixture_residue_drill.sh` | 注入式演练：在 prod 副本库上重放「批量生成夹具」→ 检出 → 清理 → 32/32 → 幂等 → 回滚（31 条断言） | 有 FAIL → 1 |

```bash
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
python3 scripts/verify/erp_invariants.py          # 账是否干净
python3 scripts/verify/e2e_fixture_residue.py     # 有没有人往库里造夹具（exit 1 = 有）
```

**e2e 跑完请务必**：① 不要直连 prod（8023）造数；② 跑完调一次
`e2e_fixture_residue.py`，有残留就按它打印的三条命令清（备份 → `--check` →
`P10_GENERIC_APPLY=1 apply.sh`）。留痕见 `docs/datafix-20261031-p11-m-w1a-004-recur.md`。
