# 云枢（CloudPivot）开发规则与流程 · Codex 执行手册

> **这份文档是给 AI 编码 Agent（Codex / Claude / Hermes worker）看的硬约束。**
> 每一节都写的是「不满足就不能交付」，不是建议。
>
> 项目：`/opt/cloudpivot`（Rust 后端 + Vue3 前端 + PostgreSQL）
> 最后更新：2026-11-18 · 与当前 `scripts/check.sh`、`.githooks/`、`kanban` 实际行为一致
>
> **优先级**：本文 > `docs/AI二开指引.md` > `docs/编码规范.md` > 其他分册
> 冲突时以本文为准，并立即报告冲突点，不要自行选择。

---

## 0. 一页速览（先看这个）

```
接卡 → 读卡面全文 → 建分支/工作区 → 改代码 → 跑门禁 → commit → push → 卡内汇报 → complete
                                                    ↑
                                            门禁不过 = 不许交付
```

**五条铁律（违反任意一条，交付作废）**：

| # | 铁律 |
|---|---|
| 1 | **迁移只能追加**，不改已应用的；版本号全局唯一；必须幂等；up/down 成对 |
| 2 | **不上生产**。你的产出最多到 `main` 分支，部署由人审批后执行 |
| 3 | **每张卡必须**：`cargo fmt` + 门禁通过 + `git commit` + `git push origin main` + 卡内 `--result` 汇报 |
| 4 | **禁止** `unwrap()` / `expect()` / 字符串拼 SQL / 手写 `service` 层 |
| 5 | **发现规则冲突或卡面歧义 → 停下来写进汇报**，不要自己发明做法 |

---

## 1. 任务管理层（Kanban）

### 1.1 卡的粒度（建卡时）

| 约束 | 值 |
|---|---|
| 单卡工作量 | **≤ 1.5 小时** |
| 单卡源文件 | **≤ 2 个**（不含测试文件） |
| 单卡测试用例 | **≤ 8 条** |
| 必须有 | 可验收的产物（能跑的命令 / 能看的接口 / 能点的页面） |

**拆不下去就说明卡定义有问题，报上来重拆。**

### 1.2 卡面必读块

每张卡正文末尾有「**交付要求（强制）**」和「**数据库约束（强制）**」，**必须完整读完再动手**。

### 1.3 状态机

```
todo → ready → running → done
                 ↓
              blocked（父卡未完成时被压住）
```

- 新卡一律 `status='todo'`
- **`todo` 单独压不住**：`recompute_ready` 会把「没有父卡」的 todo 卡提升为 ready
- **必须挂父子链**，靠父卡状态锁住

### 1.4 父子链规则（最容易踩的坑）

```
✅ 正确（单链串行）
A(done) → B(done) → C(ready) → D(todo) → E(todo)

❌ 错误（并行分叉，后面的卡会被跳过）
A(done) ├→ B(ready)
        └→ C(done) → D(done) → ...
```

**硬规则：一个未完成的父卡，只能挂 1 个子卡。**

违反后果：dispatcher 按优先级只拉起一个，另一个永远躺在 ready（真实事故：M3-27 被 M4 整批跳过、M4-11b 被 M4-12 跳过）。

**要插卡**：串在已有子卡的**后面**，不要并列挂同一个父卡。

```bash
# 插在 B 之后、C 之前：
#   拆 B → C，建 B → 新卡，建 新卡 → C
```

### 1.5 完成卡的标准动作

```bash
# 1. 提交
git add -A && git commit -m "<type>(<scope>): <说明>"

# 2. 推送（pre-push 会自动跑 L0+L1 门禁，约 2-4 分钟）
git push origin main

# 3. 卡内汇报（不带 --result = 交付不完整）
hermes kanban complete <卡号> --result "已完成。提交 <hash>：<一句话说明做了什么>"
```

**`--result` 必须包含**：提交 hash + 一句话说明。缺一个就不算完成。

---

## 2. 代码层硬规则

