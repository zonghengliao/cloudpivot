# P1-05 证据：`GET /metrics` 端点（Prometheus 格式）

> 卡面验收：`curl -s http://127.0.0.1:8023/metrics | head -20` → 200 且含至少 2 个 `cloudpivot_*` 指标名；
> 单测：端点 200 + 内容含 `# HELP`。**全部达成，实测见 01 / 02。**

## 实测环境

| 项 | 值 |
|----|----|
| 主机 | 172.18.10.99（开发机，非 144/145） |
| 后端 | `./target/debug/cloudpivot`，监听 `127.0.0.1:8023` |
| 库 | 新建空库 `cloudpivot_p105`（`create database … owner alun`），首启自动跑完迁移链 |
| profile | `NIK_PROFILE=p105` —— **仓库里没有 config-p105.toml**，即「只有 `config/config.toml` 基础配置」的空库首启场景 |
| 启动要点 | `NIK_DATABASE_USER/PASSWORD/NAME`、`CLOUDPIVOT_STORAGE_MASTER_KEY`（64 hex） |

启动日志关键两行（证明端点真的挂上了、且默认只允许回环）：

```
INFO nik_web::app: 指标端点已启用 path=/metrics 访问范围：仅本机回环（[metrics].allow_ips 为空）
INFO nik_web::app: cloudpivot 启动 -> http://127.0.0.1:8023
```

## 改了什么、为什么

| 文件 | 改动 | 原因 |
|------|------|------|
| `backend/config/config.toml` | 新增 `[metrics] enabled = true / path = "/metrics" / allow_ips = []` | 端点本身由框架提供，但**开关是运行时配置**；基础配置里没有它时，空库首启/新机器（私有 profile 是 gitignore 的）`/metrics` 直接 404 |
| `backend/src/shared/observability.rs` | **新增**：业务指标目录 `CATALOG`（19 条：名字/类型/单位/HELP）+ `set_gauge` / `inc_counter` / `describe_once` + 4 条单测 | 框架只 describe 它自己的 `nik_*` 指标；`cloudpivot_*` 之前是裸序列，**没有 `# HELP` / `# TYPE`**。另外 `metrics` 的 `describe_*` 在 recorder 安装前会被静默丢弃，所以登记挂在「写入前」（`Once`），不依赖启动钩子时序 |
| `backend/src/{shared/token_revocation,support/health,system/auth/cleanup}.rs` | 4 处 `metrics::gauge!/counter!` → `observability::set_gauge/inc_counter` + 名字改用常量 | 名字与 HELP 的单一出处；`debug_assert` 拦住「写了名字但没登记」 |
| `backend/src/task/metrics_sampler.rs` | 指标名改用目录常量；`run_once` 走 helper；**注册时立即补采一次** | 采样 cron 是「每分钟第 0 秒」，启动到首个整分钟之间 `/metrics` 上一条 `cloudpivot_*` 都没有，发布窗口里抓取器会以为 exporter 坏了 |
| `frontend/deploy/nginx.conf` | 新增 `location = /metrics`（默认 `deny all` + 回环放行 + **清空 X-Forwarded-For / X-Real-IP**） | 卡面「生产走 nginx 白名单」。不清空来源头的话，后端按真实客户端 IP 判定，白名单里的合法抓取端反而会被后端 403 |
| `docs/运维规程-指标端点与抓取.md`（新增）+ `docs/README.md` 索引 | 运维口径：端点/开关/指标清单/三种访问形态/排查表/新增指标步骤 | 端点有了，口径不能只活在代码注释里 |
| `scripts/verify/p105_metrics_acceptance.sh` / `p105_metrics_nginx.sh`（新增） | 可重跑的两组验收脚本 | 01 / 03 两份证据就是它们产出的 |

**没有**改框架 `vendor/nik-*`：`/metrics` 的挂载、IP 判定、Prometheus 渲染本来就在框架里
（`nik-web/src/app.rs::attach_metrics_endpoint` / `metrics.rs::metrics_endpoint`），
本卡补的是「默认开启 + 业务指标可读 + 访问控制落到 nginx」。

## 验证结果

| 证据 | 命令 | 结论 |
|------|------|------|
| `01_acceptance_curl.out` | `bash scripts/verify/p105_metrics_acceptance.sh 8023` | `http=200 bytes=18599 content_type=text/plain; version=0.0.4`；**18 条 `cloudpivot_*` 序列**且每条都有 `# HELP` / `# TYPE`（目录 19 条，`cloudpivot_token_blacklist_cleanup_deleted` 这个 counter 还没累加过所以无序列 —— 未出现过的指标不会凭空导出）；`X-Forwarded-For: 203.0.113.9` → **403**；`/api/metrics` → 404（端点不在 prefix 下） |
| `02_backend_unit_tests.out` | `cargo test --lib shared::observability` / `task::metrics_sampler` | 4 + 8 全过（含「端点 200 + 含 `# HELP`」「非白名单 403」「采样器每个名字都已登记」） |
| `03_nginx_whitelist.out` | `bash scripts/verify/p105_metrics_nginx.sh 18080 8023` | 临时 nginx：回环 → 200（18 条序列）；**非白名单来源（172.18.10.99）→ 403**；伪造 `X-Forwarded-For: 127.0.0.1` 过 nginx → 200（头被清空，后端按回环放行）；直连后端伪造外部来源 → 403 |
| `04_fmt_clippy.out` | `cargo fmt --check` / `cargo clippy --lib --tests -- -D warnings` | 均 exit 0，零告警 |
| `05_alert_check.out` | `bash scripts/ops/alert_check.sh --dry-run --only invariant,backlog` | `backlog: 数据源 = /metrics` —— 告警脚本走的是主路径，不再退到 psql |

nginx 模板另在 144 用真实 nginx 二进制做过语法校验（只 `nginx -t`，**未 reload、未改现网配置**）：

```
nginx: the configuration file /tmp/p105-nginx/nginx.conf syntax is ok
nginx: configuration file /tmp/p105-nginx/nginx.conf test is successful
```

## 边界（明确没做）

- **144 现网 nginx 未改动**：`/etc/nginx/sites-enabled/cloudpivot.conf` 里还没有 `/metrics` 的 location，
  所以生产上 `curl http://172.18.10.144/metrics` 目前仍会落到 SPA 回退拿到 HTML。
  模板已进仓库，需随下次发布 `nginx -t && systemctl reload nginx`（发布流程内的动作，不在本卡范围）。
- 框架侧 `nik_http_*` 指标未改（不归本仓维护）。
- 本卡不新增数据库迁移、不改任何业务接口/权限码。
