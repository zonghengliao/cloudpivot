# 云枢（CloudPivot）批次 A —— 通用底座 产品需求文档（PRD）
## 对象存储扩展 · 导入导出 · 消息通知

| 项 | 内容 |
|---|---|
| 文档版本 | v1.0（评审稿） |
| 撰写人 | product-manager |
| 日期 | 2026-10-28 |
| 产品 | 云枢（CloudPivot）后台 · **批次 A：通用底座** |
| 技术底座 | Rust（Alun 0.4.2 框架）+ Vue 3 + TDesign Vue Next 1.19 + Pinia + Vite；平台库 PostgreSQL 16；前端配色 **TDesign + 可切换配色（默认「雾蓝流金」）** |
| 本批范围 | ① 对象存储扩展（存储后端配置 / 上传策略 / 大文件直传 / 私有文件 / 文件管理增强 / AK·SK 管理）② 导入导出（模板 / 校验 / 入库 / 同步与异步导出）③ 消息通知（消息中心 / 未读角标 / 发送 / 类型 / 模板 / 多通道预留 / 保留） |
| **不含范围** | B 批次权限增强、C 批次内容管理、D 批次工作流审批 C 档（本批**只提供**通知与导入导出的内部调用契约，不设计其业务流程）；不做视频转码、不做 OCR、不做全文检索、不做 Excel 公式计算、不做跨库数据抽取 |
| 交付物 | `/root/.hermes/workspace/product-manager/cloudpivot-batchA-prd.md` |
| 下游服务对象 | architect（存储路由、异步任务、消息扇出 3 处架构决策）、backend-developer（3 个模块接口 + 存量 `/support/file` 改造）、frontend-developer（9 个页面 + 顶栏铃铛）、test-enginner（第九章 6 个验收场景可直接转用例） |

## 阅读指引

- **开发**：先读**第 0 章**（31 条已核查的代码事实 F1–F31 + 11 个已定口径 D1–D11）。这 42 条是硬约束，不要重新讨论；再读**第三章 3.2 / 3.6 / 3.7**（存储路由、私有链接、AK·SK）与**第四章 4.2 / 4.7**（校验规则、异步任务状态机），这四处是最容易做错的地方。
- **测试**：直接读**第九章**，6 个场景每步都有明确数字与文案；报错文案统一查**附录 A**（共 67 条）。
- **老大/评审**：读**第一章**（谁用、怎么用、什么场景下会骂人）+ **第二章三张优先级总表**（做什么、什么先不做）+ **附录 B**（需要你拍的 8 个决策）。
- **架构**：读 3.2（存储路由与旧文件兼容）、3.6（私有链接在本地后端上没有预签名怎么实现）、4.7（不引 Kafka 的异步任务）、5.4（120 人扇出与去重）、附录 C（非功能需求与交付前提）。

> 本文**只出产品文档，不写代码**。所有表结构以「字段 / 类型 / 约束 / 说明」表格形式给出，供 architect 转 DDL，不提供可执行 SQL。
> 凡涉及"框架能不能做"的判断，第 0 章都给了文件路径与行号证据；凡涉及数字（行数、耗时、人数、有效期），正文都给了假设与算例，可复算。

---

## 目录

| 章 | 内容 | 关键数字 |
|---|---|---|
| 第 0 章 | 前置事实核查（31 条代码事实）+ 11 个已定产品口径 | F1–F31 / D1–D11 |
| 一 | 角色与真实场景 | 4 角色 / 9 个数字化故事 / 4 个反场景 |
| 二 | 功能清单（P0 / P1） | 80 项（P0 70 / P1 10） |
| 三 | 对象存储详细设计 | 3 张表 + 3 条改造 + 6 类连通性失败 |
| 四 | 导入导出详细设计 | 30 个错误码 + 2 个状态机 + 2 个耗时算例 |
| 五 | 消息通知详细设计 | 5 张表 + 4 种发送目标 + 3 个通道 |
| 六 | 权限模型 | 41 个权限码 / 3 层数据权限 / 4 个预置角色包 |
| 七 | 菜单结构 | 1 个一级目录 / 4 个可见菜单 / 1 个隐藏菜单 |
| 八 | 页面结构与交互 | 9 个页面（逐页给入口/列表/表单/反馈/空态/异常态） |
| 九 | 验收场景 | 6 个场景（逐步骤判定） |
| 十 | 后续规划 | 一段话 |
| 附录 A | 错误与提示文案总表 | 67 条（E-STG 15 / E-IMP 20 / E-EXP 10 / E-NTC 15 / E-CMN 7） |
| 附录 B | 需要老大拍的 8 个决策 | — |
| 附录 C | 非功能需求与交付前提 | 7 项 |
| 附录 D | 本文明确不做的事 | 12 条 |
| 附录 E | 本批对外提供的内部调用契约 | 3 个契约 |

---

# 第 0 章 前置事实核查（本 PRD 的落地依据）

> 为避免"凭空设计"，撰写前对**同技术栈底座工程** `/root/.hermes/workspace/information-security/cailu-src`（Rust + Alun + Vue3 + TDesign，与云枢同栈）做了源码核查，并核对了 `alun-fs 0.4.2` 存储抽象与 `tdesign-icons-vue-next@0.4.11` 图标清单。
> 本批三个模块**全部是"在已有底座上加东西"**，不是从零建，所以"现有代码到底怎么写的"直接决定了接口契约、表结构和能不能做。以下 31 条，开发按此实现，不需要再猜。

## 0.1 平台与权限事实（F1–F12）

| # | 核查结论 | 证据 | 对本文的影响 |
|---|---|---|---|
| **F1** | 权限码存 `sys_menu.auth_code VARCHAR(100)`，格式 `{模块}:{资源}:{动作}`；`menu_type` 1 目录 / 2 菜单 / 3 按钮；`module_code VARCHAR(20)`，现有取值 `system`(22 条) / `support`(24 条) / `app`(1 条) | `backend/migrations/system/V0004__create_sys_menu.up.sql`、`seed/S__menu_data.sql` | 本批权限码统一 `business:storage:*` / `business:impexp:*` / `business:notice:*`，`module_code='business'`（20 字符够，已由 ERP 批次先例验证） |
| **F2** | 现有顶级目录：系统管理 `/system` sort 10、支撑功能 `/support` sort 20。**已被前序批次占用**：ERP sort 40、BI `/bi` sort 50、MDM `/mdm` sort 60；**30 已预留给协同办公** | `S__menu_data.sql:24,80`；ERP PRD F2、大数据 PRD 6.1 | 本批一级目录取 **sort_order = 70**（下一个空位），不抢 30 |
| **F3** | 后端接口鉴权用声明式宏 `#[alun::permission(path=…, method=…, permission=…)]` + `#[alun::get("/system/user/page")]`，路由风格 `/{模块}/{资源}/{动作}`，分页返回 `ResResult<PageData<Vec<T>>>` | `backend/src/system/user/handler.rs:43-49` | 新接口走 `/business/storage/...`、`/business/impexp/...`、`/business/notice/...`，第六章逐个列权限码 |
| **F4** | 前端页面级鉴权用路由 `meta.permission`；按钮级用 `v-privilege="['system:user:create']"` | `frontend/src/views/system/user/index.vue:63`、`directives/permission/privilege.ts` | 删除、群发、导出明文等敏感按钮一律用 `v-privilege`，不新造机制 |
| **F5** | `hasPermission()` 是**全等匹配**，只额外支持超管通配 `*:*:*`；**不支持 `business:storage:*` 这种前缀通配**；`code` 为空视为放行 | `frontend/src/stores/user.ts:63-71` | ①"整包授权"只能逐条勾选；②全员可见页面 `auth_code` 留空串；③**不要在前端写 `v-privilege="['business:storage:*']"`，它永远返回 false** |
| **F6** | 导航树由 `filterNavMenus` 递归过滤：剔除 `menu_type=3`（按钮）与 `is_visible=false`；目录下无可见子菜单则**整目录隐藏** | `frontend/src/stores/user.ts:17-38` | 隐藏菜单（消息中心）必须 `is_visible=FALSE`，但它仍参与路由与权限；目录下必须有至少一个可见子菜单，否则整个「通用底座」不显示 |
| **F7** | 侧边栏 `SideMenuItem` 递归渲染，有子节点即渲染 `t-submenu`，层级不限 | `frontend/src/layouts/components/SideMenuItem.vue` | 本批只需「目录 → 菜单」两级，不依赖三级能力 |
| **F8** | 列表页既有范式：`t-card` + `t-row/t-col` 横排筛选（`@enter` 触发查询）+ 工具栏 + `t-table` + `t-dialog` 内嵌 `t-form` | `frontend/src/views/system/user/index.vue:34-146`、`views/support/file/index.vue:1-120` | 第九章所有列表页沿用此布局，与既有页面行高、间距、按钮位置一致 |
| **F9** | `sys_role` **没有 data_scope（数据范围）字段**，只有 role_code / role_name / status / sort_order | `migrations/system/V0002__create_sys_role.up.sql` | 数据权限不能靠角色表配置，必须在服务层用「权限码 + 业务表 create_by」硬实现，见 6.2 |
| **F10** | `sys_dept` 有 `parent_id` / `dept_path` / `leader_id`；`sys_user.dept_id` 指向 `sys_dept`；后端**已实现**部门子树递归 `find_dept_subtree_ids(root_id)`（`WITH RECURSIVE`，返回自身 + 全部下级） | `V0006__create_sys_dept.up.sql`、`backend/src/system/dept/model.rs:261-279` | **"部门（含下级）"发送目标直接复用这个函数，不要重写递归**；它一次返回 UUID 列表，5.4 的扇出与去重就建在它之上 |
| **F11** | 底盘 `backend/src/` 下**无 `business` 目录**（现有：bootstrap、middleware、shared、support、system、task、utils） | `backend/src/` 目录清单 | 三个模块都是 business 域，从 0 建目录，结构见附录 C.3 |
| **F12** | 主题机制：`src/style/theme.less` 覆盖 `--td-brand-color-1..10` 注入品牌色，且**必须镜像 TDesign 的 `[theme-mode]` 选择器**（只写 `:root` 会被 TDesign 的 `[theme-mode]` 块压回默认蓝） | `frontend/src/style/theme.less:1-60` | 本批所有颜色**只用 CSS 变量**（`--td-brand-color-*`、`--td-success-color`、`--td-warning-color`、`--td-error-color`、`--td-bg-color-*`、`--td-text-color-*`），**禁止写死十六进制**，这样 22 套配色切换时业务页面零改动 |

## 0.2 现有文件模块事实（F13–F18）—— 本批改造的地基

| # | 核查结论 | 证据 | 对本文的影响 |
|---|---|---|---|
| **F13** | `sys_file` 表**已存在**，字段为：`id / file_name / original_name / file_key / file_size / file_hash / mime_type / extension / storage_path / category / status / uploaded_by / created_at / updated_at / create_by / update_by / deleted_at`。`file_key` 有部分唯一索引 `WHERE deleted_at IS NULL`；另有 `category`、`uploaded_by` 两个索引。**没有 `storage_id`、没有 `is_private`、没有 `bucket`** | `migrations/support/V0010__create_sys_file.up.sql`、`support/file/model.rs:26-58` | 本批必须 `ALTER TABLE sys_file` 加 3 列（`storage_id` / `is_private` / `upload_status`），见 3.1.3。**`storage_path` 列已存在但当前存的是绝对路径**，要改成存「相对后端根的 key」 |
| **F14** | 上传类型白名单是**硬编码常量**，共 31 个扩展名（图片 8 / 文档 11 / 压缩 5 / 音视频 7），**且只校验扩展名，不校验 MIME**；无扩展名直接拒绝 | `support/file/handler.rs:36-45`（`ALLOWED_UPLOAD_EXTENSIONS`）、`ensure_extension_allowed()` | ①"按分类配置白名单"必须把常量改成**可配置**（3.4）；②**现有实现存在绕过风险**：把 `evil.exe` 改名为 `evil.pdf` 即可上传成功，本批要补 MIME 魔数校验（3.4.3，安全项） |
| **F15** | 上传大小上限是**全局单值** `[upload].max_size_mb = 20`，读取自 `cfg().upload.max_size_mb`，**不区分分类** | `backend/config/config.toml:137-139`、`handler.rs:store_file_bytes()` | "按业务分类配置大小上限"是新能力，需新建策略表（3.1.2）；`20MB` 这个值继续作为「默认策略」的兜底 |
| **F16** | 下载接口 `/api/support/file/download/{id}` **恒定返回 `Content-Disposition: attachment`**；且用 `std::fs::read` **把整个文件读进内存**再返回 | `handler.rs:download_response()`、`build_content_disposition()` | ①**图片/PDF 预览必须新增 `inline` 模式**（现有接口只给下载），见 3.8.3；②大文件下载会占后端内存，>100MB 的文件必须走预签名直连（3.6） |
| **F17** | 秒传 = 算内容 SHA-256，命中 `file_hash`（未软删 + status=1）就**直接复用已存记录，不落盘、不插新行** | `handler.rs:sha256_hex()`、`model.rs:find_reusable_file()` | 保留该能力；但秒传命中必须**同时校验分类策略**（否则用 A 分类的宽松策略绕过 B 分类的严格策略），见 3.4.4 |
| **F18** | 审计日志已有 3 个动作：`FILE_UPLOAD` / `FILE_UPLOAD_BATCH` / `FILE_DELETE`，写 `audit_logs`（前/后快照 JSONB），日志写失败不影响业务 | `seed/S__dict_data.sql:671,682,693`、`handler.rs:log::record_audit()` | 本批新增的动作码（存储配置增删改、AK 更新、连通性测试、导入入库、导出、群发、模板改）**沿用同一套 `audit_logs`**，动作码清单见 6.4 |

## 0.3 框架存储抽象事实（F19–F21）—— 决定"能不能切 MinIO"

| # | 核查结论 | 证据 | 对本文的影响 |
|---|---|---|---|
| **F19** | `alun-fs` 定义统一 trait `StorageBackend`，方法为：`backend_type()` / `write()` / `write_at(relative_path)` / `read()` / `delete()` / `exists()` / `presign_download_url(expiry_secs)` / `health_check()`。**只有"预签名下载"，没有"预签名上传"**；`write()` / `read()` 的入参出参都是 `&[u8]` / `Vec<u8>`（**全量字节，非流式**） | `~/.cargo/registry/.../alun-fs-0.4.2/src/backend.rs` | ①**"连通性测试"直接用 `health_check()`，不用自己 send 探测请求**；②**"私有文件限时下载"用 `presign_download_url()`**，但本地后端默认返回空串（见 F20）；③**"大文件直传"框架没给，必须补一个 `presign_upload` 方法**——这是本批唯一的框架改动点，见 D3 与 3.5 |
| **F20** | `MinioBackend` 基于 `aws-sdk-s3`，**官方注释写明兼容 MinIO / AWS S3 / 腾讯 COS / 阿里 OSS**；`presign_download_url` 用 `PresigningConfig` 生成 GET 预签名 URL；`force_path_style` 的取值条件是 **`endpoint.contains("minio") \|\| !use_tls`**；本地后端 `presign_download_url` 返回空串 | `alun-fs-0.4.2/src/minio.rs:35-50`（`from_config`）、`:229+`（`presign_download_url`）、`src/backend.rs` 默认实现 | ①**阿里 OSS 与 S3 兼容存储不需要新后端，直接填 MinIO 类型即可**（D2 的依据）；②**坑：如果 MinIO 端点写成 `https://10.0.0.5:9000`（不含 "minio" 且开了 TLS），会走 virtual-hosted 风格而连不上**，页面必须给这条提示（附录 A 文案 E-STG-08）；③本地后端没有预签名 → 私有文件在本地后端上必须走"应用层签名短链"（3.6.2） |
| **F21** | `BackendRegistry` 以 **`backend_type` 字符串为键**（`register()` 同类型直接覆盖），提供 `with_default()` / `get()` / `health_check_all()`；框架侧只有两个实现：`LocalFs`（type `"local"`）与 `MinioBackend`（type `"minio"`） | `alun-fs-0.4.2/src/registry.rs:37-52`、`local.rs`、`minio.rs` | ①"4 种存储类型"里 **MinIO / S3 兼容 / 阿里 OSS 三者本质是同一个 `minio` 实现**，不能靠 registry 区分；②**业务侧要自己维护"配置行 → 后端实例"的路由表**（按配置 id 索引），不能把多行配置塞进 registry，见 3.2 |

## 0.4 加密、导入导出、实时推送事实（F22–F26）

| # | 核查结论 | 证据 | 对本文的影响 |
|---|---|---|---|
| **F22** | 平台已有 **AES-256-GCM** 加密能力：`alun_utils::Crypto::aes_encrypt/aes_decrypt`；`sys_config.is_encrypted=true` 时写入前加密、落库格式 `enc:v1:{nonce}:{cipher}`；密钥取自**环境变量 `X_CONFIG_SECRET_KEY`**，未配置时报 `CONFIG_SECRET_MISSING`（40014）并**拒绝明文落库** | `support/config/model.rs:810-880`、`shared/constants.rs:198,201`、`error_code.rs:165` | AK/SK 的"不落明文"**直接复用这套加密与密钥**，不要再引第二套加密库；`X_CONFIG_SECRET_KEY` 未配置时**禁止保存 AK/SK**（报 E-STG-11） |
| **F23** | ⚠️ **`sys_config` 的读接口会把加密值解密后明文返回前端**：`ConfigRes::from_model()` 对 `is_encrypted` 的行调用 `decrypt_config_value()` 并放进 `config_value` 字段，分页接口 `page()` 直接用它 | `support/config/dto.rs:62-66`、`support/config/handler.rs:126-130` | **AK/SK 绝不能存进 `sys_config` 走现有读接口**，否则运维打开"参数管理"就能看到明文 AK/SK。必须建独立表 + 独立 DTO，响应里 SK 恒为空串（3.7.3）。这是本批最重要的一条安全约束 |
| **F24** | 框架内置真 XLSX 读写：开 `xlsx` feature 后才有 `Export::to_xlsx_*`（rust_xlsxwriter 写）/ `Import::from_xlsx_*`（calamine 读，`Import` 本身一直存在）；**不开 feature 时这些 API 根本不存在**（不是静默退化成 CSV）；`XlsxSheet` 支持多 sheet | `alun-0.4.2/src/lib.rs:93,155,1199,1265`、`Cargo.toml:160` | **backend 当前未开 `xlsx` feature**（现有 features 只有 db/postgres/cache/plugin/fs-local/chrono/uuid/json/ipnetwork/xss），导入导出必须在 `Cargo.toml` 补 `"xlsx"`，列进交付前提（附录 C.1） |
| **F25** | 框架有 **SSE** 能力：`Broadcaster` + `sse_json`（单向推送，仅依赖 futures-util），需 `sse` feature；**backend 当前未开** | `alun-0.4.2/src/lib.rs:90,1188-1193`、`Cargo.toml:135` | 未读角标"实时 vs 轮询"的决策（D7）以此为依据：SSE 技术上可行但要开 feature + 运维长连接，P0 先用轮询 |
| **F26** | 分页常量：`DEFAULT_PAGE_SIZE = 10`、**`MAX_PAGE_SIZE = 1000`**；`utils::normalize_page()` 对 `page_size` 做 `clamp(1, 1000)` | `alun-web-0.4.2/src/api.rs:104,107`、`backend/src/utils/mod.rs:110-116` | **导出不能靠"前端翻页把数据捞全"**：20 万行 ÷ 1000 = 200 次请求，必超时。导出一律在后端一次性查询落文件（4.6） |

## 0.5 前端与部署事实（F27–F31）

| # | 核查结论 | 证据 | 对本文的影响 |
|---|---|---|---|
| **F27** | 前端 axios **全局 `timeout = 15000`（15 秒）**，`request.ts` 里两处创建实例都是 15s | `frontend/src/api/request.ts:68,138` | **15 秒是硬墙**：①同步导出必须单独把 timeout 调到 180s（4.6.1）；②导入校验 2 万行以内走同步（估 2 秒，4.5.1），**2 万行以上必须走异步**（10 万行估 14 秒，已贴住 15 秒上限） |
| **F28** | 生产 nginx 配置 `client_max_body_size 50m` | `docker/frontend/nginx.conf:16` | ①**500MB 文件走"后端中转"必然被 nginx 挡在 50MB**——这正是"大文件直传"的产品价值（浏览器 → 对象存储，不经 nginx/后端）；②本地后端不支持直传，降级方案须同步把该值调大，列进交付前提（附录 C.1） |
| **F29** | 定时任务是框架 `SchedulerPlugin`（**cron 表达式驱动，非延时队列**）；**运行期增删任务必须 `stop()` → 增删注册 → `start()`**（启动时做一次性快照，之后 register 不生效）；已有 5 个日志清理任务跑在 03:00 / 03:10 / 03:20 / 03:30 / 03:40 | `backend/src/task/mod.rs:1-30` | ①"定时发送通知"和"异步导入导出"都能用"每分钟一次的 cron 任务"驱动，**不需要引 Kafka**（D11）；②消息清理任务取 **03:50**，与现有 5 个错峰，避免同时批量删抬高 DB 压力（5.8） |
| **F30** | 图标库 `tdesign-icons-vue-next@0.4.11` 中：**`bell` / `notice` / `announcement` 不存在**；确认真实存在：`notification` `notification-filled` `notification-add` `notification-circle` `mail` `mail-filled` `cloud` `cloud-upload` `cloud-download` `upload` `download` `import` `export` `file-import` `file-export` `file` `file-pdf` `file-image` `file-excel` `file-zip` `file-attachment` `file-locked` `file-safety` `folder` `hard-disk-storage` `object-storage` `system-storage` `server` `check-circle` `error-circle` `info-circle` `tips` `pending` `task` `task-checked` `task-error` `task-time` `setting` `filter` `search` `delete` `edit` `send` `usergroup` `user` `user-business` `refresh` `history` `time` `copy` `link` `lock-on` `system-messages` `chat-message` `view-list` `root-list` `chart-bar` `system-log` | 包文件 `esm/components/` 清单实测 | **顶栏铃铛图标用 `notification`，不是 `bell`**；菜单图标：存储管理 `object-storage`、文件管理 `folder`、导入导出 `file-export`、通知管理 `notification`。用错名字会渲染成空白方块 |
| **F31** | 平台已有字典体系 `sys_dict_type` / `sys_dict_item`（如 `user_status` / `menu_type` / `job_status`）与参数表 `sys_config`（含 `select_value` 供前端下拉）；上传分类当前是**前端硬编码映射表**（`CATEGORY_LABELS`，attachment→附件 等） | `migrations/support/V0007/V0008/V0009`、`seed/S__dict_data.sql`、`views/support/file/index.vue:118+` | ①上传分类要落成字典 `storage_category`，前后端同源（现在前端硬编码、后端自由字符串，是隐患）；②所有阈值（大小上限、链接有效期、保留天数）都放 `sys_config`，**不改代码可调** |

## 0.6 十一个必须先定的产品口径（本文已定，开发照做）

| # | 问题 | 本文的定法 | 为什么这么定 |
|---|---|---|---|
| **D1** | 三个模块的菜单位置 | **新增 1 个一级目录「通用底座」`/platform`（sort 70）**，下挂 4 个可见菜单 + 1 个隐藏菜单；**旧 `/support/file` 菜单停用（`status=0`），但其 5 个接口全部保留不动** | ①这三个模块是"所有业务模块共用的底座"，混在「支撑功能」（字典/参数/日志/任务这类 IT 运维）里会让业务人员以为与自己无关；②B/C/D 批次要大量复用，独立目录便于整包授权；③停用菜单而不是改 URL，是为了不打断 ERP/BI 已有的附件调用（F13/F17 的接口契约零改动） |
| **D2** | "本地 / MinIO / S3 兼容 / 阿里 OSS"是不是 4 种实现 | **P0 只有 2 种 `backend_type`：`local` 与 `minio`**。S3 兼容、阿里 OSS 在页面上是**MinIO 类型的"连接预设"**（选中后自动填 endpoint 模板与注意项），不是独立实现 | F20 已证 `MinioBackend` 基于 `aws-sdk-s3`，官方注释就写明兼容 S3/OSS/COS；为此再写两个后端类是纯重复劳动。**代价**：同一时刻只能启用 1 个非本地后端（F21 registry 按类型建键）——但这本来就够用，因为"默认存储"只有一个。要做"同时挂 MinIO + OSS 双活"= P1（需自研按配置 id 路由的 registry） |
| **D3** | 大文件直传框架不支持怎么办 | **P0 做，但明确列出框架改动量**：需在 `StorageBackend` 补 1 个 `presign_upload(stored_path, expiry, content_type, max_size)` 方法（`aws-sdk-s3` 的 `put_object().presigned()` 原生支持）+ 新增 1 个"直传确认"接口。**本地后端不支持直传**，降级为"后端流式中转"（需同步调大 nginx `client_max_body_size`） | 500MB 走 nginx 必被 50m 挡死（F28），走后端中转则 500MB × 并发数全落在后端内存/磁盘（F19 全量字节）。直传是唯一能同时满足"后端不占内存"和"500MB"的做法。改动量小（1 方法 + 1 接口），值得放进 P0 |
| **D4** | 同步导出的行数阈值 | **默认 10000 行**（可按业务在策略里调低到 1000）。前端该请求 `timeout` 单独设 **180s**；**后端 60s 未完成则自动降级为异步任务并返回 `task_id`**，前端弹「数据量较大，已转为后台导出」 | F27 全局 15s、F26 单页 1000 行。若硬等 15s，1 万行导出大概率失败；若全部走异步，导出 500 行也要等任务，体验更差。60s 自动降级是"既快又不失败"的唯一解 |
| **D5** | 重复数据（编码已存在）默认怎么处理 | **默认「跳过」**，校验报告里单列一行「其中 N 条因编码已存在被跳过」；「覆盖」需二次确认（弹框写明将覆盖 N 条历史数据）并写审计；**「报错」不作为默认** | "报错"会让 1 条重复毁掉整批 5000 条；"覆盖"会静默改历史数据且不好追溯。跳过 + 显式告知，用户能自己判断要不要改。覆盖给主动选择权，但要留痕 |
| **D6** | 校验有失败行时能不能部分导入 | **默认关闭（全量事务，要么全成功要么全不入库）**，在业务的导入配置里可开「允许部分导入」 | 半成品数据比没数据更难收拾（例：5000 条里进了 4995 条，用户不知道该补哪 5 条，还会重复导）。默认全量事务最安全；但 10 万行场景全量失败的代价大，所以给按业务开启的开关 |
| **D7** | 未读角标：实时（SSE）还是轮询 | **P0 用轮询 60 秒** + 三个增强：①浏览器标签重新获得焦点时立即刷一次；②用户在本端读消息时本地先减再对齐；③发完通知后主动刷一次。**P1 再上 SSE** | 算一下量：120 人同时在线 × 每分钟 1 次 = **2 QPS**，对 DB 完全可忽略（P0 用一条 `COUNT(*)` 命中 `(receiver_id, read_at)` 索引）。而 SSE 要开 feature、要处理断线重连、要运维长连接与反代超时，P0 不值得。等通知量真上来了（如 D 批次审批流每秒都在推）再换 SSE（F25 已证可行） |
| **D8** | 消息与导出产物保留多久 | **未读消息永久保留**（不清）；**已读消息保留 180 天**后自动清理；**消息正文中引用的导出产物 7 天**（链接失效但记录留）；**导入错误明细文件 30 天**；全部走 `sys_config` 可调，清理任务每日 **03:50** | 未读是"待办"，删了就是丢失工作项，不能清；已读 180 天足够回溯半年内的通知。导出产物是几 MB~几百 MB 的临时文件，7 天够下载，久了纯占存储。03:50 与现有 5 个清理任务（03:00–03:40）错峰（F29） |
| **D9** | 文件 / 导入任务 / 导出任务要不要按创建人隔离 | **文件不隔离**（默认全公司可见）；**导入任务、导出任务按创建人隔离**（默认只看自己的，有 `impexp:task:view-all` 的看全部）；**私有文件**（`is_private`）下载需 `storage:file:download-private` 且只能拿限时链接 | 文件是共享资产（合同、图纸、物料图片就是要给别人看的），按人隔离会导致"我上传的附件别人打不开"，业务直接卡死。但导入/导出任务里有别人的原始数据与筛选条件，属于工作过程数据，默认隔离更符合直觉。私有文件靠"标记 + 独立权限"控制，不靠"谁上传的" |
| **D10** | 导出时敏感字段（手机号/身份证/银行卡）怎么处理 | **默认脱敏**（复用 `Mask::mobile/email/id_card/bank_card`，F22 同源工具）；要导明文必须持有独立权限码 `business:impexp:export:sensitive`，且**每次明文导出写一条审计** | 导出文件一旦离开系统就再也管不住，脱敏是最后一道闸。但财务对账、发工资这类场景确实需要明文，所以给独立权限 + 留痕，而不是一刀切禁止 |
| **D11** | 异步导入导出用什么跑 | **数据库任务表 + 每分钟一次 cron 调度器**（复用 F29 的 `SchedulerPlugin`），**不引 Kafka、不引消息队列** | ①现有 `sys_job` 体系就是 cron，团队熟；②`alun-task`（Kafka）要 CMake + C 工具链编译，重且本批用不上；③导出任务量级是"每天几十个"，DB 轮询完全够。任务表设计见 4.7.1，状态机见 4.7.2 |
---

# 一、用户角色与真实场景

## 1.1 角色总览

| 角色 | 典型岗位 | 系统身份（复用现有 RBAC，**不另建用户体系**） | 核心诉求 | 使用频率 |
|---|---|---|---|---|
| **系统管理员** | IT 运维 / 平台管理员 | 现有 `sys_role` 里已有超管或「系统管理员」角色；本批追加存储与容量相关权限码 | 配置存储后端、定上传规矩、管文件、盯导入导出任务 | 每周 1~3 次（配置类）|
| **业务操作员** | 采购员 / 仓管 / 财务专员 / HR | 现有「普通用户」角色 + 本批 `impexp:*` 权限包 | 批量导入台账、导出报表给自己或给领导 | 每天（导入导出是日常动作）|
| **普通用户** | 全员（含上面两类人） | 无需额外授权，登录即有 | 收到通知别漏审、别错过公告、一眼看到几条没读 | 每天多次（被动接收）|
| **通知管理员** | 行政 / HRBP / 财务主管 / 部门助理 | 现有角色 + `notice:template:*` + `notice:send:group` | 维护通知模板、给某个部门（含下级）群发 | 每周 1~2 次 |

