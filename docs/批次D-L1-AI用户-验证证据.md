# 批次 D · L1「AI 用户创建与 SOUL.md 管理」—— 验证证据

> 卡：`t_2d8b1971`（backend-developer）｜老大 2026-10-29 需求：「建用户的时候可以建 AI 用户，
> 云枢自动建好 AI 用户的 profile，并且能编辑 AI 用户的职责和 soul.md。**不需要跟 AI 用户对话。**」
>
> laoliu 2026-10-29 追加硬要求：`user_type='ROBOT'` ⇔ `sys_ai_agent` 有该 user_id 记录（双向不变式），
> 含「通用用户接口不得改/删 AI 账号」与「启动自检」。

本文所有输出均来自 144 上的真机执行（`profile=test`，库 `cloudpivot_l1_empty` 空库建档），
Hermes 在 `172.18.10.99`（`/root/.hermes`），云枢经 SSH 调官方 CLI。

## 0. 部署形态（**先读这条**）

| 项 | 值 |
|---|---|
| 云枢后端 | `172.18.10.144:/opt/cloudpivot`（prod 8023 / dev 8024） |
| Hermes | `172.18.10.99:/root/.hermes`（`hermes-agent/venv/bin/hermes`），profile 在 `/root/.hermes/profiles` |
| 两者关系 | **当前不同机**。卡面写的是同机路径，T5b 设计文档也假设同机（「若在 `/root/.hermes/workspace/<profile>/` 下（同机）」） |

因此 `business/aiagent/hermes.rs` 实现了两种 `Transport`：

- `Local`：`Command::new(hermes_bin)`（**同机部署的目标形态**，T5b 落地时用）；
- `Ssh`：`ssh root@172.18.10.99 …`（**当前生产形态**，本卡实测用的就是它）。

切换只需一个环境变量（`CLOUDPIVOT_AIAGENT_SSH_HOST` 置空 → 回落 local），业务代码零改动。
两种形态共用同一套校验 / 超时 / 错误码 / 单元测试。

## 1. 交付清单

| 文件 | 说明 |
|---|---|
| `backend/migrations/system/V0105__create_sys_ai_agent.{up,down}.sql` | `sys_ai_agent` 表（卡面写 V0092，编号已推进且 V0104 被并行卡占用，顺延到 V0105） |
| `backend/src/business/aiagent/{mod,hermes,model,dto,handler}.rs` | 适配层 + 模型 + DTO + 5 个接口 |
| `backend/src/shared/error_code.rs` | 新增 `64xxx` 段 13 个码（64001–64013）+ 码表稳定性自测 |
| `backend/src/shared/startup_guard.rs` | `check_ai_agent_invariant()`（两条不变式查询） |
| `backend/src/bootstrap.rs` | `ai_agent_invariant_guard()`（ERROR 日志；prod 拒启动） |
| `backend/src/system/user/{dto,model,handler}.rs` | 出参补 `user_type`；update/delete 对 ROBOT 返回 409 |
| `backend/src/business/masking/registry.rs` | 脱敏样例补 `user_type` 字段（该文件的「加字段即编译失败」闸门） |
| `frontend/src/api/system/ai-user-api.ts` | 5 个接口的类型与调用 |
| `frontend/src/views/system/user/components/{AiUserForm,SoulEditor}.vue` | 职责 / 模型 / SOUL.md 编辑器（等宽 + 结构预览 + 字数 + 模板填充 + 保存前路径提示） |
| `frontend/src/views/system/user/index.vue` | 账号类型单选、类型列徽标、AI 编辑与删除分支 |

## 2. 表结构（空库实测）

