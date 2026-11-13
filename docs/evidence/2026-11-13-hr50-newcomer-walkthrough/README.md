# HR-50 新人视角走查（三份文档能不能跑通）

> 卡：`t_a6978b4d`（HR-50 最终验收卡）· 2026-11-13 · 执行：backend-developer
> 结论：**能跑通**。三份文档（`docs/README.md` / `AGENTS.md` HR 口径 / 二开指南 HR 模板）各被实际使用一次；
> 模板里的 Rust/SQL/TS 骨架在本机真编译通过（后端 `cargo check --lib` 0 错、前端 `pnpm typecheck` 0 错）。
> **有 3 条阻断级问题**（后端指南 §17.9.3 ③ 的 dto 骨架照抄编译不过），已按卡面要求回报给文档负责人修，
> 本卡不代改他人文档。

## 一、走查方式（新人视角的硬约束）

- 只打开三份产物 + 迁移/源码核对，不预设 HR 文档在哪。
- 所有结论都有**命令与原始输出**支撑：静态对拍用脚本，数据层用 145 演练库，三层骨架用真编译。
- 演练库**只建在 145**（`172.18.10.145`），库名 `hr50_walk_drill`（含 `drill`），走查结束 `dropdb` 删除并经
  `select count(*) from pg_database where datname like 'hr50%'` 复核为 0。

## 二、路径 1：从 `docs/README.md` 找 ARCH-9

```bash
python3 readme_index_check.py   # 扫 docs/README.md 的 71 条相对链接
```

```
相对链接 71 条，死链 0 条
--- 索引里指向 ARCH-9 的条目 ---
L27: | **[architecture/ARCH-9-人力资源模块设计.md](./architecture/ARCH-9-…)** | …（文档地图，加粗）
L114:| 改员工 / 合同 / 考勤 / 请假 / 薪资的**业务口径** | [architecture/ARCH-9-…](…)（§3 表 / §4 规则 / §5 接口 / §6 页面…）
```

- **步数**：1 步。`docs/README.md` → §一「文档地图」L27（加粗行）直接到 `architecture/ARCH-9-人力资源模块设计.md`；
  §二·补「HR 速查」L114 是第二条同跳度路径（9 行速查表，覆盖业务口径 / 改代码 / 改权限 / 上线演练 / 全量回归 /
  权限矩阵 / 数字复核 / AGENTS 回写）。
- **有没有走错路**：没有。71 条相对链接 0 死链（含 evidence 目录 4 个）。
- **看不懂的索引**：一处**数字口径**——L27/L109/L129 写「落地 50 条路由」，L134 又写「`ROUTE_CONTRACT` 是 50 行」；
  但真值是「契约 51 行 / 已注册 50 条 / 已实现 49 条」（见问题清单 E1）。

## 三、路径 2：`AGENTS.md` 的菜单/权限口径 vs 真库 vs 源码 vs ARCH-9 §5

核对分四路：AGENTS.md 那 41 行菜单表、迁移 `V0387`+`V0401` 的最终态、源码里的 `#[nik::permission]`、
ARCH-9 §5.2 的码表。真库一手数据在 145 的 `hr50_walk_drill`（`CREATE DATABASE … TEMPLATE hr49_fresh_drill`，
即空库首启口径）：

```sql
SELECT menu_type, count(*) FROM sys_menu
 WHERE deleted_at IS NULL AND id::text LIKE '20000000-0000-4000-8000-00000000e0%' GROUP BY 1;
```
```
menu_type count
        1     1
        2     5
        3    35      -- ⇔ AGENTS.md「41 行菜单（1 目录 + 5 页面 + 35 按钮）」
```

目录/页面行的 `title`（侧栏名）与 `sort_order` 逐行一致（`人力资源 77` / `员工档案 10` / `考勤管理 20` /
`请假单 30` / `薪资管理 40` / `薪资项 50`），`title` 最长 4 字；8 条菜单门禁里的三条实测：