> **一句话区分**：「系统管理员」管**规矩和地基**（存储怎么配、谁能传多大）；「通知管理员」管**内容**（发什么、发给谁）。两者权限不重叠（见 6.3）。

---

## 1.2 数字化故事（9 个，全部带数字）

### 故事 1（系统管理员 · 存储切换）
老张是 IT 运维。公司文件服务器只剩 80GB，明年要换。周一上午他在 `通用底座 → 存储管理` 点「新增配置」，类型选 `MinIO`，填 `minio.corp.local:9000`、桶名 `cloudpivot`、AK/SK（SK 输入框是密码型，保存后列表只显示 `AKIA****7ZP2`）。保存前他先点了**「连通性测试」**，2 秒后绿条提示「连接成功，桶 cloudpivot 已存在（Region: us-east-1，延迟 42ms）」。他再加一条配置，把它设为默认——弹框问「切换默认存储只影响**新上传**的文件，已有 12,486 个文件仍从原存储读取，是否继续？」他点确定。

**切换后**：他上传一张测试图，去 MinIO 控制台看到对象确实落在了 `2026/10/28/{uuid}.png`；再从文件管理点开 2025 年的老合同，**照常下载成功**（系统按每个文件的 `storage_id` 找原来的本地存储）。

### 故事 2（系统管理员 · 上传策略）
下午安全部门来找他：有人把 `.exe` 改名为 `.pdf` 传上来了。他在「上传策略」里把 `attachment` 分类的**类型白名单**从 31 个扩到 40 个，同时打开**「校验文件真实类型（MIME 魔数）」**开关。第二天那人再传，页面直接报「文件内容与扩展名不符（扩展名 .pdf，实际类型 application/x-msdownload），已拒绝」。

### 故事 3（业务操作员 · 导入 5000 条物料）
小李是采购员。ERP 上线要录 5000 条物料，手工要两周。她在 `通用底座 → 导入导出 → 新建导入` 选业务「物料」→ 先点**「下载模板」**，拿到一个 Excel：第 1 行表头、第 2 行灰色示例（`MAT-000001 / 螺丝 M6×20 / 个 / 13%`）、第 3 行起留空，另有一个「字段说明」sheet 写着"物料编码：必填，字母+数字+连字符，≤32 位"。

她把 5000 行贴进去上传。**14 秒后**出校验报告：`总计 5000 行 / 可导入 4995 行 / 失败 5 行`。失败明细表格里 5 行红字：

| 行号 | 列 | 原值 | 原因 |
|---|---|---|---|
| 137 | 物料名称 | （空） | 必填项为空 |
| 892 | 计量单位 | （空） | 必填项为空 |
| 2011 | 规格型号 | （空） | 必填项为空 |
| 3045 | 物料编码 | MAT-000123 | 编码已存在（与第 88 行重复） |
| 4470 | 物料编码 | MAT-000123 | 编码已存在（与第 88 行重复） |

她点**「下载错误明细」**，拿到一个只有 5 行（外加行号列）的 Excel，改完再上传——这次直接按"跳过重复"导入成功 5 条，报告写「成功 5 / 失败 0」。全程 **2 次上传、3 分钟**，比她原来估的两周少两个数量级。

### 故事 4（业务操作员 · 异步导出 20 万行）
月末对账，她要导出全部订单。在订单列表点「导出」，勾了 8 个字段（订单号、日期、供应商、物料、数量、单价、金额、状态），系统提示「预计 203,481 行，数据量较大，将转后台导出」。她点确定，任务列表出现一行：`导出订单_20261028.xlsx · 状态：导出中 · 进度 0%`。

她去忙别的。**4 分 12 秒后**顶栏铃铛跳出「1」并弹出站内信「您的导出任务已完成，点击下载」。回到任务列表，进度 100%，点「下载」直接开始下 18.7MB 的 xlsx。链接右上角标注「有效期至 2026-10-29 18:40（剩 23h58m）」。

**反场景**：如果她第二天下午才想起来去下载，链接已过期，页面显示「下载链接已过期（有效期 24 小时），请重新导出」并给一个「重新导出」按钮——而不是给她一个 404 白屏。

### 故事 5（普通用户 · 3 条未读）
小陈是仓管。早上 8:58 打开系统，顶栏铃铛上有个红点写着 **3**。点开是三个卡片，前两个是**业务通知**（「您有一张入库单 GR-202610-0031 待审核」，带「去处理」按钮），第三个是**系统通知**（「关于 11 月 1 日系统升级的通知」）。

他点开第一条，正文自动变已读，铃铛从 3 变 2；点「全部已读」，红点消失。他点了第二张卡的「去处理」跳到入库单页面（D 批次审批功能上线后，这类通知就由审批流自动产生；本批他看到的这类通知来自通知管理员的**手动发送**或接口调用）。

### 故事 6（通知管理员 · 给财务部及下级发通知）
王主管是财务部助理。月末结账，她要通知财务部全部人。在 `通知管理 → 发送通知`：

1. 发送对象选「部门」→ 树里勾 `财务部`，右侧出现开关「**包含下级部门**」，她打开；
2. 下方实时显示「将发送给 **120** 人（财务部直属 34 人 + 下级 3 个部门 86 人，已去重 2 人）」；
3. 选模板「月度结账提醒」，变量 `{{userName}}`、`{{deadline}}` 自动填成"每个人各自的姓名"和"2026-10-31 18:00"；
4. 发送时间选「立即」→ 确认弹框「即将向 120 人发送通知，不可撤回，是否继续？」；
5. 点确定，2 秒后提示「已发送 120 人，成功 120，失败 0」。

她抽查两个人：A 的铃铛是 1 且在 8 秒内出现；B 登录后也是 1。**A 把消息读了，B 的未读数还是 1**——各自独立。

### 故事 7（通知管理员 · 定时发送）
周五下午她提前配置好「周一 09:00 发考勤新规」，选「定时发送 → 2026-11-02 09:00」。列表里状态显示「待发送（还有 2 天 15 小时）」。周一 09:00:07，状态变「已发送 · 120 人」，她收到一条自己的通知副本（如果她也在发送范围内）。

### 故事 8（业务操作员 · 导出敏感字段被脱敏）
小陈要导 3000 条供应商联系人给领导。导出时系统提示「本次导出包含 2 个敏感字段（手机号、银行卡号），已按默认脱敏规则输出」。文件里是 `138****5678`。他去找财务要明文——财务有 `impexp:export:sensitive` 权限，导出的就是明文，同时审计日志里多了一条「张三 于 14:32 导出供应商联系方式（含明文敏感字段，3000 行）」。

### 故事 9（系统管理员 · 排查失败导入）
周二早上有 4 个人反馈"导入失败"。他在 `导入导出 → 导入记录` 筛「状态=失败」，看到 4 条，其中 3 条原因都是「文件第 1 行表头缺少必需列：物料编码」。他明白是三人都用了旧模板——于是把「模板版本」从 v1 升到 v2 并在模板里加了一行醒目提示，同时把这 4 条记录的错误明细打包下载发给对方。

---

## 1.3 反场景（明确不做、明确不管的事，防止后面扯皮）

| # | 场景 | 本文的处理 | 为什么不做 |
|---|---|---|---|
| **A1** | 用户希望"两个 MinIO 集群同时在线，A 部门传 A 集群、B 部门传 B 集群" | **不做**（P1）。P0 只有"一个默认存储"，切换后新文件全落新存储 | 框架 `BackendRegistry` 按 `backend_type` 建键（F21），同时挂两个 minio 需要自研按配置 id 路由的注册中心；而"部门分存储"是容量与合规诉求，属于 P1 的"存储多实例 + 路由规则" |
| **A2** | 用户希望导入时"边传边看进度条" | **不做**。上传阶段只有浏览器自己的上传进度（含直传）；上传完成后才进入"校验中" | 解析必须在文件完整到达后做（`calamine` 不支持流式增量解析），做不出"边解析边显示第 N 行" |
| **A3** | 用户希望导出的 Excel 带公式、带合并单元格、带图表 | **不做**。导出的是"数据表 + 表头样式 + 冻结首行" | 这是"报表设计器"的范畴（BI 批次已有图表能力）；用 `rust_xlsxwriter` 做样式已够看，做公式/合并会引入双份口径风险 |
| **A4** | 用户希望通知能"撤回"（发错了收回） | **P1**，且只支持"发送前取消（定时任务）" | 站内信一旦投递就是"已读就不该被改"。真正的撤回需要"消息版本"概念，本批不做 |

---

# 二、功能清单（P0 / P1）

> **P0** = 本批必交付，缺一条主流程走不通；**P1** = 本批可不做，下批补。
> 「编号」前缀：`S` = Storage（对象存储）、`I` = Import/Export、`N` = Notice（消息通知）。

## 2.1 对象存储（S）

### S1 存储后端配置

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| S1-01 | 存储配置列表 | **P0** | 列：配置名称 / 类型 / 端点 / 桶 / 是否默认 / 状态 / 连通性 / 更新时间 / 操作。见 8.2 |
| S1-02 | 新增存储配置 | **P0** | 类型下拉 4 项（本地 / MinIO / S3 兼容 / 阿里 OSS），后三者共用 MinIO 实现（D2） |
| S1-03 | 编辑存储配置 | **P0** | 默认配置的端点/桶**不允许改**（改就要迁移数据），只能改名称、备注、状态 |
| S1-04 | 删除存储配置 | **P0** | 只能删「非默认且没有文件引用」的配置；被引用时拒绝并提示引用数 |
| S1-05 | 设为默认存储 | **P0** | 二次确认，明确"只影响新上传"；切换写审计 |
| S1-06 | 连通性测试 | **P0** | 保存前可测、列表页可测；后端调 `health_check()`（F19），返回延迟与桶状态 |
| S1-07 | AK/SK 加密存储与掩码显示 | **P0** | AES-256-GCM 落库（F22），列表显示 `AKIA****7ZP2`，**SK 任何接口都不返回明文**（3.7） |
| S1-08 | AK/SK 轮换留痕 | **P0** | 改 AK/SK 时写 `audit_logs`（只记"已更新"，不记值），并要求二次输入确认 |
| S1-09 | 类型预设模板 | **P0** | 选「阿里 OSS」自动带出 endpoint 模板与 path-style 注意事项 |
| S1-10 | 同类型多实例并行启用 | P1 | 需自研按配置 id 路由的 registry，见 A1 |

### S2 上传策略

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| S2-01 | 分类策略列表 | **P0** | 每个 `category` 一行，显示大小上限 / 扩展名白名单 / 是否校验 MIME / 是否允许私有 / 状态 |
| S2-02 | 新增 / 编辑策略 | **P0** | 大小上限（1MB–2048MB）、扩展名白名单（多选+自定义）、MIME 白名单 |
| S2-03 | 默认兜底策略 | **P0** | `default` 行兜底所有未单独配置的分类，默认值取现有 `max_size_mb=20`（F15）与 31 个扩展名（F14） |
| S2-04 | MIME 魔数校验 | **P0** | 开关式；打开后校验文件头真实类型与扩展名是否匹配（补现有仅校验扩展名的缺口，F14） |
| S2-05 | 分类字典化 | **P0** | 新增字典 `storage_category`，替换前端硬编码 `CATEGORY_LABELS`（F31） |
| S2-06 | 按角色细化策略 | P1 | 例：普通用户 20MB，管理员 200MB |

### S3 上传通道

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| S3-01 | 单文件上传（走存储路由） | **P0** | 接口契约不变（`POST /api/support/file/upload`），**内部改为经存储路由写入默认后端**（3.2.3） |
| S3-02 | 批量上传 | **P0** | 现有能力保留；任一文件超限时按"整体失败"处理并指出是哪个文件 |
| S3-03 | 秒传（含策略校验） | **P0** | 保留 SHA-256 秒传（F17），但命中后仍要过当前分类的策略校验（3.4.4） |
| S3-04 | 预签名直传 | **P0** | `POST /business/storage/upload/presign` 拿 PUT 预签名 URL，浏览器直传对象存储（3.5） |
| S3-05 | 直传确认与状态机 | **P0** | `POST /business/storage/upload/confirm` 落库；状态 `0 直传中 → 1 完成 / 2 失败` |
| S3-06 | 直传失败清理 | **P0** | 超 2 小时未确认的"直传中"记录由清理任务删除对象 + 软删记录 |
| S3-07 | 本地后端流式中转降级 | **P0** | 默认存储是 `local` 时，直传降级为后端分块写入（不整文件入内存） |

### S4 私有文件与下载

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| S4-01 | 私有文件标记 | **P0** | 上传/编辑时勾选；私有文件的列表对无 `download-private` 权限者**隐藏下载按钮** |
| S4-02 | MinIO 类后端：预签名限时链接 | **P0** | 用 `presign_download_url(expiry)`（F19/F20），真实对象存储直连下载 |
| S4-03 | 本地后端：应用层签名短链 | **P0** | 本地无预签名（F20）→ 自签 `sig + exp` 的短链，走后端校验后流式输出（3.6.2） |
| S4-04 | 有效期配置 | **P0** | 默认 60 分钟，范围 5 分钟–7 天；上限放 `sys_config` |
| S4-05 | 一次性链接 / 提取码 | P1 | 下载一次即失效 / 需输入 4 位码 |

### S5 文件管理页

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| S5-01 | 多维筛选 | **P0** | 分类（下拉）/ 上传人（选人）/ 时间范围 / 文件类型 / 关键词（原文件名模糊） |
| S5-02 | 预览（图片 / PDF） | **P0** | 新增 `inline` 下载模式（现有恒为 attachment，F16）；图片走 `t-image-viewer`，PDF 走内嵌 iframe |
| S5-03 | 批量删除 | **P0** | 多选 → 批量软删 + 尽力删对象；显示成功/失败条数 |
| S5-04 | 文件详情 | **P0** | 抽屉：原文件名 / 大小 / MIME / 扩展名 / 分类 / 存储后端 / SHA-256 / 上传人 / 上传时间 / 是否私有 |
| S5-05 | 打包下载 | P1 | 多选打包 zip（大文件耗时长，需异步） |
| S5-06 | 回收站 | P1 | 软删文件可恢复/彻底删除 |

**S 小计：34 项（P0 29 / P1 5）**

---

## 2.2 导入导出（I）

### I1 导入配置与模板

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| I1-01 | 导入模板下载 | **P0** | 按业务定义生成 xlsx：表头行 + 灰底示例行 + 「字段说明」sheet（4.1） |
| I1-02 | 模板列数据校验 | **P0** | 枚举列在模板里写 Excel 数据验证下拉；数值列限定范围 |
| I1-03 | 模板版本管理 | P1 | 模板改版后旧版本可下载，并标注「已停用」 |
| I1-04 | 导入定义配置 | **P0** | 「业务」不是写死的：字段、必填、类型、唯一键、目标表由配置定义（4.1.1） |

### I2 导入执行

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| I2-01 | 上传与解析预览 | **P0** | 上传后展示前 20 行（含表头映射结果），让用户确认列对得上 |
| I2-02 | 结构校验 | **P0** | 表头缺失/多余列、空文件、超过 10 万行、非 xlsx 格式 |
| I2-03 | 业务校验 | **P0** | 必填 / 类型 / 长度 / 枚举 / 数值范围 / 唯一键重复 / 外键存在性 |
| I2-04 | 校验报告 | **P0** | `总计 N / 可导入 X / 失败 M`，失败明细含行号 + 列名 + 原值 + 原因（4.3） |
| I2-05 | 错误明细下载 | **P0** | 下载 xlsx：`行号 + 全部原始列 + 失败原因`，改完可直接重传 |
| I2-06 | 确认入库（全量事务） | **P0** | 默认"有一条失败就整批不入"，默认策略见 D6 |
| I2-07 | 允许部分导入（开关） | **P0** | 按导入定义配置；开启后只导成功行，报告明确写出跳过条数 |
| I2-08 | 重复数据处理策略 | **P0** | 跳过（默认）/ 覆盖（二次确认）/ 报错，三选一，按导入定义配置（D5） |
| I2-09 | 大文件分片校验与进度 | **P0** | 10 万行+ 走异步任务，每 1000 行一个批次写进度（4.5） |

### I3 导出执行

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| I3-01 | 同步导出 | **P0** | ≤10000 行直接下载；60s 未完成自动降级为异步（D4） |
| I3-02 | 异步导出任务 | **P0** | >10000 行创建任务 → 后台跑 → 进度 → 完成通知 → 下载 |
| I3-03 | 导出字段勾选 | **P0** | 弹框列出全部可导列，默认全选必填列，记住用户上次选择（本地） |
| I3-04 | 导出脱敏 | **P0** | 敏感字段默认脱敏；明文需 `impexp:export:sensitive` + 审计（D10） |
| I3-05 | 下载链接有效期 | **P0** | 默认 24 小时，可配 1h–7d；过期给"重新导出"入口 |
| I3-06 | 导出任务列表 | **P0** | 列：任务名 / 业务 / 行数 / 状态 / 进度 / 创建人 / 耗时 / 创建时间 / 操作 |

### I4 任务管理

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| I4-01 | 导入记录列表 | **P0** | 列：文件名 / 业务 / 总数 / 成功 / 失败 / 重复跳过 / 状态 / 操作人 / 耗时 / 时间 |
| I4-02 | 进度轮询 | **P0** | 有"进行中"任务时每 3 秒轮询，全部终结后停止（不常驻轮询） |
| I4-03 | 任务取消 | **P0** | 仅"排队中/校验中/导出中"可取消；取消即软删产物 |
| I4-04 | 任务重试 | P1 | 导出失败可一键重跑（沿用同一筛选条件） |
| I4-05 | 任务与产物清理 | **P0** | 导出产物 7 天、错误明细 30 天、任务记录 180 天（D8） |
| I4-06 | 导出水印 | P1 | xlsx 每页页脚加"导出人+时间" |

> 注：I4-06 编号保留，P1 项计入总数。

**I 小计：25 项（P0 22 / P1 3）**

---

## 2.3 消息通知（N）

### N1 消息中心

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| N1-01 | 收件箱（未读/已读/全部） | **P0** | 三个 tab，默认"未读"；空态文案见 8.6 |
| N1-02 | 消息详情 | **P0** | 右侧详情面板（列表 + 详情双栏），标题/时间/类型标签/正文/跳转按钮 |
| N1-03 | 单条已读 | **P0** | 点开即已读；也可在列表上点"标为已读" |
| N1-04 | 全部已读 | **P0** | 二次确认，提示"N 条将标记为已读" |
| N1-05 | 删除 | **P0** | 单条删除 + 批量删除；删除为软删，不影响发送方统计 |
| N1-06 | 按类型筛选 | **P0** | 系统通知 / 业务通知 / 全部 |

### N2 未读角标

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| N2-01 | 顶栏铃铛 + 未读数 | **P0** | 图标 `notification`（F30：**没有 `bell`**）；数字 >99 显示 `99+` |
| N2-02 | 轮询与焦点刷新 | **P0** | 60s 轮询 + 标签重获焦点立即刷 + 本地已读即时减（D7） |
| N2-03 | SSE 实时推送 | P1 | 需开 `sse` feature（F25） |

### N3 发送

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| N3-01 | 发送通知表单 | **P0** | 4 种目标：指定用户 / 角色 / 部门（含下级）/ 全员；实时显示预计人数（5.4） |
| N3-02 | 立即发送 | **P0** | 确认弹框写明人数，不可撤回 |
| N3-03 | 定时发送 | **P0** | 选日期时间；支持"发送前取消" |
| N3-04 | 发送记录列表 | **P0** | 列：标题 / 目标描述 / 预计人数 / 实发/失败 / 状态 / 发送时间 / 操作人 |
| N3-05 | 定时任务取消 | P1 | 待发送状态可撤销 |

### N4 类型与模板

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| N4-01 | 通知类型 | **P0** | `system`（系统通知）/ `business`（业务通知）；后者带 `business_key` 供 D 批次审批流调用（附录 E） |
| N4-02 | 模板列表 | **P0** | 列：模板编码 / 名称 / 类型 / 标题 / 变量数 / 状态 / 更新时间 |
| N4-03 | 模板编辑 + 变量 | **P0** | `{{变量}}` 语法；变量从白名单下拉选；右侧实时预览（5.7） |
| N4-04 | 测试发送 | **P0** | 发给"我自己"，用假数据渲染，不产生正式消息（只写入"测试"标记，24h 清理） |

### N5 通道与保留

| 编号 | 功能 | 优先级 | 说明 |
|---|---|---|---|
| N5-01 | 站内信通道（实现） | **P0** | 唯一真正会投递的通道 |
| N5-02 | 邮件 / Webhook 配置与开关 | **P0** | **只存配置 + 开关，不实现发送**；发送时该通道记"已跳过（未实现）"（5.7） |
| N5-03 | 消息保留与自动清理 | **P0** | 未读永久 / 已读 180 天 / 测试消息 24h；每日 03:50（D8） |

**N 小计：21 项（P0 19 / P1 2）**

---

## 2.4 总计与优先级分布

| 模块 | 功能项 | P0 | P1 |
|---|---|---|---|
| 对象存储 S | 34 | 29 | 5 |
| 导入导出 I | 25 | 22 | 3 |
| 消息通知 N | 21 | 19 | 2 |
| **合计** | **80** | **70** | **10** |

> **交付判据**：70 个 P0 全绿 = 本批可上线；10 个 P1 允许留到下批，但**表结构与接口路径要预留**（不许事后改字段名）。
---

# 三、对象存储详细设计

## 3.0 一句话说清本模块在改什么

> **现状**：文件只有一个"硬盘目录"，上传走一个白名单常量，下载永远是"另存为"，谁都能看所有文件。
> **本批**：把"文件往哪存"变成可配置的后端（本地 / MinIO 类），把"能传什么"变成按业务分类的策略，把"大文件怎么传"变成浏览器直传，把"私有文件怎么给"变成限时链接，把"密钥怎么存"变成加密 + 掩码。
> **红线**：**现有 `/api/support/file/*` 五个接口的 URL、入参、出参一个都不许改**——ERP 与 BI 两个批次的附件功能已经依赖它（F13/F17）。

---

## 3.1 数据模型

### 3.1.1 存储后端配置表 `sys_storage_backend`（新建）

| 字段 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `id` | UUID | PK，默认 `gen_random_uuid()` | 主键 |
| `name` | VARCHAR(64) | NOT NULL | 配置名称，如「生产 MinIO」「本地默认」；软删下唯一 |
| `backend_type` | VARCHAR(20) | NOT NULL | `local` / `minio`。**只有这两种**（D2）；S3 兼容与 OSS 都以 `minio` 落库，靠 `preset` 区分展示 |
| `preset` | VARCHAR(20) | NULL | 展示用预设：`minio` / `s3` / `oss`；仅影响前端标签与提示，不影响后端行为 |
| `endpoint` | VARCHAR(255) | NOT NULL | 本地后端填根目录（如 `uploads`）；MinIO 类填 `http(s)://host:port` |
| `bucket` | VARCHAR(128) | NULL | 桶名；本地后端为空 |
| `region` | VARCHAR(64) | NULL | 签名区域，MinIO 默认 `us-east-1` |
| `access_key` | VARCHAR(512) | NULL | **AES-256-GCM 密文**，存 `enc:v1:{nonce}:{cipher}` 格式（F22） |
| `secret_key` | VARCHAR(1024) | NULL | 同上，**密文**；任何接口不返回明文（3.7.3） |
| `use_tls` | BOOLEAN | NOT NULL DEFAULT FALSE | 是否 https |
| `root_prefix` | VARCHAR(255) | NULL | 对象键公共前缀，如 `cloudpivot/`；用于多系统共用一个桶 |
| `is_default` | BOOLEAN | NOT NULL DEFAULT FALSE | 是否默认；全表**最多一行为真**（部分唯一索引） |
| `status` | SMALLINT | NOT NULL DEFAULT 1 | 1 启用 / 0 停用 |
| `last_check_at` | TIMESTAMPTZ | NULL | 最近一次连通性测试时间 |
| `last_check_ok` | BOOLEAN | NULL | 最近一次测试结果；`NULL` = 从未测过 |
| `last_check_ms` | INT | NULL | 最近一次测试耗时（毫秒） |
| `remark` | VARCHAR(255) | NULL | 备注 |
| `created_at` / `updated_at` / `create_by` / `update_by` / `deleted_at` | — | 与平台一致 | 软删 |

**索引**
- `uk_sys_storage_backend_name`：`(name) WHERE deleted_at IS NULL` —— 名称不重复
- `uk_sys_storage_backend_default`：`(is_default) WHERE is_default = TRUE AND deleted_at IS NULL` —— **保证"默认"全局唯一，靠数据库兜住并发切换**

> **为什么加 `preset` 而不是加 4 个 `backend_type`**：F21 已证框架只有 `local` 与 `minio` 两个实现，`MinioBackend` 官方注释写明兼容 S3/OSS/COS。加 4 个类型会让开发写 2 个空壳类只改 `backend_type()` 返回值，除增加维护面无任何收益。`preset` 只承担"给用户看清自己填的是哪家云"和"自动填端点模板"两个产品职责。

### 3.1.2 上传策略表 `sys_storage_policy`（新建）

| 字段 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `id` | UUID | PK | 主键 |
| `category` | VARCHAR(50) | NOT NULL | 业务分类，取值来自字典 `storage_category`；`default` 为保留字（兜底行） |
| `display_name` | VARCHAR(64) | NOT NULL | 中文名，如「通用附件」 |
| `max_size_mb` | INT | NOT NULL DEFAULT 20 | 单文件上限（MB），范围 1–2048 |
| `allowed_extensions` | TEXT[] | NOT NULL | 扩展名白名单（小写、不含点） |
| `allowed_mime` | TEXT[] | NULL | MIME 白名单；为空表示不校验 MIME |
| `check_magic` | BOOLEAN | NOT NULL DEFAULT FALSE | 是否校验"文件头真实类型 vs 扩展名" |
| `allow_private` | BOOLEAN | NOT NULL DEFAULT TRUE | 该分类是否允许标记为私有文件 |
| `direct_upload` | BOOLEAN | NOT NULL DEFAULT FALSE | 该分类是否允许走预签名直传（大文件分类才开） |
| `status` | SMALLINT | NOT NULL DEFAULT 1 | 1 启用 / 0 停用 |
| `created_at` / `updated_at` / `create_by` / `update_by` / `deleted_at` | — | 与平台一致 | — |

**索引**：`uk_sys_storage_policy_category`：`(category) WHERE deleted_at IS NULL`

### 3.1.3 `sys_file` 表改造（3 列 + 1 索引）

在既有表上**只加不改**（F13 的字段一个都不动）：

| 新增字段 | 类型 | 约束 | 说明 |
|---|---|---|---|
| `storage_id` | UUID | NULL | 指向 `sys_storage_backend.id`。**`NULL` = 本批上线前上传的历史文件**，一律按"本地默认目录"读取（3.2.2） |
| `is_private` | BOOLEAN | NOT NULL DEFAULT FALSE | 是否私有文件；为真时下载必须走限时链接 |
| `upload_status` | SMALLINT | NOT NULL DEFAULT 1 | 1 完成 / 0 直传中 / 2 直传失败（仅直传链路使用） |

**新增索引**：`idx_sys_file_storage_id (storage_id)`、`idx_sys_file_private (is_private) WHERE is_private = TRUE`、`idx_sys_file_created_at (created_at DESC)`（支撑"按时间范围"筛选，现有只有 category / uploaded_by 两个索引）

**`storage_path` 列语义变更（必须写进迁移说明）**：现值为**磁盘绝对路径**（`handler.rs` 里 `format!("{dir}/{file_key}")`），改造后统一存**相对后端根的键**（如 `2026/10/28/{uuid}.png`）。历史行的绝对路径**不改**，读取时按"是否以 `/` 开头"判定是老值并只取最后一段 `file_key`（兼容方案见 3.9）。

---

## 3.2 存储路由：一个文件从哪个后端读

### 3.2.1 为什么业务侧要自己维护路由表（而不是直接用 `BackendRegistry`）

F21：`BackendRegistry` 的键是 `backend_type` 字符串，`register()` 同类型**直接覆盖**。这意味着：

- 如果按框架用法把配置 `注册` 进去，两条 `minio` 配置会互相覆盖，只剩最后一条；
- 而用户明确需要"新增多条配置、随时切换默认"，这是产品需求（S1-02/S1-05）。

**定法**：业务侧建一个 `StorageRouter`（放在 `backend/src/business/storage/`），内部是 `配置 id → 后端实例` 的映射（进程内缓存），实例由 `MinioBackend::from_config()` / `LocalFs::new()` 创建——**复用框架的两个实现，但不复用框架的注册中心**。`health_check()`、`presign_download_url()` 这些方法照常调用。

> 给 architect 的一句话：这不是"绕过框架"，框架 trait 就是给业务实现调用的（`backend.rs` 文档注释里给了自定义实现示例）。registry 只是**可选的**便利设施，按 id 路由时它不适用。

### 3.2.2 读文件的路由规则（三条，按顺序判定）

| 顺序 | 条件 | 走哪里 | 典型场景 |
|---|---|---|---|
| 1 | `storage_id` 非空 | 按 id 找后端配置；配置已停用/已删也**照常读**（只禁止写入） | 开关切换后上传的新文件 |
| 2 | `storage_id` 为空 且 `storage_path` 以 `/` 开头 | 取 `storage_path` 最后一段作为 key，走**本地默认目录**（`[upload].path`） | 本批上线前的历史文件 |
| 3 | `storage_id` 为空 且 `storage_path` 不以 `/` 开头 | 同上按本地目录 + `file_key` 读 | 极端兜底 |

**这条规则就是"存储切换后旧文件仍可下载"的实现**（第九章场景 1）。**禁止**用"当前默认后端"去读历史文件——那是这个功能最容易写错的一行代码。

### 3.2.3 写文件的路由规则

