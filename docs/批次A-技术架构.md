# 云枢「批次 A · 通用底座」技术架构设计
## 对象存储扩展 · 导入导出框架 · 消息通知

> **代码基线**：`/root/work/c144`（`backend` = crate `cailu` v0.1.0 / edition 2024 / rustc ≥ 1.94.1，前端 `@cailu/web` v0.1.0 = Vue 3.5.32 + TDesign 1.19.1）
> **框架**：nik 0.4.2（门面 crate，`use nik::prelude::*`）
> **数据库 / 缓存**：PostgreSQL 18.6、Redis 8.0.5（本机实测版本，见 §1.3）
> **迁移占用**：`V0040`–`V0049`（V0030–V0039 已被 ERP 批次 1 设计预留，不复用）
> **文档版本**：v1.0 / 2026-10-28　**状态**：设计定稿，可直接作为实施依据
> **范围**：只含批次 A（A→B→C→D 中的 A）。B（数据权限/列级脱敏/角色互斥）、C（公告/帮助/变更日志）、D（工作流）只在「接口预留」层面出现。

---

## 0. 决策摘要（先看这一页）

本批所有结论都基于**对现有代码的实测**与 **6 组真实 POC**（§1.3、§8），不是通用后台教科书。

### 0.1 十二条硬结论

| # | 决策点 | 结论 | 一句话理由（含证据） |
|---|--------|------|---------------------|
| D1 | 对象存储后端 SDK | **不新增依赖**：复用框架 `nik-fs`（`fs-local` + `fs-minio`，后者内部就是 `aws-sdk-s3 1.149`） | POC-1 实测 `fs`+`fs-local`+`fs-minio` 与 `xlsx`/`sse`/`template`/`xss` 同时开启可编译；项目「只声明 nik」铁律无需破例 |
| D2 | 多后端抽象 | 业务侧自建 `ObjectStore` trait + `StorageRouter`（键 = `config_code`），**包装**而非替换 `nik-fs` | 框架 `BackendRegistry` 以 `backend_type` 为键（`HashMap<String, _>`），**同类型多桶/多账号无法共存**（读源码 `nik-fs/src/registry.rs:22`）；且框架缺 `presign_upload` |
| D3 | 预签名上传（PUT） | 自研 **SigV4 query signing**（≈150 行，仅用项目已有直接依赖 `sha2`） | 框架 `StorageBackend` 只有 `presign_download_url`（本地返回空串）；POC-5 实测自研签名与 `aws-sdk-s3` 官方预签名**逐字节一致** |
| D4 | AK/SK 存储 | **独立主密钥**（环境变量 `CLOUDPIVOT_STORAGE_MASTER_KEY`，32 字节）+ `Crypto::aes_encrypt` AES-256-GCM，密文 + 独立 nonce + `key_version` 入列 | 复用 `NIK_MIDDLEWARE_AUTH_JWT_SECRET` 会让「JWT 轮换 = 全部存储密钥失效」（该密钥有启动守卫强制轮换，见 `src/shared/startup_guard.rs`）；POC-3 实测加解密与错钥拒绝 |
| D5 | Excel 读写库 | **用框架 `xlsx` feature**（`Export::to_xlsx_*` = rust_xlsxwriter，`Import::from_xlsx_*` = calamine），不直接声明 `calamine`/`rust_xlsxwriter`/`umya` | 选型结论已被框架固化（`nik-utils/src/export.rs`）；POC-2 实测 10 万行×4 列：写 5.24 s / 读 3.00 s / 峰值 RSS 220 MB / 文件 2.3 MB |
| D6 | 大文件导入 | 三档：**≤5 000 行同步**；5 000–200 000 行**异步任务 + 分批入库（2 000 行/事务）**；>200 000 行**强制 CSV 流式**（不进内存） | 框架 xlsx 解析是「整本入内存」（`Import::from_xlsx(data: &[u8])` + calamine `Range`），10 万行已 220 MB，20 万行接近 450 MB，必须设闸 |
| D7 | 错误明细存储 | **JSONB 摘要（前 100 条）+ `sys_import_task_error` 明细表** 双写 | 列表页要「一眼看到错在哪」不能只靠明细表 join；但明细要分页、要导出错误行、要按错误码统计，JSONB 做不了索引统计 |
| D8 | 通知扇出 | **一条 `INSERT ... SELECT` 落 `sys_notice_receiver`**，配 `UNIQUE(notice_id,user_id)` + `ON CONFLICT DO NOTHING`；部门含下级用**递归 CTE**（与 `system/dept/model.rs::find_dept_subtree_ids` 同款） | POC-6 实测 10 003 接收人扇出 **91.5 ms**；重放幂等由唯一索引兜底 |
| D9 | 未读数查询 | 部分索引 `(user_id, notice_id) WHERE read_flag=FALSE AND is_deleted=FALSE` | POC-6 实测 `count(*)` 走 **Bitmap Heap Scan，0.55 ms**（1 万接收人表） |
| D10 | 实时推送 | **上 SSE**（框架 `sse` feature 已内置 `Broadcaster`），**不上 WebSocket**；但按用户拆分 broadcaster | 框架文档明说 SSE「天然复用 AuthLayer」而 WebSocket 不能带 `Authorization` 头；全局 `Broadcaster` 会把 A 用户的待办 id 推给 B 用户（越权泄露），必须 per-user 频道 |
| D11 | 通知模板渲染 | **自研 `{{var}}` 替换器（≈80 行）+ 变量声明校验**，不用 `nik-template` | `render_template(name, ctx)` 只能渲染「文件系统里的命名模板」（`nik-web/src/resources.rs:212`），而订阅模板存在数据库行里；引入 Jinja 类引擎等于给运维开一个任意表达式入口 |
| D12 | 兼容现有 `support/file` | **旧接口一行不改**，新增 `business/storage/*` 并行；旧接口固定走本地后端，新接口按策略路由 | `support/file/handler.rs` 697 行含完整审计/秒传/下载头逻辑，重写=回归风险；组合而非替换，见 §2.7 的兼容矩阵 |

### 0.2 与任务书假设的三处偏差（必须先对齐）

| 任务书假设 | 代码实况（证据） | 处理 |
|---|---|---|
| 「Rust 侧 SDK：`aws-sdk-s3` vs `rust-s3` vs 手写签名」需二选一 | `nik` 的 `fs-minio` feature 内部**已经引入 `aws-sdk-s3 1.149` + `aws-config 1.12`**（`nik-fs-0.4.2/Cargo.toml`），但只暴露 `write/read/delete/exists/presign_download_url` | 选型表照给（§2.2），但结论是「用框架封装 + 自研预签名上传」，不新增 SDK 依赖 |
| 「大文件浏览器直传 avoid 走后端内存」 | 框架 `StorageBackend` 只有下载预签名；上传预签名完全没有 | 自研 SigV4（D3），并用 `aws-sdk-s3` 交叉验证签名（POC-5） |
| 「模板引擎 vs 简单替换」二选一 | 框架 `template` feature 是**文件模板**（`render_template(name)`），不渲染字符串 | 结论：简单替换 + 严格变量校验（D11），并把 `template` feature 是否开启的决定交给 C 批次（公告正文需要富文本渲染时再评估） |

### 0.3 本批交付边界（明确不做）

- ❌ 邮件 / Webhook / 短信的**真实发送**：只落 `sys_notice_delivery` 状态机 + 通道配置表 + trait，不接网关（框架 `NotificationPlugin` 的 SMTP 能力保留，C/D 批次启用）
- ❌ 文件物理对象的多副本/跨后端同步复制（只做「按后端路由 + 跨后端秒传复用」，不做镜像）
- ❌ 导入的「断点续传」与「行级回滚」（事务粒度是批次，不是行；见 §3.4）
- ❌ 缩略图/图片水印/病毒扫描（风险清单里列为 B 批次的伴生需求）
- ❌ 前端富文本编辑器选型（通知正文本批为 TEXT/HTML 字符串，编辑器随 C 批次公告一起定）

---

## 1. 现状核查（设计的事实基础）

本章每条结论都指向真实文件/真实命令输出；实施时若发现不符，以代码为准并回改本文档。

### 1.1 模块与文件布局

```
/root/work/c144/backend/src/
├── system/{auth,dept,menu,role,user}/      # 底座：只读（除批准的最小改动）
├── support/{config,dashboard,dict,file,health.rs,ipgeo.rs,job,log,online}/
├── business/monitor/{mod,handler,dto,model}.rs   # 二开范本，四件套、无 service 层
├── task/mod.rs                             # 调度器单例 + 内置任务注册（match invoke_target）
├── shared/{constants,enums,error_code,guard,startup_guard}.rs
└── utils/mod.rs                            # require_uuid / normalize_page / db_error / current_user_uuid ...
```

前端：`frontend/src/{api,views}/{system,support,business}/`，路由在 `router/index.ts` 静态声明（`meta.permission` 对齐后端权限码）。

### 1.2 `support/file` 现有能力实测（`handler.rs` 697 行逐行读过）

| 能力 | 路由 | 实现要点 | 对本批的影响 |
|---|---|---|---|
| 上传 | `POST /support/file/upload`（`support:file:upload`） | multipart 字段名固定 `file`；大小上限取 `cfg().upload.max_size_mb`；扩展名白名单为**编译期常量** `ALLOWED_UPLOAD_EXTENSIONS`（31 项）；SHA-256 秒传命中直接复用旧记录 | 上传策略要按 category 可配 → 必须外置到表；常量白名单保留为「兜底」 |
| 批量上传 | `POST /support/file/upload/batch` | 循环调用同一 `store_file_bytes` | 同上 |
| 秒传预检 | `GET /support/file/check-duplicate?file_hash=` | `SELECT ... WHERE file_hash=? AND deleted_at IS NULL AND status=1 LIMIT 1` | 与「跨后端秒传」天然契合，只差一个 `storage_code` 回传 |
| 分页/详情/删除/下载 | `GET page` / `GET {id}` / `DELETE {id}` / `GET download/{id}` | **落盘直接 `std::fs::write`**；下载 **`std::fs::read` 全量进内存**；删除时**无条件 `remove_file`** | ①大文件下载 OOM 风险；②多后端必须靠新接口；③删除物理对象会连带影响秒传复用者（§2.8） |

`sys_file` 表（`migrations/support/V0010`）：`file_name, original_name, file_key UNIQUE(部分), file_size, file_hash, mime_type, extension, storage_path, category, status, uploaded_by, 审计列`。**没有后端标识列** → 本批 `V0040` 追加两列（§7）。

### 1.3 POC 实证（6 组，全部真跑，产物见 §8.2 清理说明）

环境事实先行：本机 `rustc` 初始为 **1.94.0**，而 `nik 0.4.2` 声明 `rust-version = 1.94.1`，`cargo` 直接拒绝构建。已 `rustup toolchain install 1.94.1` 后通过。**实施机必须 ≥1.94.1**。

| 编号 | 验证内容 | 结果（真实输出） |
|---|---|---|
| POC-1 | feature 组合 `db postgres cache plugin fs fs-local fs-minio xlsx sse template xss chrono uuid json ipnetwork` | ✅ 编译通过（连带把 `nik-template`、`aws-sdk-s3 1.156` 全树拉通）。**关键坑**：`use nik::nik_fs::...` 需要额外开 `fs` feature（`nik/src/lib.rs:1349` 有 `#[cfg(feature="fs")]`），只开 `fs-local` 会报「could not find nik_fs」 |
| POC-2 | Excel 10 万行×4 列读写 | 写出 2 383 KB，**写 5 243 ms，读 3 004 ms，进程峰值 RSS 220 MB**（`/proc/self/status VmHWM`） |
| POC-3 | `Crypto::aes_encrypt/decrypt`（AES-256-GCM） | ✅ 往返一致；换错密钥解密返回 `None`（GCM 认证生效） |
| POC-4 | `LocalFs::write_at/read/exists/delete/presign_download_url` | ✅ 全通；**本地后端 `presign_download_url` 返回空串**（设计上必须回落 `/download/{id}` 接口） |
| POC-5 | 自研 SigV4 `presign_put_url` vs `aws-sdk-s3` 官方预签名 | ✅ **签名完全一致**（同 AK/SK/时间/有效期下 `X-Amz-Signature` 逐字节相等，多次不同时间戳复现）。唯一差异是 URL 上查询参数书写顺序（SDK 把 `x-id` 放最前），而规范串按参数名排序，故不影响校验 |
| POC-6 | 本批 DDL + 28 条业务规则，真实 PostgreSQL 18.6 | ✅ 全部按设计拦住/放行：默认后端唯一、AK/SK 密文强制、同哈希唯一、已读一致性、扇出幂等、级联删除、状态机 CHECK。**10 003 接收人扇出 91.5 ms**；未读数 `count(*)` 走位图索引 **0.55 ms**（详见 §8.1） |

### 1.4 框架能力清单（决定「不要重复造」的边界）

| 框架能力 | 位置 | 本批怎么用 |
|---|---|---|
| `StorageBackend` trait（write/write_at/read/delete/exists/presign_download_url/health_check）+ `LocalFs` + `MinioBackend` + `BackendRegistry` + `BackendConfig` | `nik-fs 0.4.2` | **包装**成业务 `ObjectStore`；`BackendConfig` 就是我们建表字段的镜像 |
| `Crypto`（sha256/hmac/aes-gcm/random_key/base64url） | `nik-utils::Crypto`（prelude 已导出） | AK/SK 加密、SigV4 的 HMAC 备选、签名回调用 |
| `Export` / `Import` / `XlsxSheet` | `nik-utils::export`（`xlsx` feature） | 导入导出全部 Excel I/O |
| `Broadcaster<T>` + `sse_json` | `nik-web::sse`（`sse` feature，prelude 已导出） | 消息中心未读红点实时推送 |
| `NotificationPlugin`（SMTP/lettre，`is_configured()` 可探测） | `nik-plugin::notification` | 邮件通道「留接口 + 现成实现」，本批只做投递记录不发送 |
| `AsyncTaskPlugin::submit(future)`（信号量限并发） | `nik-plugin::async_task` | 异步导入/导出任务的进程内执行器 |
| `SchedulerPlugin::register(name, cron, desc, closure)` | `nik-plugin::scheduler` | 业务模块**自行**注册清理/重试定时任务，不必改 `src/task/mod.rs` |
| 事务 `db().transaction(|tx| async move { ... })` | `nik-db` | 导入批量入库、扇出、配置保存的事务边界 |
| 迁移编译期快照（解析 `CREATE TABLE` 与 `ALTER TABLE ADD COLUMN` 演进出最终列集） | `nik-db-build` | 允许对底座表做「加列」的最小改动（本批 `sys_file` 加 2 列） |

### 1.5 红线对照（本设计如何不越线）

| 红线 | 本设计的守法方式 |
|---|---|
| 新代码只进 `business/` | 全部新增代码在 `business/{storage,impexp,notice}/`；`src/task/mod.rs`、`support/file/*` **不改**；`shared/error_code.rs` 只**追加**常量（同文件追加，无逻辑改动，需批准） |
| 不设 service 层 | 四件套 `mod/handler/dto/model`；导入导出引擎是 `business/impexp/{engine,registry,definition}` 这类「能力模块」，业务编排仍在 handler |
| 错误三出口 | handler `ResResult<T>`、model `DbResult<T>`、启动/任务 `nik::Result<T>`；引擎内部用 `ApiError`/`DbError` 两态，不引入第三套错误类型 |
| 禁 `unwrap` / 拼 SQL | 所有 SQL 常量字符串 + `Value` 绑定参数；`regex` 校验在 CHECK 里，不在代码里拼串 |
| 动代码必须带单测，`cargo test` 全绿 | §5.4 给出逐模块单测点，含 POC 已验证的「签名一致性」「CHECK 拦截」等高价值用例 |

---

## 2. 对象存储扩展

### 2.1 现状问题清单（在 §1.2 基础上展开）

