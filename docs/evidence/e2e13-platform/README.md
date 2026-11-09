# E2E-13 平台功能域演练证据（导入导出 / 文件 / 字典 / 参数 / 日志 / 定时任务 / 在线用户 / 巡检 / 存储 / 帮助中心）

环境：**145 测试机** · 直连后端 `http://127.0.0.1:8033` · 库 `cloudpivot_dev` · 账号 `admin`（口令取自 `/etc/cloudpivot/cloudpivot-dev.env` 的 `NIK_ADMIN_PASSWORD`）
二进制：`e567c802`（本卡修复后重编，`systemctl restart cloudpivot-test`）
取证时间：2026-11-09 13:3x–13:4x CST
断言：**A 段 151 PASS / 0 FAIL + B 段 135 PASS / 0 FAIL = 286 断言全绿**；DB 勾稽 12 组（`db_evidence.out`）
未连 144 生产；未 DROP / TRUNCATE；未改已应用迁移；演练数据自清残留全 0（`cleanup_after.out`）

## 一、演练范围（12 个页面 / 能力）

| # | 页面 | 覆盖内容 |
|---|------|----------|
| ① | 导入向导 | 定义清单 `def/list`、字段说明 `def/columns`、模板下载（三 sheet）、上传预检、结构校验、确认入库 |
| ② | 导入任务 | `import/page`（关键词/状态/业务过滤）、详情、错误明细分页、错误行 xlsx、`import/{id}/download` 别名、重复确认 |
| ③ | 导出任务 | 同步导出（表头/状态翻译/非法筛选 422）、异步导出建任务→worker→进度→下载 SHA-256→取消/重试/删除、2000 行大批量 |
| ④ | 文件管理 | `support/file` 上传/内容秒传预检/分页/详情/下载头/删除（软删）/`.exe` 黑名单/越权 403 |
| ⑤ | 字典管理 | 类型与字典项 CRUD、重复编码/重复值 409、类型下有项 409、缓存写后立即失效（新增/改/停用/删类型） |
| ⑥ | 参数配置 | 新增/改值即时生效（不重启、不 reload）、`config/value/{key}`、`config/public`（`public.` 前缀）、不可改参数 403、reload、删除后缓存失效 |
| ⑦ | 操作日志 | 分页/按 path+username 过滤/状态码过滤/详情扩展字段、审计·登录·安全·密码四类分页、按 operation 过滤、保留期清理（自造 200 天前日志被删、当日日志保留） |
| ⑧ | 定时任务 | 建/改/启停（cron 变更、停用清空排期）/手动执行一次（同步等待）/执行日志分页+详情/未注册处理器 422/非法 cron/非法状态值 |
| ⑨ | 在线用户 | 列表（token_id/ip/login_time/expire_at）、按用户名过滤、强制下线后旧 token **立即 401**、管理端会话不受影响、重复下线 404 |
| ⑩ | 账务巡检 | 看板（latest/trend/schedule）、手动执行（80 项检查，20 项失败）、失败项带明细、**巡检报的「两账不符组数」用独立 SQL 复算一致**、历史分页、落库 MANUAL |
| ⑪ | 存储管理 | 配置分页/新建 LOCAL 后端/SSRF 防护（回环 endpoint 400+60009）/连通性探测+健康状态回写/按类型过滤/策略分页+可写+非法值拒/上传·预览下载·重登记·软删/默认后端不可删 409 |
| ⑫ | 帮助中心 | 分类建/非法编码/树、文档草稿→发布→检索→阅读计数（同日去重）→编辑退回 DRAFT→版本列表→**回滚到首版正文复原**→删除；草稿对无管理权限用户不可见 |

## 二、结果总览（关键步骤）