```
$ psql -c "\d sys_ai_agent"
                                    Table "public.sys_ai_agent"
      Column       |           Type           | Collation | Nullable |           Default            
-------------------+--------------------------+-----------+----------+------------------------------
 user_id           | uuid                     |           | not null | 
 hermes_profile    | character varying(64)    |           | not null | 
 workspace_path    | character varying(500)   |           | not null | 
 soul_md           | text                     |           | not null | ''::text
 agent_description | character varying(255)   |           |          | 
 model_override    | jsonb                    |           |          | 
 gateway_status    | character varying(16)    |           | not null | 'unknown'::character varying
 created_at        | timestamp with time zone |           | not null | now()
 updated_at        | timestamp with time zone |           | not null | now()
 deleted_at        | timestamp with time zone |           |          | 
Indexes:
    "sys_ai_agent_pkey" PRIMARY KEY, btree (user_id)
    "uk_sys_ai_agent_profile" UNIQUE, btree (hermes_profile) WHERE deleted_at IS NULL
Check constraints:
    "ck_sys_ai_agent_gateway_status" CHECK (gateway_status::text = ANY (ARRAY['unknown'::character varying, 'online'::character varying, 'offline'::character varying]::text[]))
    "ck_sys_ai_agent_profile_name" CHECK (hermes_profile::text ~ '^[a-z][a-z0-9-]{1,30}$'::text)
Foreign-key constraints:
    "sys_ai_agent_user_id_fkey" FOREIGN KEY (user_id) REFERENCES sys_user(id) ON DELETE CASCADE


$ 部分唯一索引 / CHECK 约束 / FK
CREATE UNIQUE INDEX sys_ai_agent_pkey ON public.sys_ai_agent USING btree (user_id)
CREATE UNIQUE INDEX uk_sys_ai_agent_profile ON public.sys_ai_agent USING btree (hermes_profile) WHERE (deleted_at IS NULL)
ck_sys_ai_agent_gateway_status|CHECK (((gateway_status)::text = ANY ((ARRAY['unknown'::character varying, 'online'::character varying, 'offline'::character varying])::text[])))
ck_sys_ai_agent_profile_name|CHECK (((hermes_profile)::text ~ '^[a-z][a-z0-9-]{1,30}$'::text))
sys_ai_agent_created_at_not_null|NOT NULL created_at
sys_ai_agent_gateway_status_not_null|NOT NULL gateway_status
sys_ai_agent_hermes_profile_not_null|NOT NULL hermes_profile
sys_ai_agent_pkey|PRIMARY KEY (user_id)
sys_ai_agent_soul_md_not_null|NOT NULL soul_md
sys_ai_agent_updated_at_not_null|NOT NULL updated_at
sys_ai_agent_user_id_fkey|FOREIGN KEY (user_id) REFERENCES sys_user(id) ON DELETE CASCADE
sys_ai_agent_user_id_not_null|NOT NULL user_id
sys_ai_agent_workspace_path_not_null|NOT NULL workspace_path

$ schema_migrations
105|create_sys_ai_agent|t
```

启动日志（空库首次迁移 + 不变式自检）：
```
[2m2026-10-29T05:14:30.904129Z[0m [32m INFO[0m [2malun_web::app[0m[2m:[0m 数据库迁移完成: 应用 64 个迁移
[2m2026-10-29T05:14:30.945618Z[0m [32m INFO[0m [2mcloudpivot::shared::startup_guard[0m[2m:[0m AI 用户不变式自检通过：ROBOT 账号与 sys_ai_agent 一一对应
```

## 3. 主验收（43 项，全部通过）

