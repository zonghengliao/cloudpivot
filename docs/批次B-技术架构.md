# 云枢 · 批次 B 权限增强 技术架构设计

> 数据权限（5 范围）· 列级脱敏 · 角色互斥（SoD）
>
> | 项 | 值 |
> |---|---|
> | 项目 | 云枢通用后台（基础版 CloudPivot，Rust 1.94.1 / edition 2024 / alun 0.4.2 / PostgreSQL 18.6） |
> | 基线 | 批次 A 已交付并上生产（`8825b75`，258 tests，prod 27 张表）；本设计基于工作副本 `/root/.hermes/workspace/manong/fixA` |
> | 迁移占用 | `V0040`–`V0052` 已被批次 A 用掉；**本批 V0060 起**（V0053–V0059 留空，见 §5.2） |
> | 本文档性质 | **只做设计**。所有 SQL / DDL 均在真库（PG 18.6）跑过；所有结论附文件路径与行号；无业务代码交付 |
> | 验证环境 | PG 18.6 本地实例，库 `batchb_poc2`（5 001 用户）/ `batchb_poc3`（205 001 用户 / 211 部门）；脚本见附录 B |

---

## 目录

| 章节 | 内容 | 谁最该先读 |
|---|---|---|
| §0 | 决策摘要（10 条硬结论 + 5 处与任务书的偏差 + 交付边界） | 全体（5 分钟） |
| §1 | 现状核查（代码事实 + 11 组 POC 实测 + 框架能力边界 + 红线对照） | 后端实施者 |
| §2 | 数据权限：5 范围语义 / 表设计 / (a)(b)(b′)(c) 选型 / **四道防漏闸** / 登记表清单 / JOIN 注入 / 超管与权限码 / 缓存 / API | 后端 + 评审 |
| §3 | 列级脱敏：规则表 / 7 种类型 / 3 方案选型 / 三道闸 / 权限联动 / 豁免人 / 导出 / API | 后端 + 安全 |
| §4 | 角色互斥：对称唯一索引 / 三校验点 / 存量冲突处理 / 与 delegation cap 协同 / 文案 / API | 后端 + 产品 |
| §5 | 横切：12 个权限码 / V0060–V0069 分配 / **11 项底座报批** / 34 个单测点 / 性能与索引检查 / 14 条风险 | 评审 + 老大 |
| §6 | 附录 A：完整 DDL（V0060–V0067 原文，已在 PG 18.6 实跑）+ 存量违规检查 SQL | 后端实施者 |
| §7 | 实施拆分（B0–B9，25 人日，每步含可执行验收） | 项目负责人 |
| §8 | 6 个未决项（需老大/产品决策） | 老大 |
| 附录 B | POC 脚本清单 + 实测输出 + 复跑命令 + 清理说明 | 复核者 |
| 附录 C | 注入点接入表（20 行，实施时逐行打勾） | 后端实施者 |
| 附录 D | 事实索引（每条结论 → 文件:行号） | 评审 |

---

## 0. 决策摘要（先看这一页）

### 0.1 十条硬结论

| # | 决策点 | 结论 | 一句话理由（含证据） |
|---|--------|------|---------------------|
| **D1** | 数据权限注入机制 | **选 (b) 查询构造器封装**：新建 `business/shared/scope.rs`，提供 `RowFilter{sql,params}` + `scope::paginate()` 单入口；**并把 `db().paginate_with_params` 收敛为「只允许出现在 scope.rs 与白名单」** | 框架 Hook 是 **Model 生命周期钩子**，源码明确写「绕过 Model 直接写 SQL 时 Hook 不触发——这是设计决策」（`alun-db-core-0.4.2/src/traits/hook.rs:9-10`）；方案 (c) 在本栈**物理不可行**。方案 (a) 无收口点：现有 22 个文件里有 130 处 `SELECT`、20 处 `paginate_with_params`、43 处 `find_with_params`，逐处手写必然漏 |
| **D2** | 范围片段形态 | **应用层先解析 dept_id 集合，SQL 里只出现 `dept_id = ANY(?)` 绑定数组**，SQL 内**不嵌**递归 CTE 子查询 | POC-4b（20 万用户）：内嵌递归 CTE → `Seq Scan` **28.0 ms**；预解析成 `= ANY(ARRAY[…])` → `Bitmap Index Scan on idx_sys_user_dept_id` **7.8 ms**，快 3.6× |
| **D3** | 分页 count 代价 | 本批**新增 `scope::paginate()` 自己算 count（剥掉 ORDER BY）**；已有的 `paginate_with_params` 不动 | POC-5：`COUNT(*) FROM (…ORDER BY…)` **11.17 ms**（Parallel Sort 780 kB）vs 剥 ORDER BY **4.49 ms**。框架的 count 包裹不剥排序（`alun-db-0.4.2/src/db.rs:206-208`） |
| **D4** | 多角色范围合并 | **并集（最宽松获胜）**，SUPER_ADMIN 直接短路 | 与既有权限码合并口径一致（`alun-web-0.4.2/src/middleware/mod.rs:148-150` 的 `any()`）；交集合会让「本职角色 + 临时角色」互削，产生不可解释的取数结果 |
| **D5** | scope 缓存 | **走 Redis**：`perm:user_scope:{epoch}:{uid}`，TTL 300 s（复用 `CACHE_PRINCIPAL_TTL_SECS`），部门树变更用 **epoch INCR** 全量失效 | POC-8：一次解析（3–4 表 join + 递归 CTE）**0.54–1.18 ms**——单次不贵，但它是**每个列表请求**的固定开销；既有 `perm:user_perms:` 已是同一模式（`system/auth/model.rs:24-36`、`shared/constants.rs:375`） |
| **D6** | 列级脱敏机制 | **选 (a) DTO 出参前脱敏**（`Maskable` trait + `MaskSet`），**不用** serde 自定义序列化器、**不用** SQL 层脱敏 | POC-7b：SQL 层 `regexp_replace` 对 20 万行做脱敏 **140.25 ms vs 10.06 ms（14×）**；且脱敏值无法被精确匹配（同一行明文命中 1、脱敏后命中 0）——过滤/排序/导出全部失真 |
| **D7** | 脱敏类型实现 | **复用 `alun_utils::Mask` 的 6 个具体方法，但绝不复用 `mask_by_type`**；自定义正则引入 `regex`（把既有间接依赖显式化） | 实测 `Mask::mask_by_type("custom", "张三")` **panic**：`byte index 2 is not a char boundary`（`alun-utils-0.4.2/src/mask.rs:198`，POC-9 真跑） |
| **D8** | 导出脱敏 | **必须脱敏，且 fail-closed**：`ExportColumn.sensitive=true` 但解析不到规则 → 直接 422 拒绝导出，不静默出明文 | `definition.rs:630` 的 `sensitive` 字段注释已写明「导出脱敏由 A4/A7 的权限与 Mask 能力接管；此处仅标记」，而 `business/impexp/mod.rs:31` 自认「导出脱敏与明文权限：未做」——这正是本批要补的洞 |
| **D9** | 互斥校验形态 | 校验集合 = **`已持有 ∪ 本次授予`** 做两两配对（不是「本次 vs 已持有」） | POC-6b-6c：同一次请求里同时授予互斥的 A+B 时，「本次 vs 已持有」形态**命中 0 条**（可绕过）；UNION 形态命中 1 条 |
| **D10** | 互斥配置与存量冲突 | **阻止配置（409）+ 独立冲突预检接口**；不做「允许但告警」，也不自动解绑 | SoD 的价值在于「不可共存」；告警即默认失效。「先自动解绑」是不可逆破坏性动作，必须人来做 |

### 0.2 与任务书假设的偏差（必须先对齐，共 5 处）

| 任务书假设 | 代码实况（证据） | 本设计的处理 |
|---|---|---|
| 「现有项目用 `sql!` 宏手写 SQL」 | **项目里 `sql!` 出现 0 次**（`grep -rn "sql!" backend/src` → 0 命中）。真实写法是 `String` 拼接 + `?` 占位符 + `Vec<Value>` 按序绑定，例：`system/role/model.rs:115-139`、`system/user/model.rs:185-250`。框架侧**确实有** `sql!` 宏（`alun-db-macro-0.4.2/src/lib.rs:124-160`，支持 `SqlFragment` 嵌套与参数自动配对），但项目一处未用 | 选型对比表仍按 (a)(b)(c) 给（§2.3），并增加 (b′) 「`sql!` 片段组合」子选项；**本批不迁移既有 130 处 `SELECT`（22 个文件）**（回归风险 >> 收益），仅**新增/改动的查询**可以选 `sql!` 形态。防漏靠"单入口 + grep 门禁"，不靠宏 |
| 「框架层 hook 若支持需查证」 | **不支持查询改写**。`Hook` trait 只有 `before/after_insert/update/delete`（无 query hook），且文档明写绕过 Model 的 raw SQL 不触发；`AsyncHook` 只有 `after_*` 且**延迟到事务提交后**，无法用于"阻止/缩小结果集" | 方案 (c) 在结论表里标 **不可行（证据见 `alun-db-core-0.4.2/src/traits/hook.rs:9-10, 36-42`）**，不作为候选 |
| 「`sys_role` 无 `data_scope`，需新增」 | 属实（`migrations/system/V0002__create_sys_role.up.sql`：只有 `role_code/role_name/description/status/sort_order/remark` + 审计列）。另有一条**任务书未提**的同源事实：`auth/model.rs:241-243` 的注释引用了 `shared/principal.rs`，**该文件不存在**（`ls backend/src/shared/` → `constants/enums/error_code/mod/startup_guard`），是一处悬空引用 | 本批正好补上「业务侧主体口径」这一层：新建 `business/shared/principal.rs`（超管判定 + 范围解析 + 脱敏集合），并把 `system/` 侧悬空注释的指向修正（1 行注释，随 §5.3-#7 同批提交） |
| 「脱敏可考虑 SQL 层（PG 函数）」 | 技术可行但实测代价与副作用都不可接受（14× 耗时、破坏过滤、破坏排序聚合、且 `SELECT *` 的写法无从下手） | 结论定为**不用**，把实测数字写进 §3.3 对比表，避免下一批再讨论一次 |
| 「Excel 导出是否也脱敏（必须，否则绕过）」 | 导出确实存在**两条**产出路径：同步导出 `impexp/handler.rs:780`、异步 worker `impexp/worker.rs:607`，**都**调 `engine::format_export_rows` | 改 1 处 `format_export_rows` 签名即可双路覆盖（§3.7）；异步 path 需要靠 `sys_export_task.create_by` 回查请求者身份——该列已存在（`V0041:95`），无需加列 |

### 0.3 本批交付边界（明确不做）

- ❌ 不做「行级规则表达式引擎」（`dept IN (...) AND status = 2` 这种可配置表达式）。本批只有 5 种固定范围，规则由代码枚举承载；表达式引擎留给未来的 BI/报表批（大数据批已有 `bi_data_rule` 设计，不复用）
- ❌ 不做数据权限的「临时授权 / 审批流」（B 批无工作流引擎，D 批才有）
- ❌ 不做「列表接口按列隐藏」（`HIDE` 语义）。脱敏只做 `MASK`；隐藏列需要 DTO 变体，收益 < 复杂度
- ❌ 不迁移既有 130 处 `SELECT`（22 个文件）到 `sql!`（§0.2 已说明：收益 << 回归风险）
- ❌ 不做前端页面实现（只给 API 契约 + 文案 + 交互约定，页面在实施步 B8 落地）
- ❌ 不处理批次 A 遗留的「SSE 跨副本用 Redis Pub/Sub 替换」（`business/notice/mod.rs` 头注释里提到"B 批次替换"，那是批次 A 文档的措辞；**本卡范围是权限**，该事项在本卡外，已在 §5.6 风险清单里显式移交）
- ❌ 不做 `sys_role` 的「数据范围继承」（角色树继承范围），本批只有角色扁平集合

---

## 1. 现状核查（设计的事实基础）

### 1.1 代码布局与本次要动的文件

```
backend/src/
├── system/            # 底座（本批只读，改动逐项报批 §5.3）
│   ├── auth/   model.rs(398)  handler.rs      ← 权限装载 / 主体缓存失效
│   ├── user/   model.rs(557) guard.rs(414) handler.rs(1041) dto.rs(253)
│   ├── role/   model.rs(324) handler.rs(705) dto.rs(124)
│   ├── menu/   model.rs       handler.rs
│   └── dept/   model.rs(322) handler.rs(657)   ← 递归 CTE 子树查询在这里
├── support/           # 底座（同 system/）
│   └── log/ dict/ file/ config/ job/ online/ dashboard/ ipgeo/ health/
├── business/          # 二开（本批新增代码主要落这里）
│   ├── impexp/  (8511 行，含 definition/engine/worker/handler)
│   ├── notice/  storage/  monitor/
│   └── shared/        ← 本批新建：scope.rs / mask.rs / principal.rs / sod.rs / guard.rs
├── middleware/  auth_guard.rs session_guard.rs operation_log.rs
├── shared/      constants.rs error_code.rs enums.rs startup_guard.rs(948)
├── utils/       mod.rs（require_uuid / like_pattern / db_error / build_tree …）
├── bootstrap.rs (359)  lib.rs (102)  main.rs (15)
```

### 1.2 现有 SQL 拼装的**真实**形态（这决定了注入方案）

`system/role/model.rs:110-151`（角色分页，逐行读过）：

```rust
let mut sql = String::from("SELECT * FROM sys_role WHERE deleted_at IS NULL");
let mut params: Vec<Value> = Vec::new();
if let Some(pattern) = utils::like_pattern(filter.role_name.as_deref()) {
    sql.push_str(" AND role_name LIKE ?");
    params.push(Value::from(pattern));          // ← 参数与 SQL 文本**手工**保持同序
}
...
sql.push_str(" ORDER BY sort_order ASC, created_at DESC");
let Page { list, total, page, size, pages } =
    db().paginate_with_params(&sql, params, page, page_size).await?;
```

`system/user/model.rs:179-258`（用户分页，含**已在用的**部门子树过滤）：

```rust
"SELECT u.*, d.dept_name AS dept_name FROM sys_user u \
 LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL \
 WHERE u.deleted_at IS NULL"
...
let placeholders = dept_ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
sql.push_str(&format!(" AND u.dept_id IN ({placeholders})"));   // 占位符动态生成
for id in dept_ids { params.push(Value::from(id)); }             // 参数同序追加
```

**三个必须继承的现实约束**：

1. **没有 ORM 拦截器**——`WHERE` 是字符串，注入点只能在「拼字符串的那一层」；
2. **参数位置是脆弱的**：任何"追加 WHERE 片段"的代码一旦忘了同步 `params`，就是**占位符数量错位**（运行期报 `number of parameters must be at least N`，或更糟：错位绑定出一批不报错的错数据）。所以注入接口必须**同时返回 SQL 与 params 且不允许分两次调用**（本设计用 `RowFilter{sql, params}` 单一结构强制这一点）；
3. **占位符是 `?`**（框架的 `SqlQuery` 在 Postgres 方言下会 rewrite 成 `$n`，`alun-db-sql-0.4.2/src/placeholder.rs`），业务侧只写 `?`。

### 1.3 权限校验实现（及其双口径）

| 层 | 实现 | 位置 |
|---|---|---|
| 声明 | `#[alun::permission(path, method, permission = "system:user:list")]` + `#[alun::get(...)]` | `system/user/handler.rs:44-49` 等 50+ 处 |
| 校验 | 框架中间件读 `TokenClaims.permissions`，`has_permission()` = 命中即过；`is_super_admin()` = permissions 含 `*` 或 `*:*:*` | `alun-web-0.4.2/src/middleware/mod.rs:148-160` |
| 超管短路 | 登录时若 `role_code == SUPER_ADMIN`，权限集合直接装成 `["*:*:*"]`，**不查授权表** | `system/auth/model.rs:236-245`（`shared/constants.rs:124` 定义常量） |
| 业务侧超管判定 | **必须以库中角色编码为准**，不读 JWT 里的通配码（注释明写"二者不构成双真源"） | `system/auth/model.rs:241-243`；`system/user/guard.rs:24-31` |
| 授权上限（批次 A） | ① 角色菜单授权：不能授予自己未持有的 `auth_code`（`ensure_menus_grantable`）；② 用户授角色：三条规则 `evaluate_grant`（不能动超管号 / 不能授超管角色 / 不能自我提权） | `system/role/handler.rs:651-680`；`system/user/guard.rs:98-155,160-240` |

> **对本批的直接影响**：数据权限的"超管不设限"必须用**库侧**`role_code` 判定（`user_holds_super_admin_role`，`system/user/model.rs:346`），**不能**用 `claims.is_super_admin()`——后者是签发时快照，降权后到令牌过期前仍是 true。批次 A 的 guard 已经踩过这个坑并写进注释（`guard.rs:229-232`）。

### 1.4 `sys_dept` 树结构（以及已有的递归 CTE）

```sql
-- migrations/system/V0006__create_sys_dept.up.sql
id UUID PK, dept_code UNIQUE(部分), dept_name, parent_id UUID,  -- 根节点为 NULL
dept_path VARCHAR(500),   -- '/1/2/' 形态，注释说"加速子树查询"
leader_id UUID, phone, email, status, sort_order, 审计列, deleted_at
CREATE INDEX idx_sys_dept_parent_id ON sys_dept (parent_id);
CREATE INDEX idx_sys_dept_path ON sys_dept (dept_path);
```

已有实现（**本批直接复用，不重写**）：`system/dept/model.rs:261-281`

```rust
let sql = "WITH RECURSIVE dept_tree AS ( \
     SELECT id FROM sys_dept WHERE id = ? AND deleted_at IS NULL \
     UNION ALL \
     SELECT c.id FROM sys_dept c \
     INNER JOIN dept_tree t ON c.parent_id = t.id \
     WHERE c.deleted_at IS NULL \
   ) SELECT id FROM dept_tree";
```

> 注意与任务书的措辞差异：任务书说「含 `parent_id`」，实际**还有 `dept_path`**。本设计**只用 `parent_id` 递归**，不用 `dept_path`——源码注释自己解释了原因：「不依赖可能因搬迁而未同步的 dept_path」（`dept/model.rs:245-248`）。POC-4b 也验证了 `dept_path LIKE` 在搬迁不同步时会给出错误结果集。

### 1.5 POC 实证（11 组：POC-1…POC-9，含 6b/7b 两个修正版；脚本与原始输出见附录 B）

| # | 验证什么 | 结果（实测） |
|---|---|---|
| POC-1 | 本设计 DDL（`V0060`–`V0064`、`V0067`）在真实 schema 上可执行 | 在 `V0001–V0006 + V0022 + V0010 + V0040 + V0041` 之上全部 `rc=0`；`ALTER TABLE sys_role ADD COLUMN data_scope` 成功 |
| POC-2 | 造真实规模数据（211 部门 / 5 001 用户 / 5 角色 / 151 行自定义部门） | `depts=211 users=5001 role_dept=151` |
| POC-3 | 5 种范围的行数正确性 + 计划形态 | `ALL=5001 / DEPT=1 / DEPT_CHILD=1 / SELF=1 / CUSTOM=5000`（该库数据倾斜，见附录 B 备注）；**关键**：`COUNT(*) FROM (…ORDER BY…)` 的计划里真有 `Sort` 节点（349 kB） |
| POC-4 | 20 万用户规模 | `users=205001 / depts=211`，每部门 ≈ 971 人 |
| POC-4b | 三种子树 SQL 形态对比 | 内嵌递归 CTE **28.0 ms**（Seq Scan 205 001 行）／预解析 + `ANY(ARRAY)` **7.8 ms**（`Bitmap Index Scan on idx_sys_user_dept_id`）／150 部门自定义集合 `IN(子查询)` **22.1 ms** vs `ANY(ARRAY)` **36.4 ms** |
| POC-5 | 框架 count 包裹代价 | 含 ORDER BY **11.17 ms**（Parallel Sort 780 kB + Gather Merge）／剥 ORDER BY **4.49 ms**（无 Sort） |
| POC-6 | 互斥表约束 | 反向 `B×A` 被 `uk_sys_role_mutex_pair`（`LEAST/GREATEST` 表达式唯一索引）**拒绝**；自反 `A×A` 被 `CHECK` **拒绝**；存量违规 SQL 精准捞出 1 人 |
| POC-6b | 授权前互斥校验 SQL（union 形态） | 6a 命中 1 条／6b（无冲突）0 条／**6c（同次授予两个互斥角色）命中 1 条**——证明必须用 `已持有 ∪ 本次` 形态／6d（过期绑定不算持有）0 条 |
| POC-7/7b | 脱敏的 SQL 层代价与副作用 | 20 万行 PG 侧 `regexp_replace`：**10.06 ms → 140.25 ms（14×）**；精确匹配 `mobile='139…0001'` 明文命中 **1**、套脱敏后命中 **0**；`NULLS NOT DISTINCT` 唯一索引能拦住重复的全局规则；`CHECK` 正则拦住 `sys_user; DROP TABLE` |
| POC-8 | scope 解析查库成本 | 冷 1.18 ms / 热 0.54–0.61 ms（`sys_user`→`sys_user_role`→`sys_role` + 递归 CTE） |
| POC-9 | `alun_utils::Mask` 的坑 | `Mask::mobile("13812345678")` → `138****5678` ✓；`Mask::name("张三丰")` → `张**` ✓；**`Mask::mask_by_type("custom_regex","张三")` → panic**：`byte index 2 is not a char boundary … of 张三`（`alun-utils-0.4.2/src/mask.rs:198`） |

