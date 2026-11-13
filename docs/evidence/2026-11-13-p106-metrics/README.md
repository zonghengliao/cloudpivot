# P1-06 证据：4 个关键指标（请求数 / 延迟 / 错误率 / DB 连接）

> 卡面验收：`curl -s http://127.0.0.1:8023/metrics | grep -E "requests_total|duration_seconds|errors_total|pool_connections"`
> → 4 类都在；`hey -n 100 -c 5 …` 后数值增长。**全部达成，实测见 00/01/02/03/04。**
>
> 本机实例监听 `127.0.0.1:18096`（隔离端口，不碰 8023/144），数据库 `cloudpivot_p106_test`。

## 实测环境

| 项 | 值 |
|----|----|
| 主机 | 172.18.10.99（开发机，非 144/145） |
| 后端 | `./target/debug/cloudpivot`，监听 `127.0.0.1:18096` |
| 库 | 新建空库 `cloudpivot_p106_test`（`create database … owner alun`），首启自动跑完 **335 条迁移** |
| profile | `NIK_PROFILE=p106` —— 仓库里**没有** `config-p106.toml`，即「只有基础配置」的空库首启 |
| 压测工具 | `hey 0.1.5`（`go install github.com/rakyll/hey@latest`；本机原本没有，脚本里另有 `xargs -P5` 兜底） |
| 启动日志 | `指标端点已启用 path=/metrics`、`数据库迁移完成: 应用 335 个迁移`、`运维观测采样任务已注册` |

## 改了什么、为什么

| 文件 | 改动 | 原因 |
|------|------|------|
| `backend/src/middleware/http_metrics.rs`（新增） | HTTP 埋点层：`cloudpivot_http_requests_total{method,path,status}` / `cloudpivot_http_request_duration_seconds{path}` / `cloudpivot_http_errors_total`；path 归一化 + 512 标签上限 + 只统计接口流量 + `/metrics` 自排除 + 6 条单测（含真实 500 用例） | 前三条指标的埋点位置只有一个正确答案：**框架链之外**（`layer_from_fn`）。挂在链内或 handler 里，框架自己拒绝的 401/403/429 与 404 兜底就不会进指标，故障时会得出「成功率 100%」的错觉 |
| `backend/src/lib.rs` | 注册 `middleware::http_metrics::layer`（位于 `request_context` 内侧、`operation_log` 同级） | 同上；放在 `request_context` 之前 ⇒ 指标层量的是「除 request_id 注入外」的端到端耗时 |
| `backend/src/shared/observability.rs` | `MetricKind::Histogram` + `MetricDef::histogram`；新增 5 个指标名（4 张卡面指标 + `db_pool_max_connections` 便于算使用率）；`set_gauge_labeled` / `inc_counter_labeled` / `observe_histogram` / `seed_zero_series` | 名字/类型/HELP 的单一出处（P1-05 定的规矩）；带标签与直方图此前没有写入入口 |
| `backend/src/task/metrics_sampler.rs` | 每分钟采一次连接水位（`pg_stat_activity`：idle / active / total，**排除采样自己的连接**）+ 池配置上限；解析函数可脱库单测 | 卡面「`AppState` 的 DB pool 每分钟采样一次」。框架的 `RawExecutor` 是 `pub(crate)`，业务侧拿不到池内 `in_use/idle`；服务端口径还能看出「外部客户端占了配额 / 有连接泄漏」 |
| `backend/src/task/metrics_sampler.rs` | `run_once` 调 `obs::seed_zero_series()` | Prometheus **不导出没有任何数据点的指标**：5xx 计数在健康系统上本会完全消失，`grep errors_total` 会以为没上线（P1-05 的 `token_blacklist_cleanup_deleted` 就栽在这） |
| `docs/运维规程-指标端点与抓取.md` | 指标清单补 5 行 + 新增 §2.1（口径/基数/seed/`/api/health` 不存在）+ §4/§6 更新 | 运维口径不能只活在代码注释里 |
| `scripts/verify/p106_metrics_acceptance.sh`（新增） | 8 组断言 + 压测 + 证据落盘 | 00~04 号证据就是它产出的 |

**没有**改框架 `vendor/nik-*`：HTTP 与 DB 的埋点都能在业务侧完成，不需要动框架。

### 为什么不直接复用框架的 `nik_http_*`

| 维度 | `nik_http_*`（框架） | `cloudpivot_http_*`（本卡） |
|------|----------------------|------------------------------|
| 状态码 | 只到类别 `2xx/4xx/5xx` | **精确码**（`404` / `422` / `500` 分开） |
| `/health` | 排除 | 计入（验收要压 health） |
| 错误率 | 需 `sum by (status="5xx")`，规则易写错 | 一条 `cloudpivot_http_errors_total` 直接 `rate()` |
| DB 连接 | 只有语句数与耗时 | 连接水位三档 + 池上限 |