### 2.1 后端结构（Rust）

**每模块四件套，没有 service 层**：

```
backend/app/src/business/<模块>/
├── mod.rs      # pub mod dto; pub mod handler; pub mod model;  + 模块文档 + 接口一览表
├── handler.rs  # #[nik::permission(...)] + #[nik::get/post/...]，编排逻辑
├── dto.rs      # *Req（带 validator::Validate）/ *Res（带 from_model）
└── model.rs    # #[derive(Model)] #[table("biz_xxx")] + 数据访问
```

**禁止**：`service.rs` / `repository.rs` / 额外 DTO 映射层。项目刻意扁平。

**范本**：`backend/app/src/system/user/{mod,handler,dto,model}.rs`

### 2.2 依赖铁律

```toml
# ✅ 只声明 nik
nik = { version = "0.4.2", features = ["postgres", "cache", "sse", ...] }

# ❌ 绝对禁止
cargo add nik-db / nik-web / sqlx / axum / anyhow
use nik_db::...
```

要用新能力 → **在 `nik` 的 `features` 里点单**。

### 2.3 错误处理

```rust
// ✅ handler 里直接 ? 传播
let user = SysUserModel::find_by_id(&id).await?;

// ✅ 需要上下文时
db_error(e, "创建用户")?

// ❌ 禁止
map_err(|e| ApiError::internal(format!("{}", e)))?
unwrap() / expect() / panic!()
```

### 2.4 SQL

```rust
// ✅ 用 sql! 宏（自动参数化）
let rows = sql!(SysUserModel, "WHERE status = #{status} AND name LIKE #{kw}").fetch_all(&db).await?;

// ❌ 禁止
format!("SELECT * FROM sys_user WHERE name = '{}'", name)
sqlx::query("...")   // 裸用
```

### 2.5 路由与权限

| 层 | 规则 | 示例 |
|---|---|---|
| 后端宏 | **不含** `/api` | `#[nik::get("/business/purchase/order")]` |
| 前端 API | **写全** `/api` | `get('/api/business/purchase/order/page')` |
| 权限码 | 三段式 `{模块}:{资源}:{动作}` | `business:purchase:create` |

**权限码三处必须完全一致**：
1. `#[nik::permission("/path", "GET", "business:xxx:yyy")]`
2. `sys_menu.auth_code` 种子
3. 前端 `router.meta.permission`

### 2.6 迁移（最容易出事的地方）

```
backend/app/migrations/{system,business}/
├── V0432__xxx.up.sql
└── V0432__xxx.down.sql
```

**硬规则**：

| # | 规则 | 说明 |
|---|---|---|
| 1 | **只追加，不改已应用** | 已进 `schema_migrations` 的文件一个字节都不能动 |
| 2 | **版本号全局唯一** | 跨 `system/` 和 `business/` 两个目录统一编号，门禁 `scripts/lint/migration_version_unique.py` |
| 3 | **必须幂等** | 用 `INSERT ... ON CONFLICT DO NOTHING` / `WHERE NOT EXISTS` / `DO $$ ... END $$` |
| 4 | **up/down 成对** | down 可以近似还原，但必须能跑 |
| 5 | **不得依赖 seed 行** | 迁移里不能假设某些种子数据已存在 |

**当前状态**：最大版本 `V456`（磁盘 376 个 `.up.sql`）。**下一可用版本请现场查**（不要凭记忆）：

```bash
ls backend/app/migrations/*/V*.up.sql | sed 's/.*V0*\([0-9]*\)__.*/\1/' | sort -n | tail -3
```

**迁移里的分类值必须在字典里**：`sys_config.category` 只能取 `config_category` 字典已有的值，门禁 `scripts/lint/config_category_gate.py` 会拦。

### 2.7 菜单

```sql
-- ✅ title 只写 2-6 字名词短语，硬顶 12 字
INSERT INTO sys_menu (title, ...) VALUES ('采购订单', ...)

-- ❌ 不要塞进功能说明 / 章节号 / 卡号 / 箭头
('采购订单（M4-17 / ARCH-3 §2.1 → 见文档）')
```