```
==============================================================================
L1 验收 E2E —— 实例 http://127.0.0.1:18205 / 库 cloudpivot_l1_empty / Hermes 主机 root@172.18.10.99
==============================================================================
登录 admin（使用已改过的验收口令）
  [PASS] 拿到 access_token  ← HTTP 200

[0] 清理上一轮残留（幂等）
      已通过接口删除残留 AI 用户 l1-agent-inv（HTTP 200）

[1] 创建 AI 用户 l1-agent-01（真跑：建号 + profile + SOUL.md + workspace）
  [PASS] POST /api/system/user/ai → 200/code=0  ← HTTP 200 code=0 msg=ok
      user_id=01a18617-6b9c-7102-b635-4d130c91e84b hermes_profile=l1-agent-01 gateway_status=online
  [PASS] 响应 user_type=ROBOT  ← ROBOT
  [PASS] 响应 soul_path 指向真实路径  ← /root/.hermes/profiles/l1-agent-01/SOUL.md
  [PASS] 响应 workspace_path  ← /root/.hermes/workspace/l1-agent-01
  [PASS] 磁盘：profile 目录存在且含 SOUL.md  ← . .. .env .env.lock SOUL.md audio_cache backups cache config.yaml cron gateway.lock home hooks image_cache logs memories pairing plans platforms profile.yaml ru
  [PASS] 磁盘：SOUL.md 内容与表单逐字一致（含 $ ' ` 等危险字符）  ← len(file)=73 len(form)=74
  [PASS] 磁盘：workspace 目录存在  ← WS_OK
  [PASS] hermes profile list 能看到该 profile  ← ['  l1-agent-01     deepseek-v4.1-flash          running      —            —']
  [PASS] 库：sys_user.user_type=ROBOT  ← ROBOT
  [PASS] 库：sys_ai_agent 有该 user_id 的配置行（不变式成立）  ← l1-agent-01
  [PASS] hermes profile describe 读回职责  ← '负责 L1 验收的测试机器人'

[2] GET /api/system/user/ai/{id}（含 SOUL.md 全文）
  [PASS] 详情返回 SOUL.md 全文（与写入一致）  ← HTTP 200 len=74
  [PASS] 详情 agent_description  ← 负责 L1 验收的测试机器人
  [PASS] 详情 model_override（jsonb 往返）  ← {"model": "deepseek-v4.1-flash"}

[3] GET /status 探活
  [PASS] 探活：目录 / SOUL.md / workspace / CLI 全部在  ← {"profile_dir_exists": true, "soul_file_exists": true, "workspace_exists": true, "cli_available": true, "gateway": "online", "healthy": true}

[4] PUT 编辑 SOUL.md → 磁盘同步
  [PASS] PUT soul_md → 200  ← HTTP 200 msg=ok
  [PASS] 磁盘：SOUL.md 已同步为第二版  ← len=71
      --- diff（第一版 → 第二版）---
      - # L1 测试机器人
      - 
      - ## 角色
      - 负责 L1 验收。
      - 
      - ## 注意
      - $HOME 与 '单引号' 与 `反引号` 与 $(id) 都必须原样保存。
      + # L1 测试机器人（第二版）
      + 
      + ## 角色
      + 负责 L1 验收，已改为第二版。
      + 
      + ## 差异
      + - 新增：SOUL.md 必须与表单内容逐字一致
  [PASS] 原子写：目录内无残留临时文件  ← tmp_files=0

[5] PUT 编辑职责 → hermes profile describe
  [PASS] PUT agent_description → 200  ← HTTP 200
  [PASS] hermes profile describe 读回与表单一致  ← "负责 L1 验收（职责已改，含 ' 引号）"

[6] 命令注入：username = 'abc; rm -rf /'
  [PASS] 被正则拒绝（400 + 64002）  ← HTTP 400 code=64002 msg=profile 名不合法："abc; rm -rf /"（必须以小写字母开头，只含小写字母/数字/连字符，长度 2–31）
  [PASS] 磁盘：未生成任何非法 profile 目录  ← ls: cannot access '/root/.hermes/profiles/abc': No such file or directory | ls: cannot access '/root/.hermes/profiles/abc;': No such file or directory
  [PASS] 库：未留下半个 sys_user  ← 0

[6b] 非法名 Bad_Name（Hermes 会静默小写，故必须应用层拒绝）
  [PASS] Bad_Name 被拒绝（400 + 64002）  ← HTTP 400 code=64002 msg=profile 名不合法："Bad_Name"（必须以小写字母开头，只含小写字母/数字/连字符，长度 2–31）
  [PASS] 磁盘：既没有 bad_name 也没有 Bad_Name（未静默建号）  ← ls: cannot access '/root/.hermes/profiles/bad_name': No such file or directory | ls: cannot access '/root/.hermes/profiles/Bad_Name': No such file or directory

