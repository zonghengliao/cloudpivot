# 云枢 · 批次 C 内容管理 技术架构设计

> 公告管理 · 帮助文档 · 变更日志（含「回滚不留假日志」）
>
> | 项 | 值 |
> |---|---|
> | 项目 | 云枢通用后台（CloudPivot，Rust 1.94.1 / edition 2024 / alun 0.4.2 / PostgreSQL 18.6） |
> | 代码基线 | `172.18.10.144:/opt/cloudpivot`，HEAD **`a234762`**（`feat(frontend): B8 权限增强前端`）；批次 A（存储/导入导出/通知）与批次 B（数据权限/列级脱敏/角色互斥）均已上生产 |
> | 基线事实 | 30 张表 · 迁移到 **V0068** · `cargo test` 全绿（任务书口径 **362**，B7 证据文档记录 **373**；两处数字不一致，本批按「不回归」处理，实施时以真机 `cargo test` 实测为准）· 门禁闸①/② 0/0 |
> | 迁移占用 | 已用 `V0001`–`V0052`、`V0060`–`V0068`；**本批从 V0080 起**（V0069–V0079 留空给批次 A/B 线上补丁） |
> | 本文档性质 | **只做设计，不交付业务代码**。所有 DDL 在真库 PG 18.6 实跑；所有性能结论有 `EXPLAIN (ANALYZE, BUFFERS)` 实测；所有代码事实附 `文件:行号` |
> | 验证环境 | PG 18.6（Ubuntu aarch64），验证库 `c_batchc_v1`（对比实验）/ `c_batchc_v2`（本批 DDL 实跑，11 张表）；脚本见 §附录 B |
> | 本地代码镜像 | `/root/.hermes/workspace/architect/baseline/{src,migrations,frontend-src}`（rsync 自生产 HEAD，仅供检索定位） |

---

## 目录

| 章节 | 内容 | 谁最该先读 |
|---|---|---|
| §0 | 决策摘要（15 条硬结论 + 5 处与任务书的偏差 + 交付边界） | 全体（5 分钟） |
| §1 | 现状核查：`operation_logs` 写入时机 / `support/log` 总线 / `notice::dispatch` 签名 / `storage` 上传接口 / XSS 能力 / scope 基座 / 调度器（全部带行号） | 后端实施者 |
| §2 | 公告管理：与 `sys_notice` 的边界与联动、4 张表、发布范围与数据权限结论、**Markdown vs 富文本 + XSS 四道闸**、附件接入、自动下线、已读/未读、API | 后端 + 安全 + 产品 |
| §3 | 帮助文档：分类树、Markdown 渲染与 TOC、**全文检索结论（tsvector 不可用的实测证据）**、权限、与公告分表的理由、API | 后端 + 产品 |
| §4 | 变更日志：**假日志的可复现证据**、三方案对比、`sys_change_log` 月分区、diff 结构、大字段摘要化、**脱敏 fail-closed**、索引、保留、API、**「回滚不留假日志」的可验证测试** | 后端 + 安全 + 评审 |
| §5 | 横切：20 个权限码 / V0080–V0089 分配 / **需报批的底座改动（4 必改 + 2 建议）** / C7 测试点 / 前端 6 页 / 风险 14 条 | 评审 + 老大 |
| §6 | 附录 A：V0080–V0085 完整 DDL（V0080–V0084 已在 PG 18.6 实跑通过；A.6 为 V0085 种子结构示例） | 后端实施者 |
| §7 | 实施拆分 C0–C9（每步含可执行验收 + 人日） | 项目负责人 |
| §8 | 未决项（需老大/产品拍板） | 老大 |
| 附录 B | POC 脚本清单 + 实测输出 + 复跑命令 | 复核者 |
| 附录 C | 事实索引（每条结论 → 文件:行号 / POC 编号） | 评审 |

---

## 0. 决策摘要（先看这一页）

### 0.1 十五条硬结论

| # | 决策点 | 结论 | 一句话理由（含证据） |
|---|--------|------|---------------------|
| **D1** | 公告与站内消息的关系 | **公告是「内容」，`sys_notice` 是「投递」；发布公告默认触发一次 notice 扇出，但两者是两套表、两套生命周期** | `sys_notice` 已有 `notice_type='CIRCULAR'`（`business/notice/dto.rs:39-40`）与 `biz_type/biz_id/link_url` 字段（`migrations/migrations/business/V0043__create_notice.up.sql:28-30`），天生适合做「公告的投递副本」；把正文塞进 `sys_notice.content` 会让公告无法承载附件、置顶、有效期、版本 |
| **D2** | 公告→通知的联动一致性 | **先提交公告事务，再调 `dispatch`，用幂等键 + 补偿任务兜底「已发布但没通知」** | `notice::dispatch` 内部自己开事务（`business/notice/mod.rs:473`、定时分支 `:436`），**无法**与公告事务合并；`uk_sys_notice_idem`（`V0049:27-28`）保证重试幂等，所以「提交后重试」是最小改动且可收敛的方案 |
| **D3** | 公告「发布范围」是否受数据权限影响 | **不受**。发布范围是**业务受众**（业务语义），数据权限是**数据归属过滤**（安全语义），两者正交 | 若套用 data_scope：一个 `SELF_ONLY` 范围的角色会看不到「全员公告」——语义荒谬。结论文档 §2.3 给出判据表（哪些表该注入、哪些不该） |
| **D4** | 公告内容形态 | **Markdown 存储 + 服务端渲染 + 双端净化**；不采用「富文本 HTML 直存」 | 实测（POC-3）：alun 现成的净化器（ammonia 4.2.4 默认规则）会把 WYSIWYG 输出的 `style`/`class`/`target`/`data:` 图片全部剥掉——**富文本「所见非所得」**；而 Markdown 渲染出的语义 HTML 几乎原样通过 |
| **D5** | XSS 防护 | **四道闸**：①写入前渲染即净化（`alun_utils::xss`）②入库再净化一次（幂等）③出参前第三次净化（防御纵深）④前端 `v-html` 只允许经过②③的内容 + CSP + DOMPurify 兜底；**图片禁止 base64 内嵌，必须走 storage** | POC-1/2：默认规则下 `data:image` 的 `src` 被整体删除（图片直接消失），`<script>`/`onclick`/`javascript:`/`<iframe>` 全部清除；净化成本 20 KB **0.635 ms**、64 KB **1.976 ms**（POC-2 实测） |
| **D6** | 公告附件 | **引用 `sys_file`（批次 A 存储），下载走「公告可见性校验 + 服务端流式代理」的新接口，不暴露 `business:storage:download`** | 现有 `business:storage:download` 已授予**所有角色**（`V0046:114-125`），即任何登录用户可凭任意 `file_id` 下载任意文件；公告附件若复用该接口等于把「附件可见性」交给 URL |
| **D7** | 公告已读/未读 | **要**，但只记「已读」（`sys_announcement_read`），未读 = 可见集合 − 已读集合；**公告未读独立于通知未读**，前端两个角标分开展示，另给聚合接口 | 为全员公告预写 N 行未读回执会让 2000 条公告 × 5000 人 = 1000 万行；实测动态判定：可见列表分页 **0.258 ms**、未读数 **1.515 ms**（POC-12，2000 公告 / 4665 范围行） |
| **D8** | 帮助文档全文检索 | **不上 `tsvector`**，用 `ILIKE`；仅在文档数 > 5000 时追加 `pg_trgm` GIN 索引（DDL 已备）。**不要**引入 zhparser（本机 PG 18.6 无该扩展） | 实测（POC-6/7）：PG 默认分词把整段中文当一个词——`to_tsvector('simple','系统公告管理') @@ plainto_tsquery('simple','公告')` = **false**；trgm 只对 **≥3 字**有效（3 字 0.238 ms vs 全表 131 ms），**2 字反而更慢**（153 ms > 131 ms）；2000 篇真实规模下 `ILIKE` 全表仅 **4.2–6.1 ms** |
| **D9** | 帮助文档与公告是否共用内容表 | **分开**（4 条硬理由：受众不同、生命周期不同、检索要求不同、权限码命名空间不同） | 见 §3.5 对比表；合并会让「已读回执/置顶/失效」这些公告专属字段在帮助文档上永远是 NULL，并为帮助文档引入不该有的扇出逻辑 |
| **D10** | 变更日志总体方案 | **选 (c) 混合**：新建 `sys_change_log`（业务事务内显式写 diff）+ 保留 `operation_logs` 降级为「请求流水（含失败）」+ 明确 `audit_logs` 的定位为「登录/安全类事件」 | 见 §4.2 三方案对比。(a)「修 `operation_logs`」不可行：它是 HTTP 中间件写的（`middleware/operation_log.rs:104-173`），**没有业务事务可加入**；且它承载 12 类模块的路径→动作映射（`:321-433`），改成事务内等于让每个 handler 自己写日志，55 处调用点语义全变 |
| **D11** | 「回滚不留假日志」的机制 | **成功路径的日志必须与业务同事务写（同一 `&impl SqlExecutor`），失败尝试不进 `sys_change_log`** | 并发实测（POC-4）：业务事务 ROLLBACK 后，**异步总线写日志 → 日志残留 1 行（假日志）**；同事务写日志 → 0 行。这条不变量是可被机器验证的（§4.12 三层测试） |
| **D12** | diff 存储形态 | **JSONB，只记变化字段，`{b,a}` 成对**；单字段值 > **2 KiB** 或为 text/bytea 时降级为 `{len, sha256, preview(200 字), truncated}` | 实测（POC-5）：8k 字富文本「前后双快照」落盘 **51 KB/行**，摘要化后 **2.7 KB/行**（19×）；只记变化字段的 6 字段用户修改仅 **355 B** |
| **D13** | 变更日志脱敏 | **库内永不存明文**：写入前按「敏感列清单」无条件脱敏（不看操作人角色），读取时再按读取者规则二次脱敏；**`SUPER_ADMIN` 也看不到日志明文** | 若按「操作人豁免」逻辑：超管改手机号 → 日志存明文 → 日志即后门。批次 B 的 `sys_field_mask`（`V0062`）+ 内置兜底清单（与 `operation_log.rs:60-73` 的 12 个敏感字段名对齐）作为判据 |
| **D14** | 保留与分区 | `sys_change_log` **月分区 + 保留 36 个月 + `DROP PARTITION` 清理**；**不建 DEFAULT 分区** | 实测（POC-9/10）：`DROP TABLE 分区` **1.5 ms** vs `DELETE` 3.3 万行 **37.9 ms** 且留死元组；有 DEFAULT 分区时新建正式分区会失败（`updated partition constraint ... would be violated`，因 default 里已堆了 11 万行） |
| **D15** | 迁移与底座改动 | V0080–V0086 落地（V0087–V0089 预留）；**底座必改 4 项**（`Cargo.toml` 加 Markdown 渲染器 / `task/mod.rs` 注册 **5** 个定时任务 / `error_code.rs` 加 62xxx 段 / `business/mod.rs` 加 3 个 `pub mod`） | 逐项「为什么不能绕开」见 §5.4.1。**不改** `support/log`、**不改** `shared/scope.rs` 的内容（用现成的 `ScopeTarget::unscoped` + `paginate_unscoped`，见 `shared/scope.rs:651`） |

### 0.2 与任务书假设的偏差（必须先对齐，共 5 处）

| 任务书假设 | 代码实况（证据） | 本设计的处理 |
|---|---|---|
| 「`operation_logs` 异步旁路，事务回滚仍会记日志」 | 属实，且比描述更具体：它是 **HTTP 中间件**在 `next.run()` **之后**入队（`middleware/operation_log.rs:133` 响应、`:150` 入队），后台 worker 用**独立连接、autocommit** 落库（`support/log/bus.rs:204-219`）。全仓**只有这一个写入口**（`grep -rn enqueue_operation` → 仅 `operation_log.rs:150`） | §4.1 给出可复现的并发证据；结论与任务书一致，但把「修它」改写为「**它不适合承载 diff**」，因为它天然拿不到业务事务与前后值 |
| 「修现有 `operation_logs` 改成事务内记录」 | 技术上不可行：中间件运行在 axum 层（`App::layer_from_fn`，`lib.rs:35-37` 装配说明），**没有** `Transaction` 句柄；且它靠「路径→动作」映射猜测业务语义（`:321-433`），拿不到「哪个字段从什么改成什么」 | 方案 (a) 在 §4.2 判为**不可行**并保留为对比行；真正落地的是 (c) 混合 |
| 「`audit_logs` 是独立的审计链路」 | `record_audit`（`support/log/mod.rs:204-220`）同样走异步总线，**与业务事务无关**；`before_data/after_data` 有列但无字段级配对，且**没有任何按对象反查的接口**（`LogPageQuery` 只有 username/operation/request_path/status_code/时间，`support/log/dto.rs:122-139`） | 本批**不迁移** `audit_logs` 的写入链路（会动 55 个调用点），只在其文档定位里写明「事件级审计，非字段级」；字段级能力由 `sys_change_log` 承担 |
| 「公告的富文本用前端安全渲染即可」 | 项目**没有任何** Markdown 渲染器、没有 DOMPurify、没有富文本编辑器（`backend/Cargo.toml` 与 `frontend/package.json` 均无命中）；但后端已开 alun 的 `xss` feature（`Cargo.toml` 注释「HTML 净化：社区公告等富文本入库前过滤」）→ 现成 ammonia 4.2.4 | 渲染放服务端（新增 1 个依赖，见 §5.4.1 B-1），净化复用现成的 `alun_utils::xss`；前端**仍加一道** DOMPurify 兜底（新增前端依赖） |
| 「帮助文档可以考虑 tsvector」 | PG 18.6 实测：默认分词器对中文整段成词，子串检索失效；`pg_available_extensions` 里**没有** `zhparser` / `pg_jieba`（只有 `pg_trgm` / `btree_gin` / `pgcrypto`） | D8：不用 tsvector，给出「何时再升级」的量化触发条件（文档数 > 5000 或 P95 > 50 ms） |

### 0.3 交付边界（本批**不做**）

1. **不做**工作流审批（批次 D）：公告不接审批流，`sys_announcement` 不预留 `flow_instance_id`（等 D 批次统一设计，避免两套审批字段）。
2. **不做**公告的富文本编辑器与「所见即所得」：内容形态定为 Markdown，编辑器为「Markdown + 实时预览」。
3. **不做** `operation_logs` / `audit_logs` 的写入链路改造（风险 > 收益，见 §4.2）。
4. **不做**帮助文档的「按角色可见」（D-未决项 3，需产品拍板；本批只做「全部登录用户可读」）。
5. **不做**变更日志的跨库/跨租户归档导出（只做分区 DROP + 手工 mysqldump 级备份建议）。
6. **不做**中文分词搜索（§8 未决项 2）。
7. **不迁**存量日志到 `sys_change_log`（§4.11 给迁移策略：从上线时刻起算，存量只读保留）。

---

## 1. 现状核查（全部实测，不是文档推测）

### 1.1 基线核对

```bash
# 生产 HEAD 与迁移号段（真机核对）
$ ssh root@172.18.10.144 'cd /opt/cloudpivot && git log --oneline -1'
a234762 feat(frontend): B8 权限增强前端 —— 数据范围 / 脱敏规则 / 角色互斥
$ ssh root@172.18.10.144 'ls /opt/cloudpivot/backend/migrations/*/ | grep -o "V00[0-9]*" | sort -u | tail -5'
V0064 V0065 V0066 V0067 V0068
```

| 项 | 事实 | 来源 |
|---|---|---|
| 模块树 | `system/{auth,dept,menu,role,user}`、`support/{config,dashboard,dict,file,health,ipgeo,job,log,online}`、`business/{impexp,masking,monitor,mutex,notice,permission,shared,storage}` | `find src -maxdepth 2 -type d` |
| 业务模块容器 | `business/mod.rs:16-23` 逐模块 `pub mod`，并有一段**编译期契约**证明外部可调 `notice::dispatch`（`business/mod.rs:25-63`） | 同文件 |
| 路由收集 | 宏声明（`#[alun::post("/business/...")]`），`App::scan()` 统一收集，**无需手工注册** | `lib.rs:26-28`、`lib.rs:57` |
| 中间件链 | 操作日志中间件在最外层（覆盖 401/403/429/404） | `lib.rs:35-37` |
| 调度器 | `task/mod.rs` 单例，`register_builtin()` 一次性注册（`task/mod.rs:228-245`），框架对注册表做**启动时快照** | `task/mod.rs:8-9`、`:32-33` |
| 保留期常量 | `OPERATION_LOG_RETENTION_DAYS = 90` | `shared/constants.rs:276` |
| 写方法白名单 | `AUDITED_METHODS = ["POST","PUT","PATCH","DELETE"]` | `shared/constants.rs:270` |

### 1.2 `operation_logs` 的写入时机（本批最关键的现状）

**写入链路（三跳）**

```
HTTP 请求
  → middleware/operation_log.rs::record()        [axum 层，最外层]
      · 只对 POST/PUT/PATCH/DELETE 生效           (:106-109)
      · 先捕获并脱敏请求体                        (:128-130)
      · next.run(request).await  ← 业务在这里执行完   (:133)
      · 捕获并脱敏响应体                          (:136)
      · 从「响应扩展」里补 AuthClaims              (:138-145)
      · bus::enqueue_operation(OperationLogModel)  (:150)
  → support/log/bus.rs::enqueue()                [无界 mpsc，零阻塞]  (:112-132)
  → worker_loop 批量落库（独立连接 + autocommit）  (:204-219)
      · MAX_BATCH_ROWS = 256 / FLUSH_INTERVAL = 500ms  (:36, :39)
```

**由此得到三条硬结论**

1. **该日志与业务事务没有任何关系**：`next.run()` 返回时业务事务早已提交或回滚；入队发生在响应返回前，落库发生在 500 ms 内的后台批次。因此「事务回滚仍然记日志」是**结构性的**，不是 bug 级别的小疏漏。
2. **该日志只知道「HTTP 方法 + 路径 + 状态码」，不知道「字段从什么改成什么」**：`detail` 字段被显式留空（`:158-159` 注释「操作详情无结构化来源，保持为空」）。
3. **该日志是「请求流水」而非「变更凭证」**：一次 `POST /api/system/user` 可能在业务里建 1 个用户 + 3 条角色绑定 + 1 条审计；反过来一次「批量导入 500 行」只产生 1 行日志。

**唯一写入口（已核对）**

```bash
$ grep -rn "enqueue_operation" src/
src/middleware/operation_log.rs:150:        bus::enqueue_operation(OperationLogModel {
src/support/log/bus.rs:139:pub(crate) fn enqueue_operation(model: OperationLogModel) {
```

→ 想「把 `operation_logs` 改成事务内记录」，就必须把写日志的动作搬进 30+ 个 handler，并让 handler 知道「本请求动了哪张表哪个字段」。这不是修一个函数，是改一套架构 —— 这也是 §4.2 判 (a) 不可行的核心证据。

**假日志的可复现证据（POC-4，§4.12 有脚本）**

三个独立 psql 会话并发（业务事务 rollback / 异步日志 / 同事务日志）：

```
 biz_rows_after_rollback | fake_log_rows_async | intx_log_rows
-------------------------+---------------------+----------------
                       0 |                   1 |              0
```

- 业务行回滚后 **0 行**（正确）；
- 异步路径写的日志 **1 行**（**假日志**，与当前 `operation_logs` 同构）；
- 同事务路径写的日志 **0 行**（正确 —— 这就是 `sys_change_log` 的做法）。

### 1.3 `support/log` 模块：五张表 + 一个总线

| 文件 | 行数 | 职责（据文件头注释） |
|---|---|---|
| `support/log/model.rs` | 503 | 五张日志表强类型模型 |
| `support/log/bus.rs` | 390 | 非阻塞总线（无界队列 + 后台批量落库） |
| `support/log/handler.rs` | 315 | 分页 / 详情 / 清理（`/api/support/log/*`） |
| `support/log/mod.rs` | 310 | 四个 `record_*` 写入函数 + 入参结构 |
| `support/log/plugin.rs` | 84 | 随应用生命周期启停 + 闪盘（`flush()` 最多等 5 s，`bus.rs:42`） |
| `support/log/dto.rs` | 156 | 请求/响应 DTO |

**四个 `record_*` 全部是「投递即返回」**（`mod.rs:20-29` 头注释明写「同步、无阻塞…不触碰数据库」）：

```rust
// support/log/mod.rs:204  （audit 与 operation 同构：都是入队）
pub(crate) fn record_audit(headers: &HeaderMap, connect_info: &SocketAddr, input: AuditLogInput) {
    bus::enqueue_audit(AuditLogModel { ... created_at: chrono::Utc::now() });   // :205-219
}
```

`record_audit` 是 `pub(crate)`，**签名里没有 `Tx`/`Executor` 参数** —— 这一点决定了「本批不可能复用 audit_logs 做字段级变更日志」，必须新建表（§4.2 理由 2）。

**五张表现状与定位**

| 表 | 写入者 | 现有字段（关键） | 本批定位 |
|---|---|---|---|
| `operation_logs` | 中间件（唯一） | request_id / operation / target_type / target_id / **detail(空)** / request_params / response_data / status_code / duration_ms | 降级为**请求流水**（含失败），90 天 |
| `audit_logs` | `record_audit`（55 处调用） | operation / target_type / target_id / **before_data / after_data**（JSONB 全量快照） | 保留为**事件级审计**，不改写入链路 |
| `auth_logs` | `record_auth` | auth_type / auth_status / 设备 / IP 属地 | 不动 |
| `security_logs` | `record_security` | 安全事件 | 不动 |
| `password_change_logs` | `record_password_change` | 改密留痕 | 不动 |
| **`sys_change_log`**（本批新建） | 业务 handler（**同事务**） | 见 §4.3 | **字段级变更凭证**，36 个月，月分区 |

### 1.4 `notice::dispatch` 的签名与能力边界

**对外唯一发送口径**（`business/notice/mod.rs:246`）：

```rust
pub async fn dispatch(draft: NoticeDraft) -> std::result::Result<DispatchResult, DispatchError>
```

**入参 `NoticeDraft`（`mod.rs:77-118`，24 个字段）**，与公告联动的关键字段：

| 字段 | 类型 | 公告场景取值 |
|---|---|---|
| `notice_type` | `NoticeType`（`dto.rs:30-41`） | `Circular`（公告，已存在） |
| `level` | `NoticeLevel` | 由公告 level 映射（URGENT→ERROR、IMPORTANT→WARN、NORMAL→INFO） |
| `title` / `content` | `String` | 标题 + **纯文本摘要**（不是 HTML，见 §2.4 理由） |
| `content_format` | `ContentFormat`（`dto.rs:124-131`） | `Text` |
| `link_url` | `Option<String>` | `/announcement/{id}` |
| `biz_type` / `biz_id` | `Option<String>` | `"announcement"` / 公告 id |
| `target` | `NoticeTarget`（`fanout.rs:36-45`） | `All` / `Role(vec)` / `Dept(vec)`（**含下级**）/ `User(vec)` |
| `channel_mask` | `u16` | `CHANNEL_INBOX`（1，站内信必投） |
| `idempotency_key` | `Option<String>` | **`announcement:{id}:v{version}`** |
| `expire_at` | `Option<DateTime>` | 公告 `expire_at` |
| `scheduled_at` | `Option<DateTime>` | 公告 `publish_at`（V0051 起支持定时发送） |
| `sender_id` / `sender_name` | — | 发布人 |

