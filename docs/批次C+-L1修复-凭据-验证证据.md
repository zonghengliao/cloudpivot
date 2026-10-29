# 批次 D · L1 修复「建 AI 用户必须把 provider API key 写进 profile 的 `.env`」—— 验证证据

> 卡：`t_3b74de23`（backend-developer）
> 触发：老六 2026-10-29 端到端实测挖出的真实缺陷 —— 云枢 API 建的 AI 用户「号建得出来、活干不了」。
> 宿主：`172.18.10.144`（云枢 prod :8023 / 本卡隔离实例 :18206），Hermes **v0.22.5**（`/root/.hermes`），
> 实例库：`cloudpivot_l1fix`（临时、已删），redis DB 5。

## 零、结论速览

| 验收项 | 结果 | 证据 |
|---|---|---|
| 通过 API 建全新 AI 用户 `fix-check-ai` | ✅ | §四 A/B1 |
| 其 `.env` 含 `DEEPSEEK_API_KEY`、权限 600 | ✅ | §四 B1（模板 profile **故意不放 key**） |
| 派活 → 真跑 → **done**（不是 blocked） | ✅ | §五（kanban `done` → 云枢 `REVIEW`，5 分 55 秒） |
| 三层回传（AI_REPORT / ai_result / 附件 source=AI） | ✅ | §五 B3 |
| 失败回滚：`.env` 写不成 → 整体失败 + **磁盘零残留、库内 0 行** | ✅ | §六 C |
| 存量账号补救接口（`POST /repair`） | ✅ | §六 D（含幂等） |
| `cargo test` 全绿 | ✅ 653 passed | §七 |
| `scripts/check.sh` 全绿 | ✅（闸①0 / 闸②0 / C7 G1–G6 / 色板） | §七 |
| §6.2 部署验收（release / 重启 / 迁移 / 路由 / 二进制） | ✅ | §八 |
| 测试数据清理 | ✅ | §九 |

**实测又挖出 2 个自己的 bug 并修掉**（§十）：回滚只看标志位会留孤儿 profile 目录、回滚软删会在 `sys_user` 留垃圾行。

---

## 一、根因（真机复现，不是推测）

`hermes profile create` 建出的 profile **不继承**全局 `/root/.hermes/.env`：

```
$ HERMES_HOME=/root/.hermes hermes profile create l1fix-tmpl --no-alias
  ⚠ This profile has no API keys yet. Run 'l1fix-tmpl setup' first,
    or it will inherit keys from your shell environment.
$ ls -l /root/.hermes/profiles/l1fix-tmpl/.env
-rw------- 1 root root 165 ...            # 165 字节 = 三行注释
$ 键名列表: （空 —— 没有任何凭据键）        # 一个 `KEY=` 行都没有
```

而 Hermes worker 启动时 `HERMES_HOME` 指向 **profile 目录**，凭据池（`credential_pool`）只认
「profile 级 `.env` + 进程环境」，`config.yaml` 里即使写着 `api_key` 也不参与 provider 解析。
缺 key 的后果就是老六看到的那条链：

```
AuthError: No usable credentials found for provider 'deepseek'. Set DEEPSEEK_API_KEY.
credential pool: no available entries (all exhausted or empty)
  → Kanban owner refused → crash x2 → blocked
```

**为什么 `--clone-from` 也不保险**：`hermes profile create --clone-from X` 会复制 X 的 `.env`，
但「模板 profile 自己有没有 key」是不可控的（本卡实测：从 `agent-template` 克隆时
`written_keys=["DEEPSEEK_API_KEY"]` 说明模板那次并没带过来；而不带模板时必然没 key）。
把正确性寄托在「模板恰好带着 key」上，就是这次事故的成因。

## 二、修法：A 方案（把 key 写进 profile 级 `.env`），以及为什么不用 B

卡面要求二选一、优先 B（`hermes` 官方 auth 命令）。**实测两条路都验证过，最终选 A**：