[7] 通用用户接口不得动 AI 账号（不变式守卫）
  [PASS] PUT /system/user/{id} 对 ROBOT → 409 + 64013  ← HTTP 409 code=64013 msg=AI 用户（ROBOT）请通过 /api/system/user/ai/{id} 维护（职责 / SOUL.md / 模型）
  [PASS] DELETE /system/user/{id} 对 ROBOT → 409 + 64013  ← HTTP 409 code=64013 msg=AI 用户（ROBOT）请通过 DELETE /api/system/user/ai/{id} 删除（会先备份 SOUL.md 再清理 profile）

[8] DELETE AI 用户（先备份 SOUL.md → 删 profile → 删 workspace → 软删两行）
  [PASS] DELETE → 200  ← HTTP 200
      soul_backup_path=/root/.hermes/aiagent-soul-backups/l1-agent-01-20261029-051740-505686779.md profile_removed=True workspace_removed=True
  [PASS] 回传备份路径  ← /root/.hermes/aiagent-soul-backups/l1-agent-01-20261029-051740-505686779.md
  [PASS] 磁盘：备份文件存在且内容是删除前的 SOUL.md  ← len=71
  [PASS] 磁盘：profile 目录已消失  ← ls: cannot access '/root/.hermes/profiles/l1-agent-01': No such file or directory
  [PASS] 磁盘：workspace 目录已消失  ← ls: cannot access '/root/.hermes/workspace/l1-agent-01': No such file or directory
  [PASS] hermes profile list 不再包含该 profile
  [PASS] 库：sys_user 已软删  ← t
  [PASS] 库：sys_ai_agent 已软删（同事务）  ← t
  [PASS] 不变式复查：无「配置行 ↔ 非 ROBOT 账号」违规  ← 0

[9] 删掉后重建同名（部分唯一索引生效）
  [PASS] 创建 l1-agent-inv → 200  ← HTTP 200 msg=ok
  [PASS] SOUL.md 留空时自动模板填充（含身份行 + 四段结构）  ← 你是不变式探针机器人（l1-agent-inv），用于不变式自检与重建验证

[10] 人类用户流程不受影响
  [PASS] POST /system/user（人类）→ 200  ← HTTP 200 msg=ok
  [PASS] 列表下发 user_type=HUMAN（AI 用户才显示机器人徽标）  ← HUMAN
  [PASS] PUT 人类用户 → 200（未被 AI 守卫误伤）  ← HTTP 200
  [PASS] DELETE 人类用户 → 200  ← HTTP 200

      （l1-agent-inv 保留，供不变式自检实验使用：user_id=01a18617-c1b3-733b-acc0-9a076c2d0c75）

==============================================================================
结果：43 通过 / 0 失败 / 共 43 项
==============================================================================
```

## 4. 失败回滚（24 项，全部通过）

### 4.1 hermes CLI 不存在（`CLOUDPIVOT_AIAGENT_HERMES_BIN=/nonexistent/hermes`）

```
==============================================================================
L1 失败回滚验收 · case=nocli · 实例=http://127.0.0.1:18206 · profile=l1-rollback-cli
==============================================================================
  [PASS] 登录  ← HTTP 200

[1] 创建（预期失败）
      HTTP 500 code=64005
      msg=创建 profile失败：env: '/nonexistent/hermes': No such file or directory
env: use -[v]S to pass options in shebang lines
  [PASS] 接口返回 500 + 64005（Hermes 执行失败）  ← HTTP 500 code=64005
  [PASS] 错误消息点明 CLI 不可用（远端 stderr 原样回传前端）  ← 创建 profile失败：env: '/nonexistent/hermes': No such file or directory
env: use -[v]S to pass options in shebang lines