### 1.6 框架能力边界（决定"不要重复造"与"不可能做到"）

| 能力 | 有没有 | 证据 | 对本设计的后果 |
|---|---|---|---|
| 查询改写 hook / 拦截器 | **没有** | `alun-db-core-0.4.2/src/traits/hook.rs` 只有 insert/update/delete 前后；文档明写 raw SQL 不触发 | 方案 (c) 排除 |
| 同步 hook 阻止写入 | **有**（`before_*` 返回 `Err` 即回滚） | 同上 | 但**不能**用它做互斥校验：trait 是同步的，内部无法 await DB 查询（要 `block_in_place`+`block_on`，有死锁风险）。所以互斥校验走显式调用（§4.2） |
| `sql!` 宏 + 片段组合 | **有，项目零使用** | `alun-db-macro-0.4.2/src/lib.rs:124-160`；`SqlFragment` trait 在 `alun-db-core-0.4.2/src/query.rs:52` | 作为 (b′) 可选形态，不强制 |
| 分页（含 count） | 有 | `alun-db-0.4.2/src/db.rs:194-226`：`SELECT COUNT(*) FROM ({sql}) AS _count` 然后原 SQL 追加 LIMIT/OFFSET | count 不剥 ORDER BY（POC-5 实测 2.5×）→ 本批自建 `scope::paginate` |
| 缓存 | 有 `Cache` trait：`get/set/set_ex/del/incr/keys/delete_pattern` | `alun-cache-0.4.2/src/lib.rs:33-61` | epoch INCR 方案可行（§2.8） |
| 响应体捕获/改写 | 有（项目已用） | `middleware/operation_log.rs:223-240` 用 `axum::body::to_bytes` 取回 body 再回填 | 证明「响应层脱敏」技术上可行，但**不采用**（§3.3） |
| 脱敏工具 | 有 `alun_utils::Mask` + `mask_json_value` | `alun-utils-0.4.2/src/mask.rs:20-46,120-200` | 6 个具体方法可复用；`mask_by_type` 的兜底分支有 panic（POC-9） |
| 启动自检 | 有成熟范式 | `shared/startup_guard.rs`（948 行，`preflight()` 在 `lib.rs:72-78` 调用） | 本批的"防漏启动自检"沿用同一范式（§2.4） |

### 1.7 红线对照

| 红线 | 本设计怎么守 |
|---|---|
| 新代码进 `business/` | 三个模块的实现全在 `backend/src/business/permission/{scope,mask,sod}/` + `business/shared/` |
| `system/` `support/` 只读 | 只做**11 处最小改动**，逐项在 §5.3 报批（每项 1–15 行），并给出"为什么不动它做不到" |
| 不设 service 层 | 编排在 `business/permission/*/handler.rs`，数据访问在同目录 `model.rs`；**没有** `service.rs` |
| 错误三出口 | handler 一律 `ResResult<T>`；model 一律 `DbError`；启动/任务 `alun::Result<T>` |
| 禁 unwrap / 拼 SQL 值 | scope 片段**只拼占位符**，值全部走 `Value` 绑定；`table_alias` 参数类型是 `&'static str`（编译期固定），不接受运行时字符串 |
| 动代码必带单测 | §5.4 给了 34 个单测点，每个都写明"断言什么" |

---

## 2. 数据权限（Data Scope）

### 2.1 五种范围的语义（与并集规则）

| 值 | 枚举 | 语义 | 生成的 SQL 片段（`{a}` = 主体表别名，全部占位符绑定） |
|---|---|---|---|
| 1 | `ALL` | 全部数据 | `AND 1 = 1`（片段可整体省略；实现上**恒真片段**，便于日志与自检显示"已注入"） |
| 2 | `DEPT` | 本部门（用户所属部门，**不含下级**） | `AND {a}.dept_id = ?` |
| 3 | `DEPT_AND_CHILD` | 本部门及以下 | `AND {a}.dept_id = ANY(?)`（参数 = 解析后的 `uuid[]`，见 §2.3 结论） |
| 4 | `SELF` | 仅本人 | `AND {a}.create_by = ?` 或 `AND {a}.id = ?`（按表的"归属列"决定，见 §2.5 的 `owner_col`） |
| 5 | `CUSTOM` | 自定义部门集合 | `AND {a}.dept_id = ANY(?)`（参数 = `sys_role_dept` 里该角色的 dept_id 数组） |

**合并规则（多角色）**：先按角色各算一个片段，再并集：

- 任一角色是 `ALL` → 整体直接 `1=1`（等价超管，但**仅对有该角色的用户**）；
- 否则最宽松优先：`{DEPT_AND_CHILD ∪ CUSTOM}` 合并成一个 dept 集合（`= ANY(?)`）+ 若有 `SELF` 角色则整体变 `(dept 集合命中 OR owner = ?)`；
- **实现上不生成嵌套 OR 树**，而是**先把该用户的"可见 dept 集合"与"是否可见自己"解析成两个值**（§2.8 的 `CachedScope`），再生成**最多两段**的固定片段。理由：OR 树会随角色数线性膨胀（POC-4b 的多角色 OR 已 18.2 ms），且 SQL 形态多到无法被单测穷举。

**为什么是并集而不是交集**：① 与权限码合并口径一致（`has_permission` 是 `any()`）；② 交集会让"本职角色 + 临时项目角色"互相削减可见范围，用户看到的数据比任一单角色都少——这种"越被授权越看不见"的行为在运维侧无法解释，且会把故障排查成本推给支持团队。

### 2.2 表设计

**`sys_role.data_scope`（V0060，`ALTER` 而非建表）**

```sql
ALTER TABLE sys_role ADD COLUMN data_scope SMALLINT NOT NULL DEFAULT 1;
ALTER TABLE sys_role ADD CONSTRAINT ck_sys_role_data_scope CHECK (data_scope IN (1,2,3,4,5));
```

- 类型选 `SMALLINT` 而不是 `TINYINT`：项目里所有状态位同族列都是 `SMALLINT`（`sys_role.status`、`sys_menu.menu_type`、`sys_user.status`），`EnableStatus` 的 `TypeMapper` 也是按 i16 实现（`shared/enums.rs`）；用 `TINYINT`（PG 无原生 tinyint，得用 `SMALLINT` 或 `"char"`）会引入第三种映射。
- **默认值 `1`（全部）是刻意选择**：迁移前系统**不存在**数据权限概念，等于所有角色都是"全部"。默认 `1` = 迁移后可见范围与迁移前**逐行一致**，不产生任何"无预告的数据消失"。若默认 `4`（仅本人），上线当天所有非超管用户的用户列表都会突然只剩自己——这是不可接受的生产事故型变更。收紧范围由管理员按角色逐个配置（§5.2 的 `V0067` 提供"基线收紧"脚本模板，但**默认不执行**）。
- 索引：**不加**。`data_scope` 只在"解析当前用户 scope"时按 `role_id` 读取（走 `sys_role` 主键），不参与任何过滤条件。

**`sys_role_dept`（V0061，新建）**

见附录 A §A.1。要点：
- `UNIQUE (role_id, dept_id)` —— 同一角色对同部门只一行；
- 双 `REFERENCES`（`sys_role` / `sys_dept`）与 `sys_user_role` 同风格（`V0003` 两侧都有 FK）。**注意**：`sys_dept` 是软删，FK 不会阻塞正常删部门（软删只是 `UPDATE deleted_at`），只在物理清理时才可能冲突——这与既有 `sys_user.dept_id` 用弱引用是不同取舍：这里需要 FK 是因为 `sys_role_dept` 是"授权数据"，指向不存在的部门就是脏授权；
- `data_scope != 5` 时**保留**该角色的 `sys_role_dept` 行不清空（切换范围不丢配置）。读取时只在 `data_scope = 5` 才取；`update` 接口提供 `custom_dept_ids` 字段，`None` = 不动，`Some(vec![])` = 清空。

### 2.3 注入机制：三方案对比（含 (b′)）与结论

| 维度 | **(a) 显式传参**（handler 里自己拼） | **(b) 查询构造器封装**（本设计采用） | **(b′) `sql!` 片段组合** | **(c) 框架层 hook** |
|---|---|---|---|---|
| 形态 | 每个 handler：`let f = scope::resolve(&claims).await?; sql.push_str(&f.sql); params.extend(f.params);` | 统一入口 `scope::paginate(alias, base_sql, params, page, size, &principal)`，**它自己**解析 + 注入 + 分页 | `scope::filter(...) -> SqlQuery`，调用处 `sql!("SELECT … WHERE 1=1 ", filter, " ORDER BY …")` | 期望框架在 SQL 执行前改写 |
| 可漏否 | **会漏**：50+ 处查询、新增查询必然有忘写的；漏了不报错，只会"多看到数据" | 漏了要么编译不过（缺少 `Principal` 参数），要么被 grep 门禁拦住 | 同 (b)，且**参数与 SQL 不可能错位**（宏保证成对） | 若存在则最彻底 |
| 参数错位风险 | **高**（两次调用：先拿 SQL 再拿 params） | 低（`RowFilter` 一个结构体内含两者，构造后不可拆） | **零** | — |
| 与现有代码风格冲突 | 无（就是现在的写法） | **小**：新增单入口，旧查询逐步迁移 | **大**：`sql!` 项目零使用，混用两套拼装风格 | — |
| 本栈可行性 | 可行 | 可行 | 可行但语法学习成本 + 与 150 处旧代码风格分裂 | **不可行**（§1.6） |
| 结论 | ❌ 只作为"显式可见"的次要补充（自检接口用它打印片段原文） | ✅ **采用** | 🟡 保留为"新查询可选形态"，本批不推广 | ❌ 排除 |

**采用形态（接口契约，纯设计，不贴实现代码）**

```
business/shared/scope.rs
├── pub enum ScopeKind { All, Dept, DeptAndChild, SelfOnly, Custom }   // 与 SMALLINT 1..5 一一对应
├── pub struct RowFilter { sql: String, params: Vec<Value>, kind: ScopeKind, visible: bool }
│     ├── fn allow_all() -> Self                       // "AND 1 = 1"
│     ├── fn dept_set(&self) -> Option<&[Uuid]>        // 供自检/日志显示，不参与 SQL 生成
│     └── fn is_noop(&self) -> bool                    // kind == All
├── pub struct ScopedPrincipal {      // 一次请求只解析一次
│      user_id: Uuid, is_super: bool, dept_set: Option<Vec<Uuid>>, self_only: bool }
├── pub async fn resolve(user_id: Uuid) -> Result<ScopedPrincipal, DbError>       // 带缓存，§2.8
├── pub fn filter_for(p: &ScopedPrincipal, target: ScopeTarget) -> RowFilter
│     // ScopeTarget { alias: &'static str, dept_col: &'static str, owner_col: Option<&'static str> }
└── pub async fn paginate(...) -> Result<Page<Row>, DbError>
      // ① 注入 WHERE ② count 用剥掉 ORDER BY 的形态 ③ 数据页保留 ORDER BY
```

**`ScopeTarget` 用 `&'static str` 的关键作用**：别名与列名是编译期常量，**运行时无法被用户输入污染**，这是"禁拼 SQL"红线在结构层面的保证（不是靠 code review 提醒）。`ScopeTarget` 的具体值集中登记在 §2.5 的清单里，一个表一个常量。

**为什么必须自建 `paginate`（而不是继续用 `db().paginate_with_params`）**：POC-5 已证明框架的 count 是 `SELECT COUNT(*) FROM ({sql}) AS _count`，**ORDER BY 不被剥离**，多花一次全排序（20 万行时 780 kB / 6.7 ms）。数据权限会让"行集更小但过滤更贵"，这个固定损耗占比更高。自建 `paginate` 的差异只有一处：count 用「去掉 `ORDER BY` 之后的前缀」执行。注意这要求调用方把 `ORDER BY` 作为**独立参数**传入（而不是留在 `base_sql` 里），否则我们只能做字符串切割——**禁止**做字符串切割（切错就是语法错误或静默错序）。

### 2.4 防漏：四道闸（这是本设计的核心，逐条可执行）

**闸 1 · 编译期：单入口 + 参数强制**

- 所有分页查询必须走 `scope::paginate`；它要求调用方传入 `&ScopedPrincipal`，而 `ScopedPrincipal` **只能**由 `scope::resolve()` 构造（字段私有）。于是"想查分页就必须先解析 scope"由类型系统强制。
- 非分页查询（详情、下拉、导出取数）走 `scope::filter_for(...)`，返回的 `RowFilter` 必须被显式消费进 SQL——这里没有编译期强制（Rust 不会因为"没用返回值"报错），所以补闸 2。

**闸 2 · CI 门禁：`scripts/check.sh` 增加两条断言（脚本已写好并**在真实仓库上跑过**，见下方基线输出）**

```bash
# ① 分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）
#    注意写法：用 if grep; then FAIL; fi，不要用 `! grep && echo FAIL`
#    （后者在"找不到"时反而命中 -> 门禁永远报错，是这类脚本最常见的自伤）
if grep -rn "paginate_with_params" backend/src --include=*.rs \
     | grep -v "business/shared/scope.rs" \
     | grep -v -E "^[^:]+:[0-9]+: *(//|\*)" >/tmp/_gate1.txt; then
    echo "FAIL: 以下位置绕过了 scope::paginate："; sed 's/^/  /' /tmp/_gate1.txt; exit 1
fi

# ② 登记表（§2.5）的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
#    注意写法：不用 `grep -l | while read`（子 shell 里的 exit 传不出来），改用 for 遍历
FAILED=0
for t in sys_user sys_dept sys_import_task sys_export_task sys_notice sys_file \
         operation_logs audit_logs auth_logs security_logs; do
  for f in $(grep -rl "FROM $t" backend/src --include=*.rs 2>/dev/null || true); do
    grep -q "scope::\|SCOPE-EXEMPT" "$f" || { echo "FAIL: $f 查询 $t 未接入"; FAILED=1; }
  done
done
[ "$FAILED" -eq 0 ] || exit 1
```

**基线实测（在 `fixA` 当前代码上跑，2026-10-28）**：闸 ① 命中 **20 处** `paginate_with_params`（`impexp/model.rs` 3、`notice/model.rs` 3、`storage/model.rs` 3、`config/dict/job/log/file` 各 1–2、`system/role|user/model.rs` 各 1–2）；闸 ② 命中 **17 处**（10 张登记表 × 涉及文件）。这正是本批要处理的存量：**每一条要么改成 `scope::paginate`，要么加 `// SCOPE-EXEMPT: <理由>`**（附录 C 已按此逐行列出）。

> 「显式豁免注释」是设计的一部分：确实不需要数据权限的查询——例如 `auth/model.rs` 里"按 user_id 查自己的角色"、`cleanup.rs` 里"扫过期任务"（后台任务无请求者身份，不能也不该注入 scope）、`dashboard/model.rs` 的全局计数、`notice/fanout.rs` 的"解析收件人"（它要按目标枚举人，注入 scope 会让扇出静默少人）——写上一行 `// SCOPE-EXEMPT: <理由>`。这让**每个豁免都是一个被写下来的决定**，而不是一个疏漏。

> 这条基线也说明：**闸 ② 不能做成"零命中"门禁**，否则没人能提交；它必须允许"带理由的显式豁免"。反过来，闸 ① 是"零命中（除白名单）"的硬门禁——因为分页只有一种正确写法。

**闸 3 · 启动自检（`bootstrap` 内，沿用 `startup_guard` 范式）**

检查两件事，都查 `information_schema`：

1. **表/列存在性**：`SCOPED_TABLES`（§2.5 常量清单）里声明的 `(table, dept_col, owner_col)` 必须真实存在。**为什么必须启动期查**：如果某张表被后续批次改名/删列，注入点会在运行期抛 `column does not exist`——那时是 500 + 用户报障。启动期查 = 部署即失败，符合项目"宁可不启动也不带病上线"的既有取向（`bootstrap.rs:189-200` 的 prod 缺口令 `exit(1)` 先例）。
2. **超管角色存在**：`role_code = 'SUPER_ADMIN'` 必须存在（`scope::resolve` 依赖它做短路）。既有 `bootstrap` 已对"角色缺失"记 error 日志但不中断（`bootstrap.rs:145-151`）；本批把这条**升级为 prod 下 `exit(1)`**（列进报批 §5.3-#9），因为数据权限的短路逻辑失效 = 超管被自己锁在外面。

自检实现位置：`business/shared/guard.rs::preflight_permission()`，由 `bootstrap()` 的 `tokio::join!` 增加一个分支调用（1 行改动）。

**闸 4 · 自检接口 + 真库集成测试**

- `GET /system/data-scope/preview?user_id=` → 返回该用户**最终会注入的片段原文 + 绑定参数个数 + 逐表的预估计数**（每表一条 `SELECT count(*)`，`LIMIT` 保护：只对 ≤ 3 张表计数，且 3 s 超时）。这是运维侧"为什么他看不到"的唯一权威答案（大数据批的产品需求里把这类页面叫"数据权限自检"，同一思路，见 `docs/大数据-产品需求PRD.md:1484-1486`）。
- 集成测试（`backend/tests/`，用 `tests/sql/` 的既有建库范式）：建 3 层部门树 + 4 个用户 + 5 种角色，断言 5 种范围的**可见行数**（POC-3 已验证这套断言形态可跑）。

### 2.5 哪些表需要数据权限（**登记表清单**，本批的"业务表"定义）

| 表 | 需要 | `dept_col` | `owner_col` | 理由 / 现状缺口 |
|---|---|---|---|---|
| `sys_user` | ✅ | `dept_id` | `id`（SELF 时是"本人"而不是"创建人"） | 人员列表是数据权限最主要的场景；`page_users` 已有 dept 过滤但**没有权限约束**（`user/model.rs:185-250`，谁有 `system:user:list` 就能看全量） |
| `sys_dept` | ✅ | 自身 `id` | — | 部门树必须按范围裁剪，否则"看不见人但看得见部门"仍是信息泄露（部门名/负责人/电话/邮箱） |
| `sys_role` | ❌ | — | — | 角色是全局元数据；"谁能管角色"由权限码 `system:role:*` 控制。给角色加数据范围会让"角色授权"本身变成分布式的，无法审计 |
| `sys_menu` | ❌ | — | — | 同上 |
| `sys_user_role` / `sys_role_menu` / `sys_role_dept` | ❌ | — | — | 关联表，读路径一律以主表为入口（`find_user_roles_for_users` 只按已授权可见的 user_id 取，`user/model.rs:273-297`） |
| `operation_logs` | ✅ | — | `operate_by`(实际列名见下注) | 操作日志是"审计数据"，非超管不应看到全站操作轨迹 |
| `audit_logs` | ✅ | — | `operator_id` | 同上（敏感度更高：含 before/after 快照） |
| `auth_logs` / `security_logs` / `password_change_logs` | ✅（P1） | — | `user_id` | 登录与安全日志含 IP/设备，属个人信息 |
| `sys_file` | ✅（P1） | 需 join `sys_user` 取部门 | `uploaded_by` | 文件列表会泄露"别人上传了什么"；但 `sys_file` 无 `dept_id`，SELF/本部门形态需要 join → 归 P1（B5 之后） |
| `sys_import_task` / `sys_export_task` | ✅ | — | `create_by` | **现存缺口**：只有 `only_mine` 开关（`impexp/handler.rs:336,1138`），不开启时任何有 `business:impexp:list` 的人能看到**全站**导入/导出任务（含他人上传的文件名、行数、错误摘要） |
| `sys_notice`（管理视角 `page_notices`） | ✅ | — | `sender_id` / `create_by` | **现存缺口**：`notice/model.rs:524-560` 的过滤条件只有关键词/类型/级别/业务类型，无任何范围约束 |
| `sys_storage_config` / `sys_storage_policy` | ❌ | — | — | 存储后端是全局基础设施，只有超管 + 专属权限码能碰（`business/storage`），加数据范围无意义 |
| `sys_config` / `sys_dict_*` / `sys_job` / `sys_job_log` | ❌ | — | — | 全局配置类；`sys_job_log` 属运维数据，权限码控制即可 |
| 批次 C/D 未来表（公告 / 工作流） | 未定 | — | — | 由各自批次登记；本批的 `SCOPED_TABLES` 是**开放清单**，新增只需加一行常量 + 过一个单测 |