同父 `sort_order` 间隔 10 递增且唯一。门禁 `scripts/verify/menu_invariants.sh` ⑥⑦。

**角色码不要抄老迁移**：`ADMIN` / `ACCOUNTANT` / `FINANCE` 等在生产**不存在**，SQL 不报错但授权 0 行。只能从 `select role_code from sys_role` 拿。门禁 `scripts/verify/role_code_gate.py`。

### 2.8 前端

```ts
// ✅ API 路径写全 /api
export const getOrderPage = (params: PageParams) =>
  get<PageData<OrderRes>>('/api/business/purchase/order/page', params)

// ❌
get('business/purchase/order/page')
```

- TDesign 栅格用 `t-col` 的 `span`，**不要** `t-col-xs-24`
- 图标名用存在的中划线名（`user-check` / `speed`），不存在的会白屏
- 样式用 `--app-*` 主题变量，不写死色值
- `t-drawer` 插槽用 `#body` 或默认槽，**不是** `#content`
- `src/types/components.d.ts` 必须随组件一起提交

---

## 3. 测试规则

### 3.1 后端单测

**改 `business/` 下任何文件，必须带测试**：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // ...
}
```

### 3.2 真库测试的口径（必须严格遵守）

**⚠️ 先看这一条（2026-11-18 实测发现）**：

| 库 | 迁移记账 | 最大版本 | 状态 |
|---|---|---|---|
| `cloudpivot_test`（模板库） | 326 | **V404** | ❌ **落后代码 52 个迁移** |
| `cloudpivot_dev` | 378 | V456 | ✅ 最新 |
| 磁盘迁移文件 | 376 | V456 | — |

**所以 `createdb -T cloudpivot_test` 造出来的库是旧 schema**（没有 V405+ 的表）。
建完派生库**必须先验版本**：

```bash
psql -h 172.18.10.145 -U cloudpivot -d cp_<卡号>_<用途> \
  -tAc "select max(version::int) from schema_migrations where kind='versioned' and success"
# 必须 >= 磁盘最大版本，否则这个库不能用来验收新功能
```

**推荐做法：用脚本造库**（自动跑全量迁移，schema 一定最新）：

```bash
bash scripts/verify/make_full_suite_db.sh          # 默认库名 cloudpivot_test_full
```

**唯一合法的连接口径**：

```bash
# 主机：172.18.10.145
# 用户：cloudpivot（有 CREATEDB）
# 四条 URL 必须指向同一个库
cd backend/app
ERP_DB_URL="postgres://cloudpivot:<pwd>@172.18.10.145:5432/<你的库>" \
SYS_DB_URL="$ERP_DB_URL" TASK_DB_URL="$ERP_DB_URL" P0FIX_DB_URL="$ERP_DB_URL" \
  cargo test --lib <模块路径> -- --test-threads=1

# 清场
dropdb -h 172.18.10.145 -U cloudpivot <你的库>
```

**密码从哪来**（不要写进任何文件）：

```bash
# 从 env 文件读（145 测试机）
grep NIK_DATABASE_PASSWORD /etc/cloudpivot/cloudpivot-dev.env
# 或直接用 .pgpass（已配好，psql/createdb 免密）
```

**绝对禁止**：
- ❌ 用 `alun` 用户（已 DROP）
- ❌ `127.0.0.1:5432` / `localhost:5432`（99 本机 PG 已 purge）
- ❌ 在 99 上 `su - postgres` 建库
- ❌ 用 `--test-threads` 默认值（并行会互踩，必须 `=1`）
- ❌ `--skip` / `--ignored` / 「SKIP 了不算失败」
- ❌ **在 V404 的旧模板库上验收 V405+ 的新功能**

**判据**：`0 failed`。有 1 条红就是没完成。

### 3.3 全量套件

```bash
# 造库（8-40 秒）
bash scripts/verify/make_full_suite_db.sh

