# AGENTS.md — 云枢通用后台系统（基础版 CloudPivot）

给 AI 编码 Agent 的项目说明。读完本文件即可安全动手；深入约定见 `skill/SKILL.md`、`docs/README.md`（二开文档集入口）与各分册。

## 项目是什么
基于 **Rust + Alun 框架（alun）+ Vue 3 + TDesign** 的通用后台底座（基础版）。提供用户 / 角色 / 菜单 / 字典 / 配置 / 文件 / 日志 / 在线用户 / 定时任务等通用后台能力。

> 专业版 `cloudpivot-pro`（数据权限 / 工作流 / 对象存储 / 导入导出）与企业版 `cloudpivot-saas`（多租户）是**独立仓库**，不在本目录。

## 技术栈
- 后端：Rust 1.94.1+ / edition 2024，框架 `alun`（门面 crate，能力靠 feature 点单）；PostgreSQL。
- 前端：`@cloudpivot/web` Vue 3.5 + TS + Vite 6 + TDesign Vue Next + Pinia + ECharts。
- 默认端口：后端 `8023`，前端 dev `5173`（代理 `/api`、`/health` → `:8023`）。
- 默认账号：`admin / Admin@123456`（SUPER_ADMIN 跳过权限校验）。

## 目录
```
backend/src/
  system/   support/      # 两个业务容器：每模块 mod+handler+dto+model（不设 service 层）
  middleware/ task/ utils/ shared/ bootstrap.rs
  lib.rs                    # 启动链 App::new()?...scan().start()
frontend/src/
  api/{system,support,business}/   views/{system,support,business}/
  router/index.ts  stores/  layouts/  components/
```

## 常用命令
```bash
# 后端（backend/）
cargo run                       # 自动迁移 + 启动 :8023
cargo check                     # 类型检查（零警告才提交）
cargo fmt && cargo clippy -- -D warnings

# 前端（frontend/）
pnpm install && pnpm dev        # :5173
pnpm typecheck                  # vue-tsc --noEmit

# 一键工程（项目根）
bash scripts/dev.sh             # 依赖容器 + 后端 + 前端
bash scripts/check.sh           # 提交前检查
bash scripts/build.sh           # 构建 release + 前端 dist
docker compose -f docker/docker-compose.yml up -d   # PG + Redis
```

## 必须遵守的约定
1. **后端每模块四件套** `mod.rs + handler.rs + dto.rs + model.rs`；业务编排在 `handler`，数据访问在 `model`，**不设 service 层**。
2. **二开业务放 `business/` 容器**（后端 `backend/src/business/<模块>/`、前端 `api/business/`、`views/business/<模块>/`），与 `system/`、`support/` 平级，**不侵入框架底座**。
3. **依赖铁律**：`Cargo.toml` 只声明 `alun` 一个框架 crate（`alun-utils` 必要时可加）。**不出现** `alun-db` / `alun-db-core` / `alun-web` / `alun-config`。业务代码统一 `use alun::prelude::*`。
4. **feature 语义**：默认集是 `core`（非 `full`）。用数据库必须显式 `postgres`/`mysql`/`sqlite`（`db` 只表能力、不含驱动）。预设 `mini ⊂ lite ⊂ full`；常用 `lite`（= core+postgres+cache+plugin+fs-local）。点单 feature：`fs-local`/`fs-minio`、`xlsx`、`sse`、`metrics`、`template`、`xss`。
5. **错误出口只有三个名字**：handler `ResResult<T>`、model `DbResult<T>`、启动/插件/任务 `alun::Result<T>`。handler 内 `DbError/io::Error/alun_core::Error` 直接 `?`，**不要**写多余 `.map_err(ApiError::internal(...))`。
6. **迁移号段**（动手前先 `ls backend/migrations/*/` 核对）：基础版最大 `V0017`；版本 4 位补零，已应用迁移指纹校验、不可改。
7. **权限码** `{模块}:{资源}:{动作}`，与 `sys_menu.auth_code` 和 `#[alun::permission]` 声明一一对应；`SUPER_ADMIN` 短路全部校验。
8. **路由宏路径不含 `/api` 前缀**（由 `[router] prefix` 注入）；`/health` 手动注册豁免前缀。
9. **前端 API 路径必须写全 `/api/...`**（漏前缀会命中 SPA fallback 返回 HTML 导致"响应体格式不符合"）。
10. **TDesign Grid 是 12 列制**（`t-col-xl-2`=一行 6 个、`-xl-3`=一行 4 个；`t-col-xs-24` 不存在，最大 `xs-12`）。
11. **事务已提交后的失败必须有补偿 —— 不许留死单**。
    - 凡是 `tx.commit()` 之后才执行、且**可能失败**的后续动作（过账、生成凭证、调外部服务、发通知），必须在失败路径上把前置状态**显式恢复**（打回草稿 / 标记失败 / 写补偿记录），不能只 `return Err` 把单据留在中间态。
    - **判据**：任何"提交后失败"的场景，用户必须能从界面继续操作（重试 / 重开 / 修正后重来），**不允许需要工程师改库才能恢复**。
    - **最高危**：免审直过分支（`auto_finished`）—— 它没有审批人兜底，失败后没有任何待办会来推进它。这类分支的**每一个** `Err` 都必须打回草稿。
    - 反例（2026-11-08 生产实际发生）：收货单免审过账被"采购单已收满"拦下，代码只对 SoD 错误打回 DRAFT，其他错误直接返回 → 单据永久卡 `PENDING_POST`，无待办、无手工过账接口，只能删单重建。