> **注**：`operation_logs` / `audit_logs` 的"操作人列名"以真实迁移为准（`migrations/support/V0011`、`V0014`），实施时须以 `\d operation_logs` 实测校对；本表给的是语义列，实施步 B5 必须先 `information_schema` 核对再登记——这正是闸 3 启动自检存在的意义。

### 2.6 跨表 JOIN 怎么注入（以 `page_users` 为例）

**规则（三条，简单到不会被误解）**：

1. **只锚定主体表**：`scope` 片段永远打在"这次查询的主体表别名"上（`page_users` 里是 `u`），**不打在 JOIN 出来的字典/关联表上**。
2. **需要二次裁剪的表，单独再注入一次**：`sys_dept` 作为主体（部门树接口）时打 `d`；同时被 join 时不重复注入（`LEFT JOIN` 的 `d` 只用于取 `dept_name`，不加条件——加了会把"人可见但部门不可见"变成人消失，语义错）。
3. **绝不用 `LEFT JOIN` 把 scope 写成 `ON` 条件**：`LEFT JOIN … AND u.dept_id = ANY(?)` 只会让 `dept_name` 变 NULL，**行数不变**——这是最隐蔽的漏洞（列表还在，只是部门名空了）。scope 一律进 `WHERE`。

`page_users` 注入后的 SQL 形态（`base` 来自 `user/model.rs:185-250`，`{FILTER}` 是 `RowFilter.sql`）：

```sql
SELECT u.*, d.dept_name AS dept_name
FROM sys_user u
LEFT JOIN sys_dept d ON d.id = u.dept_id AND d.deleted_at IS NULL
WHERE u.deleted_at IS NULL
  {FILTER}                       -- 例：AND u.dept_id = ANY($1)  或  AND (u.dept_id = ANY($1) OR u.id = $2)
  AND u.username LIKE ?
ORDER BY u.created_at DESC       -- ← 作为独立参数传给 scope::paginate
```

**参数顺序契约**：`scope::paginate(alias_target, base_sql, base_params, order_by, page, size, principal)` 内部固定把 `RowFilter.params` **前置**于 `base_params`（因为 WHERE 里 FILTER 的占位符出现在业务过滤条件之前）。这条契约写死在函数签名与注释里，并有单测断言"生成的 SQL 里第 1 个 `?` 对应 scope 参数"。

### 2.7 与 SUPER_ADMIN 的关系 + 权限码

- **超管跳过数据权限**：是。判定用**库侧角色编码**（`user_holds_super_admin_role`，`system/user/model.rs:346-360`，已处理 `expire_time` 过期），不用 `claims.is_super_admin()`（快照问题见 §1.3）。
- **超管也不受"配置上限"约束**：可以给任何角色设任何范围、可以建任何互斥组。与批次 A 的 `evaluate_grant`（超管不受上限约束，`guard.rs:105-107`）保持一致。
- **"能管数据权限"本身的权限码**：`system:data-scope:update`（改配置）/ `system:data-scope:list`（看配置）/ `system:data-scope:preview`（自检）。三个都属**元权限**：它们决定"别人能看到什么"，因此：
  - 种子只授 `SUPER_ADMIN`（`V0065`，与 `V0020` 只授超管同例）；
  - **非超管修改 `data_scope` 时做"范围上限"校验**：新范围的部门集合必须 ⊆ 操作者自己的可见部门集合（纯集合包含，可单测）。否则一个 `system:data-scope:update` 持有者可以把自己部门的范围扩成全公司——这是与批次 A delegation cap 同源的提权路径，本设计**提前堵掉**（`docs/大数据-产品需求PRD.md:1560` 也把"能改别人能看什么"列为必须留痕的元权限）。

### 2.8 scope 缓存：结论 + TTL + 失效时机

**结论：走 Redis（cache-aside），TTL 300 s；缓存"解析后的结果"而不是"配置"。**

| 决策 | 取值 | 理由 |
|---|---|---|
| 缓存什么 | `CachedScope { is_super: bool, kind: u8, dept_ids: Vec<Uuid>, self_only: bool }`（即解析后的**可见部门集合**） | 缓存配置（角色的 `data_scope` + `sys_role_dept`）需要在每次命中时重新算递归子树，等于没省；缓存结果则命中即用。**副作用**：部门树一改，所有缓存都可能失效 → 由 epoch 处理 |
| 键 | `perm:user_scope:{epoch}:{user_id}` | epoch 是 Redis 单键 `perm:user_scope_epoch`（`u64`），部门树变更时 `INCR` |
| TTL | 300 s（= `CACHE_PRINCIPAL_TTL_SECS`，`shared/constants.rs:375`） | 与既有 `perm:user_perms:/user_roles:/user_menus:` 同 TTL，运维只需记一套语义；300 s 也是"忘记失效"时的最大错误窗口 |
| 为什么用 epoch 而不是 `delete_pattern` | 部门 `parent_id` 变更 / 删部门会让**所有**用户的子树失效，逐用户清要找 `keys("perm:user_scope:*")`（框架 `keys` 是 SCAN/KEYS 语义，key 多时是阻塞风险，`alun-cache-0.4.2/src/lib.rs:58`）；`INCR` 是 O(1)，旧键靠 TTL 自然回收 | 见表格下方失效矩阵 |
| 缓存不可用 | **回退查库**（不 fail-closed） | 与既有 `cache_get` 语义一致（`auth/model.rs:62-68`）；数据权限是"过滤"，不是"鉴权"，查库慢但结果正确。**注意**：与 `auth_guard` 的 fail-closed（黑名单查不到就 401）是不同取向，别混 |

**失效矩阵（每一条都要落到实施步里，且必须有单测）**

| 事件 | 失效动作 | 依据 |
|---|---|---|
| 用户角色变更（`assign_roles`） | 删该用户键（`del perm:user_scope:{epoch}:{uid}`） | 已有同点位的 `invalidate_user_principal`（`user/handler.rs:757`），**加一行** |
| 角色 `data_scope` 变更 | 删该角色**所有持有者**的键（复用 `invalidate_role_holders` 的 `SELECT DISTINCT user_id` 反查，`auth/model.rs:126-156`） | 授权变更是低频操作，反查成本可接受（源码注释已有同样论断） |
| 角色 `sys_role_dept` 变更 | 同上一行（仍走 holder 反查） | 同上 |
| 角色删除 / 停用 | 同上一行 | 同上 |
| 用户换部门 | 删该用户键 | `batch_update_dept` / `update` 都已有 `invalidate_user_principal` 调用（`user/handler.rs:328,420,523,604`） |
| **部门树结构变更**（`parent_id` 变更 / 删部门 / 新增部门） | **`INCR perm:user_scope_epoch`**（全量失效） | 子树结果对所有用户都变；这是唯一 O(1) 的正确做法 |
| 用户被删除 | 无需处理 | 键靠 TTL 回收；且该用户已无请求 |

> **一期取舍**：`epoch` 方案要求"部门结构变更"这个动作被唯一定位。现状里部门变更在 `system/dept/handler.rs`（create/update/delete/move），有 4 个写点 —— 全部加 1 行 `scope::bump_epoch()`（列入底座报批 §5.3-#8）。若漏了某条路径，最坏结果是 300 s 内看到旧范围（TTL 兜底），不会永久错。

### 2.9 API 契约（数据权限）

前缀 `/api` 由 `[router].prefix` 注入，宏内只写 `/system/...`（`AGENTS.md` 第 8 条）。

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/system/role/{id}/data-scope` | `system:data-scope:list` | 读角色范围配置（`data_scope` + `custom_dept_ids` + 解析后的部门名） |
| PUT | `/system/role/{id}/data-scope` | `system:data-scope:update` | 改范围：`{ "data_scope": 3, "custom_dept_ids": null }`（`custom_dept_ids` 仅在 `data_scope=5` 时生效；`null` = 不动，`[]` = 清空） |
| GET | `/system/data-scope/tables` | `system:data-scope:list` | 登记表清单（`SCOPED_TABLES` 的投影：表名 / `dept_col` / `owner_col` / 是否已接入），运维核对用 |
| GET | `/system/data-scope/preview` | `system:data-scope:preview` | `?user_id=` → 该用户最终注入的 `where_sql` 原文 + 绑定参数个数 + 逐表预估计数（≤3 表，3 s 超时） |
| GET | `/system/data-scope/enums` | `system:data-scope:list` | 5 种范围的 `{label,value,color,description}`（走既有 `shared/enums.rs` 词汇层，前端不硬编码） |
| GET | `/system/data-scope/me` | 无（登录即可） | 当前用户的生效范围摘要（前端据此显示"你当前可见：本部门及以下（4 个部门）"横幅，避免用户误以为是数据丢失） |

响应示例（`GET /system/data-scope/preview?user_id=…`）:

```json
{ "code": 0, "msg": "ok", "data": {
    "user_id": "…", "username": "mgr_1_1", "is_super_admin": false,
    "roles": [{ "role_code": "DEPT_LEAD", "role_name": "部门及以下", "data_scope": 3 }],
    "merged": { "kind": "DEPT_AND_CHILD", "dept_count": 4, "self_only": false },
    "per_table": [
      { "table": "sys_user", "where_sql": "AND u.dept_id = ANY(?)", "binds": 1, "est_rows": 971 },
      { "table": "sys_dept", "where_sql": "AND id = ANY(?)", "binds": 1, "est_rows": 4 }
    ],
    "cache": { "hit": false, "ttl_secs": 300, "epoch": 7 } } }
```

> `where_sql` 是**只读展示**用途，不含真实参数值（只给个数）——避免自检页本身成为"枚举他人可见范围"的探测工具。这一点必须在实现时写进注释，否则后来者会"顺手把参数也返回方便排查"。

---



## 3. 列级脱敏（Field Masking）

### 3.1 表设计 `sys_field_mask`（V0062）

```sql
CREATE TABLE sys_field_mask (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    table_name  VARCHAR(64) NOT NULL,          -- 如 sys_user
    column_name VARCHAR(64) NOT NULL,          -- 如 mobile
    mask_type   SMALLINT    NOT NULL,          -- 1..7，见 §3.2
    mask_param  VARCHAR(128),                  -- 类型相关参数，如 '3,4'
    role_id     UUID REFERENCES sys_role(id),  -- NULL = 全局默认规则
    status      SMALLINT NOT NULL DEFAULT 1,
    reason      VARCHAR(255) NOT NULL,         -- 为什么脱敏（合规留痕）
    ...审计列, deleted_at
    CONSTRAINT ck_sys_field_mask_type CHECK (mask_type BETWEEN 1 AND 7),
    CONSTRAINT ck_sys_field_mask_ident CHECK (
        table_name ~ '^[a-z][a-z0-9_]{1,63}$' AND column_name ~ '^[a-z][a-z0-9_]{1,63}$')
);
CREATE UNIQUE INDEX uk_sys_field_mask_rule
    ON sys_field_mask (table_name, column_name, role_id) NULLS NOT DISTINCT
    WHERE deleted_at IS NULL;
```

三个设计点，都有实测依据：

1. **`NULLS NOT DISTINCT`（PG 15+，本项目 PG 18.6 支持）**：让"全局规则（`role_id IS NULL`）"也受唯一约束管。POC-7 实测：第二次插入同 `(sys_user, mobile, NULL)` 被拒。若用普通唯一索引，NULL 互不相等 → 会出现多条全局规则互相覆盖。
2. **`CHECK` 正则校验表名/列名**：这张表的 `table_name`/`column_name` 最终要参与"按表取规则"，是**唯一**可能被注入到 SQL 标识符位置的字段。POC-7 实测 `'sys_user; DROP TABLE sys_user'` 被拒。
3. **`role_id` 在规则表里，而不是单独 `sys_field_mask_role`**：一条规则要么全局要么属于**一个**角色（"规则适用角色集合"会让规则组合爆炸，且无法回答"这条规则到底是谁加的"）。用户持有多角色时按 **OR** 命中（任一角色有规则 → 脱敏），因为脱敏是"限制"，取并集才安全（与 scope 的并集方向相反——这点必须在代码注释里写明，否则后来者会"统一成并集"而放行明文）。

> 遗留语义：`sys_role` 软删后 `sys_field_mask.role_id` 仍指向它。解析时 `JOIN sys_role ... AND deleted_at IS NULL`，指向已删角色的规则**不生效**（不是"变全局"——这一点必须有单测，否则软删角色会静默放宽脱敏）。

### 3.2 七种脱敏类型 + 实现对照

| # | 类型 | 参数 | 示例（输入 → 输出） | 实现 |
|---|---|---|---|---|
| 1 | 手机号 | `保留前N,保留后M`（默认 `3,4`） | `13812345678` → `138****5678` | `Mask::mobile`（仅 N=3,M=4 时；其他参数用自研的按字符切片版本） |
| 2 | 身份证 | `保留前N,保留后M`（默认 `4,4`） | `110101199001011234` → `1101**********1234` | `Mask::id_card` |
| 3 | 邮箱 | 无 | `alice@mail.com` → `a***@mail.com` | `Mask::email` |
| 4 | 姓名 | `保留前N`（默认 `1`） | `张三丰` → `张**` | `Mask::name`（**注意**：它按 `chars()` 处理，多字节安全，POC-9 已验证） |
| 5 | 银行卡 | `保留前4,保留后4` | `6222021234567890` → `6222 **** 7890` | `Mask::bank_card` |
| 6 | 金额 | `区间宽度`（如 `1000` 表示按千元取整） | `48620.00` → `48000~49000` | 自研（`Mask` 无金额能力；必须用 `Decimal` 计算，**禁止**用 f64——金额精度是批次 A 已立的规矩） |
| 7 | 自定义正则 | `regex` + `保留组`（如 `^(\w{2}).*$` + `1`） | `AB12345` → `AB*****` | 自研，用 `regex 1.13.1`（已在 `Cargo.lock` 里，由 `alun-utils` 间接引入 → 显式化，零新 crate） |

**统一约束（全部类型都必须满足，写进 `mask.rs` 的单测）**：
- 输入为空/纯空白 → 原样返回（不产生 `""` → `"****"` 这种噪音）；
- 输出**永不 panic**：所有切片按 `char_indices()` 边界处理。这一条是被 POC-9 逼出来的（`alun_utils::Mask::mask_by_type` 的兜底分支在 `"张三"` 上 panic，`mask.rs:198`）——所以 **`mask_by_type` 禁止出现在本批代码里**，并加一条 CI grep 断言；
- 幂等：`mask(mask(x)) == mask(x)`（避免二次脱敏把掩码也吃掉）；
- 长度不变性：不满足最小长度的值（如 5 位手机号）原样返回而不是抛错——脏数据不该让列表接口 500。

### 3.3 落地机制：三方案对比 + 结论

| 维度 | **(a) DTO 出参前脱敏**（推荐） | **(b) 自定义 serde 序列化器 + 动态判定** | **(c) SQL 层脱敏（PG 函数）** |
|---|---|---|---|
| 怎么落 | 登记的 DTO 实现 `Maskable::mask(&mut self, set: &MaskSet)`；handler 在 `Res::ok(...)` 之前调一次 | 给每个敏感字段加 `#[serde(serialize_with = "...")]`，序列化器从 thread-local / task-local 取当前用户 | `SELECT regexp_replace(mobile, …) AS mobile` |
| 上下文来源 | 显式参数（`MaskSet` 由 handler 传入）→ **可测、可见、无隐式状态** | 隐式全局状态：`serde` 序列化发生在 `Res` 渲染时，**没有请求上下文**；只能 thread_local（axum 多线程 + tokio 任务迁移 → 需 `tokio::task_local!` 且要在每个 handler 里绑定，等于每个 handler 都要改） | SQL 里（无法感知调用者，需在拼 SQL 时把 mask 表达式拼进去） |
| 参数错位/注入风险 | 无（不碰 SQL） | 无 | **高**：mask 表达式含列名与正则，天然是"拼 SQL"；且 `SELECT *` 的写法（项目里 30+ 处 `SELECT *`，见 `role/model.rs:115`）**根本无从替换** |
| 实测代价 | 内存中字符串处理，20 万行 ≈ 数毫秒（纯 CPU，无正则回溯风险） | 同 (a)（最终也是字符串处理），但**每字段一次虚调用 dispatch**，且 thread-local 取值有额外开销 | **POC-7b：10.06 ms → 140.25 ms（14×）**，20 万行 |
| 对过滤/排序的影响 | 无（SQL 仍按明文过滤，出参才脱敏） | 无 | **致命**：POC-7b 实测同一行 `mobile='139…0001'` 明文命中 **1**、脱敏后命中 **0** → 用户搜自己搜不到；金额区间化后 `SUM` 失真 |
| 覆盖导出 | 需单独在 `format_export_rows` 加（§3.7） | 若导出走 serde → 自动覆盖；但导出走 xlsx 不走 serde，**不覆盖** | 覆盖（同一 SQL） |
| 绕过风险 | 新增接口忘了调 `mask()` → 明文泄露。靠 §3.4 三道闸防 | 序列化器只能按字段名判断，"这个字段属于哪张表"信息丢失（`real_name` 在用户 DTO 里是姓名，在通知 DTO 里可能是"发送人姓名"）→ 误脱敏 | `SELECT *` 无从下手；聚合/子查询里的列更难覆盖 |
| 结论 | ✅ **采用** | ❌ 排除（隐式上下文 + 字段名歧义） | ❌ 排除（代价 + 破坏性 + 无法覆盖 `SELECT *`） |

**实现形态（接口契约）**

```
business/shared/mask.rs
├── pub enum MaskType { Mobile, IdCard, Email, Name, BankCard, Amount, Regex }
├── pub struct MaskRule { table: &'static str, column: &'static str, ty: MaskType, param: Option<String> }
├── pub struct MaskSet(Vec<MaskRule>)                       // 一次请求解析一次（可缓存，见下）
│     ├── fn hits(&self, table: &str, column: &str) -> Option<(&MaskType, Option<&str>)>
│     └── fn is_empty(&self) -> bool
├── pub async fn resolve(principal: &ScopedPrincipal) -> Result<MaskSet, DbError>
├── pub trait Maskable { fn mask(&mut self, set: &MaskSet); }   // DTO 实现，纯内存操作
└── pub fn apply_value(ty: MaskType, param: Option<&str>, raw: &str) -> String   // 纯函数，7 类型全覆盖
```

**`MaskSet` 的缓存**：与 scope 同源（同一批失效事件），键 `perm:user_mask:{epoch}:{uid}`，TTL 300 s。规则表极小（预计 < 50 行），也可以**进程内缓存全表 + 版本号**（`sys_field_mask` 的 `updated_at` 最大值做版本），本设计选 Redis 与 scope 统一，减少一套机制。

### 3.4 防漏：为什么"新增字段忘了登记"会被抓住（三道闸）

**闸 1 · 编译期（这一闸是真正的强制力）**

每个登记为"含敏感列"的表，其 DTO 在 `business/permission/mask/registry.rs` 里必须提供**两个**东西：

1. `impl Maskable for XxxRes { fn mask(&mut self, set: &MaskSet) { … } }`
2. **测试用的全字段样例构造器**：`#[cfg(test)] fn sample_for_mask_test() -> XxxRes`

关键在第 2 点：这个函数写 **struct literal 且不使用 `..Default::default()`**。于是**任何人给 `XxxRes` 加字段，第 2 点就编译不过**——编译器会强制他去补样例值。补完样例后，闸 2 的测试会用一个"敏感值填充"的样例走一遍序列化 + 正则扫描；如果新字段是敏感列而没进 `Maskable::mask`，扫描就会发现明文手机号/身份证/邮箱 → 测试红。