**能力边界（决定设计的三条）**

1. **`dispatch` 自开事务**：即时分支 `mod.rs:473`、定时分支 `mod.rs:436` 都是 `db().transaction(...)` → **无法嵌入调用方事务**（D2 的依据）。
2. **接收人上限 5 万**：`fanout::MAX_RECEIVERS = 50_000`（`fanout.rs:32`），超限 `422 + 60027`；扇出是单条 `INSERT ... SELECT ... ON CONFLICT DO NOTHING`（`fanout.rs:178-190`），部门目标用 `WITH RECURSIVE dept_tree`（`:359-361`）。
3. **幂等靠 DB 唯一索引**：`uk_sys_notice_idem ON sys_notice(idempotency_key) WHERE idempotency_key IS NOT NULL`（`V0049:27-28`）→ 重放返回 `duplicated = true` 与首次 `notice_id`（`mod.rs:16-18` 头注释）。

**权限码现状**：`business:notice:mine`（我的通知/未读数/已读/删除我的）在 `V0045:62-92` **授予了所有非超管角色**，这正是「人人可用的接口也要有权限码、且要授予全员」的既有范式 —— 公告与帮助文档的「读」接口沿用同一做法（§5.1）。

### 1.5 `business/storage` 的上传/下载接口（公告附件的接入面）

**模块（8472 行）**：`store`（ObjectStore 契约 + 本地/S3 后端）、`sigv4`（自研预签名）、`cipher`（AK/SK AES-256-GCM）、`router`（`StorageRouter` 按 `config_code` 选路）、`model`（配置/策略）、`handler`（2411 行）、`cleanup`（每日 04:30 回收）。

**上传（服务端）**：`POST /api/business/storage/upload?category=xxx`，`multipart` 字段名 `file`，权限码 `business:storage:upload`（`handler.rs:719-731`）：

| 语义（`handler.rs:702-716`） | 细节 |
|---|---|
| ① 按 `category` 取策略校验大小/扩展名/MIME | `resolve_policy(&category)` |
| ② SHA-256 秒传 | 命中 `find_reusable_file` → **不写对象、不插行**，直接返回旧记录（`:772-780`） |
| ③ 写对象 → 落 `sys_file`（带 `storage_code` + `upload_mode='SERVER'`） | `backend.put(...)` + `record.insert(db())`（`:790-800`） |
| 顺序约束 | `Content-Length` 大小校验必须在读 multipart **之前**（`:736-748` 有长注释解释 110 MB 报 10006 的坑） |

**上传（直传）**：`POST /api/business/storage/presign` → 返回 `upload_url`（浏览器直传），完成后调 `complete` 登记（`handler.rs:826-915`）。

**响应 `StorageFileRes`（`dto.rs:427-456`，17 个字段）**：`id / file_name / original_name / file_key / file_size / mime_type / extension / category / status / uploaded_by / created_at / url / storage_code / upload_mode`。

**下载**：`GET /api/business/storage/download/{file_id}`，权限码 `business:storage:download`（`handler.rs:1117-1122`）。

> ⚠️ **本批必须绕开的一个既有授权面**：`V0046:113-125` 把 `business:storage:upload / list / download` 授予了**所有非超管角色**。也就是说「任意登录用户可用任意 `file_id` 下载任意文件」。公告附件若直接复用该接口，「只有可见者能下载附件」这条业务规则就不成立（D6）。

### 1.6 XSS 能力现状（实测，不是查文档）

| 项 | 事实 | 证据 |
|---|---|---|
| 后端净化器 | **已有**：alun 开 `xss` feature → `alun_utils::xss::{sanitize_html, sanitize_html_strict, has_potential_xss}`，底层 **ammonia 4.2.4** | `backend/Cargo.toml`（feature 注释「社区公告等富文本入库前过滤」）；`alun-utils-0.4.2/src/xss.rs:29-31` |
| 已被业务使用 | 通知模板 HTML 骨架会先净化再渲染变量 | `business/notice/render.rs:156-159`（注释明确「顺序不能反：先渲染会把已转义的值再交给净化器」） |
| 默认允许的标签 | 含 `table/thead/tbody/tr/th/td`、`img`、`a`、`h1..h6`、`pre/code`、`ul/ol/li`、`div/span` 等 70+ | `ammonia-4.2.4/src/lib.rs:379-389` |
| 默认**不**允许 | `style` 属性、`class` 属性、`a` 的 `target`、`data:` URL scheme、`<script>/<style>`（整段连同内容删除） | 同文件 `:390-448`（`tag_attributes` 与 `url_schemes`） |
| 默认会注入 | `<a>` 上强制 `rel="noopener noreferrer"` | `:488`（`link_rel: Some("noopener noreferrer")`） |
| 陷阱（实测 panic） | `Builder::default().add_tag_attributes("a", &["target","rel"]).link_rel(Some(...))` → **panic**：`assertion failed: self.tag_attributes.get("a").and_then(|a| a.get("rel")).is_none()` | `ammonia-4.2.4/src/lib.rs:1910`，POC-1 真跑 |
| 前端 | **无** DOMPurify、**无** markdown 渲染器、**无**富文本编辑器 | `frontend/package.json` 检索 `markdown|dompurify|sanitize|tiptap|quill|wangeditor` → 0 命中 |
| 后端 | **无** Markdown 渲染器 | `backend/Cargo.toml` 检索 `markdown|comrak|pulldown` → 0 命中 |

**POC-1 实测输出（默认规则净化典型 WYSIWYG 输出）**

```
输入: <h2 style="text-align:center;color:#c00">季度公告</h2>
      <p style="text-indent:2em"><strong>各位同事</strong>：<span style="color:red">本周五</span>停机维护。</p>
      <p><a href="https://example.com/a?x=1" target="_blank" class="link">详情</a></p>
      <p><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==" alt="截图" width="600"></p>
      <table style="border-collapse:collapse"><tr><td colspan="2" style="padding:4px">合并单元格</td></tr></table>
      <script>alert(document.cookie)</script><p onclick="steal()">点我</p>
      <a href="javascript:alert(1)">恶意链接</a><iframe src="https://evil.example/x"></iframe>

输出: <h2>季度公告</h2>
      <p><strong>各位同事</strong>：<span>本周五</span>停机维护。</p>
      <p><a href="https://example.com/a?x=1" rel="noopener noreferrer">详情</a></p>
      <p><img alt="截图" width="600"></p>            ← src 被整体删除（data: 不被允许），图片消失
      <table><tbody><tr><td colspan="2">合并单元格</td></tr></tbody></table>
      <p>点我</p>                                     ← onclick 被剥离
      <a rel="noopener noreferrer">恶意链接</a>        ← javascript: href 被剥离
                                                      ← script / iframe 整段删除
```

**结论**：安全目标（去脚本）**完全达成**，但**表现力损失严重**：居中/缩进/颜色/类名/新窗口/内嵌图片全部丢失。这就是 D4「不用富文本 HTML 直存」的直接依据。

### 1.7 数据权限基座（批次 B 交付，本批必须顺着它走）

| 事实 | 证据 |
|---|---|
| 五种范围 `ScopeKind{All=1,Dept=2,DeptAndChild=3,SelfOnly=4,Custom=5}` | `business/shared/scope.rs:70-80` |
| 分页唯一入口 `scope::paginate(target, base_sql, params, order_by, page, page_size, principal)` | `scope.rs:618-631` |
| 不参与数据权限的表走 `paginate_unscoped(target, ...)`，且**必须**用 `ScopeTarget::unscoped("表名")` 构造（传 scoped 锚点直接 `DbError::InvalidValue`） | `scope.rs:651-680`、`scope.rs:208-216` |
| 登记表清单 `SCOPED_TABLES`（11 项，含 `operation_logs`/`audit_logs`/`sys_notice`/`sys_file`） | `scope.rs:274-286` |
| 启动自检 `preflight_permission()` 会校验登记表的部门列/归属列是否真存在 | `business/shared/guard.rs:218-237`、`:261-278` |
| 门禁闸①：`paginate_with_params` 只允许出现在 `scope.rs` | 批次 B 架构 §2.6；`scope.rs:600-616` 注释 |

→ **本批的列表接口必须二选一**：要么登记进 `SCOPED_TABLES`（并让 guard 校验），要么显式用 `unscoped` 锚点。**不登记的第三条路不存在**（门禁会拦）。

### 1.8 定时任务注册点（公告自动下线、分区预建、版本清理都要挂这里）

```rust
// task/mod.rs:228-245（节选）
fn register_builtin(&self) {
    self.register_cron("clean_operation_log", "0 3 * * *", "系统任务：日志清理", runner);
    crate::business::storage::cleanup::register_builtin(self);      // :236
    crate::business::notice::cleanup::register_builtin(self);       // :238
    crate::business::notice::sse_hub::register_builtin(self);       // :240
    crate::business::notice::scheduled::register_builtin(self);     // :242
    crate::business::impexp::worker::register_builtin(self);        // :244
    crate::business::impexp::cleanup::register_builtin(self);       // :245
}
```

**两条硬约束**（`task/mod.rs:8-9`、`:32-33` 注释）：
1. 框架调度器在 `start()` 时对注册表做**一次性快照**，启动后再 `register()` 不生效；
2. 业务模块的注册**必须**写在 `register_builtin()` 里（写进 `bootstrap.rs` 会被随后的 `reload()` 清空）。

→ 这就是 §5.4.1 B-2 的由来：**新增定时任务必须动 `task/mod.rs`**，绕不开。

### 1.9 权限种子与迁移的既有范式（本批照抄）

**权限码三方一致性**（`V0065:11-15` 头注释）：① 后端 `#[alun::permission]` 注解 ② `sys_menu.auth_code` ③ 前端 `frontend/src/constants/permissions.ts` —— 三处**逐字一致**，任一不一致 = 接口 403。

**种子写法**（`V0065:44-76`）：
- 固定 UUID（如 `20000000-0000-0000-0000-0000000006xx`）+ `ON CONFLICT (id) DO NOTHING` → 幂等；
- 菜单 `menu_type`：1 目录 / 2 菜单 / 3 按钮（按钮 `is_visible=FALSE`、`path=NULL`）；
- `sys_menu.path` 是**前端路由**，与后端 API 前缀**无关**（`V0065:34-38` 明确说明）；
- 授权：超管全给 + 若「人人可用」则 `WHERE role_code <> 'SUPER_ADMIN'` 全给（`V0045:77-92`、`V0046:113-125`）。

**已用菜单 UUID 段**：`2x0000000-…-0000000004xx`（notice）、`5xx`（impexp）、`6xx`（batch B）。本批用 **`…0701`–`…0731`** 段，避免撞号。

### 1.10 POC 实测汇总（本批 12 项，全部真跑）

| # | POC | 关键数字 | 用到它的结论 |
|---|---|---|---|
| POC-1 | XSS 净化（典型 WYSIWYG） | `script/onclick/javascript:/iframe` 全清；`style/class/target/data:` 全丢；`a` 自动加 `rel` | D4 / D5 |
| POC-2 | 净化耗时（ammonia） | 20 KB **0.635 ms**、64 KB **1.976 ms**（release，200/50 次均值） | D5（读时再净化可接受） |
| POC-3 | 扩展规则 + 严格模式 | 允许 `a[target]` 后 `target` 保留、`rel` 自动补齐；危险片段残留 **0**；纯文本模式只剩文字 | D5 |
| POC-4 | **回滚 vs 假日志**（3 会话并发） | 业务 0 行 / 异步日志 **1 行（假）** / 同事务日志 **0 行** | D11 / §4.12 |
| POC-5 | diff 体积 | 6 字段 **355 B**；8k 字富文本双快照**落盘 51 KB/行**；摘要化 **2.7 KB/行**（19×） | D12 |
| POC-6 | 中文 FTS 行为 | `to_tsvector('simple','系统公告管理') @@ plainto_tsquery('simple','公告')` = **false**；`pg_available_extensions` 无 zhparser/pg_jieba | D8 |
| POC-7 | trgm 检索 | 3 字稀词 **0.238 ms** vs 全表 **131.5 ms**；**2 字反而 153 ms（比全表慢）**；2000 篇 `ILIKE` **4.2–6.1 ms** | D8 |
| POC-8 | 公告可见性分页 | 2000 公告 / 4665 范围行：列表 **0.258 ms**、未读数 **1.515 ms**，走 `idx_sys_announcement_scope_reverse` 反向索引 | D7 |
| POC-9 | 分区 PK 约束 | `PRIMARY KEY (id)` + `PARTITION BY RANGE(created_at)` → **ERROR: unique constraint on partitioned table must include all partitioning columns** | §4.3 |
| POC-10 | DEFAULT 分区陷阱 | default 里堆了 112 161 行后，新建正式分区 → **ERROR: updated partition constraint ... would be violated** | D14 |
| POC-11 | 变更日志写入/查询 | 10 万行写入 **1.88 s**（18.8 µs/行，5 个索引）；单对象历史 **0.065 ms**；`changed_fields && ARRAY[...]` 稀值 **6.9 ms**（GIN 命中）、广值退化为并行 Seq Scan **18.5 ms** | §4.8 |
| POC-12 | 清理代价 | `DELETE` 3.3 万行 **37.9 ms**（留死元组）vs `DROP PARTITION` **1.5 ms**；分区表总占用 100k 行 ≈ **20 MB** | D14 |
| POC-13 | TOC 锚点 `id` | 默认规则**剥离** `id`；`add_generic_attributes(&["id"])` 后 `h2/h3` 的 `id` **全部保留**，仍不含 `script`；公告侧配方（不保留 `id`）行为符合预期 | §3.2 / R13 |

---
## 2. 公告管理（Announcement）

### 2.1 与 `sys_notice` 的边界：内容 vs 投递

**一句话**：`sys_announcement` 回答「**这份公告是什么、给谁看、什么时候有效**」；`sys_notice` 回答「**这份公告已经送到谁的信箱里、他读了没有**」。

| 维度 | `sys_announcement`（本批新建） | `sys_notice`（批次 A 已有） |
|---|---|---|
| 语义 | 内容实体（可编辑、有版本、有附件、可置顶） | 一次投递事件（不可编辑，只增不改） |
| 生命周期 | `DRAFT → PUBLISHED → OFFLINE/EXPIRED`，可再编辑（version+1） | 创建即发送，只被删除/过期清理 |
| 乘客字段 | 附件（多）、置顶、有效期、置顶到期、摘要 | `template_code/template_vars/channel_mask/delivery` |
| 受众记录 | **规则**（`scope_type` + `sys_announcement_scope`） | **物化**（`sys_notice_receiver` 每人一行） |
| 已读 | `sys_announcement_read`（按公告） | `sys_notice_receiver.read_flag`（按投递） |
| 上限 | 无人数上限（规则判定） | **5 万人**（`fanout::MAX_RECEIVERS`） |
| 检索 | 标题/摘要/正文 LIKE 检索 | 不做检索（只有列表/未读） |

**为什么不做成一张表（3 条硬理由）**

1. `sys_notice` 的 `content_format` 只有 `TEXT/HTML/MARKDOWN`（`V0043:47`），**没有附件、置顶、有效期**；这些列若加到 `sys_notice`，则工作流待办、导入完成通知等 4 类非公告通知全部要背负 NULL 列。
2. `sys_notice` 的 `uk_sys_notice_idem` 幂等语义是「一个业务事件只发一次」（`V0049:27-28`）；公告需要「一次内容变更 → 一次新投递」（version 参与幂等键），混在一张表会让幂等键语义漂移。
3. `sys_notice_receiver` 是**每人一行**的物化表，公告用「规则判定」不物化；混表会让 5000 人 × 2000 条公告 = 1000 万行，而公告根本不需要这份明细（POC-8 证明动态判定只要 0.258 ms）。

### 2.2 联动设计：公告发布后是否触发 notice 扇出

**结论：默认触发，且必须做成「可关闭 + 幂等 + 可补偿」，不能做成「强一致」。**

#### 2.2.1 为什么不能强一致（技术约束，非偏好）

`notice::dispatch` 内部自己开事务（`business/notice/mod.rs:473`），**没有**接收外部 `Tx` 的入口：

```rust
// business/notice/mod.rs:473（即时发送分支）
let outcome = db()
    .transaction(|tx| async move {
        let ex = Executor::Tx(&tx);
        ... 落 notice → INSERT..SELECT 扇出 → 回写 receiver_count → 投递记录
    })
    .await
```

要把「公告发布 + 通知扇出」放进一个事务，只有两条路：
- (i) 改 `dispatch` 签名为 `dispatch_with(tx, draft)`（**动批次 A 已上线的公共 API + 8 个调用点**）；
- (ii) 在公告里自己重写一遍扇出逻辑（**违反「对外唯一发送口径」**，`mod.rs:18-20` 明确写了 C 批次只调这一个函数）。

两条都不划算。**选择第三条**：先提交公告，再扇出，失败由补偿任务收敛。

#### 2.2.2 三种联动方案对比

| 方案 | 一致性 | 改动面 | 失败后果 | 可测试性 | 结论 |
|---|---|---|---|---|---|
| (a) 同一事务（改 dispatch 签名） | 强一致 | 改批次 A 公共 API + 8 处调用点 + 事务内 5 万行扇出会拉长公告发布事务 | 无 | 好 | ❌ 不选：扇出 5 万行与「公告落库」耦合，长事务风险高 |
| (b) 提交后同步 dispatch，失败仅报错给前端 | 最终一致（弱） | 仅新代码 | 「发布成功但弹出通知失败」→ 运维不知道 | 一般 | ❌ 不选 |
| **(c) 提交后 dispatch + 幂等键 + 失败标记 + 定时补偿（推荐）** | 最终一致（可收敛） | 仅新代码 + 1 个定时任务 | 7 天内自动补齐 | 好（幂等键可断言） | ✅ **选 (c)** |
| (d) 发布时不发通知，靠用户自己看公告栏 | 无投递 | 无 | 高频公告无人看 | — | ❌ 不选（用户明确要「发布即触达」） |

#### 2.2.3 (c) 的完整执行序列

```
POST /api/business/announcement/{id}/publish   （权限 business:announcement:publish）
│
├─ 事务 T1（一个事务内，全部成功或全部回滚）
│   ├─ SELECT ... FOR UPDATE 校验状态 ∈ {DRAFT, OFFLINE?} 且内容非空
│   ├─ 渲染 + 净化 Markdown → content_html（POC-2：0.6 ms/20 KB）
│   ├─ 计算 summary（缺省取正文前 120 字纯文本）
│   ├─ UPDATE sys_announcement SET status='PUBLISHED', publish_at=?, version=version+1, 
│   │     receiver_count=?, notice_id=NULL, publisher_id/publisher_name
│   ├─ INSERT sys_change_log（action='PUBLISH'，diff 记 status/version/publish_at 变化）
│   └─ COMMIT
│
├─ 提交后（不在 T1 内）：if fanout_enabled
│   ├─ notice::dispatch(NoticeDraft{ notice_type:Circular, content_format:Text,
│   │      content: summary, link_url:/announcement/{id}, biz_type:"announcement",
│   │      biz_id:id, idempotency_key: "announcement:{id}:v{version}",
│   │      target: 由 scope_type/scope 表映射, expire_at, sender_id })
│   ├─ 成功 → UPDATE sys_announcement SET notice_id=?, receiver_count=?
│   └─ 失败（5 万上限 / 模板异常 / DB 抖动）→ 记 tracing::error! + 保持 notice_id=NULL
│
└─ 补偿任务（每 10 分钟，`announcement_dispatch_retry`）
    └─ 扫 status='PUBLISHED' AND notice_id IS NULL AND fanout_enabled AND publish_at > now()-7d
       → 逐条重调 dispatch（幂等键保证不重复投递）
```

**幂等键的三种场景验证**：

| 场景 | 结果 |
|---|---|
| 首次发布 → dispatch 成功 | 落 1 条 `sys_notice`，`duplicated=false` |
| 补偿任务重试 | `uk_sys_notice_idem` 命中 → 返回首次 `notice_id`，`duplicated=true`，**不重复扇出**（`mod.rs:16-18`） |
| 公告编辑后再次发布（version 2） | 幂等键变为 `announcement:{id}:v2` → **新的通知**（这是期望行为：内容变了要重新告知） |

**关闭扇出的场景**：`fanout_enabled=false`（如「仅置顶在公告栏、不打扰全员」），发布只落公告不投递。

### 2.3 表设计

#### 2.3.1 `sys_announcement`（主表，字段见附录 A）

| 关键列 | 设计理由 |
|---|---|
| `content_md` TEXT NOT NULL | **唯一真源**。128 KiB 上限（`octet_length BETWEEN 1 AND 131072`），与 `sys_notice.content` 的 64 KiB 上限区分：公告是长内容，通知是摘要 |
| `content_html` TEXT NOT NULL DEFAULT '' | **渲染 + 净化后的快照**，写时生成。存在的意义：①列表/详情读时不用每次渲染 ②XSS 面收敛在写入路径 ③可被 Grep 审计（DBA 能直接查有没有 `<script>`） |
| `scope_type` VARCHAR(8) | `ALL/ROLE/DEPT/USER`，与 `sys_notice.target_type`（`USER/ROLE/DEPT/ALL`）**同名同义**，便于联动映射，减少转换错 |
| `is_top` + `top_expire_at` | 置顶**必须带到期**，否则历史公告会永远压在新公告上面（`ck_sys_announcement_top`：`is_top=false` 时 `top_expire_at` 必须为 NULL） |
| `status` | `DRAFT/PUBLISHED/OFFLINE/EXPIRED`，四态而非三态：`EXPIRED` 是**系统判定**（超过 `expire_at`），`OFFLINE` 是**人工下线** —— 两者在运营报表上要分开统计 |
| `version` INT | 每次发布 +1；**参与通知幂等键**；也是「同一公告的多次发布」在变更日志里的分组依据 |
| `notice_id` UUID | 联动留痕（关闭扇出时为空）。**不加外键**：`sys_notice` 的清理任务是**硬删**过期通知（`notice/cleanup.rs:93-96` `DELETE FROM sys_notice`），加外键就必须 `ON DELETE SET NULL` 否则清理被阻断，而「公告 → 投递」是弱引用（投递过期后公告仍在），弱引用更贴合语义 |
| `receiver_count` / `read_count` | 冗余计数，供「阅读率」看板；约束 `read_count <= receiver_count` 防倒挂（照抄 `V0043` 的 `ck_sys_notice_counts` 思路） |
| `deleted_at` | 软删（公告要能恢复，且已读回执要保留统计） |

#### 2.3.2 四张子表

```sql
sys_announcement_scope   -- 发布范围明细（ROLE/DEPT/USER × target_id）
sys_announcement_read    -- 已读回执（只记已读）
sys_announcement_attachment -- 附件（引用 sys_file.id）
```

**DEPT 范围的展开时机（关键设计）**