```
$ HERMES_HOME=/tmp/authprobe hermes auth add deepseek --type api-key --api-key sk-PROBE…0001 --label probe
  → Installing Python dependencies…
  ✓ Installing Python dependencies
Added deepseek credential #1: "probe"
$ cat /tmp/authprobe/auth.json
{ "credential_pool": { "deepseek": [ { "source": "manual", "access_token": "sk-PRO…0001",
                                       "base_url": "https://api.deepseek.com/v1" } ] } }
$ ls /tmp/authprobe/.env     → 不存在
```

| 判据 | A（写 `.env`） | B（`hermes auth add`） |
|---|---|---|
| 落点 | `profiles/<name>/.env`（worker 真正读的那个文件） | `profiles/<name>/auth.json` 的 `credential_pool` |
| 卡面验收 2「检查 `.env` 里必须有 `DEEPSEEK_API_KEY`」 | ✅ 满足 | ❌ **不写 `.env`**（`auth add` 完全不碰它） |
| 与现场一致（t5b-agent 能跑就是因为 `.env` 有 key） | ✅ 同形态 | 另一套存储 |
| 副作用 | 一次原子写 + chmod | 在新 `HERMES_HOME` 上触发 `Installing Python dependencies`（实测两次），并写 `logs/` `cache/` `SOUL.md`；建号变慢且引入网络依赖 |
| 密钥暴露面 | 落 600 文件 | 明文进**命令行参数**（同机其它用户 `ps` 可见窗口） |

**结论**：B 虽然存在（`hermes -p <name> auth add` 也能指定 profile），但它不写 `.env`、
要求把明文 key 放进 argv、还会在新建 profile 上装依赖 —— 三个代价换一个「仓库形态与
事故现场不一致」的结果，不划算。所以选 A，并且实现里做足 A 的短板：
**去重不覆盖、原子写、600、写完自检**，失败即整单失败（由调用方回滚）。

## 三、代码落点

| 文件 | 内容 |
|---|---|
| `backend/src/business/aiagent/hermes.rs` | `ensure_profile_env`（补齐+自检）、`verify_profile_credentials`（只读自检）、`env_file_path` / `resolve_provider` / `read_text_opt` / `file_mode` / `write_env_text`；自由函数 `provider_api_key_var` / `is_credential_env_key` / `parse_env_pairs` / `env_lookup` / `merge_env_text` / `parse_model_provider`；`create_profile` 在 `ensure_profile_config` 之后**硬性**补齐凭据；`status()` 出参加凭据自检 |
| `backend/src/business/aiagent/handler.rs` | 创建流程新增 **⑤ 凭据自检**（进创建成功条件）；新增 `POST /system/user/ai/{id}/repair`；`rollback` 改为「以磁盘事实为准 + 库内物理删除」；`hard_delete_records` |
| `backend/src/business/aiagent/dto.rs` | `AiUserStatusRes` 补 5 个凭据字段；新增 `AiUserCredentialRes` |
| `backend/src/business/aiagent/mod.rs` | 六个接口表 + 不变式表（回滚语义改物理删除） |
| `backend/src/shared/error_code.rs` | **+64014 `AI_AGENT_CREDENTIAL_MISSING`**（64001–64014，码表自检 13→14） |

> 唯一动到 `business/aiagent/` 之外的是 `error_code.rs`（错误码按架构就该在共享段）：
> 新增 64014 是必要的行为差异 —— 用 64008（AI 配置文件 IO 失败）会把「业务上不可用」
> 伪装成「磁盘故障」，运维搜码搜不到这条真实故障形态。改动是纯追加（const + 码表 + 计数），
> 与 T6b 改的 63xxx 段不重叠。

### 关键不变式

1. **已有非空值绝不覆盖**：运维给某个 AI 员工配了专用 key 时，全局值不得顶掉它；
2. **只搬 `*_API_KEY`**：`THS_PASSWORD` / `OBSIDIAN_VAULT_PATH` 这类无关变量不进 profile（最小权限）；
3. **写完必须自检**：`ok = provider key 非空 && 权限 600`，两者缺一都算失败；
4. **失败即整单失败**：`create_profile` 里这一步是 `?`（与 `config.yaml` 的「失败只告警」不同）。