# 跑全量（170 秒单进程 / 42 秒 19 域并行）
python3 scripts/test_domain.py run --package all

# 单域
python3 scripts/test_domain.py run --package hr
```

**CWD 必须是 `backend/app`**（`scope_model::d8c` 要扫 `src/`）。

### 3.4 前端

```bash
cd frontend
pnpm typecheck    # 必须过
pnpm build        # 必须过
```

---

## 4. 门禁体系（提交/推送会自动跑）

### 4.1 五层结构

| 层 | 内容 | 耗时 | 何时跑 |
|---|---|---|---|
| **L0** | 静态闸：C7 规则 / 色板 / 迁移版本唯一 / 迁移不依赖 seed / 库存写入口 / 路径引用 / 角色码 / 测试分域登记 / 统一夹具预算 / JUnit XML / `/metrics` / 页面零壳类 / **参数分类字典** / **assignee 有效性** | 秒级 | 每次 |
| **L1** | `cargo fmt --check` / `clippy -D warnings` / `cargo check` / 词表契约单测 / `pnpm typecheck` | ≈2 分钟 | 每次 |
| **L2** | 真库用例（`CHECK_WITH_DB=1`）/ ERP 不变量 / 词表值域 / 快照巡检 / 死权限码 / 孤儿按钮 | 分钟级 | `--full` |
| **L3** | `git archive HEAD` → 干净树跑 lib 单测（证明不依赖本机私有文件） | ≈15 秒 | `--full` |
| **L4** | 临时空库真跑全量迁移 + bootstrap（判据：迁移跑满 / 版本对齐 / 0 失败 / admin↔SUPER_ADMIN / 日志无 FATAL / `/health` 200 且跑满 5s） | 15-120 秒 | `--full` |

### 4.2 命令

```bash
bash scripts/check.sh --fast      # L0+L1（≈2-3 分钟）← push 前，pre-push 钩子自动调
bash scripts/check.sh --full      # L0+L1+L2+L3+L4（发版前必跑）
bash scripts/check.sh --l0        # 只跑静态闸（秒级，改字符串/菜单时够用）
bash scripts/check.sh --fresh-db  # 只跑 L4（改了迁移时快速自证）
bash scripts/check.sh --dry-run   # 只打印会跑什么
```

### 4.3 Git 钩子

```
.githooks/pre-commit   → 3 道闸（秒级）
                          ① secret 扫描（暂存区，scripts/secret-scan.sh）
                          ② 库存三本账唯一写入口（scripts/verify/inventory_write_gate.py）
                          ③ 暂存区 .rs 的 rustfmt 检查
.githooks/pre-push     → bash scripts/check.sh --fast（L0 + L1，约 2-4 分钟）
core.hooksPath = .githooks（已配置，不要改）
```

**pre-commit 的逃生门**（只在明确知道自己在做什么时用，都会打 stderr）：
`SKIP_SECRET_SCAN=1` / `SKIP_INVENTORY_GATE=1` / `SKIP_RUSTFMT=1`

**钩子红了就是代码有问题，不许改钩子、不许 `--no-verify`。**
`pre-push` 的逃生门 `SKIP_PREPUSH=1` 只用于「本次提交本身就是修门禁问题的」这种自举场景，且必须在汇报里写明理由。

### 4.4 门禁红了怎么办

| 现象 | 处理 |
|---|---|
| L0 迁移版本重复 | 改版本号（查最大号 +1，不要复用） |
| L0 分类不在字典 | 归并到已有分类，或补字典（**写成迁移**，不要手改库） |
| L0 assignee 无效 | 卡的 assignee 必须是存在的 profile |
| L1 clippy warnings | 修代码，不许 `#[allow(...)]` 除非有明确理由写注释 |
| L4 红 | 说明迁移在空库上跑不通，**这是真 bug** |
| **门禁本身有 bug** | 报告，不要绕过 |

---

## 5. 环境

### 5.1 三台机器

