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

1. **两个环境各有一套 Hermes**：99（开发/我的工作机，v0.22.5）、144（生产，v0.15.2）
   - profile / workspace **不共享**，是两套独立数据
   - 云枢只操作 144 的这一套
2. **升级谨慎**：PyPI 的 0.15.2 与 99 的 0.22.5 有 CLI 差异（如 `-c` 在 0.15.2 是"继续会话"不是"执行命令"）。升级前先验证 `profile create/describe/kanban` 三个命令的参数没变。
3. **API key** 在 `/root/.hermes/config.yaml`（600），不入 git、不进数据库、不回显日志。
4. **首次装完 profile 列表里只有 `default`**，需要 `agent-template` 作为 clone 源（已建）。
5. 建 AI 用户失败时先查：
   - `CLOUDPIVOT_AIAGENT_CLONE_FROM` 指向的 profile 是否存在
   - `/root/.hermes/hermes-agent/venv/bin/hermes` 是否可执行
   - `journalctl -u cloudpivot` 里的 64005/64006 错误码

## 六、待办（T5b 阶段）

- AI 用户目前只能**创建/编辑/删除 profile**（L1）
- **派任务给 AI**（调 `hermes kanban create`）属于 **T5b**，届时需要：
  - 确认 144 的 v0.15.2 支持 `kanban` 子命令（待验证！99 的 0.22.5 支持）
  - 若 0.15.2 无 kanban → 要么升级 144 的 Hermes，要么改走 HTTP API