| 方案 | 写入代价 | 读取代价 | 部门搬迁后 | 结论 |
|---|---|---|---|---|
| (a) 存原始 `dept_id`，读取时递归展开子树 | 0 | **每请求一次递归 CTE**（批次 B 实测：20 万行内嵌 CTE 28 ms） | 自动跟随（子树变化后可见性变化） | ❌ 读取路径太贵 |
| **(b) 发布时用递归 CTE 展开成「含下级的部门 ID 集合」并物化（推荐）** | 一次（部门数 ≤ 数百） | `target_id = ANY($user_dept_ids)` —— POC-8 走反向索引 **0.016 ms** | **不跟随**（新部门的人看不到历史公告）→ 由「补偿任务」在部门树变更时重算 | ✅ **选 (b)** |
| (c) 物化到 per-user 接收表 | N 行/公告 | 最快 | 需重扇出 | ❌ 1000 万行级别，且对「公告未读」与「通知未读」造成双份 |

(b) 的「不跟随」是**有意的语义选择**，必须写进 PRD：*公告的受众在发布时刻冻结*。理由：公告是「对当时的这批人说的话」，事后部门调整不应该让新来的同事收到三个月前的通知。若产品要求「跟随」，则改为每日重算 `sys_announcement_scope` 的任务（§7 C6 预留开关 `announcement.scope_refresh_on_dept_change`）。

#### 2.3.3 数据权限：**给结论**

| 问题 | 结论 | 判据 |
|---|---|---|
| 公告**读取侧**（用户看公告列表/详情）是否受数据权限影响？ | **不受**。用 `scope_type` + `sys_announcement_scope` 判定 | 数据权限的语义是「按部门归属过滤业务数据的行」；公告**没有部门归属**（它是发布者写给受众的内容）。若注入：`SELF_ONLY` 角色连全员公告都看不到 → 业务不可用 |
| 公告**管理侧**（公告列表/编辑/下线）是否受数据权限影响？ | **不受**，列表走 `ScopeTarget::unscoped("sys_announcement")` + `scope::paginate_unscoped` | 能进这个页面的人已持有 `business:announcement:list`（元权限），且公告无部门归属列。若要「各部门只管自己的公告」，正确做法是**加发布单位字段并做业务过滤**，不是套数据权限 |
| 帮助文档读取/管理 | 同上，**不受** | 同上 |
| 变更日志查询 | **不受**（有意）。见 D13/§4.7：审计必须完整，注入 scope 会造成审计盲区 | 批次 B 把 `operation_logs`/`audit_logs` 登记进 `SCOPED_TABLES` 是为了「日志列表按操作人过滤」；`sys_change_log` 定位是「审计凭证」，取相反策略并在此写明 |

**判据表（本批新增三表的登记决定）**

| 表 | 是否登记 `SCOPED_TABLES` | 分页入口 | 理由 |
|---|---|---|---|
| `sys_announcement` | ❌ 不登记 | `paginate_unscoped(ScopeTarget::unscoped("sys_announcement"))` | 全局内容，无归属列 |
| `sys_announcement_read` | ❌ 不登记 | 不提供分页（只提供单条已读/未读） | 回执表 |
| `sys_help_doc` | ❌ 不登记 | `paginate_unscoped(...)` | 全局知识库 |
| `sys_change_log` | ❌ **有意不登记** | `paginate_unscoped(...)` + **显式**部门/操作人过滤参数 | 审计完整性优先于行级隔离（§4.7.4 有专门论证） |

> 这与 `/docs/后端二开指南.md` 的纪律并不冲突：该纪律要求「**新增列表接口**必须走 `scope::paginate*`」，本批三张表都走了 `paginate_unscoped`（这是合法入口，见 `scope.rs:651` 的守卫设计）。

### 2.4 内容形态：富文本 vs Markdown（含 XSS 具体方案）

#### 2.4.1 对比表

| 维度 | (a) 富文本 HTML 直存 | (b) Markdown 存源 + 服务端渲染 | (c) 富文本编辑器 + 自建白名单净化 |
|---|---|---|---|
| 现成能力 | 无编辑器、无净化白名单（ammonia 默认规则） | **后端已有 ammonia**（`Cargo.toml` 的 `xss` feature）；缺渲染器（新增 1 个 crate） | 需同时做编辑器 + 白名单 + 迁移，工作量最大 |
| 与现有净化器兼容 | ❌ 实测表现力损失严重（POC-1：style/class/target/data: 全丢） | ✅ Markdown 渲染输出的语义 HTML **原样通过**（POC-3） | ⚠️ 要放宽 `style`/`class` → 直接扩大 XSS 面（CSS 注入、`expression()`、UI 伪装钓鱼） |
| XSS 面 | 大（`<img onerror>`、`<svg>`、`data:`、`<iframe>`） | **小**（渲染器只产出固定标签集；`<script>` 在 Markdown 里是代码块） | 大（同 (a)） |
| 附件/图片 | base64 内嵌（会被净化器删掉，POC-1） | `![](/api/.../download/{id})` —— 天然走 storage | 同 (a) |
| 版本 diff | HTML 难 diff（一次改样式全变） | **Markdown 文本可 diff**（与 `sys_change_log` 的摘要化天然配合） | 同 (a) |
| 编辑体验 | 最像 Word | 需要会 Markdown（可用「工具栏 + 实时预览」缓解） | 同 (a) |
| 前端依赖 | 富文本编辑器（wangEditor/Tiptap） | markdown-it（或纯服务端渲染，前端零依赖） | 编辑器 + DOMPurify |
| 团队成本 | 编辑器接入 + 白名单调参持续投入 | 一次性接入，长期成本最低 | 最高 |
| **结论** | ❌ | ✅ **推荐** | ❌ |

#### 2.4.2 为什么「服务端渲染」而不是「前端渲染 Markdown」

| 方案 | XSS 面 | 摘要/TOC/检索 | 一致性 | 结论 |
|---|---|---|---|---|
| 前端渲染（markdown-it + DOMPurify） | 前端净化一旦漏配就全线穿透 | 需要前端生成摘要再回传，或后端再解析一遍 | 多端（Web/小程序/邮件）要各写一遍 | ❌ |
| **服务端渲染 + 净化，前端只贴 HTML（推荐）** | 收敛到 1 个 crate + 1 个函数 | 后端一次拿到 HTML/纯文本/标题树，摘要、TOC、检索都用它 | 全端一致 | ✅ |
| 前后端都渲染 | 两套渲染器两套 bug | — | 可能不一致（同一 Markdown 两边渲染不同） | ❌ |

#### 2.4.3 XSS 四道闸（具体到函数与参数）

| 闸 | 位置 | 做什么 | 失败时 |
|---|---|---|---|
| **闸 1 · 渲染即净化** | `business/announcement/render.rs::render_markdown(md)` | `pulldown-cmark` 渲染（关闭 `ENABLE_HTML`：**不允许 Markdown 里内嵌原始 HTML**）→ `xss::sanitize_html(&html)` | 渲染失败 → `422 + 62001`（内容格式错误），**不落库** |
| **闸 2 · 入库再净化** | 同上，落库前对 `content_html` 再跑一次 `sanitize_html` | 幂等（ammonia 对已净化内容输出不变，POC-3 已验证幂等性） | 同上 |
| **闸 3 · 出参前再净化** | `handler::detail()` 返回前 `sanitize_html(&row.content_html)` | 防御纵深：即使有人手工改库塞了 `<script>`，出参仍然干净。成本 0.635 ms/20 KB（POC-2），只作用于**详情接口**，不进列表 | 净化抛错不可能（纯函数）；返回空串兜底 |
| **闸 4 · 前端兜底** | 前端 `v-html` 前 `DOMPurify.sanitize(html, {ALLOWED_TAGS: [...]})` | 前端也断一次（防止「后端漏了某个接口」） | 净化后空 → 显示「内容加载失败」 |

**加固项（必须一起上）**

| 项 | 配置 | 防的是什么 |
|---|---|---|
| CSP 响应头 | `default-src 'self'; img-src 'self' data: blob:; script-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'` | 反射型 XSS、第三方脚本注入、点击劫持 |
| `X-Content-Type-Options: nosniff` | — | MIME 混淆 |
| 附件下载强制 `Content-Disposition: attachment` | 代理下载接口设置 | **HTML 附件被当页面渲染**导致的存储型 XSS（最容易被忽略的一条） |
| 图片只允许 storage URL | 渲染后校验 `<img src>` 必须以 `/api/business/storage/` 或 `/api/business/announcement/` 开头，否则整段替换为 `[图片已移除]` | `data:`/外链追踪像素/`javascript:`（净化器已挡 `javascript:`，但外链追踪像素不在其职责内） |
| 链接协议白名单 | 渲染后校验 `<a href>` scheme ∈ `{http, https, mailto}`，其余改为纯文本 | `javascript:`、`file:`、自定义 scheme 唤起 |
| `target="_blank"` | 允许（需 `add_tag_attributes("a", &["target"])`），**但不要**同时声明 `rel`（POC-1 实测 panic），`rel` 由 ammonia 的 `link_rel` 自动补 | `window.opener` 劫持（`noopener noreferrer` 已由 `:488` 注入） |

**净化器的正确配方（POC-3 已验证可运行、无 panic）**

```rust
// 建议实现（business/announcement/render.rs）
fn sanitize(content_html: &str) -> String {
    let mut builder = ammonia::Builder::default();
    // ① 只补这一个属性：新窗口打开。
    //    ⚠️ 千万不要写 &["target", "rel"]：ammonia 4.2.4 会 panic
    //    （assert tag_attributes["a"]["rel"].is_none()，lib.rs:1910，POC-1 真跑）
    builder.add_tag_attributes("a", &["target"]);
    // ② rel 保持默认 link_rel = Some("noopener noreferrer")，由 build 侧自动注入
    // ③ 不放宽 style / class / data: / iframe / svg / form
    builder.clean(content_html).to_string()
}
```

**Markdown 渲染的固定参数（必须写进实现）**

| 参数 | 取值 | 理由 |
|---|---|---|
| `Options::ENABLE_HTML` | **false** | 禁止 Markdown 内嵌原始 HTML（否则等于开了富文本入口，闸 1 白做） |
| `Options::ENABLE_TABLES` | true | 公告需要表格（公告常见「值班表」「时间表」） |
| `Options::ENABLE_STRIKETHROUGH` / `TASKLISTS` | true | 常用排版 |
| 标题层级 | 强制 `#` → `<h2>`（偏移 +1） | 页面已有 `<h1>` 标题，Markdown 的 `#` 会产出第二个 h1，破坏无障碍大纲 |
| 代码块 | 不接语法高亮（服务端只输出 `<pre><code class="language-x">`） | 避免引入 highlight.js 的服务端依赖；前端按需高亮 |
| 输出前 | 去掉 `id`/`class` 属性（除 `code` 的 `language-*`） | ammonia 默认已剥离，但要**显式断言**（防止将来放宽白名单时误开） |

### 2.5 附件：复用 `business/storage`

#### 2.5.1 接入方式（三条路径的选择）

| 方案 | 上传 | 下载鉴权 | 结论 |
|---|---|---|---|
| (a) 前端直接调 `POST /business/storage/upload`，把 `file_id` 存进 `sys_announcement_attachment` | 复用现成接口（秒传/直传/策略校验全都有） | ❌ 直接调 `GET /business/storage/download/{id}` —— 已授予所有角色（`V0046:113-125`），**任何人可下载任意文件** | ⚠️ 只采纳「上传」部分 |
| **(b) 上传复用 storage + 下载走公告代理接口（推荐）** | 同 (a) | ✅ `GET /api/business/announcement/{id}/attachments/{file_id}/download` → 先校验「当前用户可见该公告」且「该 file_id 属于该公告」，再 `global_router().pick()` 取后端流式输出 | ✅ **选 (b)** |
| (c) 公告自己实现上传 | 重复实现策略校验/秒传/SigV4 | — | ❌ |

#### 2.5.2 上传-绑定-回收时序

```
① 前端：POST /api/business/storage/upload?category=announcement   （已有接口，权限 business:storage:upload）
   → 返回 StorageFileRes{ id, original_name, file_size, mime_type, url, ... }
② 前端：保存公告时把 [{file_id, sort_order}] 放进 POST/PUT /business/announcement 的 attachments 数组
③ 后端（同一事务）：
   · 校验每个 file_id 在 sys_file 存在且 status=1
   · DELETE 旧绑定（编辑场景）→ INSERT sys_announcement_attachment（uk(announcement_id,file_id) 防重复）
④ 孤儿回收：storage/cleanup.rs 已有「每日 04:30 软删文件的对象回收」（task/mod.rs:236）；
   公告侧的「上传了但从未绑定」的文件由 `sys_file.status` 老化策略处理（见 §5.7 风险 R7）
```

**`category` 约定**：`announcement`（需在 `sys_storage_policy` 种一条策略：允许 `jpg/png/pdf/docx/xlsx/zip`、单文件 ≤ 50 MB、禁止可执行扩展名）。**这是种子数据，不是代码改动**（`V0086`）。

#### 2.5.3 代理下载接口的鉴权（防越权，具体到判定顺序）

```rust
// GET /api/business/announcement/{id}/attachments/{file_id}/download
// 权限码：business:announcement:read（全员都有）
1. 取公告：SELECT ... WHERE id=? AND deleted_at IS NULL            → 不存在 → 404 + 62002
2. 取绑定：SELECT ... WHERE announcement_id=? AND file_id=?        → 不存在 → 404 + 62003（防「拿别的公告的附件 id」）
3. 可见性：scope_type='ALL' 或 EXISTS(scope 行命中当前用户角色/部门/用户)  → 否 → 403 + 62004
4. 状态：status='PUBLISHED' 且未过期                                → 否 → 403 + 62005
5. 取 sys_file 记录 → global_router().pick(Some(&policy)) → 流式输出
   Content-Type: mime_type; Content-Disposition: attachment; filename*=UTF-8''...
   （强制 attachment，绝不让浏览器按 HTML 渲染 —— 防存储型 XSS）
```

**为什么不给「带签名的临时 URL」**：预签名 URL 的有效期内**任何人拿到 URL 都能下载**（无身份），而公告的可见性可能变化（下线/过期）。代理接口每次请求都重新判定，语义正确。代价：文件流量过应用服务器（50 MB × 并发）。→ 加 `Cache-Control: private, max-age=300` + 限流（§5.7 R8）。

### 2.6 有效期与自动下线

**状态机**

```
                 publish (人工)                expire_at 到点 (定时)
   DRAFT ──────────────────▶ PUBLISHED ─────────────────────────▶ EXPIRED
     ▲                          │                                   │
     └──── 撤回编辑 ────────────┤                                   │
                                │ offline (人工，可填原因)            │
                                ▼                                   │
                             OFFLINE ◀────── 恢复上线 ───────────────┘
                                │
                                └─ 再次发布 → PUBLISHED（version+1，重新投递）
```

**三个定时任务（`Scheduler::register_builtin` 注册，`task/mod.rs:228`）**

| 任务 | cron | 做什么 | 幂等性 |
|---|---|---|---|
| `announcement_auto_expire` | `0 */10 * * * *`（每 10 分钟） | `UPDATE sys_announcement SET status='EXPIRED' WHERE status='PUBLISHED' AND expire_at IS NOT NULL AND expire_at <= now()` | 幂等（条件更新）；走 `idx_sys_announcement_expire`（部分索引，只扫有到期的行） |
| `announcement_top_expire` | `0 2 * * * *`（每 2 分钟） | `UPDATE ... SET is_top=FALSE, top_expire_at=NULL WHERE is_top AND top_expire_at <= now()` | 幂等；走 `idx_sys_announcement_top_expire` |
| `announcement_dispatch_retry` | `0 */10 * * * *`（每 10 分钟） | 扫 `status='PUBLISHED' AND notice_id IS NULL AND fanout_enabled AND publish_at > now()-7d` → 重调 `dispatch` | **靠 `uk_sys_notice_idem` 幂等**（重试不会重复投递） |

**为什么「过期」要落成 `status` 而不是查询时算**：查询时算（`easy: WHERE expire_at > now()`）每次都要带时间条件，且运营在列表里看不到「已过期」这一态；落成 `status` 后：①列表可筛选 ②看板可统计 ③下线后可被补偿任务跳过。

**注意**：查询侧**仍然**要带 `(expire_at IS NULL OR expire_at > now())` 双保险 —— 定时任务最多有 10 分钟延迟（POC-8 的查询已包含该条件）。

### 2.7 已读/未读：要不要？怎么分离？

**结论：要；但只记「已读」，不预写「未读」。**

| 方案 | 存储量（2000 公告 × 5000 人） | 未读查询 | 结论 |
|---|---|---|---|
| (a) 预写未读回执（`sys_notice_receiver` 模式） | **1000 万行**（≈1.5 GB） | 极快（部分索引） | ❌ 公告不需要这么强 |
| **(b) 只记已读 + 动态判定（推荐）** | 已读行数（命中率 × 用户数），实测未读数 **1.515 ms**（POC-8） | 足够快 | ✅ |
| (c) 不做到读，只做「已读人数」计数 | 0 | — | ❌ 用户侧无法标记 |

**API 语义**

| 接口 | 语义 |
|---|---|
| `GET /api/business/announcement/mine/page` | 我的可见公告（含 `read` 布尔） |
| `GET /api/business/announcement/mine/unread-count` | `{ announcement_unread: 12 }`（**与通知未读分开的字段名**） |
| `POST /api/business/announcement/{id}/read` | 幂等（`uk_sys_announcement_read` 冲突 → 视为已读），并 `read_count = read_count + 1`（仅首次） |
| `POST /api/business/announcement/read-all` | 批量标记（`INSERT ... SELECT ... ON CONFLICT DO NOTHING`，只对可见集合） |

**未读如何分离展示（前端契约）**

顶栏铃铛改为**两个入口**：

| 入口 | 数据源 | 角标来源 |
|---|---|---|
| 「通知」 | `sys_notice_receiver` 未读 | `GET /api/business/notice/mine/unread-count`（已有） |
| 「公告」 | `sys_announcement_read` 差集 | `GET /api/business/announcement/mine/unread-count`（本批新增） |

另给一个聚合接口 `GET /api/business/announcement/mine/badge` → `{ notice_unread, announcement_unread, total }`，避免前端两个请求串行。

> **为什么不合并计数**：用户的心理模型是「私信」与「公告」两件事。合并后「公告已读」会掩盖「通知未读」，运营无法判断是哪类触达失效。

### 2.8 API 契约（公告）

> 约定：路径不带 `/api`（注解里写业务段，框架补 `/api`，见 `lib.rs:26-28`）；分页统一 `page`/`page_size`；响应统一 `Res<T>` / `PageData<Vec<T>>`；时间为 RFC3339 UTC。

#### 2.8.1 用户侧（全员可用，权限码 `business:announcement:read`）

| 方法 | 路径 | 说明 | 关键入参 | 出参 |
|---|---|---|---|---|
| GET | `/business/announcement/mine/page` | 我的可见公告分页（置顶优先 + 发布时间倒序） | `page,page_size,keyword?,only_unread?` | `PageData<AnnouncementCardRes[]>` |
| GET | `/business/announcement/mine/{id}` | 公告详情（**读时再次净化**，同时 `read` 上报可选） | — | `AnnouncementDetailRes{content_html, attachments[], read}` |
| POST | `/business/announcement/mine/{id}/read` | 标记已读（幂等） | — | `Res<{read_count}>` |
| POST | `/business/announcement/mine/read-all` | 全部已读（只影响可见集合） | — | `Res<{affected}>` |
| GET | `/business/announcement/mine/unread-count` | 公告未读数 | — | `Res<{announcement_unread}>` |
| GET | `/business/announcement/mine/badge` | 聚合角标（通知 + 公告） | — | `Res<{notice_unread,announcement_unread,total}>` |
| GET | `/business/announcement/{id}/attachments/{file_id}/download` | 附件下载（可见性校验 + 强制 attachment） | — | 二进制流 |

`AnnouncementCardRes`：`id, ann_no, title, summary, level, is_top, publish_at, read, attachment_count, publisher_name`

#### 2.8.2 管理侧

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/business/announcement/page` | `business:announcement:list` | 管理列表（`status/scope_type/level/keyword/时间窗`） |
| GET | `/business/announcement/{id}` | `business:announcement:list` | 管理详情（含 `content_md` 原文 + `scope` 明细） |
| POST | `/business/announcement` | `business:announcement:create` | 新建（`DRAFT`） |
| PUT | `/business/announcement/{id}` | `business:announcement:update` | 编辑（仅 `DRAFT`/`OFFLINE`；`PUBLISHED` 编辑后需重新发布） |
| DELETE | `/business/announcement/{id}` | `business:announcement:delete` | 软删 |
| POST | `/business/announcement/{id}/publish` | `business:announcement:publish` | 发布（`version+1` + 扇出 + 补偿兜底） |
| POST | `/business/announcement/{id}/offline` | `business:announcement:offline` | 下线（必填 `reason`） |
| POST | `/business/announcement/{id}/top` | `business:announcement:update` | 置顶/取消（置顶必须带 `top_expire_at`） |
| POST | `/business/announcement/preview` | `business:announcement:create` | **编辑器实时预览**：入参 `{content_md}`，出参 `{content_html, summary, warnings[]}`；与发布走**同一条**渲染+净化管线（前端不自行渲染，避免两套渲染器不一致 —— §2.4.2） |
| GET | `/business/announcement/{id}/read-stats` | `business:announcement:list` | 阅读统计（`receiver_count/read_count/阅读率`） |

#### 2.8.3 请求/响应示例（新建）

```jsonc
// POST /api/business/announcement
{
  "title": "关于 11 月 1 日系统维护的通知",
  "summary": "11 月 1 日 22:00–24:00 停机维护，请提前保存工作。",
  "content_md": "## 维护时间\n\n- 2026-11-01 22:00 ~ 24:00\n\n| 系统 | 影响 |\n|---|---|\n| 云枢 | 不可用 |\n",
  "level": "IMPORTANT",
  "scope_type": "ROLE",
  "scope_ids": ["bbbbbbbb-0000-0000-0000-000000000001"],
  "is_top": true,
  "top_expire_at": "2026-11-02T00:00:00Z",
  "expire_at": "2026-11-08T00:00:00Z",
  "fanout_enabled": true,
  "attachments": [{"file_id": "9f1c...", "sort_order": 0}]
}
// 201 → { "code":0, "data": { "id":"...", "ann_no":"AN202610290001", "status":"DRAFT", "version":1 } }
```

**校验规则（后端强校验，不依赖前端）**

| 规则 | 错误码 |
|---|---|
| `title` 1–200 字符（去空白后） | `400 + 62001` |
| `content_md` 1–128 KiB | `400 + 62001` |
| `scope_type != 'ALL'` 时 `scope_ids` 非空且 ≤ 500 个 | `400 + 62006` |
| `is_top=true` 必须有 `top_expire_at` 且 > now | `400 + 62007` |
| `expire_at`（若给）> `publish_at`（若给） | `400 + 62008` |
| `PUBLISHED` 状态下不允许直接改正文（必须先下线或走「编辑→重新发布」） | `409 + 62009` |
| 附件 `file_id` 不存在或已删 | `400 + 62003` |

### 2.9 关键查询与索引验证（POC-8 实测）

```sql
-- 我的可见公告（置顶优先，排除已读）
WITH visible AS (
  SELECT a.id, a.is_top, a.publish_at FROM sys_announcement a
  WHERE a.deleted_at IS NULL AND a.status='PUBLISHED'
    AND (a.expire_at IS NULL OR a.expire_at > now())
    AND ( a.scope_type='ALL'
       OR EXISTS (SELECT 1 FROM sys_announcement_scope s
                  WHERE s.announcement_id=a.id AND s.scope_type='ROLE' AND s.target_id = ANY($1))
       OR EXISTS (SELECT 1 FROM sys_announcement_scope s
                  WHERE s.announcement_id=a.id AND s.scope_type='DEPT' AND s.target_id = ANY($2))
       OR EXISTS (SELECT 1 FROM sys_announcement_scope s
                  WHERE s.announcement_id=a.id AND s.scope_type='USER' AND s.target_id = $3) )
)
SELECT v.* FROM visible v
LEFT JOIN sys_announcement_read r ON r.announcement_id=v.id AND r.user_id=$3
WHERE r.id IS NULL
ORDER BY v.is_top DESC, v.publish_at DESC
LIMIT $4 OFFSET $5;
```

**实测执行计划（2000 公告 / 4665 范围行）**

```
Limit (actual time=0.080..0.233 rows=20)
  -> Nested Loop Left Join
       -> Index Scan using idx_sys_announcement_list on sys_announcement a
            Index Cond: status='PUBLISHED'                        ← 走部分顺序索引，LIMIT 提前停
            SubPlan: Bitmap Index Scan on idx_sys_announcement_scope_reverse
                     Index Cond: scope_type='ROLE' AND target_id = ANY(...)   (219 rows)
                     Bitmap Index Scan ... scope_type='DEPT' AND target_id = ANY(...)  (108 rows)
                     Bitmap Index Scan ... scope_type='USER' AND target_id = ...       (108 rows)
       -> Index Scan using idx_sys_announcement_read_user on sys_announcement_read r  (20 loops)