| 编号 | 问题 | 影响 | 本批处理 |
|---|---|---|---|
| S1 | 单后端硬编码：`nik::upload_path()` + `std::fs` | 无法接 OSS/S3/MinIO，无法多桶 | `StorageRouter` + 配置表 |
| S2 | 上传策略为编译期常量（31 扩展名） | 业务方（如 ERP 导入模板只允许 xlsx）改不了 | `sys_storage_policy` 按 category 配置 |
| S3 | 下载 `std::fs::read` 全量进内存 | 500 MB 文件直接打爆进程 | 新接口 `presign`（S3）/ `tokio::fs` 流式（本地） |
| S4 | 大文件走 multipart 走后端 | 网关/内存双重压力 | 预签名直传（§2.6） |
| S5 | 删除时无条件删物理文件 | 秒传复用者拿到「记录在、文件没了」 | 删除改「软删 + 延迟回收」（§2.8） |
| S6 | 秒传跨后端无定义 | 换默认后端后，老文件全部 404 | `sys_file.storage_code` + 路由回源 |
| S7 | `file_hash` 无唯一约束 | 并发上传同内容 → 两条记录两份对象 | `uk_sys_file_hash` 部分唯一索引（POC-6 T6 验证） |

### 2.2 Rust 侧 SDK 选型对比

> 前提：项目铁律「`Cargo.toml` 只声明 `nik` 一个框架 crate」，任何新依赖都必须有不可替代的理由。

| 维度 | ① `nik-fs`（框架封装，内部 aws-sdk-s3） | ② 直接声明 `aws-sdk-s3` | ③ `rust-s3` | ④ 全手写（SigV4 + reqwest/hyper） |
|---|---|---|---|---|
| 新增依赖 | **0**（只开 feature） | 1 个族（`aws-config/aws-sdk-s3` + ~20 个 smithy 子 crate） | 1 个（`rust-s3`，自身依赖 reqwest + tokio） | 0（签名）+ 上传仍需 HTTP 客户端（axum 已带 hyper，但无 multipart/流式封装） |
| 编译体积/时长 | 已含在 `fs-minio`，**POC-1 实测全量编译可接受** | 与 ① 等价（同一棵树） | 更轻，但引入**第二套** HTTP 客户端栈 | 最轻 |
| 与框架一致性 | ✅ 与 `BackendConfig`/`StorageBackend` 同构 | ❌ 需自己做适配层，且版本可能与 nik 内部锁定的 1.149 冲突 | ❌ 又一套 SDK 风格 | ✅ |
| 预签名下载 | ✅ `presign_download_url` | ✅ | ✅ | 需自写 |
| 预签名上传 | ❌ 没有 | ✅ `put_object().presigned()` | ✅ `presign_put` | 需自写（**本设计采用**） |
| 流式读写 | ❌ 全是 `&[u8]`/`Vec<u8>` | ✅ `ByteStream` | ✅ | 需自写 |
| 多后端多桶 | ❌ `BackendRegistry` 以 `backend_type` 为键，同类型只能一个实例 | ✅ 自由 | ✅ 自由 | ✅ |
| 维护活跃度 | 与框架同版本节奏（0.4.2 / 2026-03） | AWS 官方，最活跃 | 社区维护，S3 兼容性好 | 自己维护 |
| 许可 | Apache-2.0 | Apache-2.0 | MIT | — |

**推荐：① `nik-fs` 作为「对象 I/O 底座」+ ④ 的 SigV4 片段补「预签名上传」**。理由：
1. 上传/下载/删除/存在性/健康检查这类 I/O 已被框架实现且与本项目 `BackendConfig` 字段一一对应（`endpoint/region/access_key/secret_key/use_tls/root_path/is_default`），重写毫无收益；
2. 框架缺的两件事（**上传预签名**、**流式**）中，预签名是**纯字符串签名计算**，用项目已有的 `sha2` 就能做，且已被 POC-5 与官方 SDK 对拍验证；
3. 真正需要「同类型多实例」的场景（两个 OSS 桶、生产桶 + 备份桶）用**业务侧 `StorageRouter` 自己按 `config_code` 持有 `Arc<dyn ObjectStore>`** 解决，不去改框架；
4. 若后期确认要流式大对象（>2 GB），再单独评审「声明 `aws-sdk-s3`」——一条**有边界、可延后**的决策，而不是现在就破坏依赖铁律。

**框架封装的两个坑（实施必读，均为读源码实测）**：
- `MinioBackend::from_config` 会在构造时 `ensure_bucket()`（`head_bucket` 失败就 `create_bucket`）。**AWS S3 / 阿里 OSS 的 AK 通常没有 CreateBucket 权限，且 OSS 的 bucket 必须带 region 域名**——所以接 S3/OSS 时必须**先探测再决定是否 create**，本设计用 `probe_bucket()`：`head_bucket` 成功即用，403/404 只告警不 create（错误信息更准确，也避免越权调用）。
- `force_path_style(config.endpoint.contains("minio") || !config.use_tls)` 是启发式判断，**OSS/S3 用 https 时也会被判成 path-style（错）**。所以配置表必须有显式 `path_style` 列，构造时按列取值，不交给启发式。

### 2.3 trait 抽象与路由

```rust
// business/storage/store.rs —— 业务侧的「对象 I/O 契约」，包装 nik_fs
use nik::prelude::*;

/// 存储后端统一契约：比框架 trait 多两个能力（上传预签名、流式写）
#[async_trait::async_trait]
pub trait ObjectStore: Send + Sync {
    /// 后端类型标识：LOCAL / S3
    fn backend_type(&self) -> &'static str;
    /// 后端编码（sys_storage_config.config_code）
    fn config_code(&self) -> &str;

    async fn put(&self, key: &str, data: &[u8], content_type: &str) -> Result<(), StoreError>;
    async fn get(&self, key: &str) -> Result<Vec<u8>, StoreError>;
    async fn get_stream(&self, key: &str) -> Result<BoxStream<'static, Result<Bytes, StoreError>>, StoreError>;
    async fn delete(&self, key: &str) -> Result<(), StoreError>;
    async fn exists(&self, key: &str) -> bool;
    async fn stat(&self, key: &str) -> Result<Option<ObjectStat>, StoreError>;

    /// 预签名上传（S3 系返回 URL；本地后端返回 Err(Unsupported)）
    async fn presign_put(&self, key: &str, ttl_secs: u64, content_type: Option<&str>)
        -> Result<PresignedUpload, StoreError>;
    /// 预签名下载（本地后端返回 None，由 handler 回落流式下载）
    async fn presign_get(&self, key: &str, ttl_secs: u64) -> Result<Option<String>, StoreError>;

    async fn health(&self) -> Result<(), StoreError>;
}

pub struct ObjectStat { pub size: u64, pub content_type: Option<String>, pub etag: Option<String> }
pub struct PresignedUpload { pub url: String, pub method: &'static str, pub headers: Vec<(String, String)>, pub expire_at: chrono::DateTime<chrono::Utc> }
```

三个实现：
- `LocalStore`：内嵌 `nik_fs::LocalFs`，`presign_*` 返回 `Unsupported`/`None`；路径 = `upload_path()/base_path/key`。
- `S3Store`：内嵌 `nik_fs::MinioBackend`（`write_at/read/delete/exists/presign_download_url/health_check` 直接委托），`presign_put` 调 `sigv4::presign_put_url`。
- `MockStore`（`#[cfg(test)]`）：内存 `BTreeMap`，供 handler 单测不依赖真实 S3。

路由与热更新：

```rust
pub struct StorageRouter {
    /// config_code -> 后端实例
    stores: std::sync::RwLock<std::collections::HashMap<String, std::sync::Arc<dyn ObjectStore>>>,
    default_code: std::sync::RwLock<String>,
}
impl StorageRouter {
    /// 启动时从 sys_storage_config 装载；配置变更后整表重载（原子替换 HashMap）
    pub async fn reload(&self) -> DbResult<()>;
    /// 按 category 策略选后端：策略指定 storage_code > 默认后端
    pub fn pick(&self, policy: &StoragePolicy) -> Result<Arc<dyn ObjectStore>, ApiError>;
    pub fn by_code(&self, code: &str) -> Option<Arc<dyn ObjectStore>>;
}
```

进程级单例：`storage::router()`（`OnceLock<StorageRouter>`），由 `bootstrap::bootstrap` 在 `App::start()` 之后装载（与 `support::log` 插件同寿命），加载失败**不阻断启动**（降级为「仅本地后端可用」+ `health_status` 标 DOWN，登录后由运维页面修）。

### 2.4 存储配置表（DDL 见 §7 附录 A · V0040）

`sys_storage_config` 关键设计点：

| 设计点 | 做法 | 原因 |
|---|---|---|
| 主键 vs 业务键 | `id UUID` 主键 + `config_code`（部分唯一）业务键 | 与底座所有表一致；`config_code` 是唯一会被 `sys_file` 引用的值 |
| 后端类型 | `backend_type IN ('LOCAL','S3')` | 只分两类：S3 兼容（MinIO/AWS/OSS/COS）签名协议相同，靠 `endpoint/region/path_style` 区分，**不为每家厂商建一个枚举**（否则接第 5 家就要改 CHECK 和迁移） |
| 默认后端唯一 | 部分唯一索引 `WHERE is_default AND deleted_at IS NULL` | POC-6 **T1 验证**：插第二个默认被数据库拒绝，不靠应用层「先清空再设置」的竞态代码 |
| AK/SK | `ak_cipher/ak_nonce/sk_cipher/sk_nonce/key_version` | 四列同写同读；`key_version` 支持主密钥轮换 |
| 远程/本地互斥 | `ck_sys_storage_config_remote`：LOCAL 必须无密钥，S3 必须全字段齐 | POC-6 **T2/T3 验证**：既拦住「配了 S3 却忘加密钥」，也拦住「密钥误填到本地行」 |
| 健康状态 | `health_status` + `last_check_at` + `last_error` | 配置页要能一键探测，并把最后一次错误留在行上（不必翻日志） |

### 2.5 AK/SK 加密方案（绝不明文入库）

**结论：独立主密钥 + AES-256-GCM + 逐行 nonce + key_version。**

| 方案 | 安全 | 运维 | 迁移成本 | 结论 |
|---|---|---|---|---|
| A. 复用 `NIK_MIDDLEWARE_AUTH_JWT_SECRET` | 中（同一把密钥两用，泄露面扩大） | 差：JWT 密钥是**启动守卫强制轮换**的对象（`startup_guard.rs`），轮换即全部存储密文失效 | 低 | ❌ |
| B. 明文存库/存配置 | 差 | — | 低 | ❌ 违反任务书硬约束 |
| C. **独立主密钥（env）+ AES-256-GCM + 行级 nonce + key_version** | 高（GCM 自带完整性认证，改一位即解密失败） | 好：主密钥与 JWT 生命周期解耦；轮换只需新增 `key_version=2` 并逐行重加密 | 低 | ✅ **采用** |
| D. KMS/HSM（云 KMS 托管） | 最高 | 需云账号 + 网络 + 计费 | 高 | 延后（本批留 `key_version` 与解密抽象，换成 KMS 只改一个函数） |

落地细节：
- 环境变量 `CLOUDPIVOT_STORAGE_MASTER_KEY`：**64 位十六进制**（32 字节）。启动守卫扩展（`startup_guard.rs` 已是现成范式）：缺失或长度不符 → **进程拒绝启动**，绝不「先启动再用默认密钥」。
- 加密：`Crypto::aes_encrypt(&key, plaintext) -> (ct_b64, nonce_b64)`（POC-3 已验证）。
- AAD 绑定：把 `config_code` 作为附加认证数据参与？——`Crypto::aes_encrypt` 当前签名不支持 AAD。降级做法：**密文列与 `config_code` 同在一行，且 AK/SK 只在该行内使用**；跨行搬运密文（把 A 行的 sk 拷到 B 行）在功能上等价于「管理员改配置」，不构成额外攻击面。此处**坦诚记录为已知局限**，不假装做了 AAD。
- 轮换流程（运维手册级）：`key_version=2` 的主密钥入 env → 后台点「重加密全部配置」→ 逐行 `decrypt(v1) → encrypt(v2)` 事务提交 → 保留 v1 解密能力一个版本周期后移除。
- API 永不下发明文：`GET /business/storage/config/{id}` 返回 `ak_masked`（如 `LTAI****9f2c`）+ `has_secret: true`，前端「只写不读」。

### 2.6 预签名 URL 设计

**上传（浏览器直传，避开后端内存）**

```
① 前端 POST /business/storage/direct/init
   { category:"attachment", file_name:"合同.pdf", file_size: 73400320, file_hash: "<sha256>" }
   → 后端：查策略（大小/扩展名/MIME/是否允许直传）→ 查秒传（file_hash 命中直接返回 200 + file 记录，不签 URL）
   → 未命中：生成 file_key = "{base_path}{yyyy}/{MM}/{dd}/{uuidv7}.{ext}"，签名
   返回 { file_id, file_key, upload_url, method:"PUT", headers:{...}, expire_at, storage_code }

② 浏览器 PUT upload_url（body = 文件本体，带 Content-Length）
③ 前端 POST /business/storage/direct/complete { file_id, file_key, file_size, file_hash }
   → 后端 stat(file_key) 校验：对象存在 + 大小一致 + content_type 在策略内（S3 由 `x-amz-content-sha256` 保证完整性，
      若策略要求强一致，则后端按 1 MB 分块读回算 sha256 比对——大文件可配置「不校验哈希」以换取延迟）
   → 事务内写 sys_file（storage_code / upload_mode='DIRECT'）→ 返回 FileRes
```

**下载（私有桶限时链接）**

| 后端 | 行为 |
|---|---|
| S3 系 | `GET /business/storage/download/{id}` → 302 到 `presign_download_url(ttl)`（ttl 取策略，默认 900 s，上限 7 d） |
| 本地 | 直接流式读（`tokio::fs::File` + `ReaderStream`），**不再 `std::fs::read` 全量进内存** |
| 无权限/无效 | 404 / 403（不泄露对象是否存在） |

**SigV4 实现要点（POC-5 已验证，实施照抄即可）**
1. 规范查询串 5 个固定参数 + `x-id=PutObject`，按参数名排序后做 RFC3986 编码（`/` 在值里编码成 `%2F`，在 key 的路径部分不编码）；
2. 规范请求 `PUT\n<canonical_uri>\n<canonical_query>\nhost:<host>\n\nhost\nUNSIGNED-PAYLOAD`；
3. 签名密钥链 `HMAC(HMAC(HMAC(HMAC("AWS4"+SK, date), region), "s3"), "aws4_request")`；
4. **`host` 必须与真实请求 Host 一致**：path-style（MinIO）为 `endpoint`，virtual-host（S3/OSS）为 `bucket.endpoint`——这是最常见的 403 来源；
5. `Content-Type` 不参与签名（未列入 signedHeaders）→ `headers` 里带 `Content-Type` 只是给对象打标签，不校验，所以必须靠 complete 阶段校验真实类型。

### 2.7 与现有接口的兼容矩阵

| 场景 | 调用方 | 路由 | 说明 |
|---|---|---|---|
| 老前端上传头像/附件 | `POST /support/file/upload` | 本地后端（`upload_path()`） | **接口、响应结构、审计日志全不变** |
| 老前端下载 | `GET /support/file/download/{id}` | 本地后端，整读内存 | 保持；新前端改用新接口 |
| 新前端上传 | `POST /business/storage/upload` | 按 category 策略路由 | 响应体**沿用 `FileRes` 字段**（多返回 `storage_code`、`upload_mode`），前端可复用 `file-api.ts` 类型 |
| 新前端大文件 | `direct/init` + `direct/complete` | S3 预签名 | 本地后端不支持直传 → 策略里 `direct_upload=false` 时接口返回 422 并提示「请用普通上传」 |
| 老记录在新接口下载 | `GET /business/storage/download/{id}` | 按 `sys_file.storage_code` 路由；**为 NULL 视为本地**（历史数据） | 兼容老数据的关键：NULL = 本地，不需要数据回填 |

**为什么不做「把老接口改成路由到新后端」**：老接口的删除/下载逻辑写死了 `nik::upload_path()`，改它等于改底座 + 影响所有现有调用方回归；而组合方案下**新旧并存、老接口行为冻结**，风险最小。