## 四、真机 E2E（验收 1–2）

### A. 首次建号（模板带 key 的老路径）

```
[1] POST /api/system/user/ai  建 fix-check-ai
  -> HTTP 200 code=0 msg=ok
  user_id=01a18732-5a7e-7403-a7d5-409c3acf1ef0 profile=fix-check-ai gateway=online
[2] 检查 /root/.hermes/profiles/fix-check-ai/.env
  权限: 0o600（要求 0o600）
  键名列表（值脱敏）:
    - DEEPSEEK_API_KEY  len=35  value=sk-af6…a033
  [PASS] 含 DEEPSEEK_API_KEY
  [PASS] 权限 600
[3] GET /api/system/user/ai/{id}/status
    healthy = True   credentials_ok = True
    credential_keys = ['DEEPSEEK_API_KEY']   credential_required = 'DEEPSEEK_API_KEY'
    env_file_mode = 384（=0o600）            credential_detail = None
```

### B1. 关键一击：模板 profile **故意不含 key** 时重建

先造现场（`l1fix-tmpl` 的 `.env` 只有三行注释、键名列表为空），再让 API 建号时
`clone_from=l1fix-tmpl`（＝复刻 2026-10-29 事故现场）：

```
$ 模板 l1fix-tmpl/.env 键名列表: （空 —— 没有任何凭据键）
[B1-2] POST /api/system/user/ai  clone_from=l1fix-tmpl
  -> HTTP 200 code=0 msg=ok
  user_id=01a18732-bd80-75fe-8fc0-eb1dfd98cd78 profile=fix-check-ai gateway=online
[B1-3] 检查 /root/.hermes/profiles/fix-check-ai/.env
  权限: 0o600（要求 0o600）
  键名列表（值脱敏）:
    - DEEPSEEK_API_KEY  len=35  value=sk-af6…a033
  [PASS] 含 DEEPSEEK_API_KEY
  [PASS] 权限 600
[B1-4] GET /status: healthy=True credentials_ok=True keys=['DEEPSEEK_API_KEY'] mode=384
```

服务端日志（**证明是后端写进去的，不是模板带过来的**）：

```
INFO cloudpivot::business::aiagent::hermes: 已确认 profile 级凭据（worker 凭据池可认证）
     profile="fix-check-ai" provider=Some("deepseek") required_key=DEEPSEEK_API_KEY
     written_keys=["DEEPSEEK_API_KEY"] mode=Some(384) env=/root/.hermes/profiles/fix-check-ai/.env
INFO cloudpivot::business::aiagent::handler: AI 员工凭据自检通过（worker 可认证）
     user_id=01a18732-bd80-75fe-8fc0-eb1dfd98cd78 profile=fix-check-ai
INFO cloudpivot::business::aiagent::handler: AI 用户已创建（Hermes profile + SOUL.md + workspace）
```

## 五、派活 + 真跑 + 三层回传（验收 3–5）

```
[B2-1] POST /api/business/task 建任务，负责人 = AI 账号
  -> task_id=01a18734-1d2e-7629-b032-4173df537eed status=TODO owner=L1修复验收机器人
[B2-2] POST /api/business/task/{id}/dispatch-ai
  -> HTTP 200 hermes_task_id=t_c319bc13 board=dev
[轮询] t+ 10s task.status=DOING exec_mode=AI kanban=running
       …（AI 真实执行 5 分 55 秒）
       kanban 侧: Task t_c319bc13 … status: done … started 18:29 / completed 18:34
```

**worker 侧认证证据**（`/root/.hermes/profiles/fix-check-ai/logs/agent.log`，无一条 AuthError）：

```
API call #22: model=deepseek-flash provider=deepseek in=39963 out=60 ... 
API call #31: model=deepseek-flash provider=deepseek in=48594 out=85 ...
$ grep -c "AuthError\|No usable credentials" agent.log errors.log
   agent.log:0
   errors.log:0
```