| 场景 | 走哪里 |
|---|---|
| 普通上传（≤ 直传阈值） | **当前默认后端**（`is_default=TRUE` 且 `status=1`） |
| 预签名直传 | 申请时指定 `storage_id`（页面默认取当前默认后端），**对象直落该后端** |
| 导入错误明细 / 导出产物 | 固定走**默认后端**的保留前缀 `_system/impexp/`（不占用户可见的文件列表，`category='export'` 且 `is_visible` 不参与文件管理页筛选） |
| 内部临时文件 | 本地磁盘 temp，不落任何后端 |

**对象键（object key）规则统一（重要，防"同一批文件两种目录"）**：
**普通上传与直传必须生成同一个 key 规则** —— 一律走后端的 `write_at(relative_path)` 由业务侧构造：

```
key = {root_prefix}{category}/{YYYY}/{MM}/{DD}/{uuid}.{ext}
例（root_prefix 为空、category=image）：image/2026/10/28/0192f3a4-....png
```

**为什么不让框架的 `write()` 自动命名**：F20 已证该方法的路径规则是 `YYYY/MM/DD/{uuid}.{ext}`（**不含 category**）。如果普通上传用 `write()`、直传用业务构造的 key，同一个用户的两种上传会落到两个不同目录结构里，运维做对象存储盘点、按分类做生命周期策略（如"3 天删 export/"）时全都对不上。**统一走 `write_at`，`write()` 不在业务里使用**。

### 3.2.4 存量接口改造（`/api/support/file/*` 契约零改动）

| 接口 | 改造点（内部） | 对外契约 |
|---|---|---|
| `POST /support/file/upload` | ① 大小/类型校验从"读全局 `max_size_mb`"改为"查 `sys_storage_policy`"；② 落盘从 `std::fs::write` 改为 `router.write()`；③ 新增写 `storage_id` / `is_private` / `upload_status=1` | **不变**（`?category=` 仍是唯一入参） |
| `POST /support/file/upload/batch` | 同上，逐个文件走同一逻辑 | **不变** |
| `GET /support/file/check-duplicate` | 命中后**追加当前分类的策略校验**（3.4.4） | **不变**（`duplicated` / `file` 结构不变；命中但不合规时返回 `duplicated=false`） |
| `GET /support/file/page` | 追加 3 个可选筛选：`uploaded_by` / `created_from` / `created_to`；响应 `FileRes` **追加** `storage_id` / `storage_name` / `is_private` 三个字段（**只增不改**，老前端不受影响） | 向后兼容 |
| `DELETE /support/file/{id}` | 删除时按 `storage_id` 路由到正确后端删对象（现在只会删本地磁盘） | **不变** |
| `GET /support/file/download/{id}` | 支持 `?disposition=inline`（默认 `attachment`）；私有文件**拒绝**此接口直接下载，必须换短链（3.6） | **默认行为不变** |

> **测试要点**：改造后必须回归 ERP/BI 两批的附件上传与下载，特别是"历史文件（`storage_id` 为空）能下载"这一条。

---

## 3.3 连通性测试（S1-06）

### 3.3.1 交互

**两个入口**：① 编辑表单底部的「测试连接」按钮（**保存前**可用，用表单当前值测，不落库）；② 列表页每行的「测试」文字链（用已保存的配置测）。

点下去后：按钮变 loading（`t-button :loading`），旁边出现一个状态条，最多等 **10 秒**。

### 3.3.2 六类结果与文案（必须逐条区分，不能统一说"连接失败"）

| # | 情况 | 判定依据 | 用户看到的文案（附录 A 编号） |
|---|---|---|---|
| 1 | 成功 | `health_check()` 返回 Ok | 绿条：「连接成功 · 桶 `cloudpivot` 已存在 · 延迟 **42ms**」（E-STG-01） |
| 2 | 桶不存在但可创建 | health 报 NotFound，但换用"创建桶"后成功 | 黄条：「连接成功，但桶 `cloudpivot` 不存在，已自动创建（Region: us-east-1）」 |
| 3 | 鉴权失败 | AWS SDK 返回 403 / AccessDenied | 红条：「连接失败：AK/SK 无效或无权限（服务端返回 AccessDenied）。请检查密钥是否过期、该密钥是否有 `s3:*` 或至少 `s3:GetBucketLocation` 权限」（E-STG-02） |
| 4 | 网络不可达 | 连接超时 / DNS 解析失败 | 红条：「连接失败：无法访问 `minio.corp.local:9000`（连接超时 10s）。请检查地址、端口、防火墙与内网互通」（E-STG-03） |
| 5 | 端点风格不匹配 | 返回 400 / `InvalidRequest`，且 `use_tls=true` 且 endpoint 不含 `minio` | 红条：「连接失败：端点风格不匹配。若使用 MinIO 请确认端点地址包含 `minio` 或用 http（框架按 `endpoint 含 minio 或 关闭 TLS` 判定 path-style）」（E-STG-04）—— 依据 F20 |
| 6 | 区域不匹配 | 返回 `PermanentRedirect` / `AuthorizationHeaderMalformed` | 红条：「连接失败：Region 不匹配（实际区域可能是 `cn-hangzhou`）。阿里 OSS 请填 `oss-cn-hangzhou` 这类区域值」（E-STG-05） |

**测试结果的落库**：列表行写入 `last_check_at` / `last_check_ok` / `last_check_ms`，列表"连通性"列显示「正常 42ms」/「失败」/「未测试」。

**安全**：测试**不写审计**（高频、无副作用），但**记录一条操作日志**（`operation_logs`）便于排障；测试用的 AK/SK **不入日志**。

---

## 3.4 上传策略（S2）

### 3.4.1 策略怎么选（优先级）

上传时按 `category` 精确查 `sys_storage_policy`：
1. 命中且 `status=1` → 用该行；
2. 未命中 / 已停用 → 用 `category='default'` 的兜底行；
3. 兜底行也没了（异常） → 用**代码内硬兜底**：`20MB` + 现有 31 个扩展名（保持 F14/F15 的现有行为，**绝不因为配置缺失就变成无限制**）。

> **红线**：任何情况下都**不允许出现"策略缺失=放行"**。这是上传功能最常见的安全事故原因。

### 3.4.2 默认策略（随迁移写入，可直接用）

| category | 中文名 | 大小上限 | 扩展名白名单 | 校验 MIME | 允许私有 | 允许直传 |
|---|---|---|---|---|---|---|
| `avatar` | 头像 | 5MB | jpg, jpeg, png, webp | ✅ | ❌ | ❌ |
| `attachment` | 通用附件 | 20MB | （现有 31 个全部） | ❌ | ✅ | ❌ |
| `document` | 公文/合同 | 50MB | pdf, doc, docx, xls, xlsx, ppt, pptx, txt, zip | ✅ | ✅ | ❌ |
| `image` | 图片素材 | 50MB | jpg, jpeg, png, gif, bmp, webp, svg | ✅ | ❌ | ✅ |
| `video` | 音视频 | **2048MB** | mp4, mov, avi, mkv, mp3, wav | ✅ | ✅ | ✅ |
| `import` | 导入源文件 | 100MB | xlsx, xls, csv | ✅ | ✅ | ❌ |
| `export` | 导出产物 | 200MB | xlsx | ❌ | ✅ | ❌ |
| `default` | 兜底 | 20MB | （现有 31 个全部） | ❌ | ✅ | ❌ |

**为什么 `attachment` 默认不开 MIME 校验**：历史上有 12,486 个文件（故事 1 的数字）是用旧规则传的，一上来就开魔数校验会让"重新上传同一批文件"出现行为差异；把它作为**开关**交给安全部门按需打开，并配上"存量文件不回溯校验"的说明（附录 A E-STG-13）。

### 3.4.3 MIME 魔数校验规则（S2-04）

**为什么必须加**：F14 已证现有实现**只比较扩展名**，把 `evil.exe` 改名 `evil.pdf` 就能上传成功，这是可被利用的漏洞（故事 2 就是这个场景）。

**判定方法**：读文件前 **512 字节**，取"魔数"判断真实类型，与 `extension` 声明的类型族比对：

| 文件族 | 魔数（十六进制前若干字节） | 覆盖扩展名 |
|---|---|---|
| PDF | `25 50 44 46`（`%PDF`） | pdf |
| PNG | `89 50 4E 47 0D 0A 1A 0A` | png |
| JPEG | `FF D8 FF` | jpg, jpeg |
| GIF | `47 49 46 38`（`GIF8`） | gif |
| BMP | `42 4D` | bmp |
| WEBP | `52 49 46 46` … `57 45 42 50`（RIFF + WEBP） | webp |
| ZIP 容器 | `50 4B 03 04` | zip, **docx, xlsx, pptx**（OOXML 本质是 zip） |
| OLE 复合文档 | `D0 CF 11 E0 A1 B1 1A E1` | doc, xls, ppt |
| MP4 | 第 4–8 字节为 `66 74 79 70`（`ftyp`） | mp4, mov |
| MP3 | `49 44 33`（`ID3`）或 `FF FB`/`FF F3` | mp3 |
| WAV | `52 49 46 46` … `57 41 56 45`（RIFF + WAVE) | wav |
| 文本类 | 无可识别魔数 → **跳过校验**（放行） | txt, csv, md, json, xml, svg |

**判定不一致时的文案**（附录 A E-STG-06）：
> 「文件内容与扩展名不符（扩展名 `.pdf`，实际类型 `application/x-msdownload`），已拒绝上传。请确认文件本身是否被篡改。」

**注意**：`svg` 是文本 XML，且**可内嵌 `<script>`** —— 上传放行，但**预览时必须走 `Content-Disposition: inline` + `Content-Security-Policy: sandbox`**，或干脆只给下载不给预览（3.8.3 里明确 svg 不给在线预览）。

### 3.4.4 秒传与策略的关系（S3-03）

现状（F17）：只按 `file_hash` 命中就复用，**不看分类**。这会被利用：

> A 分类（`attachment`，20MB/31 种）传一个 15MB 的 `.exe-改名.pdf`，秒传成功；B 分类（`document`，开了魔数校验）再用同一文件秒传，**直接命中复用，绕过 B 的校验**。

**定法**：秒传命中后，**仍用当前 `category` 的策略做一遍"元数据级"校验**：
1. 扩展名在 `allowed_extensions` 内？不在 → 拒绝（不命中秒传）；
2. `file_size ≤ max_size_mb`？超了 → 拒绝；
3. `check_magic=true` 时：**不重新读对象内容**（秒传的意义就是不读），而是检查**首次入库时记录的 `mime_type`**是否在该分类的 `allowed_mime` 内；不在 → 拒绝。

**用户看到的差异**：`check-duplicate` 接口在"命中但不合规"时返回 `duplicated=false` + `file=null`，前端正常走上传流程，上传时被策略拦截并给出具体原因。**不告诉调用方"因为秒传被拒"**（避免暴露其他分类的内容存在性）。

---

## 3.5 大文件直传（S3-04 / S3-05 / S3-06 / S3-07）

### 3.5.1 什么文件走直传

| 条件 | 判定 |
|---|---|
| 后端类型 = `local` | **不支持直传**，一律走后端中转（3.5.5） |
| 后端类型 = `minio`（含 S3/OSS） 且 分类 `direct_upload=true` 且 文件大小 > **50MB** | 走直传 |
| 其它 | 走后端中转 |

> **50MB 的由来**：F28，nginx `client_max_body_size 50m`。低于 50MB 的走后端中转最省事（不用跨域、不用回调）；超过就必须直传，否则直接被 nginx 挡。

### 3.5.2 直传时序（8 步，每一步都要能落到界面反馈）

```
① 用户选文件（前端读到 file.size、file.name、file.type）
② 前端算 SHA-256（>100MB 用抽样的"快速指纹"：头 4MB + 尾 4MB + 文件大小；
   完整 SHA-256 留给后端在"确认"时对对象做一次 HEAD 校验大小即可）
③ 前端 → 后端 POST /business/storage/upload/presign
     入参：file_name / file_size / mime_type / category / sha256(或 fast_fingerprint)
     后端：① 查分类策略（大小/扩展名/MIME 全部先在这里拦掉）
           ② 查 file_hash 是否可秒传 → 命中直接返回 duplicated
           ③ 生成 object_key = {root_prefix}{category}/{YYYY}/{MM}/{DD}/{uuid}.{ext}
           ④ 调 presign_upload 拿 PUT URL（有效期 15 分钟）
           ⑤ 落一条 sys_file：upload_status=0（直传中）、storage_id、is_private、file_size
     返回：{ file_id, upload_url, method:"PUT", headers:{"Content-Type": mime}, expires_in:900 }
④ 前端用 XMLHttpRequest 直传（必须用 XHR 才能拿到 upload.onprogress）：
     显示进度条「正在上传 187MB / 500MB（37%）· 剩余约 42 秒」
⑤ 传完对象存储返回 200（ETag）
⑥ 前端 → 后端 POST /business/storage/upload/confirm  { file_id, etag }
     后端：① HEAD 对象校验存在且 size 与申报一致（防"报了 500MB 实际传了 1KB"）
           ② 超限/不一致 → upload_status=2 并删除对象
           ③ 一致 → upload_status=1，写审计 FILE_UPLOAD
⑦ 前端刷新文件列表，出现新文件；返回 FileRes
⑧ 若前端在上传中关页面：记录停留 upload_status=0，2 小时后由清理任务删对象 + 软删记录
```

### 3.5.3 前端必须处理的 7 种失败（少一种就会出现"幽灵文件"）

| # | 失败点 | 界面表现 | 后端配合 |
|---|---|---|---|
| 1 | presign 接口报错（策略不合规） | 弹「上传失败：文件大小超过上限 20MB（当前 187MB）」，**不进入上传** | 返回 422 + 明确原因 |
| 2 | 直传网络中断 | 进度条变红「上传中断（已传 120MB / 500MB），点击重试」；重试重新申请 URL | — |
| 3 | 对象存储返回 403（URL 过期） | 自动重新申请一次 URL 再传（**只自动重试 1 次**），仍失败提示「上传凭证已过期，请重新上传」 | presign 有效期 15 分钟 |
| 4 | 直传成功但 confirm 失败 | 「文件已上传但登记失败，请点击重试」+ 重试按钮（携带同一 file_id 再调 confirm） | confirm 幂等 |
| 5 | confirm 时大小不一致 | 「文件校验失败：申报 500MB，实际 1.2KB」 | `upload_status=2` + 删对象 |
| 6 | 用户主动取消 | 进度条消失，**静默调用一次 abort 通知**（后端删对象 + 软删记录） | 新增 `DELETE /business/storage/upload/{file_id}` 处理"直传中"记录 |
| 7 | 浏览器不支持 XHR 进度 / 跨域失败 | 降级提示「当前浏览器不支持直传，已切换为普通上传（大文件可能失败）」 | 后端中转兜底，但会撞 nginx 50m |

> **跨域提醒（给 architect）**：直传要求对象存储桶配置 CORS：允许 `PUT`，`AllowedOrigin` 为云枢前端域名，`AllowedHeader: content-type`，`ExposeHeader: ETag`。这条**必须写进部署清单**，否则浏览器在预检请求阶段就失败（附录 C.2）。

### 3.5.4 "直传中"记录的清理（S3-06）

- **触发**：每日清理任务（与消息清理同一批 cron，见 5.8）扫 `upload_status=0 AND created_at < now() - 2h`。
- **动作**：删对象（失败只告警）→ 软删 `sys_file` 行 → 写审计 `FILE_UPLOAD_ABORT`。
- **为什么是 2 小时**：500MB 在 10Mbps 上行下约 7 分钟；2 小时足够覆盖"断点后回来继续传"的极端情况，又不会让垃圾对象躺一整天。

### 3.5.5 本地后端降级（S3-07）

本地后端没有预签名（F20），直传降级为"**后端分块流式中转**"：

- 前端按 **8MB 一块**顺序 `POST /business/storage/upload/chunk?file_id=&index=`；
- 后端把每块**直接追加写入**目标文件，**不在内存里攒整个文件**（这是对现有 `read_upload_fields()` 全量 `to_vec()` 的必要修正，F19）；
- 全部块到齐后前端调 `confirm`，后端校验总大小与块数；
- 限制：**必须同步调大 nginx `client_max_body_size`**（附录 C.1 交付前提），且大文件（>500MB）在本地后端上**明确不承诺**——页面提示「当前为本地存储，超大文件建议由管理员切换至对象存储后上传」。这句话不能省，否则运维会以为本地也能扛 2GB。

---

## 3.6 私有文件与限时下载链接（S4）

### 3.6.1 两种实现，按后端类型分派

| 后端类型 | 实现 | 链接长什么样 |
|---|---|---|
| `minio`（含 S3/OSS） | 调 `presign_download_url(stored_path, expiry_secs)`（F19/F20），生成**真的对象存储预签名 URL** | `https://minio.corp.local:9000/cloudpivot/...?X-Amz-Algorithm=...&X-Amz-Expires=3600&X-Amz-Signature=...` （浏览器**直连对象存储**，不经后端） |
| `local` | 后端**自签短链**（3.6.2），走后端流式输出 | `https://cp.example.com/api/business/storage/private/{file_id}?exp=1793260800&sig=9f2c...` |

### 3.6.2 本地后端签名短链（必须写清，这是安全项）

**链接结构**：`/api/business/storage/private/{file_id}?exp={unix_seconds}&sig={hex}`

**签名算法**（产品规格，开发照做）：
1. 待签串 `msg = file_id + "|" + exp + "|" + private_salt_version`（例：`a1b2...|1793260800|v1`）
2. `key = HMAC-SHA256(X_CONFIG_SECRET_KEY, "storage-private-link:v1")` —— **从主密钥派生子密钥，不直接拿主密钥签**（F22 的密钥已有，不引新密钥）
3. `sig = hex(HMAC-SHA256(key, msg))`，取**前 32 个十六进制字符**（128 位，足够抗穷举）
4. 校验：① `exp` 未过期；② 用**常量时间比较**校验 `sig`（禁止 `==` 短路）；③ 文件 `is_private=true`

**为什么不用 JWT**：短链要能**发给外部人员**（供应商、客户看合同），不能要求登录；JWT 会让人误以为里面能塞权限声明。这个短链**只授权"下载这一个文件"**，不含任何身份信息。

**明确不做的（写进代码注释）**：不绑定用户、不绑定 IP（否则转发就失效，与"发给外部"的诉求冲突）。P1 再考虑"弱绑定首次访问 UA"。

### 3.6.3 有效期与上限

| 项 | 默认 | 范围 | 存在哪里 |
|---|---|---|---|
| 私有文件下载链接有效期 | **60 分钟** | 5 分钟 – 7 天 | `sys_config: storage.presign.ttl_seconds` |
| 导出产物下载链接有效期 | **24 小时** | 1 小时 – 7 天 | `sys_config: impexp.export.link_ttl_hours` |
| 直传 PUT URL 有效期 | **15 分钟** | 5 – 60 分钟（固定值，不开放配置） | 代码常量 |
| 链接**过期后的界面** | 不返回 404，返回明确提示：「下载链接已过期（有效期 60 分钟），请重新获取」+「重新获取」按钮 | — | — |

> **注意 MinIO 预签名 URL 的硬限制**：AWS Signature V4 的 `X-Amz-Expires` **最大 7 天（604800 秒）**。超过 7 天的值对象存储会直接报错，所以上限锁 7 天，**不要给"永久链接"选项**（给了也生不出）。

### 3.6.4 安全红线（4 条）

1. **私有文件禁止走 `/support/file/download/{id}` 直接下载**——该接口只认登录态、不认链接有效期，会绕过限时机制。私有文件访问此接口一律返回 403 + 「该文件为私有，请通过限时链接下载」。
2. **私有文件的 `url` 字段不返回长期地址**：`FileRes.url` 对非私有文件返回原下载地址；对私有文件返回**空串**，前端改为调 `GET /business/storage/file/{id}/link` 现取现用。
3. **链接签名错误与文件不存在返回同一个响应**（403 + 「链接无效或已过期」），避免被用来探测某个 file_id 是否存在。
4. 私有文件列表对无 `business:storage:file:download-private` 权限的用户**不显示下载/预览按钮**，但**能看到元数据**（文件名、大小、上传人）——元数据不敏感，隐藏整个文件会导致"明明有附件却看不到"的困惑。

---

## 3.7 AK/SK 管理（S1-07 / S1-08）—— 本批安全等级最高的一段

### 3.7.1 加密存储（复用平台能力，不引第二套）

| 项 | 定法 |
|---|---|
| 加密算法 | **AES-256-GCM**，直接用 `alun_utils::Crypto::aes_encrypt/aes_decrypt`（F22） |
| 落库格式 | `enc:v1:{nonce_b64}:{cipher_b64}`（与平台 `sys_config` 完全一致，便于统一轮换工具） |
| 密钥来源 | 环境变量 **`X_CONFIG_SECRET_KEY`**（F22 已定义，不新引环境变量） |
| 密钥未配置时 | **禁止保存含 AK/SK 的配置**：返回 500 + 「未配置参数加密密钥（环境变量 X_CONFIG_SECRET_KEY），无法加密存储密钥」（附录 A E-STG-11）。**绝不允许"降级为明文存"**——现有 `encrypt_config_value()` 就是这么设计的（注释原文："绝不静默明文落库"） |
| 解密时机 | 仅在**真正要发对象存储请求前**（建后端实例时）在内存中解密；解密结果**不进缓存、不进日志、不进任何响应** |

### 3.7.2 掩码规则（列表与详情）

| 字段 | 存 | 返回给前端 | 例子 |
|---|---|---|---|
| `access_key` | 密文 | **掩码**：保留前 4 + 后 4，中间固定 `****`（若原文 < 12 字符则保留前 2 + 后 2） | `AKIA****7ZP2` |
| `secret_key` | 密文 | **恒为空串 `""`**（不是掩码，是不返回） | （空） |
| 是否有密钥 | — | 额外返回布尔 `has_credentials` | `true` |

> **SK 连掩码都不给**：SK 的长度与字符集本身就是信息（部分云厂商 SK 有固定长度），给"前4后4"等于泄露了 8 位。而 AK 本身不是秘密（很多云控制台公开展示 AK），掩码只是为了"不被截图直接抄走"。

### 3.7.3 "不落前端明文"的四条实现约束

这是任务书明确要求的，逐条落实：

1. **响应 DTO 与表模型分离**：`StorageBackendRes` 里 `secret_key` 字段**固定填 `""`**，`access_key` 填掩码值。**禁止**把 `SysStorageBackendModel` 直接 `Serialize` 出去（现有 `FileRes` 就是这么做的良好示范）。
2. **编辑时"留空即不变"**：编辑表单里 AK/SK 两个输入框 `type=password`、`placeholder` 分别为「留空则不修改」/「留空则不修改」。**用户不改密钥就不需要重新输入**（否则每次改名都要重填密钥，运维一定骂人）。
3. **改密钥必须二次确认**：点保存时若检测到 AK/SK 输入框非空，弹框「即将更新该存储的访问密钥，更新后立即生效。若密钥错误，新上传会失败（已有文件不受影响）。是否继续？」，并要求再次输入配置名称确认（防误操作，也不至于让人去翻键盘）。
4. **审计只记"改了"不记值**：写 `audit_logs` 时 `before_data` / `after_data` **只放 `{"access_key":"已更新","secret_key":"已更新"}`**，禁止放任何明文或密文片段。

### 3.7.4 密钥轮换流程（S1-08）

云厂商要求定期轮换 SK 时的标准动作（写进页面的「帮助」气泡）：

1. 在云控制台建第二个 AK/SK（**不删旧的**）；
2. 回到云枢编辑该存储配置，**只改 AK/SK**（端点/桶不允许改，S1-03），保存；
3. 点「测试连接」确认 `连接成功`；
4. 在云控制台删除旧 AK；
5. 用同一个文件做一次"上传 + 下载"验证（页面提供快捷入口）。

> **为什么不支持"两个密钥并存、灰度"**：对象存储的签名只认一套当前密钥，应用层做双写没有意义（不像数据库密码可以双开）。轮换必须在"改配置 → 验证 → 删旧"三步内完成，中间那几分钟是唯一窗口。

### 3.7.5 安全红线清单（评审时逐条打勾）

| # | 红线 | 验证方式 |
|---|---|---|
| 1 | AK/SK 不以任何形式进日志（含 tracing、operation_logs、audit_logs、错误堆栈） | 全文搜索日志，`grep` 不到 AK 值 |
| 2 | SK 不出现在任何 HTTP 响应（含 500 错误的 detail 字段） | 抓包 / 接口测试断言 |
| 3 | SK 不出现在前端 localStorage / sessionStorage / Pinia 状态 | 浏览器 DevTools 检查 |
| 4 | 密钥加密不可用时**拒绝保存**，不降级明文 | 不配 `X_CONFIG_SECRET_KEY` 启动，尝试保存 → 报错 |
| 5 | 密文不可跨环境解密（换密钥后旧密文解不开 → 明确报错而非静默空值） | 换密钥重启，读配置 → 报 `CONFIG_DECRYPT_FAILED` |
| 6 | 存储配置的增删改、设默认、改密钥**必须**有 `business:storage:backend:create/update/delete/default` 权限码（前端隐藏 + 后端再校验） | 用无权限账号直接调接口 → 403 |

---

## 3.8 文件管理页增强（S5）

### 3.8.1 筛选区（5 个条件 + 查询/重置）

| 条件 | 控件 | 说明 |
|---|---|---|
| 文件名 | `t-input` | 原文件名模糊，回车即查（沿用 F8 范式） |
| 分类 | `t-select` | 选项来自字典 `storage_category`（F31），**不再让用户手输英文短码**（现有实现是 `t-input` 手输 `attachment`，这是可发现性灾难） |
| 上传人 | 人员选择器 | 支持按姓名/工号搜；普通用户只能选到自己可见范围 |
| 上传时间 | `t-date-range-picker` | 快捷项：今天 / 近 7 天 / 近 30 天 / 今年 |
| 文件类型 | `t-select` | 图片 / 文档 / 压缩包 / 音视频 / 其他（按扩展名分组，不是按 MIME 逐个列） |

### 3.8.2 列表列

| 列 | 宽度 | 说明 |
|---|---|---|
| 多选框 | 48 | 支持跨页选中（`t-table` 的 `selectedRowKeys` 保留） |
| 文件名 | 自适应 | 前面带类型图标（`file-image` / `file-pdf` / `file-excel` / `file-zip` / `file`，F30 均已确认存在）；私有文件后面加 `t-tag theme="warning"` 标「私有」 |
| 分类 | 100 | 中文名（字典翻译） |
| 大小 | 100 | 人读格式（`1.2 MB`） |
| 上传人 | 120 | 姓名，不再是 UUID（**现有实现直接显示 `uploaded_by` 的 UUID 字符串，这是必须修的**） |
| 存储后端 | 120 | `storage_name`；历史文件显示「本地（历史）」 |
| 上传时间 | 170 | `YYYY-MM-DD HH:mm` |
| 操作 | 180 | 预览（图片/PDF 才有）/ 下载 / 详情 / 删除（`v-privilege` 控制） |

### 3.8.3 预览（S5-02）

| 类型 | 实现 | 注意 |
|---|---|---|
| 图片（jpg/png/gif/bmp/webp） | `t-image-viewer`，背景遮罩，支持缩放与左右切换 | 走 `?disposition=inline`（F16 说现有恒为 attachment，必须新增） |
| PDF | 详情抽屉里内嵌 `<iframe>` 指向 inline 地址 | 必须设 `Content-Disposition: inline`，否则浏览器下载而不是显示 |
| **SVG** | **不给预览**，只给下载（按钮置灰 + tooltip「SVG 可含脚本，出于安全考虑不提供在线预览」） | 见 3.4.3 |
| 其它（docx/xlsx/zip/音视频） | 不给预览，提示「该类型不支持在线预览，请下载后查看」 | 视频预览（`<video>`）放 P1，需要 Range 请求支持，本地后端现在没有 |
| 私有文件 | 预览前先取限时链接；无 `download-private` 权限则按钮不可见 | 3.6.4 第 4 条 |

### 3.8.4 批量删除（S5-03）

- 选中 N 条 → 点「批量删除」→ 弹框：「确定删除选中的 **12** 个文件？删除后不可恢复。」（P1 才有回收站，所以现在说"不可恢复"是诚实的）
- 执行：逐个软删 + 删对象；**部分失败不中断**，最后汇总：「删除完成：成功 11 个，失败 1 个（`合同扫描件.pdf`：对象存储中不存在该对象）」
- 超过 100 条时按钮提示「单次最多删除 100 个文件，请分批操作」

---

## 3.9 存量迁移与兼容（必做，否则上线当天就出事）

| # | 事项 | 做法 |
|---|---|---|
| 1 | 历史文件 `storage_id` 为空 | **不回填**。读时走 3.2.2 的第 2/3 条规则。回填反而危险（历史文件确实都在本地盘，回填成"当前默认后端"会把它们指向 MinIO，导致全部打不开） |
| 2 | 历史 `storage_path` 是绝对路径 | 读时判断"是否以 `/` 开头"，取最后一段当 key；**不做批量 UPDATE**（12,486 行改错一行就是丢文件） |
| 3 | 初始默认存储配置 | 迁移里插入一条 `name='本地默认' / backend_type='local' / endpoint='uploads' / is_default=true`，保证"没配过也能上传" |
| 4 | 初始上传策略 | 按 3.4.2 的 8 行写入 |
| 5 | 旧分类值 | 现有库里可能已有 `avatar` / `attachment` / `document` 之外的自由字符串（因为 `category` 是自由输入）。迁移时**保留原值不动**，策略查不到就走 `default` 兜底；文件管理页的分类下拉里，把库里出现过的"非字典值"追加到选项末尾并标注「历史值」 |
| 6 | 磁盘占用 | 切换默认存储后，**旧本地目录不要删、不要搬**。给出运维说明：确认所有历史文件都能下载后，再考虑归档（附录 C.2） |

---

## 3.10 容量与性能指标（给 test-enginner 的量化基线）

