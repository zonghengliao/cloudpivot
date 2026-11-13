# HR-50 文档回写 · 验收证据（2026-11-13）

卡：`t_4d9117bb`（HR-50【HR】文档回写 —— `docs/README.md` 索引、`AGENTS.md` 菜单/权限口径、二开指南补 HR 模板）

本目录回答两件事：

1. **文档里的每个数字是真量出来的**，不是从迁移文件里数的。
2. **新人照文档能跑通** —— 文档里写的「起实例 + 三个探针 + 两条验收命令」在空库上一条条真跑过。

---

## 一、跑法（可复现，一条命令）

```bash
bash docs/evidence/2026-11-13-hr50-doc-verify/run_accept.sh
```

脚本做的事（全部真跑，不 mock）：

| 步骤 | 动作 |
|---|---|
| ① | 在 **145**（`172.18.10.145`）建演练库 `hr50_docs_drill`（库名含 `drill`，不在 99 建库） |
| ② | 本机 → 145 开 ssh 隧道 `:15450`（145 的 PG 只监听 `127.0.0.1`） |
| ③ | 写临时 profile（端口 + 打开 auth）→ 用 `backend/target/debug/cloudpivot` 首启，**自动跑迁移** |
| ④ | 三个探针：`/health` / `/api/business/hr/health` / `/api/business/hr/nope` |
| ⑤ | 文档验收命令 `hr_invariants.sh`、`menu_invariants.sh` |
| ⑥ | 对着活库复核文档里的「实测基数」 |
| ⑦ | 停实例 + 删 profile + `DROP DATABASE` + 关隧道 + 查残留 |

原始输出：`02-run.out`。数据库统计：`01-db-stats.txt`。服务端日志：`00-server.log`。

---

## 二、实测结果（空库首启）

```
数据库迁移完成: 应用 335 个迁移
health ok=1 after 4s
```

| 探针 | 实测 | 期望 |
|---|---|---|
| `GET /health` | **200** | 200 |
| `GET /api/business/hr/health` | **401** | 401（需登录；证明 auth 生效） |
| `GET /api/business/hr/nope` | **404** | 404（证明是路由表命中，不是 SPA 兜底） |

```
hr_invariants.sh   → RC=0，12 行 viol=0（A1~A10 + W1/W2），blocker_fail=0 warn_fail=0
menu_invariants.sh → RC=0，8 条断言全绿（菜单总数 501 ∈ 400–520）
```

文档「实测基数」逐项对拍（`01-db-stats.txt`）：

| 文档写的 | 活库实测 | 结论 |
|---|---|---|
| 空库首启 335 条（323 版本化 + 12 种子），最大 V0403 | 335 / 323 / 12 / **V403** | ✅ |
| 10 张 HR 表 | 10 | ✅ |
| 菜单 41 行（1 目录 + 5 页面 + 35 按钮） | 41 = 5 页面 + 35 按钮（+1 目录） | ✅ |
| 权限码 40 个 = 5 `*:page` + 35 按钮（含 5 个 `*:list`） | 40 = 5 + 5 + 30 | ✅ |
| 薪资项 17 个（7 条公式） | 17 / 7 | ✅ |
| 审批流程 4 套 | 4 | ✅ |
| 迁移 17 个文件（16 条版本化 + 1 条种子） | 仓内 `ls` 实测 16 + 1 | ✅ |
| `ROUTE_CONTRACT` 50 行 / 已注册路由 50 条 | 50 / 50 | ✅ |
| 错误码段 `68001~68012` | `error_code.rs` 实测 68001~68012 | ✅ |

角色矩阵用真源解析器核对（`python3 scripts/verify/hr/hr_perm_expect.py`）：
`HR_MANAGER 40` / `FINANCE_MANAGER 5` / `DEPT_LEADER 5` / `EMPLOYEE 4` / `FINANCE_DIRECTOR 0` ——
与 `docs/编码规范.md` §1.7.2「HR 试点基线」逐项一致。

---

## 三、本卡实测踩到的两个坑（已回写进 `docs/后端二开指南.md` §17.6）

1. **`NIK_DATABASE_PORT` 不在 nik-config 的环境变量映射表里。**
   只设 env 时服务仍连 `127.0.0.1:5432`，日志 `数据库连接失败: error connecting to server` 秒退
   （第一次跑就是这么失败的，见 `02-run.out` 上方 `00-server.log` 残留的错误行）。
   **端口只能靠 profile 文件覆盖**：`backend/config/config-<profile>.toml` 的 `[database] port`。
2. **`NIK_PROFILE=<x>` 只叠加 `config/config-<x>.toml`，不会叠加 `config-test.toml`。**
   基础 `config.toml` 是 `[middleware.auth] enabled = false` ⇒ 自建 profile 里不写 auth 段，
   `/api/business/hr/health` 会回 **200** 而不是 401（实测过）。
   要验 401 得在 profile 里显式 `enabled = true`，且 **TTL 必须在同一 profile 重写**（否则被默认值盖掉）。

顺带修正：`schema_migrations.version` 是 `VARCHAR(64)`，`max(version)` 是字符串序（`'403' < '91'`），
比版本号必须 `max(version::int)` 且只算 `kind='versioned'`。

---

## 四、清场

```
removed /opt/cloudpivot/backend/config/config-hr50.toml
dropped hr50_docs_drill
145 残留 hr50% 库 = 0
本机 :18050 监听数 = 0
```

99 上未建任何库；隧道已关；临时 profile 未入库。

---

## 五、`AGENTS.md` 那一处（本卡唯一未自动落地项）

`AGENTS.md` 是**受保护 agent-instruction 文件**：写入需要人工批准，无人值守会话里批准超时＝拒绝，
且明确禁止改用 terminal / execute_code 绕道。故本卡把该文件的 6 处改动落成补丁：

```bash
cd /opt/cloudpivot
git apply --check docs/patches/AGENTS-hr50.diff   # 必须 OK（本卡实测 OK）
git apply docs/patches/AGENTS-hr50.diff
git add AGENTS.md && git commit -m "docs(agents): HR-50 回写菜单/权限口径"
rm docs/patches/AGENTS-hr50.diff   # 落地后删补丁与 docs/patches/README.md 对应节
```

补丁内容与「为什么要打补丁而不是直接写」见 `docs/patches/README.md`。
口径正文已同步落在可写权威文件里（`docs/编码规范.md` §1.7.2、`docs/后端二开指南.md` §十七、
`docs/README.md` 约定 11），补丁只负责 `AGENTS.md` 的顶层指针与过期数字。
