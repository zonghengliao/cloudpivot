# 云枢 × Hermes 集成部署记录（172.18.10.144）

> 2026-10-29 老六（laoliu）执行。老大拍板方案 **A：生产机 144 上装 Hermes**。

## 一、决策背景

原设计假设「云枢与 Hermes 同机」，实际云枢在 144、Hermes 只在 99（laoliu 工作机）。
端到端测试时 AI 用户创建报 `64005 hermes CLI 不存在`。

老大选择 **A：144 装 Hermes**（理由：AI 任务应在生产环境执行，同机 CLI 最可靠，不依赖网络/SSH/跨机认证）。

## 二、安装结果

| 项 | 值 |
|---|---|
| 安装方式 | `python3 -m venv` + `pip install hermes-agent` |
| 路径 | `/root/.hermes/hermes-agent/venv/bin/hermes`（**与云枢代码默认路径一致**） |
| 版本 | **v0.15.2**（PyPI 版；99 是源码 v0.22.5） |
| Python | 3.14.4（aarch64） |
| 关键功能验证 | `profile create --clone-from` ✅ / `profile describe` ✅ / **SOUL.md 自动生成** ✅ |

## 三、配置

### 3.1 全局模型配置 `/root/.hermes/config.yaml`（600）
```yaml
model:
  default: deepseek-v4.1-flash
  provider: custom
  base_url: https://ark.cn-beijing.volces.com/api/plan/v3
  api_key: <火山方舟 key，600 权限，不入库不进 git>
agent:
  max_turns: 120
  api_max_retries: 3
```

### 3.2 模板 profile `agent-template`
- 作为云枢建 AI 用户时的 `--clone-from` 源
- 自带一份通用 SOUL.md（587 字节），实际创建时会被用户填的内容覆盖
- 命令：`hermes profile create agent-template --description "..." --no-alias`

### 3.3 云枢环境变量 `/etc/cloudpivot/cloudpivot.env`
```
CLOUDPIVOT_AIAGENT_CLONE_FROM=agent-template
```
> **重要**：代码里默认值是 `manong`（99 上的 profile 名），144 上没有 → **必须显式配置**，否则建 AI 用户会失败。

## 四、端到端验证（真机实测）

```
【3】AI 用户创建（L1）
  ✅ 创建 AI 用户 code=0
  ✅ profile 目录/SOUL.md 存在
  ✅ SOUL.md 内容一致
  ✅ workspace 目录存在
  ✅ 读取 AI 配置 code=0 / soul_md 有内容
  ✅ 编辑 AI 用户 code=0 / SOUL.md 已更新
  ✅ 删除 AI 用户 code=0 / profile 目录已消失
结果：PASS=29  FAIL=0
```

## 五、运维注意

1. **两个环境各有一套 Hermes**：99（开发/我的工作机，v0.22.5）、144（生产，**2026-10-29 已由 v0.15.2 升到 v0.22.5**）
   - profile / workspace **不共享**，是两套独立数据
   - 云枢只操作 144 的这一套
2. **升级谨慎**：PyPI 的 0.15.2 与 99 的 0.22.5 有 CLI 差异（如 `-c` 在 0.15.2 是"继续会话"不是"执行命令"）。升级前先验证 `profile create/describe/kanban` 三个命令的参数没变。
3. **API key** 在 `/root/.hermes/config.yaml`（600），不入 git、不进数据库、不回显日志。
4. **首次装完 profile 列表里只有 `default`**，需要 `agent-template` 作为 clone 源（已建）。
5. 建 AI 用户失败时先查：
   - `CLOUDPIVOT_AIAGENT_CLONE_FROM` 指向的 profile 是否存在
   - `CLOUDPIVOT_AIAGENT_HERMES_BIN` 指向的 CLI 是否可执行
     （**v0.15.2**：`/root/.hermes/hermes-agent/venv/bin/hermes`；**v0.22.5**：`/root/.hermes/hermes-agent/hermes`。
     升级会换路径 —— 路径错的表现是「AI 跑完了，云枢一直 DOING」，因为轮询失败按红线不改任务状态）
   - `journalctl -u cloudpivot` 里的 64005/64006 错误码

## 六、T5b 已落地（原待办已清）

- **派任务给 AI**：`POST /api/business/task/{id}/dispatch-ai` → `hermes kanban --board dev create … --json`；
  body 里带**任务级令牌**（`cpai_` + 64 hex，库内只存 sha256），AI 用它在**本任务**上传附件 / 写评论。
- **状态回传**：云枢每 30 秒 `hermes kanban show <id> --json` → ready/queued→TODO、running→DOING、
  done→三层回传后 **REVIEW**（2A）、failed/blocked/404 各有处理；**同一状态只回传一次**。
- kanban 子命令在 v0.15.2 与 v0.22.5 上都已实测可用，`--body/--idempotency-key/--workspace/--max-runtime/--json` 参数一致。

### T5b 期间新增的三条运维约束

1. **`CLOUDPIVOT_AIAGENT_HERMES_BIN`** 必须跟着 Hermes 升级改（见 §五 第 5 条）。
2. **AI 执行者 profile 必须关审批、放开迭代预算**：
   ```yaml
   agent:
     max_turns: 200     # 真实任务要跑几十轮工具调用
   approvals:
     mode: "off"        # kanban worker 无人值守，任何「危险命令」会等审批 → 60s 后判「用户拒绝」
     cron_mode: approve
   ```
   不开这一项的字面表现：`BLOCKED: User denied this command. The user has NOT consented…`，然后任务 `blocked`。
3. **改 `scripts/patch-alun-web.sh` 后必须 `cargo clean -p alun-web --release`**：补丁改的是
   registry 源码，而 cargo 对 registry 依赖按 crate 校验和判定，**不会因为源码 mtime 变化重编** ——
   否则 release 会静默链着打补丁前的 `alun-web`（T5b 实测：任务令牌被当作坏 JWT，一律 401
   「Token 无效或已过期」；`grep -ac is_ignored_path target/release/cloudpivot` 应为 1）。