| 指标 | 目标值 | 测试方法 |
|---|---|---|
| 普通上传（20MB 内，后端中转） | P95 ≤ 3 秒（内网） | 连续 50 次上传 10MB 文件，统计 P95 |
| 直传（500MB，对象存储在同一内网） | 端到端 ≤ 90 秒（按 50MB/s 带宽），**后端内存增长 ≤ 50MB** | 上传时监控后端 RSS，峰值增长必须 ≤ 50MB（这是"后端不占内存"的硬证据） |
| 预签名下载链接生成 | ≤ 100ms | 连续 200 次调用统计 |
| 文件列表分页（12,486 条数据） | ≤ 200ms | 带 5 个筛选条件的最差组合 |
| 连通性测试 | 成功 ≤ 3s，失败 ≤ 10s（超时即判失败） | 用错误端点、错误密钥、正确端点各测 3 次 |
| 文件元数据表规模 | 100 万行时不需分表 | 造数后跑分页与筛选 |
---

# 四、导入导出详细设计

## 4.0 一句话说清本模块在改什么

> **现状**：平台**完全没有**导入导出功能（F24 已证 `Export`/`Import` API 存在但从未被业务使用，backend 连 `xlsx` feature 都没开）。这一批是从 0 建一条"通用通道"，让后面 ERP/BI/MDM/内容管理都能挂上来。
> **核心设计**：**导入导出不是"某个页面的按钮"，是"配置驱动的通用服务"**——业务只登记"我要导什么字段、哪些必填、拿什么判重"，模板生成、校验、报告、入库、异步任务全部复用。

---

## 4.1 导入定义与模板（I1）

### 4.1.1 导入定义（配置驱动，不在代码里写死业务）

每个可导入的业务登记一条定义（`sys_impexp_definition`），**新增业务不需要改代码**：

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | UUID | 主键 |
| `biz_code` | VARCHAR(50) | 业务标识，如 `erp.item`、`mdm.material`；软删下唯一 |
| `biz_name` | VARCHAR(64) | 中文名，如「物料」 |
| `task_kind` | VARCHAR(10) | `import` / `export` |
| `target_table` | VARCHAR(64) | 落库目标表（导入）；数据来源视图名（导出） |
| `field_schema` | JSONB | 字段定义数组，每项：`{key, label, type, required, max_len, enum_dict, ref_code, unique, sensitive, export_default}` |
| `unique_keys` | TEXT[] | 判重字段（如 `["item_code"]`）；为空表示不判重 |
| `dup_strategy` | VARCHAR(10) | 默认重复策略：`skip`（默认）/ `overwrite` / `error` |
| `allow_partial` | BOOLEAN | 是否允许部分导入，**默认 false**（D6） |
| `max_rows` | INT | 单次行数上限，默认 `100000` |
| `template_version` | INT | 模板版本号，默认 1 |
| `status` | SMALLINT | 1 启用 / 0 停用 |
| 审计字段 | — | 与平台一致 |

### 4.1.2 模板长什么样（I1-01）

生成的 xlsx **固定 3 个 sheet**：

**Sheet 1「数据」（默认打开）**
| 行 | 内容 | 样式 |
|---|---|---|
| 第 1 行 | 表头：`物料编码*` / `物料名称*` / `规格型号` / `计量单位*` / `税率` | 加粗、深色底白字、冻结、列宽按字段长度预设、必填列名带 `*` |
| 第 2 行 | **示例行**：`MAT-000001` / `螺丝 M6×20` / `M6*20mm` / `个` / `13` | 全行浅灰底、斜体，**第 1 列 A 列前加批注**「示例行，导入前请删除」 |
| 第 3 行起 | 空 | — |

**Sheet 2「字段说明」**
| 列 | 内容 |
|---|---|
| 字段名 | `物料编码` |
| 是否必填 | `是` |
| 类型 | `文本` / `数字` / `日期` / `枚举` |
| 长度/范围 | `≤32 个字符` / `0–100` / `YYYY-MM-DD` |
| 说明 | `字母+数字+连字符，全公司唯一，保存后不建议修改` |
| 可选值 | 枚举字段列出全部取值（如 `个 / 件 / 千克 / 米`） |

**Sheet 3「填写须知」**：4~6 条，固定文案（可配）：
1. 请勿修改第 1 行表头，勿增删列；
2. 第 2 行是示例，导入前请删除；
3. 单次最多 `100,000` 行；
4. 必填列（带 `*`）不能为空；
5. 日期格式统一 `YYYY-MM-DD`；
6. 枚举列请从下拉选择，不要手输。

**技术要点**：枚举列写 Excel **数据验证（下拉）**（I1-02），导出的可选值来自字典/引用表；数值列写"整数/小数"验证。这样用户在 Excel 里就被约束一次，减少校验失败率。

### 4.1.3 模板文件名与版本

- 文件名：`{biz_name}导入模板_v{template_version}.xlsx`，如 `物料导入模板_v1.xlsx`
- 用户在页面点「下载模板」时，**如果是第二次及以后下载且定义有过改版**，提示「模板已于 2026-10-20 更新（新增字段：税率），请使用最新模板」

---

## 4.2 解析与校验（I2-01 / I2-02 / I2-03）

### 4.2.1 两级校验，顺序固定

```
一级：结构校验（整表级别，任一失败直接终止，不出明细表格）
  ├─ 文件格式（必须是 .xlsx 或 .xls，按魔数判断，不看扩展名）
  ├─ 是否为空文件
  ├─ 表头是否缺必需列
  ├─ 是否超行数上限
  └─ 表头与模板列顺序是否一致（不一致仅警告，按"表头名映射"继续）

二级：业务校验（行级别，逐行收集错误，最多收集 2000 条明细）
  ├─ 必填
  ├─ 类型（数字列是不是数字）
  ├─ 长度
  ├─ 枚举（值在不在字典里）
  ├─ 数值范围
  ├─ 日期格式
  ├─ 唯一键：文件内重复 + 库内已存在
  └─ 外键存在性（如计量单位必须已存在于主数据）
```

### 4.2.2 行号规则（这个必须统一，否则用户找不到行）

> **行号 = Excel 里的真实行号**（从 1 开始，含表头和示例行）。

例：100 行的 Excel，第 1 行表头、第 2 行示例、第 3~100 行为数据。若第 57 行的"物料名称"为空，报错行号必须是 **57**，不是 56（不是数据第 56 条），也不是 55。

**示例行的处理**：第 2 行若被识别为示例行（判据：该行所有值与模板生成时的示例值**完全一致**），**自动跳过且不报错**，并在报告里注明「已自动跳过 1 行示例数据」。用户删不删都行。

### 4.2.3 三十个错误码（导入 20 + 导出 10）

**导入（E-IMP）**

| 编号 | 触发条件 | 用户文案（附录 A 亦有） | 级别 |
|---|---|---|---|
| E-IMP-01 | 文件魔数不是 xlsx/xls/zip | 文件格式不支持，请上传 Excel 文件（.xlsx / .xls） | 阻断 |
| E-IMP-02 | 只有表头没有数据行 | 文件中没有数据行，请填写后重新上传 | 阻断 |
| E-IMP-03 | 表头缺少必需列 | 文件第 1 行表头缺少必需列：物料编码、计量单位 | 阻断 |
| E-IMP-04 | 存在模板外的列 | 文件中有 2 个不属于本模板的列（备注1、备注2），这些列将被忽略 | 警告 |
| E-IMP-05 | 列顺序与模板不一致 | 表头顺序与模板不一致，已按列名匹配，请确认数据对位是否正确 | 警告 |
| E-IMP-06 | 数据行数 > `max_rows` | 数据共 120,431 行，超过单次上限 100,000 行，请拆分后导入 | 阻断 |
| E-IMP-07 | 数据行数 > 20,000 | 数据量较大（120,431 行），将转为后台校验，完成后站内信通知您 | 提示 |
| E-IMP-08 | 必填项为空 | 必填项为空 | 行级 |
| E-IMP-09 | 类型不符（数字列非数字） | 应为数字，实际为「壹佰」 | 行级 |
| E-IMP-10 | 超长 | 长度超限（最多 32 个字符，实际 45 个） | 行级 |
| E-IMP-11 | 枚举值非法 | 值「台套」不在可选范围内（个 / 件 / 千克 / 米） | 行级 |
| E-IMP-12 | 数值越界 | 数值超出范围（0–100，实际 -5） | 行级 |
| E-IMP-13 | 文件内重复 | 编码已存在（与第 88 行重复） | 行级 |
| E-IMP-14 | 库内已存在 | 编码已存在（库中已有该记录） | 行级 |
| E-IMP-15 | 外键不存在 | 计量单位「打」不存在，请先在计量单位中维护 | 行级 |
| E-IMP-16 | 日期格式错 | 日期格式不正确（应为 YYYY-MM-DD，实际 2026/13/45） | 行级 |
| E-IMP-17 | 合并单元格导致空值 | 该单元格为合并单元格，请取消合并后重新填写 | 行级 |
| E-IMP-18 | 导入定义已停用 | 该导入功能已停用，请联系管理员 | 阻断 |
| E-IMP-19 | 未开部分导入但有失败行 | 存在 5 行校验失败，本批次未开启"允许部分导入"，已全部中止，请修正后重传 | 阻断 |
| E-IMP-20 | 同一业务已有进行中的导入 | 您已有一个「物料」导入正在进行中（进度 42%），请等待完成后再提交 | 阻断 |

**导出（E-EXP）**

| 编号 | 触发条件 | 用户文案 | 级别 |
|---|---|---|---|
| E-EXP-01 | 行数 > 同步阈值 | 数据量较大（203,481 行），已转为后台导出，完成后通知您 | 提示 |
| E-EXP-02 | 未勾选任何字段 | 请至少选择 1 个导出字段 | 阻断 |
| E-EXP-03 | 不含明文权限但要求导明文 | 您没有"导出敏感字段明文"权限，本次已按脱敏规则导出 | 提示 |
| E-EXP-04 | 任务被取消 | 任务已取消 | 状态 |
| E-EXP-05 | 链接过期 | 下载链接已过期（有效期 24 小时），请重新导出 | 阻断 |
| E-EXP-06 | 产物已被清理 | 导出文件已超过保留期（7 天）被自动清理，请重新导出 | 阻断 |
| E-EXP-07 | 执行超过 10 分钟 | 导出执行超时（超过 10 分钟），已终止。请缩小时间范围或减少导出字段后重试 | 失败 |
| E-EXP-08 | 存储空间不足 | 存储空间不足，导出失败。请联系管理员检查存储容量 | 失败 |
| E-EXP-09 | 同用户并发超限 | 您已有 3 个导出任务在进行中，请等待完成后再提交 | 阻断 |
| E-EXP-10 | 无数据 | 当前筛选条件下没有数据可导出 | 阻断 |

---

## 4.3 校验报告与错误明细（I2-04 / I2-05）

### 4.3.1 报告区（导入向导第 2 步，页面见 8.4）

顶部三块数字卡（**必须一眼看懂**）：

| 卡片 | 内容 | 例 |
|---|---|---|
| 总计 | 文件数据行数（不含表头、不含已跳过的示例行） | **5,000** 行 |
| 可导入 | 通过全部校验的行数 | **4,995** 行（绿色） |
| 失败 | 未通过的行数 | **5** 行（红色） |

下方一行补充说明（按策略动态生成）：
- 默认（全量事务）：`存在 5 行校验失败。当前策略为"全部导入或全部不导入"，请修正后重新上传。`
- 开启部分导入：`存在 5 行校验失败。将导入 4,995 行，跳过 5 行。`
- 有重复跳过：`其中 12 行因编码已存在被跳过（策略：跳过）。`

### 4.3.2 失败明细表（最多展示 200 条，下拉加载）

| 列 | 内容 |
|---|---|
| 行号 | 57（Excel 真实行号，**可点击跳转**——点开一个抽屉显示该行全部列的值，方便对照原表） |
| 列名 | 物料名称 |
| 原值 | （空）/ `壹佰` / `台套` |
| 失败原因 | 必填项为空 / 应为数字… |
| 建议 | 按错误类型给一句话（如"请填写 1–32 个字符"） |

### 4.3.3 错误明细下载（I2-05）

- 文件名：`{biz_name}_导入失败明细_{yyyyMMddHHmmss}.xlsx`
- 内容：**Sheet 1「失败明细」** = `行号` + 该行业务的全部列（原值原样）+ `失败原因`；**Sheet 2「填写须知」** 复用模板的须知
- **关键体验**：用户在这个文件里**只改错的地方**，然后**直接拿这个文件重新上传**即可（因为保留了全部原始列）。这条不做，用户就要在 5000 行里重新找那 5 行，等于没做（故事 3 的全部价值就在这）
- 明细超过 2000 条时：库只存前 2000 条，**文件里给全量**（文件是异步写的，不受内存限制），页面提示「明细较多（共 8,432 条），已全部写入错误明细文件」

---

## 4.4 入库（I2-06 / I2-07 / I2-08）

### 4.4.1 入库方式：整批单事务，分批提交

**"全量事务"不等于"一行一个 insert 然后整体 commit"**——10 万行一个事务会把事务日志撑爆、锁表时间长。做法：

| 步骤 | 做法 |
|---|---|
| 第 1 段 | **校验通过后**，按 **每批 1000 行** 分批写入，**每批一个子事务** |
| 第 2 段 | 每批成功后记录 `processed_rows`；若任一批失败 → **反向补偿**：把本次任务已写入的行按"任务 id 标记"批量删除（导入行在目标表上打 `import_task_id`，或写一张 `sys_impexp_task_row` 映射表） |
| 第 3 段 | 全部成功 → 任务成功；补偿完成 → 任务失败，文案「导入失败，已回滚」 |

> **给 architect 的选择题（附录 B D-B3）**：① 目标表加 `import_task_id` 列（侵入业务表，但补偿最快）；② 建 `sys_impexp_task_row(task_id, target_table, target_pk)` 映射表（不侵入业务表，但多一张大表）。
> **本文建议 ②**，理由：本批要服务的业务表会越来越多（ERP 物料、MDM 物料、内容管理文章…），给每张业务表加一列是不收敛的做法。

### 4.4.2 重复策略（D5，三选一，按导入定义配置）

| 策略 | 行为 | 界面提示 | 适用 |
|---|---|---|---|
| **跳过（默认）** | 库里已存在的记录**不覆盖、不报错**，计入 `skip_rows` | 「其中 12 行因编码已存在被跳过」 | 绝大多数场景（新增为主） |
| **覆盖** | 用文件里的值**更新**已存在记录（只更新模板里有的字段，不动其它字段） | 二次确认弹框：「将覆盖 12 条已存在的记录，覆盖后原值不可恢复（可在审计日志中查询变更前后值）。是否继续？」+ 需输入「覆盖」二字确认 | 主数据批量修订、价格调整 |
| **报错** | 已存在记录视为失败行，进失败明细 | 「编码已存在（库中已有该记录）」 | 只允许新增、绝不允许改价的场景 |

**覆盖必须留痕**：`overwrite` 的每一行都写 `audit_logs`（`before_data` / `after_data` 只放**变更的字段**，不放整行，避免日志爆炸）。10 万行覆盖 = 10 万条审计？**不**——改为**按任务汇总一条审计**：`{task_id, biz_code, overwrite_count: 12, changed_fields: ["price","spec"]}`，明细放任务详情页可查。

### 4.4.3 部分导入开关（D6）

- `allow_partial=false`（默认）：有任何失败行 → **整批不入库**，任务状态 `失败`，文案见 E-IMP-19。
- `allow_partial=true`：只入库成功行，任务状态 `部分成功`，报告写「成功 4,995 / 失败 5（已跳过失败行）」。
- **失败明细文件仍然生成**，且任务详情里给「用明细文件重新导入」按钮。

---

## 4.5 大文件（10 万行+）：进度与超时（I2-09）

### 4.5.1 阈值与降级

| 行数 | 处理方式 | 估时 |
|---|---|---|
| ≤ 20,000 行 | **同步**：一个请求内完成解析 + 校验，直接返回报告 | ≤ 2 秒 |
| 20,001 – 100,000 行 | **异步**：创建任务 → 后台校验 → 站内信通知 → 回向导第 2 步 | 5–30 秒 |
| > 100,000 行 | **拒绝**（E-IMP-06），要求拆分 | — |

### 4.5.2 耗时算例（10 万行 × 12 列，作为性能基线）

| 阶段 | 计算 | 估时 |
|---|---|---|
| 上传 xlsx（约 18MB） | 内网 20MB/s | 0.9 s |
| calamine 解析 | 实测吞吐约 2.5 万行/秒 → 100,000 ÷ 25,000 | 4.0 s |
| 行级业务校验 | 12 列 × 10 万 = 120 万次判定，约 400 万次/秒 | 3.0 s |
| 文件内查重 | HashMap 插入 10 万个键 | 0.3 s |
| 库内查重 | 按 1000 行/批查 `WHERE code = ANY(...)`，100 批 × 60ms | 6.0 s |
| 外键校验 | 1 次全量拉取字典（通常 < 5000 行） | 0.1 s |
| **合计（校验）** | | **≈ 14.3 s** |
| 入库（若通过） | 100 批 × 100ms（每批 1000 行 insert） | 10.0 s |
| **端到端** | | **≈ 25 s** |

> **结论**：10 万行在 25 秒量级，所以**校验必须异步**（前端 15s 硬超时，F27），但**不需要分片上传文件**（18MB 一次传完就行）。这两件事不要混在一起做。

### 4.5.3 进度怎么算（不能伪造进度条）

- 任务表记 `total_rows` / `processed_rows` / `percent`
- 解析阶段：`calamine` 是流式迭代行的，每读 1000 行更新一次 `processed_rows`，此阶段占进度 **0–40%**
- 校验阶段：每校验 1000 行更新一次，占 **40–80%**
- 入库阶段：每批 1000 行更新一次，占 **80–100%**
- 前端每 **3 秒**轮询一次（只在有进行中任务时轮询，任务终结即停）
- **界面文案**要区分阶段：`正在解析文件（32%）` / `正在校验数据（65%）` / `正在写入（88%）` —— 三个阶段的进度含义不同，混成一个"进度 65%"用户不知道在等什么

### 4.5.4 超时与失败

| 情况 | 处理 |
|---|---|
| 解析/校验超 **10 分钟** | 任务置 `失败`，原因「处理超时（超过 10 分钟）」；建议用户拆分 |
| 后端重启导致任务中断 | 启动时扫 `status in (校验中, 入库中) AND updated_at < now()-10min` 的任务，置 `失败`（原因「服务重启导致中断，请重试」），**已经入库的按 4.4.1 第 2 段补偿** |
| 用户关页面 | 任务继续跑，完成后站内信通知（这正是异步的价值） |

---

## 4.6 导出（I3）

### 4.6.1 同步导出（I3-01）

| 项 | 定法 |
|---|---|
| 触发 | 业务列表页/查询页的「导出」按钮 |
| 行数判定 | 后端先 `COUNT(*)`；> `impexp.export.sync_max_rows`（默认 **10000**）→ 转异步（E-EXP-01） |
| 前端超时 | 该请求**单独设 `timeout: 180000`**（F27 全局是 15s，必须覆盖），见 8.5 的前端说明 |
| 后端守护 | 超过 **60 秒**未完成 → 不再继续同步返回，而是**自动落成异步任务**并返回 `{ mode: "async", task_id }`，前端同时弹「数据量较大，已转为后台导出，完成后通知您」 |
| 文件名 | `{biz_name}_{yyyyMMddHHmmss}.xlsx`，如 `采购订单_20261028143012.xlsx` |
| 内容 | Sheet 1 = 表头（加粗冻结）+ 数据行；列顺序 = 用户在字段勾选里的顺序 |
| 单元格格式 | **全文本导出**（避免长数字被 Excel 转科学计数法：订单号 `202610280001234567` 变 `2.0261E+17` 是经典事故）。金额列例外：数字格式 `#,##0.00`；日期列 `yyyy-mm-dd` |

> **"全文本导出"的代价与取舍**：用户在 Excel 里没法直接对金额求和。所以金额/数量列仍按数字写，**但订单号、编码类列必须强制文本**（写入时以 `format="@"` 指定）。这条要写进验收。

### 4.6.2 异步导出（I3-02）

**流程**：
1. 用户点导出 → 勾字段 → 确认 → 后端 `COUNT(*)` 超阈值 → 建任务（`status=0 排队`），**立即返回 task_id**
2. 每分钟的调度器扫描（4.7.3）→ 取 1 个排队任务 → `status=1 执行中`
3. 执行：**分批查询**（每批 5000 行，按主键游标翻页，不用 `OFFSET`，避免深分页越翻越慢）→ 写入 xlsx → 每批更新 `processed_rows` 与 `percent`
4. 完成后：产物落默认存储（`category='export'`）→ `status=2 成功` → `result_expires_at = now + 24h` → **发站内信**「您的导出任务已完成，点击下载」
5. 失败：`status=3 失败` + `error_msg`（用 E-EXP-07/08 的文案）

**耗时算例（20 万行订单，15 列，4 表关联）**

| 阶段 | 计算 | 估时 |
|---|---|---|
| 计数 `COUNT(*)` | 走时间索引 | 0.2 s |
| 分批查询 | 40 批 × 5000 行，每批含 4 表 join ≈ 1.5–3 s | 60–120 s |
| 写 xlsx | 20 万行 × 15 列 = 300 万单元格，rust_xlsxwriter 约 30 万单元格/秒 | 10 s |
| 落存储 | 18.7MB 写入默认后端 | 1 s |
| **合计** | | **≈ 1.5–2.5 分钟** |

> 与故事 4 的"4 分 12 秒"对照：真实环境因订单明细表数据量大（明细行数比订单头多 3~5 倍），实测会落在 2–5 分钟区间，符合预期。**验收不看绝对秒数，看"进度条是否单调递增、是否最终 100%、通知是否到达"**（附录 B D-B5）。

### 4.6.3 导出字段勾选（I3-03）

- 弹框里按 `field_schema` 列出全部可导字段，分「基本信息 / 明细信息」两组
- 默认勾选：`export_default=true` 的字段（一般为编码、名称、日期、金额等 8~12 个）
- 用户选项**记在浏览器本地**（`localStorage`，按 `biz_code` 分键），下次打开自动回填
- 敏感字段（`sensitive=true`）旁边有 `t-tag theme="warning"` 标「敏感」，并有 tooltip「导出后默认脱敏（如 138****5678）；需要明文请联系管理员申请权限」
- 至少选 1 个字段，否则 E-EXP-02

### 4.6.4 导出脱敏（D10 / I3-04）

| 字段类型 | 无 `export:sensitive` 权限 | 有权限 |
|---|---|---|
| 手机号 | `138****5678`（`Mask::mobile`） | 明文 |
| 邮箱 | `a***@b.com`（`Mask::email`） | 明文 |
| 身份证 | `3201****1234`（`Mask::id_card`） | 明文 |
| 银行卡 | `6222****7890`（`Mask::bank_card`） | 明文 |
| 姓名 | **不脱敏**（姓名是业务必需，脱敏后报表没法用） | 明文 |
| 地址 | `北京市朝****`（`Mask::address`） | 明文 |

**明文导出的三条约束**：
1. 必须有 `business:impexp:export:sensitive` 权限码（后端校验，前端隐藏选项）
2. 每次明文导出写一条审计：`{biz_code, row_count, sensitive_fields, file_name}`——**不写具体值**
3. 明文导出的文件名自动加后缀 `_含敏感字段`，让人一眼知道这份文件不能乱发

> 脱敏工具直接用 `alun_utils::Mask`（F22 同源），**不要自己写正则**——自己写的脱敏是最容易出"188****1234"这种位数不对的低级 bug 的地方。

### 4.6.5 下载链接有效期（I3-05）

- 产物存在默认后端，**不直接暴露存储路径**
- 下载走 `GET /business/impexp/task/{id}/download` → 后端按 3.6 的规则生成限时链接：
  - `minio` 后端：302 重定向到预签名 URL（浏览器直连对象存储，**不占后端带宽**）
  - `local` 后端：302 到自签短链（3.6.2）
- 有效期默认 **24 小时**，`sys_config: impexp.export.link_ttl_hours`
- 过期界面：见 E-EXP-05，给「重新导出」按钮（复用原任务参数一键重跑，属 P1 I4-04；P0 至少给「返回列表」+ 提示）

---

## 4.7 异步任务（D11）

### 4.7.1 任务表 `sys_impexp_task`（导入导出共用一张，用 `task_kind` 区分）

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | UUID | 主键（也是前端轮询的 task_id） |
| `task_kind` | VARCHAR(10) | `import` / `export` |
| `definition_id` | UUID | 指向 `sys_impexp_definition` |
| `biz_code` / `biz_name` | VARCHAR | 冗余存储，定义被改也不影响历史任务显示 |
| `status` | SMALLINT | 见 4.7.2 状态码 |
| `stage` | VARCHAR(20) | `parse` / `validate` / `write` / `render`，用于界面显示阶段文案 |
| `total_rows` | BIGINT | 总行数（解析完成前为 0） |
| `processed_rows` | BIGINT | 已处理行数 |
| `success_rows` / `fail_rows` / `skip_rows` | BIGINT | 结果统计 |
| `percent` | SMALLINT | 0–100，后端算好给前端，**不让前端自己算** |
| `source_file_id` | UUID | 导入源文件（`sys_file.id`） |
| `result_file_id` | UUID | 导出产物（`sys_file.id`） |
| `error_file_id` | UUID | 失败明细文件（`sys_file.id`） |
| `error_msg` | VARCHAR(500) | 失败原因（对用户的话术，不是堆栈） |
| `params` | JSONB | 导出筛选条件 / 导入选项（重复策略、是否部分导入），用于"重试"时复原 |
| `dup_strategy` | VARCHAR(10) | 本次实际使用的重复策略 |
| `result_expires_at` | TIMESTAMPTZ | 产物过期时间 |
| `duration_ms` | BIGINT | 耗时（完成后写） |
| `created_by` / `created_at` / `started_at` / `finished_at` / `deleted_at` | — | 常规 |

**索引**：`(created_by, created_at DESC)`、`(status, created_at)`（调度器扫排队任务用）、`(result_expires_at) WHERE status = 2`（清理用）

### 4.7.2 两个状态机

**导入任务**

| 码 | 状态 | 说明 | 可取消 |
|---|---|---|---|
| 0 | 待校验 | 刚创建（异步校验） | ✅ |
| 1 | 校验中 | 解析 + 校验 | ✅ |
| 2 | **待确认** | 校验完成，等用户点"确认导入"（**这是唯一需要人参与的中间态**） | ✅ |
| 3 | 入库中 | 正在写库 | ❌ |
| 4 | 成功 | 全部成功 | — |
| 5 | 部分成功 | 部分导入模式下有失败行 | — |
| 6 | 失败 | 校验失败/入库失败/超时 | — |
| 7 | 已取消 | 用户主动取消 | — |

流转：`0 → 1 →（失败）6` / `0 → 1 → 2 →（确认）3 → 4 | 5 | 6`；任意 0/1/2 → `7`

**导出任务**

| 码 | 状态 | 说明 | 可取消 |
|---|---|---|---|
| 0 | 排队中 | 等调度器取 | ✅ |
| 1 | 执行中 | 查询 + 写文件 | ✅ |
| 2 | 成功 | 产物可用，链接 24h | — |
| 3 | 失败 | 超时/空间不足/异常 | — |
| 4 | 已取消 | 用户主动取消，产物已删 | — |
| 5 | 已过期 | 产物被清理（仅展示用，DB 里仍为 2 + `result_expires_at` 已过） | — |

> **状态 5 不入库**：界面判断"`status=2` 且 `result_expires_at < now()`"就显示"已过期"，避免一个定时任务去改历史行状态。

### 4.7.3 调度器（每分钟）

新增一个 cron 任务（`invoke_target = impexp_dispatch`，`0 * * * * *` 每分钟）：

| 步骤 | 动作 |
|---|---|
| 1 | 取 `task_kind=export AND status=0` 的任务，**按创建时间升序取 1 个**（单实例并发 1，避免大导出把 DB 打满） |
| 2 | 置 `status=1, started_at=now()`，开始执行 |
| 3 | 执行完成/失败，写状态与 `duration_ms`，成功则发站内信 |
| 4 | 扫 `status in (0,1) AND updated_at < now() - 10min` 的"僵尸任务"，置失败（服务重启场景） |
| 5 | 同一 tick 内顺带执行清理（产物过期、直传中残留），见 4.8.2 |

**并发控制**：同一用户同时进行中的导出任务上限 **3** 个（E-EXP-09）；全局同时执行 1 个（P0），P1 可配到 2–4。

---

## 4.8 任务列表与清理（I3-06 / I4-01 / I4-05）

### 4.8.1 列表（导入与导出两个 tab，列不同）

**导出任务 tab 列**：任务名 / 业务 / 行数 / 状态（含进度条） / 创建人 / 耗时 / 创建时间 / 操作（下载·取消·删除）

**进度条的三种视觉**（TDesign `t-progress`）：
- 排队中：灰色 + 文案「排队中」
- 执行中：`theme="primary"` + 百分比 + 阶段文案（`正在查询（45%）`）
- 成功：绿色满格 + 「已完成 · 1 分 42 秒」
- 失败：红色 + 「失败」+ hover 显示原因全文

**导入记录 tab 列**：文件名 / 业务 / 总行数 / 成功 / 失败 / 重复跳过 / 状态 / 操作人 / 耗时 / 时间 / 操作（查看报告·下载明细·删除）

> **"删除任务"要不要单独授权？** 要（任务书要求）。`business:impexp:task:delete` 独立权限码，且**只能删自己的任务**；有 `view-all` 的人能看别人的，但**不能删别人的**（防"删掉证据"）。

### 4.8.2 清理（I4-05 / D8）

| 对象 | 保留期 | 清理动作 | 时间 |
|---|---|---|---|
| 导出产物文件 | **7 天** | 删对象 + `result_file_id` 置空（任务记录保留） | 每日 03:50 |
| 导入错误明细文件 | **30 天** | 同上 | 每日 03:50 |
| 导入源文件 | **30 天** | 同上 | 每日 03:50 |
| 任务记录（含成功/失败） | **180 天** | 软删 | 每日 03:50 |
| 直传中残留记录 | **2 小时** | 删对象 + 软删（3.5.4） | 每小时 |

**安全**：清理前先删对象再更新数据库；对象删除失败**不阻断**数据库更新（否则一条坏对象会让清理任务天天卡住），但记 `warn` 日志并在"存储用量"里体现差异（P1）。
---

