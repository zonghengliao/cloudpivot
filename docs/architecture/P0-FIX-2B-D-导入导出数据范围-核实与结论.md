# P0-FIX-2B-D · 批量导入导出三页：导出内容是否逐模块注入数据范围（核实与结论）

> 卡片：`t_e464a285`（上游 `t_34cee9ea` 有意没授的 3 页）
> 证据：`docs/evidence/20261105-impexp-scope/`（8 份逐字输出，含隔离副本展开实验与生产 403 探针）
> 本文件回答三个问题（问题 1–3 全部给 `文件:行号`，不给"应该"），然后落 A/B 结论。

---

## 0. 一句话结论

**结论 B —— 3 页维持不授（尤其不得授 `FULL`）。**

三条理由，每条都有实测：

1. **导出内容查询确实没有逐模块注入数据范围**：5 个导出定义里只有 `system.user` 覆写了带 scope 的入口，
   其余 4 个（`system.dict_item` / `erp.stock_in` / `erp.payable` / `erp.voucher`）落回**默认实现**，
   而默认实现**把 `principal` 丢掉**（`definition.rs:893-906` / `910-923` 的 `let _ = principal;`），
   直接跑裸 SQL（`erp_doc.rs:466` / `:481`）。
2. **比"数据范围"更硬的一条：导出通道不认模块权限码。** `export:create` 一个码就能导任何已注册的
   `biz_type`（`handler.rs:1093` 只校验 `business:impexp:export:create`，`biz_type` 只查注册表存在性）。
   实测 `laoliu_test`（3 档，TECH）对凭证列表 **403**，但一旦拿到 `export:create`，
   导出 `erp.voucher` 会返回 **21 张全公司凭证** —— 这是权限绕过，不只是范围问题。
3. **"补几行 scope 过滤"在这里做不出来**：`biz_stock_in` / `biz_payable` / `biz_voucher` 三张表
   **没有 `dept_id`**（实测 `information_schema`，只有 `create_by`）。给它们强行按 `create_by` 注入，
   会退化成"仅本人"（`scope.rs:459-470` 的降级分支），于是**列表 14 行、导出 5 行**，口径分叉比不注入更糟。
   真要部门隔离 = 数据模型改造（加列 + 回填 + 列表与导出同时切换 + 权限矩阵重评），已在
   `docs/待决策.md` ④ 作为产品事项登记，本卡不夹带。

**同时也证明了一件对后续有用的事**：`READ` 授权的 3 页**不含** `export:create` / `import:confirm`
（隔离副本用仓库里**原文**的 `expand.sql` 展开实测，9 行/角色、7 个去重码，见证据 02）。
所以"只授 READ"是零外带零写入的安全动作 —— 但它同时**没有业务价值**（导入向导会卡在"确认入库"、
导出任务页永远空），不建议用"授 READ"来结案。

---

## 1. 证据环境（怎么复现）

| 项 | 值 |
|---|---|
| 代码树 | `/opt/cloudpivot` @ `2b24753`（99 工作仓）；生产 144 @ `7d96692` |
| 生产库 | `cloudpivot`（只读；仅 `SELECT`，本卡未写任何生产行） |
| 隔离副本 | `cloudpivot_impexp_iso1`（144 上，`pg_dump` 逻辑复制自生产快照：`sys_menu` 329 / `sys_role_menu` 976） |
| 为什么没用 `createdb -T` | 卡片建议的 `createdb <clone> -T cloudpivot` 在 144 上**直接失败**：`ERROR: source database "cloudpivot" is being accessed by other users（10 sessions）`。改用 `pg_dump --no-owner --no-privileges --schema=public` + `psql -f` 逻辑复制，数据等价（行数逐表对平，见证据 01 与 README） |
| 展开实验 | 复用仓库原文 `scripts/datafix/bizperm/expand.sql`（md5 `372bd316c88222c9edb36aecfbe7cd45`，两处同一份），在副本上 `BEGIN … ROLLBACK` 跑，副本零残留 |
| 生产 403 探针 | `mint_token.py` 按库内真值自签令牌（等价真登录），`curl` 打 4 个导入导出接口 |