Execution Time: 0.258 ms
```

未读数（同一套 WHERE + `NOT EXISTS` 已读）**1.515 ms**。

---
## 3. 帮助文档（Help / Knowledge Base）

### 3.1 表设计：分类树 + 文档 + 版本快照

**三张表的职责**

| 表 | 职责 | 关键决策 |
|---|---|---|
| `sys_help_category` | 分类树（`parent_id` 自引用） | **用自引用 + 递归 CTE，不用 `path` 物化路径**（项目已有教训：`system/dept` 的 `dept_path LIKE` 搬迁不重算，见 `system/dept/handler.rs` 已知限制） |
| `sys_help_doc` | 文档主体（Markdown 源 + HTML 快照） | 与公告同构（`content_md`/`content_html`），复用同一套渲染+净化管线 |
| `sys_help_doc_version` | **发布时的全文快照** | 只保留最近 20 版；用于「一键回滚」 |

**分类表：树形要不要？——要，但只允许 3 层**

| 方案 | 理由 | 结论 |
|---|---|---|
| 单层分类（`category` 字符串） | 实现最简 | ❌ 帮助中心的目录会很快膨胀成一长条 |
| **自引用树（`parent_id`）+ 应用层限深 3 层** | 递归 CTE 取子树是一次性查询（分类数量 ≤ 数百），代价可忽略；限深避免「无限嵌套 → 前端树组件爆炸」 | ✅ |
| 物化路径（`path`） | 查询快 | ❌ 项目已有 `dept_path` 搬迁不重算的坑，不重蹈 |
| 邻接表 + 闭包表 | 最灵活 | ❌ 过度设计（分类 < 500 条） |

**限深 3 层**：`创建/移动分类时用递归 CTE 计算 depth，> 3 → 422 + 62010`。数据库侧无法用 CHECK 表达「深度 ≤ 3」，由 model 层保证 + 单测断言。

**版本快照要不要？**

| 方案 | 存储 | 回滚能力 | 结论 |
|---|---|---|---|
| 无版本表，只靠 `sys_change_log` 的 diff | 最省 | 只能看「改了哪几段」，无法一键还原（diff 的大字段被摘要化为 hash + preview） | ❌ 不够 |
| **`sys_help_doc_version` 全文快照（保留 20 版）** | 单篇 20 KB × 20 = 400 KB/篇；1000 篇 = 400 MB | 一键回滚（`UPDATE doc SET content_md=旧版`） | ✅ |
| 全量保留所有版本 | — | — | ❌ 膨胀不可控 |

**关键列说明**

| 列 | 类型 | 说明 |
|---|---|---|
| `sys_help_doc.slug` | `VARCHAR(120) UNIQUE WHERE deleted_at IS NULL` | 对外稳定 URL（`/help/{slug}`），格式 `^[a-z0-9][a-z0-9-]{1,119}$`（DB CHECK 已实跑拒绝 `Bad Slug`） |
| `keywords` | `VARCHAR(255)` | 逗号分隔的关键词（运营手填），参与检索加权（不建索引） |
| `status` | `DRAFT/PUBLISHED/OFFLINE` | 与公告一致（只读端只看 `PUBLISHED`） |
| `view_count` | `INT` | 阅读量（异步累加，减少热点行锁，见 §5.7 R9） |
| `version` | `INT` | 每次发布 +1，与 `sys_help_doc_version.version` 对齐 |

### 3.2 内容形态：Markdown + 站内渲染 + 目录（TOC）生成

**内容形态结论与公告一致（Markdown，服务端渲染 + 净化），但 TOC 生成方式不同：**

| TOC 方案 | 实现 | 优点 | 缺点 | 结论 |
|---|---|---|---|---|
| 前端从 DOM 提取 `h2/h3` | `querySelectorAll` | 零后端改动 | 服务端渲染的 HTML 已剥离 `id`，锚点要前端临时生成；SSR/SEO 不可用；滚动高亮要额外写 | ⚠️ 备选 |
| **服务端渲染时生成 `id` 并输出 TOC 树（推荐）** | 渲染时给 `h2/h3` 注入 `id="h-{序号}-{slug化文本}"`，同时产出 `[{level,text,id,children}]` | 一次渲染三份产物（HTML / 纯文本摘要 / TOC），前端零逻辑；锚点稳定（同输入同输出） | 渲染器要自己接管 heading 事件 | ✅ |
| 单独存 `toc` JSONB 列 | 存库 | 读取零成本 | 内容变更要同步维护，容易不一致 | ❌ |

**实现要点（写进实现约束）**

1. `pulldown-cmark` 的 `Heading` 事件里取文本 → 生成 id：`h-{level}-{index}`（**不用文本 slug 做主键部分**，因为中文 slug 化不稳定；顺序号保证唯一稳定）。
2. `id` 是 ammonia 的 `generic_attributes` **之外**的属性 → **默认会被剥离**！这与公告的「去掉 id」相反，所以帮助文档的净化配方要多一步（**POC-13 已实测确认**：默认输出 `<h2>维护时间</h2>`，加白后 `<h2 id="h-2-1">维护时间</h2>`）：

```rust
// business/help/render.rs —— 与公告的差异只有一行
let mut b = ammonia::Builder::default();
b.add_tag_attributes("a", &["target"]);
b.add_generic_attributes(&["id"]);       // ← 保留 heading 的锚点 id（供 TOC 跳转）
// 仍然不放宽 style / class / data: / iframe
```

> ⚠️ 这是一个**必须实测的坑**：`id` 属于 `generic_attributes`（默认只有 `lang`、`title`，`ammonia-4.2.4/src/lib.rs:390`），不显式加白会被静默剥离，TOC 跳转全线失效。**验收断言**：`assert!(sanitized.contains("<h2 id=\"h-2-1\">"))`。
> 安全代价：允许 `id` 只带来「DOM clobbering」的理论风险（`id="location"` 之类，需要页面 JS 用 `window[name]` 访问），本项目前端不使用该模式，风险可接受；同时 `id` 值必须匹配 `^h-[0-9]+-[0-9]+$`（渲染器自己生成，不透传用户输入）。

3. **站内渲染的性能**：文档详情页读时渲染（Markdown 20 KB → HTML + 净化）≈ 0.6–1.5 ms（POC-2 的净化部分 0.635 ms + 渲染）。**但详情页同时返回 `content_html` 快照**（写时已生成），读时只做「闸 3 再净化」，不重新渲染 → 稳定 0.6 ms。

### 3.3 全文检索：tsvector vs LIKE（给结论）

#### 3.3.1 三种方案对比（全部实测）

| 方案 | 中文子串检索 | 实测性能（5 万文档 / 27 MB） | 依赖 | 维护成本 | 结论 |
|---|---|---|---|---|---|
| (a) `tsvector` + GIN | ❌ **不可用**：默认分词器把「系统公告管理」当一个词，搜「公告」命中 **false**（POC-6） | 若分词正确则 <1 ms | 需 `zhparser`/`pg_jieba`——**本机 PG 18.6 无此扩展**（`pg_available_extensions` 只有 `pg_trgm`/`btree_gin`/`pgcrypto`） | 高（装扩展 + 自定义词典 + 中文停用词） | ❌ **不选** |
| (b) `ILIKE '%kw%'` | ✅ 天然支持 | **2000 篇：4.2–6.1 ms**（真实规模）；5 万篇：131–135 ms（不可接受） | 无 | 零 | ✅ **本批选它** |
| (c) `ILIKE` + `pg_trgm` GIN | ✅ | 5 万篇：稀词 3 字 **0.238 ms**；**2 字 153 ms（比全表还慢）**；常见词 108 ms（退化为 Seq Scan） | `pg_trgm`（本机可用） | 中（索引体积 = 表体积的 ~50%，写入放大） | ⚠️ **条件启用**（文档数 > 5000 再加） |

#### 3.3.2 结论与触发条件

**本批：`ILIKE`，不建 trgm 索引，不上 tsvector。**

| 量级 | 决策 | 依据 |
|---|---|---|
| 文档数 ≤ 5000（本批预估 200–1000 篇） | `ILIKE` + `status/published_at` 索引 | POC-7：2000 篇全表扫描 **4.2–6.1 ms**，远低于 200 ms 预算 |
| 文档数 > 5000 且 P95 > 50 ms | 追加 `CREATE INDEX ... USING gin (title gin_trgm_ops)` + `content` 的 trgm（DDL 已备，见 §3.7） | POC-7：3 字检索 585× 提速 |
| 需要「搜索建议/相关度排序/多语言分词」 | 才考虑 zhparser 或外部检索引擎（MeiliSearch/OpenSearch）——**本批不做** | 引入外来组件的运维成本远超收益 |

**必须做的检索细节（否则会被用户投诉「搜不到」）**

| 细节 | 做法 |
|---|---|
| `%`/`_`/`\` 转义 | 前端传入的关键词必须转义再拼 `ILIKE`：`keyword.replace('\\',"\\\\").replace('%',"\\%").replace('_',"\\_")`，SQL 里写 `ILIKE ? ESCAPE '\'` ——**不转义时用户搜 `%` 会全表命中** |
| 大小写 | `ILIKE` 已不敏感（中文无关，英文有用） |
| 多关键词 | 按空格切分，**AND** 连接（每词一个 `(title ILIKE ? OR content_md ILIKE ?)`），≤ 5 个词 |
| 只搜已发布 | 普通用户强制 `status='PUBLISHED' AND deleted_at IS NULL`；管理员可选 `status` |
| 命中片段 | 列表返回 `snippet`：在 `content_md` 里定位首个命中位置，取前后 40 字（**纯文本**，不返回 HTML，避免二次 XSS 面） |
| 排序 | `ORDER BY published_at DESC, sort_order ASC`（不做相关度排序，避免伪科学） |

**为什么不搜 `content_html`**：HTML 里含标签文本，搜「h2」会命中所有带标题的文档；且 `content_html` 是渲染产物（体积大 1.5–2×）。搜 `content_md` 即可。

### 3.4 权限：谁可读、谁可编辑、要不要「按角色可见」

| 问题 | 结论 | 理由 |
|---|---|---|
| 所有登录用户可读？ | ✅ **是**。`system:help:read` 这个码**授予所有角色**（照 `V0045:77-92` 的 notice:mine 范式） | 帮助文档是「产品自带说明书」，不是业务数据；若按角色开放，新员工入职第一天看不到任何文档 |
| 管理员可编辑？ | ✅ 只有持 `system:help:doc:create/update/delete/publish`（授 SUPER_ADMIN + 内容运营角色） | 编辑权限是元权限 |
| 是否需要「按角色可见」？ | ❌ **本批不做**（列为 §8 未决项 3） | ① 无业务需求证据 ② 一旦引入，`sys_help_doc` 就需要范围表 + 可见性判定 + 缓存，复杂度接近公告 ③ 真要做，正确形态是「按**菜单/模块**可见」（文档天然属于某个功能模块），不是按角色 |
| 未发布文档谁能看？ | 只有持 `system:help:doc:list` 的人（预览接口） | 普通用户 `read` 接口强制 `status='PUBLISHED'` |

**权限码 → 接口映射见 §5.1；所有 `#[alun::permission]` 必须与种子 `auth_code` 逐字一致（`V0065:11-15` 的纪律）。**

### 3.5 与公告的关系：分开还是合并？（给结论）

**结论：分开（两张独立主表），但共用一套「内容渲染 + 净化 + 版本」基础设施。**

| 判据 | 公告 | 帮助文档 | 合并的后果 |
|---|---|---|---|
| 受众 | 有时效的通知对象（角色/部门/用户） | 全体登录用户（无受众概念） | 帮助文档要凭空长出一个 `scope_type` 列并永远存 `ALL` |
| 生命周期 | `DRAFT→PUBLISHED→EXPIRED/OFFLINE`，**必然过期** | `DRAFT→PUBLISHED→OFFLINE`，**永不过期** | `expire_at` 对帮助文档永远 NULL，索引选择性差 |
| 触达 | 需要扇出到 `sys_notice` | **绝不能**扇出（改一条 FAQ 就给全员发通知是事故） | 合并后必须加 `kind` 分支判断，每个查询都要 `WHERE kind=?` |
| 已读 | 需要 per-user 已读回执 | 不需要（只有 `view_count`） | `sys_announcement_read` 要为帮助文档维护空表 |
| 检索 | 标题/摘要 LIKE，量小 | 正文 LIKE，需要 snippet | 两种检索策略挤在一个 handler 里 |
| 权限码 | `business:announcement:*` | `system:help:*` | 任务书已把两套命名空间分开 → 暗示两类资源不同 |
| 表膨胀 | 公告几千条 | 文档几千条，但**正文 20 KB 级** | 单表混存后 `content_md` 的平均行宽被文档拉高，公告列表扫描变慢 |

**共用部分（避免重复造）**

| 共用能力 | 落地形式 |
|---|---|
| Markdown 渲染 + TOC | `business/shared/content.rs`（**新文件**，纯函数，两个模块都调） |
| XSS 净化配方 | 同上（`sanitize_with(mods)`），公告不保留 `id`、帮助保留 `id` |
| 版本快照模式 | 两张表各自的 `*_version` 表，但保存函数共用 |
| 附件 | 帮助文档**本批不支持附件**（用 Markdown 图片指向 storage 即可），减少一套绑定表 |

### 3.6 API 契约（帮助文档）

#### 3.6.1 用户侧（`system:help:read`，全员）

| 方法 | 路径 | 说明 | 出参 |
|---|---|---|---|
| GET | `/system/help/category/tree` | 分类树（只返回有已发布文档的分支） | `Res<HelpCategoryNode[]>`（含 `doc_count`） |
| GET | `/system/help/doc/page` | 文档列表（`category_id`、`keyword`、`page`） | `PageData<HelpDocCardRes[]>` |
| GET | `/system/help/doc/{slug}` | 按 slug 读文档（**闸 3 再净化** + TOC） | `HelpDocDetailRes{content_html, toc[], version, published_at}` |
| POST | `/system/help/doc/{id}/view` | 阅读量 +1（异步，去重按 IP+UA 5 分钟） | `Res<()>` |
| GET | `/system/help/doc/search` | 检索（返回 snippet） | `PageData<HelpDocHitRes[]>` |

#### 3.6.2 管理侧

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/system/help/category/page` | `system:help:category:list` | 分类管理列表（含停用） |
| POST | `/system/help/category` | `system:help:category:create` | 新建（校验深度 ≤ 3） |
| PUT | `/system/help/category/{id}` | `system:help:category:update` | 改名/移动（移动需校验深度） |
| DELETE | `/system/help/category/{id}` | `system:help:category:delete` | 删除（**有文档时 409 + 62011**，不做级联删） |
| GET | `/system/help/doc/page` | `system:help:doc:list` | 管理列表（含 `DRAFT`） |
| GET | `/system/help/doc/{id}/edit` | `system:help:doc:list` | 编辑回显（`content_md` 原文 + 版本列表） |
| POST | `/system/help/doc` | `system:help:doc:create` | 新建草稿 |
| PUT | `/system/help/doc/{id}` | `system:help:doc:update` | 编辑（`PUBLISHED` 编辑自动进 `DRAFT` 并 `version+1` 待发布） |
| DELETE | `/system/help/doc/{id}` | `system:help:doc:delete` | 软删 |
| POST | `/system/help/doc/{id}/publish` | `system:help:doc:publish` | 发布（生成 `sys_help_doc_version` 快照） |
| POST | `/system/help/doc/{id}/rollback` | `system:help:doc:publish` | 回滚到指定 `version`（**生成新版本，不覆盖历史**） |
| GET | `/system/help/doc/{id}/versions` | `system:help:doc:list` | 版本列表 |

### 3.7 检索与索引验证

```sql
-- 用户侧检索（ILIKE + 转义 + 排序）
SELECT id, title, slug, published_at,
       -- snippet 在应用层生成（不返回 HTML）
       left(content_md, 0) AS _