# 五、消息通知详细设计

## 5.0 一句话说清本模块在改什么

> **现状**：平台**没有任何消息/通知能力**（F 核查：`backend/src` 下无 notice/message 模块，migrations 里无消息表）。系统里"发生了什么事"只能靠人自己去看。
> **本批**：建一条**站内信主干道**——投递、收件箱、未读数、模板、群发、保留策略。同时把"业务系统怎么发通知"的**调用契约**定下来（附录 E），D 批次审批流直接调，不用改本批的表。

---

## 5.1 数据模型（5 张表）

### 5.1.1 `sys_notice_template`（通知模板）

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | UUID | 主键 |
| `template_code` | VARCHAR(50) | 模板编码，如 `approval_todo`、`month_end_reminder`；软删下唯一 |
| `template_name` | VARCHAR(100) | 中文名 |
| `notice_type` | VARCHAR(20) | `system` / `business`（N4-01） |
| `business_key` | VARCHAR(50) | 业务标识，仅 `business` 类型有（如 `erp.purchase_order`），供 D 批次按业务查模板 |
| `title_tpl` | VARCHAR(200) | 标题模板，支持 `{{var}}` |
| `content_tpl` | TEXT | 正文模板，支持 `{{var}}`；**纯文本 + 换行**（P0 不做富文本，理由见附录 D） |
| `variables` | JSONB | 变量声明数组：`[{key, label, sample, required}]`，**白名单来源**（5.7.2） |
| `default_channels` | JSONB | 默认通道：`{"site": true, "email": false, "webhook": false}` |
| `level` | VARCHAR(10) | 重要级别：`info` / `warning` / `urgent`，影响列表左侧色条与图标 |
| `status` | SMALLINT | 1 启用 / 0 停用 |
| 审计字段 | — | 与平台一致 |

### 5.1.2 `sys_notice_batch`（发送批次）

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | UUID | 主键 |
| `batch_no` | VARCHAR(32) | 批次号，如 `NB202610280001`（日期 + 当日序号） |
| `title` | VARCHAR(200) | 本次发送的实际标题（模板渲染后的样本标题） |
| `notice_type` | VARCHAR(20) | 同模板 |
| `template_code` | VARCHAR(50) | 使用模板（可为空=自由填写） |
| `target_type` | VARCHAR(20) | `user` / `role` / `dept` / `all` |
| `target_snapshot` | JSONB | **目标快照**：`{dept_ids:[...], include_children:true, role_ids:[...], user_ids:[...]}`，发送后即不可变（用于审计与追溯"当时发给了谁"） |
| `target_desc` | VARCHAR(255) | 人读描述，如「财务部及下级（3 个部门）」 |
| `planned_count` | INT | 计划人数（展开去重后） |
| `sent_count` | INT | 实际投递成功行数 |
| `fail_count` | INT | 失败行数 |
| `send_mode` | VARCHAR(10) | `now` / `scheduled` |
| `scheduled_at` | TIMESTAMPTZ | 定时发送时间（`now` 时为空） |
| `status` | SMALLINT | 0 待发送 / 1 发送中 / 2 已发送 / 3 部分失败 / 4 已取消 / 5 发送失败 |
| `var_values` | JSONB | 变量取值（非按人变化的公共值，如 `{"deadline":"2026-10-31 18:00"}`） |
| `created_by` / `created_at` / `sent_at` / `deleted_at` | — | 常规 |

### 5.1.3 `sys_notice_message`（消息主体，一次发送一行）

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | UUID | 主键 |
| `batch_id` | UUID | 指向批次 |
| `notice_type` | VARCHAR(20) | `system` / `business` |
| `business_key` | VARCHAR(50) | 业务标识（供按业务跳转、按业务统计） |
| `biz_ref_id` | VARCHAR(64) | 业务单据 ID（如采购订单 id），供"去处理"精确跳转 |
| `title` | VARCHAR(200) | **渲染后的最终标题**（存快照，不存模板——模板改了历史消息不能跟着变） |
| `content` | TEXT | **渲染后的最终正文** |
| `level` | VARCHAR(10) | 重要级别 |
| `action_url` | VARCHAR(500) | 跳转地址（如 `/erp/purchase/po/123`），可为空 |
| `action_text` | VARCHAR(20) | 按钮文案，如「去处理」「查看详情」 |
| `sender_id` | UUID | 发送人（系统发出为空） |
| `sender_name` | VARCHAR(64) | 发送人姓名快照 |
| `is_test` | BOOLEAN | 是否测试发送（24h 清理） |
| `channels` | JSONB | 各通道结果：`{"site":"sent","email":"skipped_not_implemented","webhook":"skipped_not_implemented"}` |
| `created_at` | TIMESTAMPTZ | 创建时间 |

### 5.1.4 `sys_notice_receiver`（收件人 × 消息 —— **已读状态各自独立的关键**）

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | UUID | 主键 |
| `message_id` | UUID | 指向消息 |
| `receiver_id` | UUID | 收件人 `sys_user.id` |
| `read_at` | TIMESTAMPTZ | 读时间；`NULL` = 未读 |
| `deleted_at` | TIMESTAMPTZ | 收件人删除（**只影响自己**，不影响别人） |
| `created_at` | TIMESTAMPTZ | 投递时间 |

**索引（决定未读数性能，必须建）**
- `uk_sys_notice_receiver`：`(message_id, receiver_id) WHERE deleted_at IS NULL` —— 防重投
- `idx_sys_notice_receiver_unread`：`(receiver_id, read_at) WHERE read_at IS NULL AND deleted_at IS NULL` —— **未读数查询走这个部分索引**
- `idx_sys_notice_receiver_list`：`(receiver_id, created_at DESC)` —— 收件箱列表

> **为什么"一人一行"而不是"一条消息一行 + 已读人数组"**：数组方案在"10 万人全员通知"下会变成 10 万元素的 JSONB 更新，每次读都要反序列化整个数组；一人一行是 120 行（故事 6 的规模）或 10 万行（全员），插入用一条多值 INSERT 即可，未读数走部分索引是 O(log n)。**这是消息系统最经典的一次建模选择，选错后期改不动**。

### 5.1.5 `sys_notice_channel`（通道配置）

| 字段 | 类型 | 说明 |
|---|---|---|
| `id` | UUID | 主键 |
| `channel` | VARCHAR(20) | `site` / `email` / `webhook`；软删下唯一 |
| `enabled` | BOOLEAN | 开关 |
| `config` | JSONB | 配置；**含密钥的字段用 AES-256-GCM 加密后存**（F22，同 AK/SK 规则） |
| `implemented` | BOOLEAN | 系统字段，**不可由用户改**：`site=true`，`email/webhook=false` |
| `remark` | VARCHAR(255) | — |

> `implemented` 这个字段是本批"只留配置不实现发送"的**诚实落地**：页面上 email/webhook 的开关可以打开，但列表里常驻一行灰字「当前版本仅记录配置，尚未实现发送」，避免运维以为配了就能用。

---

## 5.2 消息中心（N1）

### 5.2.1 收件箱查询（N1-01）

- 接口：`GET /business/notice/message/page?tab=unread|read|all&notice_type=&page=&page_size=`
- **永远只查自己的**（`receiver_id = 当前用户`），**没有"看别人消息"的接口**——连管理员也没有。这是产品红线，不是权限问题。
- 三个 tab 的语义：
  - **未读**：`read_at IS NULL AND deleted_at IS NULL`
  - **已读**：`read_at IS NOT NULL AND deleted_at IS NULL`
  - **全部**：`deleted_at IS NULL`
- 排序：`created_at DESC`（最新在前）
- 分页：默认每页 10 条（F26 的 `DEFAULT_PAGE_SIZE`），收件箱用 `t-table` 或自定义卡片列表——**建议自定义卡片列表**（消息中心不是数据表格，用 `t-table` 会让"标题 + 摘要"两行文字挤在一格里）

### 5.2.2 消息详情（N1-02）

双栏布局（桌面端）：**左列表（380px 固定）/ 右详情（自适应）**。

右侧详情显示：
| 区域 | 内容 |
|---|---|
| 顶部 | 类型标签（`系统通知` 灰蓝 / `业务通知` 主色）+ 级别色条（`urgent` 红色左边框）+ 标题（18px 加粗） |
| 元信息 | 「发送人：王主管（财务部） · 2026-10-28 14:32」或「系统通知 · 2026-11-01 09:00」 |
| 正文 | 等宽不改、保留换行（`white-space: pre-wrap`） |
| 底部 | 若有 `action_url`：主按钮「{action_text}」；否则无按钮 |
| 右下角 | 「删除」文字链 |

**空态**：右栏显示插画 + 「从左侧选择一条消息查看详情」

### 5.2.3 已读、全部已读、删除（N1-03 / N1-04 / N1-05）

| 操作 | 交互 | 接口行为 |
|---|---|---|
| 点开消息 | 立即置已读，**未读数本地先减 1**，左侧列表该行文字由加粗变常规 | `PUT /business/notice/message/{id}/read`（幂等：已读再调不报错、不改 `read_at`） |
| 单条「标为已读」 | 列表行 hover 出现 | 同上 |
| 「全部已读」 | 二次确认：「将把 **37** 条未读消息标记为已读？」 | `PUT /business/notice/message/read-all`，返回实际影响条数（`37`），少于预期时以返回值为准刷新 |
| 删除 | 单条：hover 出现「删除」，点了直接删（不加确认，因为可理解、影响小）；批量：选中后确认「删除选中的 5 条消息？」 | `DELETE /business/notice/message/{id}`、`DELETE /business/notice/message/batch`（`ids` 数组，上限 100） |

**已读/删除都是"只影响自己"**：`sys_notice_receiver` 单行更新，不碰 `sys_notice_message`。所以故事 6 里"A 读了 B 还是未读"天然成立。

---

## 5.3 未读角标（N2，D7）

### 5.3.1 位置与样式

- 位置：顶栏**主题切换按钮与用户头像之间**（F/`BasicLayout.vue:43-58` 之间的位置），`t-button variant="text" shape="square"`
- 图标：**`notification`**（**不是 `bell`，F30 已证 `bell` 不存在**）
- 角标：`t-badge :count="unread" :max-count="99"`，即 >99 显示 `99+`；`0` 时不显示角标
- 悬浮：`t-tooltip` 显示「消息中心（3 条未读）」
- 点击：跳 `/platform/message`

**下拉快捷面板（P0，值得做）**：点铃铛不直接跳页，而是先弹一个 `t-popup` 面板显示最近 **5** 条未读（标题 + 相对时间），底部两个按钮「查看全部」「全部已读」。理由：绝大多数时候用户只需要"知道是什么事"，不必跳到整页。

### 5.3.2 刷新策略（三个触发点，缺一个用户就会觉得"不准"）

| # | 触发 | 时机 | 说明 |
|---|---|---|---|
| 1 | **轮询** | 每 **60 秒** | 仅当页面可见（`document.visibilityState === 'visible'`）时轮询，切到后台就停——省 60% 无效请求 |
| 2 | **焦点恢复** | 窗口 `focus` / `visibilitychange` 变可见 | 用户切回浏览器立刻刷，避免"明明有 3 条却显示 1 条" |
| 3 | **动作后主动刷** | 读完/删完/发完后 | 本端立即刷；群发后也刷（自己若在范围内） |

**接口**：`GET /business/notice/message/unread-count` → `{ "total": 3, "by_type": {"system": 1, "business": 2} }`

**性能账**（D7 的依据）：120 人 × 每分钟 1 次 = **2 QPS**；查询走 `idx_sys_notice_receiver_unread` 部分索引，单次 < 2ms。相比"每次打开页面查全表"，这个量对 DB 完全可以忽略。等通知量到"每秒数十条推送"（D 批次审批流的量级）时再上 SSE（F25 已证可行，P1）。

### 5.3.3 轮询的边界处理

- 轮询失败（网络抖动）：**不弹错误**，静默重试下一轮（用户不需要知道"未读数没刷上"）
- 连续 3 次失败：铃铛图标加一个小红点降级样式，tooltip 改「消息状态暂不可用」
- **不在轮询里拉消息列表**（只拉数字），列表在打开面板时按需拉取

---

## 5.4 发送（N3）—— 四种目标与扇出算法

### 5.4.1 四种目标

| 目标类型 | 选择控件 | 关键规则 |
|---|---|---|
| 指定用户 | 人员选择器（多选，支持姓名/工号搜索） | 上限 **500** 人（防止"选了 5000 人"变成误操作群发）；超出提示「指定用户发送上限 500 人，如需更大范围请选"部门"或"全员"」 |
| 角色 | `t-select` 多选角色 | 展开为"持有该角色且 status=1 的用户" |
| **部门（含下级）** | 部门树（`t-tree`，多选） + 「包含下级部门」开关（**默认关闭**） | 复用 `find_dept_subtree_ids()`（F10 现成能力） |
| 全员 | 单选按钮 | 展开为 `sys_user where status=1 and deleted_at is null`；发送前**必须**二次确认并显示人数 |

### 5.4.2 扇出算法（伪流程，供开发照做）

```
输入：target_type, target_snapshot, include_children

1. 收集用户 ID 集合 U（有序 Set，天然去重）
   1.1 user  → U = snapshot.user_ids（过滤：用户存在、status=1、未软删）
   1.2 role  → 对每个 role_id，查 sys_user_role 得 uid 集合，并入 U
   1.3 dept  → 对每个 dept_id：
                include_children=true  → ids = find_dept_subtree_ids(dept_id)   ← F10 复用
                include_children=false → ids = [dept_id]
                查 sys_user where dept_id in ids，并入 U
   1.4 all   → 查全部 status=1 用户，并入 U

2. 二次过滤（**必须做，否则会给停用账号发消息**）
   U = { u ∈ U | u.status = 1 且 u.deleted_at IS NULL }

3. 计算人数
   planned_count = |U|
   同时在预览里给出"展开明细"：
     财务部直属 34 人 + 下级 3 个部门 86 人 = 120 人（去重 2 人）
     （去重数 = 各来源简单相加 - 最终 |U|）

4. 渲染
   公共变量（如 deadline）取 var_values；
   按人变量（如 userName / userRealName）在投递时逐人渲染

5. 投递（事务内）
   5.1 插 1 行 sys_notice_batch（status=1 发送中）
   5.2 插 1 行 sys_notice_message（标题/正文存**渲染样本**；按人变量在 receiver 行里不再存，
       改为：若正文含按人变量，则**每条 message 按"变量组合"分组合并**——
       P0 简化：**按人变量的消息为每人生成 1 条 message**（120 人 = 120 条 message + 120 条 receiver）
   5.3 批量插 receiver：一条 INSERT 多值（120 行 ≈ 20ms）
   5.4 更新 batch：sent_count / status=2

6. 结果
   返回 {batch_id, sent_count, fail_count}
```

> **关于"按人变量"的取舍（重要）**：`{{userName}}` 这种变量在 120 人通知里会导致 120 条 message。若强行"一条 message + 变量数组"，未读/已读模型会变形。**P0 选择"每人一条 message"**，代价是全员 1 万人通知产生 1 万条 message（约 3MB 正文），可接受；**P1 优化**为"按变量组合分组"（同一组合共用一条 message），接口不变。

### 5.4.3 120 人的具体校验点（对应第九章场景 5）

| 校验点 | 期望 |
|---|---|
| 财务部直属人数 | 34（示例值，以实际数据为准） |
| 下级 3 个部门合计 | 86 |
| 去重人数 | 2（某人同时在主部门与下级部门兼职） |
| 最终 `planned_count` | **120** |
| `sys_notice_receiver` 新增行数 | **120** |
| 每人未读数 | 各自 +1 |
| A 读完后 | A=已读，B 未读（互不影响） |
| 发送耗时 | ≤ 3 秒（120 行插入 + 索引更新） |

### 5.4.4 定时发送（N3-03）

- 表单选「定时发送」+ 日期时间选择器（精确到分钟，**只能选未来时间**，最早 = 当前 + 5 分钟）
- 建批次 `status=0 待发送`，`scheduled_at` 落库
- **调度器**：每分钟扫 `status=0 AND scheduled_at <= now()` → 置 `1` → 走 5.4.2 的投递 → 置 `2`
- 列表显示「待发送（还有 2 天 15 小时）」；`status=0` 时可点「取消」（N3-05 P1，但**P0 至少要能在创建后 5 分钟内撤销**——否则误设定时就只能干等）
- **错过时间窗口**（服务停机 3 小时）：重启后调度器发现 `scheduled_at` 已过 → **立即补发**，并在批次备注写「原定 2026-11-02 09:00，实际 2026-11-02 12:03 补发」。不做静默丢弃。

### 5.4.5 发送记录（N3-04）

列：批次号 / 标题 / 目标描述 / 预计人数 / 实发 / 失败 / 状态 / 发送方式（立即/定时）/ 发送时间 / 操作人 / 操作（查看明细·取消·重新发送）

「查看明细」抽屉：目标快照描述 + 人数统计 + **失败名单**（若有）+ 每人已读状态统计（已读 42 / 未读 78）——注意这里给的是**聚合数**，不给"谁读了谁没读"的名单（除非另有需求，附录 D 明确不做"已读回执明细"）。

---

## 5.5 通知类型（N4-01）

| 类型 | `notice_type` | 谁发 | 典型场景 | 在收件箱的标识 |
|---|---|---|---|---|
| **系统通知** | `system` | 通知管理员（手动） | 升级公告、制度发布、节假日安排 | 灰蓝标签「系统通知」，图标 `notification` |
| **业务通知** | `business` | 业务模块调用接口（附录 E）或通知管理员代发 | 待审批提醒、单据被驳回、任务完成（导出完成也是这类） | 主色标签「业务通知」，图标 `task` |

**类型不是"装饰"，它决定三件事**：
1. 收件箱筛选（N1-06）；
2. 业务通知**必须**带 `business_key`，用于"同类通知去重/覆盖"（见附录 E 的幂等键）；
3. 系统通知**不允许**带 `action_url` 以外的业务跳转（避免系统公告把用户带到某个业务单据）。

---

## 5.6 模板与变量（N4-02 / N4-03 / N4-04）

### 5.6.1 变量语法与渲染

- 占位符语法：**`{{变量名}}`**（双花括号，变量名 `[a-zA-Z][a-zA-Z0-9_]{0,31}`）
- 渲染规则（四条，必须一致）：
  1. 变量存在 → 替换为值（字符串原样，不做 HTML 转义因为正文是纯文本）
  2. 变量**缺失或为空** → 替换为 **`—`**（长破折号），**绝不**把 `{{userName}}` 原样显示给用户
  3. 变量名非法（不在白名单）→ **保存时就报错**，不等到发送才炸：「变量 `{{userNmae}}` 未在变量清单中定义（是否想用 `{{userName}}`？）」
  4. 渲染结果长度校验：标题 ≤200 字符、正文 ≤5000 字符，超了在预览时就红字提示

**算例（第九章场景 6）**：
- 模板正文：`您好 {{userName}}，单据 {{docNo}} 已通过`
- 变量取值：`userName=张三`、`docNo=CG-202610-0001`
- 渲染结果：**`您好 张三，单据 CG-202610-0001 已通过`**
- 若 `docNo` 缺失：**`您好 张三，单据 — 已通过`**
- 若模板写成 `{{ userName }}`（含空格）→ 保存时报「变量名不能含空格」

### 5.6.2 变量白名单（P0 固定 12 个）

| 变量 | 中文名 | 来源 | 说明 |
|---|---|---|---|
| `{{userName}}` | 收件人姓名 | 投递时按 `receiver_id` 查 `sys_user.real_name` | 按人不同 |
| `{{userAccount}}` | 收件人账号 | `sys_user.username` | 按人不同 |
| `{{deptName}}` | 收件人部门 | `sys_dept.dept_name` | 按人不同 |
| `{{now}}` | 当前时间 | 投递时刻 | 格式 `YYYY-MM-DD HH:mm` |
| `{{today}}` | 今天日期 | 投递时刻 | 格式 `YYYY-MM-DD` |
| `{{docNo}}` | 单据编号 | 调用方传入 | 业务通知用 |
| `{{docType}}` | 单据类型 | 调用方传入 | 如「采购订单」 |
| `{{amount}}` | 金额 | 调用方传入 | 调用方负责格式化（如 `1,234.00`） |
| `{{deadline}}` | 截止时间 | 调用方传入 | — |
| `{{reason}}` | 原因/意见 | 调用方传入 | 如「价格超标」 |
| `{{senderName}}` | 发送人 | 当前登录用户 | 系统发为空 |
| `{{orgName}}` | 公司名 | `sys_config: sys.org.name` | 全局配置 |

> **为什么要有白名单**：模板变量本质是"从数据里取字段拼字符串"，没有白名单就等于给了一个模板注入面（`{{user.password}}` 这种）。P0 只允许这 12 个，**业务要新变量必须走代码登记**（附录 E 的 `business` 契约里 `vars` 只接受字符串 KV，不接受路径表达式）。

### 5.6.3 模板编辑页（详见 8.7）

- 左：标题输入 + 正文 `t-textarea`（最少 8 行高，自适应）
- 右：**变量面板**（12 个变量，点击插入到光标处）+ **实时预览**（用示例值填充，如 `张三`/`CG-202610-0001`）
- 底部：`保存` / `预览` / `测试发送`（N4-04）

### 5.6.4 测试发送（N4-04）

- 只能发给自己（不做"发给别人看看效果"——那会变成绕过群发权限的旁路）
- 走完整渲染与投递流程，但 `sys_notice_message.is_test = true`
- 标题自动加前缀 **`[测试]`**
- 测试消息 **24 小时**后由清理任务删除（不污染收件箱）
- 界面上测试消息有 `t-tag` 标「测试」

---

## 5.7 多通道预留（N5-01 / N5-02）

### 5.7.1 三个通道的现状与边界

| 通道 | `implemented` | 本批行为 | 页面表现 |
|---|---|---|---|
| **站内信 `site`** | `true` | **完整实现**：写 message + receiver，未读/已读/删除全链路 | 唯一不可关闭的通道（开关置灰 + tooltip「站内信为系统基础通道，不可关闭」） |
| **邮件 `email`** | `false` | **只存配置与开关，不发邮件** | 可填 SMTP 主机/端口/发件人/账号/密码（密码加密存）；开关可开；列表有灰字提示「尚未实现发送，配置将在下版本生效」 |
| **Webhook `webhook`** | `false` | **只存配置与开关，不发请求** | 可填 URL / 密钥 / 超时 / 重试次数；同上提示 |

### 5.7.2 "预留"要做到什么程度才算合格

不是"页面上放个开关就叫预留"。本批必须做到：

1. **表结构一次做对**：`sys_notice_message.channels` 里**记录每个通道的处理结果**。站内信发出去时，email/webhook 记 `"skipped_not_implemented"`。这样下批实现时，**历史消息能区分"当时没实现"和"当时发了但失败"**。
2. **发送链路留好扩展点**：投递逻辑按 `channels` 配置遍历，`site` 分支实现，另两个分支只写"跳过"记录。将来加实现=补一个分支，**不改表、不改调用方**。
3. **配置加密一致**：SMTP 密码、Webhook 密钥都用 F22 的 AES-256-GCM，格式 `enc:v1:...`，与 AK/SK 同一套（3.7.1）。
4. **界面诚实**：每一处都写清"尚未实现"，**不允许**出现"配置成功=以为能发"的误导（这是运维投诉的高发点）。

---

## 5.8 保留与自动清理（N5-03，D8）

| 对象 | 保留期 | 清理时间 | 说明 |
|---|---|---|---|
| **未读消息** | **永不清理** | — | 未读是待办，删了就是丢工作 |
| **已读消息**（`sys_notice_receiver.read_at` 非空） | **180 天** | 每日 03:50 | 按 `read_at` 计算，不按发送时间 |
| **测试消息**（`is_test=true`） | **24 小时** | 每日 03:50 | 不管读没读 |
| 已删除的 receiver 行（`deleted_at` 非空） | **30 天** | 每日 03:50 | 物理删除，回收空间 |
| 消息主体（`sys_notice_message`） | 当**所有 receiver 行都被清理** | 每日 03:50 | 孤儿消息物理删除 |
| 发送批次（`sys_notice_batch`） | **2 年** | 每日 03:50 | 审计价值高，保留久一点 |

**清理任务**：`invoke_target = notice_cleanup`，cron `0 50 3 * * *?`（每日 03:50，与现有 5 个日志清理任务 03:00–03:40 错峰，F29）。

**可配置**（`sys_config`，`category='通知设置'`）：

| 键 | 默认 | 说明 |
|---|---|---|
| `notice.retention.read_days` | 180 | 已读保留天数 |
| `notice.retention.test_hours` | 24 | 测试消息保留小时 |
| `notice.retention.deleted_days` | 30 | 已删行物理清理天数 |
| `notice.unread_poll_seconds` | 60 | 前端轮询间隔（下发给前端，改配置即生效） |

> **清理必须分批**：每批 1000 行 `DELETE ... WHERE id IN (SELECT ... LIMIT 1000)` 循环，直到影响行数为 0。**禁止**一条 `DELETE` 扫 200 万行——那会长时间锁表并把 WAL 撑爆（这与现有日志清理任务的写法一致）。
---

# 六、权限模型

## 6.0 三条设计前提

1. **权限码格式固定** `{模块}:{资源}:{动作}`，存 `sys_menu.auth_code`（F1）。本批模块段统一 `business`，因此形如 `business:storage:backend:create`（**四段**——资源本身可能带子资源，这与既有 `support:file:upload` 三段并不冲突：F1 只约定以 `:` 分段，未限制段数；ERP 批次已用 `business:erp:{资源}:{动作}` 先例）。
2. **前端 `hasPermission()` 是全等匹配**（F5）：所以**每一个要判断的动作都必须有自己独立的一行 `auth_code`**，绝不能用 `business:storage:*` 这种前缀写法（它永远返回 false，按钮会永远不显示——这是最容易踩的坑）。
3. **权限点必须落成 `sys_menu` 种子数据**（F6）：`menu_type=3`（按钮）、`is_visible=FALSE`，前端导航不渲染它们，但 `v-privilege` 与角色分配页都靠它们。

---

## 6.1 权限码清单（41 个）

### 6.1.1 对象存储 `business:storage:*`（14 个）

| # | 权限码 | 名称 | menu_type | 敏感 | 说明 |
|---|---|---|---|---|---|
| 1 | `business:storage:backend:list` | 查看存储配置 | 2（菜单） | | 页面级。**列表接口也要这个码** |
| 2 | `business:storage:backend:create` | 新增存储配置 | 3 | ✅ | 含录入 AK/SK |
| 3 | `business:storage:backend:update` | 编辑存储配置 | 3 | ✅ | 不含密钥（见 #6） |
| 4 | `business:storage:backend:delete` | 删除存储配置 | 3 | ✅ | 被引用时后端拒绝 |
| 5 | `business:storage:backend:default` | 设为默认存储 | 3 | ✅ | 影响全局写入路径 |
| 6 | `business:storage:backend:secret` | 更新访问密钥（AK/SK） | 3 | ✅✅ | **单独拆出**：允许改端点名称但不让改密钥 |
| 7 | `business:storage:backend:test` | 存储连通性测试 | 3 | | 无副作用（不写审计） |
| 8 | `business:storage:policy:list` | 查看上传策略 | 2 | | |
| 9 | `business:storage:policy:update` | 编辑上传策略 | 3 | ✅ | 改白名单等于改安全边界 |
| 10 | `business:storage:file:list` | 查看文件列表 | 2（菜单） | | 文件管理页 |
| 11 | `business:storage:file:upload` | 上传文件 | 3 | | 页面传文件、业务表单挂附件都算 |
| 12 | `business:storage:file:delete` | 删除文件 | 3 | ✅ | 含批量删除 |
| 13 | `business:storage:file:update` | 修改文件属性（分类/私有标记） | 3 | | |
| 14 | `business:storage:file:download-private` | 下载私有文件 | 3 | ✅ | 无此码看不到私有文件的下载/预览按钮，且短链接口拒绝 |

### 6.1.2 导入导出 `business:impexp:*`（12 个）

| # | 权限码 | 名称 | menu_type | 敏感 | 说明 |
|---|---|---|---|---|---|
| 15 | `business:impexp:task:list` | 查看任务列表 | 2（菜单） | | 默认只看自己的 |
| 16 | `business:impexp:task:view-all` | 查看全部人的任务 | 3 | ✅ | 数据权限开关（6.2） |
| 17 | `business:impexp:task:delete` | 删除任务 | 3 | ✅ | **只能删自己的**，有 view-all 也不行 |
| 18 | `business:impexp:definition:list` | 查看导入导出定义 | 2 | | 一般随菜单给 |
| 19 | `business:impexp:definition:update` | 维护导入导出定义 | 3 | ✅ | 改字段/必填/判重规则 |
| 20 | `business:impexp:template:download` | 下载导入模板 | 3 | | 无此码不能下载模板，但仍可上传 |
| 21 | `business:impexp:import:create` | 发起导入 | 3 | | 上传 + 校验 |
| 22 | `business:impexp:import:confirm` | 确认入库 | 3 | ✅ | **单独拆出**：校验和写库分离授权，防止"只能看不能改"的人误入库 |
| 23 | `business:impexp:import:cancel` | 取消导入任务 | 3 | | |
| 24 | `business:impexp:export:create` | 发起导出 | 3 | | 同步与异步共用 |
| 25 | `business:impexp:export:sensitive` | 导出敏感字段明文 | 3 | ✅✅ | 无此码强制脱敏（D10） |
| 26 | `business:impexp:error:download` | 下载失败明细 / 导出产物 | 3 | | 产物与明细都算 |

### 6.1.3 消息通知 `business:notice:*`（15 个）

