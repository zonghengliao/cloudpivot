# AUD-02 · 上传附件审计行不落库（DOC_ATTACHMENT_UPLOAD 改直写）

卡片：`t_d7e1ba45`　提交：`9201ed4`　日期：2026-11-09（CST）
范围：`backend/src/business/attachment/{handler,tests}.rs`，迁移 **0 条**。
未碰 144、未自动部署生产；145 仅按既有口径更新 `cloudpivot-test`（debug）。

## 1. 症状与原因

ATT-04 在 145 用真 token 上传 8 次（4 成功 / 2×403 / 1×400 / 1×404）后查库：

```sql
select operation, count(*) from operation_logs
 where created_at > now() - interval '45 minutes' and operation like 'DOC_ATTACHMENT%'
 group by 1;
-- DOC_ATTACHMENT_DELETE | 3     ← 删除是直写，落了
-- （无 DOC_ATTACHMENT_UPLOAD）
```

原因与 ATT-03 修删除时**逐字相同**：`handler::upload` 走 `log::record_audit(...)` →
日志总线（500 ms 批量）；进程里总线未初始化/未刷盘时事件静默消失，剩下的只有全局中间件
那条 `POST /api/business/attachment/upload`（无单据类型 / 文件名 / 大小）。
`upload` 是附件模块里**唯一还走总线**的写操作。

## 2. 修法

照抄 `download_log` / `delete_log` 的既有范式（同文件内已存在，ATT-02 / ATT-03 建立）：

| 项 | 内容 |
|---|---|
| 纯函数 | `handler::upload_log(headers, connect_info, claims, &UploadLogFacts, duration_ms) -> OperationLogModel` |
| 直写 | `handler::record_upload_log(...)` → `OperationLogModel::insert(db())`，失败只 `warn` |
| 事实结构 | `UploadLogFacts`（binding_id / doc_type / doc_id / file_id / file_role / file_name / submitted_name / file_size / storage_code / duplicated），避免 10 个位置参数错位 |
| operation | `DOC_ATTACHMENT_UPLOAD`（`handler::OP_UPLOAD`） |
| target | `DocAttachment` / **绑定行 id**（不是 file_id / doc_id） |
| status_code | `200`，`request_method=POST`，`request_path=/api/business/attachment/upload` |
| 旁路语义 | 写库失败只 warn —— 留痕不能反压上传（与下载 / 删除同口径） |

`detail` 九个键，且**两个文件名键都给**：

```json
{"doc_type":"CONTRACT","doc_id":"…","file_id":"…","file_role":"CONTRACT_SCAN",
 "file_name":"aud02_052922.pdf","original_name":"aud02_052922.pdf",
 "file_size":92,"storage_code":"local-default","duplicated":false}
```

- `file_name` = 落库显示名（`sys_file.original_name`，与列表 / 下载 / 删除留痕同一口径，
  秒传时是**首个**上传者的文件名）；
- `original_name` = 本次 multipart 提交的文件名（秒传时与 `file_name` 不同 —— 这个差异
  本身说明「本次没写物理对象」）。

旧实现**只有 `original_name`**，ATT-04 的清理脚本按 `detail->>'file_name'` 检索一条都查不到 ——
现在两个键都能命中（下面 145 实测各 1 条）。

顺带把 `upload` 的响应体 `file_name` 与留痕抽成同一个局部变量，保证「界面看到的」=「审计记下的」。

## 3. 单测

| 环境 | 命令 | 结果 |
|---|---|---|
| 99 纯逻辑 | `cargo test --lib business::attachment -- --test-threads=1` | **91 passed / 0 failed**（0.03s）→ `00_99_pure.txt` |
| 99 全量 lib | `cargo test --lib -- --test-threads=4`（无 `ERP_DB_URL`） | **3854 passed / 0 failed** → `01_99_full.txt` |
| 99 → 145 `cloudpivot_dev`（`ssh -L 15432` 隧道，真库） | 同第 1 行 + `ERP_DB_URL` | **91 passed / 0 failed（41.37s，无 SKIP）** → `02_145db_attachment.txt` |

新增 2 条（`tests.rs`）：

1. `upload_log_fields`（纯逻辑）—— 操作码 / target=绑定行 / 方法 / 路径 / 状态码 / 耗时 /
   用户与账号 / request_id / UA / IP / `response_data.id`，以及 detail 九个键逐字断言；
   另断言**秒传场景下 `file_name` 与 `original_name` 可分辨**（`duplicated=true`）。
2. `db_upload_log_lands_in_operation_logs`（真库）—— 走
   `OperationLogModel::insert(db())` 真写，再用 SQL 读回 `operation / target_type / target_id /
   status_code / request_method / request_path / username / detail->>'doc_type' /
   detail->>'file_name' / detail->>'original_name' / detail->>'storage_code' /
   (detail->>'file_size')::bigint / (detail->>'duplicated')::boolean / host(ip)`，
   证明 `INET` 与 `JSONB` 绑定真的写得进（这类错误编译期看不出来，且写失败会被 warn 吞掉）。