```sql
-- ① title > 12（页面/目录行）       → 0
-- ② 同父 sort 重复（menu_type=3）    → 0 rows
-- ③ /business/hr 下 path 重复        → 0 rows
-- ④ 权限码出现在非页面行却标 :page   → 0
```

**权限码逐码对拍**（`hr50_walk_check.py`：解析 AGENTS.md 表 / 迁移 SQL / 源码宏 / ARCH-9 §5.2 四份集合）：

```
codes: agents=40 db=40 src=31 arch52=32
agents_minus_db = []          # AGENTS 40 码 ↔ 迁移 40 码，双向差集为空
db_minus_agents = []
agents_minus_src = ['business:hr:attendance:page', 'business:hr:employee:export', 'business:hr:employee:page',
                    'business:hr:employee:resign', 'business:hr:leave:delete', 'business:hr:leave:page',
                    'business:hr:salary-item:delete', 'business:hr:salary-item:page', 'business:hr:salary:page']
arch_minus_agents = []        # ARCH-9 §5.2 的 32 码全部落在 AGENTS 的 40 码内
```

`agents_minus_src` 的 9 条**正好**是 AGENTS.md「40 个码的四种状态」写的那 9 个（5 个页面码 + 3 个决定不接路由
`employee:resign` / `leave:delete` / `salary-item:delete` + 未落地的 `employee:export`），一条不多一条不少 →
**口径与实现无自相矛盾**。

**抽查的 5 个权限码**（卡面要求 ≥5，逐条在四路里都落地）：

| 权限码 | AGENTS.md 表 | 迁移最终态 | 源码注解 | ARCH-9 §5.2 |
|---|---|---|---|---|
| `business:hr:employee:list` | 按钮行 sort 100「全量列表」 | `V0401` 新建按钮 `…e061` | `GET /employee/page` | #1 |
| `business:hr:employee:sensitive` | 按钮行 sort 50「查看明文」 | `V0387` 按钮 | `GET /employee/{id}/sensitive` | #4 |
| `business:hr:salary:post` | 按钮行 sort 40「过账」 | `V0387` 按钮 | `POST /salary/{id}/post` | #42 |
| `business:hr:leave:cancel` | 按钮行 sort 50「作废请假单」 | `V0387` 按钮 | `POST /leave/{id}/cancel` | #31 |
| `business:hr:salary-item:delete` | 按钮行 sort 30（决定不接路由） | `V0387` 按钮 | **源码 0 次**（与「决定不接路由」一致） | #34 无接口 |
| `business:hr:employee:page` | 页面行 sort 10 | `V0401` 改名后的页面码 | **源码 0 次**（N14/N18 不违约） | 不含（§5.2 无页面码） |

**角色矩阵**（同一份码集合，两个库口径）：

```
cloudpivot_dev（生产克隆口径）: HR_MANAGER 40 · SUPER_ADMIN 40 · DEPT_LEADER 5 · FINANCE_MANAGER 5 · EMPLOYEE 4 · FINANCE_DIRECTOR 0
hr50_walk_drill（空库首启口径）: HR_MANAGER 40 · SUPER_ADMIN 0 ·  DEPT_LEADER 5 · FINANCE_MANAGER 5 · EMPLOYEE 4 · FINANCE_DIRECTOR 0
EMPLOYEE 的 *:list 码 = 0 条；DEPT_LEADER 只有 employee:list（刻意不给 leave:list）
```

与 AGENTS.md 的六角色矩阵逐格一致；`SUPER_ADMIN` 的 40 与 0 之差正是 `V0401` 里写明的「空库首启已知无害差异」
（种子晚于版本化迁移）——**文档没写错，但新人先跑空库会撞上**（见 E6）。

## 四、路径 3：按二开指南 §17.9 / 前端 §11.1 真拼一个最小骨架