复现命令见 `docs/evidence/20261105-impexp-scope/README.md`。

---

## 2. 问题 1 —— 导出任务执行时，内容查询走的是哪个函数？有没有 scope？

### 2.1 三条入口，全部先解析 principal，再走 `*_scoped`

| 入口 | 链路 | 证据 |
|---|---|---|
| 同步导出（页面「导出」） | `export_sync` → `export_sync_response` → `principal::resolve_for_claims` → `count_scoped` → `fetch_scoped` | `handler.rs:748`（路由/权限 `business:impexp:export:create`）、`handler.rs:763`（响应构造）、`handler.rs:782`、`handler.rs:810` |
| 异步导出（建任务） | `export_create` → 解析 principal → `count_scoped`（建任务时先按范围数行）→ 写 `sys_export_task` | `handler.rs:1093`、`handler.rs:1102`、`handler.rs:1113` |
| 异步执行（worker） | `do_export` → `filter_from_params` → `principal::resolve(task.create_by)` → `count_scoped` → 循环 `fetch_scoped` → 落盘 | `worker.rs:512`、`worker.rs:525`、`worker.rs:528-535`、`worker.rs:540`、`worker.rs:580` |
| 列表页批量导出（D7） | `batch_export::create_by_ids` → 建同一条异步任务 → 同一个 worker | `batch_export.rs:74`、`batch_export.rs:151` |

`engine.rs` 与数据无关：它是 xlsx 解析/写入（`grep -c "SELECT\|sqlx\|db()" engine.rs → 0`，见证据 04 §05），
所以"内容查询在 engine.rs"这个假设不成立，取数在**定义**里。

### 2.2 关键在定义层：带 scope 的入口是**可选**的，默认实现把 principal 丢掉

```rust
// backend/src/business/impexp/definition.rs:893-906（count_scoped 默认实现，节选）
async fn count_scoped(&self, filter: &ExportFilter, principal: Option<&ScopedPrincipal>)
    -> Result<i64, DbError> {
    let _ = principal;          // ← 默认实现显式忽略发起人
    self.count(filter).await    // ← 回落裸 count
}
// :910-923 fetch_scoped 同形：let _ = principal; self.fetch(...)
```

也就是说：**框架给了注入点，但"注不注入"由每个定义自己决定**，`definition.rs:879-890` 的注释也写明了
"需要按范围取数的定义覆盖这两个方法即可"。

### 2.3 谁真的覆盖了？全仓只有 1 个

```
$ grep -rn "async fn count_scoped|async fn fetch_scoped" backend/src/business --include=*.rs
backend/src/business/impexp/definition.rs:893   ← 默认实现
backend/src/business/impexp/definition.rs:910   ← 默认实现
backend/src/business/masking/user_export.rs:154 ← 唯一覆盖（system.user）
backend/src/business/masking/user_export.rs:173 ← 唯一覆盖（system.user）
```

`system.user` 的实现是**对的**，而且是 deny-by-default：

```rust
// backend/src/business/masking/user_export.rs:106-116
fn build_where(filter, principal) -> (String, Vec<Value>) {
    let row_filter = match principal {
        Some(principal) => scope::filter_for(principal, ScopeTarget::SYS_USER),
        None => scope::RowFilter::deny_all(),   // 拿不到发起人 → 拒绝全部，不是放行
    };
    ...
}
```

### 2.4 ERP 单据导出：裸 SQL，没有任何 scope 谓词

```rust
// backend/src/business/impexp/builtin/erp_doc.rs:465-470
async fn count(&self, filter: &ExportFilter) -> Result<i64, DbError> {
    let mut sql = format!("SELECT COUNT(*) {}", self.spec.from);   // ← 只有业务过滤
    push_filters(&mut sql, &mut params, self.spec, filter);
    db().scalar_with_params(&sql, params).await
}
// :481 同形：let mut sql = format!("{} {}", self.spec.select, self.spec.from);
```

`self.spec.from` 是**编译期常量**，三条分别是：