FROM sys_help_doc
WHERE deleted_at IS NULL AND status='PUBLISHED'
  AND (title ILIKE $1 ESCAPE '\' OR content_md ILIKE $1 ESCAPE '\')
ORDER BY published_at DESC
LIMIT $2 OFFSET $3;
```

**实测（POC-7，2000 篇 / 1.5 MB）**

```
Seq Scan on probe_doc_small  (rows removed by filter 1998)
  Filter: ((content ~~* '%基座%') OR (title ~~* '%基座%'))
Execution Time: 6.157 ms     ← 含 ORDER BY + LIMIT 20
count(*) 全表: 4.229 ms
```

**升级预案（文档数 > 5000 时执行，不改代码）**

```sql
CREATE EXTENSION IF NOT EXISTS pg_trgm;                    -- V0083 已预留注释
CREATE INDEX idx_sys_help_doc_title_trgm   ON sys_help_doc USING gin (title gin_trgm_ops);
CREATE INDEX idx_sys_help_doc_content_trgm ON sys_help_doc USING gin (content_md gin_trgm_ops);
-- 实测：3 字稀词 0.238 ms；注意 2 字中文查询仍会退化为全表（POC-7 153 ms）
```

---

## 4. 变更日志（Change Log）——本批最需要谨慎的一块

### 4.1 现状缺陷的精确描述与可复现证据

#### 4.1.1 缺陷拆成三条（而不是一句「回滚也记日志」）

| # | 缺陷 | 机制 | 后果 | 证据 |
|---|---|---|---|---|
| **F1** | **假日志**：业务事务回滚了，日志照样留 | 日志在 axum 中间件里于 `next.run()` **之后**入队（`middleware/operation_log.rs:133`→`:150`），后台 worker 用**独立连接 autocommit** 落库（`support/log/bus.rs:204-219`）。日志与业务事务**无任何事务耦合** | 查日志看到「2026-10-29 10:03 admin 新增用户 张三」，但用户表里没有张三 → **审计结论反向误导**；对账/追责时无法自证 | POC-4 并发实测 |
| **F2** | **无字段级 diff**：只知道「改了用户」，不知道「把手机号从 X 改成了 Y」 | 中间件 `detail` 字段显式留空（`operation_log.rs:158-159`）；`request_params` 是**请求体**（不含旧值），`response_data` 是**响应体**（脱敏后） | 无法回答「这个用户被改过几次、每次改了什么」——本批任务书的核心诉求 | `operation_log.rs:150-170`；`LogPageQuery` 无按对象过滤（`support/log/dto.rs:122-139`） |
| **F3** | **丢日志**：进程重启/崩溃时，队列里未落库的日志消失 | 无界 mpsc + 500 ms 批量（`bus.rs:36-39`）；只有优雅退出才 `flush()`（`plugin.rs:80-83`） | 500 ms 窗口 + 崩溃即丢；对「删除类操作」等于无痕迹 | `bus.rs:204-219`、`plugin.rs:80-83` |

#### 4.1.2 POC-4：三个会话并发复现（真跑输出）

```bash
# 会话1（业务）：BEGIN → INSERT 业务行 → sleep 2 → ROLLBACK
# 会话2（异步日志，模拟现有总线）：sleep 3 → 独立 INSERT 日志 → 隐式提交
# 会话3（同事务日志，模拟新方案）：BEGIN → INSERT 业务行 → INSERT 日志 → sleep 2 → ROLLBACK
$ bash /tmp/demo_rollback.sh
 biz_rows_after_rollback | fake_log_rows_async | intx_log_rows
-------------------------+---------------------+----------------
                       0 |                   1 |              0
```

**读法**：业务行回滚后为 0（正确）；异步路径留下 **1 行日志**（**F1 假日志**）；同事务路径为 **0 行**（新方案正确）。

#### 4.1.3 顺带查清的两件事

1. **`operation_logs` 只有一个写入者**（`grep -rn enqueue_operation` 仅 `operation_log.rs:150`）→ 想改造它，无处可「局部修」，只能整体换架构。
2. **`audit_logs` 也走同一条异步总线**（`support/log/mod.rs:204-220`），但它的 55 处调用点都在业务事务**提交之后**（如 `system/user/handler.rs:269-276` 事务 → `:284` `record_audit`）→ 今天它的问题是 **F3 丢日志**，而不是 F1 假日志。这个区分很重要：**不能拿 `audit_logs` 当变更凭证**（它既不原子、也没字段级配对）。

### 4.2 三方案对比与推荐

| 判据 | (a) 修 `operation_logs`（改事务内记录） | (b) 新建 `sys_change_log`，保留 operation_logs 现状 | **(c) 混合（推荐）** |
|---|---|---|---|
| 能否拿到业务事务 | ❌ 中间件在 axum 层（`lib.rs:35-37` 装配），**没有** `Transaction` 句柄 | ✅ 业务 handler 手里就有 `tx` | ✅ |
| 能否拿到「字段旧值→新值」 | ❌ 中间件只有 HTTP 报文；旧值要额外查询，而查询点也在中间件里（多一次全表读） | ✅ handler 本来就要 `SELECT ... FOR UPDATE` 拿旧值 | ✅ |
| 改动面 | **极大**：要改 30+ handler 的签名与调用，并删掉中间件的路径→动作映射（`operation_log.rs:321-433`，111 行） | 小：新增模块 | 小：新增模块 + 定位说明 |
| 是否保住「请求流水（含 401/403/404）」 | ❌ 一旦搬进 handler，**未进入 handler 的请求（401/403/404/429）就再也记不到** | ✅ 中间件保留 | ✅ |
| 长事务风险 | — | 无 | 低（change_log 单行 INSERT，POC-11：18.8 µs/行） |
| 存量数据 | 语义变更 → 存量与新数据不可比 | 存量继续有效（作为流水） | 存量继续有效（明确标注口径切换日） |
| **结论** | ❌ **不可行**（不是「难」，是「拿不到事务」） | ⚠️ 可行但缺「请求流水」定位，容易与 change_log 重复记录 | ✅ **选 (c)** |

#### 4.2.1 (c) 的责任划分（写进文档与代码注释，防止下次又混）

| 表 | 回答什么问题 | 写入时机 | 保留 | 谁看 |
|---|---|---|---|---|
| **`sys_change_log`** | 「**谁**在**什么时候**把**哪条数据的哪个字段**从**什么**改成了**什么**」 | 业务事务**内**（同 `tx`） | 36 个月 | 审计员、业务负责人（按对象查历史） |
| `operation_logs` | 「谁在什么时候**调了哪个接口**、结果码多少、耗时多少」 | HTTP 中间件（现状不变） | 90 天（`constants.rs:276`） | 运维、安全（异常排查、慢接口） |
| `audit_logs` | 「发生了什么**安全/认证事件**」（登录、改密、明文豁免读取） | 事件发生处（现状不变） | 现状 | 安全 |
| `auth_logs` / `security_logs` / `password_change_logs` | 专项事件 | 现状不变 | 现状 | 安全 |

#### 4.2.2 不选 (b) 的具体理由（虽然 (b) 也能工作）

(b) 让 `operation_logs` 与 `sys_change_log` **同时**在业务路径上被写，等于同一次操作写两遍日志（一遍流水、一遍凭证），但**没有任何代码约束保证两遍说的是同一件事**。本批明确 `operation_logs` 的唯一职责是「HTTP 请求流水（含失败）」、`sys_change_log` 的唯一职责是「已提交的字段级变更」，并在 §5.4 用单测断言「**一次请求最多产生 N 行 change_log，且每行都能对应到一次提交**」。

### 4.3 表设计：`sys_change_log`（月分区）

#### 4.3.1 分区带来的第一个硬约束（实测）

```sql
CREATE TABLE probe_cl_bad (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  target_id uuid NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
) PARTITION BY RANGE (created_at);
-- ERROR:  unique constraint on partitioned table must include all partitioning columns
-- DETAIL:  PRIMARY KEY constraint on table "probe_cl_bad" lacks column "created_at"
--          which is part of the partition key.
```

→ **主键必须是 `(id, created_at)`**（POC-9）。这条约束的连带影响**必须写进实现规范**：

| 影响 | 处置 |
|---|---|
| 不能用 `Model::find(id)` 这类**单主键** API | `sys_change_log` **不派生** `Model` 的单主键查找；`model.rs` 里一律 `find_with_params`（项目既有写法，见 `system/user/model.rs:185-250`） |
| 详情接口传 `(id, created_at)` | `GET /system/change-log/{id}?created_at=...`，否则分区裁剪失效（全分区扫） |
| 外键引用它 | 不提供（审计表不做被引用方） |

#### 4.3.2 表结构（完整 DDL 见附录 A）

```sql
CREATE TABLE sys_change_log (
    id uuid NOT NULL DEFAULT gen_random_uuid(),
    request_id       varchar(36),        -- 与 operation_logs.request_id 可关联
    trace_source     varchar(16) NOT NULL DEFAULT 'API',   -- API/JOB/IMPORT/SYSTEM
    operator_id      uuid,
    operator_name    varchar(50),        -- 冗余：用户被删后仍可读
    operator_dept_id uuid,               -- 冗余：便于按部门审计（用户调部门后不乱）
    client_ip        inet,
    module           varchar(32) NOT NULL,   -- system / business / support
    resource         varchar(64) NOT NULL,   -- sys_user / sys_role / sys_announcement / sys_field_mask
    resource_id      varchar(64) NOT NULL,   -- 业务主键（UUID 或复合键字符串）
    action           varchar(32) NOT NULL,   -- CREATE/UPDATE/DELETE/STATUS/ASSIGN/PUBLISH/OFFLINE/UPLOAD/DOWNLOAD/CLEAN/LOGIN
    action_label     varchar(100) NOT NULL,  -- 中文可读（「修改用户」「重置密码」）
    diff             jsonb NOT NULL DEFAULT '{}'::jsonb,
    diff_mode        varchar(16) NOT NULL DEFAULT 'WHITELIST',
    changed_fields   text[],                 -- 冗余字段名列表（供 && 反查）
    result           varchar(16) NOT NULL DEFAULT 'SUCCESS',
    error_code       int,
    duration_ms      int,
    created_at       timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (id, created_at)
) PARTITION BY RANGE (created_at);
```

**约束（已实跑拒绝非法值）**

| 约束 | 作用 | 实测 |
|---|---|---|
| `ck_sys_change_log_action` | 动作枚举封闭（防「HACK」这类脏值写进去） | ✅ 拒绝 `'HACK'` |
| `ck_sys_change_log_resource` | `^[a-z][a-z0-9_]{1,63}$`（表名口径，拒绝 `'Sys-User'`） | ✅ 拒绝 `'Sys-User'` |
| `ck_sys_change_log_source` | `API/JOB/IMPORT/SYSTEM` | — |
| `ck_sys_change_log_mode` | `WHITELIST/FULL/REDACTED` | — |
| `ck_sys_change_log_result` | `SUCCESS/FAILED`（**本批只写 SUCCESS**，见 4.3.3） | — |
| 无 DEFAULT 分区 | 越界时间写入 → **直接报错**（`no partition of relation "sys_change_log" found for row`） | ✅ POC-9 |

#### 4.3.3 `result` 字段的语义（明确边界，避免又造出「半真半假」的日志）

**本批只写 `SUCCESS`。** 理由：

1. 失败尝试若写在**同一事务**内，事务回滚时它自己也会被回滚 → 写不进去（这正是我们要的性质）；
2. 若写在**独立事务**里，那就回到了「与业务结果无关的日志」这条老路（F1 的翻版），只是换了个字段名；
3. 「失败的尝试」已经在 `operation_logs` 里有（`status_code` 非 2xx）+ `audit_logs` 里有（业务显式记录）。

→ `result` 列保留、`CHECK` 允许 `FAILED`，但**v1 实现只写 `SUCCESS`**，并在 model 层用 `debug_assert_eq!(result, "SUCCESS")` 钉死。若将来 D 批次要为「审批驳回」记录失败态，那是**业务状态变更**（成功写入了状态=REJECTED），仍属 SUCCESS。

**这条边界必须写进测试**（§5.4 T-CL-07）：`SELECT count(*) FROM sys_change_log WHERE result<>'SUCCESS'` 必须为 0。

### 4.4 记录内容与 diff 结构

#### 4.4.1 必填 9 要素（对照任务书）

| 任务书要求 | 列 | 说明 |
|---|---|---|
| 操作人 | `operator_id` + `operator_name` + `operator_dept_id` | 三重冗余：改名/调部门后历史仍可读 |
| 时间 | `created_at` | `TIMESTAMPTZ`，与业务事务同一时刻 |
| IP | `client_ip` | 来自 `utils::client_ip(headers, connect_info)`（与中间件同一口径，`operation_log.rs:118`） |
| 模块 | `module` | `system`/`business`/`support` |
| 动作 | `action` + `action_label` | 英文枚举（可聚合）+ 中文（可展示） |
| 对象 ID | `resource` + `resource_id` | 「表名 + 主键」，支持 UUID 与复合键字符串 |
| **变更前后值（diff）** | `diff` | 见 4.4.2 |
| 请求 ID | `request_id` | 与 `operation_logs.request_id` 可 join（同一请求的流水与凭证能对上） |
| 结果 | `result` | 本批恒 `SUCCESS` |

#### 4.4.2 diff 的 JSON Schema（版本化）

```jsonc
{
  "v": 1,                                  // schema 版本（将来加字段靠它分支）
  "mode": "WHITELIST",                     // WHITELIST=只记白名单字段的差异；FULL=全字段；REDACTED=全部脱敏
  "fields": {
    "real_name": { "b": "张三",      "a": "张三丰" },
    "mobile":    { "b": "138****0000", "a": "139****1111", "masked": true },
    "status":    { "b": 1,           "a": 0 },
    "remark":    { "b": null,        "a": "离职" }
  },
  "big": {
    "content_md": {                        // 大字段/长文本：摘要化，不存明文
      "b": { "len": 8213, "sha256": "9f2c…", "preview": "## 维护时间…" },
      "a": { "len": 8450, "sha256": "aa71…", "preview": "## 维护时间…" },
      "truncated": true
    }
  },
  "meta": { "table": "sys_user", "pk": "…", "tx": "api" }
}
```

**三条铁律（实现时必须遵守）**

| 铁律 | 违反后果 | 实现约束 |
|---|---|---|
| **只记变化字段**（值相等的不记） | diff 里塞满未变字段 → 行宽爆炸（POC-5：6 字段 355 B vs 全字段 ~2 KB） | 提供 `diff::pairs(before, after, whitelist)` 纯函数，先做 `!=` 过滤 |
| **`b`/`a` 必须成对** | 只有 `a` 没有 `b` → 展示成「从空改成 X」；对 UPDATE 是错的 | `pair()` 构造函数强制两侧都存在（新增用 `create` 模式：`b` 省略并置 `"b": null` 且 `action=CREATE`） |
| **同一字段只有一个键** | 出现 `mobile` 与 `Mobile` 两行 | 字段名一律取**列名原样**（小写下划线），不做驼峰转换 |

#### 4.4.3 白名单 vs 全字段：怎么定

| 方案 | 写入字段数 | 存储 | 可解释性 | 结论 |
|---|---|---|---|---|
| 全字段快照 | 表有多少列记多少列 | 大（POC-5：355 B → ~2 KB/行） | 噪声大（`updated_at` 每次都变） | ❌ |
| **每资源显式白名单（推荐）** | 6–12 个业务字段 | 355 B/行 | 清晰 | ✅ |
| 自动排除审计列 | 全字段 − `created_at/updated_at/update_by/…` | 中 | 中等 | ⚠️ 备选 |

**白名单登记方式**：在 `business/changelog/registry.rs` 里用 `const` 声明，一表一条：

```rust
pub const USER_FIELDS: &[&str] = &["username","real_name","mobile","email","dept_id","status","remark","avatar"];
pub const ANNOUNCEMENT_FIELDS: &[&str] = &["title","summary","content_md","level","scope_type","is_top","expire_at","status"];
// 未登记的表 → 拒绝记录（fail-closed），错误码 62012
```

**未登记就拒记录**（而不是「退化为全字段」）：与批次 B 的 D8 fail-closed 一致 —— 宁可让开发在接入时补白名单，也不要让 `password` 这类列悄悄进 diff。

### 4.5 大字段（富文本）怎么处理

#### 4.5.1 实测数据（POC-5）

| 场景 | jsonb 逻辑大小 | 落盘（TOAST 压缩后） | 每 100 行 |
|---|---|---|---|
| 6 字段用户修改 | 355 B | 355 B | — |
| 8k 字富文本「前后双快照」（**内容可压缩**，重复文本） | 137 kB | **2 539 B/行** | 248 kB |
| 8k 字富文本「前后双快照」（**内容不可压缩**，随机汉字） | 137 kB | **51 000 B/行** | 5 080 kB |
| 8k 字富文本「摘要化」（len + sha256 + preview 200 字） | 1 450 B | **2 703 B/行** | 264 kB |

> 注意两个「落盘」数字差 20 倍：**高度重复的文本被 TOAST 压缩得很厉害**，真实富文本不会。所以设计必须按**不可压缩**的 51 KB 来算。

#### 4.5.2 结论与阈值

| 决策 | 取值 | 依据 |
|---|---|---|
| 触发摘要化的阈值 | 单字段序列化后 **> 2 KiB**，或列类型 ∈ `{text, bytea, jsonb}` 且长度 > 2048 | 2 KiB ≈ 中文 700 字，覆盖 95% 的「短字段」 |
| 摘要内容 | `{len, sha256, preview(前 200 字), truncated:true}` | preview 让审计员能肉眼判断「改的是哪一段」，sha256 让「是否真的变了」可机器判定 |
| 明文要不要留一份 | **不留**。全文另存 `sys_help_doc_version` / `sys_announcement` 自身历史（版本表） | 日志表是热表，不该扛 51 KB × N |
| 阈值可配 | `sys_config: changelog.big_field_bytes = 2048` | 运维可调，不改代码 |

**为什么用 sha256 而不只是 len**：`len` 相同但内容不同（改了一个字）时，审计员无法分辨；`sha256` 可以，且能顺手做「同一内容重复修改」的识别（`b.sha256 == a.sha256` → 实际上没变，可跳过不记）。

### 4.6 事务内记录：必须遵守的三条实现约束

| 约束 | 内容 | 违反时的后果 | 强制手段 |
|---|---|---|---|
| **C1** | `changelog::record()` 的第一个参数**必须**是 `&impl SqlExecutor`（`&Db` 与 `&Transaction` 通用，项目惯例见 `business/notice/model.rs:746`） | 传 `db()` → 又变成独立事务 → 假日志复活 | **签名设计**：不提供「无 executor」的重载；code review checklist |
| **C2** | 记录动作必须发生在**业务写操作之后、事务提交之前**（同一批次内），且**不得** `?` 之后忘记调用 | 漏记（业务成功但无凭证） | 集成测试断言「每类变更都有对应行」 |
| **C3** | `record()` 内部**禁止**再调用 `db()`（任何形式的独立查询） | 事务外读取 → 读到别的快照；或第二事务 → 死锁风险 | 门禁 grep：`changelog/mod.rs` 与 `changelog/model.rs` 里出现 `db()` → CI 失败（照批次 B 闸① 的写法，`scope.rs` 同类门禁） |

**标准调用形态（示意，非交付代码）**

```rust
// 一个典型 handler（修改用户）
let before = model::find_for_update(&tx, &id).await?;         // 旧值（事务内）
let mut user = before.clone();
user.real_name = req.real_name;
user.mobile = req.mobile;
user.update(&tx).await?;                                      // 业务写
changelog::record(
    &tx,                                                      // ← C1：同一事务
    ChangeCtx::from_claims(&claims, &headers, &connect_info),  // 操作人/IP/request_id
    ChangeTarget::of("system", "sys_user", &id.to_string(), Action::Update, "修改用户"),
    registry::USER_FIELDS,
    DiffInput { before: json!(before), after: json!(user) },
).await?;                                                     // 失败 → 整个事务回滚
```

### 4.7 与数据脱敏联动（**必须脱敏，否则日志成后门**）

#### 4.7.1 为什么「按操作人豁免」是错的（必须在文档里钉死）

一个常见的错误设计：*「超管有明文权限，所以超管操作时日志也存明文」*。这会直接制造后门：

> 超管把某人的手机号从 `13800001111` 改成 `13800002222` → 日志里存明文 → **任何能看日志的人（包括只有 `system:change-log:list` 的普通审计员）都能拿到手机号明文**。

所以本设计的铁律是：**日志库内的值，与「谁改的」无关，只与「这是不是敏感列」有关**。

#### 4.7.2 三层判定（写入时无条件、读取时按人）

| 层 | 时机 | 依据 | 行为 |
|---|---|---|---|
| L1 | 写入前 | ① `sys_field_mask` 已登记的 `(table, column)` ② 内置兜底清单（12 个字段名，对齐 `operation_log.rs:60-73`：`password/new_password/old_password/confirm_password/mobile/phone/id_card/idcard/token/access_token/refresh_token/secret`） | 命中 → 调 `masker::apply(ty, value, param)` 脱敏后入库；`diff.fields[x].masked=true` |
| L2 | 读取时 | 读取者的 `MaskSet`（`masking::registry::load_user_set`，批次 B 已有） | 再脱敏一次（幂等：`masker` 对已脱敏值不变形，`masking/masker.rs` 头注释「幂等 / 空值原样」） |
| L3 | 出参前 | 同 L2 | 同上（防 L2 漏配） |

**L1 的 fail-closed 策略（有意与批次 B 的「导出 422」不同）**

| 情况 | 批次 B 导出 | **本批变更日志** | 理由 |
|---|---|---|---|
| 敏感列解析不到脱敏规则 | `422` 拒绝导出 | **把该字段值替换为 `{"redacted":true}`，`tracing::error!` 告警 + 指标 +1，业务继续** | 日志是旁路产物，让「用户改个资料」因为「脱敏规则没配」而失败，用户体验与可用性代价过大；且 `support/log` 既有策略就是「日志失败绝不影响调用方」（`support/log/mod.rs:26-29`）。**值一定是被抹掉的，绝不会落明文** —— 这才是安全底线 |

> 这段必须写进 PRD/评审记录：**我们选择「不阻断业务，但绝不存明文」**。可验证：断言 `diff::to_string()` 中不含 11 位手机号正则、不含 `password` 键的值。

#### 4.7.3 脱敏后的 diff 长什么样

```jsonc
{
  "fields": {
    "mobile": { "b": "138****0000", "a": "139****1111", "masked": true },
    "email":  { "b": "a***@x.com",   "a": "b***@y.com",   "masked": true }
  }
}
```

**注意 `b` 也脱敏**：只有新值脱敏、旧值明文，等于把历史值泄了。

#### 4.7.4 「变更日志查询是否受数据权限影响」的正面论证

| 观点 | 支持注入 scope | 反对注入（本设计采纳） |
|---|---|---|
| 审计完整性 | — | ✅ 审计员必须看到「全量变更」才能发现越权；注入后 `SELF_ONLY` 的审计员只看到自己 → 形同没有审计 |
| 信息隔离 | 部门经理只想看本部门 | 用**显式过滤参数**（`operator_dept_id`）满足，而不是把「隐式安全边界」塞进审计查询 |
| 与批次 B 的一致性 | `operation_logs`/`audit_logs` 已登记 | 那两张是「日志查看」；`sys_change_log` 是「审计凭证」。**语义不同 → 策略不同**，并在 `scope.rs` 不登记处加注释说明（§5.4.2 S-1） |
| 谁能看 | — | `system:change-log:list` 只授 SUPER_ADMIN + 审计员角色（种子明确） |

### 4.8 索引设计（面向四种真实查询）

| # | 查询 | 索引 | 实测 |
|---|---|---|---|
| Q1 | **某对象的变更历史**（「这个用户被改过几次」） | `(resource, resource_id, created_at DESC)` | **0.065 ms**（跨 3 分区 Append + Index Scan，Limit 20） |
| Q2 | 某操作人的操作序列 | `(operator_id, created_at DESC)` | 同构（B-tree） |
| Q3 | 全库检索（模块 + 动作 + 时间窗） | `(module, action, created_at DESC)` | **6.8 ms**（Bitmap Index Scan，2 万行命中） |
| Q4 | 「哪些变更动过 mobile」 | `GIN (changed_fields)`，SQL 里 `changed_fields && $1::text[]` | 稀值（0.5%）**6.9 ms** 走 GIN；广值（100%）退化为并行 Seq Scan **18.5 ms** |
| Q5 | 按 request_id 关联请求流水 | `(request_id)` | 一请求一行，等值极快 |
| Q6 | 按部门审计 | 不在索引里（用 `operator_dept_id` 过滤，命中率低时靠 Q3 的索引 + Filter） | 若成为热查询，再加 `(operator_dept_id, created_at DESC)` |

**分区裁剪的前提（必须写进实现）**：Q1/Q3/Q4 **都要带 `created_at` 时间范围**，否则会扫全部分区（实测 200k 行 / 3 分区：带时间窗 0.072 ms，不带 15.5 ms）。API 层若用户没传时间范围，**默认补最近 90 天**。

### 4.9 保留策略与分区运维

| 项 | 决策 | 依据 |
|---|---|---|
| 分区粒度 | **按月**（`RANGE (created_at)`） | 36 个月 = 36 个分区；单分区 ≤ 数十 MB，索引可缓存 |
| 预建策略 | 建表时预建当前月 + 后 2 个月；**定时任务每月 1 日 00:05 建下个月** | 无 DEFAULT 分区 → 不预建就会写失败（POC-9 的报错即后果） |
| 是否建 DEFAULT 分区 | **不建** | POC-10：default 堆了 11 万行后，新建正式分区直接失败（`updated partition constraint ... would be violated`）；运维要手工搬数据才能继续 |
| 保留期 | **36 个月**（`sys_config: changelog.retention_months`） | 与财务/审计常见要求对齐；`operation_logs` 保持 90 天不变（职责不同） |
| 清理方式 | `DROP TABLE sys_change_log_YYYY_MM`（**O(1)**，实测 1.5 ms） | 对比 `DELETE` 3.3 万行 37.9 ms 且留死元组（POC-12） |
| 归档 | 清理前可选 `COPY TO` 到对象存储（`business/storage`）—— 本批不做，留 §8 未决项 4 | — |
| 分区数上限 | 36 + 3 预建 = 39；PG 建议单表分区数 < 1000（远低于） | — |
| 清理任务 | `changelog_partition_maintain`（每日 04:10，错峰于 03:xx 的日志清理与 04:30 的存储回收） | `task/mod.rs:44-48` 的错峰纪律 |

**分区运维的三个坑（都要写进 runbook）**

1. **建分区要先锁**：`CREATE TABLE ... PARTITION OF` 会取父表 `AccessExclusiveLock`（毫秒级），必须在低峰执行；
2. **主键含分区键** → 任何「按 id 单查」的接口都要带时间（§4.3.1）；
3. **`pg_inherits` 才是分区清单**（`pg_total_relation_size('父表')` 返回 0，实测过），巡检脚本要用 `sum(pg_total_relation_size(inhrelid))`。

### 4.10 API 契约（变更日志）

| 方法 | 路径 | 权限码 | 说明 | 关键参数 |
|---|---|---|---|---|
| GET | `/system/change-log/page` | `system:change-log:list` | 全局检索 | `module,resource,resource_id,action,operator_id,operator_dept_id,start_time,end_time,keyword,page` |
| GET | `/system/change-log/{id}` | `system:change-log:list` | 单条详情（**必须带 `created_at`**） | `created_at`（必填，分区裁剪） |
| GET | `/system/change-log/object/{resource}/{resource_id}` | `system:change-log:list` | **对象变更历史**（「这个用户被改过几次」） | `start_time?`（缺省最近 90 天）、`page` |
| GET | `/system/change-log/object/{resource}/{resource_id}/timeline` | `system:change-log:list` | 时间线（前端 Tab 用，返回精简字段） | `limit=50` |
| POST | `/system/change-log/export` | `system:change-log:export` | 导出（复用批次 A `impexp` 框架，同步 ≤5000 行） | 同 page 的筛选条件 |
| POST | `/system/change-log/partition/clean` | `system:change-log:clean` | 手工清理过期分区（**必须显式传分区名 + confirm**） | `{partition:"sys_change_log_2023_01", confirm:true}` |
| GET | `/system/change-log/partition/list` | `system:change-log:list` | 分区清单与占用（运维巡检） | — |

**响应示例（对象历史）**

```jsonc
// GET /api/system/change-log/object/sys_user/9c2f.../timeline?limit=3
{ "code": 0, "data": [
  { "id":"...", "created_at":"2026-10-29T02:11:03Z", "action":"UPDATE", "action_label":"修改用户",
    "operator_name":"admin", "changed_fields":["mobile","dept_id"],
    "diff":{ "fields":{ "mobile":{"b":"138****0000","a":"139****1111","masked":true},
                        "dept_id":{"b":"d-1","a":"d-2"} } } },
  { "id":"...", "created_at":"2026-10-21T08:02:44Z", "action":"STATUS", "action_label":"修改用户状态",
    "operator_name":"ops01", "changed_fields":["status"],
    "diff":{ "fields":{ "status":{"b":1,"a":0} } } },
  { "id":"...", "created_at":"2026-09-30T11:20:00Z", "action":"CREATE", "action_label":"新增用户",
    "operator_name":"admin", "changed_fields":["username","real_name","mobile","dept_id"],
    "diff":{ "fields":{ "username":{"b":null,"a":"zhangsan"}, "mobile":{"b":null,"a":"138****0000","masked":true} } } }
]}
```

### 4.11 存量日志怎么办（迁移策略）

| 项 | 决策 | 理由 |
|---|---|---|
| `operation_logs` 存量 | **不迁移**，原地保留，按 90 天自然淘汰 | 它的语义（请求流水）与新表不同，搬过去会把「流水」冒充「凭证」 |
| `audit_logs` 存量 | **不迁移**，原地保留；在 UI 上标注「事件级审计（2026-11-xx 之前）」 | `before_data/after_data` 是全量快照且未必脱敏齐全（早期版本），搬进 `sys_change_log` 会污染「字段级 + 已脱敏」的口径 |
| 口径切换日 | 在 `sys_config` 写 `changelog.since = '2026-11-xx'`，前端「变更历史」Tab 对早于该日的对象显示「该日期之前无字段级记录，可查看操作日志」并给跳转链接 | 诚实告知不可比，而不是假装历史完整 |
| 若审计要求「上线前的变更也要可查」 | 提供**一次性只读回填**脚本（从 `audit_logs.before_data/after_data` 反算 diff），产物写 `trace_source='SYSTEM'` + `diff.v=0`（旧口径标记）+ `result='SUCCESS'`，并在表上加 `backfilled boolean` 列区分 | 明确标记来源与口径，避免与新数据混淆；**本批不做，列为 §8 未决项 5** |
| 是否删除旧表 | **不删**。`operation_logs` / `audit_logs` 仍是排障与安全事件的主入口 | 职责不同 |

### 4.12 「回滚不留假日志」的可验证测试方法（三层，缺一不可）

#### 4.12.1 第 1 层：数据库级并发脚本（证明机制，不依赖 Rust）

**脚本**（`scripts/verify/changelog_rollback.sql` + `run.sh`，可直接进 CI 的 `docker-compose` 环境）：

```bash
# 三个独立连接并发：
#   S1: BEGIN; INSERT INTO demo_biz ...; pg_sleep(2); ROLLBACK;
#   S2: pg_sleep(3); INSERT INTO demo_cl_async ...;            -- 模拟旧异步总线
#   S3: BEGIN; INSERT INTO demo_biz ...; INSERT INTO demo_cl_intx ...; pg_sleep(2); ROLLBACK;
# 断言：
#   count(demo_biz)=0  AND count(demo_cl_async)=1  AND count(demo_cl_intx)=0
```

**实测输出（POC-4，本机真跑）**：`0 | 1 | 0` → **断言通过**（旧路径产生假日志、新路径不产生）。

> 这个脚本的价值：它把「架构承诺」变成「一条 SQL 断言」。谁想偷偷把日志改回异步，这条断言会立刻红。

#### 4.12.2 第 2 层：Rust 单元测试（证明实现，不依赖数据库）

`business/changelog/tests`（纯函数 + 一个可注入的 mock executor）：

| 用例 | 断言 |
|---|---|
| `record_with_tx_marker` | 传给 `record()` 的 executor 是 `Tx` 变体（用 trait 上的 `is_tx()` 标记或 `#[cfg(test)]` 计数器）；**若传 `Db` → 测试失败** |
| `record_builds_pairs` | `before{real_name:张三}, after{real_name:张三丰}` → `diff.fields.real_name == {b:"张三", a:"张三丰"}`；未变字段**不出现** |
| `record_skips_unchanged` | 全部字段相等 → `diff.fields == {}` 且**不产生行**（返回 `Skipped`），不写空日志 |
| `record_big_field_summarized` | 3000 字 text 字段 → `diff.big[x] = {len,sha256,preview(200),truncated:true}`，且 `diff` 序列化后 **不含**原文片段（断言 `!contains(&raw[0..100])`） |
| `record_masks_sensitive` | `mobile` 进 diff 后匹配 `^\d{3}\*{4}\d{4}$`，且 `masked=true`；`password` 的值**永不出现在**序列化结果里 |
| `record_rejects_unregistered_table` | 未登记 resource → `Err(62012)`（fail-closed） |
| `record_never_calls_db` | 编译期/门禁断言：`changelog/{mod,model}.rs` 内无 `db()` 调用 |

#### 4.12.3 第 3 层：集成测试（证明端到端，走真 HTTP + 真库）

| 用例 ID | 场景 | 断言（**可执行**） |
|---|---|---|
| **T-CL-01** | `POST /api/system/user` 成功后**强制事务回滚**：测试专用 header `X-Test-Force-Rollback: 1`（`#[cfg(feature="test-hooks")]` 门控，生产不编译）→ 在事务末尾 `Err(...)` 触发回滚，响应 500 | `SELECT count(*) FROM sys_user WHERE username=?` = 0 **且** `SELECT count(*) FROM sys_change_log WHERE resource='sys_user' AND resource_id=?` = **0** |
| **T-CL-02** | 正常创建用户（200） | `sys_user` 1 行 **且** `sys_change_log` 1 行，`action='CREATE'`，`diff.fields` 含 `username/real_name/…` 且 `mobile` 已脱敏 |
| **T-CL-03** | 正常修改用户（200） | `sys_change_log` 1 行，`changed_fields` **恰好等于**本次实际变更的字段集合（用 `assert_eq` 比对排序后的数组） |
| **T-CL-04** | 修改时传入与原值相同的字段 | `changed_fields` **不含**该字段（断言 `!contains`） |
| **T-CL-05** | 一次请求内两次更新同一对象（如同一 handler 内先改状态再改部门） | `sys_change_log` **2 行**，各自 `action` 不同（证明「一操作一凭证」，而非合并成一行） |
| **T-CL-06** | 删除用户（软删） | `action='DELETE'`，`diff.fields` 含 `deleted_at`（或 `status`），且 `before` 侧字段齐全 |
| **T-CL-07** | 全表扫描 | `SELECT count(*) FROM sys_change_log WHERE result<>'SUCCESS'` = **0** |
| **T-CL-08** | 敏感字段 | 对 `sys_user.mobile` 做 3 次修改后 `SELECT diff::text FROM sys_change_log WHERE ...` → 正则 `1[3-9]\d{9}` **0 命中** |
| **T-CL-09** | `SUPER_ADMIN` 操作 | 超管改手机号 → 日志里**仍是脱敏值**（断言同上，不是「超管例外」） |
| **T-CL-10** | 分区越界 | 直接 `INSERT` 一条 `created_at='2029-01-01'` → 数据库报 `no partition ... found`（并用一条**反向**断言：应用层不允许写未来时间，`400 + 62013`） |
| **T-CL-11** | 分区裁剪 | `EXPLAIN` 对象历史查询 → 计划里只出现 1 个分区（`assert plan.contains("sys_change_log_2026_10") && !plan.contains("2026_09")`） |
| **T-CL-12** | 日志写失败不影响业务 | mock executor 在 changelog INSERT 上返回错误 → 业务**整体回滚**（这是同事务的必然结果，**必须断言**：`sys_user` 也是 0 行）—— 这条把「原子性」变成可测断言 |
| **T-CL-13** | 并发同一对象 | 10 线程并发改同一用户 → `sys_change_log` **10 行**（各自独立事务成功者），无主键冲突（`id` 是 UUIDv7） |

**T-CL-01 的实现要点（这是整套测试的支点）**

```rust
// 仅测试构建启用：在 handler 的事务闭包末尾插入一个可控失败点
#[cfg(feature = "test-hooks")]
if headers.get("x-test-force-rollback").is_some() {
    return Err(DbError::InvalidValue("test: forced rollback".into()));   // 事务整体回滚
}
```

> 没有这个钩子，「回滚不留假日志」就只能靠人工断言，无法进 CI。**这个钩子是本批的必交付项**（列进 §7 C7 验收）。

#### 4.12.4 门禁断言（防止将来又被改回去）

| 门禁 | 断言 | 说明 |
|---|---|---|
| G1 | `grep -rn "sys_change_log" backend/src | grep -v "business/changelog/"` → **0 命中** | 只有 changelog 模块能写变更日志（防止别处偷偷 `INSERT`） |
| G2 | `grep -rn "db()" backend/src/business/changelog/` → **0 命中** | C3：模块内禁止独立事务 |
| G3 | `grep -rn "record(" backend/src/business/changelog/mod.rs` 的签名中 `&impl SqlExecutor` 必须存在 | C1 |
| G4 | `grep -rn "async fn record" ... ` 不得存在「无 executor 版本」 | C1 |
| G5 | 每个 `record(` 调用点必须在 `transaction(` 的词法范围内（用 `scripts/lint/changelog_in_tx.py` 做括号/作用域静态检查） | 这是唯一能自动化的「同事务」证据 |

**G5 的已知局限（诚实说明）**：静态检查只能证明「调用点出现在事务闭包内」，不能证明「传的就是 `&tx` 而不是 `db()`」。因此 **G2 + G5 + T-CL-01 三者必须同时存在**：G2 排除模块内自开事务，G5 排除模块外错误位置，T-CL-01 用运行时行为兜底。

---
## 5. 横切

### 5.1 权限码清单（20 个 + 4 个菜单）

> **三方一致纪律**（`V0065:11-15`）：① 后端 `#[alun::permission(permission="…")]` ② `sys_menu.auth_code` ③ `frontend/src/constants/permissions.ts` —— 三处逐字一致。
> **新代码只进 `business/`**（项目红线）。因此帮助文档与变更日志的**代码**在 `business/help`、`business/changelog`，而**权限码**按任务书用 `system:help:*`、`system:change-log:*` —— 这与批次 B 的 `business/masking` 用 `system:field-mask:*` 是同一先例，**不是笔误**。

#### 5.1.1 公告（`business:announcement:*`，7 个）

| 权限码 | 语义 | 授予 |
|---|---|---|
| `business:announcement:list` | 管理列表 / 详情 / 阅读统计 | SUPER_ADMIN + 公告管理员 |
| `business:announcement:create` | 新建草稿 | 同上 |
| `business:announcement:update` | 编辑 / 置顶 | 同上 |
| `business:announcement:delete` | 软删 | SUPER_ADMIN |
| `business:announcement:publish` | 发布（含扇出）/ 再次发布 | SUPER_ADMIN + 公告管理员 |
| `business:announcement:offline` | 下线 | SUPER_ADMIN + 公告管理员 |
| **`business:announcement:read`** | **我的公告列表/详情/已读/未读数/附件下载** | **所有角色（含超管）** |

#### 5.1.2 帮助文档（`system:help:*`，10 个）

| 权限码 | 语义 | 授予 |
|---|---|---|
| `system:help:category:list` | 分类管理列表 | SUPER_ADMIN + 内容运营 |
| `system:help:category:create` | 新建分类 | 同上 |
| `system:help:category:update` | 改名/移动 | 同上 |
| `system:help:category:delete` | 删除分类（有文档时 409） | SUPER_ADMIN |
| `system:help:doc:list` | 文档管理列表 / 编辑回显 / 版本列表 | SUPER_ADMIN + 内容运营 |
| `system:help:doc:create` | 新建文档 | 同上 |
| `system:help:doc:update` | 编辑文档 | 同上 |
| `system:help:doc:delete` | 软删文档 | SUPER_ADMIN |
| `system:help:doc:publish` | 发布 / 回滚版本 | SUPER_ADMIN + 内容运营 |
| **`system:help:read`** | **分类树 / 文档列表 / 按 slug 阅读 / 检索 / 阅读量上报** | **所有角色** |

#### 5.1.3 变更日志（`system:change-log:*`，3 个）

| 权限码 | 语义 | 授予 |
|---|---|---|
| `system:change-log:list` | 全局检索 / 详情 / 对象历史 / 分区清单 | **SUPER_ADMIN + 审计员角色**（不授予业务角色） |
| `system:change-log:export` | 导出 | SUPER_ADMIN + 审计员 |
| `system:change-log:clean` | 手工清理过期分区（需 confirm） | **仅 SUPER_ADMIN** |

#### 5.1.4 菜单（4 个，UUID 段 `20000000-…-0000000007xx`）

| UUID 尾号 | 类型 | 名称 | auth_code | path | component | 可见 |
|---|---|---|---|---|---|---|
| `…0700` | 2 菜单 | 公告中心（用户侧） | `business:announcement:read` | `/announcement` | `business/announcement/center` | TRUE |
| `…0710` | 2 菜单 | 公告管理 | `business:announcement:list` | `/business/announcement` | `business/announcement/index` | TRUE |
| `…0720` | 2 菜单 | 帮助文档管理 | `system:help:doc:list` | `/system/help` | `system/help/index` | TRUE |
| `…0730` | 2 菜单 | 变更日志 | `system:change-log:list` | `/system/change-log` | `system/change-log/index` | TRUE |
| `…0711`–`…0716` | 3 按钮 | 公告 6 个按钮 | 同 5.1.1 的管理码 | NULL | NULL | FALSE |
| `…0721`–`…0729` | 3 按钮 | 帮助 9 个按钮 | 同 5.1.2 的管理码 | NULL | NULL | FALSE |
| `…0731`–`…0733` | 3 按钮 | 变更日志 3 个按钮 | 同 5.1.3 | NULL | NULL | FALSE |

**授权 SQL 结构（照 `V0045:62-92` / `V0046:100-125`）**

```sql
-- ① SUPER_ADMIN：全部 20 个
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP FROM sys_role r
CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000700'::uuid), ...) AS m(id)
WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ② 其余所有角色：仅「公告中心 + 帮助文档阅读」两个读入口
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP FROM sys_role r
CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000700'::uuid),
                   ('20000000-0000-0000-0000-000000000720'::uuid)) AS m(id)
WHERE r.role_code <> 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;

-- ③ 审计员角色（若已存在 role_code='AUDITOR'）：给 change-log 三码
INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP FROM sys_role r
CROSS JOIN (VALUES ('20000000-0000-0000-0000-000000000730'::uuid)) AS m(id)
WHERE r.role_code = 'AUDITOR' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;
```

> `AUDITOR` 角色本批**只播种菜单授权**，不创建角色（创建角色是运维动作，见 §8 未决项 1）。

### 5.2 错误码（`shared/error_code.rs` 追加 62xxx 段）

| 码 | 常量名 | HTTP | 文案 |
|---|---|---|---|
| 62001 | `ANNOUNCEMENT_CONTENT_INVALID` | 400 | 公告标题/正文格式或长度不合法 |
| 62002 | `ANNOUNCEMENT_NOT_FOUND` | 404 | 公告不存在或已删除 |
| 62003 | `ANNOUNCEMENT_ATTACHMENT_NOT_FOUND` | 404 | 附件不存在或不属于该公告 |
| 62004 | `ANNOUNCEMENT_FORBIDDEN` | 403 | 无权访问该公告 |
| 62005 | `ANNOUNCEMENT_NOT_AVAILABLE` | 403 | 公告未发布或已过期 |
| 62006 | `ANNOUNCEMENT_SCOPE_INVALID` | 400 | 发布范围为空或超过 500 个对象 |
| 62007 | `ANNOUNCEMENT_TOP_INVALID` | 400 | 置顶必须指定未来到期时间 |
| 62008 | `ANNOUNCEMENT_EXPIRE_INVALID` | 400 | 有效期不合法（须晚于发布时间） |
| 62009 | `ANNOUNCEMENT_STATUS_CONFLICT` | 409 | 已发布公告不可直接编辑，请先下线 |
| 62010 | `HELP_CATEGORY_DEPTH_EXCEEDED` | 422 | 分类层级超过 3 层 |
| 62011 | `HELP_CATEGORY_NOT_EMPTY` | 409 | 分类下仍有文档，不可删除 |
| 62012 | `CHANGE_LOG_RESOURCE_UNREGISTERED` | 500 | 变更日志：资源未登记白名单（接入缺陷） |
| 62013 | `CHANGE_LOG_TIME_INVALID` | 400 | 变更时间不合法（未来时间或早于保留期） |

> 62012 用 500 是**有意的**：它不是用户输入问题，是开发接入缺陷，必须报警而不是静默。

### 5.3 迁移分配（V0080–V0089）

| 版本 | 文件 | 内容 | 回滚 |
|---|---|---|---|
| **V0080** | `business/V0080__create_announcement` | `sys_announcement` + `sys_announcement_scope` + `sys_announcement_read` + `sys_announcement_attachment`（4 表） | `DROP TABLE` 逆序 |
| **V0081** | `business/V0081__announcement_indexes` | 公告 6 个索引（含 2 个部分索引） | `DROP INDEX` |
| **V0082** | `business/V0082__create_help` | `sys_help_category` + `sys_help_doc` + `sys_help_doc_version`（3 表） | 逆序 |
| **V0083** | `business/V0083__help_indexes` | 帮助 4 个索引 + **pg_trgm 升级预案（注释态）** | `DROP INDEX` |
| **V0084** | `business/V0084__create_change_log` | `sys_change_log` 父表 + **预建 3 个月分区** + 5 个索引 | `DROP TABLE`（含分区） |
| **V0085** | `business/V0085__seed_batch_c_permissions` | 20 个权限码 + 4 个菜单 + 授权（超管全给 / 其余角色两个读入口 / AUDITOR） | 删除对应 `sys_menu` 与 `sys_role_menu` |
| **V0086** | `business/V0086__seed_batch_c_data` | ① `sys_storage_policy` 增 `announcement` 分类策略 ② `sys_config` 增 5 个参数（见下）③ 帮助文档默认分类 3 条 | 按 id 删除 |
| V0087 | — | **预留**（`operation_logs` 若批准加 `origin` 列 / 注释修订） | — |
| V0088 | — | **预留**（变更日志存量回填，若 §8 未决项 5 批准） | — |
| V0089 | — | **预留**（批次 A/B 线上补丁） | — |

**V0086 的 `sys_config` 参数（不改代码的运维旋钮）**

| key | 默认值 | 用途 |
|---|---|---|
| `announcement.default_expire_days` | `30` | 新建公告时前端预填的有效期 |
| `announcement.fanout_enabled` | `true` | 全局扇出开关（应急止血） |
| `announcement.scope_refresh_on_dept_change` | `false` | §2.3.2 的「受众是否跟随部门变更」 |
| `changelog.retention_months` | `36` | 分区保留月数 |
| `changelog.big_field_bytes` | `2048` | 大字段摘要化阈值 |

### 5.4 需报批的底座改动清单（逐项说明为什么必须动）

> 判定标准：能否用「新增 `business/` 下的代码 + 种子 SQL」实现？能 → 不需要报批；不能 → 列入本清单。

#### 5.4.1 必改（4 项）

| # | 改动 | 为什么**必须**动 | 不改的后果 | 风险 |
|---|---|---|---|---|
| **B-1** | `backend/Cargo.toml`：`alun` 之外新增 `pulldown-cmark = "0.13"`（或 `comrak`） | 仓库**没有任何** Markdown 渲染器（`grep markdown|comrak|pulldown` → 0 命中）；D4 选定「Markdown 源 + 服务端渲染」，渲染器是硬需求 | 只能选「富文本 HTML 直存」→ 实测表现力崩塌（POC-1）+ XSS 面扩大 + 无 TOC/摘要 | 低（纯 Rust、无 C 依赖、无 IO；`edition 2024` 兼容）。**需评审**：新增依赖违反「不引入 alun 之外的新 crate」的既有惯例（既有例外：`calamine`、`futures-util`、`sha2`、`tokio-util` 都已在 `Cargo.toml` 里，都是「必要且不可替代」） |
| **B-2** | `src/task/mod.rs::register_builtin()`：追加 **5** 个 `register_cron`（`announcement_auto_expire` / `announcement_top_expire` / `announcement_dispatch_retry` / `changelog_partition_maintain` / `help_version_cleanup`） | 调度器对注册表做**启动时一次性快照**，注册点只有 `register_builtin()`（`task/mod.rs:8-9`、`:228-245`）；写进 `bootstrap.rs` 会被随后的 `reload()` 清空（`:32-33` 明写） | 公告永不过期、分区永不预建（下月写入直接失败，POC-9）、版本快照无限膨胀 | 低（纯注册，每行 1 条语句，与既有 8 条同构） |
| **B-3** | `src/shared/error_code.rs`：追加 62xxx 段 13 个常量 | 项目纪律「错误三出口」要求业务码集中定义（`shared/error_code.rs` 现有 83 个常量，`61001`–`61012` 是批次 B 段）；散落定义会被评审打回 | 无法给出稳定业务码 → 前端只能按 HTTP 状态猜错误 | 极低 |
| **B-4** | `src/business/mod.rs`：追加 `pub mod announcement; pub mod changelog; pub mod help;` | 业务模块容器的**唯一入口**（`business/mod.rs:16-23`）；不加则模块不参与编译 | 模块不存在 | 极低 |

#### 5.4.2 建议改（2 项，可延后）

| # | 改动 | 为什么建议 | 不改的后果 | 是否本批 |
|---|---|---|---|---|
| **S-1** | `src/business/shared/scope.rs`：在 `SCOPED_TABLES` 附近加**注释**说明「`sys_announcement` / `sys_help_doc` / `sys_change_log` 有意不登记（全局内容 / 审计完整性）」 | 批次 B 的纪律是「新表必须登记或显式 unscoped」；不写清楚，下一批会有人「顺手登记」→ 公告列表被数据权限过滤、审计出现盲区 | 未来回归风险（非本批功能风险） | ✅ 建议本批做（纯注释，2 行） |
| **S-2** | `docs/后端二开指南.md`：检查清单追加「新增列表接口必须走 `scope::paginate*`」「新增可变更资源必须登记 `changelog::registry` 白名单」 | 把本批的两条不变量写进团队 SOP（批次 B 就是这么做的，`scope.rs:270-273` 引用了该指南） | 下一个模块接入时漏登记（62012 会报，但代价是返工） | ✅ 建议本批做（文档） |

#### 5.4.3 **不改**的（明确写出来，防止评审误以为漏了）

| 项 | 为什么不动 |
|---|---|
| `support/log/mod.rs` 的 `record_audit` / `record_operation` | 55 处调用点的语义会全变（§4.2 方案 (a) 已否决）；本批用新表解决，不动老链路 |
| `business/notice/mod.rs::dispatch` 的签名 | 保持「对外唯一发送口径」不变；联动用「提交后调用 + 幂等键 + 补偿」解决（§2.2） |
| `business/storage/*` | 附件上传/直传/回收全部复用现有接口；下载用「公告侧代理接口」实现，不需要改 storage |
| `middleware/operation_log.rs` | 保留为「请求流水」；`detail` 字段保持为空（它拿不到前后值，不该假装能拿到） |
| `shared/scope.rs` 的 `SCOPED_TABLES` 内容 | 三张新表**故意不登记**（S-1 只是加注释说明） |

### 5.5 测试要求（C7）

#### 5.5.1 公告（`business/announcement`）

| 用例 | 断言 |
|---|---|
| A-01 范围判定矩阵 | `{ALL, ROLE, DEPT(含下级), USER}` × `{命中, 不命中}` 8 组；用 `sys_dept` 三级树验证「含下级」只命中子树 |
| A-02 状态过滤 | `DRAFT/OFFLINE/EXPIRED` 对普通用户**全部不可见**（三种各断言 0 行） |
| A-03 到期双保险 | `expire_at` 已过但定时任务未跑（把 status 手工留成 PUBLISHED）→ 查询仍不可见 |
| A-04 置顶排序 | 置顶公告在前；置顶到期后（`top_expire_at` 已过）自动落到按时间排序位置 |
| A-05 发布扇出 | 发布 → `sys_notice` 1 行（`biz_type='announcement'`、`idempotency_key='announcement:{id}:v1'`）；编辑后再发布 → **第 2 行**（v2） |
| A-06 扇出幂等 | 同一 version 重复调 `dispatch` → `sys_notice` 仍 1 行、`duplicated=true` |
| A-07 扇出失败补偿 | mock `dispatch` 失败 → 公告 `PUBLISHED` 且 `notice_id IS NULL`；跑补偿任务 → `notice_id` 被回填、`sys_notice` 恰好 1 行 |
| A-08 已读幂等 | 同一用户连点 10 次已读 → `sys_announcement_read` 1 行、`read_count` = 1 |
| A-09 已读计数不越界 | 并发已读 → `read_count <= receiver_count`（DB 约束兜底，断言不抛 500） |
| A-10 未读数口径 | 未读数 = 可见未过期 − 已读；与 `mine/page` 的 `only_unread` 总数一致（两接口交叉断言） |
| A-11 附件越权 | 用「另一条公告的 file_id」拼 URL → `404 + 62003`；用不可见公告的附件 → `403 + 62004` |
| A-12 附件响应头 | `Content-Disposition` 以 `attachment` 开头；`Content-Type` 不是 `text/html`（防存储型 XSS） |
| A-13 XSS（渲染） | 内容含 `<script>`、`<img onerror>`、`[x](javascript:alert(1))`、`<iframe>`、`<svg onload>` → 落库 `content_html` **不含** `script/onerror/javascript:/iframe/svg onload`（逐项断言），且命中 `62001` 或净化后为空 |
| A-14 XSS（Markdown 内嵌 HTML） | `content_md` 里写 `<div onclick=...>` → 渲染器 `ENABLE_HTML=false` → 输出为**纯文本转义**（断言含 `&lt;div`） |
| A-15 图片白名单 | `![](https://evil.example/x.png)` → 渲染后 `<img>` 被替换为 `[图片已移除]` |
| A-16 链路协议 | `[x](file:///etc/passwd)` → 链接降级为纯文本 |
| A-17 长度上限 | 128 KiB + 1 字节 → `400 + 62001`；正文为空 → `400 + 62001` |
| A-18 状态机 | `PUBLISHED` 直接 `PUT` → `409 + 62009`；`OFFLINE → publish` → 成功且 `version+1` |
| A-19 定时下线 | 造 3 条 `expire_at` 已过 → 跑 `announcement_auto_expire` → 全部 `EXPIRED`；再跑一次 → 影响行数 0（幂等） |
| A-20 权限码 | 无 `business:announcement:create` 的角色 `POST` → 403；无 `business:announcement:read` **不存在**（全员都有，断言种子覆盖所有角色） |

#### 5.5.2 帮助文档（`business/help`）

| 用例 | 断言 |
|---|---|
| H-01 分类深度 | 建 4 层 → `422 + 62010`；移动导致超深 → 同码 |
| H-02 分类非空 | 分类下有文档时删除 → `409 + 62011`，且分类仍在 |
| H-03 slug 唯一 | 重复 slug → 409；格式非法（空格/大写）→ 400（DB CHECK 兜底，`ck_sys_help_doc_slug` 已实跑） |
| H-04 发布必填时间 | `status='PUBLISHED'` 而 `published_at` 为 NULL → DB 拒绝（`ck_sys_help_doc_publish` 已实跑） |
| H-05 版本快照 | 连发 3 版 → `sys_help_doc_version` 3 行，`version` 1/2/3，内容各自正确 |
| H-06 回滚 | 回滚到 v1 → 文档内容 = v1 内容，`version` = 4（**新版本**，历史不动） |
| H-07 版本清理 | 造 25 版 → 跑 `help_version_cleanup` → 只剩 20 版，且删的是最旧的 |
| H-08 检索转义 | 关键词 `100%` / `a_b` / `\` → 不 panic、不全表命中（断言只命中真含该字面量的行） |
| H-09 检索只含已发布 | `DRAFT` 文档里的关键词 → 普通用户检索 0 命中；管理员 + `status=DRAFT` → 命中 |
| H-10 snippet 纯文本 | 返回的 `snippet` 不含 `<`/`>`（断言正则） |
| H-11 TOC 锚点 | 内容含 2 个 `##`、1 个 `###` → `toc` 树 2 项（第二项 1 子）；`content_html` 含 `id="h-2-1"`（**验证 `add_generic_attributes(&["id"])` 生效**） |
| H-12 TOC 唯一性 | 三个同名 `## 概述` → id 分别 `h-2-1/2/3`（断言不重复） |
| H-13 阅读量 | 同 IP + 同 UA 5 分钟内连点 10 次 → `view_count` +1（去重生效） |
| H-14 权限 | 无 `system:help:doc:update` 的角色 `PUT` → 403；无 `system:help:read` **不存在**（全员） |

#### 5.5.3 变更日志（`business/changelog`）——**C7 的重点**

§4.12 已给 13 个集成用例 + 7 个单测 + 5 个门禁。此处只列**验收口径**：

| 类别 | 必须全绿 | 说明 |
|---|---|---|
| 回滚不留假日志 | T-CL-01 + POC-4 脚本 | **本批的验收红线**，一票否决 |
| 原子性 | T-CL-12 | 日志写失败 → 业务回滚（同事务的必然结果） |
| 字段级正确 | T-CL-03 / T-CL-04 / T-CL-05 | `changed_fields` 精确等于实际变更集合 |
| 脱敏 | T-CL-08 / T-CL-09 + `record_masks_sensitive` | 手机号正则 0 命中，超管不例外 |
| 大字段 | `record_big_field_summarized` | 摘要化且不含原文 |
| 分区 | T-CL-10 / T-CL-11 | 越界拒绝 + 裁剪生效 |
| 门禁 | G1–G5 | 5 条 grep/静态检查进 CI |
| 性能 | 对象历史 P95 < 5 ms | POC-11 实测 0.065 ms，留 70× 余量 |

#### 5.5.4 回归红线（每批必跑）

| 项 | 期望 |
|---|---|
| `cargo test` | 基线全绿（任务书 **362** / B7 证据 **373**，以真机实测为准）+ 本批新增 ≥ 95（公告 30 + 帮助 25 + 变更日志 25 + 门禁 15）→ **≥ 457** |
| 门禁闸①/② | 0/0（不新增 `paginate_with_params` 裸调用） |
| `cargo clippy --all-targets` | 0 warning（项目既有标准） |
| 启动自检 | `preflight_permission()` 仍 0 缺失（本批不登记新表 → 不会引入缺失项） |

### 5.6 前端页面清单（6 页 + 1 复用组件）

| # | 页面 | 路由 | 组件目录 | 权限码 | 关键交互 |
|---|---|---|---|---|---|
| P1 | **公告中心**（用户侧列表 + 详情） | `/announcement` | `business/announcement/center` | `business:announcement:read` | 置顶区 + 列表 + 已读标记 + 未读角标；详情页贴 `content_html`（**经 DOMPurify**）+ 附件下载 + TOC 不需要 |
| P2 | **公告管理**（列表 + 编辑抽屉） | `/business/announcement` | `business/announcement/index` | `business:announcement:list` | TDesign `Grid`；编辑器 = Markdown 文本框 + 右侧实时预览（预览走后端 `POST /business/announcement/preview` 渲染，**不在前端渲染**）；发布二次确认显示「将通知 N 人」 |
| P3 | **帮助中心**（用户侧） | `/help` | `system/help/center` | `system:help:read` | 左侧分类树（`t-tree`）+ 中间列表 + 右侧文档（TOC 联动滚动高亮，锚点用服务端生成的 `id`） |
| P4 | **帮助文档管理** | `/system/help` | `system/help/index` | `system:help:doc:list` | 分类树 CRUD + 文档编辑器（同 P2 的 Markdown 组件）+ 版本列表 + 回滚确认 + 发布 |
| P5 | **变更日志查询** | `/system/change-log` | `system/change-log/index` | `system:change-log:list` | 筛选栏（模块/资源/动作/操作人/时间窗，**默认最近 90 天**）+ 表格 + diff 抽屉（字段级左右对照，脱敏值直接展示） |
| P6 | **分区运维**（挂在 P5 的 Tab） | `/system/change-log?tab=partition` | 同上 | `system:change-log:list` | 分区清单 + 行数 + 占用 + 清理按钮（需输入分区名确认） |
| C1 | **对象变更历史组件**（复用） | 嵌入各详情页 Tab | `components/ChangeHistory` | `system:change-log:list` | 入参 `{resource, resourceId}`，时间线 + diff；**无权限时整块隐藏**（不是显示空） |

**前端硬约束（项目既有）**：API 路径写全 `/api/...`；TDesign `Grid` 12 列；权限用 `v-privilege="[PERM.xxx]"`（`constants/permissions.ts`），**禁止页面里写权限码字面量**（该文件头注释已明确）。

### 5.7 风险清单（14 条）

| # | 风险 | 影响 | 概率 | 缓解 | 验证方式 |
|---|---|---|---|---|---|
| **R1** | **XSS**：净化器被放宽（有人为了「支持颜色」放开 `style`） | 存储型 XSS，全站用户 cookie/令牌泄露 | 中 | ① 白名单**写死在 1 个函数**（`shared/content.rs::sanitize*`），全仓只此一处 ② CSP `script-src 'self'` ③ 单测 A-13~A-16 ④ code review checklist 明写「禁止放宽 style/class」 | A-13 用例 + 手工注入 5 种载荷 |
| **R2** | **附件 XSS**：HTML 附件被浏览器当页面渲染 | 同 R1 | 中 | 代理接口强制 `Content-Disposition: attachment` + `Content-Type` 覆写；storage 侧 `policy` 禁 `html/svg` 扩展名 | A-12 断言响应头 |
| **R3** | **日志膨胀**：变更日志存全文快照 | 单条 51 KB（POC-5），10 万条/月 ≈ 5 GB | 中 | 大字段阈值 2 KiB + sha256 摘要（19× 压缩，实测）；月分区 + 36 月上限（≤ 39 分区） | 上线前 `EXPLAIN` + 每月巡检脚本 |
| **R4** | **假日志复活**：新人把 `record()` 改成 `db()` | 审计结论失真，本批核心目标落空 | **中高** | G1–G5 五道门禁 + T-CL-01 集成测试 + POC-4 脚本进 CI | CI 红即拦 |
| **R5** | **脱敏后门**：某敏感列未登记白名单 → 以明文进 diff | 手机号/证件号泄露给审计员 | 中 | 内置 12 字段兜底清单（写死在代码）+ L2/L3 读取时再脱敏 + `changelog::registry` 未登记拒记录（62012） | T-CL-08/09 + 正则扫描日志表 |
| **R6** | **分区未预建** → 跨月写入失败 | 月初 00:00 起所有写操作 500 | 中 | 定时任务每月 1 日 00:05 建下月 + 预建 2 个月 + **不建 DEFAULT 分区**（保证失败即显形，POC-10 的反面） + 启动自检「下月分区存在」 | 单测模拟「下月分区缺失 → 启动告警」 |
| **R7** | **孤儿文件**：上传后未保存公告 | 存储增长 | 高 | 复用 storage 的秒传与回收；`sys_file` 增加「未绑定超 7 天」巡检（batch C 只建巡检 SQL，不自动删） | 巡检脚本输出 |
| **R8** | **附件流量打爆应用**：50 MB × 并发经应用服务器代理 | 带宽/内存 | 中 | 流式输出（`tokio_util::io::ReaderStream`，storage 已有能力）+ 限流 5 并发/用户 + `Cache-Control: private, max-age=300` | 压测 20 并发 × 50 MB |
| **R9** | **阅读量热点行**：热门文档 `view_count` 每次 +1 行锁 | 写入排队 | 中 | 异步累加（`bus` 模式：内存计数 + 每 10 s 批量 `UPDATE`），或 Redis INCR 定时回写 | 压测 200 QPS 阅读 |
| **R10** | **公告扇出超 5 万**：`dispatch` 422 | 发布失败 | 低 | 发布前用 `fanout::count_receivers`（`fanout.rs:212`）+ `ensure_within_limit`（`:247`）预演命中人数 → 前端提前提示「将通知 6.2 万人，超过 5 万上限，请改用全员公告」 | A-05 的边界用例 |
| **R11** | **Markdown 渲染器新增依赖**触碰「不引入新 crate」惯例 | 评审返工 | 中 | §5.4.1 B-1 已写理由 + 备选 `comrak`；离线可编译（无网络依赖） | 评审通过记录 |
| **R12** | **中国式「搜不到」**：2 字中文检索不走索引 | 5 万文档时 150 ms+ | 低（当前量级 200–1000 篇） | D8 的量化触发条件 + pg_trgm 预案 DDL 已备 | 文档数到 5000 时压测 |
| **R13** | **TOC 锚点全失效**：`id` 被 ammonia 剥离 | 目录点不动 | **高（若不实测）** | §3.2 明确 `add_generic_attributes(&["id"])`；**POC-13 已实测验证加白后 `id` 保留**；H-11 断言 | H-11 用例 |
| **R14** | **`sys_change_log` 主键与分区键耦合**：有人写 `WHERE id=?` 单查 | 全分区扫描（39 分区） | 中 | API 契约强制 `created_at` 必填（§4.10）+ 门禁 G6：`grep "FROM sys_change_log WHERE id"` → 0 命中 | 代码评审 + grep |

### 5.8 性能基线（上线后必须复测）

| 接口 | 目标 P95 | 依据 |
|---|---|---|
| `GET /business/announcement/mine/page` | < 20 ms | POC-8 实测 0.258 ms（DB 层）+ 网络/序列化余量 |
| `GET /business/announcement/mine/unread-count` | < 20 ms | POC-8 实测 1.515 ms |
| `GET /business/announcement/mine/{id}` | < 30 ms | 含闸 3 净化 0.635 ms/20 KB |
| `GET /system/help/doc/search` | < 100 ms | POC-7 2000 篇 6.2 ms |
| `GET /system/change-log/object/{r}/{id}/timeline` | < 50 ms | POC-11 0.065 ms |
| `GET /system/change-log/page`（90 天窗） | < 200 ms | POC-11 6.8 ms（2 万命中） |
| 单次业务写入新增开销（change_log） | < 1 ms | POC-11 18.8 µs/行 × 1–3 行 + diff 计算 |

---

## 6. 附录 A：完整 DDL（V0080–V0084，**已在 PG 18.6 实跑通过**）

> 实跑命令：`su -s /bin/bash postgres -c "psql -q -d c_batchc_v2 -v ON_ERROR_STOP=1" < batchC_ddl.sql`
> 结果：`DDL OK`，`information_schema.tables` = **11**（公告 4 + 帮助 3 + change_log 父表 1 + 3 个分区）
> 负面测试（8 条约束 + 1 条分区越界）**全部按预期拒绝**，见附录 B。

### A.1 V0080 —— 公告四表

```sql
-- ======================================================
-- V0080：公告管理（主体 / 发布范围 / 已读回执 / 附件）
-- 依据：docs/批次C-技术架构.md §2.3、§附录 A
-- 回滚：V0080__create_announcement.down.sql
-- ======================================================

CREATE TABLE sys_announcement (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    ann_no          VARCHAR(32)  NOT NULL,
    title           VARCHAR(200) NOT NULL,
    summary         VARCHAR(500) NOT NULL DEFAULT '',
    content_md      TEXT         NOT NULL,
    content_html    TEXT         NOT NULL DEFAULT '',
    content_format  VARCHAR(8)   NOT NULL DEFAULT 'MARKDOWN',
    level           VARCHAR(16)  NOT NULL DEFAULT 'NORMAL',
    scope_type      VARCHAR(8)   NOT NULL DEFAULT 'ALL',
    is_top          BOOLEAN      NOT NULL DEFAULT FALSE,
    top_expire_at   TIMESTAMPTZ,
    status          VARCHAR(16)  NOT NULL DEFAULT 'DRAFT',
    publish_at      TIMESTAMPTZ,
    expire_at       TIMESTAMPTZ,
    publisher_id    UUID,
    publisher_name  VARCHAR(50),
    notice_id       UUID,
    receiver_count  INT          NOT NULL DEFAULT 0,
    read_count      INT          NOT NULL DEFAULT 0,
    version         INT          NOT NULL DEFAULT 1,
    remark          VARCHAR(500),
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at      TIMESTAMPTZ  NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by       UUID,
    update_by       UUID,
    deleted_at      TIMESTAMPTZ,
    CONSTRAINT ck_sys_announcement_status  CHECK (status IN ('DRAFT','PUBLISHED','OFFLINE','EXPIRED')),
    CONSTRAINT ck_sys_announcement_level   CHECK (level IN ('NORMAL','IMPORTANT','URGENT')),
    CONSTRAINT ck_sys_announcement_scope   CHECK (scope_type IN ('ALL','ROLE','DEPT','USER')),
    CONSTRAINT ck_sys_announcement_format  CHECK (content_format IN ('MARKDOWN','HTML')),
    CONSTRAINT ck_sys_announcement_publish CHECK (
        (status = 'DRAFT' AND publish_at IS NULL) OR (status <> 'DRAFT' AND publish_at IS NOT NULL)),
    CONSTRAINT ck_sys_announcement_expire  CHECK (expire_at IS NULL OR publish_at IS NULL OR expire_at > publish_at),
    CONSTRAINT ck_sys_announcement_top     CHECK (is_top OR top_expire_at IS NULL),
    CONSTRAINT ck_sys_announcement_counts  CHECK (receiver_count >= 0 AND read_count >= 0
                                                  AND read_count <= receiver_count),
    CONSTRAINT ck_sys_announcement_body    CHECK (octet_length(content_md) BETWEEN 1 AND 131072)
);

COMMENT ON TABLE  sys_announcement IS '公告主体：内容（Markdown 源 + 渲染快照）；投递由 sys_notice 承担';
COMMENT ON COLUMN sys_announcement.content_md   IS 'Markdown 源（唯一真源，128 KiB 上限）';
COMMENT ON COLUMN sys_announcement.content_html IS '服务端渲染 + ammonia 净化后的 HTML 快照（写时生成，读时再净化）';
COMMENT ON COLUMN sys_announcement.notice_id    IS '发布时扇出的 sys_notice.id（关闭扇出时为空；不加外键：通知会被清理任务硬删）';
COMMENT ON COLUMN sys_announcement.version      IS '内容版本：每次发布 +1，参与 notice 幂等键 announcement:{id}:v{version}';
COMMENT ON COLUMN sys_announcement.status       IS 'DRAFT 草稿 / PUBLISHED 已发布 / OFFLINE 人工下线 / EXPIRED 到期自动';

CREATE TABLE sys_announcement_scope (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    announcement_id UUID NOT NULL REFERENCES sys_announcement(id) ON DELETE CASCADE,
    scope_type      VARCHAR(8) NOT NULL,
    target_id       UUID NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_sys_announcement_scope_type CHECK (scope_type IN ('ROLE','DEPT','USER')),
    CONSTRAINT uk_sys_announcement_scope UNIQUE (announcement_id, scope_type, target_id)
);
COMMENT ON TABLE sys_announcement_scope IS '发布范围明细：DEPT 已按发布时刻展开为「含下级的部门 ID 集合」；读取侧只做 ANY(array) 判定';

CREATE TABLE sys_announcement_read (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    announcement_id UUID NOT NULL REFERENCES sys_announcement(id) ON DELETE CASCADE,
    user_id         UUID NOT NULL,
    read_at         TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT uk_sys_announcement_read UNIQUE (announcement_id, user_id)
);
COMMENT ON TABLE sys_announcement_read IS '已读回执：只记已读（未读 = 可见集合 - 已读集合），避免为全员预写 N 行';

CREATE TABLE sys_announcement_attachment (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    announcement_id UUID NOT NULL REFERENCES sys_announcement(id) ON DELETE CASCADE,
    file_id         UUID NOT NULL,
    file_name       VARCHAR(255) NOT NULL,
    file_size       BIGINT NOT NULL DEFAULT 0,
    mime_type       VARCHAR(128),
    sort_order      INT NOT NULL DEFAULT 0,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT uk_sys_announcement_attachment UNIQUE (announcement_id, file_id)
);
COMMENT ON TABLE sys_announcement_attachment IS '公告附件：引用 sys_file（批次 A 存储）；下载必须经公告可见性校验的代理接口';
```

### A.2 V0081 —— 公告索引

```sql
CREATE UNIQUE INDEX uk_sys_announcement_no ON sys_announcement (ann_no);
-- 列表主查询（status + 置顶 + 时间），部分索引排除软删
CREATE INDEX idx_sys_announcement_list ON sys_announcement (status, is_top DESC, publish_at DESC)
    WHERE deleted_at IS NULL;
-- 自动下线扫描（只扫有到期时间的已发布行）
CREATE INDEX idx_sys_announcement_expire ON sys_announcement (expire_at)
    WHERE status = 'PUBLISHED' AND expire_at IS NOT NULL;
-- 置顶到期扫描
CREATE INDEX idx_sys_announcement_top_expire ON sys_announcement (top_expire_at)
    WHERE is_top = TRUE AND top_expire_at IS NOT NULL;
-- 可见性判定的反向索引（scope_type + target_id → announcement_id）
CREATE INDEX idx_sys_announcement_scope_reverse ON sys_announcement_scope (scope_type, target_id, announcement_id);
-- 我的已读（uk 是 (announcement_id,user_id)，这里需要 user_id 打头）
CREATE INDEX idx_sys_announcement_read_user ON sys_announcement_read (user_id, announcement_id);
-- 附件孤儿巡检
CREATE INDEX idx_sys_announcement_attachment_file ON sys_announcement_attachment (file_id);
```

### A.3 V0082 —— 帮助文档三表

```sql
CREATE TABLE sys_help_category (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    parent_id   UUID REFERENCES sys_help_category(id) ON DELETE RESTRICT,
    name        VARCHAR(100) NOT NULL,
    code        VARCHAR(64)  NOT NULL,
    icon        VARCHAR(64),
    description VARCHAR(500),
    sort_order  INT NOT NULL DEFAULT 0,
    status      SMALLINT NOT NULL DEFAULT 1,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    deleted_at  TIMESTAMPTZ,
    CONSTRAINT ck_sys_help_category_status CHECK (status IN (0,1)),
    CONSTRAINT ck_sys_help_category_code   CHECK (code ~ '^[a-z][a-z0-9_-]{1,63}$')
);
CREATE UNIQUE INDEX uk_sys_help_category_code ON sys_help_category (code) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_help_category_parent ON sys_help_category (parent_id, sort_order);

CREATE TABLE sys_help_doc (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    category_id  UUID NOT NULL REFERENCES sys_help_category(id) ON DELETE RESTRICT,
    title        VARCHAR(200) NOT NULL,
    slug         VARCHAR(120) NOT NULL,
    summary      VARCHAR(500) NOT NULL DEFAULT '',
    content_md   TEXT NOT NULL,
    content_html TEXT NOT NULL DEFAULT '',
    keywords     VARCHAR(255),
    version      INT NOT NULL DEFAULT 1,
    status       VARCHAR(16) NOT NULL DEFAULT 'DRAFT',
    sort_order   INT NOT NULL DEFAULT 0,
    view_count   INT NOT NULL DEFAULT 0,
    published_at TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    update_by    UUID,
    deleted_at   TIMESTAMPTZ,
    CONSTRAINT ck_sys_help_doc_status  CHECK (status IN ('DRAFT','PUBLISHED','OFFLINE')),
    CONSTRAINT ck_sys_help_doc_slug    CHECK (slug ~ '^[a-z0-9][a-z0-9-]{1,119}$'),
    CONSTRAINT ck_sys_help_doc_body    CHECK (octet_length(content_md) BETWEEN 1 AND 262144),
    CONSTRAINT ck_sys_help_doc_publish CHECK (
        (status = 'DRAFT' AND published_at IS NULL) OR (status <> 'DRAFT' AND published_at IS NOT NULL))
);
CREATE UNIQUE INDEX uk_sys_help_doc_slug ON sys_help_doc (slug) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_help_doc_category  ON sys_help_doc (category_id, sort_order) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_help_doc_published ON sys_help_doc (status, published_at DESC) WHERE deleted_at IS NULL;

CREATE TABLE sys_help_doc_version (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    doc_id       UUID NOT NULL REFERENCES sys_help_doc(id) ON DELETE CASCADE,
    version      INT NOT NULL,
    title        VARCHAR(200) NOT NULL,
    content_md   TEXT NOT NULL,
    content_html TEXT NOT NULL DEFAULT '',
    change_note  VARCHAR(500),
    created_at   TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by    UUID,
    CONSTRAINT uk_sys_help_doc_version UNIQUE (doc_id, version)
);
COMMENT ON TABLE sys_help_doc_version IS '文档版本快照：发布时留一份全文用于一键回滚；保留最近 20 版，超出由清理任务删除';
```

### A.4 V0083 —— trgm 升级预案（注释态）

> 说明：帮助文档只有 4 个索引（唯一 slug / 分类 / 发布状态 / 分类排序），量小，**随 V0082 一起建**，不单独拆迁移。本迁移只承载「文档数 > 5000 时的 trgm 升级预案」，默认注释不执行；写一条 `SELECT 1;` 保证迁移文件非空（框架按版本序合并，空文件也可以，但保留占位便于将来就地启用）。

```sql
-- 触发条件：sys_help_doc 行数 > 5000 或检索 P95 > 50 ms（见架构 §3.3.2）
-- CREATE EXTENSION IF NOT EXISTS pg_trgm;
-- CREATE INDEX idx_sys_help_doc_title_trgm   ON sys_help_doc USING gin (title gin_trgm_ops);
-- CREATE INDEX idx_sys_help_doc_content_trgm ON sys_help_doc USING gin (content_md gin_trgm_ops);

SELECT 1;
```

### A.5 V0084 —— 变更日志（父表 + 3 个分区 + 5 个索引）

```sql
CREATE TABLE sys_change_log (
    id               UUID NOT NULL DEFAULT gen_random_uuid(),
    request_id       VARCHAR(36),
    trace_source     VARCHAR(16) NOT NULL DEFAULT 'API',
    operator_id      UUID,
    operator_name    VARCHAR(50),
    operator_dept_id UUID,
    client_ip        INET,
    module           VARCHAR(32) NOT NULL,
    resource         VARCHAR(64) NOT NULL,
    resource_id      VARCHAR(64) NOT NULL,
    action           VARCHAR(32) NOT NULL,
    action_label     VARCHAR(100) NOT NULL,
    diff             JSONB NOT NULL DEFAULT '{}'::jsonb,
    diff_mode        VARCHAR(16) NOT NULL DEFAULT 'WHITELIST',
    changed_fields   TEXT[],
    result           VARCHAR(16) NOT NULL DEFAULT 'SUCCESS',
    error_code       INT,
    duration_ms      INT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (id, created_at),          -- 分区表主键必须含分区键（POC-9 实测）
    CONSTRAINT ck_sys_change_log_source   CHECK (trace_source IN ('API','JOB','IMPORT','SYSTEM')),
    CONSTRAINT ck_sys_change_log_action   CHECK (action IN
        ('CREATE','UPDATE','DELETE','STATUS','ASSIGN','PUBLISH','OFFLINE','UPLOAD','DOWNLOAD','CLEAN','LOGIN')),
    CONSTRAINT ck_sys_change_log_mode     CHECK (diff_mode IN ('WHITELIST','FULL','REDACTED')),
    CONSTRAINT ck_sys_change_log_result   CHECK (result IN ('SUCCESS','FAILED')),
    CONSTRAINT ck_sys_change_log_resource CHECK (resource ~ '^[a-z][a-z0-9_]{1,63}$')
) PARTITION BY RANGE (created_at);
COMMENT ON TABLE sys_change_log IS '变更日志：业务事务内显式写入的字段级 diff（成功路径），月分区，DROP PARTITION 清理';
COMMENT ON COLUMN sys_change_log.diff IS 'JSON schema v1：{v,mode,fields:{col:{b,a,masked?}},big:{col:{b:{len,sha256,preview},a:{...},truncated}},meta}';
COMMENT ON COLUMN sys_change_log.changed_fields IS '冗余字段名数组，供 GIN && 反查「哪些变更动过 X 字段」';
COMMENT ON COLUMN sys_change_log.result IS '本批只写 SUCCESS（失败尝试已在 operation_logs/audit_logs 留痕）';

-- 初始 3 个分区（上线月 + 后 2 个月；之后由定时任务每月 1 日 00:05 预建下月）
CREATE TABLE sys_change_log_2026_10 PARTITION OF sys_change_log FOR VALUES FROM ('2026-10-01') TO ('2026-11-01');
CREATE TABLE sys_change_log_2026_11 PARTITION OF sys_change_log FOR VALUES FROM ('2026-11-01') TO ('2026-12-01');
CREATE TABLE sys_change_log_2026_12 PARTITION OF sys_change_log FOR VALUES FROM ('2026-12-01') TO ('2027-01-01');
-- ⚠️ 刻意不建 DEFAULT 分区：越界写入必须立刻报错（POC-9/POC-10）

CREATE INDEX idx_sys_change_log_object   ON sys_change_log (resource, resource_id, created_at DESC);
CREATE INDEX idx_sys_change_log_operator ON sys_change_log (operator_id, created_at DESC);
CREATE INDEX idx_sys_change_log_module   ON sys_change_log (module, action, created_at DESC);
CREATE INDEX idx_sys_change_log_fields   ON sys_change_log USING gin (changed_fields);
CREATE INDEX idx_sys_change_log_request  ON sys_change_log (request_id);
```

### A.6 V0085 —— 权限种子（结构示例，完整 20 码 × 4 菜单照 §5.1 展开）

```sql
-- 4 个菜单页（menu_type=2）
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000700'::uuid, '公告中心', 2, 'business:announcement:read',
     '/announcement', NULL, 'business/announcement/center',
     70, 1, 'notification', '公告中心', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000710'::uuid, '公告管理', 2, 'business:announcement:list',
     '/business/announcement', NULL, 'business/announcement/index',
     71, 1, 'announcement', '公告管理', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000720'::uuid, '帮助文档管理', 2, 'system:help:doc:list',
     '/system/help', NULL, 'system/help/index',
     72, 1, 'help-circle', '帮助文档管理', TRUE, 'business'),
    ('20000000-0000-0000-0000-000000000730'::uuid, '变更日志', 2, 'system:change-log:list',
     '/system/change-log', NULL, 'system/change-log/index',
     73, 1, 'history', '变更日志', TRUE, 'business')
ON CONFLICT (id) DO NOTHING;

-- 按钮（menu_type=3）：…0711–0716 公告 6 个 / …0721–0729 帮助 9 个 / …0731–0733 变更日志 3 个
-- 授权：见 §5.1.4 的三段 SQL（超管全给 / 其余角色两个读入口 / AUDITOR 三码）
```

---

## 7. 实施拆分（C0–C9，含可执行验收）

> 人日为单人估算；C0–C1 可并行（不同模块），C7 必须在全部功能完成后。

| 步 | 交付物 | 可执行验收（命令级） | 人日 |
|---|---|---|---|
| **C0** | 底座报批落地：`Cargo.toml` + `pulldown-cmark`；`error_code.rs` 13 码；`business/mod.rs` 3 个 `pub mod`（空壳）；`task/mod.rs` 5 个注册（空实现） | `cargo build` 通过；`cargo test` **基线不回归**（362/373 以真机实测为准）；`grep -cE "= 620[0-9]{2}" src/shared/error_code.rs` = 13；`cargo run` 启动日志出现 5 个新任务名 | 2 |
| **C1** | `business/shared/content.rs`：Markdown 渲染（ENABLE_HTML=false、heading 偏移 +1、id 生成）+ 三套净化配方（公告/帮助/纯文本）+ TOC 树生成；**单测 12 个** | `cargo test content` 全绿；断言 `<h2 id="h-2-1">`；断言 `content_md` 里 `<div onclick>` 输出为 `&lt;div`；断言 `![](http://evil/x.png)` → `[图片已移除]` | 3 |
| **C2** | 公告：V0080/V0081 迁移 + `model.rs`（可见性查询/已读/计数）+ `dto.rs` + `handler.rs`（用户侧 7 个接口） | 迁移在测试库跑通（复用 b7 的 `migrate_test.sh` 范式，含 `down` 回滚）；A-01/A-02/A-03/A-10 用例绿；`EXPLAIN` 断言走 `idx_sys_announcement_scope_reverse` | 5 |
| **C3** | 公告：管理侧 10 个接口（含 `preview`）+ 发布/下线状态机 + 扇出与补偿 + 附件代理下载 | A-05~A-09、A-11、A-12、A-18 全绿；补偿任务跑两次后 `sys_notice` 仍 1 行 | 5 |
| **C4** | 帮助文档：V0082/V0083 迁移 + 分类树 + 文档 CRUD + 发布/版本/回滚 + 检索（含转义）+ 阅读量去重 | H-01~H-14 全绿；检索性能实测 < 100 ms（2000 篇造数） | 5 |
| **C5** | 变更日志：V0084 迁移 + `diff` 引擎（白名单/成对/大字段摘要）+ 脱敏 L1 + `record()`（`&impl SqlExecutor`） | 单测 7 个全绿（§4.12.2）；`record_rejects_unregistered_table` 返回 62012 | 4 |
| **C6** | 变更日志：查询 6 个接口 + 分区运维任务（预建/清理）+ 首批接入 4 张表（`sys_user`/`sys_role`/`sys_announcement`/`sys_help_doc`） | T-CL-02/03/04/05/06/11/13 全绿；`GET .../timeline` 在 10 万行造数上 < 50 ms | 4 |
| **C7** | **测试专项**：T-CL-01 强制回滚钩子（`#[cfg(feature="test-hooks")]`）+ POC-4 脚本进 CI + 门禁 G1–G6 | `cargo test --features test-hooks` 全绿；`bash scripts/verify/changelog_rollback.sh` 输出 `0|1|0` 并 exit 0；6 条门禁全 `exit 0` | 3 |
| **C8** | 种子与配置：V0085（权限）+ V0086（storage 策略 + 5 个 sys_config + 3 个默认分类） | 新权限码在「角色-菜单」树可见；非超管账号能进「公告中心/帮助中心」、**进不了**「变更日志」；`sys_storage_policy` 有 `announcement` 行 | 1 |
| **C9** | 前端 6 页 + 1 组件 + 权限常量；联调 | 6 个路由可达；`v-privilege` 生效（无权限按钮不渲染）；公告详情 `content_html` 经 DOMPurify 后无 `script`（浏览器 console 断言）；TOC 点击滚动 | 8 |
| — | **合计** | — | **40** |

**关键路径**：C0 → C1 → {C2→C3, C4, C5→C6} → C7 → C8 → C9。C2/C4/C5 可三人并行。

---

## 8. 未决项（需老大/产品拍板）

| # | 问题 | 选项 | 建议 | 影响 |
|---|---|---|---|---|
| 1 | 是否创建 `AUDITOR`（审计员）角色？ | (a) 新建角色并授 `system:change-log:*` (b) 只授超管 (c) 授某个现有业务角色 | **(a)**：审计独立性要求「看日志的人不能改数据」 | 若不建，变更日志实际只有超管能看 → 审计价值打折 |
| 2 | 是否需要中文分词检索（zhparser / 外部引擎）？ | (a) 不做（ILIKE） (b) 装 zhparser (c) 接 MeiliSearch | **(a)**，文档数 > 5000 时再评估 | (b)(c) 都要额外运维组件 |
| 3 | 帮助文档是否需要「按角色可见」？ | (a) 不做 (b) 按角色 (c) 按菜单/模块 | **(a) 本批**；(c) 是将来正确形态 | 若必须 (b)，C4 需 +3 人日（范围表 + 判定 + 缓存） |
| 4 | 变更日志是否需要「归档到对象存储」再删分区？ | (a) 直接 DROP (b) COPY 到 storage 再 DROP | **(b) 二期**：合规场景常要求「可恢复的删除」 | (a) 简单但不可恢复 |
| 5 | 是否把 `audit_logs.before/after` 回填成 `sys_change_log`（上线前历史）？ | (a) 不回填 (b) 一次性回填并标 `backfilled` | **(a)**：老数据脱敏不齐，回填会把明文带进新表 | (b) 需 +2 人日 + 安全复核 |
| 6 | 公告的「受众是否跟随部门变更」？ | (a) 冻结（发布时刻） (b) 每日重算 | **(a)**，理由见 §2.3.2 | (b) 需每日全量重算任务 + 可见性漂移的解释成本 |

---

## 附录 B：POC 脚本与实测输出

### B.1 环境

```
PostgreSQL 18.6 (Ubuntu 18.6-0ubuntu0.26.04.1) aarch64
验证库：c_batchc_v1（对比实验） / c_batchc_v2（本批 DDL，11 表）
Rust：1.94.1（`rustup override set 1.94.1`），ammonia 4.2.4（离线 registry）
脚本目录：/root/.hermes/workspace/architect/baseline/verify/ 与 .../cache/scratch/xsspoc/
```

### B.2 脚本清单

| 脚本 | 目的 | 复跑命令 |
|---|---|---|
| `verify/batchC_ddl.sql` | V0080–V0084 完整 DDL | `su -s /bin/bash postgres -c "createdb c_batchc_v3" && su -s /bin/bash postgres -c "psql -q -d c_batchc_v3 -v ON_ERROR_STOP=1" < batchC_ddl.sql` |
| `verify/batchC_negative.sql` | 8 条约束 + 1 条分区越界的负面测试 | `su -s /bin/bash postgres -c "psql -d c_batchc_v3 -P pager=off" < batchC_negative.sql` |
| `verify/poc_visibility_data.sql` + `poc_visibility_query.sql` | 公告可见性/未读数（POC-8） | 依次执行 |
| `verify/poc_change_log.sql` / `poc_change_log2.sql` | 变更日志写入/查询/清理（POC-11/12） | 依次执行 |
| `/tmp/demo_rollback.sh` + `sess_{tx,async,intx}.sql` | **假日志 vs 同事务**（POC-4） | `bash /tmp/demo_rollback.sh` |
| `xsspoc/src/main.rs` | XSS 净化行为（POC-1/3） | `cd xsspoc && cargo run --offline -q` |
| `xsspoc/src/bin/bench.rs` | 净化耗时（POC-2） | `cargo run --offline -q --release --bin bench` |
| `xsspoc/src/bin/toc.rs` | TOC 锚点 `id` 是否被剥离（POC-13） | `cargo run --offline -q --bin toc` |

### B.3 关键实测输出（原文摘录）

**POC-4（假日志）**

```
 biz_rows_after_rollback | fake_log_rows_async | intx_log_rows
-------------------------+---------------------+----------------
                       0 |                   1 |              0
```

**POC-2（净化耗时）**

```
ammonia 20KB: 输入 20736 bytes → 0.635 ms/次（200 次平均）
ammonia 64KB: 输入 65772 bytes → 1.976 ms/次（50 次平均）
```

**POC-6（中文 FTS 失效）**

```
   cfg    |                          v
----------+------------------------------------------------------
 simple   | '云枢通用后台管理系统':1 '公告管理':2 '帮助文档':3
 fts_hit_substring | fts_hit_exact
-------------------+---------------
 f                 | t          ← 搜「公告」匹配不到「系统公告管理」
```

**POC-7（trgm 只对 ≥3 字有效）**

```
3 字稀词 '%云枢基%' → Bitmap Index Scan → Execution Time: 0.238 ms
2 字稀词 '%基座%'   → Bitmap Index Scan rows=50000 → 153.306 ms（比全表 131.5 ms 更慢）
2000 篇 ILIKE       → Seq Scan → 6.157 ms（含 ORDER BY + LIMIT 20）
```

**POC-5（大字段）**

```
6字段            |        355 B
24KB富文本快照   |     140443 B   (jsonb 逻辑大小)
随机8k字双快照   |      51 kB/行  (落盘，不可压缩)
摘要化(同正文)   |    2703 B/行   → 19× 压缩
```

**POC-9/10（分区约束）**

```
ERROR:  unique constraint on partitioned table must include all partitioning columns
ERROR:  updated partition constraint for default partition "probe_cl_default" would be violated by some row
```

**POC-11/12（写入与清理）**

```
INSERT 0 100000  Time: 1875 ms（18.8 µs/行，5 索引）
单对象历史 LIMIT 20 → Append + Index Scan → 0.065 ms
DELETE 3.3 万行 → 37.885 ms；DROP PARTITION → 1.493 ms
```

**POC-13（TOC 锚点）**

```
默认规则        : <h2>维护时间</h2><p>正文</p><h3>影响范围</h3>...        ← id 被剥离
加白 ["id"] 后  : <h2 id="h-2-1">维护时间</h2><p>正文</p><h3 id="h-3-1">影响范围</h3>...
断言 默认剥离 id: true / 加白后保留 id: true / h3 也保留: true / 仍不含 script: true
公告侧配方（不保留 id）: 无 id（符合预期）
```

### B.4 清理说明

验证库 `c_batchc_v1` / `c_batchc_v2` 保留（供复核者复跑）；`xsspoc` 保留在 architect 的 scratch 目录（`rustup override` 已设为 1.94.1，不影响其他项目）。生产库**未做任何改动**。

---

## 附录 C：事实索引（结论 → 证据）

| 结论 | 证据（文件:行号 / POC） |
|---|---|
| `operation_logs` 由 HTTP 中间件写入，且在业务之后入队 | `src/middleware/operation_log.rs:104-173`（`:133` 响应、`:150` 入队） |
| 中间件只记 4 种写方法 | `src/shared/constants.rs:270`；`operation_log.rs:106-109` |
| 中间件 `detail` 恒为空（无结构化变更详情） | `operation_log.rs:158-159` |
| 日志后台 worker 独立连接、500 ms 批量、256 行阈值 | `src/support/log/bus.rs:36,39,204-219` |
| `operation_logs` 唯一写入者 | `grep -rn enqueue_operation` → 仅 `operation_log.rs:150` |
| `audit_logs` 也走异步总线（`pub(crate)`、无 executor 参数） | `src/support/log/mod.rs:204-220` |
| 审计日志查询无「按对象」过滤 | `src/support/log/dto.rs:122-139` |
| 日志失败不影响业务是有意设计 | `src/support/log/mod.rs:26-29` |
| `notice::dispatch` 签名与入参 | `src/business/notice/mod.rs:246`、`:77-118` |
| `dispatch` 内部自开事务（无法嵌入调用方事务） | `notice/mod.rs:473`（即时）、`:436`（定时） |
| 接收人上限 5 万 + 单条 INSERT..SELECT 扇出 + 部门递归 | `business/notice/fanout.rs:32,178-190,359-361` |
| `notice` 幂等靠 `uk_sys_notice_idem` | `migrations/migrations/business/V0049__notice_indexes.up.sql:27-28` |
| `notice:mine` 授予所有非超管角色（全员接口范式） | `V0045__seed_notice_permissions.up.sql:62-92` |
| storage 上传接口与三条语义 | `src/business/storage/handler.rs:700-830` |
| storage 上传/下载对**所有角色**开放（既有授权面） | `V0046__seed_storage_permissions.up.sql:113-125` |
| `StorageFileRes` 17 字段 | `src/business/storage/dto.rs:427-456` |
| 净化器 = ammonia 4.2.4，默认 tags/attrs/url_schemes | `alun-utils-0.4.2/src/xss.rs:29-31`；`ammonia-4.2.4/src/lib.rs:379-460` |
| `a[rel]` + `link_rel` 同时设置会 panic | `ammonia-4.2.4/src/lib.rs:1910`，POC-1 |
| `id` 属于 generic_attributes（默认不含，需显式加白） | `ammonia-4.2.4/src/lib.rs:390` |
| 通知模板已用「先净化骨架、再渲染变量」 | `business/notice/render.rs:156-159` |
| 数据权限五种范围 | `business/shared/scope.rs:70-80` |
| 分页两入口 + unscoped 守卫 | `scope.rs:618-631,651-680,208-216` |
| 登记表清单 11 项 | `scope.rs:274-286` |
| 启动自检校验登记表列存在 | `business/shared/guard.rs:218-237,261-278` |
| 定时任务注册唯一入口 + 一次性快照 | `task/mod.rs:8-9,32-33,228-245` |
| 日志保留期 90 天 | `shared/constants.rs:276` |
| 权限码三方一致纪律 | `V0065__seed_batchb_permissions.up.sql:11-15` |
| 新代码只进 `business/` | `business/mod.rs:1-5`；批次 B 先例 `business/masking` 用 `system:field-mask:*` |
| 路由宏收集（无需手工注册） | `src/lib.rs:26-28,57` |
| 中间件在链最外层 | `src/lib.rs:35-37` |
| 事务内写多表的范式 | `system/user/handler.rs:269-276`；`business/notice/model.rs:746` |
| 分区表主键必须含分区键 | POC-9（真报错） |
| DEFAULT 分区会阻断新分区创建 | POC-10（112 161 行致失败） |
| 中文 tsvector 无分词 → 子串检索失效 | POC-6 |
| trgm 仅对 ≥3 字有效，2 字更慢 | POC-7 |
| 2000 篇文档 ILIKE 4–6 ms | POC-7 |
| 公告可见性 0.258 ms / 未读数 1.515 ms | POC-8 |
| 变更日志 18.8 µs/行；对象历史 0.065 ms | POC-11 |
| 大字段双快照 51 KB/行 vs 摘要化 2.7 KB/行 | POC-5 |
| DROP PARTITION 1.5 ms vs DELETE 37.9 ms | POC-12 |
| 回滚后异步日志残留 / 同事务日志不残留 | POC-4 |
| heading 的 `id` 默认被剥离、加白后保留（TOC 依赖它） | POC-13 |