骨架：`certificate` 子域 = 1 张表 + 3 个接口（列表 / 详情 / 新建）+ 1 个页面占位。
全过程在**独立 git worktree**（`hr50/walkthrough-*` 分支，`/root/.../wt-hr50walk`）里做，
不碰主干、不影响并发 worker。

### 4.1 数据层（§17.9.2 原样代入 `<your_table>=employee_certificate`）

```bash
psql -h 172.18.10.145 -U cloudpivot -d postgres -c "CREATE DATABASE hr50_walk_drill TEMPLATE hr49_fresh_drill"
psql -h 172.18.10.145 -U cloudpivot -d hr50_walk_drill -v ON_ERROR_STOP=1 -f V0404__hr_certificate_tables.up.sql
```
```
① up 第一遍：NOTICE:  biz_employee_certificate 完成：表/列/部分唯一索引/双向 CHECK/软删复用/外键 6 类断言全过
② up 第二遍：NOTICE:  relation "uk_biz_employee_certificate_no" already exists, skipping
             NOTICE:  biz_employee_certificate 完成：… 6 类断言全过          -- 幂等
③ 插 1 行后 down：ERROR:  V0404 down 拒绝执行：biz_employee_certificate 还有 1 行未删数据（先导出/清理）
④ 清数据后 down：NOTICE:  V0404 down 通过：表已拆，第二遍执行不报错（DROP IF EXISTS）
⑤ down 第二遍：NOTICE:  table "biz_employee_certificate" does not exist, skipping … 通过
⑥ 残留：select count(*) from pg_class where relname like '%employee_certificate%' → 0
```

骨架里的 `LIST_SQL` / `FIND_SQL` / `INSERT_SQL` 形状也对真库跑过一遍（`AS no` 别名、`::text` 日期、
`NULLIF(?::text,'')` 空值手法都对）：

```
列表SQL:|d29eb298-…|走查员工|CERT-W1|2026-11-13|
详情SQL:|1
```

### 4.2 三层 Rust 骨架（§17.9.3 model / dto + §17.9.4 handler）

```bash
cargo check --lib          # 首次 11 错（其中 1 条是我自己数错数组长度，3 类共 3 处是文档骨架的问题）
cargo check --lib          # 按本仓真实写法纠完后
exit=0  （0 error）
cargo test --lib business::hr::certificate
# test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 5552 filtered out
# 0 warning（cargo test 的 2 条 unused import 也已清）
rustfmt --edition 2024 --check src/business/hr/certificate_*.rs   # 无 diff
```

> 三次修复**已回写进 `docs/后端二开指南.md` §17.9.3 ③**（该节是上一张文档卡 `t_ccb6a06b` 由本岗落的，
> 属「文档负责人修」；改动是 3 处类型 + 1 段日期解析 + 2 条 ⚠ 注释 + §17.9.6 一条自查行，未动他人章节）。
> 完整骨架（后端 3 文件 + 迁移 2 个 + 前端 5 文件 + 4 处接线）见同目录 `skeleton.patch`（`git apply` 可复现）。

`cargo test --lib business::hr::certificate`：见「6. 结论」。

### 4.3 前端五文件（前端指南 §11.1）

```bash
pnpm typecheck
> vue-tsc --noEmit                       # 无输出 = 0 错
tsx --test tests/unit/hr-certificate-page.test.ts
# pass 2  fail 0   （状态词表逐值对拍后端 DTO + 页面码只出现在 router）
```

## 五、问题清单（三档）

### A. 阻断（照抄必挂；**A1~A3 已当场回写进 §17.9.3 ③**，其余按卡面只报不改）