| 角色 | 地址 | 说明 |
|---|---|---|
| 开发 | `172.18.10.99`（本机 `/root`） | 代码主仓 `/opt/cloudpivot`，从这里 push |
| 测试 | `172.18.10.145` | bare 仓库 + 测试服务（8033）+ PG（`cloudpivot_test` 模板） |
| 生产 | `172.18.10.144` | `http://172.18.10.144/`，服务名 `cloudpivot` |

**链路**：99 push → 145 bare → 145 自动编译/迁移/重启 → 人工审批 → 144。

### 5.2 分支

```
main   唯一长期分支，所有提交直接进 main
```

**没有 feature 分支流程**。高风险改动（>2 文件）可以开临时 worktree，但最终仍进 `main`。

---

## 6. 交付定义（DoD · 缺一条即未完成）

一张卡要能标 `done`，必须同时满足：

| # | 判据 | 怎么验证 |
|---|---|---|
| 1 | 代码已提交 | `git log -1 --oneline` 有你的 hash |
| 2 | 已推送到 origin | `git rev-parse HEAD` == `git rev-parse origin/main` |
| 3 | 门禁通过 | `bash scripts/check.sh --fast` exit=0 |
| 4 | 有测试且通过 | `cargo test --lib <模块>` → `0 failed` |
| 5 | 卡内汇报 | `hermes kanban complete <id> --result "已完成。提交 <hash>：<一句话>"` |
| 6 | 卡面「交付要求」全满足 | 逐条对照卡面 |

**注意**：`dev done ≠ 上线`。部署是**独立的人工动作**，由审批流触发，不是你的职责。

### 6.1 什么算「完成」

- ✅ 「提交 `a1b2c3d`：采购订单加 contract_id 字段 + 挂合同校验，5 条单测全绿」
- ❌ 「已实现」/「已完成」/「应该没问题」—— 没有 hash、没有测试结果 = 不算交付

---

## 7. 绝对禁止清单

### 7.1 代码

```
❌ unwrap() / expect() / panic!()
❌ 字符串拼 SQL
❌ 新建 service 层 / repository 层
❌ cargo add sqlx / axum / anyhow / tokio（用 nik features）
❌ 改动已应用的迁移文件
❌ 迁移里手写 sys_role_menu 角色码（从 sys_role 查）
❌ 前端 API 路径漏 /api
❌ 后端路由宏带 /api
❌ t-col-xs-24
❌ 不存在的图标名
❌ menu.title 塞功能说明/章节号/卡号
```

### 7.2 流程

```
❌ --no-verify 跳钩子
❌ 改门禁脚本让它变绿
❌ 用 alun 用户 / 127.0.0.1:5432 建库
❌ 在 99 上 su - postgres
❌ 并行跑真库单测（必须 --test-threads=1）
❌ 只跑 dev 环境就标 done
❌ 迁移文件提交了但没执行就标 done
❌ 接口 404 却写「已交付」
❌ 不带 --result 就 complete
❌ 自己部署到生产（必须走审批流）
```

### 7.3 安全边界（AI 硬约束）

> **AI 不能自动改生产代码（只能改客户环境的副本），代码可以提交到测试区。改完不能自动上线（客户点「确认」才部署），必须通过特定审批流才能部署。**

- 你写代码的地方：99（开发机）
- 你的产出可以到：`main` 分支 + 145 测试区
- **你不能碰**：144 生产（除了读日志/查库做诊断）

---

## 8. 常见坑（真实踩过的）