### 2.8 文件去重（跨后端）

现状：`file_hash` 只做「命中即复用记录」，但无唯一约束、无后端信息。

本批：
1. `uk_sys_file_hash`（部分唯一：`file_hash IS NOT NULL AND deleted_at IS NULL`）→ 并发首传也只会有一条记录（POC-6 **T6** 验证；软删后可重新入库 **T6b**）。
2. 秒传命中时返回 `FileRes`（含 `storage_code`）→ 读取按其 `storage_code` 路由，因此**跨后端秒传天然成立**：内容只存一份，落在「首传时选中的那个后端」。
3. 若首传后端已下线/不可用：`StorageRouter` 发现 `by_code()` 缺失 → 返回 409 `STORAGE_BACKEND_UNAVAILABLE` + 提示「文件所在后端已停用，请重新上传」（**不做自动跨后端复制**，避免隐式大流量与不一致）。
4. 删除语义修正（S5）：`DELETE /business/storage/file/{id}` 只做 `deleted_at = now()`；物理对象交给 `business/storage` 自注册的清理任务（cron `0 30 4 * * *`）：**软删超过 7 天且 `file_hash` 无其他有效记录**才 `delete(key)`。这样秒传复用者不会因别人删记录而丢文件。
5. **已知风险（写进风险清单）**：秒传返回的是**同一条 `sys_file` 记录**，若业务 A 删除该记录，业务 B 手里的 `file_id` 就失效（记录软删、下载 404）。缓解：①业务侧应保存自己的「引用表」（ERP 的附件关联表就是）；②本批提供 `sys_file.storage_code` 不变的「重新登记」接口 `POST /business/storage/file/{id}/rebind`（把同 key 重新登记为新记录）。彻底方案（`sys_file_ref` 引用计数表）留 B 批次与数据权限一起做。

### 2.9 对象存储 API 契约（前缀 `/api/business/storage`，路由宏内写 `/business/storage`）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/storage/config/page` | `business:storage:config:list` | 后端配置分页（AK 掩码） |
| POST | `/storage/config` | `business:storage:config:create` | 新建（AK/SK 明文入参，落库前加密） |
| PUT | `/storage/config/{id}` | `business:storage:config:update` | 修改（`ak` 为空表示不修改密钥） |
| DELETE | `/storage/config/{id}` | `business:storage:config:delete` | 删除（有引用 `sys_file.storage_code` 时 409） |
| POST | `/storage/config/{id}/default` | `business:storage:config:update` | 设为默认（事务内先清后设 + 唯一索引兜底） |
| POST | `/storage/config/{id}/test` | `business:storage:config:test` | 连通性探测（写 1 KB 探针对象再删） |
| GET | `/storage/policy/page` | `business:storage:policy:list` | 上传策略列表 |
| PUT | `/storage/policy/{id}` | `business:storage:policy:update` | 修改策略（大小/白名单/直传/TTL） |
| POST | `/storage/upload` | `business:storage:upload` | 服务端上传（multipart，字段 `file`，`?category=`） |
| POST | `/storage/direct/init` | `business:storage:upload` | 预签名直传初始化（含秒传短路） |
| POST | `/storage/direct/complete` | `business:storage:upload` | 直传完成登记（校验大小/类型/哈希） |
| GET | `/storage/file/page` | `business:storage:list` | 文件分页（可按 `storage_code/category/hash` 过滤） |
| GET | `/storage/download/{id}` | `business:storage:download` | 下载（S3 → 302；本地 → 流式） |
| GET | `/storage/presign/{id}` | `business:storage:download` | 取限时链接（JSON，前端自行 window.open） |
| DELETE | `/storage/file/{id}` | `business:storage:delete` | 软删（物理对象延迟回收） |
| POST | `/storage/file/{id}/rebind` | `business:storage:upload` | 重新登记（同 key 新记录） |

统一响应体沿用 `Res<T>`/`PageData<Vec<T>>`；错误码新增段位（`6000x`）与落地清单见 §6「需批准的底座最小改动」。

---
## 3. 导入导出框架

### 3.1 Excel 库选型

| 维度 | `calamine` 0.36（读） | `rust_xlsxwriter`（写） | `umya-spreadsheet`（读写） | **框架 `xlsx` feature（= calamine + rust_xlsxwriter）** |
|---|---|---|---|---|
| 定位 | 纯读（xls/xlsx/ods/xlsb） | 纯写（无读） | 读写一体 | 读 + 写都封装好 |
| 读 10 万行×4 列 | 快（POC-2 经框架封装实测 **3 004 ms**） | — | 较慢（DOM 全量 + 富文本模型，通常 1.5–3× calamine） | **3 004 ms** |
| 写 10 万行×4 列 | — | 快（**5 243 ms**，含表头样式与列宽自适应） | 明显更慢（每次 set 都走内部模型） | **5 243 ms** |
| 内存模型 | `Range` = 整表矩形读入（POC-2 峰值 **220 MB** @10 万行） | 行式写，写完 `save` 出字节 | 完整 DOM（单元格样式/合并/图片都建模） | 同 calamine |
| 流式能力 | `worksheet_rows()` 迭代（框架未透出） | 无（必须先建完再 save） | 无 | 未透出 |
| 样式/图表 | 不支持 | 字体/底色/列宽/冻结/合并/条件格式 | 最全（含图表、图片） | 仅表头加粗 + 底色 + 列宽 + 冻结 |
| 许可 | MIT | MIT/Apache-2.0 | MIT | Apache-2.0 |
| 维护 | 活跃（0.36） | 活跃 | 活跃但单人维护 | 随框架 |
| 新增依赖 | 1 | 1 | 1 | **0**（feature 开关） |

**推荐：用框架 `xlsx` feature，不直接声明任何 Excel crate。**
理由：① 需求是「表格化数据交换」，不需要图表/图片/条件格式，`umya` 的能力富余用不上；② 框架已把两个最佳单一职责库组合好，并处理了「首行表头 + 冻结窗格 + 列宽自适应」这些每次都要重写的样板；③ 零新增依赖，不破坏铁律；④ POC-2 已量到 10 万行的真实时间与内存，容量规划有据。

**框架 API 实测签名（照此调用，勿猜）**：
```rust
// 写（单 sheet / 多 sheet）
let bytes: Vec<u8> = Export::to_xlsx_bytes(&["code","name"], &rows)?;          // rows: &[HashMap<String,String>]
let bytes: Vec<u8> = Export::to_xlsx_workbook(&[XlsxSheet::new("清单", &cols, &rows)])?;
// 读（首 sheet / 指定 sheet / 类型化）
let rows: Vec<HashMap<String,String>> = Import::from_xlsx(&bytes)?;
let names: Vec<String> = Import::xlsx_sheet_names(&bytes)?;                    // 用于识别「错误行」sheet
let typed: Vec<MaterialRow> = Import::from_xlsx_typed(&bytes)?;                // 表头=字段名，自动类型推断
```
> 注：`Export::to_xlsx_bytes(columns, rows)` **只有两个参数**（第三参传 sheet 名会编译失败，POC 实测踩过）。

### 3.2 导入流程（状态机 + 时序）

```
                 ┌──────────┐  上传/解析成功   ┌───────────┐  用户确认   ┌───────────┐
  提交文件 ───▶   │ PENDING  │ ───────────────▶ │ VALIDATED │ ──────────▶ │ IMPORTING │
                 └──────────┘                  └───────────┘             └───────────┘
                      │ 解析失败                     │ 用户取消                │
                      ▼                              ▼                        ▼
                   FAILED                        CANCELED          SUCCESS / PARTIAL / FAILED
```
`PARSING` 是 `PENDING → VALIDATED` 的中间态（进度条用），`PARTIAL` = `mode='PARTIAL'` 且有错误行时跳过的入库结果。

**两步式（预检 → 确认）是本设计的核心取舍**：绝不「上传即入库」。原因：Excel 里一行错了，用户需要看到「第 37 行『物料编码』重复」再决定是修文件还是跳过；单步导入只能给一句「导入失败」，运营无法自助。

时序（关键交互）：

| 步骤 | 谁做 | 事务边界 | 产物 |
|---|---|---|---|
| 1 模板下载 | `GET /impexp/import/template?biz_type=erp.material` | 无 | 由定义动态生成 xlsx（表头 + 1 行示例 + 「填写说明」第二 sheet） |
| 2 上传 | `POST /impexp/import/upload?biz_type=...&mode=STRICT` | 建任务行 | `sys_import_task`(PENDING) + 复用 `business/storage` 存文件 → `file_id` |
| 3 解析 | 后台 worker（`AsyncTaskPlugin`） | 无（只读文件） | 状态 `PARSING` → 行数据在内存 `Vec<RowValues>` |
| 4 逐行校验 | 同上 | 无 | `total/valid/error` + 错误明细批量落 `sys_import_task_error` + `error_summary` 摘要 |
| 5 回显 | `GET /impexp/import/{id}` + `/errors` | 无 | 前端表格：行号/列/原因；可下载「错误行」xlsx |
| 6 确认入库 | `POST /impexp/import/{id}/confirm` | **按批次事务**（见 §3.4） | `IMPORTING` → `SUCCESS`/`PARTIAL`，`imported_rows` 与进度更新 |
| 7 完成通知 | worker 末尾调 `notice::dispatch` | 独立短事务 | 站内信「导入完成，成功 980/失败 20」 |

### 3.3 逐行校验引擎（业务接入契约）

```rust
// business/impexp/definition.rs
/// 列定义：决定模板表头、解析类型、必填、字典翻译、唯一性提示
pub struct ImportColumn {
    pub key: &'static str,            // 字段名（= 解析后 HashMap 的键，也是错误明细 column_key）
    pub header: &'static str,         // 中文表头
    pub required: bool,
    pub kind: ColumnKind,             // Text / Int / Decimal / Bool / Date / DateTime / Enum(&[&str]) / Dict(code)
    pub unique_key: bool,             // 参与「文件内唯一」校验（同文件内重复即报错）
    pub fk: Option<FkSpec>,           // 外键存在性：表名 + 列名 + 展示列（只读底座表）
    pub example: &'static str,        // 模板示例值
    pub width: Option<u16>,
}

/// 业务方实现这一个 trait，就拥有「模板下载 + 预检 + 入库 + 错误回显」全套能力
#[async_trait::async_trait]
pub trait ImportDefinition: Send + Sync {
    fn biz_type(&self) -> &'static str;          // 全局唯一，如 "erp.material"
    fn biz_name(&self) -> &'static str;          // "物料主数据"
    fn columns(&self) -> &'static [ImportColumn];
    fn max_rows(&self) -> usize { 200_000 }      // 超过即拒绝（防 OOM，§3.4）
    /// 单行业务规则（跨列校验、编码规则、状态机约束）；返回 Vec 以支持一行多错
    async fn validate_row(&self, ctx: &ImportCtx, row: &RowValues) -> Result<(), Vec<RowError>>;
    /// 入库前钩子（如：检查导入期间是否有人改了基础资料）——在同一事务内
    async fn before_import(&self, tx: &Transaction, ctx: &ImportCtx) -> DbResult<()> { Ok(()) }
    /// 批量入库：由框架传入已校验的行，业务方自己写 INSERT/UPDATE
    async fn persist(&self, tx: &Transaction, rows: &[RowValues], ctx: &ImportCtx) -> DbResult<usize>;
    fn batch_size(&self) -> usize { 2_000 }
}
```

引擎做的通用校验（业务方不用重复写）：

| 校验 | error_code | 实现 | 依据/坑 |
|---|---|---|---|
| 必填 | `REQUIRED` | `required && value.trim().is_empty()` | 全角空格要 trim（`char::is_whitespace`） |
| 类型 | `TYPE` | `ColumnKind` 解析（Int/Decimal/Date/DateTime/Bool） | 日期要兼容 `2026/10/28`、`2026-10-28`、Excel 序列号三种写法 |
| 枚举 | `ENUM` | `kind = Enum(&["启用","停用"])` | 中文值 ↔ 存库码的映射由 `ColumnKind::Enum` 双列表达（`&[(&str,&str)]`） |
| 字典 | `DICT` | 查 `sys_dict_type/sys_dict_item`（进程级缓存 5 min） | 字典项映射：label → value；**反向**（导出）value → label |
| 文件内唯一 | `UNIQUE` | `unique_key` 组合 `HashMap` 去重 | 只查文件内，不查库（库内唯一由业务 `validate_row` 或 DB 唯一索引兜底） |
| 外键存在 | `FK_NOT_FOUND` | `FkSpec{table,column,label}` 批量 `SELECT ... WHERE col = ANY($1)` 一次查全 | **禁止逐行查库**（10 万行 = 10 万次 RTT） |
| 行数上限 | `ROW_LIMIT` | `max_rows()` | 与 §3.4 的阈值联动 |

**外键批量校验的落地**：预检阶段先扫一遍收集所有 `fk` 列的取值 → 每列一次 `WHERE col = ANY($1::text[])` → 建 `HashSet` → 逐行 O(1) 判定。10 万行 × 3 个外键 = **3 次查询**，而不是 30 万次。

### 3.4 大文件（10 万行+）方案

**实测数据（POC-2）**：10 万行×4 列 → 文件 2.3 MB、解析 3.0 s、峰值 RSS **220 MB**。线性外推：20 万行 ≈ 450 MB，50 万行 ≈ 1.1 GB。**所以不能无脑放开。**

三档策略（阈值写在 `sys_config`，运行期可调）：

| 档位 | 行数 | 解析 | 入库 | 事务 | 依据 |
|---|---|---|---|---|---|
| 快速档 | ≤ 5 000 | 同步（handler 内直接解析） | 单事务一次 `persist` | **1 个事务** | 5 000 行 ≈ 15 MB 内存、300 ms 解析，用户可接受同步等待 |
| 标准档 | 5 001–200 000 | 异步任务（`AsyncTaskPlugin`，并发上限 2） | 分批 2 000 行/事务 | **批事务**：每批独立提交，失败只回滚该批并记 `PARTIAL` | 单事务 20 万行会长时间持有锁 + 撑爆 WAL；批事务让失败可定位到批次 |
| 超大档 | > 200 000 | **拒绝 xlsx**，要求 CSV | 流式逐行读（`BufRead`）+ 每 2 000 行入库 | 批事务 | xlsx 解析必须整本入内存（calamine `Range`），200 MB+ 进程内存不可控；CSV 可流式，内存恒定 |

**CSV 路径为什么不用 `Import::from_csv`**：框架 `from_csv(csv_str)` 收 `&str`（整串），仍是全量。超大档用「`BufReader` 逐行 + 分块提交」，只需 `std::io`，零新依赖；同时保留框架 `from_csv` 给 ≤5 000 行的小 CSV。

**分批事务与失败语义（重要）**：
- `mode='STRICT'`：任一批失败 → **终止后续批次**，已提交批次**不回滚**（无法跨事务回滚），任务标 `FAILED` 并在 `message` 写明「第 3 批（行 4001–6000）失败，前 4000 行已入库」，同时提供「按已入库范围修正后重导」的说明。这是**诚实的取舍**：分布式跨批事务要么 2PC，要么大事务，两者在 20 万行规模上代价更高。
- `mode='PARTIAL'`：失败批跳过并记账，最终 `PARTIAL`（成功 N / 失败 M）。
- 幂等：`persist` 由业务方负责（推荐 `INSERT ... ON CONFLICT (业务唯一键) DO UPDATE`），框架在 `before_import` 里提供「本任务已导入行数」用于重放跳过。

### 3.5 导出流程

| 场景 | 阈值 | 通道 | 说明 |
|---|---|---|---|
| 同步导出 | ≤ 5 000 行 | `GET /impexp/export/sync` 直接返回文件流 | 不落任务表，`Content-Disposition` 走 RFC 5987（复用 `support/file` 的 `percent_encode` 思路） |
| 异步导出 | > 5 000 行 | `POST /impexp/export` 建任务 → 轮询/SSE → 完成后下载 | 落 `sys_export_task`；分块 5 000 行查一页写一页 |