| 步骤 | 操作 | HTTP | DB 状态 | 结论 |
|---|---|---|---|---|
| 导入-模板 | `GET /api/business/impexp/import/template?biz_type=system.dict_item` | 200 | — | 三 sheet（数据/字段说明/填写须知）、表头 `字典编码*`、示例行、`filename*=UTF-8''` |
| 导入-预检 | `POST .../import/upload?biz_type=system.dict_item&mode=STRICT`（5 行干净） | 200 | `sys_import_task` VALIDATED，total/valid/error=5/5/0，`file_sha256` 与上传字节一致 | 外键 SQL 次数=1（=外键列数） |
| 导入-入库 | `POST .../import/{id}/confirm` | 200 | SUCCESS、imported_rows=5、progress=100、`finished_at` 非空；`sys_dict_item` 5 行 | 幂等重导仍 5 行 |
| 导入-部分 | `POST .../import/upload?biz_type=business.erp.unit&mode=PARTIAL`（2 合法 + 6 错行） | 200 | 预检 total=8 / valid=3 / error=5（第 9 行「基准单位不存在」校验期看不出） | 错误行号 4–8 与 Excel 行号一致 |
| 导入-部分入库 | `POST .../import/{id}/confirm` | 200 | **PARTIAL、imported_rows=2**、`biz_unit` 2 行、错误编码 0 行落库、错误明细 6 条（含入库失败行 9） | 见「修复 1」 |
| 导入-STRICT | 同一坏文件 `mode=STRICT` + confirm | 422 / 60014 | FAILED、imported=0、库内行数未变 | 整批中止 |
| 导出-同步 | `GET /api/business/impexp/export/sync?biz_type=system.dict_item&keyword=E2E13` | 200 | 行数 = 库内（5），状态列「启用/停用」 | `erp.stock_in&status=1` → 422 点名 `status_code` |
| 导出-异步 | `POST /api/business/impexp/export` → worker | 200 | SUCCESS、progress=100、`exported_rows`=库内全量、`file_sha256` 64 位、下载字节 SHA 一致 | 2005 行产物分块拼接无丢行 |
| 导出-取消/删除 | `POST .../export/{id}/cancel`、`DELETE .../export/{id}` | 200 | CANCELED；删除后 404 + `deleted_at` 非空 | — |
| 文件-上传/下载 | `POST /api/support/file/upload`（png）→ `GET .../download/{id}` | 200 | `sys_file` 行 1 条，`file_key`/`uploader_name`/`url` 齐；字节一致 | `nosniff` + `filename*` |
| 文件-秒传/删除 | `GET .../check-duplicate?file_hash=` / `DELETE` | 200 | `duplicated=true`；软删后详情 404 | `.exe` → 400 |
| 字典 | 类型/字典项 CRUD | 200/409 | 类型下有项删除 409；缓存写后立即失效（下拉立刻只回启用项） | — |
| 参数 | 建 → 改 v1→v2 → 读 → 删 | 200 | `sys_config` 落 v2，`config/value` 立刻 v2（无重载）；删后 `data=null` | `public.e2e13.demo` 在匿名接口 p1→p2 |
| 日志 | 分页/过滤/详情/四类/清理 | 200 | `removed≥1`，自造 200 天前日志被删；当日日志保留 | — |
| 定时任务 | 建 → 停用 → 启用 → 手动执行 → 删除 | 200 | 停用 `next_run_at=NULL`、启用后**立刻有下次执行时间**、`last_run_at` 非空、`sys_job_log` 1 条成功 | 见「修复 2」 |
| 在线用户 | 第二账号登录 → 强制下线 | 200 | 列表出现/消失；被下线 token → **401** | 管理端 200 |
| 巡检 | `POST /api/support/invariant/run` | 200 | `sys_invariant_result` MANUAL、total=80=passed60+failed20、failures=20 | 「B1 两账不符组数」独立 SQL 复算一致 |
| 存储 | 建 LOCAL → 探测 → 上传 → 下载 → 删 | 200 | `health_status=HEALTHY`；默认后端删除 409 | S3 回环 endpoint 400+60009 |
| 帮助 | 建分类/文档 → 发布 → 检索 → 阅读 → 编辑 → 回滚 → 删 | 200 | `PUBLISHED`→编辑退回 `DRAFT`；version 3 = 版本表最大 3；回滚后正文复原 | 草稿对无权用户 404/403 |