| biz_type | from（`erp_doc.rs`） | 表 |
|---|---|---|
| `erp.stock_in` | `:395-398` `FROM biz_stock_in s LEFT JOIN biz_supplier … LEFT JOIN biz_warehouse … WHERE s.deleted_at IS NULL` | `biz_stock_in` |
| `erp.payable` | `:414-416` `FROM biz_payable p LEFT JOIN biz_supplier … WHERE p.deleted_at IS NULL` | `biz_payable` |
| `erp.voucher` | `:433` `FROM biz_voucher v WHERE v.deleted_at IS NULL` | `biz_voucher` |

`push_filters`（`erp_doc.rs:511-529`）只拼 `keyword`（`erp_doc.rs:512-524`）与 `ids`（`:525-528`）两类**业务**条件，
没有任何范围谓词；params 里也不会出现 `dept` 或 `user_id`。
⚠️ 顺带发现一处**与数据范围无关**的缺陷：导出过滤参数 `status` 对 ERP 单据**静默失效**。
`ExportSyncQuery.status` / `ExportFilter.status` 都是 `Option<i16>`，注释写的是「1 启用 / 0 停用」
（`dto.rs:40-41`、`definition.rs:848-849`）—— 它只服务字典项 / 用户的启用状态；
ERP 单据的状态是文本枚举（`POSTED` / `DRAFT` / `OPEN` …），`push_filters` 里也没有 status 分支
⇒ 传了 `status` 也返回全量，且不报错。属"未实现 + 静默"，已另开卡 `t_93cba3f4` 跟踪，不在本卡修。

### 2.5 但（重要）这三张表在**列表侧本来就是 unscoped**

| 列表 | 锚点 | 证据 |
|---|---|---|
| 收货入库单 | `LIST_TARGET = ScopeTarget::unscoped("biz_stock_in")` | `erp/stockin_base.rs:632`，`stockin_query.rs:170` 起分页 |
| 应付单 | `LIST_TARGET = ScopeTarget::unscoped(TABLE)` | `erp/payable_query.rs:100`，用 `paginate_unscoped`（`:459`） |
| 凭证 | `LIST_TARGET = ScopeTarget::unscoped(TABLE)` | `erp/voucher_query.rs:102`，`:581` |

且模块头已登记 `SCOPE-EXEMPT`：

```
SCOPE-EXEMPT: biz_voucher / biz_purchase_order / biz_stock_in / biz_stock_transfer /
biz_inventory 都是按「会计期间」取的全局台账（无 dept_id 与归属人列）…
                                        —— backend/src/business/erp/period_items.rs:75-79
SCOPE-EXEMPT: 凭证 / 分录 / 科目映射规则是会计台账（V0149 建表，无 dept_id / 归属人列）…
                                        —— backend/src/business/erp/voucher.rs:70-72
```

`info = unscoped` 时 `scope::filter_for` 恒返回 `allow_all()`（`shared/scope.rs:436-442`），
注入形态就是 `AND 1 = 1`。**结论：导出与列表同口径，导出不多给一行。**

实测（证据 03，隔离副本）：

| 实体 | 导出 count（定义里的裸 SQL） | 列表 count（unscoped） | 是否相等 |
|---|---|---|---|
| `erp.stock_in` | 14 | 14 | ✅ |
| `erp.payable` | 12 | 12 | ✅ |
| `erp.voucher` | 21 | 21 | ✅ |

---

## 3. 问题 2 —— 导入时是否校验目标表的数据范围？

**没有。而且没有可校验的维度。**

1. 导入上下文**只有操作人 id，没有 principal / 数据范围**：

   ```rust
   // backend/src/business/impexp/definition.rs:790-797
   pub struct ImportCtx { pub task_id: Uuid, pub operator: Option<Uuid>, pub mode: ImportMode }
   ```

   `import_confirm`（`handler.rs:568`）里解析出的 `principal` **只用于** `load_task`（任务可见性，
   `model.rs:437` 的 `SYS_IMPORT_TASK` 收口），**不进** `validate`（`validate.rs:315`）也不进 `persist`。
2. 逐行校验只做业务规则：必填 / 长度 / 枚举 / 唯一 / 外键存在 / 数值下限（`validate.rs`），
   没有任何"这行属于哪个部门、你能不能写"的判据。