异步导出的执行器：`SchedulerPlugin` 注册 `impexp_export_worker`，cron `*/5 * * * * *`（每 5 秒扫一次），用 **`FOR UPDATE SKIP LOCKED`** 认领任务：

```sql
-- 多实例安全的任务认领（单机多协程、多副本都成立）
WITH pickup AS (
    SELECT id FROM sys_export_task
    WHERE status = 'PENDING' AND deleted_at IS NULL
    ORDER BY created_at
    LIMIT 1
    FOR UPDATE SKIP LOCKED
)
UPDATE sys_export_task t
SET status = 'RUNNING', started_at = now(), progress = 0
FROM pickup WHERE t.id = pickup.id
RETURNING t.id, t.biz_type, t.query_params, t.columns_snapshot, t.format;
```

进度更新节流：每写 1 个分块（5 000 行）更新一次 `progress`（`exported_rows/total_rows*100`），**不做每行更新**（10 万行 = 10 万次 UPDATE 会自己把自己拖死）。

完成后：写 `file_id/file_size/expire_at`（默认 7 天）→ 可选发站内信（`notice::dispatch`）→ 前端轮询 `GET /impexp/export/{id}` 得到 `file_id` 再走 `business/storage/download`。

清理任务：`impexp_task_cleanup`，cron `0 40 4 * * *`：过期导出任务的物理文件回收 + `SUCCESS` 但 `expire_at < now()` 置 `EXPIRED` + 导入错误明细超过 90 天的任务整条清理。

### 3.6 表设计取舍：错误明细存哪

| 方案 | 优点 | 缺点 | 结论 |
|---|---|---|---|
| 全 JSONB（`error_detail JSONB`） | 一列搞定、随任务行原子写 | 不能分页（10 万错误 = 单个 10 MB JSON）、不能 `GROUP BY error_code`、不能逐行导出 | ❌ 单独用 |
| 单独明细表 | 可索引、可分页、可统计、可流式导出 | 列表页要多一次 join/聚合 | ✅ 采用（明细） |
| **JSONB 摘要（前 100 条）+ 明细表** | 列表页一次查询就能显示「前几条错误」；明细承载全量 | 双写 | ✅ **最终方案** |

理由补充：`ck_sys_import_task_rows (valid+error <= total)` + `sys_import_task_error` 的 `(task_id,row_no)` 索引，让「错误行导出」变成 `ORDER BY row_no` 的流式扫描，POC-6 **T13c 实测批量写入 1 000 行错误明细正常**。

### 3.7 导出定义契约（与导入对称）

```rust
pub struct ExportColumn {
    pub key: &'static str,
    pub header: &'static str,
    pub kind: ColumnKind,          // 用于格式化（Decimal 保留位、Date 格式化、Dict 翻译）
    pub width: Option<u16>,
}
#[async_trait::async_trait]
pub trait ExportDefinition: Send + Sync {
    fn biz_type(&self) -> &'static str;
    fn biz_name(&self) -> &'static str;
    fn columns(&self) -> &'static [ExportColumn];
    /// 总量（用于进度百分比；允许返回 0 表示「未知」，前端显示不确定进度条）
    async fn count(&self, q: &serde_json::Value) -> DbResult<i64>;
    /// 分块取数：keyset 分页优先（ORDER BY id），避免 OFFSET 越翻越慢
    async fn fetch(&self, q: &serde_json::Value, after_id: Option<&str>, limit: u32) -> DbResult<Vec<ExportRow>>;
}
```
`ExportRow = HashMap<&'static str, String>`（框架 `Export::to_xlsx_bytes` 正是这个入参形态，零适配成本）。

### 3.8 导入导出 API 契约（前缀 `/api/business/impexp`）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/impexp/def/list` | `business:impexp:def:list` | 已注册的导入/导出定义（前端下拉） |
| GET | `/impexp/import/template` | `business:impexp:import:template` | 下载导入模板（`?biz_type=`） |
| POST | `/impexp/import/upload` | `business:impexp:import:upload` | 上传并建任务（`?biz_type=&mode=`），返回 task_id |
| GET | `/impexp/import/page` | `business:impexp:import:list` | 导入任务分页（我的/全部） |
| GET | `/impexp/import/{id}` | `business:impexp:import:list` | 任务详情（含摘要错误） |
| GET | `/impexp/import/{id}/errors` | `business:impexp:import:list` | 错误明细分页（行号/列/原因） |
| GET | `/impexp/import/{id}/errors/file` | `business:impexp:import:download` | 下载「错误行」xlsx（推荐路径） |
| GET | `/impexp/import/{id}/download` | `business:impexp:import:download` | 同上（文档别名，与上一条同实现；遗留修复④） |
| POST | `/impexp/import/{id}/confirm` | `business:impexp:import:confirm` | 确认入库（`VALIDATED → IMPORTING`） |
| POST | `/impexp/import/{id}/cancel` | `business:impexp:import:cancel` | 取消（仅 PENDING/VALIDATED） |
| POST | `/impexp/import/{id}/retry` | `business:impexp:import:retry` | 失败重试（新建任务，保留原任务） |
| GET | `/impexp/export/sync` | `business:impexp:export:create` | 同步导出（≤5 000 行） |
| POST | `/impexp/export` | `business:impexp:export:create` | 建异步导出任务 |
| GET | `/impexp/export/page` | `business:impexp:export:list` | 导出任务分页 |
| GET | `/impexp/export/{id}` | `business:impexp:export:list` | 任务详情（进度/文件） |
| GET | `/impexp/export/{id}/download` | `business:impexp:export:download` | 下载结果（过期返回 410） |
| POST | `/impexp/export/{id}/cancel` | `business:impexp:export:cancel` | 取消（仅 PENDING） |
| POST | `/impexp/export/{id}/retry` | `business:impexp:export:retry` | 重试（FAILED/CANCELED/已过期；V0052 补种权限点） |
| DELETE | `/impexp/export/{id}` | `business:impexp:export:delete` | 删除任务与文件 |

---

## 4. 消息通知

### 4.1 数据模型（DDL 见 §7 附录 A · V0043–V0045）

三张核心表 + 一张预留表：

```
sys_notice_template ──(template_code)──┐
                                       ▼
sys_notice ──1:N──▶ sys_notice_receiver ──1:N──▶ sys_notice_delivery
（一次业务事件）      （每用户一行，已读状态）      （邮件/Webhook/短信投递记录）
```

关键列语义：

| 列 | 作用 | 陷阱 |
|---|---|---|
| `target_type` + `target_ids JSONB` | 留痕「发给谁」（USER/ROLE/DEPT/ALL） | 目标只是**输入**，实际接收人在 fanout 后落 `receiver` 表；`target_ids` 不参与任何读路径 |
| `channel_mask SMALLINT` | 位掩码 1站内/2邮件/4Webhook/8短信 | CHECK `BETWEEN 1 AND 15` 防越界（POC-6 T7c） |
| `idempotency_key` | 业务事件唯一键（如 `wf:instance:123:todo`） | 必须由调用方传；`uk_sys_notice_idem` 保证重复触发只发一次（POC-6 T8b） |
| `receiver_count/read_count` | 冗余计数，供「送达率/阅读率」看板 | `ck_sys_notice_counts` 强制 `read_count <= receiver_count`（POC-6 T7b） |
| `sys_notice_receiver.read_flag + read_at` | 已读状态 | CHECK 强制「已读必有时间」（POC-6 T7）——否则统计口径会飘 |
| `sys_notice_receiver.is_deleted` | 用户侧删除 | 与 `read_flag` 独立：删了也算「已送达」 |

### 4.2 发送目标解析与扇出（1 万接收人怎么写）

**结论：四条 SQL 配置一次 `INSERT ... SELECT`，永不「查出来再 for 循环插」。**

```sql
-- ① 用户：直接用
SELECT id FROM sys_user WHERE id = ANY($1::uuid[]) AND deleted_at IS NULL AND status = 1

-- ② 角色：join 授权表，过滤过期与停用用户
SELECT DISTINCT u.id FROM sys_user_role ur
JOIN sys_user u ON u.id = ur.user_id AND u.deleted_at IS NULL AND u.status = 1
WHERE ur.role_id = ANY($1::uuid[]) AND (ur.expire_time IS NULL OR ur.expire_time > now())

-- ③ 部门含下级：递归 CTE（与 system/dept/model.rs 同款，不依赖可能失同步的 dept_path）
WITH RECURSIVE dept_tree AS (
    SELECT id FROM sys_dept WHERE id = ANY($1::uuid[]) AND deleted_at IS NULL
    UNION ALL
    SELECT c.id FROM sys_dept c JOIN dept_tree t ON c.parent_id = t.id WHERE c.deleted_at IS NULL
)
SELECT u.id FROM sys_user u JOIN dept_tree t ON u.dept_id = t.id
WHERE u.deleted_at IS NULL AND u.status = 1

-- ④ 全员
SELECT id FROM sys_user WHERE deleted_at IS NULL AND status = 1

-- 统一落库（一条语句，天然去重 + 重放幂等）
INSERT INTO sys_notice_receiver (notice_id, user_id)
SELECT $1, u.user_id FROM (<上面命中的 UNION>) u
ON CONFLICT (notice_id, user_id) DO NOTHING;
```

实测（POC-6 T9/T10/T11）：
- 部门含下级命中 3 人、角色命中 1 人、全员命中 3 人，**均正确排除软删用户与停用用户**；
- **10 003 接收人扇出耗时 91.5 ms**（单条 `INSERT ... SELECT`，含唯一索引维护）；
- 重放同一 SQL 零新增行（`ON CONFLICT DO NOTHING`，T8 验证）。

**为什么不用 `dept_path LIKE`**：底座自己的注释就写明 `dept_path` 在部门搬迁时不会重算（`system/dept/handler.rs:283` 的已知限制），递归 CTE 的代价（1 万部门 ≈ 几十 ms，且只在发送时执行一次）远小于「发出去了但名单是错的」。

**超大规模兜底**（>5 万接收人）：分批 `INSERT`（每批 5 000 行）+ 单条通知最多 5 万接收人上限（超出返回 422，提示改用「全员公告」或分批发送）。落地阈值放 `sys_config`。

### 4.3 已读 / 未读

| 接口 | SQL 要点 | 索引 |
|---|---|---|
| 单条已读 | `UPDATE sys_notice_receiver SET read_flag=TRUE, read_at=now() WHERE id=$1 AND user_id=$2 AND read_flag=FALSE` | 主键 |
| 全部已读 | `UPDATE ... SET read_flag=TRUE, read_at=now() WHERE user_id=$1 AND read_flag=FALSE` | `idx_sys_notice_receiver_unread` |
| 未读数 | `SELECT count(*) ... WHERE user_id=$1 AND read_flag=FALSE AND is_deleted=FALSE` | **部分索引**（只索引未读行，索引体积 ≈ 未读数而非全表） |
| 我的列表 | `... WHERE user_id=$1 AND is_deleted=FALSE ORDER BY created_at DESC LIMIT ?` | `idx_sys_notice_receiver_list` |

POC-6 T11b 真实执行计划（1 万接收人表）：

```
Aggregate (actual time=0.551..0.552 rows=1)
  ->  Bitmap Heap Scan on sys_notice_receiver (actual time=0.549..0.550 rows=1)
        Recheck Cond: ((user_id = ...) AND (NOT read_flag) AND (NOT is_deleted))
        Heap Blocks: exact=1
```
→ **0.55 ms**，且随表增长只随「该用户未读数」增长，不随总行数增长。

列表查询（T11c）走 `idx_sys_notice_receiver_list` + `Hash Join`，0.42 ms（Limit 20）。

**未读数的缓存策略**：不缓存。理由：① 查询已是 0.5 ms 级；② 缓存会让「已读后红点不消失」，是消息中心最容易被投诉的 bug；③ 若未来压力上来，用 Redis `INCR/DECR` 维护计数（`notice:unread:{user_id}`），**写路径已读/扇出时同步增减**——预留但不实现（本批量级无必要）。

### 4.4 多通道预留（站内信必做，其余只留接口）

| 通道 | 本批 | 复用/预留方式 |
|---|---|---|
| 站内信 | ✅ 完整实现 | `sys_notice` + `sys_notice_receiver` |
| 邮件 | 只落投递记录 | 框架 `NotificationPlugin`（SMTP/lettre）已具备 `send_text/send_html/is_configured`；`worker` 里判断 `channel_mask & 2` 且 `plugin.is_configured()` 才尝试，未配置则 `SKIPPED`。注：它是**同步阻塞**实现，必须放 `spawn_blocking` |
| Webhook | 只落投递记录 + 配置 | 复用 `sys_notice_channel_config`（本设计不建独立表，配置存 `sys_config` 的 `notice.webhook.*`，理由：单例配置不值得一张表） |
| 短信 | 只落投递记录 | 同上；`target` 取 `sys_user.mobile` |

`sys_notice_delivery` 状态机：`PENDING → SENDING → SENT / FAILED(重试) / SKIPPED(未配置)`，指数退避 `next_retry_at`（1 min / 5 min / 25 min，最多 3 次），`uk_sys_notice_delivery(notice_id, receiver_id, channel)` 保证不重复投递。

**统一发送入口（本批最关键的一段「预留」代码）**：
```rust
// business/notice/api.rs —— 供 C 批次公告、D 批次工作流直接调用
pub struct NoticeDraft {
    pub notice_type: NoticeType,          // SYSTEM/TODO/ALERT/WORKFLOW/CIRCULAR
    pub level: NoticeLevel,
    pub title: String,
    pub content: String,
    pub content_format: ContentFormat,
    pub link_url: Option<String>,
    pub biz_type: Option<String>,         // "workflow" / "announcement"
    pub biz_id: Option<String>,
    pub template_code: Option<String>,
    pub template_vars: serde_json::Value, // {"userName":"张三"}
    pub target: NoticeTarget,             // User(Vec<Uuid>) / Role(..) / Dept(..) / All
    pub channel_mask: u16,
    pub idempotency_key: Option<String>,
    pub expire_at: Option<DateTime<Utc>>,
}
/// 唯一的对外发送口径：内部完成模板渲染 → 落 notice → 扇出 → 投递记录 → SSE 推送
pub async fn dispatch(draft: NoticeDraft) -> DbResult<DispatchResult>;
```
D 批次工作流只要 `notice::dispatch(NoticeDraft{ notice_type: TODO, biz_type:"workflow", biz_id: instance_id, idempotency_key: Some("wf:{id}:todo:{task_id}") ... })`，**重复调用天然幂等**（唯一索引拦）。这就是任务书要求的「本批把接口预留好」。

### 4.5 实时推送：要不要 SSE / WebSocket

**结论：上 SSE，不上 WebSocket；且必须按用户拆频道。**

| 方案 | 鉴权 | 断线重连 | 服务端复杂度 | 结论 |
|---|---|---|---|---|
| 轮询（前端 30 s 拉未读数） | 复用现有 JWT | 天然 | 0 | ✅ 兜底（必须保留，SSE 挂了不能瞎） |
| **SSE** | 走普通 HTTP，可带 `Authorization`，**天然复用 `AuthLayer`** | 浏览器 `EventSource` 自动重连 | 低（框架已有 `Broadcaster` + `sse_json`） | ✅ **采用** |
| WebSocket | 不能带 `Authorization` 头，需要 query token / subprotocol 额外方案 | 自己写心跳与重连 | 高（连接注册表、心跳、鉴权） | ❌ 本项目无 WS 基础设施，不值得为「红点」引入 |

**框架 `Broadcaster` 的一个真实陷阱**：它是**进程级单播广播**（`tokio::sync::broadcast`），所有订阅者收到同样消息。若直接用，A 用户会收到「B 有新的审批待办」，`notice_id` 甚至标题都会泄露。**必须 per-user**：