- 它**能**保证：新增字段必须被"看见"（编译期），敏感值不会静默出现在出参里（测试期）。
- 它**不能**保证：一个既非手机号/身份证/邮箱形态的敏感值（如内部工号 `EMP20260001`）——这类必须靠人登记，测试抓不到。诚实写明，并把它列为 §5.6 风险 R6。

**闸 2 · 测试期：全量敏感值扫描**

对注册表里每个 DTO：构造"每个字符串字段都填最敏感形态"的样例 → 调 `mask()`（非超管 MaskSet）→ `serde_json::to_value` → 递归扫描所有字符串，断言：
- 不匹配 `^1[3-9]\d{9}$`（手机号）
- 不匹配 `^\d{17}[\dXx]$`（身份证）
- 不匹配 `[\w.\-]+@[\w.\-]+\.\w+`（邮箱）
- 且 `mask()` 前后**至少有一个字段发生变化**（防止"实现了空 mask"假通过）

**闸 3 · 运行期：导出 fail-closed + 启动自检**

- 导出路径（§3.7）：`sensitive = true` 的列如果解析不到规则 → **422 拒绝导出**，返回明确文案。这保证"有敏感列没配规则"这件事**在第一次导出时就暴露**，而不是出一份明文 Excel 流到微信群里。
- 启动自检：遍历所有已注册 `ExportDefinition`，收集 `sensitive: true` 的 `(table, column)`，与 `sys_field_mask` 比对；缺失则 `tracing::error!` + 在 prod 下 `exit(1)`。这比"第一次导出才失败"更早。

### 3.5 权限联动：按角色、按范围，还是组合？

**结论：按「角色 ∪ 全局」判定；范围（scope）不参与脱敏判定。**

理由（逐条）：
1. **语义正交**：scope 回答"能看哪些**行**"，脱敏回答"看得到的行里**哪些**值是明文"。把 scope 塞进脱敏判定会产生"同一行数据，因为我范围小所以看到明文"这种反直觉结论（范围小 = 权限小，却看到更多明文）。
2. **可解释性**：运维回答"他为什么看到 `138****5678`"只需查角色 → 规则；若叠加范围，需要还原"他的范围 + 该规则的范围条件"两个维度。
3. **实现成本**：`MaskSet` 只依赖"角色集合"，可在登录时/首请求时解析一次并缓存 300 s；若叠加 scope，则 scope 一变（部门搬迁）脱敏清单也要跟着变，失效矩阵翻倍。
4. **规则表达力足够**：确实需要"数据管理员看明文、业务员看脱敏"时，做法是给数据管理员**单独的角色 + 该角色配置的规则/或用 `system:field-mask:plain` 权限码**（§3.6），而不是给规则加范围条件。

**组合场景的实际落法（举例）**：若业务要求"采购部内部看本人手机号明文，看别人必须脱敏"——本批**不支持**（需要"本人行免脱敏"的能力）。这条能力放到 §5.6 风险 R7 里显式记录为"已知不支持，需时再设计"，并给出低成本近似方案：DTO 层比较 `mask_set` 与 `dto.id == principal.user_id` 时跳过本人字段（约 10 行）——但**本批不做**，因为会引入"同一字段在不同行脱敏策略不同"的测试矩阵。

### 3.6 谁不受脱敏

| 主体 | 是否脱敏 | 依据 |
|---|---|---|
| `SUPER_ADMIN` 角色持有者 | **不脱敏** | 与"超管短路全部校验"的总口径一致（`shared/constants.rs:122-124`）；且超管本就该能核对数据 |
| 持 `system:field-mask:plain` 权限码的人 | **不脱敏**，但**每次明文访问必须落 `audit_logs`** | 元权限必须留痕：写一条 `FIELD_MASK_PLAIN_READ`，字段含 `table/columns/row_count/route`。没有这条留痕，"看明文"就是不可审计的后门 |
| 任何人（含超管）在**操作日志**里 | 见下 | `operation_logs` 的请求/响应体已由 `mask_json_value` 递归脱敏（`middleware/operation_log.rs:285` + `SENSITIVE_FIELDS`），与本批机制**独立且不冲突**；本批只在 `SENSITIVE_FIELDS` 的语义注释上补一句"这是落库前的日志脱敏，与列表出参脱敏是两套"（防误合并） |

**判定实现**：`principal.is_super`（库侧角色编码）**或** `claims.has_permission("system:field-mask:plain")` → `MaskSet::empty()`。

### 3.7 导出 / 导入时的脱敏（必须做，否则绕过的就是这条路）

**现状**（两处调用点，同一函数）：

```
业务导出：impexp/handler.rs:780   format_export_rows(definition.columns(), &rows, &dicts)
异步导出：impexp/worker.rs:607    engine::format_export_rows(definition.columns(), &rows, &dicts)
                    ↓
       engine.rs:408-437  format_export_rows(...)      ← 唯一改点
```

**改法（1 个函数，3 个参数变化）**：

1. `format_export_rows(columns, rows, dicts, mask: &MaskSet)` —— 新增 `mask` 参数。签名变更会**强制**两个调用点都编译失败，这是有意的（防止漏改一条路径）。
2. 对每列：`if column.sensitive { match mask.hits(definition.table(), column.key) { Some(rule) => 出参填脱敏值, None => return Err(422 FAIL_CLOSED) } }`——**fail-closed**（§3.4 闸 3）。
3. 异步路径的身份来源：`sys_export_task.create_by`（`V0041:95` 已存在）→ worker 处理时按 `create_by` 重新 `resolve(MaskSet)`，**不能用"发起导出那一刻的 MaskSet 快照"**：导出任务可能跑 10 分钟，期间管理员可能收紧了规则；用当前规则更安全（宁可在任务完成前改变结果，也不要出一份"按旧规则"的明文文件）。同时把"本次是否脱敏"写进 `sys_export_task.mask_applied`（**V0067 加列**），产物可审计。
4. **导入不需要脱敏**（导入是"写"，脱敏是"读"）。但导入的**错误明细导出**（`result_file_id`）走的是同一条 `format_export_rows` 路径 → 自动被覆盖。

**额外硬约束**：`ExportColumn.table` 需要新增一个字段（`&'static str`）把"列属于哪张表"显式写进定义里——现有 `ExportColumn`（`definition.rs:620-633`）只有 key/header/kind/width/sensitive，**没有表名**，无法查规则。这是 `business/` 内的改动（不涉及底座），但会影响所有 `ExportDefinition` 实现（当前 1 个内建实现 `dict_item.rs` + 未来 N 个），列入 §5.2 的 `V0067` 同步工作。

### 3.8 API 契约（脱敏）

前缀 `/api` 由 `[router].prefix` 注入，宏内只写 `/system/field-mask/...`（项目约定，`AGENTS.md` 第 8 条）。

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/system/field-mask/page` | `system:field-mask:list` | 规则分页（`?table_name=&column_name=&scope=global\|role`） |
| POST | `/system/field-mask` | `system:field-mask:create` | 新建规则（校验表/列存在于 `information_schema`；`role_id` 为空 = 全局） |
| PUT | `/system/field-mask/{id}` | `system:field-mask:update` | 修改（`mask_type`/`mask_param`/`status`/`reason`） |
| DELETE | `/system/field-mask/{id}` | `system:field-mask:delete` | 软删 |
| GET | `/system/field-mask/preview` | `system:field-mask:list` | `?table_name=&column_name=&value=` → 7 种类型的试运行结果（含参数校验错误提示） |
| GET | `/system/field-mask/effective` | `system:field-mask:list` | `?user_id=` → 该用户**实际生效**的规则清单（含来源：全局 / 角色名） |

响应示例（`GET /system/field-mask/effective?user_id=…`）：

```json
{ "code": 0, "msg": "ok", "data": [
  { "table_name": "sys_user", "column_name": "mobile", "mask_type": 1, "mask_param": "3,4",
    "source": "GLOBAL", "source_name": null, "sample": "138****5678" },
  { "table_name": "sys_user", "column_name": "email", "mask_type": 3, "mask_param": null,
    "source": "ROLE", "source_name": "部门主管", "sample": "a***@mail.com" } ] }
```

---

## 4. 角色互斥（SoD, Separation of Duties）

### 4.1 表设计 `sys_role_mutex`（V0063）+ 双向语义

```sql
CREATE TABLE sys_role_mutex (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    role_a_id UUID NOT NULL REFERENCES sys_role(id),
    role_b_id UUID NOT NULL REFERENCES sys_role(id),
    reason    VARCHAR(255) NOT NULL,           -- 为什么互斥（审计/合规留痕，必填）
    ...create_by, created_at, deleted_at,
    CONSTRAINT ck_sys_role_mutex_distinct CHECK (role_a_id <> role_b_id)
);
CREATE UNIQUE INDEX uk_sys_role_mutex_pair
    ON sys_role_mutex (LEAST(role_a_id, role_b_id), GREATEST(role_a_id, role_b_id))
    WHERE deleted_at IS NULL;
```

**是否双向**：**是，且由数据库强制**。`A 互斥 B` 与 `B 互斥 A` 是同一条规则（互斥是对称关系）。实现用 **`LEAST/GREATEST` 表达式唯一索引**：写入时无论顺序如何，规范化后的键相同 → 第二次写入被拒。POC-6 实测：先插 `(DEPT_LEAD, AUDITOR)`，再插 `(AUDITOR, DEPT_LEAD)` 被 `uk_sys_role_mutex_pair` 拒绝。

> 为什么不在应用层"查询时双向匹配"就够了：那样库里可以存两条互为反向的记录，之后删掉一条，另一条还在生效——语义漂移。唯一索引让"同一规则只有一行"成为**数据不变量**。同时读取时仍用 `(a=? AND b=?) OR (a=? AND b=?)` 双条件（不能假设写入顺序）。

### 4.2 校验点（三处）+ 校验 SQL

**校验 SQL 的唯一正确形态（POC-6b 已逐例验证）**：

```
-- 输入：$1 = 目标用户, $2 = 本次要授予的 role_id 数组
WITH held AS (
    SELECT role_id FROM sys_user_role
     WHERE user_id = $1 AND (expire_time IS NULL OR expire_time > now())
    UNION                                   -- ← 关键：UNION 而不是「本次 vs 已持有」
    SELECT unnest($2::uuid[])
)
SELECT a.role_id, b.role_id, m.reason
FROM held a JOIN held b ON a.role_id < b.role_id
JOIN sys_role_mutex m
  ON LEAST(m.role_a_id, m.role_b_id) = a.role_id
 AND GREATEST(m.role_a_id, m.role_b_id) = b.role_id
WHERE m.deleted_at IS NULL
```

实测矩阵（POC-6b，同一份 SQL）：

| 用例 | 已持有 | 本次授予 | 期望 | 实测 |
|---|---|---|---|---|
| 6a | DEPT_LEAD | AUDITOR | 命中 1 | ✅ 1 |
| 6b | DEPT_LEAD | STAFF | 0 | ✅ 0 |
| 6c | DEPT_LEAD | **DEPT_LEAD + AUDITOR（同次）** | 命中 1（否则可绕过） | ✅ 1 |
| 6d | DEPT_MGR（**已过期**） | STAFF | 0（过期绑定不算持有） | ✅ 0 |

**三个校验点**：

| # | 场景 | 校验内容 | 落点 | 失败返回 |
|---|---|---|---|---|
| 1 | 给用户授予角色（`PUT /system/user/{id}/roles`，单角色与批量走同一接口） | `held ∪ requested` 两两配对 | `system/user/handler.rs:717`（`assign_roles`）在 `guard::ensure_role_grant_allowed` **之后**插 1 行 `sod::ensure_no_mutex(...)` | 409 `ROLE_MUTEX_CONFLICT` + `data.conflicts[{role_a,role_b,reason}]` |
| 2 | 新建互斥规则（`POST /system/role-mutex`） | 该 `(a,b)` 是否存在"同时持有"的存量用户 | `business/permission/sod/handler.rs`（新代码） | **409 `ROLE_MUTEX_HAS_HOLDERS`** + 冲突用户清单（前 20 条）+ `total` → 让调用方先解绑 |
| 3 | 删除互斥规则 | 无校验（删除是放松约束，不需要检查存量） | — | — |

**校验点 1 的顺序（三层，不可交换）**：

```
① 框架 #[alun::permission("system:user:role")]      → 403 无权（框架层）
② guard::ensure_role_grant_allowed(claims, …)      → 403 超管/提权相关（批次 A 已实现）
③ sod::ensure_no_mutex(user_id, &role_ids)         → 409 互斥（本批新增）
④ 事务：replace_user_roles + 令牌版本自增 + 强制下线（批次 A 已实现）
```

**为什么 ③ 必须在 ② 之后**：② 会先拒掉"非超管授予超管角色"这类更严重的违规；如果 ③ 先跑，用户会先看到"角色互斥"提示，掩盖真正的越权意图（也是安全审计希望优先看到的信号）。**为什么都在事务之前**：互斥冲突是"可预判"的业务规则，不需要在事务里查（事务里查只会让冲突路径也持锁）。

### 4.3 修改角色的互斥配置时，已有用户同时持有这两个角色怎么办

**结论：阻止配置（409），并给出「冲突用户清单 + 一键跳转解绑」；同时提供独立的预检接口供前端先查。**

| 候选方案 | 结论 | 理由 |
|---|---|---|
| **A. 阻止配置**（本设计） | ✅ 采用 | SoD 的意义是"不可共存"。允许配置后再告警 = 规则从生效第一天就是失效的；且告警无 SLA（谁看？谁处理？） |
| B. 允许但告警 | ❌ | 违反 SoD 语义；告警渠道（站内信）已在批次 A 可用，但"已知违规"状态会长期存在 |
| C. 要求先解绑（系统自动解绑） | ❌ | 自动解除他人角色是**高危不可逆**动作，且会静默改变用户权限（用户第二天发现自己进不去某个模块）。解绑必须由管理员显式操作，且留 `audit_logs` |
| D. 先解绑 → 再配置（人工两步） | ✅ 作为 A 的**后续流程** | 409 的响应里给出冲突用户清单 + 前端链接到「用户管理 → 该用户 → 角色」页，管理员自己解绑后重试配置 |

**前端体验（必须按这个顺序做，否则管理员会卡住）**：

1. 打开"新建互斥"表单 → 选 A、B → 点"校验冲突"（`GET /system/role-mutex/conflict?role_a_id=&role_b_id=`）→ 如果 0 个冲突用户，按钮变成可提交；
2. 如果 >0：表格列出用户（`username / real_name / dept_name`），顶部黄色横幅："以下 **12** 个用户同时持有这两个角色，需先解除其中一个角色才能建立互斥关系。"，每行给"去处理"链接；
3. 直接提交（跳过预检）时后端仍会 409（**后端是权威，前端预检只是体验**），返回同样结构，前端渲染同一张表。

### 4.4 与批次 A「权限上限（delegation cap）」的协同

| 维度 | 批次 A 的 cap | 本批 SoD | 协同规则 |
|---|---|---|---|
| 管什么 | **操作者资格**：能不能动这个对象/授这个角色（人 × 权限码） | **被授予角色的组合合法性**（角色 × 角色） | 两者正交，都要过 |
| 超管 | 不受限 | 不受限（但超管也应看到冲突提示——见下） | 超管可以建互斥、可以给用户同时持有互斥角色吗？**不可以**：互斥是"数据完整性"约束，不是"提权"约束。所以 `sod::ensure_no_mutex` **对超管同样生效**。这是与 cap 的一个**刻意差异**，必须在代码注释与 API 文档里写明，否则实施时会"顺手给超管加个短路" |
| 错误码 | `PERMISSION_DENIED(30001)` | `ROLE_MUTEX_CONFLICT(6100x)` | 不同码，前端文案不同：前者"你没有权限"，后者"这两个角色不能同时授予" |
| 实现位置 | `system/user/guard.rs`（底座已有） | `business/permission/sod/`（新代码）+ 底座 1 行调用 | 见 §5.3-#6 |

**超管也不例外的例外**：**种子/初始化路径**必须能建立互斥（例如 `V0068` 想预置一条基线互斥规则，而库里已有用户同时持有）。所以 `sod::ensure_no_mutex` 只挂在 **API 校验点**，不挂在"迁移/种子"路径上——迁移用 SQL，天然不受 Rust 校验约束；但这样会让"迁移预置的互斥"与"存量用户违规"共存。因此 **本设计不预置任何互斥规则**（`V0063` 只建表），基线规则由管理员在预检通过后再建。

### 4.5 错误文案（前端直接可用）

| 场景 | HTTP | code | 文案 |
|---|---|---|---|
| 授予角色命中互斥 | 409 | `ROLE_MUTEX_CONFLICT` | `角色「{role_b_name}」与用户已持有的「{role_a_name}」互斥：{reason}。请先解除其中一个角色。` |
| 批量授予中多组冲突 | 409 | `ROLE_MUTEX_CONFLICT` | `本次授权命中 {n} 组互斥关系（{pairs}），请先解除冲突角色。` |
| 新建互斥但存量有冲突 | 409 | `ROLE_MUTEX_HAS_HOLDERS` | `无法建立互斥关系：{total} 个用户同时持有「{a}」与「{b}」，请先解除他们的其中一个角色。` |
| 互斥规则重复（含反向） | 409 | `ROLE_MUTEX_DUPLICATE` | `「{a}」与「{b}」的互斥关系已存在（互斥关系无方向，A 与 B 视为同一条）。` |
| 互斥自己 | 400 | `PARAM_INVALID` | `不能建立角色与自身的互斥关系。` |
| 未填理由 | 400 | `PARAM_INVALID` | `请填写互斥理由（用于合规留痕）。` |
| 非超管试图改自己持有的角色范围 | 403 | `PERMISSION_DENIED` | `不能修改自己持有角色的数据范围（避免自我扩权）。` |

### 4.6 API 契约（SoD）

| 方法 | 路径 | 权限码 | 说明 |
|---|---|---|---|
| GET | `/system/role-mutex/page` | `system:role-mutex:list` | 互斥组分页（返回已规范化的 `role_a/role_b` 名称，`a < b` 按 role_code 字典序输出，避免前端每次自己排序） |
| POST | `/system/role-mutex` | `system:role-mutex:create` | 新建（**先跑存量冲突检查**，有冲突则 409 不落库） |
| DELETE | `/system/role-mutex/{id}` | `system:role-mutex:delete` | 软删（放松约束，不检查存量） |
| GET | `/system/role-mutex/conflict` | `system:role-mutex:conflict` | `?role_a_id=&role_b_id=` → 冲突用户分页（预检，不落库） |
| GET | `/system/role-mutex/check-user` | `system:role-mutex:conflict` | `?user_id=` → 该用户当前持有的角色里已有的互斥对（详情页提示用） |

---
## 5. 横切设计

### 5.1 权限码清单（三模块 · 12 个业务码 + 3 个菜单载体）

命名沿用 `{模块}:{资源}:{动作}`（`AGENTS.md` 第 7 条），**逐字**与 `#[alun::permission]` 声明、`sys_menu.auth_code` 一致（三处不一致 = 接口 403，这是批次 A 已经踩过的坑，见 `V0045` 头注释）。

| # | 权限码 | HTTP | 语义 | 授予 |
|---|---|---|---|---|
| 1 | `system:data-scope:list` | GET | 查看角色数据范围配置 | 超管 |
| 2 | `system:data-scope:update` | PUT | 修改角色数据范围（含自定义部门集合） | 超管（刻意不授其它角色） |
| 3 | `system:data-scope:preview` | GET | 数据权限自检（预览某用户最终 WHERE + 可见行数） | 超管 |
| 4 | `system:field-mask:list` | GET | 查看脱敏规则 / 生效清单 / 试运行 | 超管 |
| 5 | `system:field-mask:create` | POST | 新建脱敏规则 | 超管 |
| 6 | `system:field-mask:update` | PUT | 修改脱敏规则 | 超管 |
| 7 | `system:field-mask:delete` | DELETE | 删除脱敏规则 | 超管 |
| 8 | `system:field-mask:plain` | —（判定位） | **查看未脱敏明文**（豁免脱敏） | 超管；需明文日常作业的角色（如 HR 专员）经审批后授予 |
| 9 | `system:role-mutex:list` | GET | 查看互斥组 | 超管 |
| 10 | `system:role-mutex:create` | POST | 新建互斥组 | 超管（合规/风控专员可授） |
| 11 | `system:role-mutex:delete` | DELETE | 删除互斥组 | 超管 |
| 12 | `system:role-mutex:conflict` | GET | 冲突用户预检 / 用户互斥体检 | 超管 + 所有能管角色的角色（否则管理员授不出角色时看不到冲突原因） |