3. 目标表**没有部门列**，所以也做不出"只能导入本部门"。实测（证据 03 §D）：
   `biz_supplier / biz_material / biz_unit / biz_account / biz_warehouse / biz_location / sys_dict_item`
   的 `dept_id` 列数 = **0**。它们是全局主数据（`erp/model.rs:51-54` 有 SCOPE-EXEMPT 登记）。
4. 写入是**全局 upsert**：`ON CONFLICT … DO UPDATE`（`master_data/supplier.rs:194`、`material.rs:236`、
   `unit.rs:355`、`account.rs:295`、`warehouse.rs:216`、`location.rs:312`，`mod.rs:271-279` 决定 `DO UPDATE` 还是 `DO NOTHING`）。
   即：有 `import:confirm` 的人可以**覆盖同编码的既有主数据行**，而不看这行的归属。

> 对客口径：导入是"主数据维护"通道，不是"我的数据"通道 —— 权限码 `business:impexp:import:confirm`
> 本身就是"能改全公司主数据"的授权，这一点必须在授权时讲清，不要指望数据范围兜底。

---

## 4. 问题 3 —— 导出定义登记表逐条：每个可导出实体是否 scope 注入

登记处：`builtin/mod.rs:32-45`（内置）+ `registry.rs:181-186`（`system.user`）。

| # | biz_type | 业务名 | 源表 | 定义实现 | `count_scoped/fetch_scoped` 覆写 | 内容查询注入 scope | 该表本身是否参与数据权限 |
|---|---|---|---|---|---|---|---|
| 1 | `system.dict_item` | 字典项 | `sys_dict_item`（JOIN `sys_dict_type`） | `dict_item.rs:327 / :347` | ❌ 走默认 | ❌ | ❌ 底座全局配置（`scope.rs` 的 `unscoped` 清单） |
| 2 | `system.user` | 用户 | `sys_user` | `masking/user_export.rs:154 / :173` | ✅ **唯一** | ✅ `scope::filter_for(SYS_USER)` + 无 principal 即 `deny_all` | ✅ `SYS_USER`（dept_id + id） |
| 3 | `erp.stock_in` | 收货入库单 | `biz_stock_in` | `erp_doc.rs:465 / :472` | ❌ 走默认 | ❌ 裸 SQL | ❌ `unscoped`（`stockin_base.rs:632`，SCOPE-EXEMPT） |
| 4 | `erp.payable` | 应付单 | `biz_payable` | 同上 | ❌ 走默认 | ❌ 裸 SQL | ❌ `unscoped`（`payable_query.rs:100`） |
| 5 | `erp.voucher` | 凭证 | `biz_voucher` | 同上 | ❌ 走默认 | ❌ 裸 SQL | ❌ `unscoped`（`voucher_query.rs:102`，SCOPE-EXEMPT） |
| — | `business.erp.{supplier,material,unit,account,warehouse,location}` | 6 个主数据 | 各自主数据表 | `master_data/*.rs` | **只注册导入**（不参与导出清单，`builtin/mod.rs:35-41`） | — | ❌ 全局主数据 |

**读表方式**：只有 `system.user` 一个实体是"导出比列表更严格/更安全"的；其余 4 个 = 与列表完全同口径。

---

## 5. 决定性的实测：授 `FULL` 到底会得到什么

### 5.1 三档账号现在能不能进这 3 页？—— 不能（这是当前的生产事实）

证据 05（生产 144，真身份令牌）：

| 账号 | import/page | export/page | def/list | POST import/upload |
|---|---|---|---|---|
| `admin`（对照） | 200 | 200 | 200 | 400（multipart boundary，说明**过闸了**） |
| `laoliu` | 403 | 403 | 403 | 403 |
| `laoliu_test` | 403 | 403 | 403 | 403 |
| `henry.liao` | 403 | 403 | 403 | 403 |

### 5.2 授 `READ` 会授到什么？—— 零外带零写入（隔离副本，用仓库原文 `expand.sql`）

证据 02（3 页 × 3 角色 mode=READ，真实展开）：