```rust
// business/notice/sse_hub.rs
static HUB: OnceLock<RwLock<HashMap<Uuid, Broadcaster<String>>>> = OnceLock::new();
/// 每个用户一个 broadcaster，容量 64；无人订阅时惰性创建，30 分钟无订阅者由清理任务回收
pub fn channel(user_id: Uuid) -> Arc<Broadcaster<String>>;
pub fn publish(user_id: Uuid, payload: NoticeEvent);   // 只推给该用户
```
订阅端点 `GET /business/notice/sse`：从 `AuthClaims` 取 `user_id` → `sse_json(hub.channel(user_id).subscribe(), "notice")`。断连即退订（`receiver_count` 归零）。

**多副本注意**：进程内 `Broadcaster` 只在**同进程**生效；多副本部署时 A 副本发的通知，连在 B 副本上的用户收不到。缓解：前端仍以轮询为权威（SSE 只做「立即刷新」的加速），并在 B 批次引入 Redis Pub/Sub 做跨副本转发（设计预留：`publish()` 内部已是单点，替换实现即可）。

### 4.6 模板渲染

**结论：自研 `{{var}}` 替换器 + 变量声明白名单 + 严格模式（未知变量报错，不静默留空）。**

| 方案 | 表现力 | 安全 | 与「模板存在数据库」契合 | 结论 |
|---|---|---|---|---|
| 简单字符串替换（自研 ≈80 行） | 变量替换、日期/数字格式化 | 好（只做值替换，无求值） | ✅ 天然 | ✅ **采用** |
| `nik-template`（框架 `template` feature） | 循环/条件/继承 | 好 | ❌ `render_template(name, ctx)` 按**名字**从目录取模板（`nik-web/src/resources.rs:212`），DB 行用不了 | ❌ |
| Tera/Jinja 类 | 最全 | 需沙箱，模板里写 `{{ x.__class__ }}` 这类逃逸是真实攻击面 | 可以但重 | ❌（C 批次公告若需循环再评估） |

渲染规则（必须有单测）：
1. 语法：`{{var}}` / `{{ var }}`，变量名 `[a-zA-Z_][a-zA-Z0-9_]{0,63}`；不支持表达式、函数、过滤器。
2. 未知变量：`Err(ApiError 422 TEMPLATE_VAR_MISSING)`（**不静默替换成空串** —— 静默会导致「发给客户的邮件里写着 尊敬的」，比报错难查 100 倍）。
3. HTML 格式化：`content_format='HTML'` 时，**先对变量值做 HTML 转义**再替换（防 XSS：变量常来自用户输入，如真实姓名），模板骨架本身入库前用框架 `xss`（`ammonia`）净化。
4. 长度：渲染后 `title ≤ 255`、`content ≤ 64 KB`，超限拒绝（防止模板把 `sys_notice` 撑爆）。
5. 模板保存时用声明的 `variables` 做一次「示例值试渲染」，失败即拒绝保存（编辑页即时反馈）。

### 4.7 与后续批次的关系（接口预留清单）

| 后续批次 | 需求 | 本批预留 |
|---|---|---|
| C 公告管理 | 公告发布即推送全员/指定部门 | 直接调 `notice::dispatch`（`notice_type=CIRCULAR`, `target=Dept/All`） |
| C 变更日志 | 版本更新后给「关注者」发通知 | 同上（`biz_type="changelog"`） |
| D 工作流 | 待办/抄送/超时提醒 | `notice_type=TODO` + `idempotency_key="wf:{instance}:{node}:{user}"` + `link_url="/workflow/task/{id}"`；**超时提醒靠 `expire_at` 之外的时间字段，在 D 批次加 `sys_notice.schedule_at`（本期不预留列，避免空列长期闲置——记入 D 批次迁移）** |
| B 数据权限 | 通知也要按数据权限过滤接收人？ | 不需要：接收人是「人」，不是「数据行」；但**离职/停用用户不再接收**已在本批 SQL 中实现 |

### 4.8 通知 API 契约（前缀 `/api/business/notice`）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| POST | `/notice` | `business:notice:send` | 发送通知（管理端手工/批量） |
| GET | `/notice/page` | `business:notice:list` | 通知主体分页（管理视角，含送达/已读统计） |
| GET | `/notice/{id}` | `business:notice:list` | 通知详情（含接收人统计） |
| DELETE | `/notice/{id}` | `business:notice:delete` | 删除通知（级联接收行，POC-6 T12 验证） |
| GET | `/notice/mine` | `business:notice:mine` | 我的通知分页（`?read_flag=`） |
| GET | `/notice/unread-count` | `business:notice:mine` | 未读数（高频，0.5 ms 级） |
| PUT | `/notice/read/{receiver_id}` | `business:notice:mine` | 单条已读 |
| PUT | `/notice/read-all` | `business:notice:mine` | 全部已读 |
| DELETE | `/notice/mine/{receiver_id}` | `business:notice:mine` | 删除我的这条（`is_deleted`） |
| GET | `/notice/sse` | `business:notice:mine` | SSE 订阅（按 `user_id` 分频道） |
| GET | `/notice/template/page` | `business:notice:template:list` | 模板列表 |
| POST | `/notice/template` | `business:notice:template:create` | 新建模板（保存前试渲染） |
| PUT | `/notice/template/{id}` | `business:notice:template:update` | 修改模板 |
| DELETE | `/notice/template/{id}` | `business:notice:template:delete` | 删除模板（被通知引用时 409） |
| POST | `/notice/template/{id}/preview` | `business:notice:template:list` | 试渲染（传变量 JSON） |
| GET | `/notice/channel/config` | `business:notice:channel:config` | 通道配置读取（邮件已配置/未配置、Webhook 地址掩码） |

> 注意：**已读/我的通知这类「人人要用」的接口在底座里也必须声明权限码**（实测 `system/user/handler.rs` 10/10、`support/online/handler.rs` 2/2 全部带 `#[nik::permission]`），因此 `V0046` 必须把 `business:notice:mine` 授予**所有已存在角色**（而不只是 SUPER_ADMIN），否则普通用户点开消息中心就 403。

---
## 5. 横切设计

### 5.1 权限码清单（33 个）

命名遵循底座约定 `{模块}:{资源}:{动作}`（`sys_menu.auth_code` 与 `#[nik::permission]` 逐字一致）。三个模块全部落在 `business:` 前两段，与 `business:monitor:server-status` 的既有风格一致。

**对象存储（11）**

| 权限码 | 用途 | 授予 |
|---|---|---|
| `business:storage:config:list` | 后端配置查看 | 超管 / 运维 |
| `business:storage:config:create` | 新增后端 | 超管 |
| `business:storage:config:update` | 修改 / 设默认 | 超管 |
| `business:storage:config:delete` | 删除后端 | 超管 |
| `business:storage:config:test` | 连通性探测 | 超管 / 运维 |
| `business:storage:policy:list` | 策略查看 | 超管 / 运维 |
| `business:storage:policy:update` | 策略修改 | 超管 |
| `business:storage:upload` | 上传（含直传初始化/完成） | 全部业务角色 |
| `business:storage:list` | 文件分页 | 全部业务角色 |
| `business:storage:download` | 下载 / 取限时链接 | 全部业务角色 |
| `business:storage:delete` | 软删文件 | 超管 / 文件所有者 |

**导入导出（14）**

| 权限码 | 用途 |
|---|---|
| `business:impexp:def:list` | 可用导入/导出定义 |
| `business:impexp:import:template` | 下载导入模板 |
| `business:impexp:import:upload` | 上传并建导入任务 |
| `business:impexp:import:list` | 任务列表 / 详情 / 错误明细分页 |
| `business:impexp:import:confirm` | 确认入库 |
| `business:impexp:import:cancel` | 取消任务 |
| `business:impexp:import:retry` | 失败重试 |
| `business:impexp:import:download` | 下载「错误行」文件（原文档笔误写 `download-error`，以实现为准；遗留修复④） |
| `business:impexp:export:create` | 同步 / 异步导出 |
| `business:impexp:export:list` | 导出任务列表 / 详情 |
| `business:impexp:export:download` | 下载导出结果 |
| `business:impexp:export:cancel` | 取消导出 |
| `business:impexp:export:retry` | 重试导出（V0052 补种；遗留修复⑤） |
| `business:impexp:export:delete` | 删除导出任务 |

**消息通知（9）**

| 权限码 | 用途 | 授予 |
|---|---|---|
| `business:notice:send` | 发送通知 | 超管 / 运营 |
| `business:notice:list` | 通知主体列表 / 详情 | 超管 / 运营 |
| `business:notice:delete` | 删除通知 | 超管 |
| `business:notice:mine` | 我的通知 / 未读数 / 已读 / SSE 订阅 | **所有角色（含默认新角色模板）** |
| `business:notice:template:list` | 模板列表 / 预览 | 超管 / 运营 |
| `business:notice:template:create` | 新建模板 | 超管 |
| `business:notice:template:update` | 修改模板 | 超管 |
| `business:notice:template:delete` | 删除模板 | 超管 |
| `business:notice:channel:config` | 通道配置查看 | 超管 |

> 授予策略写进 `V0046`：`SUPER_ADMIN` 由 `role_code` 短路本就全通（写入 `sys_role_menu` 只为权限树可见，与 `V0020` 同款），但 `business:notice:mine` / `business:storage:*`（上传/列表/下载）/ `business:impexp:*`（导入导出主链路）必须额外授予 `SELECT r.id FROM sys_role r WHERE r.deleted_at IS NULL` 的**全部角色**，否则非超管用户进消息中心即 403（底座实测所有 handler 都带权限宏，没有「登录即可访问」的豁免口子）。

### 5.2 迁移文件分配（V0040–V0049）

全部放 `backend/migrations/business/`（与 `src/business/` 对应，符合 `docs/后端二开指南.md` 第九节）。版本号必须全局唯一（框架在**编译期**把所有 `*.sql` 按版本序合并成 schema 快照，`nik-db-build` 语义），现有最大 V0022、ERP 预留 V0030–V0039，故本批从 V0040 起。

| 版本 | 名称 | 内容 | 可回滚性 |
|---|---|---|---|
| **V0040** | `create_storage_config` | `sys_storage_config`、`sys_storage_policy`、`ALTER sys_file ADD storage_code/upload_mode` + 3 个索引 | ✅ `.down.sql` 删表删列 |
| **V0041** | `create_import_task` | `sys_import_task`、`sys_import_task_error` + 4 索引 | ✅ |
| **V0042** | `create_export_task` | `sys_export_task` + 4 索引 | ✅ |
| **V0043** | `create_notice` | `sys_notice_template`、`sys_notice` + 4 索引 | ✅ |
| **V0044** | `create_notice_receiver` | `sys_notice_receiver` + 3 索引（含未读部分索引） | ✅ |
| **V0045** | `create_notice_delivery` | `sys_notice_delivery` + 2 索引 | ✅ |
| **V0046** | `add_biz_batch_a_permission` | 菜单目录 1 条 + 菜单页 6 条 + 按钮权限点 33 条（幂等 `ON CONFLICT (id) DO NOTHING`，固定 UUID `21000000-…`/`22000000-…`/`23000000-…`/`24000000-…`）+ `sys_role_menu` 授予 | ✅ 删 `sys_menu` 行（`sys_role_menu` 级联） |
| **V0047** | `seed_batch_a_dict` | 字典 6 类：`storage_backend_type`、`import_task_status`、`import_mode`、`export_format`、`notice_type`、`notice_level`（字典项与 Rust 枚举逐字对齐） | ✅ 删字典 |
| **V0048** | `seed_batch_a_config` | `sys_config` 参数 8 条（导入阈值/导出阈值/扇出上限/预签名 TTL/清理保留天数/通道开关）+ 默认后端 `local-default` 1 行 + 默认上传策略 5 行（attachment/avatar/document/import/template） | ✅ |
| **V0049** | `seed_batch_a_notice_template` | 默认通知模板 3 条（`NOTICE_IMPORT_DONE` / `NOTICE_EXPORT_DONE` / `NOTICE_WELCOME`）+ 本批次占位说明（V0050+ 归 B 批次） | ✅ |

**注意**：V0048 的「默认后端 `local-default`」写入时 `endpoint/bucket` 为空（走 `ck_sys_storage_config_remote` 的 LOCAL 分支），`base_path=''`；其存储根目录仍取 `nik::upload_path()`，因此**历史文件零迁移**。

### 5.3 前端页面清单

| # | 页面 / 组件 | 路由 | 权限码 | 复用/新建 |
|---|---|---|---|---|
| 1 | 存储配置 | `/business/storage/config` | `business:storage:config:list` | 新建（TDesign `t-table` + `t-drawer` 表单 + 「测试连接」按钮） |
| 2 | 上传策略 | `/business/storage/policy` | `business:storage:policy:list` | 新建（行内编辑 + 扩展名 tag 输入） |
| 3 | 文件浏览（新） | `/business/storage/file` | `business:storage:list` | 新建（列表 + 拖拽上传 + 大文件直传进度 + 后端列 + 预览/下载） |
| 4 | 导入任务 | `/business/impexp/import` | `business:impexp:import:list` | 新建（列表 + 详情抽屉 + 错误表格分页 + 下载错误行） |
| 5 | 导出任务 | `/business/impexp/export` | `business:impexp:export:list` | 新建（进度条轮询 3 s + 完成下载） |
| 6 | 消息中心 | `/business/notice/mine` | `business:notice:mine` | 新建（页内列表 + 顶栏铃铛组件 `NoticeBell.vue` + SSE 订阅 + 未读红点） |
| 7 | 通知管理 | `/business/notice/manage` | `business:notice:list` | 新建（发送表单：选目标用户/角色/部门树 + 预览 + 送达统计） |
| 8 | 通知模板 | `/business/notice/template` | `business:notice:template:list` | 新建（列表 + 编辑抽屉 + 变量面板 + 实时预览） |
| — | 旧文件管理 | `/support/file`（已存在） | `support:file:list` | **保留不动**（兼容入口，走旧接口） |

前端约定（沿用现有工程）：
- API 路径**必须写全 `/api/business/...`**（漏 `/api` 会命中 SPA fallback 返回 HTML，报「响应体格式不符合」，AGENTS.md 高频踩坑第 1 条）；
- `t-col` 12 列制（`t-col-xl-3` = 一行 4 个）；
- 文件上传用 `t-upload` 的 `requestMethod` 自定义；大文件直传用 `XMLHttpRequest` + `upload.onprogress`（`t-upload` 的 action 模式无法拿到 PUT 进度）；
- 图标名必须查表（TDesign 无 `speed`/`user-check`）：本批用 `folder`、`cloud-upload`、`upload`、`download`、`notification`、`mail`、`file`。

### 5.4 测试要求（C7：每个模块 正常 / 边界 / 错误分支）

原则：**纯函数优先单测，DB 与对象存储用 trait 打桩**。`cargo test` 必须全绿，且不依赖真实 PG/S3（CI 无外部依赖）。

**A. 对象存储（`business/storage`）**

| 用例 | 类型 | 断言要点 |
|---|---|---|
| `sigv4_presign_matches_aws_sdk` | 正常 | 与 `aws-sdk-s3` 生成的签名逐字节相等（**POC-5 已跑通，直接固化**；dev-dependency 只在测试用） |
| `sigv4_uri_encode_rules` | 边界 | 空格→`%20`、`+`→`%2B`、key 内 `/` 保留、中文正确编码（POC-5 已跑通） |
| `sigv4_path_style_vs_virtual_host` | 边界 | MinIO（path）与 OSS（virtual）的 canonical_uri 差异 |
| `policy_rejects_oversize` | 错误 | 超 `max_size_mb` → 422 `STORAGE_SIZE_EXCEEDED` |
| `policy_rejects_bad_extension` | 错误 | `.exe` / 无扩展名 → 400 `STORAGE_TYPE_NOT_ALLOWED` |
| `policy_rejects_mime_mismatch` | 错误 | 扩展名 `pdf` 但 MIME `text/html` → 400 |
| `dedupe_hit_returns_same_record` | 正常 | 同 hash 第二次上传不写对象、不插行，返回同 id |
| `dedupe_concurrent_unique` | 边界 | 并发插同 hash：第二次命中 `uk_sys_file_hash` 后转查旧记录（不冒 500） |
| `router_fallback_to_default` | 正常 | 策略未指定 `storage_code` 时用默认后端 |
| `router_missing_backend_conflict` | 错误 | 记录的 `storage_code` 已停用 → 409 `STORAGE_BACKEND_UNAVAILABLE` |
| `crypto_roundtrip_and_wrong_key` | 正常+错误 | 加密→解密一致；错密钥返回 `None`（POC-3 已跑通） |
| `master_key_missing_blocks_startup` | 错误 | 主密钥缺失/长度错 → 启动守卫返回 `Err`（**不 panic**） |
| `mask_ak_never_leaks` | 错误 | 配置详情响应体不含完整 SK 字符串（断言序列化结果） |
| `delete_keeps_object_until_gc` | 边界 | 软删后 `exists()` 仍为真，GC 任务到期才删 |