三层回传（API 取回，`B3`）：

```
[B3-1] GET /api/business/task/{id}
    task_no = 'TSK-202610-000001'   status = 'REVIEW'   progress = 100
    exec_mode = 'AI'   hermes_task_id = 't_c319bc13'   hermes_status = 'done'
    ai_summary = '已完成 L1 修复验收任务并全链路回传云枢。凭据验证：用 profile 级
                  /root/.hermes/profiles/fix-check-ai/.env 中的 DEEPSEEK_API_KEY 调用
                  https://api.deepseek.com/models，真实返回 HTTP 200 …'
    ai_result 类型 = dict 长度 = 1499
      {"artifacts": ["/root/.hermes/workspace/fix-check-ai/l1-fix-report.md"],
       "endpoints": [{"method":"GET","purpose":"profile .env 凭据真实性验证","status":200,
                      "url":"https://api.deepseek.com/models"}, …], …}
[B3-2] GET .../comments -> 4 条
    - type='PLAIN'     len=61     （派活留痕）
    - type='PLAIN'     len=1373
    - type='AI_REPORT' len=731    （AI 自己写的报告）
    - type='AI_REPORT' len=141    （云枢回传留痕：本次回传 1 个产出文件 l1-fix-report.md）
[B3-3] GET .../attachments -> 1 个
    {"file_name":"l1-fix-report.md","file_size":1652,"mime_type":"text/markdown",
     "source":"AI","ai_profile":"fix-check-ai", …}
=== 验收结论 ===
  [PASS] 任务跑到 REVIEW（不是 blocked/pending）
  [PASS] ① AI_REPORT 评论   [PASS] ② ai_result 非空   [PASS] ③ 附件 source=AI
```

> 附注：AI 自己还做了一次「凭据真实性」的独立验证 —— 拿 profile 级 `.env` 的 key 请求
> `https://api.deepseek.com/models` 得到 HTTP 200。这比「文件里有这行」更强：证明这个
> key 真的能认证。

## 六、失败回滚（验收 6）+ 修复接口（卡面第 4 条）

### C. 拿不到 key 时必须整单失败、零残留

现场：把全局 `/root/.hermes/.env` 临时移走（3 秒，随后立刻还原，权限复核 600）。

```
[C-2] POST /api/system/user/ai 建 fix-fail-ai（期望 500 + 64014）
  -> HTTP 500 code=64014
  msg = profile fix-fail-ai 缺少 DEEPSEEK_API_KEY：全局凭据文件 /root/.hermes/.env 与
        云枢进程环境里都没有该键，而 worker 只读 profile 级凭据（缺键必然 AuthError）；
        拒绝留下一个「能建号、不能干活」的 AI 用户
[C-3] 残留检查（磁盘 + 库）
  [PASS] profile 目录不存在：/root/.hermes/profiles/fix-fail-ai
  [PASS] workspace 目录不存在：/root/.hermes/workspace/fix-fail-ai
  有效（未软删）sys_user 行数 = 0
  sys_user 总行数（含软删）= 0        ← 物理删除，一条都不留
  有效 sys_ai_agent 行数 = 0
  不变式违规行数（ROBOT⇔配置行）= 0
  [PASS] C 阶段：失败 + 磁盘零残留 + 库内无有效行
```

### D. 存量账号补救：`POST /api/system/user/ai/{id}/repair`

```
[D-1] 人为破坏 .env：去掉 key + 权限改 644
   现有键：[]  权限：0o644
[D-2] GET /status -> credentials_ok = False（644=420）
   credential_detail = '缺少 DEEPSEEK_API_KEY（现有键：无）'
[D-3] POST /api/system/user/ai/{id}/repair
  -> HTTP 200 code=0
    provider='deepseek' required_key='DEEPSEEK_API_KEY' present_keys=['DEEPSEEK_API_KEY']
    env_file_mode=384 credentials_ok=True repaired_keys=['DEEPSEEK_API_KEY'] detail=None
[D-4] 修复后磁盘复核：键名=['DEEPSEEK_API_KEY'] 权限=0o600
  再点一次（幂等）-> repaired_keys = []（应为空数组）
  [PASS] D 阶段：修复接口补齐 key + 600 + 幂等
```