## 三、勾稽验证（`db_evidence.out` 12 组，全部 0 差异）

| 组 | 勾稽内容 | 结果 |
|---|---|---|
| G1 | 导入任务状态分布 + `valid_rows+error_rows<=total_rows` | FAILED 2 / PARTIAL 1 / SUCCESS 3，自洽违例 **0** |
| G2 | 任务 × 错误明细条数（错误行逐条可查、行号在真实区间） | PARTIAL 任务明细 6 条（5 校验 + 1 入库失败） |
| G3 | 字典项落库 vs 导入声明 | v 批 5 行 / 去重 5；big 批 2000 行 / 去重 2000 |
| G4 | PARTIAL：错误编码 0 行落库、合法行全落、基准单位解析正确 | 2 / 0 / 1 |
| G5 | 任务 `imported_rows` = 实际库内行数 | 2 = 2 |
| G6 | 导出任务 ↔ 产物（progress=100、行数相等、SHA 64 位、有路径与失效时间） | 5/5、2005/2005，CANCELED 无产物 |
| G7 | 定时任务 ↔ 执行日志 | 有上次执行 t、日志 1 条、最近状态 1 |
| G8 | 巡检记录计数自洽 + 失败项数 | total 80 = 60+20，failures 20 |
| G9 | 参数删除后仅软删留痕 | 2 行均 `deleted_at` 非空 |
| G10 | 帮助文档 `version` = 版本表最大版本 | 3 = 3 |
| G11 | 平台域写操作在 `operation_logs` 留痕 | 12 类路径，最小状态码 200 |
| G12 | `audit_logs` 覆盖关键动作 | 10 类：FILE_UPLOAD 49 / CONFIG_UPDATE 33 / JOB_* 等 |

## 四、问题清单

| # | 问题 | 级别 | 复现步骤 | 影响 | 建议 |
|---|---|---|---|---|---|
| 1 | **PARTIAL 导入整批静默丢行**：`persist` 整批失败时只 `tracing::warn!` 一句，接口仍回 `PARTIAL + imported_rows=0`，文案只说「校验失败 N 行已跳过」，那批合法行既没进库也不在失败明细里，用户没有任何补救入口 | **P1** | `mode=PARTIAL` 上传 8 行（1 行「基准单位不存在」只有入库期才失败）→ confirm → 见 `imported_rows=0` | 最多 `batch_size`(=2000) 行合法数据被吞，且提示与实际原因不符（AGENTS.md 约定 11 同类：事务提交后失败必须可恢复/如实回报） | **本卡已修**（见修复 1） |
| 2 | **定时任务 `next_run_at` 长期为空**：新建 / 改 cron / 停用后再启用的任务，列表页「下次执行时间」列一直空，直到任务真跑过一次（`Scheduler::touch_job` 才回填） | P3 | 建任务 → 详情/列表看 `next_run_at=null`；停用→启用后仍 null（探测 5s 内不变） | 运维无法判断任务是否真的排上；停用期间还留着过期时间 | **本卡已修**（见修复 2） |
| 3 | **字典项删除无引用校验**：`DELETE /support/dict/item/{id}` 只做软删，没有「被业务引用则拒」的检查；只有**类型级**做了「下有字典项 → 409」。卡面要求「字典项被引用后不能删」在实现里没有对应逻辑（库里也没有引用关系表） | P3 | 建字典项 → 删除 → 200 软删（任何业务侧 JSON 里写死的 `dict_code+value` 会静默失配） | 下拉/标签渲染降级成空值，历史单据文案丢失 | 先明确引用来源（业务表 / 配置 JSON），再做「引用计数 → 409」；或至少在删除审计里回显被引用的配置键 |
| 4 | **修改已发布帮助文档时状态机副作用**：`PUT /business/help/doc/{id}` 会把 `PUBLISHED` 退回 `DRAFT` 并清空 `published_at`（设计如此），但响应里没有「已下线」提示，前端若不刷新列表会误以为文档仍在线 | P4 | 编辑已发布文档 → 详情 `status=DRAFT`、`published_at=null` | 读者视角文档突然消失，作者不知情 | 响应加 `status_changed_to` 或前端在保存后提示「已退回草稿，需重新发布」 |
| 5 | **导出任务 `attempt_count` 恒为 0**：`claim_export_task` 不递增 `attempt_count`，只有失败重试路径才 +1 → 成功任务「已尝试次数」显示 0 | P4 | 建导出任务 → worker 成功 → `sys_export_task.attempt_count=0` | 该列对成功任务永远误导（0 次尝试却有产物） | 认领 SQL 里 `attempt_count = attempt_count + 1` |
| 6 | **导入「下载」别名语义反直觉**：`/impexp/import/{id}/download` 实际是**错误明细 xlsx**（与 `/errors/file` 同一实现），不是上传原件；干净任务也会下到一个只有表头的文件 | P4 | 干净导入任务 → `GET .../import/{id}/download` → 三列空表 | 调用方按名字以为拿原件 | 文档已在注释里说明是别名；建议再补一条「下载原件」接口或把别名改名 `/errors/download` |
| 7 | **字典项导入不支持 PARTIAL**：`system.dict_item` 的 `allow_partial=false`（PRD D6 固定关闭），`mode=PARTIAL` → 400 + 10001 | P4（设计如此，记录在案） | 上传字典项文件带 `mode=PARTIAL` | 字典项只能整批成功/整批失败 | 页面文案明示「该业务不支持部分导入」，别让用户以为勾错了 |