**B. 导入导出（`business/impexp`）**

| 用例 | 类型 | 断言要点 |
|---|---|---|
| `template_headers_match_columns` | 正常 | 模板首行 = `columns().iter().map(header)` |
| `parse_roundtrip_xlsx` | 正常 | 写→读→行数与中文不乱码（POC-2 已跑通） |
| `validate_required_empty` | 错误 | 空串 / 全角空格 → `REQUIRED` |
| `validate_type_date_variants` | 边界 | `2026/10/28`、`2026-10-28`、Excel 序列号 三种日期写法等价 |
| `validate_enum_and_dict_translate` | 正常 | 「启用」→`1`；字典 label→value |
| `validate_unique_in_file` | 错误 | 同文件重复编码 → 第二条 `UNIQUE` 且 `row_no` 正确 |
| `validate_fk_batch_lookup` | 正常+性能 | 3 个外键列只发 3 次查询（用 `MockDb` 计数断言，防「逐行查库」回归） |
| `row_limit_rejects_200k_xlsx` | 错误 | 超 `max_rows` → 422 `IMPORT_ROW_LIMIT` |
| `strict_mode_aborts_on_first_batch_error` | 错误 | 第 2 批失败 → 任务 `FAILED`，`imported_rows` = 第 1 批行数 |
| `partial_mode_skips_bad_rows` | 边界 | 结果 `PARTIAL`，成功数 = 总行数 - 错误数 |
| `error_summary_capped_at_100` | 边界 | 150 条错误 → `error_summary` 长度 100，明细表 150 |
| `error_file_contains_row_numbers` | 正常 | 「错误行」xlsx 含原行号与原因列 |
| `export_worker_claims_once` | 并发 | 两个 worker 同时扫描只认领 1 条（`SKIP LOCKED`） |
| `export_progress_monotonic` | 边界 | 进度不回退、终值 100 |
| `export_expired_returns_410` | 错误 | `expire_at < now()` → 410 |
| `sync_export_threshold` | 边界 | 5 000 行走同步、5 001 行要求异步 |

**C. 消息通知（`business/notice`）**

| 用例 | 类型 | 断言要点 |
|---|---|---|
| `render_replaces_vars` | 正常 | `{{userName}}` → 张三；多变量、未声明空格容错 |
| `render_unknown_var_errors` | 错误 | 未知变量 → 422 `TEMPLATE_VAR_MISSING`（不静默为空） |
| `render_html_escapes_values` | 安全 | 变量含 `<script>` → 转义为 `&lt;script&gt;` |
| `render_length_limit` | 边界 | 渲染后超 255 字符 → 拒绝 |
| `fanout_user_role_dept_all` | 正常 | 四种目标各自命中人数正确（**POC-6 T9/T10 场景已固化**） |
| `fanout_excludes_disabled_and_deleted` | 边界 | 停用用户、软删用户不接收（POC-6 已验证） |
| `fanout_idempotent_replay` | 并发 | 同一 SQL 重放两次 → 接收行数不变（POC-6 T8） |
| `fanout_over_limit_rejects` | 错误 | >5 万接收人 → 422 `NOTICE_RECEIVER_LIMIT` |
| `idempotency_key_dedup` | 并发 | 同 key 第二次 dispatch 返回既有通知 id（POC-6 T8b） |
| `read_once_sets_time` | 正常 | 第二次已读不改 `read_at` |
| `read_flag_requires_time` | 错误 | 直接构造 `read_flag=true, read_at=None` 被 CHECK 拦（POC-6 T7） |
| `unread_count_excludes_deleted` | 边界 | 用户删除后未读数减 1 |
| `sse_channel_is_per_user` | 安全 | 用户 A 的 broadcaster 收不到用户 B 的 publish（**最关键的一条**） |
| `cancel_or_delete_notice_cascades` | 正常 | 删通知 → 接收行/投递记录清零（POC-6 T12） |
| `delivery_retry_backoff` | 边界 | 失败 3 次后不再重试，`status=FAILED` |

**测试基础设施（需在实施第 1 步就搭）**
- `business/tests/fixtures.rs`：`MemoryStore`（`ObjectStore` 内存实现）、`MockDb`（记录 SQL 调用次数，用于断言「批量查询而非逐行」）。
- 集成测试（真 PG）标记 `#[ignore]`，由 `scripts/check.sh --with-db` 单独跑：本批提供 `tests/sql/batchA_ddl.sql`（= §7 附录）+ `tests/sql/batchA_rules.sql`（= §8 的验证脚本），保证「设计与库结构不漂移」。

### 5.5 风险清单

| # | 类别 | 风险 | 触发条件 | 缓解措施（本批落地） | 残余风险 |
|---|---|---|---|---|---|
| R1 | 安全 | **上传 getshell**：伪装成图片的 `.php/.jsp/.sh` 落盘后被容器/反代执行 | 攻击者改扩展名或 MIME | ①扩展名白名单（策略表可配，仍强制下限）；②`X-Content-Type-Options: nosniff`；③下载恒为 `Content-Disposition: attachment`；④本地后端对象目录**不在静态资源根**（`nik::upload_path()` 由后端流式输出，nginx 不直挂）；⑤SVG 单独开关（SVG 可内嵌脚本，默认策略不含 svg） | 无病毒扫描 → B 批次接 ClamAV |
| R2 | 安全 | **SSRF**：`endpoint` 填内网地址（`http://169.254.169.254/`、Redis、PG）让后端去请求 | 管理员误配或低权账号配存储 | ①配置写权限只给超管（`business:storage:config:*`）；②保存时解析 `endpoint` 主机名并**拒绝私网/回环/链路本地地址**（`10/8`、`172.16/12`、`192.168/16`、`127/8`、`169.254/16`、`::1`、`fc00::/7`）；③「测试连接」也只允许已通过校验的 endpoint | 内网自建 MinIO 需例外白名单（在 `sys_config` 里显式允许的 host 才放开） |
| R3 | 安全 | **AK 泄露**：接口回显、日志、错误信息带明文 | 前端表单回填、异常堆栈 | ①只写不读（掩码 + `has_secret`）；②错误消息统一「认证失败」，不透传 SDK 原文（SDK 错误含 endpoint/AK 前缀）；③序列化层用 `#[serde(skip_serializing)]` 包一层 `Secret` 类型 | 内存中的明文密钥随进程 dump 泄露（操作系统级） |
| R4 | 安全 | **预签名链接外泄**：链接被转发后任何人在 TTL 内可下载 | 用户复制链接 | ①TTL 默认 900 s、上限 7 天；②下载接口每次现签，不落库；③敏感分类（策略里 `direct_upload=false` + 仅服务端代理下载） | 无 IP 绑定（S3 SigV4 不支持） |
| R5 | 安全 | **通知 XSS**：`content_format='HTML'` 由运营填写，变量来自用户输入 | 恶意真实姓名/文件名 | 入库前 `ammonia` 净化模板骨架 + 渲染时对变量值 HTML 转义（§4.6 规则 3） | 富文本编辑器引入后需重评 |
| R6 | 性能 | **大文件下载打爆内存**：旧接口 `std::fs::read` 全量 | 用户下载 1 GB 文件 | 新接口一律流式（`ReaderStream`）/302 到预签名；旧接口保留但文档标注「≤50 MB 适用」 | 旧接口仍可被恶意触发 → 网关层加单请求体积/时长限制 |
| R7 | 性能 | **大导出拖垮 DB**：10 万行一次 `SELECT` 全量 + 排序 | 运营高峰期导出 | keyset 分页（`ORDER BY id`）+ 每页 5 000 行 + 同时运行导出任务数上限 2（`AsyncTaskPlugin` 信号量）+ 单任务行数上限 100 万 | 复杂 `WHERE` 无索引时单页也慢 → 由各业务 `ExportDefinition::fetch` 自行保证索引 |
| R8 | 性能 | **扇出锁表**：5 万接收人单条 INSERT 长时间持锁 | 全员通知 | 单条通知上限 5 万；超出分批（每批 5 000）+ `ON CONFLICT DO NOTHING` 幂等；实测 1 万行 91.5 ms 可线性外推 | 极端打满时 `sys_notice_receiver` 写放大 |
| R9 | 一致性 | **异步任务失败无重试**：worker 崩了任务永远 `RUNNING` | 进程 OOM/部署重启 | ①认领即写 `started_at`；②自注册「僵尸任务回收」cron（`RUNNING` 且 `started_at < now()-30min` → 置 `FAILED` 并计数）；③`retry` 接口按原参数新建任务（保留原任务做审计） | 部分批次已入库 → 靠 `PARTIAL` 语义 + 业务 `persist` 幂等 |
| R10 | 一致性 | **批事务部分提交**：`STRICT` 模式也做不到「全成功」 | 中途失败 | 已在 §3.4 明示语义（提供已入库行数、失败批次范围）；不做 2PC | 业务需能容忍「部分入库」并按提示修数据 |
| R11 | 一致性 | **秒传记录被删**导致他人 file_id 失效 | 业务 A 删了共享文件 | 软删 + 延迟回收；`rebind` 接口 | 引用计数表留 B 批次 |
| R12 | 一致性 | **SSE 多副本不同步** | 部署 2 副本以上 | SSE 只做加速、轮询为权威（前端 30 s 兜底）；`publish` 单点预留 Redis Pub/Sub | 多副本下「实时」退化为「30 s 内」 |
| R13 | 兼容 | **迁移不可回滚**：`.down.sql` 写错导致回滚失败 | 上线失败需回退 | 每个 DDL 迁移配 `.down.sql`；`sys_file` 加列用 `ADD COLUMN IF NOT EXISTS`；**列不删只停用**（`status=0`） | 已写入的业务数据无法随 down 恢复 |
| R14 | 运维 | **主密钥丢失**：env 丢失/变更 → 全部存储配置解不开 | 部署脚本漏配 | 启动时校验 + 配置页显示「密钥版本 N」；主密钥与 JWT 密钥分开注入、分开备份 | 丢失即需人工重填 AK/SK（有文档 SOP） |

---

### 5.6 错误码新增清单（`6000x` 段，共 19 个）

底座现有码段：`1000x` 通用 / `2000x` 认证 / `3000x` 授权 / `4000x` 支撑域。本批新增业务段 `6000x`（避开 ERP 批次已规划的 `5000x`），全部追加到 `src/shared/error_code.rs`，**不改既有码值**。

| 常量 | 码值 | HTTP | 触发场景 |
|---|---|---|---|
| `STORAGE_BACKEND_UNAVAILABLE` | 60001 | 409 | 文件所在后端已停用/删除 |
| `STORAGE_BACKEND_DISABLED` | 60002 | 409 | 目标后端 `status=0` |
| `STORAGE_SIZE_EXCEEDED` | 60003 | 422 | 超策略 `max_size_mb` |
| `STORAGE_TYPE_NOT_ALLOWED` | 60004 | 400 | 扩展名不在策略白名单 |
| `STORAGE_MIME_NOT_ALLOWED` | 60005 | 400 | MIME 与扩展名/白名单不符 |
| `STORAGE_DIRECT_UPLOAD_DISABLED` | 60006 | 422 | 该分类未开直传（本地后端） |
| `STORAGE_OBJECT_MISSING` | 60007 | 404 | 登记时对象不存在（直传未真正完成） |
| `STORAGE_CONFIG_IN_USE` | 60008 | 409 | 删除被 `sys_file` 引用的后端 |
| `STORAGE_ENDPOINT_FORBIDDEN` | 60009 | 400 | endpoint 指向私网/回环（SSRF 防护） |
| `STORAGE_CREDENTIAL_INVALID` | 60010 | 422 | AK/SK 解密失败或探测 403（不区分，防探测） |
| `IMPORT_DEF_NOT_FOUND` | 60011 | 404 | `biz_type` 未注册 |
| `IMPORT_ROW_LIMIT` | 60012 | 422 | 超 `max_rows` 或超同步档上限 |
| `IMPORT_STATUS_INVALID` | 60013 | 409 | 状态机非法跃迁（如对 `SUCCESS` 再 confirm） |
| `IMPORT_BATCH_FAILED` | 60014 | 422 | 某批入库失败（message 带批次与行号范围） |
| `EXPORT_NOT_READY` | 60015 | 409 | 任务未完/失败时请求下载 |
| `EXPORT_EXPIRED` | 60016 | 410 | 超过 `expire_at` |
| `NOTICE_RECEIVER_LIMIT` | 60017 | 422 | 接收人超上限（默认 5 万） |
| `TEMPLATE_VAR_MISSING` | 60018 | 422 | 模板变量缺失/未知 |
| `TEMPLATE_RENDER_FAILED` | 60019 | 422 | 渲染后超长或 HTML 净化失败 |

---

## 6. 实施拆分（可独立交付的 7 步）

> 人日按「1 人 = 1 个熟练 Rust/TS 工程师」估算，含单测与自测，不含联调等待。**每步都可单独合并上线**，且每步都带「不改底座」的验证点。

| 步骤 | 交付物 | 验收标准（可执行） | 人日 | 依赖 |
|---|---|---|---|---|
| **A1 存储底座** | `business/storage/{store,router,sigv4,cipher,model,dto}.rs`；迁移 `V0040` | ①`cargo test` 中 `sigv4_*`、`crypto_*`、`policy_*`、`router_*` 全绿；②`V0040` 在真 PG 执行通过，`uk_sys_storage_config_default` 拦住第二个默认（脚本可复用 §8.2）；③配置 CRUD + 测试连接接口可用 | 6 | — |
| **A2 存储接口与兼容** | `business/storage/handler.rs`（上传/直传/下载/软删/rebind）+ `V0048` 默认配置与策略 | ①旧 `/support/file/*` 全部回归用例不变（手工 + 冒烟脚本）；②新 `/business/storage/*` 上传→下载闭环；③本地后端下载为流式（1 GB 文件 RSS 增长 <200 MB 实测）；④S3（MinIO 容器）直传闭环 | 5 | A1 |
| **A3 导入导出引擎** | `business/impexp/{definition,registry,engine,validate,template}.rs` + `V0041`/`V0042` | ①定义注册后可下载模板（表头/示例/说明三要素齐）；②预检产出正确 `total/valid/error` 与明细（含行号列名）；③外键批量校验 SQL 次数 = 外键列数（断言）；④10 万行 xlsx 预检 ≤ 15 s、峰值 RSS ≤ 400 MB | 8 | A1 |
| **A4 任务与 worker** | `business/impexp/{worker,cleanup}.rs` + 确认入库/取消/重试/进度/清理；`bootstrap.rs` 3 行注册（需批准） | ①异步导出 10 万行端到端跑通，进度单调；②两 worker 并发只认领一次（`SKIP LOCKED`）；③僵尸任务 30 min 回收；④过期下载 410 | 5 | A3 |
| **A5 通知底座** | `business/notice/{model,handler,dto,render,fanout}.rs` + `V0043`/`V0044`/`V0045`/`V0049` | ①四种目标扇出人数与 §8 脚本一致；②模板渲染 5 条规则单测全绿；③未读数查询命中部分索引（`EXPLAIN` 断言 `Bitmap Heap Scan`）；④与 C/D 的 `notice::dispatch` 可被外部模块调用（编译期示例） | 6 | A1 |
| **A6 SSE 与通道预留** | `business/notice/{sse_hub,delivery}.rs` | ①per-user 隔离用例通过（A 收不到 B）；②断连后 `receiver_count` 归零；③通道未配置时投递记录为 `SKIPPED` 而非失败 | 3 | A5 |
| **A7 前端 8 页 + 菜单联调** | `frontend/src/{api,views}/business/{storage,impexp,notice}/` + `V0046`/`V0047` | ①`pnpm typecheck` + `pnpm build` 通过；②每页完成「列表-新增-编辑-删除-权限隐藏」五件事；③非超管账号能看消息中心（权限种子正确）；④直传进度条真实可用（100 MB 文件） | 8 | A2/A4/A5 |
| **合计** | — | — | **41 人日** | — |