手写 INSERT 是不允许的：`ip` 是 `INET`、`detail` 是 `JSONB`，框架 `TypeMapper` 才处理得对。

## 4. 145 真机（真 token，两次独立跑）

部署：`12_145_deploy.txt` —— `git pull --ff-only` 到 `9201ed4` →
`cargo build --bin cloudpivot`（1m38s，`BUILD_EXIT=0`）→ `systemctl restart cloudpivot-test`
→ `active`，二进制 md5 `28cf2dfcf4fe7beb724d0c685cd558e7`。
新代码在二进制里的证据：`grep -ac "单据附件上传留痕写库失败" target/debug/cloudpivot` = 1。

脚本：登录真 token（`NIK_ADMIN_PASSWORD` 现取，不落仓库）→ `POST /api/business/attachment/upload`
（`doc_type=CONTRACT`，真实合同 id，`file_role=CONTRACT_SCAN`，唯一的 92 字节 PDF）→
**紧接着（无 sleep）** 查 `operation_logs`。

### 第一跑（`10_145_verify_run1.txt`）

```
BINDING=01a1bd10-b601-767a-b367-6600a9354f5b FILE_ID=01a1bd10-b600-7652-a9ea-8384b7690369
 DOC_ATTACHMENT_UPLOAD | DocAttachment | 01a1bd10-b601-… | 200 | POST |
 /api/business/attachment/upload | admin | 10 ms | CONTRACT | … | CONTRACT_SCAN |
 aud02_052911.pdf | aud02_052911.pdf | 92 | local-default | false
AUDIT_ROWS=1
file_name_hits=1
original_name_hits=1
residue=0|0|0|0
```

### 第二跑（`11_145_verify_run2.txt`，补 `X-Real-IP` 证明 IP 也落）

```
 DOC_ATTACHMENT_UPLOAD | DocAttachment | 01a1bd10-e432-… | 200 | POST |
 /api/business/attachment/upload | admin | 4 ms | 10.9.8.11 | CONTRACT | … |
 aud02_052922.pdf | aud02_052922.pdf | 92 | local-default | false
AUDIT_ROWS=1
file_name_hits=1
original_name_hits=1
== 中间件通用流水对照 ==
DOC_ATTACHMENT_UPLOAD x1
POST /api/business/attachment/upload x1
```

- 两条都留：中间件是「HTTP 层面发生了什么」，本条是「业务层面谁往哪张单挂了哪个附件」；
- 第一跑 `ip` 为空是本地 curl 没有代理头（`utils::client_ip` 过滤未指定地址），
  第二跑带 `X-Real-IP` 即落 `10.9.8.11` —— 与删除 / 下载留痕同一个 helper，口径一致；
- 生产经 nginx 转发时 `X-Real-IP` 总在，不是缺陷。

### 夹具清理

两跑各自清理，残留一律 `0|0|0|0`（绑定行 / 留痕行 / `sys_file` / 按唯一文件名再查一遍），
临时 PDF 已删。合同是库里既有的真实单据，未改。

## 5. 质量门

- `rustfmt +1.94.1 --edition 2024 --check` → `FMT_EXIT=0`（`03_fmt.txt`）；
- `cargo clippy --lib --tests` → `CLIPPY_EXIT=0`，输出里 `attachment/handler.rs` 与
  `attachment/tests.rs` **零告警**（`04_clippy.txt`；仓内既有告警属他人文件：
  `flow/edge_admin_tests.rs`、`attachment/rule.rs`、`erp/asset_linkage.rs`、`erp/fx_revaluation.rs`）。

## 6. 残留风险

- 被 400 / 403 / 404 拦下的上传**不会**有这条结构化行（没有绑定行可指），只在中间件流水里
  留下状态码 —— 与「留痕只记成功业务动作」的口径一致；要审计失败尝试得另开一卡（会引入
  「谁在探测某单据」的可疑流量记录，涉及隐私取舍，不在本卡范围）。
- `duplicated=true`（秒传）时 `file_id` 指向**别人的** `sys_file` 记录，`detail` 里已用
  `duplicated` 标记，按 `file_id` 反查会看到首个上传者 —— 这是秒传本身的设计，非本卡引入。
- 排班/任务/job 等其它模块的 `log::record_audit` 仍走总线，是否有同类丢事件不在本卡范围。

## 7. 提交

| 提交 | 内容 |
|---|---|
| `9201ed4` | 功能 + 测试（`attachment/handler.rs`、`attachment/tests.rs`）—— 145 上跑的就是这一个 |
| 本提交（docs） | `attachment/mod.rs` 模块文档（三条写路径的留痕表）+ 本目录证据 |

145 的 `cloudpivot-test` 停在 `9201ed4`（功能代码完整）；docs 提交只改注释与 markdown，
再 `git pull` 也不需要重新验证行为（与 ATT-04 同一处理方式）。