> 卡面第 4 条允许「启动自检 WARN」**或**「手工修接口」，本卡落地的是后者：
> 每个 AI 用户的真实凭据状态可以从 `GET /status` 直接看到（`credentials_ok` 与
> `credential_detail` 会点名缺哪个键），需要修时点一次 `POST /repair`。
> 没做「启动时全量扫描 + 自动修」是刻意的：自动修会覆盖运维给单个 AI 配的专用 key，
> 而全量扫描要么动 `shared/startup_guard.rs`（跨卡热点），要么每次启动逐个 profile 读盘 ——
> 单账号按需修 + 状态可见已经覆盖运维需求。

## 七、单测与检查链（验收 8–9）

```
$ cargo test -j 1
test result: ok. 653 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 0/1/1/1/11/8/4/0/5/6/7/1/2 passed …（14 个测试目标全部 ok）
test result: ok. 0 passed; 0 failed; 7 ignored（doctest）

$ bash scripts/check.sh
==> 后端：cargo fmt --check          PASS
==> 后端：cargo clippy -- -D warnings PASS
==> 后端：cargo check                 PASS
==> 前端：pnpm typecheck              PASS
==> 批次B 闸①：0 处  闸②：0 处        PASS
==> C7 G1–G6：未通过 0 条             PASS（G5 扫了 166 个文件 / 21 个 record 调用点）
==> 偏好字典与色板一致性：22 套逐字一致 PASS
==> 全部检查通过
```

新增单测（`business::aiagent` 29 passed，其中 9 条是本卡新增）：

| 用例 | 守的是什么 |
|---|---|
| `provider_api_key_var_maps_known_and_generic` | provider→env 名（表 + 表外通用规则）；只把 `*_API_KEY` 当凭据键 |
| `parse_env_pairs_handles_quotes_comments_and_export` | `.env` 解析：引号 / `export` / 行内注释 / 非法行 / 空值 |
| `merge_env_text_keeps_existing_values_and_fills_empty` | 合并三原则 + **幂等**（第二次零写入） |
| `parse_model_provider_reads_only_model_block` | `fallback_providers` 里的 `provider:` 不得误判 + 行内写法 |
| `ensure_profile_env_adds_missing_provider_key` | 核心：只有注释的 `.env` → 补齐 + 600 + 自检转绿 |
| `ensure_profile_env_does_not_overwrite_existing_key` | 专用 key 不被全局值顶掉 |
| `ensure_profile_env_fails_without_global_key` | 没有 key → 64014，且不写坏文件、不搬无关变量 |
| `verify_flags_bad_mode_and_sync_fixes_it` | 644 判红 → 同步纠正 600；目录不存在 → 404/64004 |
| `create_profile_provisions_credentials` | **根因回归**：假 CLI 复刻「建目录 + 只有注释的 .env」，`create_profile` 必须补上 key |

## 八、§6.2 部署验收（验收 10）

```
=== release 产物 ===
-rwxr-xr-x 2 root root 33534048 18:45 target/release/cloudpivot
f247d3b073012bc15c5f7ebfe29b5ba5ace454f258840e3442ce97cc94dcbe6b
=== 重启生产 ===
active
"数据库迁移完成: 应用 0 个迁移"          ← 本卡无迁移，0 新增符合预期
"AI 用户不变式自检通过：ROBOT 账号与 sys_ai_agent 一一对应"
生产库 schema_migrations 行数=72
=== 路由存在性（401=已注册；404=没注册）===
  POST /api/system/user/ai/{id}/repair -> 401   ← 新接口已注册
  GET  /api/system/user/ai/{id}/status -> 401
  POST /api/system/user/ai            -> 401
=== 二进制含新逻辑（grep -ac 中文文案）===
  "AI 员工凭据自检不通过" = 1      "而 worker 只读 profile 级凭据" = 1
  "AI 员工凭据修复完成" = 1        "已确认 profile 级凭据" = 1
  "worker 可认证" = 1              "凭据文件权限是" = 1
=== 生产真实接口：admin 查既有 AI 用户的凭据状态 ===
  生产登录 -> HTTP 200 code=0
    verify-ai: healthy=True credentials_ok=True keys=['DEEPSEEK_API_KEY'] mode=384 detail=None
```