```
每个角色得到 7 个去重码（9 行）：
  business:impexp:def:list         (list)
  business:impexp:import:template  (template)
  business:impexp:import:upload    (upload ← 页面节点自身的码)
  business:impexp:import:list      (list)
  business:impexp:import:download  (download)
  business:impexp:export:list      (list)
  business:impexp:export:download  (download)
断言：has_export_create=0  has_import_confirm=0  has_export_delete=0  has_import_retry=0
```

原因：`expand.sql:15-21` 的只读白名单是
`list/read/detail/view/export/download/mine/template/aux-balance/trial-balance/health/self-check/cancel-impact`，
`create` / `confirm` / `retry` / `cancel` / `delete` 都不在里面；`expand.sql:55-69` 只额外保"目标节点自身"。

### 5.3 授 `FULL` 会授到什么？—— `export:create` + `import:confirm`（对照组实测）

证据 02 §B：同样 3 页 mode=FULL，3 个角色各得 **17 个去重码**，其中包含
`business:impexp:export:create`（3 个角色各 1）与 `business:impexp:import:confirm`（各 1），
另外还有 `export:retry / export:cancel / export:delete / import:cancel / import:retry`。

### 5.4 拿到 `export:create` 之后能导走什么？（数量化）

证据 06（生产，真身份）—— 三档账号 `laoliu_test`（dept=TECH，data_scope=3）在**列表页**上的可见量：

| 列表接口 | `laoliu_test` | `admin` |
|---|---|---|
| `/api/business/erp/stock-in/page` | **200，total=14** | 200，total=14 |
| `/api/business/erp/payable/page` | **200，total=12** | 200，total=12 |
| `/api/business/erp/voucher/page` | **403**（无 `business:erp:voucher:list`） | 200，total=21 |

再叠加证据 03 的导出全量与部门分布：

| 实体 | 导出会返回 | 其中"非 TECH 子树"的行 | 若真按部门过滤（TECH 子树）应返回 |
|---|---|---|---|
| `erp.stock_in` | 14 | **14**（全部；14 行全在 `云枢总部`） | 0 |
| `erp.payable` | 12 | **12**（11 行 `云枢总部` + 1 行归属人未落部门） | 0 |
| `erp.voucher` | 21 | **21**（20 行 `云枢总部` + 1 行归属人未落部门） | 0（且他连列表都 403） |

三条读法：

1. `stock_in` / `payable`：导出**不比列表多给范围**（都是 14 / 12），差别只是"逐页翻" → "一次 xlsx 带走"。
2. `voucher`：**导出比列表多给**。列表 403、导出 21 张 —— 因为导出通道只认 `business:impexp:export:create`，
   不认 `business:erp:voucher:list`（`handler.rs:1093` 附近只有这一个权限码，全模块 `grep "auth_code|has_permission"`
   无任何 biz_type 级校验，`def/list` 也把全部定义返回给有 `def:list` 的人，`handler.rs:99`）。
3. 单据当前几乎全挂在 HQ（`云枢总部`；另有 payable/voucher 各 1 行归属人未落部门），TECH 子树一行都没有 ⇒
   给 3 档角色开导出，拿到的全部是他部门数据（TECH 子树按部门过滤应为 0 行，证据 03 §C7）。

---

## 6. 决策：B（不授），并把边界写死

| 分支 | 是否成立 | 判定依据 |
|---|---|---|
| A「已注入 / 可注入」 | **不成立** | 4/5 导出定义未注入（`definition.rs:893/910` 默认实现）；且源表**没有部门列**（证据 03 §D），"补几行 scope 过滤"只能按 `create_by` 降级成"仅本人"（`scope.rs:459-470`），造成列表 14 / 导出 5 的口径分叉 |
| B「没注入且改造量大」 | **成立** | 改造 = ①导出通道加 biz_type→模块权限码映射 ②ERP 单据表加 `dept_id`+回填 ③列表/导出/权限矩阵同步改。三项都要设计卡，不是本卡范围 |

**因此：**