**串并行建议**：A1 → （A2 ∥ A3）→ （A4 ∥ A5）→ A6 → A7。A7 可与 A4/A5 并行开工（前端先按契约 mock，接口就绪后切换）。**关键路径 = A1 → A3 → A4 → A7 ≈ 27 人日**。

**需批准的底座最小改动（一次性清单，共 3 处、均无逻辑改写）**
1. `src/bootstrap.rs`：末尾追加 3 行（`business::impexp::worker::register_tasks()`、`business::storage::cleanup::register_task()`、`business::notice::cleanup::register_task()`）。原因：框架调度器一次性快照，必须在 `on_startup`（插件启动前）注册。
2. `src/shared/error_code.rs`：追加 `6000x` 段常量（本批 19 个错误码，清单见 §5.6），不动既有码值。原因：项目约定「错误码统一在此定义，禁止散落魔法数字」。
3. `src/shared/startup_guard.rs`：追加主密钥校验（与现有 JWT 密钥校验同款范式，约 20 行）。原因：主密钥必须在 `App::new()` 前就拒绝启动，该文件是唯一的启动前检查点。

若上述 3 处均不批准：替代方案是「业务自带 `OnceLock` 初始化 + 由 `/health` 首次探测触发 + 定时任务改外部 systemd timer 调管理端点」，代价是**启动期无法拒绝主密钥缺失**、任务调度依赖外部 cron，可靠性下降一档。

---
## 7. 附录 A：完整 DDL（已在 PostgreSQL 18.6 实跑验证）

> 本附录即为 §8.1 验证所用**原文**。落地时按 §5.2 拆成 `V0040`–`V0045` 六个 up/down 对；开头「底座替身」四张表（`sys_user/sys_dept/sys_role/sys_user_role/sys_file`）**仅验证脚本需要**，正式迁移中不存在（真实库里已有）。

```sql
-- ============================================================
-- 云枢 批次 A 通用底座 DDL（V0040–V0045 建表部分，真实 PG 18.6 验证用）
-- ============================================================
-- 说明：验证库内不建 sys_file / sys_user / sys_dept 等底座表的完整定义，
--       只建最小替身（列集合与真实迁移一致），用于验证 ALTER 与扇出 SQL。

-- ---------- 底座替身（仅验证用，真实迁移已存在） ----------
CREATE TABLE sys_user (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    username VARCHAR(50) NOT NULL,
    real_name VARCHAR(50),
    email VARCHAR(100),
    mobile VARCHAR(20),
    dept_id UUID,
    status SMALLINT NOT NULL DEFAULT 1,
    deleted_at TIMESTAMPTZ
);
CREATE TABLE sys_dept (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    dept_code VARCHAR(50) NOT NULL,
    dept_name VARCHAR(100) NOT NULL,
    parent_id UUID,
    dept_path VARCHAR(500),
    status SMALLINT NOT NULL DEFAULT 1,
    deleted_at TIMESTAMPTZ
);
CREATE TABLE sys_role (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    role_code VARCHAR(50) NOT NULL,
    role_name VARCHAR(100) NOT NULL,
    status SMALLINT NOT NULL DEFAULT 1,
    deleted_at TIMESTAMPTZ
);
CREATE TABLE sys_user_role (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES sys_user(id),
    role_id UUID NOT NULL REFERENCES sys_role(id),
    expire_time TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (user_id, role_id)
);
CREATE TABLE sys_file (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    file_name VARCHAR(255) NOT NULL,
    original_name VARCHAR(255),
    file_key VARCHAR(500) NOT NULL,
    file_size BIGINT NOT NULL,
    file_hash VARCHAR(64),
    mime_type VARCHAR(100),
    extension VARCHAR(50),
    storage_path VARCHAR(500),
    category VARCHAR(50),
    status SMALLINT NOT NULL DEFAULT 1,
    uploaded_by UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX uk_sys_file_key ON sys_file (file_key) WHERE deleted_at IS NULL;

-- ============================================================
-- V0040：对象存储后端配置与上传策略
-- ============================================================

CREATE TABLE sys_storage_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    config_code VARCHAR(64) NOT NULL,
    config_name VARCHAR(100) NOT NULL,
    backend_type VARCHAR(16) NOT NULL,
    endpoint VARCHAR(255),
    bucket VARCHAR(128),
    region VARCHAR(64) NOT NULL DEFAULT 'us-east-1',
    base_path VARCHAR(255) NOT NULL DEFAULT '',
    use_tls BOOLEAN NOT NULL DEFAULT TRUE,
    path_style BOOLEAN NOT NULL DEFAULT FALSE,
    ak_cipher TEXT,
    ak_nonce VARCHAR(64),
    sk_cipher TEXT,
    sk_nonce VARCHAR(64),
    key_version SMALLINT NOT NULL DEFAULT 1,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    status SMALLINT NOT NULL DEFAULT 1,
    health_status VARCHAR(16) NOT NULL DEFAULT 'UNKNOWN',
    last_check_at TIMESTAMPTZ,
    last_error VARCHAR(500),
    sort_order INT NOT NULL DEFAULT 0,
    remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_storage_config_backend CHECK (backend_type IN ('LOCAL', 'S3')),
    CONSTRAINT ck_sys_storage_config_health CHECK (health_status IN ('UNKNOWN', 'HEALTHY', 'DOWN')),
    -- 远端后端必须给全 endpoint / bucket / 双密钥密文，本地后端必须为空（密钥不外泄到本地行）
    CONSTRAINT ck_sys_storage_config_remote CHECK (
        (backend_type = 'LOCAL'
            AND endpoint IS NULL AND (bucket IS NULL OR bucket = '')
            AND ak_cipher IS NULL AND sk_cipher IS NULL)
        OR
        (backend_type = 'S3'
            AND endpoint IS NOT NULL AND length(btrim(endpoint)) > 0
            AND bucket IS NOT NULL AND length(btrim(bucket)) > 0
            AND ak_cipher IS NOT NULL AND ak_nonce IS NOT NULL
            AND sk_cipher IS NOT NULL AND sk_nonce IS NOT NULL)
    ),
    CONSTRAINT ck_sys_storage_config_code CHECK (config_code ~ '^[a-z][a-z0-9_-]{1,63}$')
);

COMMENT ON TABLE sys_storage_config IS '存储后端配置：本地磁盘 / S3 兼容（MinIO、AWS S3、阿里 OSS），AK/SK 只存国密级密文';
COMMENT ON COLUMN sys_storage_config.config_code IS '后端编码（小写字母开头，路由键，写入 sys_file.storage_code）';
COMMENT ON COLUMN sys_storage_config.backend_type IS '后端类型：LOCAL 本地磁盘 / S3 S3 兼容对象存储';
COMMENT ON COLUMN sys_storage_config.base_path IS '对象键前缀（如 public/），空串表示桶根';
COMMENT ON COLUMN sys_storage_config.path_style IS '是否 path 风格寻址（MinIO 必须 true，AWS S3/OSS 用 false）';
COMMENT ON COLUMN sys_storage_config.ak_cipher IS 'AccessKey 密文（AES-256-GCM，Base64）';
COMMENT ON COLUMN sys_storage_config.ak_nonce IS 'AccessKey 加密随机数（Base64，每条独立）';
COMMENT ON COLUMN sys_storage_config.sk_cipher IS 'SecretKey 密文（AES-256-GCM，Base64）';
COMMENT ON COLUMN sys_storage_config.sk_nonce IS 'SecretKey 加密随机数（Base64）';
COMMENT ON COLUMN sys_storage_config.key_version IS '主密钥版本号，用于主密钥轮换（旧行保留旧版本即可解密）';
COMMENT ON COLUMN sys_storage_config.health_status IS '连通性探测结果：UNKNOWN / HEALTHY / DOWN';

CREATE UNIQUE INDEX uk_sys_storage_config_code ON sys_storage_config (config_code) WHERE deleted_at IS NULL;
-- 全局最多一个默认后端：把「唯一默认」交给数据库，不依赖应用层判断
CREATE UNIQUE INDEX uk_sys_storage_config_default ON sys_storage_config ((is_default))
    WHERE is_default AND deleted_at IS NULL;
CREATE INDEX idx_sys_storage_config_status ON sys_storage_config (status, sort_order) WHERE deleted_at IS NULL;

CREATE TABLE sys_storage_policy (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    category VARCHAR(50) NOT NULL,
    category_name VARCHAR(100) NOT NULL,
    max_size_mb INT NOT NULL DEFAULT 50,
    allowed_extensions TEXT NOT NULL DEFAULT '',
    allowed_mimes TEXT NOT NULL DEFAULT '',
    allow_overwrite BOOLEAN NOT NULL DEFAULT FALSE,
    direct_upload BOOLEAN NOT NULL DEFAULT FALSE,
    presign_ttl_secs INT NOT NULL DEFAULT 900,
    storage_code VARCHAR(64),
    status SMALLINT NOT NULL DEFAULT 1,
    sort_order INT NOT NULL DEFAULT 0,
    remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_storage_policy_size CHECK (max_size_mb BETWEEN 1 AND 5120),
    CONSTRAINT ck_sys_storage_policy_ttl CHECK (presign_ttl_secs BETWEEN 60 AND 604800),
    CONSTRAINT ck_sys_storage_policy_ext CHECK (allowed_extensions NOT LIKE '%.%')
);

COMMENT ON TABLE sys_storage_policy IS '上传策略：按业务分类配置大小上限、扩展名/MIME 白名单、是否直传与覆盖';
COMMENT ON COLUMN sys_storage_policy.category IS '业务分类（与 sys_file.category 一一对应，如 attachment / avatar / import）';
COMMENT ON COLUMN sys_storage_policy.allowed_extensions IS '扩展名白名单，小写逗号分隔且不带点（如 jpg,png,pdf）；空串表示沿用全局默认';
COMMENT ON COLUMN sys_storage_policy.allowed_mimes IS 'MIME 白名单，逗号分隔；空串表示不校验 MIME';
COMMENT ON COLUMN sys_storage_policy.direct_upload IS '是否允许浏览器预签名直传（大文件必开）';
COMMENT ON COLUMN sys_storage_policy.presign_ttl_secs IS '预签名 URL 有效期（秒）';
COMMENT ON COLUMN sys_storage_policy.storage_code IS '指定后端编码（NULL 表示使用默认后端）';

CREATE UNIQUE INDEX uk_sys_storage_policy_category ON sys_storage_policy (category) WHERE deleted_at IS NULL;

-- sys_file 扩展：记录对象落在哪个后端 + 是否直传（底座最小改动，两列均可空、向后兼容）
ALTER TABLE sys_file ADD COLUMN IF NOT EXISTS storage_code VARCHAR(64);
ALTER TABLE sys_file ADD COLUMN IF NOT EXISTS upload_mode VARCHAR(16) NOT NULL DEFAULT 'SERVER';
ALTER TABLE sys_file ADD CONSTRAINT ck_sys_file_upload_mode CHECK (upload_mode IN ('SERVER', 'DIRECT'));
-- 跨后端秒传：同一内容哈希只保留一条有效记录（含它所在后端），命中即复用，不再复制对象
CREATE UNIQUE INDEX uk_sys_file_hash ON sys_file (file_hash)
    WHERE file_hash IS NOT NULL AND deleted_at IS NULL;
CREATE INDEX idx_sys_file_storage_code ON sys_file (storage_code);

-- ============================================================
-- V0041：导入任务 + 行级错误明细
-- ============================================================

CREATE TABLE sys_import_task (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_no VARCHAR(32) NOT NULL,
    biz_type VARCHAR(64) NOT NULL,
    biz_name VARCHAR(100) NOT NULL,
    module_code VARCHAR(32) NOT NULL DEFAULT 'business',
    file_id UUID,
    file_name VARCHAR(255),
    file_size BIGINT,
    mode VARCHAR(16) NOT NULL DEFAULT 'STRICT',
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING',
    total_rows INT NOT NULL DEFAULT 0,
    valid_rows INT NOT NULL DEFAULT 0,
    error_rows INT NOT NULL DEFAULT 0,
    imported_rows INT NOT NULL DEFAULT 0,
    progress SMALLINT NOT NULL DEFAULT 0,
    error_summary JSONB,
    options JSONB,
    result_file_id UUID,
    message VARCHAR(500),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_import_task_status CHECK (
        status IN ('PENDING', 'PARSING', 'VALIDATED', 'IMPORTING', 'SUCCESS', 'PARTIAL', 'FAILED', 'CANCELED')),
    CONSTRAINT ck_sys_import_task_mode CHECK (mode IN ('STRICT', 'PARTIAL')),
    CONSTRAINT ck_sys_import_task_progress CHECK (progress BETWEEN 0 AND 100),
    CONSTRAINT ck_sys_import_task_rows CHECK (total_rows >= 0 AND valid_rows >= 0 AND error_rows >= 0
                                              AND imported_rows >= 0 AND valid_rows + error_rows <= total_rows)
);

COMMENT ON TABLE sys_import_task IS '导入任务：一次 Excel/CSV 导入的全生命周期（解析 → 校验 → 入库），可按 biz_type 复用于任意业务';
COMMENT ON COLUMN sys_import_task.biz_type IS '导入定义编码（由业务模块注册，如 erp.material / system.user）';
COMMENT ON COLUMN sys_import_task.mode IS 'STRICT 全通过才入库 / PARTIAL 跳过错误行入库';
COMMENT ON COLUMN sys_import_task.status IS 'PENDING 待处理 / PARSING 解析中 / VALIDATED 校验完成待确认 / IMPORTING 入库中 / SUCCESS / PARTIAL 部分成功 / FAILED / CANCELED';
COMMENT ON COLUMN sys_import_task.error_summary IS '前 100 条错误摘要（JSONB，用于列表页直接回显，明细见 sys_import_task_error）';
COMMENT ON COLUMN sys_import_task.options IS '导入选项快照（字典翻译开关、冲突策略等），保证任务可复现';

CREATE UNIQUE INDEX uk_sys_import_task_no ON sys_import_task (task_no);
CREATE INDEX idx_sys_import_task_claim ON sys_import_task (created_at)
    WHERE status IN ('PENDING', 'VALIDATED', 'IMPORTING');
CREATE INDEX idx_sys_import_task_creator ON sys_import_task (create_by, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_import_task_biz ON sys_import_task (biz_type, created_at DESC) WHERE deleted_at IS NULL;

CREATE TABLE sys_import_task_error (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_id UUID NOT NULL REFERENCES sys_import_task(id) ON DELETE CASCADE,
    row_no INT NOT NULL,
    column_key VARCHAR(64),
    column_header VARCHAR(100),
    raw_value TEXT,
    error_code VARCHAR(32) NOT NULL,
    error_message VARCHAR(500) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

COMMENT ON TABLE sys_import_task_error IS '导入错误明细：行号 + 列 + 原因，供前端逐行回显与导出「错误行」Excel';
COMMENT ON COLUMN sys_import_task_error.row_no IS 'Excel 行号（含表头，从 1 开始，与用户在 Excel 里看到的行号一致）';
COMMENT ON COLUMN sys_import_task_error.column_key IS '列键（业务定义里的字段名，整行级错误为空）';
COMMENT ON COLUMN sys_import_task_error.error_code IS '错误码：REQUIRED / TYPE / ENUM / DICT / UNIQUE / FK_NOT_FOUND / RULE / ROW_LIMIT';

CREATE INDEX idx_sys_import_task_error_task ON sys_import_task_error (task_id, row_no);

-- ============================================================
-- V0042：导出任务
-- ============================================================

CREATE TABLE sys_export_task (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    task_no VARCHAR(32) NOT NULL,
    biz_type VARCHAR(64) NOT NULL,
    biz_name VARCHAR(100) NOT NULL,
    module_code VARCHAR(32) NOT NULL DEFAULT 'business',
    export_name VARCHAR(200) NOT NULL,
    format VARCHAR(8) NOT NULL DEFAULT 'XLSX',
    query_params JSONB,
    columns_snapshot JSONB,
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING',
    total_rows BIGINT NOT NULL DEFAULT 0,
    exported_rows BIGINT NOT NULL DEFAULT 0,
    progress SMALLINT NOT NULL DEFAULT 0,
    file_id UUID,
    file_name VARCHAR(255),
    file_size BIGINT,
    expire_at TIMESTAMPTZ,
    message VARCHAR(500),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_export_task_status CHECK (
        status IN ('PENDING', 'RUNNING', 'SUCCESS', 'FAILED', 'CANCELED', 'EXPIRED')),
    CONSTRAINT ck_sys_export_task_format CHECK (format IN ('XLSX', 'CSV')),
    CONSTRAINT ck_sys_export_task_progress CHECK (progress BETWEEN 0 AND 100),
    CONSTRAINT ck_sys_export_task_rows CHECK (total_rows >= 0 AND exported_rows >= 0)
);

COMMENT ON TABLE sys_export_task IS '导出任务：大数据量异步导出（小数据量走同步接口，不落表）';
COMMENT ON TABLE sys_export_task IS '导出任务：大数据量异步导出（小数据量走同步接口，不落表）';
COMMENT ON COLUMN sys_export_task.columns_snapshot IS '导出列定义快照（字段/表头/字典翻译），保证任务完成后列语义不随定义变更而错位';
COMMENT ON COLUMN sys_export_task.expire_at IS '下载有效期截止时间，过期后文件由清理任务回收';
COMMENT ON COLUMN sys_export_task.format IS 'XLSX（≤20 万行）/ CSV（超 20 万行或纯数据交换）';

CREATE UNIQUE INDEX uk_sys_export_task_no ON sys_export_task (task_no);
CREATE INDEX idx_sys_export_task_claim ON sys_export_task (created_at) WHERE status = 'PENDING';
CREATE INDEX idx_sys_export_task_creator ON sys_export_task (create_by, created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_export_task_expire ON sys_export_task (expire_at) WHERE status = 'SUCCESS';

-- ============================================================
-- V0043：通知主体 + 模板
-- ============================================================

CREATE TABLE sys_notice_template (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_code VARCHAR(64) NOT NULL,
    template_name VARCHAR(100) NOT NULL,
    notice_type VARCHAR(32) NOT NULL DEFAULT 'SYSTEM',
    default_level VARCHAR(16) NOT NULL DEFAULT 'INFO',
    title_template VARCHAR(255) NOT NULL,
    content_template TEXT NOT NULL,
    content_format VARCHAR(8) NOT NULL DEFAULT 'TEXT',
    link_template VARCHAR(500),
    variables JSONB,
    status SMALLINT NOT NULL DEFAULT 1,
    remark TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_notice_template_format CHECK (content_format IN ('TEXT', 'HTML', 'MARKDOWN')),
    CONSTRAINT ck_sys_notice_template_level CHECK (default_level IN ('INFO', 'SUCCESS', 'WARN', 'ERROR'))
);

COMMENT ON TABLE sys_notice_template IS '通知模板：{{变量}} 占位符 + 变量声明，发送时渲染为最终标题/正文';
COMMENT ON COLUMN sys_notice_template.variables IS '变量声明 [{name,label,required,sample}]，用于模板编辑页校验与示例渲染';
COMMENT ON COLUMN sys_notice_template.content_format IS '正文格式：TEXT 纯文本 / HTML（入库前净化）/ MARKDOWN';

CREATE UNIQUE INDEX uk_sys_notice_template_code ON sys_notice_template (template_code) WHERE deleted_at IS NULL;

CREATE TABLE sys_notice (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notice_no VARCHAR(32) NOT NULL,
    notice_type VARCHAR(32) NOT NULL DEFAULT 'SYSTEM',
    level VARCHAR(16) NOT NULL DEFAULT 'INFO',
    title VARCHAR(255) NOT NULL,
    content TEXT NOT NULL,
    content_format VARCHAR(8) NOT NULL DEFAULT 'TEXT',
    link_url VARCHAR(500),
    biz_type VARCHAR(64),
    biz_id VARCHAR(64),
    template_code VARCHAR(64),
    template_vars JSONB,
    sender_id UUID,
    sender_name VARCHAR(50),
    target_type VARCHAR(16) NOT NULL,
    target_ids JSONB,
    channel_mask SMALLINT NOT NULL DEFAULT 1,
    receiver_count INT NOT NULL DEFAULT 0,
    read_count INT NOT NULL DEFAULT 0,
    idempotency_key VARCHAR(128),
    status SMALLINT NOT NULL DEFAULT 1,
    expire_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_notice_level CHECK (level IN ('INFO', 'SUCCESS', 'WARN', 'ERROR')),
    CONSTRAINT ck_sys_notice_target CHECK (target_type IN ('USER', 'ROLE', 'DEPT', 'ALL')),
    CONSTRAINT ck_sys_notice_format CHECK (content_format IN ('TEXT', 'HTML', 'MARKDOWN')),
    CONSTRAINT ck_sys_notice_channel CHECK (channel_mask BETWEEN 1 AND 15),
    CONSTRAINT ck_sys_notice_counts CHECK (receiver_count >= 0 AND read_count >= 0 AND read_count <= receiver_count)
);

COMMENT ON TABLE sys_notice IS '通知主体：一条通知 = 一次业务事件，接收人落在 sys_notice_receiver';
COMMENT ON COLUMN sys_notice.target_type IS '发送目标类型：USER 指定用户 / ROLE 角色 / DEPT 部门（含下级）/ ALL 全员';
COMMENT ON COLUMN sys_notice.target_ids IS '目标 ID 数组（UUID 字符串），ALL 时为空数组；仅留痕，实际接收人已扇出到 receiver 表';
COMMENT ON COLUMN sys_notice.channel_mask IS '通道位掩码：1 站内信 / 2 邮件 / 4 Webhook / 8 短信（本批只实现 1，其余留投递表与配置）';
COMMENT ON COLUMN sys_notice.idempotency_key IS '幂等键（业务事件唯一标识），同一键只发一次通知';

CREATE UNIQUE INDEX uk_sys_notice_no ON sys_notice (notice_no);
CREATE UNIQUE INDEX uk_sys_notice_idem ON sys_notice (idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX idx_sys_notice_created ON sys_notice (created_at DESC) WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_notice_biz ON sys_notice (biz_type, biz_id) WHERE biz_id IS NOT NULL;

-- ============================================================
-- V0044：通知接收与已读状态
-- ============================================================

CREATE TABLE sys_notice_receiver (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notice_id UUID NOT NULL REFERENCES sys_notice(id) ON DELETE CASCADE,
    user_id UUID NOT NULL,
    read_flag BOOLEAN NOT NULL DEFAULT FALSE,
    read_at TIMESTAMPTZ,
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_sys_notice_receiver_read CHECK ((read_flag = FALSE AND read_at IS NULL)
                                                  OR (read_flag = TRUE AND read_at IS NOT NULL))
);

COMMENT ON TABLE sys_notice_receiver IS '通知接收人：扇出后的每用户一行，承载已读状态与用户侧删除';
COMMENT ON COLUMN sys_notice_receiver.is_deleted IS '用户侧删除（不影响其他接收人，物理行保留以便统计送达率）';

-- 扇出幂等：同一通知对同一用户只允许一行，重放安全
CREATE UNIQUE INDEX uk_sys_notice_receiver ON sys_notice_receiver (notice_id, user_id);
-- 未读数高频查询：覆盖索引 + 部分索引，只索引未读行
CREATE INDEX idx_sys_notice_receiver_unread ON sys_notice_receiver (user_id, notice_id)
    WHERE read_flag = FALSE AND is_deleted = FALSE;
-- 我的通知列表：按时间倒序分页
CREATE INDEX idx_sys_notice_receiver_list ON sys_notice_receiver (user_id, created_at DESC)
    WHERE is_deleted = FALSE;

-- ============================================================
-- V0045：通知多通道投递（邮件 / Webhook / 短信，本批只落状态不实现发送）
-- ============================================================

CREATE TABLE sys_notice_delivery (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notice_id UUID NOT NULL REFERENCES sys_notice(id) ON DELETE CASCADE,
    receiver_id UUID REFERENCES sys_notice_receiver(id) ON DELETE CASCADE,
    channel VARCHAR(16) NOT NULL,
    target VARCHAR(255) NOT NULL,
    status VARCHAR(16) NOT NULL DEFAULT 'PENDING',
    attempts SMALLINT NOT NULL DEFAULT 0,
    max_attempts SMALLINT NOT NULL DEFAULT 3,
    next_retry_at TIMESTAMPTZ,
    last_error VARCHAR(500),
    sent_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT ck_sys_notice_delivery_channel CHECK (channel IN ('EMAIL', 'WEBHOOK', 'SMS')),
    CONSTRAINT ck_sys_notice_delivery_status CHECK (
        status IN ('PENDING', 'SENDING', 'SENT', 'FAILED', 'SKIPPED')),
    CONSTRAINT ck_sys_notice_delivery_attempts CHECK (attempts >= 0 AND attempts <= max_attempts)
);

COMMENT ON TABLE sys_notice_delivery IS '外部通道投递记录：本批只预留接口与状态机，不实现真实发送';
COMMENT ON COLUMN sys_notice_delivery.next_retry_at IS '下次重试时间（指数退避：1min / 5min / 25min）';

CREATE UNIQUE INDEX uk_sys_notice_delivery ON sys_notice_delivery (notice_id, COALESCE(receiver_id, notice_id), channel);
CREATE INDEX idx_sys_notice_delivery_retry ON sys_notice_delivery (next_retry_at)
    WHERE status IN ('PENDING', 'FAILED');
```
## 8. 附录 B：验证记录