| # | 位置 | 现象（原始报错） | 正确写法（本仓真代码出处） |
|---|---|---|---|
| A1 | 后端指南 §17.9.3 ③ `validate` 签名 | `pub fn validate(&self) -> Result<<YourEntity>Write, ApiError>` → `error[E0107]: type alias takes 1 generic argument but 2 generic arguments were supplied` + `error[E0308]: expected Error, found ApiError`。根因：`use nik::prelude::*` 把 `Result` 换成**一参数**别名 `nik::Result<T>`（`E = nik::Error`） | `-> std::result::Result<…, ApiError>`。本仓 `contract_dto.rs:344`、`leave_dto.rs`、`attendance_period_handler.rs` 全是这么写 |
| A2 | 后端指南 §17.9.3 ③ 入参结构 | 入参 `pub employee_id: Uuid` + `#[derive(Deserialize)]` → `error[E0277]: the trait bound nik::uuid::Uuid: serde::Deserialize is not satisfied`（同块的 `NaiveDate` 反而能过 —— chrono 的 serde 是开的，只有 uuid 没开） | 入参一律字符串：`Option<String>`/`String`，`validate` 里 `Uuid::parse_str`（`NaiveDate::parse_from_str` 同款）。出处 `leave_dto::LeaveCreateReq`（`employee_id: Option<String>`）、`contract_dto::ContractCreateReq` |
| A3 | 后端指南 §17.9.3 ③ 出参结构 | 出参 `pub id: Uuid` + `#[derive(Serialize)]` → `error[E0277]: the trait bound nik::uuid::Uuid: serde::Serialize is not satisfied` | `id` / `*_id` 下发 `String`。出处 `employee_dto.rs:443` 的注释直接把理由写在字段上（「`Uuid` → `String`：前端不需要知道它是 UUID，但也别让它变成 `null`」）。**§17.9 通篇没有「DTO 里 Uuid → String」这条铁律**，建议补进 17.9.3 的表 |

> A1~A3 都在同一个代码块（§17.9.3 ③），一次修完即可；model（②）与 handler（④）的骨架**原样可编译**，
> 数据层 SQL（§17.9.2）与前端五文件（§11.1 ①②）也**原样可跑**。

### B. 影响体验（不阻断，但会让新人多绕一圈）

| # | 位置 | 现象 | 建议 |
|---|---|---|---|
| E1 | `AGENTS.md` L175 + `docs/README.md` L134 | 两处写「`ROUTE_CONTRACT` 50 行」、L134 还算「48 行已实现」；真值是**51 行**（49 已实现 + 2 未实现 `employee:import/export`）、已注册 50 条（+`/health`）。后端指南 §17.9.4 ① 已经是 51 —— 三份文档里两份没跟上 HR-47 的登记 | 回写 50→51、48→49；建议把「契约 51 / 已注册 50 / 已实现 49」写成一格表，避免三个数各说各的 |
| E2 | 后端指南 §17.9.4 ④ | 正文先写「两条既有单测会自动扫到你的新文件，**不需要新写**（不登记就红）」，⚠ 才补「新子域要改 `SEED_PARTS` 与 `assert_eq!(seeded.len(), 35)`」。新人先读到的是「不用动测试」，然后被红 | 把 ⚠ 提为 ④ 的第一句：「新子域 ⇒ 必须扩 `SEED_PARTS` 并把断言改成 35 + 新增数」 |
| E3 | 后端指南 §17.9.3 ⑦ | 建单骨架调 `state_machine::log_created(&tx, <ENTITY_ID>, …)`，而 `<ENTITY_ID>` 要在 `business/shared/state_machine.rs` 加 `Entity` 变体 + `DocKind` 变体 + 合法边 + `parse`（跨两个文件约 50 行），§17.9.1 只在占位符表里一句「新增时在 state_machine.rs 登记」，**没有 diff** | 贴一段最小登记 diff，或明确写「本骨架故意不含状态日志，接状态机请看 `leave_*` 的登记四处」 |
| E4 | 前端指南 §11.1 ③ | 骨架里 `form` / `rules` / `submit` / `employeeOptions` 四处空洞只写「按实例补全」；`employeeOptions` 的真正来源（`@/api/business/hr/employee` 的 `fetchEmployeePage`）没写 | 给一行指针：「员工下拉照 `views/business/hr/leave/index.vue` 的 `employeeOptions` 段（L858~L890）」 |
| E5 | 前端指南提交前命令 | `tests/unit/*.test.ts` 不在 `tsconfig.json` 的 `include`（只有 `src/**`），所以 `pnpm typecheck` **不检查第 5 个文件**，`tsx` 又是纯剥类型不校验 | 在 §11.1 ⑤ 注明「第 5 个文件的类型错误只有 `pnpm test:unit` 跑挂时才看得见」，或把它纳入 tsconfig |
| E6 | `AGENTS.md` 角色矩阵 | `SUPER_ADMIN = 40` 只成立于有生产克隆口径的库；空库首启实测 **0**（同一行有解释，但新人先跑空库会以为文档错） | 矩阵加一列「空库首启实测」，或把 `V0401` 的那段 ⚠ 引用进来 |