**权限码设计取舍**：

- `:plain` 没有对应路由（它是"判定位"）。项目的 `#[alun::permission]` 只校验声明了路由的 handler；`plain` 通过在 handler 内 `claims.has_permission(...)` 读取。**为什么不给它挂一个路由**：挂路由就得有个端点，而豁免的语义是"作用于所有读接口"，不是"一个开关接口"。为让它在 `sys_menu` 里可勾选，仍建一个 `menu_type=3` 的按钮节点（与 `V0045` 的 `business:notice:mine` 同例）。
- 三个模块的写权限**刻意只授超管**：它们是"能改别人能看什么"的元权限（同 `docs/大数据-产品需求PRD.md:1560` 的结论）。`V0065` 只给 `SUPER_ADMIN` 授 15 个节点（3 菜单 + 12 按钮）。
- **`system:role-mutex:conflict` 例外**：它只读、不改变态，且"授角色"的日常操作者需要它才知道为什么被拒。若只授超管，非超管管理员点保存会拿到 409 但打不开冲突清单（因为清单接口 403）——体验断裂。所以建议在所有**持有 `system:user:role` 的角色**上追加授予（`V0068` 提供该 SQL，但默认不执行，由运维按组织现状决定）。

### 5.2 迁移分配（V0060–V0069）

**全部放 `backend/migrations/business/`**，理由与批次 A 相同（`docs/批次A-技术架构.md:791` 的论证）：迁移与 `src/business/` 代码同生命周期、一起评审；且 **`migrations/system/` 目录本身不动**（红线）。

> 需要明说的偏离：`V0060`（`ALTER sys_role`）、`V0061`（`sys_role_dept`）操作的是 `system/` 域的表。**迁移文件放 business 目录不等于"不是底座改动"**——所以这两条同时出现在 §5.3 的报批清单里（#1、#2），不能因为"文件在 business 下"就绕过报批。

| 版本 | 文件名 | 内容 | `.down.sql` |
|---|---|---|---|
| **V0060** | `alter_sys_role_add_data_scope` | `ALTER TABLE sys_role ADD data_scope SMALLINT NOT NULL DEFAULT 1` + `CHECK (1..5)` + 列注释 | `DROP CONSTRAINT` + `DROP COLUMN` |
| **V0061** | `create_sys_role_dept` | 建表（`role_id`/`dept_id` 双 FK + `UNIQUE`）+ 2 索引 | `DROP TABLE` |
| **V0062** | `create_sys_field_mask` | 建表 + `NULLS NOT DISTINCT` 唯一索引 + 标识符 `CHECK` + 1 索引 | `DROP TABLE` |
| **V0063** | `create_sys_role_mutex` | 建表 + `LEAST/GREATEST` 对称唯一索引 + `CHECK` 自反 | `DROP TABLE` |
| **V0064** | `batchb_indexes` | `idx_sys_user_dept_created (dept_id, created_at DESC) WHERE deleted_at IS NULL`、`idx_sys_role_dept_dept`、`idx_sys_field_mask_table`、`idx_sys_role_mutex_b` | `DROP INDEX` ×4 |
| **V0065** | `seed_batchb_permissions` | 15 个 `sys_menu` 节点（3 菜单 + 12 按钮）+ `SUPER_ADMIN` 全授 + 非超管不授 | 按固定 UUID `DELETE`（菜单 + 授权） |
| **V0066** | `seed_default_field_masks` | 3 条默认规则：`sys_user.mobile`(1,3,4) / `.email`(3) / `.real_name`(4,1)，全局（`role_id NULL`） | 按固定 UUID `DELETE` |
| **V0067** | `alter_export_task_add_mask_applied` | `sys_export_task ADD mask_applied BOOLEAN NOT NULL DEFAULT FALSE` + 列注释；同批**代码侧**给 `ExportColumn` 加 `table: &'static str`（Rust 改动，无迁移） | `DROP COLUMN` |
| V0068 | **保留（基线收紧模板，默认不执行）** | ① `UPDATE sys_role SET data_scope = 4 WHERE role_code NOT IN ('SUPER_ADMIN')` 的**注释版**（说明后果 + 回滚 SQL）；② 给持有 `system:user:role` 的角色补授 `system:role-mutex:conflict` | 对应反向 SQL（同样注释版） |
| V0069 | **保留（实施期修正）** | 批次 A 的 `V0050–V0052` 就是实施期追加的（`alter_impexp_task_runtime` / `notice_scheduled_dispatch` / `seed_impexp_export_retry`）；本批预留一个号给同类需要 | — |

**为什么 V0068 是"注释版"而不是"可执行 SQL"**：它改的是**所有角色的可见范围**，属于"业务策略决定"，不是"schema 迁移"。把它做成迁移会在 `cargo run` 自动迁移时静默执行——这正是本设计极力避免的"无预告行为变更"（§2.2 默认值讨论）。保留成文档化模板 + 运维手动执行 + 强制留痕，是唯一负责的做法。

**号段空隙说明**：`V0053–V0059` 不分配。原因：批次 A 已用到 `V0052`，若本批从 `V0053` 连续分配，将来"批次 A 的补丁迁移"就没有号可用（`V0050–V0052` 正是这种补丁）。留 7 个号给批次 A 的线上修复，是运维友好性。

### 5.3 必须报批的底座改动（11 项，逐项说明"为什么必须动"）

> 判定标准：凡是**改 `system/`、`support/` 的 Rust 文件**，或**改这两域的表结构**，都算底座改动。每项给出：改哪个文件、多少行、不动它会怎样。

| # | 文件 | 改动 | 行数量级 | **为什么必须动底座（不动会怎样）** |
|---|---|---|---|---|
| 1 | `migrations/business/V0060` | `ALTER TABLE sys_role ADD data_scope` | 6 行 | `sys_role` 是底座表；数据范围只能挂在角色上（挂用户表会和"角色决定权限"的模型冲突，挂独立映射表则每查一次范围要两跳）。不动 = 无处存范围 |
| 2 | `migrations/business/V0061` | 新建 `sys_role_dept` | 14 行 | 自定义部门集合必须持久化；不动 = `data_scope=5` 无法配置 |
| 3 | `system/role/model.rs::SysRoleModel` | 加 `pub data_scope: i16` 字段（1 行 + 注释） | ~5 行 | `Model` 派生的 `insert/update/from_row` 按字段清单生成 SQL。不加字段 → `INSERT INTO sys_role` 不含该列（落默认 1），**`UPDATE` 时把 data_scope 悄悄写回默认值**（因为整行 update 用 `SysRoleModel` 全字段）。这是"必须"而非"顺便" |
| 4 | `system/role/dto.rs` | `RoleRes` / `RoleSaveReq` 加 `data_scope: Option<i16>` | ~10 行 | `RoleSaveReq` 是 **PUT 全量更新**（`role/handler.rs:231-274` 用 `req` 重建 model）。不加 → 任何一次"改角色名"都会把 `data_scope` 重置为默认。这不是功能缺失，是**数据破坏** |
| 5 | `system/role/handler.rs` | ① `create`/`update` 透传 `data_scope`（含 `from_i16` 校验）；② `update` 里加 1 行 `scope::ensure_scope_grant_allowed(&claims, new_scope, &custom_dept_ids)`；③ `assign_menus` 之后若涉及 `system:data-scope:*` 授权则 bump 缓存 | ~25 行 | ② 的"范围上限"校验必须发生在写库前，而写库点在底座；③ 角色授权变化会影响脱敏/范围缓存，这是底座唯一知道的时机 |
| 6 | `system/user/handler.rs::assign_roles`（:717） | 插 1 行 `sod::ensure_no_mutex(&id, &role_ids).await?;`（在 `guard::ensure_role_grant_allowed` 之后） | **1 行** | 这是**唯一**的用户角色写入权威点（`replace_user_roles` 是 transaction 内部函数，绕开它写就是绕过所有校验）。不在这里插 = 互斥可被"正常接口"绕过 |
| 7 | `system/user/model.rs::page_users`（:179） | 改用 `scope::paginate`（替掉 `db().paginate_with_params`），签名加 `&ScopedPrincipal` | ~20 行 | 用户分页的 SQL 在底座。不改 = 本批最核心的"人员可见范围"完全没生效（唯一出路是"把 page_users 复制一份到 business/ 再改"，那是更糟的重复维护） |
| 8 | `system/dept/model.rs` + `handler.rs` | 树查询加 scope 裁剪；4 个写点（create/update/delete/move）加 `scope::bump_epoch()` | ~30 行 | 部门树在底座；`bump_epoch` 必须挂在底座写点（唯一知道"树变了"的地方） |
| 9 | `bootstrap.rs` | `tokio::join!` 里加 `business::shared::guard::preflight_permission()?`（1 行）；prod 下自检失败 `exit(1)` | ~5 行 | 启动钩子是**唯一**在"资源就绪、插件未启动"时执行的入口（`bootstrap.rs:3-5` 注释），也是项目已确立的 fail-fast 位置（`bootstrap.rs:189-200`） |
| 10 | `shared/error_code.rs` | 追加 `6100x` 段：12 个码（见 §5.3 末表） | ~30 行 | 项目铁律"错误码统一在此定义，禁止散落魔法数字"（`error_code.rs` 头注释）。加在 `business/` 里会形成第二处码表 → 前端要维护两套映射 |
| 11 | `shared/constants.rs` | 追加：`CACHE_KEY_PREFIX_USER_SCOPE`、`CACHE_KEY_USER_SCOPE_EPOCH`、`CACHE_KEY_PREFIX_USER_MASK`（3 个键）+ 注释 | ~10 行 | 缓存键常量集中在此（`constants.rs:343-401` 已有 12 个同族键），散落会导致键冲突与失效遗漏 |

**没列进报批、但需要评审知情的 2 项（在 `business/` 内，不越线）**：

| 项 | 文件 | 为什么不算底座改动 |
|---|---|---|
| `ExportColumn` 加 `table: &'static str` | `business/impexp/definition.rs:620` | 属批次 A 的业务代码；但会**破坏所有 `ExportDefinition` 实现**（当前 1 个 + 未来 N 个）→ 需要在实施步 B6 统一改 |
| `format_export_rows` 加 `mask` 参数 | `business/impexp/engine.rs:408` | 同上；签名变更强制两个调用点（`handler.rs:780`/`worker.rs:607`）编译失败——这是**有意的**（漏改就编译不过） |

**`Cargo.toml` 是否需要改**：需要，加 `regex = "1"`（1 行）。理由与项目已有 3 个先例完全一致（`tokio-util`、`futures-util`、`redis` 的注释都写"本就在依赖树中，这里只是显式化，不引入新 crate"，`Cargo.toml:66-84`）；`regex 1.13.1` 已在 `Cargo.lock`（`alun-utils` 的依赖）。**这一项不算越线，但要报批**，因为"只声明 alun"是铁律，任何新增行都要过评审。

**error_code 新增码表（6100x 段，接在批次 A 的 6003x 之后）**

| 码 | 常量名 | HTTP | 语义 |
|---|---|---|---|
| 61001 | `DATA_SCOPE_INVALID` | 400 | `data_scope` 取值非法 / `custom_dept_ids` 与 `data_scope=5` 不匹配 |
| 61002 | `DATA_SCOPE_EXCEED_LIMIT` | 403 | 新范围超出操作者自身可见范围（防止自我扩权） |
| 61003 | `DATA_SCOPE_SELF_ROLE_LOCKED` | 403 | 试图修改自己持有角色的数据范围 |
| 61004 | `DATA_SCOPE_DEPT_NOT_FOUND` | 404 | `custom_dept_ids` 里有不存在的部门 |
| 61005 | `FIELD_MASK_TYPE_INVALID` | 400 | `mask_type` 非法 / `mask_param` 格式不符（如手机号参数不是 `"3,4"`） |
| 61006 | `FIELD_MASK_TARGET_NOT_FOUND` | 404 | `table_name`/`column_name` 在 `information_schema` 里不存在 |
| 61007 | `FIELD_MASK_REGEX_INVALID` | 422 | 自定义正则编译失败 / 捕获组越界 |
| 61008 | `FIELD_MASK_RULE_MISSING` | 422 | **导出 fail-closed**：敏感列没有生效规则 |
| 61009 | `ROLE_MUTEX_CONFLICT` | 409 | 授予角色命中互斥 |
| 61010 | `ROLE_MUTEX_HAS_HOLDERS` | 409 | 建立互斥时已有用户同时持有 |
| 61011 | `ROLE_MUTEX_DUPLICATE` | 409 | 互斥关系已存在（含反向） |
| 61012 | `ROLE_MUTEX_SELF_PAIR` | 400 | 角色与自身互斥 |

> 前端 `request.ts:170` 只把 `code` 透传成 `ApiError.code`（`frontend/src/api/request.ts:170`），所以**前端零改动**即可拿到新码；文案表按上面 §4.5 维护在前端 `error-code.ts`（实施步 B8）。

### 5.4 测试要求（C7）：每模块单测覆盖点

**总量**：34 个单测点 + 3 个真库集成测试。纯函数优先（不连库），DB 相关的用 `tests/` 现有范式（`backend/tests/impexp/`、`tests/sql/` 已存在）。

**A. 数据范围（`business/permission/scope/`）—— 14 点**

| # | 测什么 | 断言 |
|---|---|---|
| S1 | `ScopeKind::from_i16` | 1..5 映射正确；0/6/-1 → `Err(DATA_SCOPE_INVALID)` |
| S2 | `filter_for(All)` | `sql` 含 `1 = 1`、`params` 为空、`is_noop()` 为 true |
| S3 | `filter_for(Dept)` | `sql` **恰好**含 1 个 `?`；`params.len() == 1` |
| S4 | `filter_for(DeptAndChild)` | `sql` 是 `= ANY(?)` 形态（**不含** `WITH RECURSIVE`——守住 POC-4b 的性能结论，防止后来者改回内嵌 CTE） |
| S5 | `filter_for(SelfOnly)` 在 `sys_user` vs `sys_import_task` | 前者用 `id`，后者用 `create_by`（`owner_col` 生效） |
| S6 | 多角色并集：`Dept ∪ Custom` | 结果 `dept_set` = 两者并集且**去重**；`sql` 仍只有 1 个 `?` |
| S7 | 多角色并集：`All ∪ 任意` | 结果为 `All`（短路） |
| S8 | 多角色并集：`SelfOnly ∪ Dept` | `sql` 形如 `(dept = ANY(?) OR id = ?)`，**2 个** `?`，`params` 顺序为 `[dept_array, user_id]` |
| S9 | 超管短路 | `is_super=true` → `filter_for(...)` 恒 `All`（即使库里有 `data_scope=4` 的角色） |
| S10 | 空 dept 集合（`data_scope=5` 但没配部门） | **降级为 `SELF` 而不是 `allow_all`**（安全侧降级；配错了不能变放行） |
| S11 | 用户无部门（`dept_id IS NULL`）且 `data_scope=3` | 降级为 `SELF`（不能因 NULL 生成 `dept_id = NULL` 恒空，也不能放行） |
| S12 | `scope::paginate` 的 count 剥离 | 传入 `order_by="ORDER BY created_at DESC"` 时，断言 count 执行的 SQL **不含** `ORDER BY`，数据页**含** |
| S13 | `scope::paginate` 参数顺序契约 | 断言最终 SQL 里第 1 个 `?` 对应 `RowFilter.params[0]`（用 mock SQL 字符串比较） |
| S14 | `ensure_scope_grant_allowed`（范围上限） | 新 dept 集合 ⊆ 操作者可见集合 → Ok；任一 dept 越界 → `DATA_SCOPE_EXCEED_LIMIT` |

**B. 列级脱敏（`business/permission/mask/`）—— 12 点**

| # | 测什么 | 断言 |
|---|---|---|
| M1..M7 | 7 种类型的 `apply_value` | 每种给 3 组用例（正常 / 短值 / 空值）；**全部断言不 panic**（用一个 `catch_unwind` 包裹的辅助函数统一跑） |
| M8 | `Mask::mask_by_type` 禁用 | CI grep 断言 `src/` 内不出现 `mask_by_type`（POC-9 的 panic 教训） |
| M9 | 幂等 | `apply(mask(x)) == apply(x)`（7 种全覆盖） |
| M10 | 金额用 `Decimal` | 参数 `1000` 且输入 `48620.00` → `48000~49000`；且断言内部**没有** f64 转换（用一个不可能被 f64 精确表示的金额，如 `0.1+0.2` 形态的字符串） |
| M11 | 自定义正则参数校验 | 编译失败 → `FIELD_MASK_REGEX_INVALID`；捕获组编号越界 → 同码 |
| M12 | 规则解析：软删角色上的规则**不生效** | 造一条 `role_id` 指向软删角色的规则，`MaskSet` 不得包含它（防"软删变全局"） |
| — | **DTO 扫描测试（闸 2）** | 注册表里每个 DTO：敏感样例 → `mask()` → 序列化 → 断言无手机号/身份证/邮箱明文；且至少一个字段发生变化 |
| — | **编译期样例测试（闸 1）** | 每个 `sample_for_mask_test()` 不用 `..Default::default()`（代码评审项 + 本段注释说明其作用） |

**C. 角色互斥（`business/permission/sod/`）—— 8 点**

| # | 测什么 | 断言（可直接照搬 POC-6b 四例） |
|---|---|---|
| D1 | `held ∪ requested` 形态 | 已持 A + 本次授 B → 命中（对应 POC 6a） |
| D2 | 无冲突 | 已持 A + 本次授 C → 0 条（6b） |
| D3 | **同次授予两个互斥角色** | 本次同时授 A+B → 命中（6c，**这是防绕过的关键用例**） |
| D4 | 过期绑定不算持有 | `expire_time` 过去 → 0 条（6d） |
| D5 | 反向规范化 | 库里存 `(A,B)`，查询 `(B,A)` 也能命中 |
| D6 | 超管**不**短路 | 超管授予互斥角色 → 仍然 409（与 cap 的刻意差异，见 §4.4） |
| D7 | 存量冲突检查 | 建互斥前有 1 个用户双持 → 返回该用户 + `total=1` |
| D8 | 唯一索引兜底 | 插入反向重复 → DB 唯一索引报错被转成 `ROLE_MUTEX_DUPLICATE`（而不是 500） |

**D. 真库集成测试（`backend/tests/`）—— 3 个**

| # | 场景 | 验收 |
|---|---|---|
| I1 | 3 层部门树 + 5 种范围 | 每种范围的可见行数与预期**精确相等**（POC-3 已验证此断言形态） |
| I2 | 导出脱敏 fail-closed | `sensitive=true` 但无规则 → 导出返回 422 `FIELD_MASK_RULE_MISSING`，**不产出文件** |
| I3 | 互斥端到端 | 建互斥 → 给用户授冲突角色 → 409 且 `sys_user_role` 未变化（事务/无副作用） |

> **本机环境提示**：本地 `rustc 1.94.0` 低于项目 `rust-version = 1.94.1`（`Cargo.toml:6`），`cargo test` 需要 ≥1.94.1 的工具链；用 `--ignore-rust-version` 能编过但**不作为验收口径**。验收一律在开发机 / CI 的 1.94.1 上跑。

### 5.5 性能影响与检查方法

**结论：数据权限注入**不会**导致索引失效，但会引入两类固定开销，其中一类本批顺手修掉。**

| 发现（实测） | 数字 | 处理 |
|---|---|---|
| 内嵌递归 CTE 子查询 → 规划器选 `Seq Scan` | 205 001 行 **28.0 ms** | ✅ 规避：预解析成 `= ANY(?)` → `Bitmap Index Scan on idx_sys_user_dept_id` **7.8 ms**（3.6×） |
| 预解析后 `IN` 列表过长时索引选择性差 | 150/211 部门（72% 行）：`ANY(ARRAY)` **36.4 ms** vs `IN(子查询)` **22.1 ms** | ⚠️ 接受：两种都快于 40 ms，且规划器会按估算自行切换；**不做**"按比例降级为不过滤"（那是放宽权限） |
| 框架 count 包裹不剥 `ORDER BY` | 含 ORDER BY **11.17 ms** / 剥后 **4.49 ms**（2.5×，Sort 780 kB） | ✅ 本批自建 `scope::paginate` 修掉 |
| 多角色 OR 展开 | 2 角色（1 dept + 150 custom）**18.2 ms** | ✅ 规避：解析阶段先合并成最多 2 段固定形态（§2.1），不让 OR 树随角色数膨胀 |
| scope 解析（3–4 表 join + 递归 CTE） | 0.54–1.18 ms / 请求 | ✅ 走 Redis（TTL 300 s），命中即 0 次查库 |
| 脱敏（应用层，纯内存） | 20 万行字符串处理量级：与 SQL 层 140 ms 相对，应用层是"只处理出参当页（≤100 行）" | ✅ 无感（列表页只脱敏当前页 20–100 行） |
| 脱敏（SQL 层，已否决） | 140.25 ms / 20 万行（14×） | ❌ 不采用 |