> 说明：本次没有 P0 / P2；P1 已当场修复并回归。

## 五、修复记录

| 缺陷 | commit | 文件 | 改法 |
|---|---|---|---|
| 1. PARTIAL 整批丢行 | `dacea40a` | `backend/src/business/impexp/handler.rs` | 整批 `persist` 失败时**降级为逐行入库**：能进的照常进，进不去的行写入 `sys_import_task_error`（失败明细页可见，带 Excel 行号与原因），`status` 依「校验失败 + 入库失败」任一非零判 PARTIAL，文案如实区分「校验失败 N 行 / 入库失败 M 行」 |
| 2. 定时任务下次执行时间 | `e567c802` | `backend/src/support/job/handler.rs`、`backend/src/support/job/model.rs` | 新增 `model::refresh_next_run_at(id, Option<cron>)`，用与调度器**同一个** `CronSchedule` 预排期；新建/更新/启停三条路径都调用（停用写 NULL），并把结果回填到响应体 |

回归方式：`cargo check` + `cargo clippy -p cloudpivot`（本卡改动零新告警）→ 145 `git pull` + `cargo build` + `systemctl restart cloudpivot-test` → A/B 两段脚本重跑（286 断言全绿）。
修复涉及 **3 个源文件**（缺陷 1 = 1 个，缺陷 2 = 2 个；单缺陷均 ≤2 文件）。

## 六、文件清单

```
scripts/verify/e2e13_lib.py          公共库（HTTP/psql/xlsx 读写/轮询/断言）
scripts/verify/e2e13_a.py            A 段：导入导出 + 文件管理（151 断言）
scripts/verify/e2e13_b.py            B 段：字典/参数/日志/定时任务/在线/巡检/存储/帮助（135 断言）
scripts/verify/e2e13_db_evidence.sh  DB 勾稽 12 组（只读）
scripts/verify/e2e13_cleanup.sql     自清 SQL（只删 E2E13 前缀自有数据）
scripts/verify/e2e13_cleanup.sh      自清 + 残留核对
docs/evidence/e2e13-platform/        run_a.out / run_b.out / asserts_a.log / asserts_b.log /
                                    db_evidence.out / cleanup_after.out / raw_a/ raw_b/
```

## 七、结论

**全部通畅**（12 个页面 / 能力全链走通；286 断言 0 FAIL；DB 勾稽 12 组 0 差异）。本卡修复 1 条 P1、1 条 P3，遗留 P3×1 + P4×4 已列入问题清单。