12. **菜单（`sys_menu`）两列的语义 —— 别把功能说明当名字**。
    - `name` = 唯一标识（权限码关联），短名，不带空格和长句。
    - `title` = **侧边栏显示名，2-6 个字的菜单名**，不带标点、不带说明、不带章节引用。
    - **禁止**把功能说明写进 `title`。反例：`即时 / 月末汇率维护 + 按月批量录入（ARCH-7 §15.6.3 ②）`（38 字，侧边栏直接换行）。
    - 功能说明放 `description` 列 / `docs/` / 前端 tooltip。
    - 同一父节点下 `sort_order` **必须唯一**（插菜单用 10 递增；插空用中间值如 15/25）。重复会导致侧边栏顺序每次刷新都变。
    - **提交前自查**（两条都应为 0 行）：
      ```sql
      SELECT path, name, title, length(title) FROM sys_menu
       WHERE deleted_at IS NULL AND menu_type IN (1,2) AND length(title) > 12;
      SELECT parent_id, sort_order, count(*) FROM sys_menu
       WHERE deleted_at IS NULL AND menu_type = 2 GROUP BY 1,2 HAVING count(*) > 1;
      ```
13. **姓名：存快照还是实时 join —— 按语义分，不能一刀切**。
    - **审计/历史类 → 必须存姓名快照**（用户改名/删号后要能回溯"当时是谁"）。
      例：状态日志 `operator_name`、操作日志 `username`、审批 `assignee_name`、附件 `uploader_name`。
    - **业务展示类 → join 实时取名**（要显示"现在是谁"）。
      例：列表的「创建人」、详情的「负责人」、合同 `owner`、佣金 `salesperson`。
    - **主数据关联 → join 实时取名**（要显示"最新名"）。
      例：客户 / 物料 / 部门 / 凭证 —— 主数据改名后，单据显示新名才是对的。
    - 反例（2026-11-08 发现）：附件表只存 `uploaded_by` 不存 `uploader_name` → 界面上直接显示 UUID。
    - **防 N+1**：批量 join，不能每行查一次；DTO 里凡是 `*_id` 都要同时返回 `*_name`。

## 高频踩坑（先读）
- 报表/接口不显示：先查前端 api 路径是否漏 `/api`。
- 图标渲染空白：`user-check`→`user-checked`、`speed`→`rocket`（TDesign 无这两个名）。
- **单据停在中间态（如 `PENDING_POST`）且界面上没有可点的按钮**：查是不是「事务提交后失败没补偿」（见约定 11）。这类单通常审批已过、账没动、没有待办，**只能改库**——遇到就是 bug，要修代码不是修数据。
- **登录后所有接口 400、但后端日志里没有 400**：请求头（尤其累积的 cookie）超过 nginx `large_client_header_buffers`，nginx 在转给后端之前就拒了。查 nginx access.log 的 ` 400 ` 行；容忍上限见 `/etc/nginx/nginx.conf`（当前 4×32k）。

## 详细文档（二开文档集，见 `docs/README.md` 总入口）
- `docs/README.md` — 二开文档集总入口（结构 / 设计亮点 / 前后端二开 / 编码规范 / AI 指引）
- `docs/项目结构与设计亮点.md` — 目录树、17 张表、Alun 设计亮点、部署模型
- `docs/后端二开指南.md` — 后端四件套模板、新增模块流程、迁移/事务/错误出口
- `docs/前端二开指南.md` — 前端 api/列表页/路由/主题二开
- `docs/编码规范.md` — 后端+前端编码规范与提交前检查
- `docs/AI二开指引.md` — AI 二开强制约定、反魔幻清单、范本指针
- `skill/SKILL.md` — 本仓库 AI 知识包（浓缩速查）