两套标签集不同、语义不同，不是同一条序列写两遍；代价是每请求多 2 次原子自增 + 1 次直方图观察。

## 验证结果

| 证据 | 命令 | 结论 |
|------|------|------|
| `00_metrics_before.txt` | `curl -s :18096/metrics` | 压测前基线；四类指标族全在，且都带 `# HELP` / `# TYPE` |
| `01_four_categories.txt` | `grep -E "requests_total\|duration_seconds\|errors_total\|pool_connections"` | **卡面 grep 口径**：`cloudpivot_http_requests_total` / `cloudpivot_http_request_duration_seconds`（含 0.01/0.05/0.1/0.5/1/5 桶）/ `cloudpivot_http_errors_total` / `cloudpivot_db_pool_connections` + `_max_connections` |
| `02_load_heavy.txt` / `02_load_cardy.txt` / `02_load_summary.txt` | `hey -n 100 -c 5 http://127.0.0.1:18096/health` 与 `…/api/health` | 各 100 请求 / 5 并发；`/health` 约 2293 req/s |
| `03_metrics_after.txt` | 再抓一次 `/metrics` | `/health` `status="200"` → **102**；`/api/health` `status="404"` → **100**；直方图 `_count` → **102**；`errors_total` 仍为 **0**（100 次 404 没污染错误率）；`path="/metrics"` 未出现（抓取不计数） |
| `04_acceptance_log.txt` | `bash scripts/verify/p106_metrics_acceptance.sh 18096 …` | **15 条 PASS / 0 FAIL，exit=0** |
| `05_unit_tests.out` | `cargo test --lib middleware::http_metrics` / `shared::observability` / `task::metrics_sampler` / 全量 `cargo test --lib` | 6 + 4 + 10 = **20 passed / 0 failed**；全量 **5586 passed / 0 failed**。其中 `metrics_are_exported_after_real_requests` 起真 axum 服务打**真实 500**：健康时 `errors_total = 0`（seed 出来的 0 序列），来一次 500 后变 **1**，同时 `requests_total{status="500"}=1`、2xx 那条纹丝不动 |
| `06_fmt_clippy.out` | `cargo fmt --check` / `cargo clippy --lib --tests -- -D warnings` | 均 exit 0，零告警 |

### 顺手修掉一条「别人留下的红」

全量 `cargo test --lib` 第一次跑是 **5585 passed / 1 failed**：`task::tests::dynamic_registration_overwrites_builtin_of_same_name`
在**同步上下文**里调 `Scheduler::register_builtin()`，而 P1-05 在这里加的「注册即补采一次」用的是裸
`tokio::spawn` —— 没有 runtime 就 panic `there is no reactor running`。不是本卡引入的
（`git show HEAD:backend/src/task/metrics_sampler.rs | grep -n tokio::spawn` 能看到第 385 行；
P1-05 当时只跑了 `task::metrics_sampler` / `shared::observability` 两个过滤，没跑全量）。
修法：改用 `tokio::runtime::Handle::try_current()`，没有 runtime 就跳过补采（真跑起来时一定有时）。
修完全量 **5586 / 0**。

## 两个必须说清楚的边界

1. **卡面的 `/api/health` 不存在**（实测 404）：`/health` 是根路径探针，手动注册以豁免
   `[router].prefix`（AGENTS 约定 8）。所以 `hey … /api/health` 打到的是 404 —— 本卡照样
   按 `status="404"` 精确计数（证据 03：100 次），但它**不是**健康检查。要压活的探针用 `/health`。
2. **5xx 没有在隔离实例上人为制造**：试过 `pg_terminate_backend` 掐连接，池按需重连后
   `/health/deps` 仍然 200（这反而说明池的容错是好的）。错误数增长路径由单测覆盖 ——
   那里是**真的 HTTP 500**（axum 路由返回 500），断言 `errors_total` 从 0 → 1。
   运行期证据里能证明的另一半是「4xx 不进错误率」：100 次 404 后 `errors_total` 仍为 0。

## 边界（明确没做）

- **144 现网未部署**：本卡只改代码与配置口径，发布动作（`cargo build` + `systemctl restart`）不在卡内；
  144 的 nginx `/metrics` location 仍待随发布 `reload`（P1-05 遗留，模板已入库）。
- 本机验收库 `cloudpivot_p106_test` 留在 99，供复跑；`cloudpivot_p105_test` 同样待清理脚本回收。