| # | 权限码 | 名称 | menu_type | 敏感 | 说明 |
|---|---|---|---|---|---|
| 27 | `business:notice:message:list` | 查看消息中心 | 2（菜单，隐藏） | | **所有人默认给**（登录就能收通知） |
| 28 | `business:notice:message:read` | 标记已读 | 3 | | 只作用于自己 |
| 29 | `business:notice:message:delete` | 删除消息 | 3 | | 只作用于自己 |
| 30 | `business:notice:send:create` | 发送通知（用户/角色） | 3 | ✅ | 范围受限：≤500 人 |
| 31 | `business:notice:send:group` | 群发通知（部门含下级/全员） | 3 | ✅✅ | **与 #30 分开**：这是"能打扰全公司"的权力 |
| 32 | `business:notice:send:cancel` | 取消定时发送 | 3 | ✅ | |
| 33 | `business:notice:record:list` | 查看发送记录 | 2 | ✅ | 能看到"谁发的、发给谁" |
| 34 | `business:notice:template:list` | 查看通知模板 | 2（菜单） | | |
| 35 | `business:notice:template:create` | 新增模板 | 3 | | |
| 36 | `business:notice:template:update` | 编辑模板 | 3 | | |
| 37 | `business:notice:template:delete` | 删除模板 | 3 | ✅ | 被批次引用时拒绝 |
| 38 | `business:notice:template:test` | 测试发送 | 3 | | 只能发给自己 |
| 39 | `business:notice:channel:list` | 查看通道配置 | 3 | | |
| 40 | `business:notice:channel:update` | 编辑通道配置 | 3 | ✅✅ | 含 SMTP 密码 / Webhook 密钥 |
| 41 | `business:notice:setting:update` | 编辑通知设置（保留期/轮询间隔） | 3 | | |

**统计**：storage 14 + impexp 12 + notice 15 = **41 个权限码**（其中 menu_type=2 的菜单级 8 个、菜单级隐藏 1 个、按钮级 32 个；标记敏感 22 个，其中 ✅✅ 极高敏 4 个：`backend:secret`、`export:sensitive`、`send:group`、`channel:update`）。

---

## 6.2 数据权限（3 层，D9）

> **前提**：`sys_role` 没有 `data_scope` 字段（F9），所以数据权限**不能靠配置**，只能在服务层用"权限码 + 业务列"硬实现。下面是三层规则，开发照做。

### 第 1 层：文件（不按人隔离）

| 规则 | 说明 |
|---|---|
| 列表可见范围 | **全部未删除文件**。任何有 `file:list` 的人都能看到所有文件的元数据（文件名、大小、上传人、时间） |
| 下载范围 | 非私有文件：所有有 `file:list` 的人可下载 |
| 私有文件 | 需 `file:download-private`；且**只能通过限时短链**下载（3.6.4） |
| 删除范围 | 需 `file:delete`。**不做"只能删自己上传的"限制**——文件是共享资产，限制会导致"这个人离职了，他上传的图纸谁也删不掉" |

### 第 2 层：导入 / 导出任务（按创建人隔离）

| 场景 | 规则 |
|---|---|
| 无 `task:view-all` | `WHERE created_by = 当前用户` —— 列表只出自己的任务 |
| 有 `task:view-all` | 全量可见，列表增加「创建人」筛选 |
| 详情/下载/取消 | 无 view-all 时**校验 `created_by`**，不是自己的一律 403 + 「无权查看他人的任务」（不是 404——404 会让运维以为数据丢了） |
| 删除 | 无论有没有 view-all，**只能删自己的**（`task:delete` + `created_by` 双条件） |

> **为什么要隔离**：导出任务里存着别人的筛选条件与数据范围（"张三导了全公司薪资"这条记录本身就是敏感信息），导入任务里是别人整理了两天的原始数据。工作过程数据默认私有，符合直觉。

### 第 3 层：通知（只能看自己的接收记录）

| 场景 | 规则 |
|---|---|
| 收件箱 | **硬编码 `receiver_id = 当前用户`**，接口不接受 `receiver_id` 参数（传了也忽略） |
| 发送记录 | 需 `record:list`；有 `send:group` 的人默认也有（他们是发送者） |
| 批次明细里的"谁读了" | **只给聚合数**（已读 42 / 未读 78），不给名单（附录 D 明确不做） |

---

## 6.3 敏感操作单独授权（8 个）

任务书要求"存储配置（含 AK/SK）、群发通知、删除导出任务"必须单独授权。本文落实为 8 个独立权限码，并给出**为什么必须拆**：

| # | 操作 | 权限码 | 不拆会怎样 |
|---|---|---|---|
| 1 | 改 AK/SK | `storage:backend:secret` | 让"能改名"的人顺手就能换掉密钥，把文件指到自己的桶里 |
| 2 | 设为默认存储 | `storage:backend:default` | 一次误点，全公司新上传全落到错误位置，且历史文件不受影响 → 极难发现 |
| 3 | 改上传策略 | `storage:policy:update` | 顺手把 `.exe` 加进白名单 |
| 4 | 下载私有文件 | `storage:file:download-private` | 私有文件等于公开 |
| 5 | **群发（部门/全员）** | `notice:send:group` | 一个普通运营就能给全公司发消息 → 典型事故 |
| 6 | 导出敏感明文 | `impexp:export:sensitive` | 薪资/身份证号随手导出外发 |
| 7 | **删除导出任务** | `impexp:task:delete` | 删掉"我导过什么"的痕迹 |
| 8 | 改通道配置（SMTP/Webhook 密钥） | `notice:channel:update` | 把通知外带到自己的服务器 |

**并且：前端隐藏 ≠ 安全**。以上 8 个权限码**每一个都必须在后端 `#[alun::permission]` 上声明**（F3），前端 `v-privilege` 只是不让用户白点。

---

## 6.4 审计动作码（沿用 `audit_logs`，F18）

| 动作码 | 触发 | 记录什么（`before_data` / `after_data`） | 级别 |
|---|---|---|---|
| `STORAGE_BACKEND_CREATE` | 新增存储配置 | after：`{name, backend_type, endpoint, bucket}`（**不含密钥**） | 高 |
| `STORAGE_BACKEND_UPDATE` | 编辑存储配置 | before/after：仅变更字段 | 高 |
| `STORAGE_BACKEND_DELETE` | 删除存储配置 | before：`{name, backend_type}` | 高 |
| `STORAGE_BACKEND_DEFAULT` | 设为默认 | before：`{default: "旧名称"}` / after：`{default: "新名称"}` | **极高** |
| `STORAGE_BACKEND_SECRET` | 改 AK/SK | before/after：`{"access_key":"已更新","secret_key":"已更新"}`（**绝不记值**） | **极高** |
| `STORAGE_POLICY_UPDATE` | 改上传策略 | before/after：`{category, max_size_mb, allowed_extensions}`（白名单变化是重点） | 高 |
| `FILE_UPLOAD` / `FILE_UPLOAD_BATCH` / `FILE_DELETE` | 已有（F18） | 沿用 | 中 |
| `FILE_UPLOAD_ABORT` | 直传失败/取消清理 | after：`{file_id, reason}` | 中 |
| `FILE_PRIVATE_LINK` | 生成私有文件限时链接 | after：`{file_id, ttl}`（不记链接本身） | 中 |
| `IMPEXP_IMPORT_COMMIT` | 确认入库 | after：`{task_id, biz_code, success_rows, fail_rows, skip_rows, dup_strategy}` | 高 |
| `IMPEXP_IMPORT_OVERWRITE` | 覆盖策略生效 | after：`{task_id, overwrite_count, changed_fields}` | **高** |
| `IMPEXP_EXPORT_CREATE` | 发起导出 | after：`{task_id, biz_code, fields, row_count_estimate}` | 中 |
| `IMPEXP_EXPORT_SENSITIVE` | 明文导出敏感字段 | after：`{task_id, biz_code, sensitive_fields, row_count}`（**不记值**） | **极高** |
| `IMPEXP_TASK_DELETE` | 删除任务 | before：`{task_id, task_kind, row_count}` | 高 |
| `NOTICE_SEND` | 立即发送 | after：`{batch_id, target_type, planned_count, sent_count}` | **高** |
| `NOTICE_SEND_SCHEDULE` | 定时发送创建 | after：`{batch_id, scheduled_at, planned_count}` | 高 |
| `NOTICE_SEND_CANCEL` | 取消定时 | before：`{batch_id, scheduled_at}` | 中 |
| `NOTICE_TEMPLATE_UPDATE` | 改模板 | before/after：仅变更字段 | 中 |
| `NOTICE_CHANNEL_UPDATE` | 改通道配置 | before/after：`{"enabled":true,"config":"已更新"}`（**不含密码**） | **极高** |
| `NOTICE_SETTING_UPDATE` | 改保留/轮询设置 | before/after：仅变更项 | 中 |

---

## 6.5 四个预置角色包（迁移时写入 `sys_role_menu`）

| 角色包 | 建议角色名 | 包含权限码 | 说明 |
|---|---|---|---|
| **A. 平台管理员** | 系统管理员（已有角色追加） | 6.1.1 全部 14 + 6.1.3 的 27/30/31/32/33 + 6.1.2 的 15/16/17/26 | 全套存储 + 群发；不给导入导出定义维护（那是业务侧的事） |
| **B. 业务操作员** | 普通用户（已有角色追加） | 10/11/13 + 20/21/22/24/26 + 27/28/29 | 能传文件、能导入导出自己的数据、能收消息。**不含** 12（删文件）与 25（明文导出） |
| **C. 通知管理员** | 新增角色「通知管理员」 | 27/28/29/30/31/32/33/34/35/36/37/38/39 + 13 | 可群发、可维护模板与通道查看（**不给 40 改通道**，改通道留给平台管理员） |
| **D. 审计员（只读）** | 新增角色「审计员」 | 1/8/10/15（+16 view-all）/33 + 只读查询 | 只能看，不能改任何东西；用来给内审/合规用 |

> **迁移纪律**：角色包只做"建议绑定"，**不硬把权限塞进现有角色**——因为线上已有角色的权限集合是运维精心调过的，迁移硬塞会造成"某人突然多了一堆权限"。做法：迁移只写 `sys_menu`（权限点本身）与 `sys_role_menu` 的**新角色**绑定；**已有角色一律不自动加**，在管理页给一个「一键补齐本批权限」按钮（有 `system:role:update` 的人点了才生效）。这条写进附录 B 的决策（D-B6）。

---

# 七、菜单结构

## 7.1 决策：1 个新一级目录，4 个可见菜单 + 1 个隐藏菜单

**为什么单独开一级目录而不是塞进「支撑功能」**（F2 现有 `/support`）：支撑功能里放的是**字典、参数、日志、定时任务、在线用户**——这些是"IT 运维自己用的"，业务人员一年点不了一次。而本批三个模块是**业务人员天天用**的（上传附件、导入台账、收通知）。混在一起会导致：
1. 业务人员在侧边栏找不到"导入导出"（它会排在"定时任务"后面）；
2. 权限整包授权时无法"把业务相关的打包给业务角色"；
3. B/C/D 批次还要往这里挂东西，越长越乱。

**sort_order = 70** 的由来：F2 已占用 10/20（现有）、40（ERP）、50（BI）、60（MDM），且 30 预留给协同办公。70 是下一个空位，不需要动任何已有菜单的排序。

## 7.2 菜单树（种子数据规格，供开发直接落 `sys_menu`）

```
通用底座                                        /platform         目录  sort 70  icon=system-storage
├─ 存储管理                                    /platform/storage  菜单  sort 10  icon=object-storage
│   auth_code = business:storage:backend:list
│   component = business/storage/backend/index
│   ├─ [按钮] 新增存储配置      business:storage:backend:create
│   ├─ [按钮] 编辑存储配置      business:storage:backend:update
│   ├─ [按钮] 删除存储配置      business:storage:backend:delete
│   ├─ [按钮] 设为默认          business:storage:backend:default
│   ├─ [按钮] 更新访问密钥      business:storage:backend:secret
│   ├─ [按钮] 连通性测试        business:storage:backend:test
│   ├─ [按钮] 查看上传策略      business:storage:policy:list
│   └─ [按钮] 编辑上传策略      business:storage:policy:update
│
├─ 文件管理                                    /platform/file     菜单  sort 20  icon=folder
│   auth_code = business:storage:file:list
│   component = business/storage/file/index
│   ├─ [按钮] 上传文件              business:storage:file:upload
│   ├─ [按钮] 删除文件              business:storage:file:delete
│   ├─ [按钮] 修改文件属性          business:storage:file:update
│   └─ [按钮] 下载私有文件          business:storage:file:download-private
│
├─ 导入导出                                    /platform/impexp   菜单  sort 30  icon=file-export
│   auth_code = business:impexp:task:list
│   component = business/impexp/task/index
│   ├─ [按钮] 查看全部任务          business:impexp:task:view-all
│   ├─ [按钮] 删除任务              business:impexp:task:delete
│   ├─ [按钮] 查看导入导出定义      business:impexp:definition:list
│   ├─ [按钮] 维护导入导出定义      business:impexp:definition:update
│   ├─ [按钮] 下载导入模板          business:impexp:template:download
│   ├─ [按钮] 发起导入              business:impexp:import:create
│   ├─ [按钮] 确认入库              business:impexp:import:confirm
│   ├─ [按钮] 取消导入任务          business:impexp:import:cancel
│   ├─ [按钮] 发起导出              business:impexp:export:create
│   ├─ [按钮] 导出敏感字段明文      business:impexp:export:sensitive
│   └─ [按钮] 下载明细/产物         business:impexp:error:download
│
├─ 通知管理                                    /platform/notice   菜单  sort 40  icon=notification
│   auth_code = business:notice:template:list
│   component = business/notice/template/index
│   ├─ [按钮] 发送通知（用户/角色）   business:notice:send:create
│   ├─ [按钮] 群发通知（部门/全员）   business:notice:send:group
│   ├─ [按钮] 取消定时发送            business:notice:send:cancel
│   ├─ [按钮] 查看发送记录            business:notice:record:list
│   ├─ [按钮] 新增模板                business:notice:template:create
│   ├─ [按钮] 编辑模板                business:notice:template:update
│   ├─ [按钮] 删除模板                business:notice:template:delete
│   ├─ [按钮] 测试发送                business:notice:template:test
│   ├─ [按钮] 查看通道配置            business:notice:channel:list
│   ├─ [按钮] 编辑通道配置            business:notice:channel:update
│   └─ [按钮] 编辑通知设置            business:notice:setting:update
│
└─ 消息中心                                    /platform/message  菜单  sort 50  icon=system-messages
    auth_code = business:notice:message:list
    is_visible = FALSE        ← 不在侧边栏显示，从顶栏铃铛进入（F6 已证隐藏菜单不进导航）
    component = business/notice/message/index
    ├─ [按钮] 标记已读        business:notice:message:read
    └─ [按钮] 删除消息        business:notice:message:delete
```

## 7.3 三个必须注意的落地细节

| # | 细节 | 说明 |
|---|---|---|
| 1 | **`module_code` 全部填 `business`**（含按钮行） | F1：字段 `VARCHAR(20)`，`business` 合法。现有菜单管理页的"模块"筛选下拉若是硬编码 `system/support`，**必须加上 `business` 选项**，否则管理员筛不到本批菜单（ERP 批次已踩过这个坑，见其 PRD 附录）。 |
| 2 | **`component` 路径中的页面文件必须真实存在** | 五个：`business/storage/backend/index` / `business/storage/file/index` / `business/impexp/task/index` / `business/notice/template/index` / `business/notice/message/index`。缺一个就是一个白屏。 |
| 3 | **隐藏菜单也要 `auth_code` 非空** | 否则 `filterNavMenus` 里 `hasPermission('')` 返回 true（F5：空码视为放行），等于"任何人都能访问消息中心路由"——虽然消息只查自己的，但应按最小权限给码。 |

## 7.4 旧菜单处置（`/support/file`）

| 项 | 处置 | 理由 |
|---|---|---|
| 菜单行 | `status = 0`（停用） | 从侧边栏消失，但不删除记录（角色绑定关系保留，随时可恢复） |
| 页面与路由 | **保留**（`views/support/file/index.vue` 不删） | 万一新页面出问题可以立刻切回 |
| 5 个接口 | **全部保留**（3.2.4） | ERP/BI 的附件调用依赖它们 |
| 公告 | 上线公告里写明「文件管理已迁至 通用底座 → 文件管理」 | 避免用户找不到 |
---

# 八、页面结构与交互

## 8.0 全局约定（先定这 10 条，后面 9 个页面不再重复）

| # | 约定 | 具体做法 |
|---|---|---|
| 1 | **布局基线** | 沿用 F8 范式：`t-card :bordered="false"` 筛选卡 + 表格卡；页面外边距 `16px`；卡片间距 `16px` |
| 2 | **配色只用变量** | 品牌色一律 `var(--td-brand-color-7)`（或 `-hover` / `-active`）；成功 `var(--td-success-color)`、警告 `var(--td-warning-color)`、错误 `var(--td-error-color)`、正文 `var(--td-text-color-primary)`、次要 `var(--td-text-color-secondary)`、边框 `var(--td-component-border)`、页面底 `var(--td-bg-color-page)`。**禁止任何硬编码色值**（F12：硬编码颜色在切换 22 套配色时会变成"一套配色里混着另一套的蓝"） |
| 3 | **状态色语义固定** | 成功/已完成 → success；进行中/排队 → brand（主色）；警告/待确认/部分成功 → warning；失败/过期/已取消 → error；停用/历史 → default（灰）。**不允许"进行中用蓝色、另一个页面进行中用绿色"** |
| 4 | **加载态** | 列表用 `t-table :loading`；整页首次加载用 `t-loading` 覆盖层；**按钮级操作必须在按钮上加 `:loading`**（禁止"点了没反应、3 秒后才发现成功了"） |
| 5 | **危险操作必须二次确认** | 删除、覆盖导入、设为默认、群发、改密钥、明文导出 —— 全部 `DialogPlugin.confirm`，文案里**必须带具体数字**（"删除选中的 12 个文件"，不是"删除选中文件"） |
| 6 | **成功反馈** | 轻操作用 `MessagePlugin.success('已保存')`（2 秒消失）；重操作（导入入库、批量删除、群发）用**结果提示条 + 汇总数字**（"成功 120，失败 0"），因为用户需要读数 |
| 7 | **错误反馈** | 后端返回 `{code, msg}` 时，**直接展示 `msg`**（后端 msg 已经是给用户看的话，见附录 A）；**禁止**前端自己拼"操作失败，请重试"这种没有信息量的话 |
| 8 | **空态** | 统一三段式：图标（`t-icon` 灰 48px）+ 一句主文案（如"还没有文件"）+ 一个引导按钮（如"上传文件"）。**禁止只显示"暂无数据"** |
| 9 | **时间与数字格式** | 时间 `YYYY-MM-DD HH:mm`（列表）/ `YYYY-MM-DD HH:mm:ss`（详情）；文件大小走 `formatFileSize`（已有工具）；行数带千分位（`5,000`）；百分比整数 |
| 10 | **响应式** | 最小支持 **1366×768**；表格列总宽超容器时横向滚动（不压缩文字）；消息中心双栏在 <1200px 时退化为"列表 → 点击进详情页"的单栏 |

---

## 8.1 页面总览

| # | 页面 | 路由 | 权限（页面级） | 主要组件 |
|---|---|---|---|---|
| P1 | 存储配置 | `/platform/storage` | `business:storage:backend:list` | 列表 + 抽屉表单 + 策略 tab |
| P2 | 文件管理 | `/platform/file` | `business:storage:file:list` | 筛选 + 表格 + 预览抽屉 + 上传 |
| P3 | 导入向导 | `/platform/impexp/import` | `business:impexp:import:create` | `t-steps` 三步 |
| P4 | 导入导出任务 | `/platform/impexp` | `business:impexp:task:list` | 双 tab + 进度条 |
| P5 | 消息中心 | `/platform/message` | `business:notice:message:list` | 双栏列表 + 详情 |
| P6 | 顶栏铃铛 | 全局（`BasicLayout` 内） | 同上 | `t-badge` + `t-popup` |
| P7 | 通知模板 | `/platform/notice` | `business:notice:template:list` | 列表 + 编辑抽屉（变量面板 + 预览）+ 三个 tab（模板 / 发送记录 / 通道与设置） |
| P8 | 发送通知 | `/platform/notice/send`（子路由，不在菜单中列出；从模板列表「发送通知」按钮进入） | `business:notice:send:create` | 表单 + 人员/部门选择器 |
| P9 | 通知设置 | `/platform/notice?tab=setting`（P7 页面的第三个 tab，非独立路由） | `business:notice:channel:list` | 通道卡片 + 保留期表单 |

---

## 8.2 P1 存储配置（`/platform/storage`）

### 8.2.1 页面结构

`t-tabs` 两个页签：**存储后端**（默认）/ **上传策略**。

### 8.2.2 「存储后端」列表

**工具栏**：`[+ 新增存储配置]`（primary，`v-privilege` 包 `backend:create`）· `[刷新]` · 右侧「共 N 条」

**列**

| 列 | 宽 | 内容 |
|---|---|---|
| 配置名称 | 160 | 加粗；默认那条前面有 `t-tag theme="primary" size="small"` 标「默认」 |
| 类型 | 100 | 「本地存储」/「MinIO」/「S3 兼容」/「阿里 OSS」（按 `backend_type` + `preset` 翻译） |
| 端点 / 桶 | 260 | `minio.corp.local:9000` `／cloudpivot`；过长省略号 + tooltip |
| 密钥 | 100 | `AKIA****7ZP2`（掩码）或灰字「未配置」 |
| 连通性 | 110 | 绿点「正常 42ms」/ 红点「失败」/ 灰点「未测试」+ 相对时间 tooltip |
| 状态 | 80 | `t-switch`（有 `backend:update` 才可点；停用默认后端时拒绝并提示） |
| 更新时间 | 160 | — |
| 操作 | 200 | 测试 / 编辑 / 设为默认 / 删除（全部 `v-privilege` 控制） |

**行内操作反馈**

| 操作 | 反馈 |
|---|---|
| 点「测试」 | 该行连通性列变 `t-loading`，最多 10 秒；结果直接更新该行（不弹窗），并 `MessagePlugin`：「连接成功（延迟 42ms）」 |
| 点「设为默认」 | 弹框：「将默认存储从「本地默认」切换为「生产 MinIO」。**只影响之后上传的新文件**，已有 12,486 个文件仍从各自原存储读取。是否继续？」→ 确定后整表刷新，新默认行出现「默认」标签 |
| 点「删除」 | 若被文件引用 → 弹错误框：「该存储上还有 **1,204** 个文件，无法删除。请先迁移或删除这些文件，或将其它存储设为默认后再试」；未被引用 → 「确定删除存储配置「测试 MinIO」？删除后不可恢复」 |
| 切「状态」开关 | 停用当前默认后端 → 拒绝：「默认存储不能停用，请先把其它存储设为默认」 |

**空态**：图标 `object-storage` + 「还没有配置存储后端」+ 灰字「系统将使用本地磁盘（`uploads` 目录）作为默认存储」+ 按钮「新增存储配置」

---

### 8.2.3 新增 / 编辑存储配置（右侧抽屉 `t-drawer`，宽 560）

**为什么用抽屉不用弹窗**：字段多（11 个）+ 有"测试连接"结果区需要持续可见，弹窗高度会溢出。

**表单字段（新增）**

| 字段 | 控件 | 校验与联动 |
|---|---|---|
| 配置名称 | `t-input` | 必填，≤64 字符；重名报「已存在同名配置」 |
| 存储类型 | `t-radio-group`（4 项） | **本地存储 / MinIO / S3 兼容 / 阿里 OSS** |
| 端点 | `t-input` | 本地：填目录（默认 `uploads`）；MinIO 类：必须有 `http://` 或 `https://` 前缀。**校验不过提示**：「端点需以 http:// 或 https:// 开头」 |
| 桶名称 | `t-input` | 选「本地存储」时整行隐藏；其它必填，3–63 字符，小写字母数字与 `-` |
| Region | `t-input` | 默认 `us-east-1`；选「阿里 OSS」时 placeholder 变 `oss-cn-hangzhou` |
| Access Key | `t-input`（明码，因为 AK 不敏感） | 必填（本地存储除外） |
| Secret Key | `t-input type="password"` + 显示/隐藏眼睛 | 必填（本地除外）；**保存后永不再回显** |
| 使用 TLS | `t-switch` | 与端点协议不一致时提示（`https` 却关 TLS → 警告） |
| 对象前缀 | `t-input` | 选填，如 `cloudpivot/`；自动补尾斜杠 |
| 设为默认 | `t-switch` | 打开时显示提示条：「设为默认后，新上传的文件将写入该存储」 |
| 备注 | `t-textarea` | ≤255 |

**联动规则（选类型时自动带出，这是 S1-09 的全部价值）**

| 选「阿里 OSS」 | 选「S3 兼容」（如腾讯 COS / AWS） |
|---|---|
| 端点 placeholder：`https://oss-cn-hangzhou.aliyuncs.com` | 端点 placeholder：`https://s3.us-east-1.amazonaws.com` |
| Region placeholder：`oss-cn-hangzhou` | Region placeholder：`us-east-1` |
| 下方提示条（`t-alert theme="info"`）：「阿里 OSS 使用 Path-Style 访问需端点含 `minio` 或关闭 TLS；若使用虚拟主机风格请保持 TLS 开启」（依据 F20 的 `force_path_style` 判定） | 提示条：「腾讯 COS 端点形如 `https://cos.ap-guangzhou.myqcloud.com`，Region 填 `ap-guangzhou`」 |

**「测试连接」按钮（S1-06，本页最关键的一个按钮）**

- 位置：抽屉底部左侧，与「取消 / 保存」同一行
- 前置条件：端点、桶、AK、SK 已填（否则按钮 disabled + tooltip「请先填写完整连接信息」）
- 点击：按钮 loading + 右侧结果区出现 `t-loading`；**最多 10 秒**
- 结果区（在按钮右侧，占满剩余宽度）：

| 结果 | 展示 |
|---|---|
| 成功 | 绿底条 + `check-circle` 图标 + 「连接成功 · 桶 `cloudpivot` 已存在 · Region `us-east-1` · 延迟 **42ms**」 |
| 桶不存在已创建 | 黄底条 + `info-circle` + 「连接成功，但桶不存在，已自动创建」 |
| 失败 | 红底条 + `error-circle` + **具体原因**（六类文案见 3.3.2，直接展示后端 msg） |

- **测试通过不自动保存**：用户仍需点「保存」（避免"我只是试一下"就落库）
- **测试不通过仍可保存**？**允许**（用户可能先配好、稍后再通网络），但保存时弹提示：「连接测试未通过，仍然保存吗？保存后新上传可能失败」+ 「仍然保存」

**编辑态的差异（S1-03）**
- 配置名称、备注、TLS、对象前缀、状态可改
- **端点、桶、Region 置灰不可改**，旁边灰字：「地址与桶不可修改（修改会导致已有文件无法访问）。如需更换，请新增一个配置并切换默认存储」
- AK/SK 输入框 placeholder 改为「留空则不修改」；**不显示原值**（连掩码都不显示在输入框里，掩码只在列表/详情展示）
- 改 AK/SK 时保存 → 二次确认（3.7.3 第 3 条）

---

### 8.2.4 「上传策略」页签（S2）

**列表**：每行一个分类。列：分类（中文名 + 英文短码灰字）/ 单文件上限 / 类型白名单（显示前 5 个 + 「等 N 种」）/ 校验真实类型 / 允许私有 / 允许直传 / 状态 / 操作（编辑）

每行末尾固定一个「恢复默认」文字链（有 `policy:update` 才显示），点了弹：「恢复「通用附件」为系统默认策略（20MB / 31 种类型）？」

**编辑抽屉**
- 上限：`t-input-number`（1–2048 MB）+ 快捷按钮 `5 / 20 / 50 / 100 / 500 / 2048`
- 扩展名白名单：`t-select multiple filterable` + 「自定义添加」（输入后回车加入，统一转小写、去点）；**下方灰字提示**「不在白名单的扩展名将被拒绝上传，错误提示为「不允许上传该类型文件：exe」」
- 真实类型校验：`t-switch` + 说明「开启后将读取文件头判断真实类型，与扩展名不符时拒绝（可防「改扩展名」绕过）」
- 允许私有：`t-switch`
- 允许直传：`t-switch` + 说明「开启后 >50MB 的文件将由浏览器直接上传到对象存储（仅对象存储后端有效）」
- **保存前提示影响面**：若删减了白名单，显示「本次将移除 6 种类型（rar/7z/gz/tar/mkv），这些类型的已有文件**不受影响**，但无法再上传新的」

---

## 8.3 P2 文件管理（`/platform/file`）

### 8.3.1 筛选区（见 3.8.1 的 5 个条件）

右侧按钮：`[查询]`（primary）`[重置]`

### 8.3.2 工具栏

`[上传文件]`（`t-upload` 包装，`theme="primary"`）· `[批量删除]`（选中 >0 才启用，`theme="danger" variant="outline"`）· `[刷新]` · 右侧「已选 3 项 / 共 12,486 条」

### 8.3.3 表格

列见 3.8.2。补充交互：

| 交互 | 细节 |
|---|---|
| 点击文件名 | 图片/PDF → 打开预览抽屉；其它类型 → 直接下载（`t-link`）；私有文件无权限 → 文字变灰 + tooltip「私有文件，需下载权限」 |
| 上传按钮 | 选文件后立即显示每个文件的进度（`t-upload` 内置）；超过分类上限时**在上传前就拦**（前端先查策略）+ 后端再拦；失败文件在列表里标红并显示原因，成功的正常出现 |
| 批量删除 | 见 3.8.4 |
| 跨页选择 | 保留选中（横幅提示「已选 12 项，其中 5 项在其它页」+ 「清空选择」） |

**空态（有筛选条件时）**：图标 `search` + 「没有找到符合条件的文件」+ 按钮「清除筛选条件」
**空态（无筛选）**：图标 `folder` + 「还没有上传任何文件」+ 按钮「上传文件」

### 8.3.4 预览抽屉

- 宽 720，标题 = 原文件名 + 右侧三个操作：下载 / 复制链接（**仅非私有**）/ 关闭
- 内容：图片 → `t-image-viewer` 内嵌；PDF → `<iframe>` 高 560px
- 底部信息条（一目了然）：`1.2 MB · PDF · 合同 · 张伟 · 2026-10-20 14:32 · MinIO 生产`
- **私有文件**：抽屉顶部一条黄色提示「私有文件 · 此预览链接 60 分钟后失效」

---

## 8.4 P3 导入向导（`/platform/impexp/import`）

### 8.4.1 三步结构（`t-steps` 顶部固定，可回退到第 1 步）