### 8.1 DDL 在真实 PostgreSQL 18.6 的验证结果

执行方式：`psql -d batcha_v2 -v ON_ERROR_STOP=1 -f batchA_ddl.sql` 然后 `-f batchA_verify.sql`，**28 条断言全部 PASS、退出码 0**。完整脚本已归档到工作区 `batchA_verify.sql`（可直接进仓库 `backend/tests/sql/`）。

| 编号 | 断言 | 期望 | 实际结果 |
|---|---|---|---|
| T1 | 插入第二个默认后端 | 拒绝 | ✅ `duplicate key ... uk_sys_storage_config_default` |
| T2 | S3 后端缺 AK/SK 密文 | 拒绝 | ✅ `ck_sys_storage_config_remote` |
| T2b | 带密文的 S3 后端 | 接受 | ✅ |
| T3 | 本地后端携带密钥 | 拒绝 | ✅ `ck_sys_storage_config_remote` |
| T4 | `backend_type='OSS'` | 拒绝 | ✅ `ck_sys_storage_config_backend` |
| T4b | `config_code='Bad_Code'` | 拒绝 | ✅ `ck_sys_storage_config_code` |
| T5 | `max_size_mb=0` | 拒绝 | ✅ `ck_sys_storage_policy_size` |
| T5b | 扩展名写 `.png,.jpg` | 拒绝 | ✅ `ck_sys_storage_policy_ext` |
| T6 | 同 `file_hash` 第二条有效记录 | 拒绝 | ✅ `uk_sys_file_hash` |
| T6b | 软删后同 hash 重新入库 | 接受 | ✅ |
| T6c | `upload_mode='FTP'` | 拒绝 | ✅ `ck_sys_file_upload_mode` |
| T7 | `read_flag=TRUE` 且 `read_at IS NULL` | 拒绝 | ✅ `ck_sys_notice_receiver_read` |
| T7b | `read_count > receiver_count` | 拒绝 | ✅ `ck_sys_notice_counts` |
| T7c | `channel_mask=16` | 拒绝 | ✅ `ck_sys_notice_channel` |
| T8 | 扇出 SQL 重放（`ON CONFLICT DO NOTHING`） | 行数不变 | ✅ |
| T8b | 同 `idempotency_key` 重复通知 | 拒绝 | ✅ `uk_sys_notice_idem` |
| T9 | 部门（含下级）扇出 | 3 人（排除停用/软删） | ✅ 3 人 |
| T10 | 角色扇出 | 1 人（排除软删） | ✅ 1 人 |
| T10b | 全员扇出 | 3 人（排除软删与停用） | ✅ 3 人（首次写期望 4 是**测试脚本自身口径错**，按真实用户集合修正为 3，非设计缺陷） |
| T11 | 10 003 接收人扇出耗时 | < 1 s | ✅ **91.5 ms** |
| T11b | 未读数查询计划 | 用部分索引 | ✅ `Bitmap Heap Scan`，**0.55 ms** |
| T11c | 我的通知列表计划 | 用列表索引 | ✅ `Hash Join` + Limit，0.42 ms |
| T12 | 删通知级联 | 接收行/投递记录清零 | ✅ 10 003 → 0 |
| T13 | 非法导入状态 `DONE` | 拒绝 | ✅ `ck_sys_import_task_status` |
| T13b | `valid+error > total` | 拒绝 | ✅ `ck_sys_import_task_rows` |
| T13c | 1 000 行错误明细批量写 | 成功 | ✅ |
| T14 | 导出格式 `PDF` | 拒绝 | ✅ `ck_sys_export_task_format` |
| T14b | 删导入任务级联错误明细 | 清零 | ✅ |

### 8.2 POC 清单与清理

POC 全部在**工作区之外**的临时目录，不属于交付物，验证完即清理（任务书要求「POC 需说明并清理」）：

| POC | 路径 | 内容 | 状态 |
|---|---|---|---|
| POC-CRATE | `/tmp/poc-batchA-archive/poc-a/`（已移出工作区） | `Cargo.toml`（nik 全 feature）+ `src/main.rs`（8 个测试）+ `src/sigv4.rs`（预签名实现与 SDK 对拍） | 已跑通 8/8，源码暂存 `/tmp` 随系统临时目录回收，日志见 `batchA_poc_test.log` |
| POC-DB | PostgreSQL 库 `batcha_v1`、`batcha_v2` | `batchA_ddl.sql` + `batchA_verify.sql` | 验证完成，**两个库已 DROP**（已核实 `pg_database` 无残留） |
| 工具链 | `rustup toolchain 1.94.1` | 因为项目 MSRV 1.94.1 > 本机初始 1.94.0 | 保留（实施也需要） |

产物归档（留在工作区、作为实施依据）：
- `cloudpivot-batchA-architecture.md`（本文档）
- `batchA_ddl.sql`（= §7 附录，可直接拆成 V0040–V0045）
- `batchA_verify.sql`（= §8.1 断言脚本，建议进 `backend/tests/sql/`）

### 8.3 结论

1. **对象存储**：不需要新依赖即可覆盖本地磁盘 + MinIO + AWS S3 + 阿里 OSS；框架缺口只有「上传预签名」，已用项目已有依赖实现并与官方 SDK 对拍一致。
2. **导入导出**：Excel 能力框架已内建，10 万行实测 3 s 读 / 5 s 写 / 220 MB 内存，配合「三档阈值 + 分批事务」可控；错误回显走「JSONB 摘要 + 明细表」双写。
3. **消息通知**：扇出、未读、幂等、级联四条路径都在真库上被约束和索引兜住；实时推送用 SSE（per-user 频道），不为红点引入 WebSocket。
4. **可落地性**：33 个权限码、10 个迁移、8 个前端页面、41 人日、7 步可独立交付；对底座的改动压缩到 3 处、共约 25 行且无逻辑改写。