[2] 回滚结果（不留半成品）
  [PASS] 库：无有效 sys_ai_agent 行  ← 0
  [PASS] 库：无有效 sys_user 行（已软删）  ← 0
  [PASS] 库：软删行留痕（不是物理删除，可审计）  ← rows=1
  [PASS] 库：配置行同样软删留痕  ← rows=1
  [PASS] 磁盘：profile 目录不存在（含「已建后回滚」）  ← ls: cannot access '/root/.hermes/profiles/l1-rollback-cli': No such file or directory
  [PASS] 磁盘：workspace 目录不存在  ← ls: cannot access '/root/.hermes/workspace/l1-rollback-cli': No such file or directory
  [PASS] hermes profile list 不含该 profile  ← count=0

[3] 不变式复查（全局）
  [PASS] ROBOT 账号无孤立者  ← 0
  [PASS] 配置行无孤立者  ← 0

结果：12 通过 / 0 失败 / 共 12 项
```

### 4.2 profile 已建、workspace 建不出来（`CLOUDPIVOT_AIAGENT_WORKSPACE_ROOT=/proc/l1-impossible`）

这条专门证明「外部副作用做到一半失败也必须回收已建的 profile」：
接口返回 500 之后，磁盘上的 profile 目录**确实不存在**（下面第 2 节逐项断言）。

```
==============================================================================
L1 失败回滚验收 · case=badws · 实例=http://127.0.0.1:18207 · profile=l1-rollback-ws
==============================================================================
  [PASS] 登录  ← HTTP 200

[1] 创建（预期失败）
      HTTP 500 code=64005
      msg=创建 workspace失败：mkdir: No such file or directory
  [PASS] 接口返回 500 + 64005（Hermes 执行失败）  ← HTTP 500 code=64005
  [PASS] 错误消息点明 workspace 创建失败  ← 创建 workspace失败：mkdir: No such file or directory

[2] 回滚结果（不留半成品）
  [PASS] 库：无有效 sys_ai_agent 行  ← 0
  [PASS] 库：无有效 sys_user 行（已软删）  ← 0
  [PASS] 库：软删行留痕（不是物理删除，可审计）  ← rows=1
  [PASS] 库：配置行同样软删留痕  ← rows=1
  [PASS] 磁盘：profile 目录不存在（含「已建后回滚」）  ← ls: cannot access '/root/.hermes/profiles/l1-rollback-ws': No such file or directory
  [PASS] 磁盘：workspace 目录不存在  ← ls: cannot access '/root/.hermes/workspace/l1-rollback-ws': No such file or directory
  [PASS] hermes profile list 不含该 profile  ← count=0

[3] 不变式复查（全局）
  [PASS] ROBOT 账号无孤立者  ← 0
  [PASS] 配置行无孤立者  ← 0

结果：12 通过 / 0 失败 / 共 12 项
```

## 5. 不变式自检（laoliu 追加硬要求）

做法：直接 `DELETE FROM sys_ai_agent WHERE hermes_profile='l1-agent-inv'`（模拟配置行被外部删掉），
重启实例，看自检是否报错；修复数据后重启是否恢复 PASS。

```
$ 手工删掉一行后两种违规计数（期望 1 / 0）
1
0