| 坑 | 现象 | 正确做法 |
|---|---|---|
| **并行分叉** | 一个父卡挂 2 个子卡，后来的卡永远 ready 不被拉起 | 单链串行，插卡串在后面 |
| **todo 压不住** | 新建的无父卡被 `recompute_ready` 提升为 ready | 必须挂父子链 |
| **真库并行互踩** | 6 条用例单独跑绿、并行跑红（`biz_flow_linkage_rule` 全库唯一行） | 模块级互斥锁 + `--test-threads=1` |
| **迁移版本撞车** | 两个批次都写了 V0432 | 全局唯一门禁会拦；建卡时先查最大号 |
| **分类造新 tab** | 参数页冒出英文 tab | `sys_config.category` 必须在字典里（V0441 教训） |
| **角色码静默 0 行** | 迁移成功但没人有权限 | 从 `sys_role` 查真实码 |
| **菜单标题超长** | 侧栏换行截断 | title ≤12 字，门禁 `menu_invariants.sh` |
| **L4 空库红** | 本机有数据所以绿，空库跑不起来 | 迁移不能依赖 seed 行 |
| **push 卡 90-220 秒** | 以为失败了 | 正常，L0+L1 要跑完 |

---

## 9. 命令速查

```bash
# ─── 环境 ───
cd /opt/cloudpivot

# ─── 提交前 ───
cargo fmt                                    # 后端格式化
bash scripts/check.sh --fast                 # L0+L1 门禁（必过）
cd frontend && pnpm typecheck && pnpm build  # 前端（改前端必跑）

# ─── 提交 ───
git add -A
git commit -m "feat(采购): 订单加 contract_id 字段"
git push origin main                         # pre-push 自动跑门禁

# ─── 卡内汇报 ───
hermes kanban complete t_xxxx --result "已完成。提交 $(git rev-parse --short HEAD)：<说明>"

# ─── 测试 ───
cd backend/app
ERP_DB_URL="postgres://cloudpivot:<pwd>@172.18.10.145:5432/cp_<卡号>_x" \
SYS_DB_URL="$ERP_DB_URL" TASK_DB_URL="$ERP_DB_URL" P0FIX_DB_URL="$ERP_DB_URL" \
  cargo test --lib <模块> -- --test-threads=1

# ─── 查迁移最大版本 ───
ls backend/app/migrations/*/V*.up.sql | sed 's/.*V0*\([0-9]*\)__.*/\1/' | sort -n | tail -3

# ─── 查角色码（写授权段前）───
psql -h 172.18.10.145 -U cloudpivot -d cloudpivot_test -c "select role_code from sys_role where deleted_at is null and status=1"

# ─── 查字典分类（写 sys_config 前）───
psql -h 172.18.10.145 -U cloudpivot -d cloudpivot_test -c "select i.item_value, i.item_label from sys_dict_item i join sys_dict_type t on t.id=i.dict_type_id where t.dict_code='config_category' order by i.sort_order"
```

---

## 10. 不确定的时候

**停下来，写进卡内汇报，等人回答。** 不要猜、不要发明新做法、不要为了「跑通」绕过规则。

三种必须停下的情况：

1. **卡面自相矛盾**（要求 A 又要求非 A）
2. **规则冲突**（本文与卡面/其他文档冲突）
3. **需要改门禁才能通过**

汇报格式：

```
【需要决策】
问题：<一句话>
现状：<我查到了什么>
可选方案：A ... / B ...
我倾向：<哪个，为什么>
阻塞点：<不解决就没法继续的具体原因>
```

---

## 附录：关键文件索引

| 想看什么 | 去哪 |
|---|---|
| AI 二开规则（防魔幻生成） | `docs/AI二开指引.md` |
| 完整编码规范 | `docs/编码规范.md` |
| 后端二开指南 | `docs/后端二开指南.md` |
| 前端二开指南 | `docs/前端二开指南.md` |
| 门禁口径 | `docs/门禁口径-v2-20261103.md` |
| 测试库口径 | `docs/测试库口径-全量套件.md` |
| 测试卫生（真库分库跑法） | `docs/测试卫生-真库单测分库跑法.md` |
| 门禁脚本本体 | `scripts/check.sh`（1450 行，文件头有完整说明） |
| 各闸实现 | `scripts/lint/*.py`、`scripts/verify/*` |
| 架构文档 | `docs/architecture/` |
| 功能关系图 | `docs/architecture/cloudpivot-func-graph.html` |