**索引失效的检查方法（给实施与运维的可执行清单）**

1. **首次上线前，对每张登记表跑一次计划断言**（脚本放 `scripts/explain-check.sh`）：

```sql
EXPLAIN (ANALYZE, BUFFERS)
SELECT u.id FROM sys_user u
WHERE u.deleted_at IS NULL AND u.dept_id = ANY($1::uuid[])
ORDER BY u.created_at DESC LIMIT 20;
-- 断言：出现 "Index Scan using idx_sys_user_dept_id" 或 "Bitmap Index Scan"，
--       且 "Seq Scan on sys_user" 不是获取行的主节点
```

2. **登记表的必备索引清单**（缺一个就补，DDL 在 `V0064`）：

| 表 | 必需索引 | 现状 |
|---|---|---|
| `sys_user` | `(dept_id)` | ✅ 已有 `idx_sys_user_dept_id`（`V0001`） |
| `sys_user` | `(dept_id, created_at DESC) WHERE deleted_at IS NULL` | ➕ `V0064` 新增（覆盖"按部门筛 + 按创建时间排序"的最常见组合） |
| `sys_dept` | `(parent_id)`、`(dept_path)` | ✅ 已有（`V0006`） |
| `sys_role_dept` | `(role_id, dept_id)` UNIQUE、`(dept_id)` | ➕ 两者都在 `V0061`/`V0064` |
| `sys_field_mask` | `(table_name, column_name, role_id)` UNIQUE、`(table_name)` | ➕ `V0062` |
| `sys_role_mutex` | `(LEAST,GREATEST)` UNIQUE、`(role_b_id)` | ➕ `V0063` |
| `sys_import_task` / `sys_export_task` | `(create_by)` | ❗ **现状缺失** → 见下 |

3. **❗ 一个必须在本批补的索引缺口**：`sys_import_task` / `sys_export_task` 本批要按 `create_by` 注入 SELF 范围，但两者**当前没有 `create_by` 索引**（`V0041` 只有 `biz_type`/`status`/`created_at` 相关索引）。→ 加进 `V0064`（本设计已含在附录 A），否则"我的导出任务"列表会全表扫。

4. **上线后监控**：`pg_stat_statements` 按 `mean_exec_time` 排序，过滤"含 `ANY($` 的查询"；阈值 **200 ms** 告警；另加一条只读 SQL 巡检（每周）：找出 `seq_scan > 1000 AND seq_tup_read / seq_scan > 10000` 的表（`docs/` 里的运维清单可复用）。

5. **必须写进 code review 检查表的反模式**：
   - `LEFT JOIN … AND scope`（scope 写进 ON 条件）→ 行数不变，静默失效（§2.6 规则 3）；
   - 在 `WHERE` 里对 `dept_id` 做函数包装（如 `dept_id::text = ?`）→ 索引失效；
   - `OR` 组合超过 2 段（说明没有走 §2.1 的合并逻辑）。

### 5.6 风险清单

| # | 风险 | 等级 | 触发条件 | 缓解 / 已有兜底 |
|---|---|---|---|---|
| R1 | **新增查询漏接数据权限**（最核心风险） | 🔴 高 | 后续批次（C/D）新增列表接口时复制了旧代码模板，忘改成分页单入口 | 四道闸（§2.4）：编译期单入口、CI grep 门禁、启动自检、自检接口。**承认**：闸 2 的白名单是"人维护的清单"，新表若不登记就不在扫描范围 → 因此把"新表必须登记 `SCOPED_TABLES`"写进 `docs/后端二开指南.md` 的 checklist（实施步 B0） |
| R2 | scope 缓存脏读（TTL 内的旧范围） | 🟡 中 | 部门搬迁 / `parent_id` 变更时漏调 `bump_epoch` | TTL 300 s 兜底（最大错误窗口 5 分钟）；失效矩阵 8 条逐条有单测；`bump_epoch` 集中在 4 个写点（§5.3-#8） |
| R3 | **脱敏被绕过**：新接口直接返回 Model 而不是 DTO | 🟡 中 | 后来者用 `Res::ok(SysUserModel::…)` —— 但 `SysUserModel` 未派生 `Serialize`（`auth/model.rs:38-42` 的注释：「`SysRoleModel` 未派生 serde（枚举字段仅实现 `TypeMapper`）」——`SysUserModel` 同族同因），所以**编译不过**；真正的绕过是新建一个 DTO 但没实现 `Maskable` | 闸 1（编译期样例函数）+ 闸 2（敏感值扫描）。**承认**的盲区：非"手机号/身份证/邮箱"形态的敏感值（如内部工号）抓不到（§3.4） |
| R4 | 导出仍可能出明文 | 🟡 中 | 新 `ExportDefinition` 声明 `sensitive: true` 却没配规则 | fail-closed（§3.7）+ 启动自检（§3.4 闸 3）。**已消除**"异步路径漏改"风险：签名变更强制两处编译失败 |
| R5 | `sys_role.data_scope` 默认值掩盖了"未配置" | 🟡 中 | 管理员以为"新建角色默认仅本人"，实际默认"全部" | 前端在"新建角色"表单里把范围默认值**显式显示为"全部"**并要求确认（不是隐藏字段）；`GET /system/role/{id}` 回显 `data_scope`，页面顶部提示"数据范围：全部（可看全公司数据）" |
| R6 | SoD 被"非 API 路径"绕过 | 🟡 中 | 运维直接用 SQL 写 `sys_user_role`；或未来新增的批量导入角色功能 | 唯一索引只保证"互斥规则不重复"，不保证"用户不双持"（那是业务约束）。兜底：① `GET /system/role-mutex/check-user`（体检接口）；② 建议加一个每日巡检任务（`sys_job`，04:30）扫违规并写 `security_logs` —— 本批**不实现**（须先有告警通道口径），列入后续 |
| R7 | "本人行免脱敏"不支持 | 🟢 低 | 业务方要求"看自己手机号明文" | 明确不支持（§3.5）；近似方案已写（DTO 内比对 `principal.user_id`），需要时 ~10 行 |
| R8 | 超管也受互斥限制，可能导致"超管把自己锁住" | 🟢 低 | 超管给自己授互斥角色 | 超管仍可删除互斥规则（`system:role-mutex:delete`）→ 有解；且超管可 SQL 修复。**文档明确**：这是刻意设计（§4.4） |
| R9 | `sys_field_mask` 规则表被配错（如把 `password` 列配成脱敏） | 🟢 低 | 人为 | 只影响出参显示（密码哈希不会出现在任何 DTO 里）；`preview` 接口给试运行；变更写 `audit_logs` |
| R10 | 递归 CTE 在超深部门树（>100 层）性能劣化 | 🟢 低 | 组织架构异常 | 加一条启动自检：`SELECT max(depth) FROM (递归)` > 20 时 `tracing::warn!`；DSQL 仍正确 |
| R11 | 迁移 `V0060` 的 `ALTER TABLE` 在大表上锁表 | 🟢 低 | `sys_role` 行数（角色，通常 <100） | `ADD COLUMN ... DEFAULT 常量` 在 PG 11+ 是**元数据操作**（不重写表），毫秒级；`V0064` 建索引用 `CREATE INDEX`（非 CONCURRENTLY，因为 `sys_user` 在 prod 只有数千行；若客户环境 >100 万行，实施步 B1 需改为 `CONCURRENTLY`——已写入实施步验收条件） |
| R12 | `epoch` 键被误删（Redis flush）导致全量缓存失效 | 🟢 低 | 运维 | 失效 = 回源查库（正确性不受影响），只是短时 QPS 上升；这是 cache-aside 的固有特性 |
| R13 | 与批次 A 遗留事项混淆：SSE 跨副本推送（`notice/mod.rs` 头注释称"B 批次用 Redis Pub/Sub 替换"） | 🟢 低 | 评审时被误认为本批范围 | **本批不做**（§0.3）；已在交接说明里显式移交，避免"B 批没做完"的误解 |
| R14 | 两个新缓存命名空间与既有 `perm:user_perms:` 混淆 | 🟢 低 | 键前缀相似 | 命名 `perm:user_scope:` / `perm:user_mask:`，与 `perm:user_roles|perms|menus` 同族同 TTL；失效统一走 `invalidate_user_principal` 的同点位（§2.8 矩阵） |

---

## 6. 附录 A：完整 DDL（V0060–V0067 原文，已在 PG 18.6 实跑）

> 下面每一段都**原样**在 `batchb_poc3`（真实 schema：`V0001–V0006 + V0022 + V0010 + V0040 + V0041`）上执行过，`rc=0`。可逐段粘贴为迁移文件（每段需另配 `.down.sql`，见 §5.2 表的第四列）。

### A.1 V0060 — `alter_sys_role_add_data_scope`

```sql
-- 数据权限：角色级数据范围（1 全部 / 2 本部门 / 3 本部门及以下 / 4 仅本人 / 5 自定义）
--
-- 默认值 1（全部）是**刻意的**：迁移前系统不存在数据权限概念，所有角色的实际可见范围
-- 都是「全部」。默认 1 = 迁移后可见范围与迁移前逐行一致，不产生无预告的数据消失。
-- 收紧由管理员按角色配置（模板见 V0068，默认不执行）。
--
-- 不建索引：data_scope 只在「解析当前用户 scope」时按 role_id 读取（走主键），
-- 不参与任何过滤条件。
ALTER TABLE sys_role
    ADD COLUMN IF NOT EXISTS data_scope SMALLINT NOT NULL DEFAULT 1;

ALTER TABLE sys_role
    ADD CONSTRAINT ck_sys_role_data_scope CHECK (data_scope IN (1, 2, 3, 4, 5));

COMMENT ON COLUMN sys_role.data_scope IS
    '数据范围：1 全部 / 2 本部门 / 3 本部门及以下 / 4 仅本人 / 5 自定义部门集合';
```

### A.2 V0061 — `create_sys_role_dept`

```sql
-- 角色-自定义部门集合（data_scope = 5 时生效）
--
-- 双外键与 sys_user_role（V0003）同风格：这是「授权数据」，指向不存在的部门就是脏授权。
-- sys_dept 为软删表，FK 不阻塞正常删部门（软删只是 UPDATE deleted_at）。
-- 切换 data_scope 时**不清空**本表，便于来回切换不丢配置；读取时只在 data_scope=5 才取。
CREATE TABLE sys_role_dept (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),  -- 主键
    role_id UUID NOT NULL REFERENCES sys_role(id),  -- 角色 ID
    dept_id UUID NOT NULL REFERENCES sys_dept(id),  -- 部门 ID
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,  -- 创建时间
    create_by UUID,                                  -- 创建人
    UNIQUE (role_id, dept_id)                        -- 同一角色对同部门只一行
);

COMMENT ON TABLE sys_role_dept IS '角色-自定义部门集合（data_scope=5 时生效）';
COMMENT ON COLUMN sys_role_dept.role_id IS '角色 ID（sys_role.id）';
COMMENT ON COLUMN sys_role_dept.dept_id IS '部门 ID（sys_dept.id）';
COMMENT ON COLUMN sys_role_dept.created_at IS '创建时间';
COMMENT ON COLUMN sys_role_dept.create_by IS '创建人 ID';

CREATE INDEX idx_sys_role_dept_role ON sys_role_dept (role_id);
CREATE INDEX idx_sys_role_dept_dept ON sys_role_dept (dept_id);
```

### A.3 V0062 — `create_sys_field_mask`

```sql
-- 列级脱敏规则：按「表 + 列」定义脱敏方式，可按角色差异化（role_id 为空 = 全局默认）
--
-- 三个设计点（都有实测依据，见架构文档 §3.1）：
-- 1. UNIQUE ... NULLS NOT DISTINCT（PG 15+）：让 role_id IS NULL 的全局规则也受唯一约束；
--    普通唯一索引下 NULL 互不相等，会存进多条互相覆盖的全局规则。
-- 2. CHECK 正则约束标识符：table_name/column_name 是本表唯一可能影响 SQL 标识符的字段。
-- 3. role_id 在规则表内（而非独立的角色关联表）：一条规则只属于「全局」或「一个角色」，
--    多角色用户按 OR 命中（脱敏是限制，取并集才安全）。
CREATE TABLE sys_field_mask (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),   -- 主键
    table_name VARCHAR(64) NOT NULL,                 -- 表名（小写下划线）
    column_name VARCHAR(64) NOT NULL,                -- 列名（小写下划线）
    mask_type SMALLINT NOT NULL,                     -- 1 手机号 2 身份证 3 邮箱 4 姓名 5 银行卡 6 金额 7 自定义正则
    mask_param VARCHAR(128),                         -- 类型相关参数，如手机号 '3,4'
    role_id UUID REFERENCES sys_role(id),            -- 适用角色；NULL = 全局默认
    status SMALLINT NOT NULL DEFAULT 1,              -- 1 启用 / 0 停用
    reason VARCHAR(255) NOT NULL,                    -- 脱敏理由（合规留痕）
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    update_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_field_mask_type CHECK (mask_type BETWEEN 1 AND 7),
    CONSTRAINT ck_sys_field_mask_ident CHECK (
        table_name ~ '^[a-z][a-z0-9_]{1,63}$' AND column_name ~ '^[a-z][a-z0-9_]{1,63}$')
);

COMMENT ON TABLE sys_field_mask IS '列级脱敏规则（表+列+脱敏方式，可按角色差异化）';
COMMENT ON COLUMN sys_field_mask.mask_type IS
    '脱敏类型：1 手机号 / 2 身份证 / 3 邮箱 / 4 姓名 / 5 银行卡 / 6 金额 / 7 自定义正则';
COMMENT ON COLUMN sys_field_mask.mask_param IS
    '类型参数：手机号/身份证/银行卡 "保留前N,保留后M"；姓名 "保留前N"；金额 "区间宽度"；正则 "pattern|group"';
COMMENT ON COLUMN sys_field_mask.role_id IS '适用角色；NULL 表示对所有人生效的全局默认规则';
COMMENT ON COLUMN sys_field_mask.reason IS '脱敏理由（合规留痕，必填）';

CREATE UNIQUE INDEX uk_sys_field_mask_rule
    ON sys_field_mask (table_name, column_name, role_id) NULLS NOT DISTINCT
    WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_field_mask_table
    ON sys_field_mask (table_name) WHERE deleted_at IS NULL;
```

### A.4 V0063 — `create_sys_role_mutex`

```sql
-- 角色互斥（SoD）：A 与 B 不可同时授予同一用户。互斥是**对称**关系，A×B 与 B×A 同一条。
CREATE TABLE sys_role_mutex (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    role_a_id UUID NOT NULL REFERENCES sys_role(id),
    role_b_id UUID NOT NULL REFERENCES sys_role(id),
    reason VARCHAR(255) NOT NULL,                    -- 互斥理由（合规留痕，必填）
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    create_by UUID,
    deleted_at TIMESTAMPTZ,
    CONSTRAINT ck_sys_role_mutex_distinct CHECK (role_a_id <> role_b_id)
);

COMMENT ON TABLE sys_role_mutex IS '角色互斥（SoD）：两角色不可同时授予同一用户（对称关系）';
COMMENT ON COLUMN sys_role_mutex.role_a_id IS '互斥角色 A（与 role_b_id 无方向语义）';
COMMENT ON COLUMN sys_role_mutex.role_b_id IS '互斥角色 B（与 role_a_id 无方向语义）';
COMMENT ON COLUMN sys_role_mutex.reason IS '互斥理由（合规留痕，必填）';

-- 对称唯一：规范化后 (A,B) 与 (B,A) 落到同一个键 → 同一条规则只能存一行
CREATE UNIQUE INDEX uk_sys_role_mutex_pair
    ON sys_role_mutex (LEAST(role_a_id, role_b_id), GREATEST(role_a_id, role_b_id))
    WHERE deleted_at IS NULL;
CREATE INDEX idx_sys_role_mutex_b ON sys_role_mutex (role_b_id) WHERE deleted_at IS NULL;
```

### A.5 V0064 — `batchb_indexes`

```sql
-- 批次 B 索引补强（每条都对应一个真实查询路径，见架构文档 §5.5）
--
-- 1) sys_user (dept_id, created_at DESC)：数据权限注入后最常见的组合（按部门集过滤 + 按创建时间排序）
CREATE INDEX IF NOT EXISTS idx_sys_user_dept_created
    ON sys_user (dept_id, created_at DESC) WHERE deleted_at IS NULL;

-- 2) 导入/导出任务按 create_by 注入 SELF 范围（V0041 建表时无此索引）
CREATE INDEX IF NOT EXISTS idx_sys_import_task_create_by
    ON sys_import_task (create_by) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_sys_export_task_create_by
    ON sys_export_task (create_by) WHERE deleted_at IS NULL;

-- 3) 互斥查询的第二个方向（第一个方向由 uk 覆盖）
CREATE INDEX IF NOT EXISTS idx_sys_role_mutex_b
    ON sys_role_mutex (role_b_id) WHERE deleted_at IS NULL;

-- 说明：若目标环境 sys_user / sys_import_task 行数 > 100 万，本文件的三条 CREATE INDEX
-- 需改为 CREATE INDEX CONCURRENTLY（不可在事务内执行）——见实施步 B1 的验收条件。
```

### A.6 V0065 — `seed_batchb_permissions`（15 个节点 + 超管授权）

```sql
-- 批次 B 三模块权限点（3 个菜单 + 12 个按钮）
-- 权限码必须与 backend/src/business/permission/*/handler.rs 的 #[alun::permission] 逐字一致。
-- 授权策略：只授 SUPER_ADMIN（这三项都是「能改别人能看什么」的元权限）。
-- 幂等：固定 UUID + ON CONFLICT DO NOTHING。
INSERT INTO sys_menu (id, name, menu_type, auth_code, path, parent_id, component,
                      sort_order, status, icon, title, is_visible, module_code)
VALUES
    ('20000000-0000-0000-0000-000000000600'::uuid, '数据权限', 2, 'system:data-scope:list',
     '/system/data-scope', NULL, 'system/data-scope/index', 60, 1, 'lock-on', '数据权限', TRUE, 'system'),
    ('20000000-0000-0000-0000-000000000610'::uuid, '字段脱敏', 2, 'system:field-mask:list',
     '/system/field-mask', NULL, 'system/field-mask/index', 61, 1, 'mask', '字段脱敏', TRUE, 'system'),
    ('20000000-0000-0000-0000-000000000620'::uuid, '角色互斥', 2, 'system:role-mutex:list',
     '/system/role-mutex', NULL, 'system/role-mutex/index', 62, 1, 'swap', '角色互斥', TRUE, 'system'),
    ('20000000-0000-0000-0000-000000000601'::uuid, 'system:data-scope:list', 3, 'system:data-scope:list',
     NULL, '20000000-0000-0000-0000-000000000600'::uuid, NULL, 10, 1, NULL, '查看数据范围', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000602'::uuid, 'system:data-scope:update', 3, 'system:data-scope:update',
     NULL, '20000000-0000-0000-0000-000000000600'::uuid, NULL, 20, 1, NULL, '修改数据范围', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000603'::uuid, 'system:data-scope:preview', 3, 'system:data-scope:preview',
     NULL, '20000000-0000-0000-0000-000000000600'::uuid, NULL, 30, 1, NULL, '数据权限自检', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000611'::uuid, 'system:field-mask:list', 3, 'system:field-mask:list',
     NULL, '20000000-0000-0000-0000-000000000610'::uuid, NULL, 10, 1, NULL, '查看脱敏规则', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000612'::uuid, 'system:field-mask:create', 3, 'system:field-mask:create',
     NULL, '20000000-0000-0000-0000-000000000610'::uuid, NULL, 20, 1, NULL, '新建脱敏规则', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000613'::uuid, 'system:field-mask:update', 3, 'system:field-mask:update',
     NULL, '20000000-0000-0000-0000-000000000610'::uuid, NULL, 30, 1, NULL, '修改脱敏规则', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000614'::uuid, 'system:field-mask:delete', 3, 'system:field-mask:delete',
     NULL, '20000000-0000-0000-0000-000000000610'::uuid, NULL, 40, 1, NULL, '删除脱敏规则', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000615'::uuid, 'system:field-mask:plain', 3, 'system:field-mask:plain',
     NULL, '20000000-0000-0000-0000-000000000610'::uuid, NULL, 50, 1, NULL, '查看未脱敏明文', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000621'::uuid, 'system:role-mutex:list', 3, 'system:role-mutex:list',
     NULL, '20000000-0000-0000-0000-000000000620'::uuid, NULL, 10, 1, NULL, '查看互斥组', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000622'::uuid, 'system:role-mutex:create', 3, 'system:role-mutex:create',
     NULL, '20000000-0000-0000-0000-000000000620'::uuid, NULL, 20, 1, NULL, '新建互斥组', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000623'::uuid, 'system:role-mutex:delete', 3, 'system:role-mutex:delete',
     NULL, '20000000-0000-0000-0000-000000000620'::uuid, NULL, 30, 1, NULL, '删除互斥组', FALSE, 'system'),
    ('20000000-0000-0000-0000-000000000624'::uuid, 'system:role-mutex:conflict', 3, 'system:role-mutex:conflict',
     NULL, '20000000-0000-0000-0000-000000000620'::uuid, NULL, 40, 1, NULL, '冲突用户预检', FALSE, 'system')
ON CONFLICT (id) DO NOTHING;

INSERT INTO sys_role_menu (role_id, menu_id, created_at)
SELECT r.id, m.id, CURRENT_TIMESTAMP
FROM sys_role r
CROSS JOIN (VALUES
    ('20000000-0000-0000-0000-000000000600'::uuid), ('20000000-0000-0000-0000-000000000601'::uuid),
    ('20000000-0000-0000-0000-000000000602'::uuid), ('20000000-0000-0000-0000-000000000603'::uuid),
    ('20000000-0000-0000-0000-000000000610'::uuid), ('20000000-0000-0000-0000-000000000611'::uuid),
    ('20000000-0000-0000-0000-000000000612'::uuid), ('20000000-0000-0000-0000-000000000613'::uuid),
    ('20000000-0000-0000-0000-000000000614'::uuid), ('20000000-0000-0000-0000-000000000615'::uuid),
    ('20000000-0000-0000-0000-000000000620'::uuid), ('20000000-0000-0000-0000-000000000621'::uuid),
    ('20000000-0000-0000-0000-000000000622'::uuid), ('20000000-0000-0000-0000-000000000623'::uuid),
    ('20000000-0000-0000-0000-000000000624'::uuid)
) AS m(id)
WHERE r.role_code = 'SUPER_ADMIN' AND r.deleted_at IS NULL
ON CONFLICT (role_id, menu_id) DO NOTHING;
```