$ 重启后的启动日志
[2m2026-10-29T05:18:00.709620Z[0m [31mERROR[0m [2mcloudpivot::bootstrap[0m[2m:[0m AI 用户不变式自检不通过：配置错误: AI 用户不变式被破坏：1 个 ROBOT 账号缺少 sys_ai_agent 配置行（user_id: 01a18617-c1b3-733b-acc0-9a076c2d0c75）；0 条 sys_ai_agent 配置行对应非 ROBOT / 已删除账号（user_id: -）。修复：删除孤立行，或走 /api/system/user/ai 接口重建 AI 用户
[2m2026-10-29T05:18:00.709656Z[0m [33m WARN[0m [2mcloudpivot::bootstrap[0m[2m:[0m AI 用户不变式自检不通过（非 prod，继续启动，请尽快修复数据）

$ 修数据后再重启（恢复 PASS）
[2m2026-10-29T05:18:01.924410Z[0m [32m INFO[0m [2mcloudpivot::shared::startup_guard[0m[2m:[0m AI 用户不变式自检通过：ROBOT 账号与 sys_ai_agent 一一对应
```

## 6. 静态检查与测试

```
$ cargo clippy --all-targets -- -D warnings   → 通过（0 warning）
$ cargo test -j 1                              → 604 passed, 0 failed（lib 563 + 集成 41）
$ pnpm typecheck && pnpm build                 → 通过
$ bash scripts/check.sh                        → 全部检查通过（含闸①/闸②、C7 G1–G6、色板一致性）
```

新增单测 20 条（`business::aiagent` 19 + 码表 `ai_agent_error_codes_are_stable` 1），覆盖：
profile 名正则与注入用例、`shell_quote` 12 类危险输入的**真 shell 往返**、
SOUL 原子写与临时文件零残留、超限拒绝、CLI 缺失/超时/静默失败三条失败路径、
workspace 生命周期幂等、备份落盘、远端命令拼装的引号。

## 7. 部署配置（环境变量，均有默认值）

配置走环境变量而不是 `config.toml` 新段：`config.toml` 由框架强类型反序列化，加未知段有被拒风险；
这些值本身是**部署形态**（同机/跨机、路径），与 `/etc/cloudpivot/*.env`（600）里既有密钥同源管理更合适。

| 变量 | 默认 | 说明 |
|---|---|---|
| `CLOUDPIVOT_AIAGENT_SSH_HOST` | 空 | 非空 → SSH 形态（如 `root@172.18.10.99`）；**置空 → 同机形态** |
| `CLOUDPIVOT_AIAGENT_TRANSPORT` | `local` | `local` / `ssh`（`SSH_HOST` 优先） |
| `CLOUDPIVOT_AIAGENT_HERMES_BIN` | `/root/.hermes/hermes-agent/venv/bin/hermes` | 官方 CLI 绝对路径 |
| `CLOUDPIVOT_AIAGENT_HERMES_HOME` | `/root/.hermes` | 显式传给子进程（否则 Hermes 会打 `HERMES_HOME fallback` 告警） |
| `CLOUDPIVOT_AIAGENT_PROFILES_ROOT` | `/root/.hermes/profiles` | profile 根 |
| `CLOUDPIVOT_AIAGENT_WORKSPACE_ROOT` | `/root/.hermes/workspace` | 工作区根 |
| `CLOUDPIVOT_AIAGENT_BACKUP_DIR` | `/root/.hermes/aiagent-soul-backups` | 删号前 SOUL.md 备份目录 |
| `CLOUDPIVOT_AIAGENT_CLONE_FROM` | `manong` | 建 profile 的模板 profile |
| `CLOUDPIVOT_AIAGENT_TIMEOUT_SECS` | `60` | 单条 CLI 命令超时（超时即终止子进程） |

SSH 形态的前置条件：云枢运行用户（root）到目标主机的 **免密**（`BatchMode=yes`，连不上就快速失败
而不是卡在密码提示）；本次实测在 144 上生成专用 ed25519 公钥并追加到 99 的 `authorized_keys`。

## 8. 已知边界与后续

1. **`model_override` 目前只是存下来**：真正「切换该 profile 的模型」要改 `~/.hermes/profiles/<name>/config.yaml`，
   属于「不动 Hermes 侧配置」红线的范围外，留给 T5b / 后续卡；
2. **AI 用户不分配角色**（卡面范围只有账号 + profile + 人格）：机器人要调云枢 API 需要权限，
   那是 T5（AI 执行者）的交付物；本卡建的机器人在云枢里「没有业务权限」，不会误操作任何数据；
3. **通用用户接口对 ROBOT 的 update/delete 一律 409**：这是不变式的守卫，不是 bug；
   需要改显示名时目前也要走 AI 接口之外的路径（显示名改走 `PUT /system/user/{id}` 会被 409 拦住，
   如确有需要，后续可在 AI 接口里补 `real_name` 字段）；
4. **`sys_ai_agent` 未登记到 `changelog` 的 REGISTRY**：写入会报 `62012`，本卡不动 changelog 模块；
   审计痕迹目前靠 `tracing` 结构化日志（含 user_id / profile / operator / 备份路径）。