### C. 建议（顺手改进，不影响跑通）

| # | 位置 | 建议 |
|---|---|---|
| S1 | `AGENTS.md` 约定 12 | 「`title` 2-6 字（硬顶 12）」与门禁口径不一致：门禁只查 `menu_type IN (1,2)`，而现有按钮行 `business:hr:employee:sensitive` 的 title「查看明文（身份证/银行账号）」是 **14 字**（实测）。要么改文案，要么在约定里写明「按钮行豁免」 |
| S2 | 前端指南 §11.1 ④（路由注册） | 只说「紧邻 hr 其它页面」，实际 `router/index.ts` 有 800+ 行、每个条目带 5~10 行注释。给个锚点（「插在 `BusinessHrSalaryItem` 之后」）能省一次全文件搜索 |
| S3 | 后端指南 §17.9.2 ⑤ 号段骨架 | 骨架是「`INSERT … SELECT … WHERE NOT EXISTS`」，但没提醒 `biz_seq` 的 11 列里 `remark` 也是 NOT NULL 语义（照抄能过，但漏填会得到空备注）。补一句「remark 写卡号便于回溯」即可 |
| S4 | `docs/README.md` §二·补 | 「实测基数」块里的三个数字（表 10 / 路由 50 / 码 40）建议追加一行「契约 51 行（含 2 行未实现）」——现在要读下面那段 `>` 注才拼得出来 |

## 六、结论

**能跑通。** 三份文档各自都真的能带一个新人走到可运行的状态：

1. `docs/README.md`：71 条链接 0 死链，ARCH-9 **1 步可达**，HR 速查 9 行覆盖「改业务口径 / 改代码 / 改权限 / 看证据」。
2. `AGENTS.md` 的菜单/权限口径：41 行菜单、40 个权限码与迁移/源码/ARCH-9 §5.2 **双向零差**；菜单不变量四条实测 0 违规；
   六角色矩阵在生产克隆口径下逐格命中。
3. 二开指南 HR 模板：数据层 SQL **原样**在 145 演练库跑通（up / 幂等 / 护栏 / down / 幂等 / 零残留 六段全过），
   三层 Rust 骨架与前端五文件**纠 3 处 dto 写法后**真编译通过（`cargo check --lib` exit 0、`pnpm typecheck` 0 错、
   `tsx --test` 2/2）。

卡在哪一步：**曾经只有一处**——后端指南 §17.9.3 ③ 的 dto 骨架（A1~A3）。它不是「文档没写清楚」，而是
「照抄编译不过」，属于必须回写的那一类。**本卡已当场回写**（§17.9.3 ③ 三处类型 + 日期解析 + 两条 ⚠ + §17.9.6
一条自查行），回写后整条路径不再需要「问老人」的环节；复跑 `cargo check --lib` / `pnpm typecheck` /
`cargo test --lib business::hr::certificate` 全绿即为证据。

剩下的 E1~E6 / S1~S4 **不阻断**，按卡面只报不改（E1 是 `AGENTS.md` + `docs/README.md` 的 `ROUTE_CONTRACT 50→51`
两个数，其中 `AGENTS.md` 是受保护文件，需要人工批准才能改）。