- `scripts/datafix/bizperm/plan.sql` **不动**（3 页仍不进矩阵）；生产授权**一行未改**（证据 05 证明 4 个账号仍 403）。
- 已把「导入导出对业务角色不可用」写进 `docs/对客验收手册.md` §4.5（走查/演示口径）。
- 已把「要开必须补什么」登记进 `docs/待决策.md` ⑤（老三样：产品拍板 + 设计卡 + 施工卡）。

### 如果将来老大要开，最小可行路径（按顺序，别跳）

| 步 | 做什么 | 为什么必须在前一步之后 |
|---|---|---|
| S0 | **只授 READ**（`plan.sql` 3 行 `mode=READ`） | 实测零外带零写入（§5.2）；但导入向导会卡在"确认入库"、导出页恒空 ⇒ 只能当"看板"用，不要当功能交付 |
| S1 | 在 impexp 加 **biz_type → 模块权限码** 映射（表驱动，例如 `erp.voucher → business:erp:voucher:list`），`export_create` / `export_sync` / `batch_export` 三处统一校验 | 堵住"有 export:create 就能导自己连列表都无权看的实体"（§5.4 第 2 条） |
| S2 | 给 `biz_stock_in` / `biz_payable` / `biz_voucher` 补 `dept_id`（或明确按 `create_by` 的部门）+ 回填 + 巡检，然后**列表与导出同时**切到 `scope::paginate` / `count_scoped` | 只改导出不改列表 = 口径分叉；只改列表不改导出 = 外带。两边必须同一张卡 |
| S3 | 再考虑 `FULL` | 只有 S1+S2 都落地，"能看能办"才不等于"能外带" |

---

## 7. 影响面与回退

| 项 | 本卡是否触碰 |
|---|---|
| 生产库（`sys_role_menu` / `sys_menu` / `sys_user_role` / `data_scope`） | **零写入**（只有 `SELECT`；`pg_dump` 读；副本库是 144 上新建的 `cloudpivot_impexp_iso1`） |
| `scripts/datafix/bizperm/plan.sql` | **未改**（3 页仍不授） |
| 后端代码 | **未改**（本卡只做核实与落纸） |
| 生产授权 | 未变：`laoliu / laoliu_test / henry.liao` 对 4 个导入导出接口仍 **403**（证据 05） |

回退：本卡无生产变更，无需回退。隔离副本 `cloudpivot_impexp_iso1` 保留供复核，
清理命令（不阻塞本卡，任何人复核完可跑）：

```bash
ssh root@172.18.10.144 'su -s /bin/bash postgres -c "dropdb cloudpivot_impexp_iso1"'
```

---

## 8. 证据索引（`docs/evidence/20261105-impexp-scope/`）

| 文件 | 内容 |
|---|---|
| `README.md` | 索引 + 逐条复现命令 + `createdb -T` 失败改用 `pg_dump` 的说明 |
| `prod/01_menu_tree_and_columns.txt` | 3 页节点 / 14 个后代码 / 现状持有者（只有 SUPER_ADMIN）/ 三张单据表只有 `create_by` 无 `dept_id` / 行数 |
| `prod/02_read_vs_full_expansion.txt` | 隔离副本：READ 展开（7 码/角色，`create/confirm` 全 0）vs FULL 展开（17 码/角色，含 `create`+`confirm`） |
| `prod/03_content_vs_list_scope.txt` | 导出 count = 列表 count（14/12/21）；部门分布（100% HQ）；TECH 子树按部门过滤 = 0；主数据表 `dept_id`=0 |
| `prod/04_code_chain_grep.txt` | 所有 `file:line` 的 grep 原文（导出链路 / 默认实现 / 唯一 override / 登记表 / 裸 SQL / SCOPE-EXEMPT / ImportCtx） |
| `prod/05_prod_http_current.txt` | 生产真身份 403 探针（admin 200 对照，三个业务账号全 403） |
| `prod/06_list_visibility_now.txt` | `laoliu_test` 列表可见量（stock-in 14 / payable 12 / voucher 403）vs `admin` |
| `probe_*.sql` `probe_*.sh` | 可复跑的探针脚本（计划表、展开驱动、内容口径、HTTP、列表可见性） |