```
① 上传文件  →  ② 校验报告  →  ③ 确认导入（结果）
```

**入口**：① 任务列表页右上角 `[+ 新建导入]`；② 各业务列表页的「导入」按钮（带 `biz_code` 直达）。

### 8.4.2 第 1 步：上传文件

| 区域 | 内容 |
|---|---|
| 业务选择 | `t-select`（选项 = 启用的 import 定义），选中后**页面标题变**「导入物料」 |
| 模板下载 | 卡片：`file-excel` 图标 + 「物料导入模板_v1.xlsx」+ 灰字「含 12 个字段，5 个必填，最多 100,000 行」+ 按钮「下载模板」（`v-privilege` 包 `template:download`） |
| 上传区 | `t-upload` 拖拽区（虚线框，`theme="custom"`）：主文案「点击或拖拽 Excel 文件到此处」+ 副文案「支持 .xlsx / .xls，单文件 ≤ 100MB」 |
| 导入选项 | 两个：**重复数据处理**（`t-radio-group`：跳过 / 覆盖 / 报错，默认取定义的 `dup_strategy`）；**允许部分导入**（`t-switch`，默认关，只有定义 `allow_partial=true` 才显示） |
| 底部 | `[开始校验]`（primary，未选文件时 disabled） |

**覆盖策略选中时的提示**：黄色 `t-alert`：「覆盖策略会用文件中的数据更新已存在的记录。覆盖操作会记录审计日志，但原值不可一键恢复。」

**上传后**：前端不解析文件（也解析不了），直接把文件 `POST` 给后端；显示「正在上传 2.1MB…」→「正在解析与校验…」→ 转入第 2 步。

### 8.4.3 第 2 步：校验报告

**结构（从上到下）**

1. **三块数字卡**（见 4.3.1）：总计 5,000 / 可导入 4,995（绿）/ 失败 5（红）
2. **补充说明条**（按策略动态文案，见 4.3.1）
3. **失败明细表**（列：行号 / 列名 / 原值 / 失败原因 / 建议；最多展示 200 条，滚动加载）
4. **底部操作区**：

| 按钮 | 条件 | 行为 |
|---|---|---|
| `[下载错误明细]` | 失败数 > 0 | 下载 xlsx（4.3.3），`MessagePlugin.success('已开始下载')` |
| `[上一步]` | 总是 | 回到第 1 步（保留业务与选项，**清空文件**） |
| `[确认导入]` | 可导入数 > 0 **且**（失败数 = 0 **或** 允许部分导入） | primary；点了弹确认：「将导入 **4,995** 行到「物料」。其中 12 行因编码已存在被跳过。是否继续？」 |
| 禁止态提示 | 失败 > 0 且未开部分导入 | `[确认导入]` disabled，旁边红字「存在 5 行校验失败，请修正后重新上传，或开启"允许部分导入"」 |

**执行中**：按钮变 loading，下方出现进度条 + 阶段文案（`正在写入（88%）`）；**不允许关闭页面**（`beforeunload` 拦截：「导入正在进行，关闭页面将中断导入」）

### 8.4.4 第 3 步：结果

| 结果 | 展示 |
|---|---|
| 全部成功 | 绿勾大图标 + 「导入成功：5,000 行全部写入」+ 耗时「8.4 秒」+ 按钮「查看数据」（跳业务列表）/「再导一批」 |
| 部分成功 | 黄叹号 + 「导入完成：成功 4,995 行，失败 5 行（已跳过）」+ 「下载错误明细」+「重新导入失败行」 |
| 失败 | 红叉 + 「导入失败」+ **具体原因**（如 E-IMP-19）+ 「下载错误明细」/「重新上传」 |

> **「重新导入失败行」只做一件事**：把错误明细文件当作源文件再走一遍向导（不是新功能，是路径复用）。P1。

---

## 8.5 P4 导入导出任务（`/platform/impexp`）

### 8.5.1 双 tab

`t-tabs`：**导出任务**（默认）/ **导入记录**。右上角 `[+ 新建导入]`、以及（有 `definition:update` 时）`[导入导出定义]`。

### 8.5.2 导出任务列表

**筛选**：任务名（模糊）/ 状态（多选）/ 业务 / 创建人（仅 `view-all`）/ 时间范围

**列**：任务名 / 业务 / 行数 / **状态+进度** / 创建人 / 耗时 / 创建时间 / 操作

**进度单元格（一行内三态）**
```
排队中        ▓▓░░░░░░░░  排队中（第 2 位）
执行中        ▓▓▓▓▓░░░░░  正在查询（45%）
已完成        ▓▓▓▓▓▓▓▓▓▓  已完成 · 1 分 42 秒
失败          ▓▓▓▓░░░░░░  失败 ⓘ（hover 显示原因）
```
- `t-progress` 线形，`theme` 随状态（进行中=primary、成功=success、失败=error、排队=default）
- 有进行中任务时**每 3 秒轮询**（4.7.3）；列表里全部终结后停止轮询（顶部小字「已暂停自动刷新」）

**行操作**

| 操作 | 条件 | 反馈 |
|---|---|---|
| 下载 | 状态=成功 且未过期 | 直接触发下载；同时 `MessagePlugin.success('开始下载')`；hover 该按钮显示「有效期至 2026-10-29 18:40」 |
| 下载（已过期） | 状态=成功 且 `result_expires_at` 已过 | 按钮变灰文字「已过期」+ tooltip「产物已保留 7 天后清理，链接有效期 24 小时」；点「重新导出」 |
| 取消 | 排队中/执行中 | 确认「确定取消该导出任务？」→ 状态变「已取消」，产物已删 |
| 删除 | 自己的 且有 `task:delete` | 确认后软删；列表短暂保留该行并标「已删除」再刷新 |
| 详情 | 总是 | 抽屉：筛选条件快照（`params` 可读化）、字段列表、行数、耗时、操作人、错误信息 |

**空态**：图标 `file-export` + 「还没有导出任务」+ 灰字「在任意列表页点击「导出」即可创建任务」

### 8.5.3 导入记录列表

**列**：文件名（点击 → 任务详情抽屉）/ 业务 / 总行数 / 成功（绿）/ 失败（红，>0 时可点开明细）/ 重复跳过（灰）/ 状态 / 操作人 / 耗时 / 时间 / 操作（查看报告 · 下载错误明细 · 删除）

**任务详情抽屉**内容：文件信息 + 三块数字卡（同 8.4.3）+ 失败明细表（同 8.4.3）+ 选项快照（重复策略/是否部分导入）

### 8.5.4 「导入导出定义」抽屉（`definition:update`）

- 左侧业务列表（`t-list`），右侧字段表
- 字段表列：字段名 / 中文标签 / 类型 / 必填 / 长度 / 枚举字典 / 判重 / 敏感 / 默认导出
- 顶部三个开关：默认重复策略 / 允许部分导入 / 单次行数上限
- **保存前强提示**：「修改字段定义会影响之后的导入导出。已经生成的历史模板（v1）将不再匹配，模板版本将自动升到 v2。」（呼应 I1-03）

---

## 8.6 P5 消息中心（`/platform/message`）+ P6 顶栏铃铛

### 8.6.1 顶栏铃铛（P6，全局）

- 位置：主题切换按钮与用户头像之间（8.0 约定 + 5.3.1）
- `t-badge :count="unread" :max-count="99"` 包一个 `t-button variant="text" shape="square"`，图标 `notification`
- 点击 → `t-popup` 面板（宽 360）：
  - 顶部：「消息中心」+ 右侧「全部已读」文字链（未读 0 时隐藏）
  - 列表：最近 **5** 条未读，每条两行（标题单行省略 / 相对时间 + 类型标签）；未读左侧有 6px 主色圆点
  - 底部：「查看全部（37）」按钮（整宽，`variant="text"`）
  - 无未读：「暂无未读消息」+ 灰字「有新的审批或公告会出现在这里」
- 点某条 → 直接跳该条详情（`/platform/message?id=xxx`），**不经过列表**

### 8.6.2 消息中心页面（P5）

**布局**：左右双栏（左 380 固定 + 右自适应），各自独立滚动。

**左栏**

| 区域 | 内容 |
|---|---|
| 页签 | `t-tabs`：未读（带数字角标）/ 已读 / 全部 |
| 类型筛选 | 一排 `t-radio-group variant="default-filled"`：全部 / 系统通知 / 业务通知 |
| 列表 | 卡片行：左侧 4px 级别色条（`urgent` 红 / `warning` 橙 / `info` 透明）+ 标题（未读加粗）+ 摘要一行（正文前 40 字）+ 底部「类型标签 · 相对时间」 |
| 选中态 | 背景 `var(--td-bg-color-container-hover)` + 左侧加 2px 主色描边 |
| 底部 | `t-pagination`（每页 10）+ 「全部已读」（有未读时显示） |

**右栏（详情，见 5.2.2）**

**关键操作反馈**

| 操作 | 反馈 |
|---|---|
| 点开未读消息 | 立即已读：左栏该行文字由加粗变常规、圆点消失、**顶栏未读数 -1**（本地先减，不等待接口）；接口失败则回滚并 `MessagePlugin.error` |
| 全部已读 | 确认「将把 **37** 条未读消息标记为已读？」→ 成功「已全部标记为已读（37 条）」→ 当前在"未读"页签时列表转空态 |
| 删除 | 左栏行 hover 出现 `delete` 图标 → 点后**直接删**（轻操作）→ 该行淡出移除；若删的是当前打开的那条，右栏回到空态 |
| 批量删除 | 左栏顶部出现「选择」模式（P0 可只做单条删除，批量删除标 P1 的替代：给一个「清空已读」入口） |

**空态**

| 场景 | 文案 |
|---|---|
| 未读为空 | 图标 `check-circle`（success 色）+ 「没有未读消息」+ 灰字「所有消息都看过了」 |
| 已读为空 | 「还没有已读消息」 |
| 筛选后为空 | 「没有该类型的消息」+ 「清除筛选」 |

---

## 8.7 P7 通知模板（`/platform/notice`）

### 8.7.1 页面结构

`t-tabs`：**通知模板**（默认）/ **发送记录** / **通道与设置**（三个 tab 合在一个页面，减少菜单数量）。

### 8.7.2 模板列表

**工具栏**：`[+ 新增模板]` / `[发送通知]`（跳 P8）/ 右侧搜索框（模板编码/名称）

**列**：模板编码（等宽字体）/ 模板名称 / 类型（系统通知/业务通知标签）/ 标题（省略）/ 变量数（如「3 个」，hover 列出）/ 状态 / 更新时间 / 操作（编辑 · 测试 · 删除）

**空态**：图标 `notification` + 「还没有通知模板」+ 灰字「模板让你一次写好文案，反复用」+ 按钮「新增模板」

### 8.7.3 模板编辑抽屉（宽 760，**双栏**：左边编辑、右边预览）

| 区域 | 控件 |
|---|---|
| 基本信息 | 模板编码（`t-input`，编辑态不可改）、模板名称、通知类型（`t-radio-group`）、业务标识（选"业务通知"时出现，`t-input`，如 `erp.purchase_order`）、重要级别（`t-radio-group`：普通/警告/紧急，选紧急时右侧出现红字「紧急通知在收件箱最上方并有红色标识」） |
| 标题 | `t-input` + 右侧变量按钮组 |
| 正文 | `t-textarea :autosize="{minRows:8}"` + 右侧变量按钮组 |
| **变量面板** | 标题下方一行：「可用变量：」+ 12 个 `t-tag` 可点击（点击插入到光标处）；每个 tag hover 显示中文名与示例值 |
| 默认通道 | 三个开关（站内信 disabled 且开启；邮件/Webhook 可开，旁有灰字「尚未实现发送」） |

**右侧预览区（吸附）**
- 「预览」标题 + 刷新图标
- 用示例值渲染：`{{userName}}`→`张三`、`{{docNo}}`→`CG-202610-0001`、`{{deadline}}`→`2026-10-31 18:00`、其余按变量声明的 `sample`
- 渲染结果**实时更新**（正文输入时同步）
- 底部一条灰字：「预览使用示例数据；实际发送时按收件人填充」

**关键反馈**

| 情况 | 反馈 |
|---|---|
| 正文里有未定义变量 | 输入时**实时**红字提示在正文下方：「变量 `{{userNmae}}` 未定义（可用：userName / docNo …）」，且 `[保存]` disabled |
| 变量名含空格（`{{ userName }}`） | 「变量名不能含空格，请写作 `{{userName}}`」 |
| 标题超 200 / 正文超 5000 | 字数计数变红 + 保存 disabled |
| 保存成功 | `MessagePlugin.success('模板已保存')` + 列表刷新 |
| 点「测试发送」 | `DialogPlugin.confirm`：「将用示例数据渲染并发送一条带「[测试]」标记的消息给 **你自己**，24 小时后自动清除。是否继续？」→ 成功「测试消息已发送，请查看顶栏铃铛」 |
| 删除被引用模板 | 「该模板已被 **3** 个发送批次使用，无法删除。可将状态改为「停用」」（停用后不能再选来发送，但历史消息不受影响） |

---

## 8.8 P8 发送通知（`/platform/notice/send`）

### 8.8.1 表单结构（单页，非向导）

| 区块 | 字段 | 说明 |
|---|---|---|
| **① 内容** | 使用模板（`t-select`，选填）/ 标题 / 正文 / 重要级别 | 选模板后自动填充标题与正文，并显示「已填充模板「月度结账提醒」，可继续修改」；修改后不再联动 |
| **② 发送对象** | `t-radio-group`：指定用户 / 角色 / 部门 / 全员 | 选不同项时下方选择器切换（见 8.8.2） |
| **③ 变量取值** | 动态表单：模板里出现的**公共变量**逐项出现（如 `deadline` 出现 `t-date-picker`）；按人变量（`userName` 等）不出现，只显示灰字「按收件人自动填充」 | 无模板时无此区块 |
| **④ 发送时间** | `t-radio-group`：立即发送 / 定时发送 + `t-date-picker enable-time-picker` | 定时只能选未来，最早 = 当前 + 5 分钟 |
| **⑤ 底部** | 左：「将发送给 **120** 人」大字（主色）+ 下方"展开明细"链接；右：`[取消]` `[发送]` | 人数实时计算（防抖 500ms） |

### 8.8.2 四种目标的选择器

| 目标 | 控件 | 细节 |
|---|---|---|
| 指定用户 | `t-select multiple filterable`（远程搜索用户） | 已选显示为 `t-tag` 可删；超过 500 人时**不再允许添加**并提示上限 |
| 角色 | `t-select multiple`（角色列表） | 每个选项后显示该角色人数，如「财务专员（18 人）」 |
| **部门** | 部门树（`t-tree` 多选，复选框） + **「包含下级部门」开关** | 树默认展开一级；开关默认**关**（防误发）；打开后人数立即重算 |
| 全员 | 无选择器，直接显示 `t-alert theme="warning"`：「将发送给全部启用用户（**1,024** 人）。此操作不可撤回，请确认内容无误。」 | `v-privilege` 包 `send:group`，无权限时该单选项 disabled + tooltip「需「群发通知」权限」 |

### 8.8.3 人数预览（必须做，这是群发唯一的"防呆"）

点「展开明细」弹小窗：

```
发送目标：财务部（含下级）
├─ 财务部（直属）          34 人
├─ 财务部-会计核算组        52 人
├─ 财务部-资金组            21 人
└─ 财务部-税务组            13 人
────────────────────────────────
  合计（去重 2 人）        120 人
  将排除：停用账号 3 人、已删除账号 1 人
```

> **为什么要把"排除了几个停用账号"写出来**：运维看到"选了 120 人却只发了 116"，第一反应是"系统丢了 4 个人"。把排除项摊开，投诉量直接归零。

### 8.8.4 关键反馈

| 操作 | 反馈 |
|---|---|
| 点「发送」（立即） | 确认框：「即将向 **120** 人发送通知，**不可撤回**。标题：月度结账提醒（11 月）。是否继续？」→ 按钮 loading → 成功：`MessagePlugin.success('已发送 120 人（失败 0）')` + 自动跳「发送记录」并高亮新批次 |
| 点「发送」（定时） | 确认：「将于 **2026-11-02 09:00** 向 **120** 人发送。在此之前可在发送记录中取消。」→ 成功提示后跳发送记录 |
| 有 `send:create` 无 `send:group` 且选了部门/全员 | 部门树与全员项 disabled + tooltip「需「群发通知」权限」 |
| 标题/正文为空 | 校验红字，按钮 disabled |
| 模板变量未填 | 该变量输入框红框 + 「请填写「截止时间」」 |

### 8.8.5 发送记录 tab

列：批次号 / 标题 / 目标 / 预计人数 / 实发 / 失败 / 状态 / 方式（立即/定时） / 发送时间 / 操作人 / 操作（查看明细 · 取消）

「查看明细」抽屉：目标快照描述 + 人数分解（同 8.8.3）+ **阅读统计**（已读 42 / 未读 78）+ 失败名单（若有）

### 8.8.6 通道与设置 tab（P9）

**通道卡片**（三张横排）
| 站内信 | 邮件 | Webhook |
|---|---|---|
| 图标 `notification` + 「站内信」+ `t-switch` **disabled 且开** + 灰字「基础通道，不可关闭」 | 图标 `mail` + 「邮件」+ `t-switch` 可开 + **黄色提示条「当前版本仅记录配置，尚未实现发送」** + 「配置」按钮（抽屉：SMTP 主机/端口/发件人/账号/密码/加密方式） | 图标 `link` + 「Webhook」+ 同邮件 + 配置抽屉（URL/密钥/超时/重试次数/签名方式） |

**通知设置**（下方表单，`setting:update` 才可改）
- 已读消息保留天数（`t-input-number`，默认 180，范围 7–3650）
- 测试消息保留小时（默认 24）
- 未读轮询间隔（秒，默认 60，范围 30–300）
- `[保存设置]`

---

## 8.9 页面与权限矩阵（给前端做 `v-privilege` 的对照表）

| 页面 | 元素 | 需要的权限码 |
|---|---|---|
| P1 | 页面进入 | `storage:backend:list` |
| P1 | 新增按钮 | `storage:backend:create` |
| P1 | 编辑 / 测试 / 设默认 / 删除 / 状态开关 | `update` / `test` / `default` / `delete` / `update` |
| P1 | AK/SK 输入框可编辑 | `storage:backend:secret`（无此码时输入框 disabled + tooltip「需「更新访问密钥」权限」） |
| P1 | 上传策略 tab 可见 | `storage:policy:list`；编辑按钮 `policy:update` |
| P2 | 页面进入 | `storage:file:list` |
| P2 | 上传 / 删除 / 私有文件下载 / 改属性 | `file:upload` / `file:delete` / `file:download-private` / `file:update` |
| P3 | 进入向导 / 下载模板 / 确认导入 | `import:create` / `template:download` / `import:confirm` |
| P4 | 页面进入 / 全部人任务 / 删除 / 下载产物 | `task:list` / `task:view-all` / `task:delete` / `error:download` |
| P4 | 维护定义 | `definition:update` |
| P5 | 页面进入 / 已读 / 删除 | `message:list` / `message:read` / `message:delete` |
| P7 | 页面进入 / 增删改 / 测试 | `template:list` / `template:create|update|delete` / `template:test` |
| P8 | 发送（用户/角色） / 部门/全员 / 取消定时 | `send:create` / `send:group` / `send:cancel` |
| P8 | 发送记录 tab | `record:list` |
| P9 | 通道查看 / 编辑 / 设置 | `channel:list` / `channel:update` / `setting:update` |
---

# 九、验收场景

> 6 个场景，**每一步都有可判定的结果**（数字 / 文案 / 状态），test-enginner 可直接逐条转用例。
> 每个场景前置条件统一：① 已按第六章写入 41 个权限码与 5 个菜单；② 有一个超管账号 `admin`；③ 已执行 3.9 的初始数据迁移（一条本地默认存储 + 8 条上传策略）。

---

## 场景 1【存储切换】默认存储从本地切到 MinIO，旧文件仍能下载

**目标**：证明"切换默认存储"只影响新文件，历史文件不丢。

| 步 | 操作 | 预期结果（判定点） |
|---|---|---|
| 1 | 准备：先确认当前有历史文件。以 `admin` 登录 → 文件管理，记下**文件总数**（假设 **12,486** 条）与任意一个 2025 年上传的文件 A 的 ID | 列表显示「共 12,486 条」 |
| 2 | 点 A 的「下载」 | 200，文件正常下载，大小与列表显示一致 |
| 3 | 进 `存储管理` → 新增配置：名称「MinIO 生产」/ 类型 MinIO / 端点 `http://minio.corp.local:9000` / 桶 `cloudpivot` / AK·SK 正确值 → 点**「测试连接」** | 绿条：「连接成功 · 桶 cloudpivot 已存在 · 延迟 **<100ms**」 |
| 4 | 点「保存」 | 列表新增一行，类型「MinIO」，密钥列显示掩码（形如 `AKIA****7ZP2`），连通性「正常 xxms」，**默认标签仍在「本地默认」那行** |
| 5 | 点「MinIO 生产」行的「设为默认」 | 确认框文案必须包含「**只影响之后上传的新文件**，已有 12,486 个文件仍从各自原存储读取」 |
| 6 | 确定 | ① 刷新列表：**「默认」标签移到 MinIO 行**，本地行默认标签消失；② `audit_logs` 新增一条 `STORAGE_BACKEND_DEFAULT`，before/after 各记一个存储名称；③ `sys_storage_backend` 中 `is_default=true` 的行**有且仅有 1 行** |
| 7 | 回文件管理，上传一个新文件 B（`test-minio.png`，1.2MB） | 上传成功；详情抽屉里「存储后端」显示 **MinIO 生产** |
| 8 | 去 MinIO 控制台/`mc ls` 查桶 `cloudpivot` | 存在对象 `image/2026/10/28/{uuid}.png`，大小 1.2MB（**证明真的落 MinIO，不是本地盘**；注意 key **不含** bucket 名重复的 `cloudpivot/` 前缀，规则见 3.2.3） |
| 9 | 回到文件管理，下载**老文件 A** | **200，正常下载**（关键判定：旧文件仍可访问） |
| 10 | 再下载**新文件 B** | 200，正常下载（新文件从 MinIO 读回） |
| 11 | 检查文件总数 | **12,487**（12,486 + 1），历史记录数量未变 |
| 12 | 反例校验：尝试「停用」MinIO 存储（它是默认） | 拒绝，提示「默认存储不能停用，请先把其它存储设为默认」（E-STG-10） |

**失败判据**（任一出现即不通过）：切换后老文件 404 / 步骤 7 后对象出现在本地 `uploads` 目录 / 默认标签出现 2 个 / 老文件的 `storage_id` 被批量回填成 MinIO 的 id。

---

## 场景 2【大文件直传】500MB 文件浏览器直传对象存储，后端不占内存，进度可见

**目标**：证明"浏览器 → 对象存储"绕过后端，且后端内存不随文件大小增长。

| 步 | 操作 | 预期结果（判定点） |
|---|---|---|
| 1 | 前置：默认存储为 MinIO 类（场景 1 完成后即有）；确认 `video` 分类策略 `max_size_mb ≥ 2048`、`direct_upload=true` | 上传策略页 `video` 行显示 2048MB、允许直传=开 |
| 2 | 准备一个 **500MB** 的 `test-500m.mp4`（可用 `dd` 生成后改扩展名，但要注意 MIME 校验会拦——所以用真实 mp4 或临时关闭 `video` 的 MIME 校验） | 文件存在，`du -h` 显示 500M |
| 3 | 记录后端进程 RSS 基线（如 `120MB`） | 记下数值 |
| 4 | 文件管理点「上传」，选该文件，分类选「音视频」 | 前端先调 presign，拿到 PUT URL；**浏览器开发者工具 Network 里能看到一个 `PUT` 请求直发 `minio.corp.local:9000`**（关键判定：目标域名不是云枢后端域名） |
| 5 | 上传过程中观察 | ① 进度条实时变化，显示「正在上传 187MB / 500MB（37%）」；② 进度**单调递增**，不回退 |
| 6 | 上传过程中观察后端 RSS | **峰值增长 ≤ 50MB**（关键判定：后端不占内存）。若 RSS 涨到 500MB+ 说明走了后端中转，不通过 |
| 7 | 上传完成 | 自动调 confirm → 文件出现在列表；详情「存储后端」= MinIO；大小 = 524,288,000 字节 |
| 8 | 去 MinIO 查对象 | 对象存在且大小为 500MB（`mc stat` 的 Size 一致） |
| 9 | 检查 `sys_file` 该行 | `upload_status = 1`、`storage_id` 非空、`is_private` 按上传时选择 |
| 10 | 反例 1：上传一个 600MB 的文件但分类是「通用附件」（上限 20MB） | 立即失败（**不进入上传**）：「文件大小超过上限 20MB（当前 600MB）」（E-STG-12） |
| 11 | 反例 2：直传中途取消（关标签页），等 2 小时后跑清理 | 该 `upload_status=0` 的记录被软删，MinIO 里对应对象被删除，`audit_logs` 有 `FILE_UPLOAD_ABORT` |
| 12 | 反例 3：把默认存储切成「本地」再传 100MB 文件 | 提示降级「当前为本地存储，将使用普通上传……」；且因 nginx `client_max_body_size=50m`，**预期失败并给出可理解的错误**——这条要在测试报告里显式记录为"已知限制 + 已在附录 C 列出交付前提" |

**必须记录的数据**：上传总耗时、后端 RSS 峰值、MinIO 侧对象大小、PUT 请求的目标域名。四个数缺一不可。

---

## 场景 3【导入失败回显】导入 5000 行物料，3 行必填缺失 + 2 行编码重复

**目标**：证明校验能精确定位到"第几行第几列为什么错"，且用户能改完重传成功。

| 步 | 操作 | 预期结果（判定点） |
|---|---|---|
| 1 | 以业务操作员登录 → `导入导出` → 「新建导入」→ 业务选「物料」 | 标题变「导入物料」；显示模板下载卡，灰字「含 12 个字段，5 个必填，最多 100,000 行」 |
| 2 | 点「下载模板」 | 得到 `物料导入模板_v1.xlsx`；打开确认：**3 个 sheet**（数据 / 字段说明 / 填写须知）；第 1 行表头必填列带 `*`；第 2 行是灰底示例；枚举列（计量单位）有下拉验证 |
| 3 | 构造测试文件：**5000 行**数据，其中：第 137 行「物料名称」留空、第 892 行「计量单位」留空、第 2011 行「规格型号」留空、第 3045 行与 4470 行的「物料编码」都填 `MAT-000123`（且库里不存在该编码） | 文件就绪（5000 行 + 表头 + 示例行） |
| 4 | 上传该文件，重复策略保持默认「跳过」，点「开始校验」 | 转第 2 步；**4 秒内**出现校验报告 |
| 5 | 读三块数字卡 | **总计 5,000 / 可导入 4,995 / 失败 5**（三个数必须完全一致，这是核心判定） |
| 6 | 读失败明细表 | **恰好 5 行**，且内容为：<br>行 137 / 物料名称 / （空）/ 必填项为空<br>行 892 / 计量单位 / （空）/ 必填项为空<br>行 2011 / 规格型号 / （空）/ 必填项为空<br>行 3045 / 物料编码 / MAT-000123 / 编码已存在（与第 88 行重复）<br>行 4470 / 物料编码 / MAT-000123 / 编码已存在（与第 88 行重复） |
| 7 | **行号正确性判定** | 行号必须是 Excel 真实行号：137/892/2011/3045/4470。**不是** 136/891/…（差 1 即不通过），也不是数据序号 |
| 8 | 补充说明条 | 显示「其中 2 行因编码已存在于文件中被跳过」类文案；示例行被自动跳过并注明「已自动跳过 1 行示例数据」 |
| 9 | 点「下载错误明细」 | 得到 `物料_导入失败明细_*.xlsx`；**恰好 5 行数据 + 表头**；列 = 行号 + 全部原始业务列 + 失败原因；**打开后可以直接改这 5 行**（列齐全，不是只有错误列） |
| 10 | 点「确认导入」 | 因为默认策略是"全量事务"，此时按钮 **disabled**，旁边红字「存在 5 行校验失败，请修正后重新上传，或开启"允许部分导入"」（E-IMP-19 语义） |
| 11 | 打开错误明细文件改数据：补上 3 个空值；把 4470 行的编码改成 `MAT-000124`；把 3045 行的编码改成 `MAT-000125`（避开与第 88 行重复）→ 保存 | 5 行数据修正完毕 |
| 12 | 上一步：把修正后的 5 行文件重新上传（只含 5 行数据） | 校验报告：**总计 5 / 可导入 5 / 失败 0** |
| 13 | 点「确认导入」 | 确认框「将导入 **5** 行到「物料」…」；确定后进度条到 100%，转第 3 步：「导入成功：5,000 行全部写入」→ 此处应为「**5** 行」 |
| 14 | 校验库 | `SELECT count(*)` 该批次导入的物料 = 5；编码 MAT-000124、MAT-000125 存在；MAT-000123 不存在 |
| 15 | 回「导入记录」tab | 有 2 条记录：第 1 条 `5000 / 成功 0 / 失败 5 / 状态 失败`，第 2 条 `5 / 成功 5 / 失败 0 / 状态 成功`；两条都能下载错误明细/查看报告 |
| 16 | 反例：勾选「允许部分导入」后重新上传最初那个 5000 行文件 | 报告「失败 5」但「确认导入」**可点**；确认后状态 = **部分成功**；报告写「成功 4,995 / 失败 5（已跳过失败行）」；库里实际新增 4,995 条 |

**性能判定**：步骤 4 从点「开始校验」到出报告 ≤ **8 秒**（5000 行）；步骤 13 入库 ≤ **5 秒**。

---

## 场景 4【异步导出】导出 20 万行订单，进度 0→100%，完成后收到通知，链接 24h 有效