### A.7 V0066 — `seed_default_field_masks`

```sql
-- 默认脱敏规则（最小可用集）：只覆盖 sys_user 的三个高频个人信息列，全局生效。
-- 其它表/列的规则由管理员按合规要求配置（避免"上线即成默认策略"的过度决策）。
INSERT INTO sys_field_mask (id, table_name, column_name, mask_type, mask_param, role_id, reason)
VALUES
    ('30000000-0000-0000-0000-000000000001'::uuid, 'sys_user', 'mobile',    1, '3,4', NULL, '手机号对公司内非授权人员脱敏'),
    ('30000000-0000-0000-0000-000000000002'::uuid, 'sys_user', 'email',     3, NULL,  NULL, '邮箱统一脱敏'),
    ('30000000-0000-0000-0000-000000000003'::uuid, 'sys_user', 'real_name', 4, '1',   NULL, '姓名保留首字')
ON CONFLICT DO NOTHING;
```

### A.8 V0067 — `alter_export_task_add_mask_applied`

```sql
-- 导出产物是否经过脱敏（审计用：能回答「这份 Excel 里是明文还是掩码」）
ALTER TABLE sys_export_task
    ADD COLUMN IF NOT EXISTS mask_applied BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN sys_export_task.mask_applied IS
    '产物是否已应用列级脱敏（TRUE = 安全，FALSE = 请求者持 field-mask:plain 权限导出明文）';
```

### A.9 存量违规检查 SQL（互斥，配置前 / 巡检用）

```sql
-- 目的：在建立互斥关系前，找出「已同时持有该互斥对」的用户。
-- 口径：绑定未过期 + 角色未软删 + 用户未软删（与权限装载口径一致，见 system/auth/model.rs:266-268）。
-- 实测：POC-6 捞出了 1 条违规（p1 同时持 DEPT_LEAD 与 AUDITOR）。
SELECT u.username,
       COALESCE(NULLIF(u.real_name, ''), u.username) AS display_name,
       ra.role_code AS role_a,
       rb.role_code AS role_b,
       m.reason
FROM sys_role_mutex m
JOIN sys_role ra ON ra.id = m.role_a_id AND ra.deleted_at IS NULL
JOIN sys_role rb ON rb.id = m.role_b_id AND rb.deleted_at IS NULL
JOIN sys_user_role ur_a ON ur_a.role_id = m.role_a_id
                       AND (ur_a.expire_time IS NULL OR ur_a.expire_time > now())
JOIN sys_user_role ur_b ON ur_b.user_id = ur_a.user_id
                       AND ur_b.role_id = m.role_b_id
                       AND (ur_b.expire_time IS NULL OR ur_b.expire_time > now())
JOIN sys_user u ON u.id = ur_a.user_id AND u.deleted_at IS NULL
ORDER BY u.username;

-- 全量巡检版本（不看互斥表，直接扫「两两角色对」里的违规）：
--   把上面的 sys_role_mutex 换成「所有已存在的互斥对」即可；本批另提供
--   GET /system/role-mutex/check-user?user_id= 给单用户体检。
```
---

## 7. 实施拆分（10 步 B0–B9，可独立交付，25 人日）

依赖图：`B0 → B1 → B2 → {B3, B4} → B5`；`B1 → B6`（脱敏只依赖表）；`B3 → B7`（SoD 的 guard 接线依赖 assign_roles 已改）；`B4,B6,B7 → B8`（前端）；全部 → `B9`。

### B0 · 基线固化与文档同步（0.5 人日 · 后端）

| 项 | 内容 |
|---|---|
| 交付物 | ① `docs/后端二开指南.md` 增加「数据权限 / 脱敏 / 互斥」三节检查清单（含"新增列表接口必须走 `scope::paginate`"、"新表必须登记 `SCOPED_TABLES`"、"新 DTO 若含敏感列必须实现 `Maskable` + 样例函数"）；② `scripts/check.sh` 增加 §2.4 闸 2 的两条断言（脚本草案已实测跑通，见附录 C 末尾的基线输出）；③ 记录基线：闸 ① 20 处、闸 ② 17 处 |
| 可执行验收 | `bash scripts/check.sh` 在**当前代码上必须报出预期失败**（20 / 17 条，说明门禁不哑）；在 B5 完成后必须**只剩 `SCOPE-EXEMPT` 的豁免项**；`grep -rc "SCOPE-EXEMPT" backend/src \| wc -l` 覆盖 ≥ 19 处（附录 C 备注的清单） |
| 依赖 | 无 |

### B1 · 迁移落地（V0060–V0064 + V0067）（1 人日 · 后端）

| 项 | 内容 |
|---|---|
| 交付物 | 8 个 up/down 迁移对（V0060–V0064、V0067 含 down）；`docs/批次B-DDL.md`（附录 A 原文 + 每条的 down 说明） |
| 可执行验收 | ① 空库全量迁移：`cargo run`（自动迁移）后在 `psql` 里 `\d sys_role` 看到 `data_scope`、`\d sys_field_mask` 看到 `uk_sys_field_mask_rule`、`\d sys_role_mutex` 看到 `uk_sys_role_mutex_pair`；② **down 可逆**：`migrator.rollback` 到 V0059 后三张新表消失、`sys_role.data_scope` 消失，再 `run` 回到 V0067（往返 1 次）；③ 在大表环境（>100 万行）确认 `V0064` 的 `CREATE INDEX` 是否需要改 `CONCURRENTLY`（若需要，改完重跑②） |
| 依赖 | B0 |

### B2 · 数据权限基座（`business/shared/` + `business/permission/scope/`）（3 人日 · 后端）

| 项 | 内容 |
|---|---|
| 交付物 | ① `business/shared/principal.rs`：`ScopedPrincipal` + `resolve(user_id)`（3–4 表 join + 递归 CTE，**复用** `dept::model::find_dept_subtree_ids` 的 SQL 形态）；② `business/shared/scope.rs`：`ScopeKind` / `RowFilter` / `ScopeTarget` / `filter_for` / `paginate`；③ 缓存层（键 + epoch INCR + 失效 8 条）；④ `business/shared/guard.rs`：`bump_epoch()` / `preflight_permission()` |
| 可执行验收 | ① §5.4 的 S1–S14 全绿，其中 **S4（不出现 `WITH RECURSIVE`）、S8（2 个 `?` 且顺序正确）、S12（count 无 ORDER BY）** 三条为硬门槛；② `cargo clippy -- -D warnings` 无警告；③ 单测里 `scope::paginate` 生成的 SQL 对 5 种范围各打印一次（作为评审证据）；④ 手工验证：同一用户连续 2 次 `resolve`，第 2 次命中缓存（日志/埋点计数），`bump_epoch` 后第 3 次未命中 |
| 依赖 | B1 |

### B3 · 底座接入（报批项 #3–#11）（3 人日 · 后端）

| 项 | 内容 |
|---|---|
| 交付物 | ① `SysRoleModel.data_scope` + `RoleRes`/`RoleSaveReq` + handler 透传与上限校验；② `page_users` 改走 `scope::paginate`；③ 部门树按 scope 裁剪 + 4 个写点 `bump_epoch`；④ `error_code.rs` 6100x 段 + `constants.rs` 3 个缓存键；⑤ `bootstrap` 自检；⑥ `Cargo.toml` 加 `regex` |
| 可执行验收 | ① `cargo test` 全绿（含既有 258 个）；② 手工：以"部门主管（`data_scope=2`）"登录 → `GET /api/system/user/page` 只回本部门行；③ 改角色名后 `data_scope` **不变**（验证 #4 的"不重置"）；④ 越权用例：管理员把自己角色范围改成 `ALL` 以外的更大集合 → 403 `DATA_SCOPE_EXCEED_LIMIT`；⑤ 部门 `parent_id` 变更后，另一位用户的列表在 1 次请求内变为新范围（验证 epoch）；⑥ 断网/清空 Redis 后列表仍正确（回退查库） |
| 依赖 | B2 |

### B4 · 数据权限 API + 自检接口（2 人日 · 后端）

| 项 | 内容 |
|---|---|
| 交付物 | 6 个接口（§2.9 表，下方补全）：`GET/PUT /system/role/{id}/data-scope`、`GET /system/data-scope/preview`、`GET /system/data-scope/tables`（登记表清单，运维核对用）、`PUT /system/role/{id}/data-scope/custom-depts`、`GET /system/data-scope/enums` |
| 可执行验收 | ① 用 `curl` 走通 5 种范围的配置 → 读回一致；② `preview` 返回的 `where_sql` 与"直接调 `filter_for` 得到的结果"逐字相同（单测内比对）；③ `preview` 对 3 张登记表各返回一个计数值，且总耗时 < 3 s（超时保护生效）；④ 权限码校验：不带 `system:data-scope:update` 调 PUT → 403 |
| 依赖 | B2 |

### B5 · 其它登记表接入（3 人日 · 后端）

| 项 | 内容 |
|---|---|
| 交付物 | 按 §2.5 清单逐表接入：`operation_logs` / `audit_logs` / `auth_logs` / `security_logs`（P0）、`sys_import_task` / `sys_export_task`（P0，同时修"只有 `only_mine` 开关"的缺口）、`sys_notice` 管理视角（P0）、`sys_file`（P1，需 join `sys_user` 取部门） |
| 可执行验收 | ① 每个接入点都有单测（断言 SQL 含 `RowFilter` 片段）；② 反向验收：以"仅本人"角色登录 → 导出任务列表只看到自己的；③ 每条不需要数据权限的查询都在代码里留下 `// SCOPE-EXEMPT: <理由>`（`grep -c "SCOPE-EXEMPT"` 输出作为评审证据，预期 8–12 处） |
| 依赖 | B3 |

### B6 · 列级脱敏（4 人日 · 后端）

| 项 | 内容 |
|---|---|
| 交付物 | ① `business/permission/mask/`：`MaskType`/`MaskRule`/`MaskSet`/`resolve`/`apply_value`（7 类型）+ `Maskable` trait + 注册表；② `sys_user` 的 `UserRes`/`UserDetailRes`/`RoleUserRes` 实现 `Maskable`（存量 DTO 全覆盖，因为 `V0066` 默认就脱敏这三列）；③ `ExportColumn` 加 `table`、`format_export_rows` 加 `mask`、`sys_export_task.mask_applied` 写入；④ `plain` 权限审计（`FIELD_MASK_PLAIN_READ`） |
| 可执行验收 | ① 非超管登录 → `GET /api/system/user/page` 里 `mobile=138****5678`、`email=a***@s.com`、`real_name=张**`；② 超管 → 明文；③ 持 `plain` 权限的非超管 → 明文 **且** `audit_logs` 多一条记录；④ 导出：非超管导出用户列表 → Excel 内是掩码，`sys_export_task.mask_applied = true`；⑤ fail-closed：故意删掉 `sys_user.mobile` 的规则 → 导出返回 422 `FIELD_MASK_RULE_MISSING` 且**无文件产出**；⑥ 闸 1/闸 2 的编译期与扫描测试全绿 |
| 依赖 | B1（表）；与 B3 并行 |

### B7 · 角色互斥（2.5 人日 · 后端）

| 项 | 内容 |
|---|---|
| 交付物 | ① `business/permission/sod/`：`ensure_no_mutex`（UNION 形态 SQL）、`conflicts_for_pair`、`user_mutex_pairs`；② 5 个接口（§4.6）；③ 底座 1 行接线（`assign_roles`）；④ 错误码 61009–61012 使用 |
| 可执行验收 | ① §5.4 的 D1–D8 全绿（**D3 同次授予两个互斥角色必须 409**）；② 端到端：建互斥 (A,B) → 给用户授 A → 成功；再授 B → 409 且 `sys_user_role` **不变**（`GET /system/user/{id}/roles` 仍是 [A]）；③ 反向重复建互斥 → 409 `ROLE_MUTEX_DUPLICATE`（不是 500）；④ 存量冲突：先造双持用户 → 建互斥 → 409 且返回该用户名；⑤ 超管双持 → 也 409（验证"超管不短路"） |
| 依赖 | B3（`assign_roles` 已在 B3 改动范围内） |

### B8 · 前端三页 + 文案（4 人日 · 前端）

| 项 | 内容 |
|---|---|
| 交付物 | ① 「系统管理 → 数据权限」：角色列表 + 范围单选（5 种）+ 自定义部门树穿梭 + 「自检」抽屉（显示最终 WHERE 与计数）；② 「字段脱敏」：规则表格 + 新建/编辑（类型联动参数表单）+ 试运行 + 生效清单；③ 「角色互斥」：互斥组表格 + 新建（先预检）+ 冲突用户表；④ `error-code.ts` 补 61001–61012 文案（§4.5 原文）；⑤ 角色编辑页增加「数据范围」字段（显式展示"全部"并二次确认，R5） |
| 可执行验收 | ① `pnpm typecheck` 0 错；② 三页的权限码与后端逐字一致（`grep` 比对 `sys_menu.auth_code` 与前端 `v-permission` 指令）；③ 用非超管账号打开三页 → 菜单不显示、直接输 URL → 403 页面（不是白屏）；④ 互斥新建时冲突态展示出用户名与"去处理"链接 |
| 依赖 | B4、B6、B7 |

### B9 · 验收与交付（2 人日 · 后端 + 测试）

| 项 | 内容 |
|---|---|
| 交付物 | ① `docs/批次B-验证证据.md`（每个验收点一段命令 + 输出）；② 3 个集成测试（I1–I3）；③ 性能断言脚本 `scripts/explain-check.sh`（§5.5 的 4 条）；④ 上线检查单（含 V0068 是否执行的决策记录） |
| 可执行验收 | ① `cargo test` 全绿（预期 ≥ 258 + 34 ≈ 292 个）；② `scripts/explain-check.sh` 在 20 万行数据集上通过（不出现主节点 `Seq Scan`）；③ `cargo fmt --check && cargo clippy -- -D warnings` 干净；④ 用 §7 的"手工验收"清单逐条打勾（21 条），证据落档 |
| 依赖 | B5、B6、B7、B8 |

**工时汇总**：后端 0.5+1+3+3+2+3+4+2.5+2 = 21 人日；前端 4 人日；合计 **25 人日**（不含评审与返工缓冲；建议按 30 人日排期）。

---

## 8. 需要决策的 6 个未决项（不阻塞本设计落地，但影响上线策略）

| # | 未决项 | 选项 | 本设计的默认 | 谁决定 |
|---|---|---|---|---|
| Q1 | 上线时是否执行 `V0068`（把所有非超管角色收紧到"仅本人"） | ① 不执行（默认，范围全开，管理员按角色逐步收紧）② 执行（上线即最小权限，但会导致全员列表"骤减"） | **① 不执行**（避免无预告行为变更） | 老大 + 产品 |
| Q2 | 是否给"非超管管理员"授予 `system:role-mutex:conflict` | ① 只给超管（简单，但非超管管理员被 409 时看不到冲突清单）② 给所有持 `system:user:role` 的角色（体验完整） | **②**（已写进 `V0068` 模板，默认不执行，等确认） | 老大 |
| Q3 | `sys_file` 的数据权限（P1）是否本批做 | ① 本批做（需 join `sys_user`，SQL 复杂）② 下一批（先把人员/日志/任务三类补齐） | **② 下一批**（P1 标注） | 老大 + 后端 |
| Q4 | 日志类表（`operation_logs`/`audit_logs`）的"操作人列"真实列名 | 需 `\d operation_logs` 实测（本设计未连真库核对，只给了语义列名） | 实施步 B5 第一件事就是核对 | 后端 |
| Q5 | 是否需要每日 SoD 违规巡检任务（写 `security_logs`） | ① 需要（`sys_job` 加一个 04:30 任务）② 不需要（靠 API 兜底） | **① 建议做，但不在本批**（需先定告警口径，R6） | 老大 |
| Q6 | 通知正文（`sys_notice.content` 自由文本）里的手机号是否要脱敏 | 列级脱敏管不到自由文本；需要"内容级"规则（正则扫描 + 替换） | **不支持**，记入风险（本批边界） | 产品 |

---

## 附录 B：POC 脚本清单与实测输出

**脚本位置**：`/root/.hermes/workspace/architect/batchB_poc/`（POC 产物，**不属于交付代码**；按任务书要求"POC 需说明并清理"——见 B.4 清理说明）

| 文件 | 内容 | 结果 |
|---|---|---|
| `V0060_batchb_ddl.sql` | 附录 A 的 V0060–V0064 + V0067 原样（合并版） | `rc=0` |
| `poc2_seed.sql` | 211 部门 / 5 001 用户 / 5 角色 / 151 行自定义部门 | `depts=211 users=5001 role_dept=151` |
| `poc3_scope_queries.sql` | 5 种范围的 EXPLAIN + 行数断言 + 框架 count 包裹形态 | 见 B.1 |
| `poc4_scale.sql` | 20 万用户 / 211 部门 + 计划对比 | `users=205001 depts=211` |
| `poc4b_plans.sql` | 三种子树 SQL 形态对比 + 多角色 OR | 见 B.2 |
| `poc5_count.sql` | 框架 count 包裹 vs 剥 ORDER BY | 见 B.3 |
| `poc6_sod.sql` | 对称唯一索引 / 自反 CHECK / 存量违规检查 | 见 B.4 |
| `poc6b_sod_check.sql` | 授权前互斥校验（UNION 形态）四用例 | 见 B.4 |
| `poc7_mask.sql` / `poc7b_mask.sql` | 规则表约束 + SQL 层脱敏代价与副作用 | 见 B.5 |
| `poc8_scope_resolve.sql` | scope 解析查库成本 | 0.54–1.18 ms |
| `maskpoc/`（临时 crate） | 验证 `alun_utils::Mask` 的行为与 panic | 见 B.6 |
| `gate_batchb.sh` | 闸 2 门禁脚本（实测在真实仓库跑过） | 见附录 C 末尾的基线输出 |
| `poc_evidence.txt` | 上述脚本的关键输出汇总 | 已生成 |