生产上既有的 `verify-ai`（老六手工补过 key）在新接口下判定为「可认证」，与本卡逻辑一致。

## 九、测试数据清理（验收 7）

```
=== 1. 通过接口删 AI 用户 ===
  DELETE /api/system/user/ai/01a18732-bd80-75fe-8fc0-eb1dfd98cd78 -> HTTP 200 code=0
  data = {"profile_removed": true, "workspace_removed": true,
          "soul_backup_path": ".../aiagent-soul-backups/fix-check-ai-20261029-104608-198926446.md"}
=== 2. kanban card ===
  Archived t_c319bc13 / Deleted t_c319bc13
  看板 dev 剩余：[t_8c89eb86 verify-ai 【老六验证v2】T5b 端到端 AI 派活]   ← 别人的卡，未动
=== 3. 临时模板 profile l1fix-tmpl → Profile 'l1fix-tmpl' deleted.
=== 4. 附件 blob + SOUL 备份 ===
  已删 /opt/cloudpivot/backend/uploads/2026/10/29/01a18739-71d7-7002-9f82-cdf14c3b8ae5.md
  已删 2 个 fix-check-ai-*.md SOUL 备份
=== 5. 清理后核对 ===
  profile 目录：不存在 ✓   workspace 目录：不存在 ✓
  uploads 残留：无 ✓        SOUL 备份残留：无 ✓
=== 6. 实例与库 ===
  已停 pid=1168346；DROP DATABASE cloudpivot_l1fix → 库数 0
  redis DB5 dbsize=0；生产 /health -> 200
```

## 十、实测暴露并修掉的 2 个 bug（作者自查）

| # | 现象 | 根因 | 修法 |
|---|---|---|---|
| 1 | 缺 key 建号失败后，`/root/.hermes/profiles/fix-fail-ai/` **还在**（第一次跑 C 阶段时 `[FAIL] profile 目录不存在`） | 回滚只看调用方标志位 `profile_created`；而 `create_profile` 是复合动作 —— CLI 已建出目录、后续步骤（补凭据）才失败时它返回 `Err`，标志位仍是 `false` → 补偿跳过删 profile | `rollback` 改为**以磁盘事实为准**：先 `profile_dir_exists` / `workspace_path_exists` 探一次，标志位与磁盘「或」为真就删 |
| 2 | 回滚后 `sys_user` 仍留一条 `deleted_at` 非空的行（卡面要求「库内零残留」） | 沿用删号的软删语义；但回滚掉的账号**从未对用户可见**，软删只留下一条指向不存在账号的垃圾行 | 新增 `hard_delete_records`（同事务清 `sys_user_role` + 物理删 `sys_ai_agent` / `sys_user`），**只给回滚用**；删号仍走软删（业务留痕） |

## 十一、运维注意

1. **新增的 `.env` 是 profile 的资产，不是缓存的**：删 profile 就没了；手工改过 `.env` 后
   `GET /status` 会立刻反映（`credentials_ok` / `credential_detail`），不需要重启。
2. **换 provider 时**：`provider_api_key_var` 里是已知表 + 通用规则（大写 + `_API_KEY`）。
   新增 provider 若命名不合通用规则，改那张表即可（单测会盯住映射）。
3. **key 只搬 `*_API_KEY`**：`THS_PASSWORD` 之类不会进 profile；如需额外变量，改
   `is_credential_env_key` 的口径并同步单测。
4. **回滚是物理删除**：误判导致创建失败时，账号不会被「找回」，需要重建（本卡 C 阶段实测：
   重名可立刻复用，因为没有任何残留行）。