| 步 | 操作 | 预期结果（判定点） |
|---|---|---|
| 1 | 准备：订单表造 **203,481** 行数据（含明细关联） | `SELECT count(*)` = 203,481 |
| 2 | 在订单列表点「导出」 | 弹字段勾选框：列出全部可导字段，「基本信息/明细信息」两组；默认勾选 8~12 个；敏感字段带「敏感」标签 |
| 3 | 勾选 8 个字段（订单号、日期、供应商、物料、数量、单价、金额、状态），点确定 | 提示条：**「数据量较大（203,481 行），已转为后台导出，完成后通知您」**（E-EXP-01） |
| 4 | 自动跳转/被引导到 `导入导出 → 导出任务` | 新增一行：任务名 `导出订单_YYYYMMDDHHmmss.xlsx`，行数 203,481，状态「排队中」或已「执行中」 |
| 5 | 观察进度 | ① 进度条**单调递增**，不回退不跳动；② 阶段文案经历「正在查询 → 正在生成文件」；③ 每 3 秒刷新一次（打开 Network 面板可见轮询请求） |
| 6 | 记录后端耗时 | 端到端落在 **1.5–5 分钟**区间（4.6.2 算例 1.5–2.5 分钟；含明细 join 实际 2–5 分钟）；**超过 10 分钟必须判失败**（E-EXP-07） |
| 7 | 完成后检查顶栏铃铛 | **未读数 +1**，下拉面板第一条是「您的导出任务已完成，点击下载」，类型标签「业务通知」 |
| 8 | 检查任务行 | 状态「已完成 · X 分 Y 秒」；进度 100%；「下载」按钮可点；hover 显示「有效期至 明天 18:40」 |
| 9 | 点「下载」 | 开始下载 `导出订单_*.xlsx`；文件大小约 **18–20MB**；打开后：列 = 所勾 8 个字段且**顺序一致**；首行冻结；**订单号是文本格式**（不是 `2.026E+17`）；金额列有千分位两位小数；行数 = 203,482（含表头） |
| 10 | 校验没有多余数据 | 与 `count(*)` 一致（203,481 + 1 表头） |
| 11 | 检查 `audit_logs` | 有 `IMPEXP_EXPORT_CREATE`，after 含 `{task_id, biz_code, fields, row_count_estimate}` |
| 12 | **链接有效期判定**：把该任务的 `result_expires_at` 手动改成过去时间 → 刷新列表 | 「下载」按钮变灰「已过期」，tooltip 说明产物保留 7 天、链接 24h；点「重新导出」可重跑（E-EXP-05） |
| 13 | 反例 1：同步导出（筛出 8,000 行） | 直接下载，无任务；前端该请求 timeout 为 180s（不是全局 15s）；耗时 < 60s |
| 14 | 反例 2：导出 15,000 行但让后端处理超过 60 秒（可人为 sleep） | 后端 60s 未完成 → 返回 `mode=async` + 弹「数据量较大，已转为后台导出」；任务列表出现该任务（D4 的自动降级） |
| 15 | 反例 3：同一账号连开 4 个导出 | 第 4 个被拒：「您已有 3 个导出任务在进行中，请等待完成后再提交」（E-EXP-09） |
| 16 | 反例 4：无 `export:sensitive` 权限的账号导出含手机号的业务 | 提示「您没有"导出敏感字段明文"权限，本次已按脱敏规则导出」（E-EXP-03）；文件内是 `138****5678`；`audit_logs` 无 `IMPEXP_EXPORT_SENSITIVE` |

---

## 场景 5【群发通知】给「财务部及下级」（120 人）发通知，全部收到、未读数正确、已读各自独立

| 步 | 操作 | 预期结果（判定点） |
|---|---|---|
| 1 | 准备：财务部组织结构 —— 直属 **34** 人；下级 3 个部门共 **86** 人；其中 **2** 人同时在主部门与下级部门（导致重复）；另有 **3** 个停用账号、**1** 个已删除账号在这棵树里 | `sys_user` 数据就绪；理论去重人数 = 34 + 86 - 2 = **118**，再排除停用 3 + 删除 1（若这 4 人在上述集合内）→ 以实际可投递数为准。**本场景以最终 `planned_count = 120` 为判定基准**（组织数据需构造到该值） |
| 2 | 以通知管理员登录 → `通知管理` → 「发送通知」 | 表单打开，四个目标单选项 |
| 3 | 内容：标题「月度结账提醒（11 月）」，正文选模板「月度结账提醒」，模板含 `{{userName}}` 与 `{{deadline}}`；填 `deadline = 2026-10-31 18:00` | 正文自动填充；`{{userName}}` 显示灰字「按收件人自动填充」；`deadline` 出现日期控件 |
| 4 | 目标选「部门」→ 树里勾 `财务部` → 打开「包含下级部门」 | 底部人数实时变为 **120**（防抖后） |
| 5 | 点「展开明细」 | 显示：财务部直属 34 + 会计核算组 52 + 资金组 21 + 税务组 13 = 120（去重 2 人）；并列出「将排除：停用账号 3 人」等 |
| 6 | 发送时间选「立即」，点「发送」 | 确认框：「即将向 **120** 人发送通知，**不可撤回**。标题：月度结账提醒（11 月）。是否继续？」 |
| 7 | 确定 | ① ≤3 秒内提示「已发送 120 人（失败 0）」；② 自动跳「发送记录」，新批次高亮，状态「已发送」，实发 120 / 失败 0 |
| 8 | 校验 `sys_notice_receiver` | 新增 **120** 行，`receiver_id` 互不相同（去重生效），`read_at` 全为 NULL |
| 9 | 校验 `sys_notice_batch` | `planned_count=120`、`sent_count=120`、`target_type=dept`、`target_snapshot` 含 `{dept_ids:[财务部id], include_children:true}` |
| 10 | 用收件人 A（普通用户）登录 | 顶栏铃铛角标 **1**；点开面板看到该条；正文渲染为「您好 **A的姓名**，……截止时间 **2026-10-31 18:00**」 |
| 11 | 用收件人 B 登录 | 顶栏铃铛角标 **1** |
| 12 | A 点开该消息（置已读） | A 的未读数 1 → **0**；A 的消息进「已读」tab |
| 13 | **B 刷新页面（或等 60 秒轮询）** | **B 的未读数仍为 1**（关键判定：已读状态各自独立） |
| 14 | B 点「全部已读」 | 确认框「将把 **1** 条未读消息标记为已读？」→ B 未读 0 |
| 15 | 检查发送记录的阅读统计 | 「已读 2 / 未读 118」（随步骤 12/14 变化），**且不显示"谁读了"的名单**（附录 D） |
| 16 | 反例 1：内容留空点发送 | 校验红字，按钮 disabled |
| 17 | 反例 2：定时时间选「现在之前」 | 日期控件直接禁止选择（最早 = 当前 + 5 分钟） |
| 18 | 反例 3：无 `send:group` 权限的账号打开发送页 | 「部门」「全员」两个单选项 **disabled** + tooltip「需「群发通知」权限」；直接调接口返回 403 |
| 19 | 反例 4：指定用户选了 501 人 | 第 501 个加不进去，提示「指定用户发送上限 500 人，如需更大范围请选"部门"或"全员"」（E-NTC-05） |

---

## 场景 6【模板变量】模板 `您好 {{userName}}，单据 {{docNo}} 已通过` 渲染正确

| 步 | 操作 | 预期结果（判定点） |
|---|---|---|
| 1 | 通知管理 → 新增模板：编码 `approval_passed`，名称「审批通过通知」，类型「业务通知」，业务标识 `erp.purchase_order` | — |
| 2 | 标题填 `单据 {{docNo}} 审批结果` | 右侧预览实时变「单据 CG-202610-0001 审批结果」（示例值） |
| 3 | 正文填 `您好 {{userName}}，单据 {{docNo}} 已通过` | 预览区显示 **`您好 张三，单据 CG-202610-0001 已通过`** |
| 4 | 检查变量面板 | 12 个变量可点；点 `{{userName}}` 插入到光标处；hover 显示「收件人姓名 · 示例：张三」 |
| 5 | **故意写错变量**：正文改成 `您好 {{userNmae}}，单据 {{docNo}} 已通过` | 正文下方**实时红字**：「变量 `{{userNmae}}` 未定义（是否想用 `{{userName}}`？）」；`[保存]` **disabled** |
| 6 | 改成 `{{ userName }}`（含空格） | 提示「变量名不能含空格，请写作 `{{userName}}`」 |
| 7 | 改回正确写法并保存 | `MessagePlugin.success('模板已保存')`；列表新增一行，变量数显示「2 个」 |
| 8 | 点「测试发送」 | 确认框说明「发给**你自己**，带「[测试]」标记，24 小时后自动清除」→ 确定 → 「测试消息已发送」 |
| 9 | 看自己的铃铛 | 未读 +1；标题为 **`[测试] 单据 CG-202610-0001 审批结果`**；消息有「测试」标签 |
| 10 | 打开该测试消息 | 正文 = `您好 张三，单据 CG-202610-0001 已通过`（**用的就是示例值**，因为 12 个变量里按人变量在测试时用 sample） |
| 11 | **真实发送校验**：用场景 5 的方式给「指定用户 = 李四」发一条该模板的通知，`docNo` 填 `CG-202610-0077` | 李四收到：标题 `单据 CG-202610-0077 审批结果`，正文 **`您好 李四，单据 CG-202610-0077 已通过`**（`userName` 按收件人真实填充，不是张三） |
| 12 | **缺失变量校验**：再发一条但不填 `docNo` | 正文渲染为 **`您好 王五，单据 — 已通过`**（缺失变量替换为长破折号，**绝不出现 `{{docNo}}` 原文**） |
| 13 | 检查 `sys_notice_message` 该行 | `title` / `content` 存的是**渲染后的最终文本**（不是模板）；改模板后再看历史消息，内容不变 |
| 14 | 反例：删除被引用的模板 | 「该模板已被 **3** 个发送批次使用，无法删除。可将状态改为「停用」」（E-NTC-09） |

---

## 9.7 验收总表（给 test-enginner 的勾选清单）

| 场景 | 关键判定点 | 必须记录的数据 |
|---|---|---|
| 1 存储切换 | 老文件可下载、新文件落 MinIO、默认唯一 | 文件总数前后、MiNIO 对象路径、审计条数 |
| 2 大文件直传 | PUT 目标域名 = 对象存储、后端 RSS 峰值 ≤ +50MB | 上传耗时、RSS 峰值、对象大小 |
| 3 导入失败回显 | 5000/4995/5、5 条明细行号精确、明细可改可重传 | 校验耗时、入库耗时、明细文件行数 |
| 4 异步导出 | 进度单调、通知到达、链接 24h、订单号为文本 | 端到端耗时、文件大小、行数 |
| 5 群发通知 | 120 行 receiver、去重生效、A 已读 B 未读 | 展开明细人数、实发数、阅读统计 |
| 6 模板变量 | 3 种渲染结果（正常/缺失/按人不同） | 渲染前后文本对照 |

---

# 十、后续规划

本批交付的是"通用底座"——对象存储、导入导出、消息通知这三件事，往下所有业务模块都要用，所以先把它们做成**配置驱动、接口稳定**的公共服务（导入导出靠定义表扩展业务、通知靠模板与调用契约扩展场景），后续 B 批次权限增强会直接复用本批的 41 个权限码与三层数据权限骨架，C 批次内容管理会把图片/视频/附件的上传下载全部挂到本批的存储路由与上传策略上（不再自建文件表），D 批次工作流审批 C 档则通过附录 E 的"发通知契约"与"业务通知类型 + `business_key` + 幂等键"接入消息中心，**后续批次如需新增存储类型、通知通道、导入业务，只加配置与实现，不改本批的表结构与接口路径**。

---

# 附录 A 错误与提示文案总表

> 全部为**用户能看懂的中文**，不出现错误码、堆栈、英文异常名。后端 `ApiError::new(code, error_code, "文案")` 的第三个参数就用这里的文案。
> 表格中"界面位置"指文案出现处：**表单**（字段下方红字）、**Toast**（2 秒消息）、**Dialog**（弹框）、**Page**（页面内提示条）。
> **共 67 条**：A.1 对象存储 15 条 + A.2 导入 20 条（完整文案表见 4.2.3，此处只补"界面位置"）+ A.3 导出 10 条（同上）+ A.4 消息通知 15 条 + A.5 通用 7 条。编号全局唯一，前端与后端都按编号索引同一份文案。

## A.1 对象存储（E-STG）

| 编号 | 文案 | 界面位置 | 触发 |
|---|---|---|---|
| E-STG-01 | 连接成功 · 桶 `{bucket}` 已存在 · 延迟 {n}ms | Page（绿） | 连通性测试成功 |
| E-STG-02 | 连接失败：AK/SK 无效或无权限（服务端返回 AccessDenied）。请检查密钥是否过期、是否有该桶的读写权限 | Page（红） | 鉴权失败 |
| E-STG-03 | 连接失败：无法访问 `{endpoint}`（连接超时 10 秒）。请检查地址、端口、防火墙与内网互通 | Page（红） | 网络不可达 |
| E-STG-04 | 连接失败：端点风格不匹配。若使用 MinIO，请确认端点地址包含 `minio` 或使用 http（系统按"端点含 minio 或关闭 TLS"选择 Path-Style 访问） | Page（红） | path-style 判定失败 |
| E-STG-05 | 连接失败：Region 不匹配（实际区域可能是 `{guess}`）。阿里 OSS 请填写 `oss-cn-hangzhou` 这类区域值 | Page（红） | 区域不匹配 |
| E-STG-06 | 文件内容与扩展名不符（扩展名 `{ext}`，实际类型 `{mime}`），已拒绝上传 | Dialog | MIME 魔数校验失败 |
| E-STG-07 | 端点需以 `http://` 或 `https://` 开头 | 表单 | 端点格式错 |
| E-STG-08 | 提示：阿里 OSS / S3 兼容存储使用本类型即可。若访问异常，请确认端点与 TLS 配置（Path-Style 判定规则见上条） | Page（蓝，info） | 选「阿里 OSS」/「S3 兼容」时 |
| E-STG-09 | 该存储上还有 **{n}** 个文件，无法删除。请先迁移或删除这些文件，或将其它存储设为默认后再试 | Dialog | 删除被引用配置 |
| E-STG-10 | 默认存储不能停用，请先把其它存储设为默认 | Toast | 停用默认配置 |
| E-STG-11 | 未配置参数加密密钥（环境变量 X_CONFIG_SECRET_KEY），无法加密存储密钥 | Dialog | 密钥缺失时保存 AK/SK |
| E-STG-12 | 文件大小超过上限 {max}MB（当前 {actual}） | Toast | 超限 |
| E-STG-13 | 已自动跳过 1 行示例数据 | Page（灰） | 导入时识别到示例行 |
| E-STG-14 | 该文件为私有文件，请通过限时链接下载 | Toast/Dialog | 私有文件走普通下载接口 |
| E-STG-15 | 链接无效或已过期 | Page（红） | 短链校验失败/过期 |

## A.2 导入（E-IMP，20 条见 4.2.3，此处给"界面位置"补充）

| 编号 | 界面位置 | 备注 |
|---|---|---|
| E-IMP-01 ~ E-IMP-07、E-IMP-18 ~ E-IMP-20 | Page（第 1 步上传区下方 / 第 2 步顶部） | 整表级问题，不进入明细表 |
| E-IMP-08 ~ E-IMP-17 | 表格行（失败明细表的「失败原因」列） | 行级问题，**同一行可能有多个原因时用「；」拼接**，最多列 3 条 |

## A.3 导出（E-EXP，10 条见 4.2.3）

| 编号 | 界面位置 |
|---|---|
| E-EXP-01 / E-EXP-03 | Toast + Page |
| E-EXP-02 | 表单（字段勾选区下方） |
| E-EXP-04 ~ E-EXP-06 | Toast / 行内灰字 |
| E-EXP-07 ~ E-EXP-10 | Dialog / Toast |

## A.4 消息通知（E-NTC）

| 编号 | 文案 | 界面位置 |
|---|---|---|
| E-NTC-01 | 变量 `{{ {name} }}` 未定义（是否想用 `{{ {suggest} }}`？） | 表单（正文下方） |
| E-NTC-02 | 变量名不能含空格，请写作 `{{userName}}` | 表单 |
| E-NTC-03 | 标题不能超过 200 个字符（当前 {n}） | 表单 |
| E-NTC-04 | 正文不能超过 5000 个字符（当前 {n}） | 表单 |
| E-NTC-05 | 指定用户发送上限 500 人，如需更大范围请选「部门」或「全员」 | Toast |
| E-NTC-06 | 发送对象为空，请至少选择 1 个用户 / 角色 / 部门 | 表单 |
| E-NTC-07 | 定时发送时间必须晚于当前时间 5 分钟以上 | 表单 |
| E-NTC-08 | 您没有「群发通知」权限，无法向部门或全员发送 | Toast |
| E-NTC-09 | 该模板已被 **{n}** 个发送批次使用，无法删除。可将状态改为「停用」 | Dialog |
| E-NTC-10 | 站内信为系统基础通道，不可关闭 | Tooltip |
| E-NTC-11 | 当前版本仅记录配置，尚未实现发送，配置将在下版本生效 | Page（黄，警告） |
| E-NTC-12 | 即将向 **{n}** 人发送通知，不可撤回。标题：{title}。是否继续？ | Dialog |
| E-NTC-13 | 已发送 {n} 人（失败 {m}） | Toast |
| E-NTC-14 | 消息不存在或已被删除 | Toast |
| E-NTC-15 | 定时发送已错过原定时间（原定 {t1}），已于 {t2} 补发 | Page（灰，批次备注） |

## A.5 通用（E-CMN）

| 编号 | 文案 | 界面位置 |
|---|---|---|
| E-CMN-01 | 您没有该操作的权限，请联系管理员 | Toast（后端 403） |
| E-CMN-02 | 参数不合法：{detail} | Toast（400） |
| E-CMN-03 | 数据不存在或已被删除 | Toast（404） |
| E-CMN-04 | 服务器内部错误，请稍后重试或联系管理员 | Toast（500，**不暴露细节**） |
| E-CMN-05 | 操作太频繁，请稍后再试 | Toast（429） |
| E-CMN-06 | 确定删除选中的 **{n}** 项？删除后不可恢复。 | Dialog |
| E-CMN-07 | 确定删除「{name}」？删除后不可恢复。 | Dialog |

---

# 附录 B 需要老大拍的 8 个决策

| # | 决策 | 本文的建议 | 备选与代价 |
|---|---|---|---|
| **D-B1** | 「22 套可切换配色」的具体口径 | **按现有实现口径落地**：`frontend/src/style/palettes.ts` 当前是 **6 套色板 × 明暗双模式 = 12 组**（默认「雾蓝流金」`#6F8AB8`）。本文所有页面**只用 CSS 变量**（8.0 约定第 2 条），所以如果"22 套"是指 11 套 ×2 模式，**前端零改动**、只需扩 `palettes.ts` | 若"22 套"是指"22 个独立主题（不分明暗）"，需要重新定义色阶推导算法；建议先确认口径，避免前端按 12 组做完又要返工 |
| **D-B2** | S3 兼容 / 阿里 OSS 是独立类型还是 MinIO 类型的预设（D2） | **预设**（P0 只有 `local` + `minio` 两种实现）。省 2 个空壳后端类，且 `aws-sdk-s3` 原生兼容 | 独立类型需自研 3 个 `StorageBackend` 实现，收益仅是"列表里类型名不同" |
| **D-B3** | 导入"全量事务"的回滚机制 | **建 `sys_impexp_task_row` 映射表**（不侵入业务表） | 目标表加 `import_task_id` 列：快，但要改每一张业务表，不可持续 |
| **D-B4** | 大文件直传要框架补 `presign_upload`（D3） | **P0 就补**（1 个 trait 方法 + 1 个确认接口，`aws-sdk-s3` 原生支持 PUT 预签名） | 不补则 500MB 验收无法通过（nginx 50m + 后端全量内存两重障碍） |
| **D-B5** | 导出耗时没有硬性秒数指标 | **不作为验收硬指标**，改为"进度单调递增 + 最终 100% + 通知必达" | 若老大要求"20 万行必须 60 秒内"，需要建导出物化表/预聚合，工作量翻倍 |
| **D-B6** | 41 个新权限码要不要自动塞进现有角色 | **不自动塞**。迁移只写权限点与新角色包，线上已有角色用「一键补齐」按钮手动加 | 自动塞会让线上某个人突然多出"群发全员""导出明文"权限，属安全事故 |
| **D-B7** | 未读角标用轮询 60s（D7） | **P0 轮询**（120 人 = 2 QPS，可忽略），P1 上 SSE | 直接上 SSE 需要开 feature + 长连接运维 + 断线重连，P0 收益不抵成本 |
| **D-B8** | 消息中心是否给"谁读了谁没读"的名单 | **不给**（只给聚合数），避免变成"已读监控"工具引发员工反感 | 给了能满足"催办"，但会带来隐私争议；如确需，建议放到 D 批次审批流做"催办"按钮（按未读名单群发催办，不展示名单） |

---

# 附录 C 非功能需求与交付前提

## C.1 交付前提（7 项，缺一项就有 P0 做不成）

| # | 前提 | 为什么必须 | 责任方 |
|---|---|---|---|
| 1 | `backend/Cargo.toml` 增加 `xlsx` feature（F24 现未开） | 不开则 `Export::to_xlsx_*` / `Import::from_xlsx_*` **根本不存在**，导入导出无从谈起 | backend-developer |
| 2 | 环境变量 **`X_CONFIG_SECRET_KEY`** 必须在生产/测试环境配置（F22） | 未配置则 AK/SK 与通道密码**无法加密存储**（E-STG-11），存储配置功能不可用 | 运维 |
| 3 | 对象存储桶需配置 **CORS**：允许 `PUT`/`GET`，`AllowedOrigin` = 云枢前端域名，`AllowedHeader: content-type`，`ExposeHeader: ETag` | 不做则浏览器直传在预检阶段就失败（3.5.3 提醒） | 运维 |
| 4 | 若坚持用本地后端接大文件：**nginx `client_max_body_size` 从 50m 调到 2048m**（F28） | 否则 >50MB 上传必然 413 | 运维 |
| 5 | 新增 2 个 cron 任务注册：`impexp_dispatch`（每分钟）、`notice_cleanup`（每日 03:50）（F29） | 异步导出/定时发送/清理全靠它们；**注册需走 `Scheduler::reload`（stop→注册→start）** | backend-developer |
| 6 | 迁移顺序：① `sys_storage_backend` + 初始本地配置 → ② `sys_storage_policy` + 8 条策略 → ③ `ALTER sys_file` 加 3 列 → ④ 5 张通知表 → ⑤ 3 张 impexp 表（含 `task_row`） → ⑥ `sys_menu` 41 个权限码 + 5 菜单 → ⑦ 字典 `storage_category` + `sys_config` 若干键 | 顺序错会导致"菜单已上线但表不存在"的白屏 | architect |
| 7 | `sys_menu` 模块筛选下拉补 `business` 选项（7.3 第 1 条） | 否则管理员筛不到本批菜单 | frontend-developer |

## C.2 运维说明（上线时随公告发出）

1. **切换默认存储后不要删本地 `uploads` 目录**，也不要做数据搬迁。等确认所有历史文件可下载后，再按归档流程处理（3.9 第 6 条）。
2. 若 MinIO 后端的端点写成 **IP + https**，会因 Path-Style 判定走虚拟主机风格而失败——改成域名含 `minio` 或使用 http（F20 / E-STG-04）。
3. 每日 **03:00–03:50** 是本系统的清理窗口（5 个日志 + 直传残留 + 消息 + 导出产物），此期间避免跑大导出。
4. 存储配置里的 **AK/SK 更换后立即生效**，旧密钥在云控制台删除前请先点一次「测试连接」。

## C.3 目录结构建议（给 backend-developer）

```
backend/src/business/
├── storage/
│   ├── router.rs      # StorageRouter：配置 id → 后端实例，读写路由（3.2）
│   ├── backend/       # 存储配置 CRUD + 连通性测试
│   ├── policy/        # 上传策略
│   └── file/          # 文件管理增强（预览/批量/私有链接）+ 直传 presign/confirm
├── impexp/
│   ├── definition/    # 导入导出定义
│   ├── template/      # 模板生成（Export::to_xlsx_*）
│   ├── import/        # 解析/校验/入库
│   └── export/        # 同步/异步导出
└── notice/
    ├── template/      # 模板 CRUD + 渲染
    ├── send/          # 发送（扇出/定时）
    ├── message/       # 收件箱/已读/未读
    └── channel/       # 通道配置
```
（`backend/src/` 下现无 `business` 目录，F11；`task/` 里新增 2 个 handler 注册到 `Scheduler::reload`。）

## C.4 性能与容量基线

| 项 | 目标 | 依据 |
|---|---|---|
| 文件元数据表 | 100 万行不需分表 | 3.10 |
| 未读数查询 | < 2ms（部分索引） | 5.3.2 |
| 20 万行导出 | 1.5–5 分钟，进度单调 | 4.6.2 |
| 10 万行导入校验 | ≈ 14s（异步） | 4.5.2 |
| 5000 行导入校验 | ≤ 8s（同步） | 场景 3 |
| 120 人通知投递 | ≤ 3s | 5.4.3 |
| 500MB 直传时后端内存增长 | ≤ 50MB | 3.10 / 场景 2 |
| 消息表规模 | 1000 万行 receiver 仍可（部分索引 + 分批清理） | 5.8 |

---

# 附录 D 本文明确不做的事（12 条，防评审扯皮）

| # | 不做 | 一句理由 |
|---|---|---|
| 1 | 富文本/HTML 通知正文 | P0 纯文本，避免 XSS 面与"邮件样式不兼容"；需要排版用模板换行与级别色 |
| 2 | 通知撤回（已投递的） | 已读即已读，撤回需要消息版本概念 |
| 3 | "谁读了谁没读"名单 | 见 D-B8，隐私争议；只给聚合数 |
| 4 | 邮件 / Webhook 真正发送 | 本批只要配置与开关（5.7）；实现放下一批 |
| 5 | 视频在线预览 | 需要 Range 请求支持，本地后端现在没有（3.8.3） |
| 6 | SVG 在线预览 | 可内嵌脚本，安全风险（3.4.3） |
| 7 | 文件回收站 / 打包下载 | P1（S5-05 / S5-06） |
| 8 | 一次性链接 / 提取码 | P1（S4-05） |
| 9 | 分片断点续传（跨会话） | P1；本批只有"分块顺序上传"（3.5.5），不做断点记忆 |
| 10 | 导出带公式/合并单元格/图表 | 属报表设计器范畴（反场景 A3） |
| 11 | 双存储并行（部门分存储） | P1，需自研按 id 路由的 registry（反场景 A1） |
| 12 | 导入的"边传边看进度" | calamine 不支持流式增量解析（反场景 A2） |

---

# 附录 E 本批对外提供的内部调用契约（供 B/C/D 批次）

> 这三个契约是**本批的交付物**，本批负责实现并保证稳定；**后续批次只调用，不改**。契约的字段一旦发布即视为兼容承诺。

## E.1 发通知契约（供 D 批次审批流调用）

`POST /business/notice/message/send`（内部调用，权限码 `business:notice:send:create`）

| 入参 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `notice_type` | string | ✅ | 固定 `business` |
| `business_key` | string | ✅ | 业务标识，如 `erp.purchase_order` |
| `biz_ref_id` | string | | 业务单据 ID，用于跳转与幂等 |
| `template_code` | string | | 用模板渲染；与 `title`/`content` 二选一 |
| `title` / `content` | string | | 不用模板时直接给（**不支持变量**） |
| `vars` | object | | 变量取值，**仅接受字符串 KV**（不接受路径表达式） |
| `level` | string | | `info` / `warning` / `urgent` |
| `action_url` / `action_text` | string | | 跳转地址与按钮文案 |
| `receivers` | object | ✅ | `{users:[id], roles:[code], depts:[id], include_children:bool}`（**调用方一般只用 `users`**） |
| `idempotency_key` | string | | 幂等键（建议 `{business_key}:{biz_ref_id}:{event}`），相同键 24h 内重复调用返回首次结果，**不重复投递** |

**返回**：`{ batch_id, message_ids: [...], receiver_count }`

**D 批次使用示例（文字描述，非代码）**：采购订单 `CG-202610-0001` 进入审批 → 用模板 `approval_todo`、`vars={docNo:"CG-202610-0001", docType:"采购订单", amount:"1,234.00"}`、`receivers.users=[当前审批人的用户 id]`、`idempotency_key="erp.purchase_order:CG-202610-0001:submit"` 调本接口。

## E.2 导入导出注册契约（供 B/C 批次新增业务）

新增一个可导入/导出的业务 = **插一条 `sys_impexp_definition` 记录 + 实现两个后端函数**（`import_row_handler` / `export_query_provider`），**不需要改本批任何表结构或接口**。

| 契约点 | 说明 |
|---|---|
| `biz_code` | 全局唯一，建议 `{模块}.{实体}`，如 `cms.article` |
| `field_schema` | 字段定义（4.1.1），决定模板列、校验规则、导出列 |
| `unique_keys` | 判重字段，决定"跳过/覆盖"如何识别同一行 |
| 导入落库 | 由业务侧提供"单行校验后写入"的实现；本批负责解析、校验编排、报告、事务补偿 |
| 导出取数 | 由业务侧提供"按筛选条件分批取数"的实现；本批负责渲染 xlsx、脱敏、产物与链接 |

## E.3 文件附件契约（已存在，本批保证不变）

`POST /api/support/file/upload?category={cat}` → 返回 `FileRes`（含 `id` 与 `url`）。

- **本批承诺**：URL 不变、入参不变、`FileRes` 只增字段不删字段（3.2.4）。
- **C 批次注意**：内容管理的图片/视频请**直接复用本契约**并新增分类（`image` / `video` 已在 3.4.2 预置），**不要自建文件表**——否则会出现两套文件管理、两套存储路由、两套私有链接。

---

## 文档结束

| 项 | 值 |
|---|---|
| 总章数 | 10 章 + 5 附录 |
| 已核查代码事实 | 31 条（F1–F31） |
| 已定产品口径 | 11 条（D1–D11） |
| 功能项 | 80 项（P0 70 / P1 10） |
| 权限码 | 41 个 |
| 错误与提示文案 | 67 条（E-STG 15 / E-IMP 20 / E-EXP 10 / E-NTC 15 / E-CMN 7） |
| 验收场景 | 6 个（逐步骤可判定） |
| 需老大拍板 | 8 条（附录 B） |