### B.1 POC-3：5 种范围的行数断言（`batchb_poc2`）

```
    scope     | count
--------------+-------
 1_ALL        |  5001
 2_DEPT       |     1
 3_DEPT_CHILD |     1
 4_SELF       |     1
 5_CUSTOM     |  5000
```

> **数据倾斜说明（诚实记录）**：该库的 5 001 个用户被一个有缺陷的"相关子查询"全部塞进了 `D1-1` 一个部门（`SELECT id FROM sys_dept ORDER BY md5(i) LIMIT 1` 被规划成一次求值），所以 `2_DEPT`/`3_DEPT_CHILD` 只有 1 行、而 `5_CUSTOM`（覆盖 `D1-1` + 全部三级部门）反而是 5 000。**这些数字只用于验证 SQL 形态正确性（每种范围都能返回、形态与语义一致），不作为规模性能证据**；性能证据一律用 `batchb_poc3`（20 万行、round-robin 铺满 211 部门）的 POC-4b/5。

### B.2 POC-4b：三种「本部门及以下」形态（`batchb_poc3`，205 001 用户 / 211 部门）

```
=== 形态 A：SQL 内嵌递归 CTE 子查询（21 部门）===
 Execution Time: 28.042 ms      ← Seq Scan on sys_user (actual rows=205001)

=== 形态 B：应用层预解析 → IN (?,?,…) 21 个绑定参数 ===
 Execution Time: 14.973 ms

=== 形态 B（贴近实现）：= ANY(ARRAY[…]) ===
 Execution Time: 7.845 ms       ← Bitmap Index Scan on idx_sys_user_dept_id (actual rows=24909)

=== ⑤ CUSTOM：150 部门（IN 子查询形态）===
 Execution Time: 22.139 ms
=== ⑤ CUSTOM：150 部门（ANY(ARRAY) 形态）===
 Execution Time: 36.408 ms      ← 选择性差（72% 行）时 bitmap 反而更慢

=== 多角色并集（OR 展开：1 dept + 150 custom）===
 Execution Time: 18.182 ms
```

**结论**：`= ANY(?)`（预解析）在主场景（子树局部）比内嵌递归 CTE 快 3.6×；范围极大时两者都在 40 ms 内，由规划器自行选择；多角色 OR 展开 18 ms，可通过"解析阶段合并成最多 2 段"进一步压低（§2.1）。

### B.3 POC-5：框架 count 包裹代价

```
=== A：框架真实形态 COUNT(*) FROM (…ORDER BY…) ===
  count  | 24909        Time: 11.167 ms
  计划：Aggregate → Gather Merge → Parallel Sort (780 kB) → Bitmap Heap Scan

=== B：剥掉 ORDER BY 的 count（本设计建议形态）===
  count  | 24909        Time: 4.492 ms
  计划：Aggregate → Bitmap Heap Scan（无 Sort）
```

**结论**：count 不剥 ORDER BY 多花 2.5×（含一次 780 kB 并行排序）。→ `scope::paginate` 自算 count。

### B.4 POC-6 / 6b：互斥

```
=== 1) 正常插入 A×B ===
INSERT 0 1
=== 2) 反向 B×A —— 期望被对称唯一索引拒绝 ===
ERROR:  duplicate key value violates unique constraint "uk_sys_role_mutex_pair"
DETAIL:  Key (LEAST(role_a_id, role_b_id), GREATEST(role_a_id, role_b_id))=(…003, …005) already exists.
=== 3) 自反 A×A —— 期望被 CHECK 拒绝 ===
ERROR:  new row for relation "sys_role_mutex" violates check constraint "ck_sys_role_mutex_distinct"
=== 4) 记录数应为 1 ===
 mutex_rows = 1
=== 5) 存量违规检查 ===
 reason                    | username | role_a    | role_b
 部门及以下与审计员不得同一人 | p1       | DEPT_LEAD | AUDITOR   (1 row)

=== 6a) 已持 DEPT_LEAD，本次再授 AUDITOR → 命中 1 ===  ✅ 1 row
=== 6b) 本次只授 STAFF → 0 ===                          ✅ 0 rows
=== 6c) 同次授 DEPT_LEAD + AUDITOR → 命中 1（防绕过） ===  ✅ 1 row
=== 6d) 已过期绑定不算持有 → 0 ===                        ✅ 0 rows
```

### B.5 POC-7 / 7b：脱敏

```
=== 全局规则重复 → NULLS NOT DISTINCT 拒绝 ===
ERROR: duplicate key value violates unique constraint "uk_sys_field_mask_rule"
DETAIL: Key (table_name, column_name, role_id)=(sys_user, mobile, null) already exists.
=== 注入尝试（表名带 ; DROP TABLE）→ CHECK 拒绝 ===
ERROR: violates check constraint "ck_sys_field_mask_ident"

=== A) 精确匹配：明文命中 1 / 脱敏后命中 0 ===
 raw_exact = 1 ; masked_exact = 0
=== B) 20 万行脱敏成本（强制物化）===
 sum(length(mobile))                                 → 10.062 ms
 sum(length(regexp_replace(mobile,…)))               → 140.254 ms     ← 14×
```

### B.6 POC-9：`alun_utils::Mask` 实测（临时 crate `maskpoc`，依赖 alun-utils 0.4.2）

```
mobile=138****5678
name=张**

thread 'main' panicked at …/alun-utils-0.4.2/src/mask.rs:198:23:
byte index 2 is not a char boundary; it is inside '张' (bytes 0..3) of `张三`
unknown-type PANICKED
```

**结论**：`Mask::{mobile,email,id_card,bank_card,name,user_id,address,license_plate}` 可安全复用；**`Mask::mask_by_type` 的兜底分支对多字节字符串 panic** → 本设计禁止使用（CI grep 断言，§5.4-M8），自定义类型用自研 `apply_value`。

### B.7 清理说明（POC 不作为交付代码）

- `batchB_poc/` 目录（12 个 SQL + 1 个临时 crate + 证据文件）**仅为本次设计的验证产物**，不进入 `cloudpivot` 仓库；
- 三个测试库 `batchb_poc1/2/3` 是本地 PG 实例上的临时库，**不含任何生产数据**（全部由 `generate_series` 与固定 UUID 造出）；交付后由运维 `DROP DATABASE`（本机因无审批权限未执行 DROP，见下注）；
- 临时 crate `maskpoc/` 未提交、未加入任何 workspace；
- **复跑方式**（任何人可重现本文档的所有数字）：

```bash
createdb batchb_poc3
for f in system/V0001__create_sys_user system/V0002__create_sys_role \
         system/V0003__create_sys_user_role system/V0004__create_sys_menu \
         system/V0005__create_sys_role_menu system/V0006__create_sys_dept \
         system/V0022__add_session_security_flags support/V0010__create_sys_file \
         business/V0040__create_storage_config business/V0041__create_impexp_task; do
  psql -q -d batchb_poc3 -v ON_ERROR_STOP=1 -f backend/migrations/$f.up.sql
done
psql -q -d batchb_poc3 -v ON_ERROR_STOP=1 -f batchB_poc/V0060_batchb_ddl.sql
psql -q -d batchb_poc3 -v ON_ERROR_STOP=1 -f batchB_poc/poc2_seed.sql
psql -q -d batchb_poc3 -v ON_ERROR_STOP=1 -f batchB_poc/poc4_scale.sql      # 20 万行
psql -d batchb_poc3 -f batchB_poc/poc4b_plans.sql
psql -d batchb_poc3 -f batchB_poc/poc5_count.sql
psql -d batchb_poc3 -f batchB_poc/poc6_sod.sql ; psql -d batchb_poc3 -f batchB_poc/poc6b_sod_check.sql
psql -d batchb_poc3 -f batchB_poc/poc7b_mask.sql
```

---

## 附录 C：注入点接入表（实施时逐行打勾）

> 用法：B3/B5 的实施者按此表逐行处理；每行的"形态"决定用哪个入口；结束时应**没有空行**。

| # | 文件::函数 | 主体表 | `ScopeTarget`（alias, dept_col, owner_col） | 形态 | 步 |
|---|---|---|---|---|---|
| 1 | `system/user/model.rs::page_users` | `sys_user` | `("u", "dept_id", Some("id"))` | `scope::paginate` | B3 |
| 2 | `system/user/model.rs::find_user_roles_for_users` | `sys_user_role` | — | 豁免（入参已受 #1 约束） | B3 |
| 3 | `system/user/model.rs::user_holds_super_admin_role` | `sys_user_role` | — | 豁免（判自己） | — |
| 4 | `system/user/model.rs::batch_update_dept` | `sys_user` | — | 写操作：目标用户集合先经 `filter_for` 校验（不能改可见范围外的用户） | B5 |
| 5 | `system/dept/model.rs::find_dept_list` | `sys_dept` | `("", "id", None)` | `filter_for` | B3 |
| 6 | `system/dept/model.rs::find_dept_subtree_ids` | `sys_dept` | — | 豁免（被 scope 解析自身调用） | — |
| 7 | `system/role/model.rs::page_role_users`（角色详情页签） | `sys_user` | `("u", "dept_id", Some("id"))` | `scope::paginate` | B5 |
| 8 | `system/role/model.rs::page_roles` | `sys_role` | — | 豁免（§2.5：角色是全局元数据） | — |
| 9 | `support/log/model.rs`（操作日志分页） | `operation_logs` | `("", None, Some("操作人列"))` | `scope::paginate`（列名待 Q4 实测） | B5 |
| 10 | 同上（审计日志 / 登录日志 / 安全日志） | `audit_logs` 等 | `("", None, Some(语义列))` | 同上 | B5 |
| 11 | `support/file/model.rs`（文件分页） | `sys_file` | `("f", "→join sys_user", Some("uploaded_by"))` | `scope::paginate`（P1） | B5 |
| 12 | `business/impexp/model.rs`（导入任务分页） | `sys_import_task` | `("", None, Some("create_by"))` | `scope::paginate` | B5 |
| 13 | 同上（导出任务分页） | `sys_export_task` | `("", None, Some("create_by"))` | `scope::paginate` | B5 |
| 14 | `business/notice/model.rs::page_notices` | `sys_notice` | `("", None, Some("sender_id"))` | `scope::paginate` | B5 |
| 15 | `business/notice/model.rs::page_my_notices` | `sys_notice_receiver` | — | 豁免（已按 `user_id` 硬绑定） | — |
| 16 | `business/notice/model.rs::page_templates` | `sys_notice_template` | — | 豁免（模板是全局资产，权限码控制） | — |
| 17 | `business/storage/model.rs`（配置列表） | `sys_storage_config` | — | 豁免（§2.5） | — |
| 18 | `support/dict|config|job` 各分页 | 各自 | — | 豁免（全局配置类） | — |
| 19 | `business/impexp/handler.rs:780`（同步导出取数） | 各 `ExportDefinition` | — | 导出走 `definition.fetch()`，**由 `ExportDefinition` 实现自己在 `fetch` 里调 `filter_for`**（登记表：`dict_item` 现阶段 → 豁免，因为它导的是字典） | B6 |
| 20 | `business/impexp/worker.rs:607`（异步导出） | 同上 | — | 同上 + `mask` 参数 | B6 |
| 21 | `business/impexp/cleanup.rs`（清理过期任务） | `sys_import_task`/`sys_export_task` | — | 豁免：后台任务无请求者身份，不能注入 scope | B5 |
| 22 | `business/notice/cleanup.rs`（清理过期通知） | `sys_notice` | — | 同上 | B5 |
| 23 | `business/storage/cleanup.rs`（清理孤儿文件） | `sys_file` | — | 同上 | B5 |
| 24 | `support/dashboard/model.rs`（工作台全局计数） | `sys_user`/`sys_dept` | — | 豁免：聚合计数、首页只对超管开放（与 `business/monitor` 同口径） | B5 |
| 25 | `business/notice/fanout.rs`（按目标解析收件人） | `sys_user`/`sys_dept` | — | 豁免：**必须**能枚举收件人；注入 scope 会让扇出静默少人（这是业务正确性问题，不是权限问题） | B5 |
| 26 | `system/dept/model.rs::find_user_display_names` | `sys_user` | — | 豁免：入参是已授权的 `user_id` 列表，二次约束无意义 | — |
| 27 | `system/auth/model.rs`（角色/权限/菜单装载） | `sys_user` 等 | — | 豁免：装载**当前登录者**自身上下文 | — |

> 备注：`SCOPE-EXEMPT` 实际应落在 #2/#3/#6/#8/#15/#16/#17/#18/#21–#27，共 **19 处**（B5 验收口径：`grep -c "SCOPE-EXEMPT"` ≥ 19）。#21–#27 是**闸 2 在真实仓库上跑出来的**结果（附录 B 的 `gate_batchb.sh`，基线 17 条命中），不是坐在这里想的——这批"清理任务 / 聚合计数 / 扇出"如果机械地"全部注入 scope"，会得到**静默少数据**的故障（比漏权限更危险）。

**闸 2 门禁的基线输出（`bash gate_batchb.sh <repo>/backend/src`，2026-10-28 实测）**

```
[1/2] 分页必须走 scope::paginate（paginate_with_params 仅允许出现在白名单）
  FAIL: 以下位置绕过了 scope::paginate：
    business/impexp/model.rs:455 / :549 / :913
    business/notice/model.rs:559 / :673 / :833
    business/storage/model.rs:220 / :465 / :991
    support/config/model.rs:447   support/dict/model.rs:175 / :297
    support/file/model.rs:153     support/job/model.rs:256 / :314
    support/log/model.rs:333 / :448
    system/role/model.rs:138 / :204
    system/user/model.rs:240                      ← 共 20 处
[2/2] 登记表的查询必须接入 scope:: 或写明 SCOPE-EXEMPT 理由
  FAIL: business/notice/fanout.rs 查询 sys_user / sys_dept 未接入
  FAIL: support/dashboard/model.rs 查询 sys_user / sys_dept 未接入
  FAIL: system/auth/model.rs 查询 sys_user 未接入
  FAIL: system/dept/model.rs 查询 sys_user / sys_dept 未接入
  FAIL: system/role/model.rs 查询 sys_user 未接入
  FAIL: system/user/model.rs 查询 sys_user 未接入
  FAIL: business/impexp/cleanup.rs 查询 sys_import_task / sys_export_task 未接入
  FAIL: business/impexp/model.rs 查询 sys_import_task / sys_export_task 未接入
  FAIL: business/notice/cleanup.rs 查询 sys_notice 未接入
  FAIL: business/notice/model.rs 查询 sys_notice 未接入
  FAIL: business/storage/cleanup.rs / model.rs 查询 sys_file 未接入   ← 共 17 条
```

**这份基线本身就是设计输入**：它把"要改哪些点"从猜测变成清单，并且暴露出 7 类**不该注入 scope** 的查询（清理任务 / 聚合计数 / 扇出 / 自身上下文装载 / 已授权 id 批量取名）——附录 C 的 #21–#27 就是从这份输出里逐条落下来的。

---

## 附录 D：本文档引用的事实索引（评审可逐条核对）

| 事实 | 位置 |
|---|---|
| `sys_role` 无 `data_scope` | `backend/migrations/system/V0002__create_sys_role.up.sql`（全表 13 列） |
| `sys_dept` 有 `parent_id` + `dept_path` | `backend/migrations/system/V0006__create_sys_dept.up.sql` |
| `sys_user` 有 `dept_id` + `idx_sys_user_dept_id` | `backend/migrations/system/V0001__create_sys_user.up.sql` |
| `sys_user_role` 双 FK + `expire_time` + UNIQUE | `backend/migrations/system/V0003__create_sys_user_role.up.sql` |
| 递归 CTE 子树查询（可复用） | `backend/src/system/dept/model.rs:261-281` |
| 用户分页现有 dept 过滤 + `paginate_with_params` | `backend/src/system/user/model.rs:179-258` |
| 角色分页 raw SQL 拼装范式 | `backend/src/system/role/model.rs:110-151` |
| 超管判定（库侧，处理过期绑定） | `backend/src/system/user/model.rs:317-360` |
| 业务侧超管口径注释（并引用**不存在的** `shared/principal.rs`） | `backend/src/system/auth/model.rs:241-243` |
| 权限装载（超管短路成 `*:*:*`） | `backend/src/system/auth/model.rs:236-255` |
| 主体缓存键与失效 | `backend/src/system/auth/model.rs:24-36, 91-156` |
| `CACHE_PRINCIPAL_TTL_SECS = 300` / 键前缀 | `backend/src/shared/constants.rs:363-375` |
| 授权上限（三条规则，纯函数 + 单测友好） | `backend/src/system/user/guard.rs:98-155, 160-240` |
| 角色菜单授予上限（不能授自己没有的码） | `backend/src/system/role/handler.rs:651-680` |
| 用户角色写入权威点（1 行接线处） | `backend/src/system/user/handler.rs:717-760` |
| 角色变更强制下线（为何必须吊销） | `backend/src/system/role/handler.rs:276-294, 508-522` |
| `ExportColumn.sensitive` 只做标记 | `backend/src/business/impexp/definition.rs:620-633` |
| 「导出脱敏与明文权限：未做」自认 | `backend/src/business/impexp/mod.rs:31` |
| 导出格式化的两个调用点 | `backend/src/business/impexp/handler.rs:780`、`worker.rs:607` |
| `format_export_rows` 实现 | `backend/src/business/impexp/engine.rs:408-437` |
| 日志中间件的响应体捕获 + `mask_json_value` | `backend/src/middleware/operation_log.rs:60-79, 223-240, 285` |
| 启动钩子（无错误通道 / prod `exit(1)` 先例） | `backend/src/bootstrap.rs:26-53, 189-200` |
| 启动预检范式 | `backend/src/shared/startup_guard.rs`；调用点 `backend/src/lib.rs:72-78` |
| 装配链（route_layer / layer_from_fn / scan） | `backend/src/lib.rs:80-101` |
| Framework：Hook 仅 Model 生命周期，raw SQL 不触发 | `alun-db-core-0.4.2/src/traits/hook.rs:9-10` |
| Framework：`paginate_with_params` 的 count 包裹 | `alun-db-0.4.2/src/db.rs:194-226` |
| Framework：`sql!` 宏与 `SqlFragment` 组合 | `alun-db-macro-0.4.2/src/lib.rs:124-160`；`alun-db-core-0.4.2/src/query.rs:52` |
| Framework：`has_permission` / `is_super_admin`（通配） | `alun-web-0.4.2/src/middleware/mod.rs:148-160` |
| Framework：`Cache` trait（含 `incr`/`delete_pattern`） | `alun-cache-0.4.2/src/lib.rs:33-61` |
| Framework：`Mask` 的 6 个方法 + `mask_by_type` 的 panic | `alun-utils-0.4.2/src/mask.rs:120-200`（panic 在 `:198`） |
| 权限种子范式（固定 UUID + `ON CONFLICT`） | `backend/migrations/business/V0045__seed_notice_permissions.up.sql` |
| `sys_export_task.create_by`（异步导出身份来源） | `backend/migrations/business/V0041__create_impexp_task.up.sql:95` |
| 「能改别人能看什么 = 元权限，必须留痕」的同源论断 | `docs/大数据-产品需求PRD.md:1560` |
| 「数据权限自检页」的同源设计 | `docs/大数据-产品需求PRD.md:1484-1486` |
| 项目铁律（四件套 / 错误三出口 / 迁移号段 / 权限码格式） | `AGENTS.md`「必须遵守的约定」1–10 |
| 本机工具链低于项目要求（1.94.0 < 1.94.1） | `backend/Cargo.toml:6`；`rustc --version` |

---

## 结语：本设计的三条主线

1. **数据权限不靠"记得写"，靠"写不出来"**——单入口 + 类型强制 + CI 门禁 + 启动自检 + 自检接口，四道闸各自失效时其他三道仍在。
2. **脱敏不靠"序列化器魔法"，靠"编译期样例 + 敏感值扫描 + 导出 fail-closed"**——诚实承认内部工号这类"非标准形态敏感值"抓不到，把它写进风险而不是假装覆盖。
3. **互斥不靠"应用层记得查"，靠"数据库不变量（对称唯一索引） + 唯一写入点接线 + UNION 形态校验"**——POC 实测的 6c 用例（同次授予两个互斥角色）是把"看起来对"和"实际对"分开的那一厘米。

> 全部 DDL 在真库跑过；全部性能数字可复跑（附录 B.7）；全部代码事实可核对（附录 D）；不能做到的事已在 §0.3 / §5.6 / §8 明说。
