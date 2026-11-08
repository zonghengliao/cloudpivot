# AUD-02 全站 UUID 显示排查 —— 人员字段 DTO 统一补 name（批量 join，防 N+1）

- 卡片：`t_b5d5d74c`（AUD-02），上游 AUD-01（附件上传人快照）
- 日期：2026-11-09
- 结论：**界面上「只显示 id 不显示姓名」的三处真实漏点已消灭**（合同负责人、帮助文档版本操作人、文件列表上传人），
  另外 9 个「出参 DTO 有人字段却没名字」的结构体按统一入口补齐；取名字**恒定 1 条 SQL**，
  且已用计数器在真库用例里钉死。

---

## 1. 一句话口径（沿用 `AGENTS.md` 约定 13）

| 类别 | 口径 | 本次怎么处理 |
|---|---|---|
| **审计 / 历史** | 写入时存**姓名快照**（改名后显示旧名） | 附件上传人（AUD-01，V0352 `uploader_name`）、状态日志 `operator_name`、审批 `assignee_name`、操作日志 `username` —— **本次一个都没改成实时 join**（改了就是审计事故） |
| **实时展示** | 后端 DTO **实时 join** 取现值（改名后显示新名） | 合同负责人 / 制单人 / 修改人、ERP 单据制单人、文件上传人、帮助版本操作人、部门负责人 |
| **主数据关联** | 实时 join 最新名 | 客户 / 物料 / 仓库 / 供应商 / 部门 —— 本次不涉及（早已有 `*_name`） |

判据不是「字段叫不叫 name」，而是：**界面上能不能看到一串 `01a19012-7573-…`**。

---

## 2. 扫描方法与结果（可复现）

```bash
# ① 盘点（同时写本目录的 inventory.md）
python3 scripts/verify/aud02_person_name_inventory.py

# ② 门禁（提交前 / CI 跑这个，退出码 0 = 通过）
python3 scripts/verify/aud02_person_name_inventory.py --check
```

扫描器解析 `backend/src/**/*.rs` 的 `pub struct` 与 `frontend/src/**/*.ts` 的
`interface/type`，按字段名识别 20 种人员字段（`create_by` / `update_by` / `owner_id` /
`salesperson_id` / `uploaded_by` / `operator_id` / `assignee_id` / `leader_id` / `user_id` …），
再检查同一结构体里有没有配对的 `*_name`（含 `uploader_name` / `assignee_name` / `created_by_name`
这类换算别名）。

实测（本次提交时）：

| 范围 | 含人员字段的结构体 | 缺 name | 其中「出参 DTO」 |
|---|---|---|---|
| 后端 `backend/src` | 245 | 166 | 19（**逐个判定为 N/A，见 §6**） |
| 前端 `frontend/src` | 66 个类型 | 41 | 0（全部是 `*Req` / `*Query` / 表单草稿，见 §6） |

> 「缺 name 166 个」听起来很多，其实是**库行 `*Row` 与入参 `*Req`**：它们不该有 `*_name`
> —— `Row` 只做 ORM 中转（名字在 DTO 里补），`Req` 是前端提交的 id（给人名反而会被误回写）。
> 真正会序列化给前端的「出参 DTO」只有 19 个缺，且全部已判定为 N/A（§6 逐条给了理由）。

---

## 3. 界面真实漏点清单（修复前 → 修复后）

扫描前端 `views/**` 里所有「人员字段被直接渲染成字符串」的位置，命中 3 处**真**漏点：

| # | 文件:行（修复前） | 修复前界面显示 | 修复后 |
|---|---|---|---|
| 1 | `frontend/src/views/business/erp/contract/index.vue:441` | `{{ detail.owner_id ? shortId(detail.owner_id) : '—' }}` → **截断 UUID**（`01a19012-7573-…`） | `{{ detail.owner_name \|\| '—' }}`（后端 join 的姓名） |
| 2 | `frontend/src/views/business/help/manage/index.vue:258` | `{{ version.create_by \|\| '—' }}` → **完整 UUID**（操作人） | `{{ version.create_by_name \|\| '—' }}` |
| 3 | `frontend/src/views/business/storage/file/index.vue:103` | `uploaderDisplayName(row.uploaded_by, uploaderNames)` → 无权限 / 用户超出译名页时**回退渲染 UUID** | `uploaderLabelOf(row, ...)`：后端 `uploader_name` → 前端译名 → `—`（**永不回退 UUID**） |

另外两处「看似漏点、实际已安全」的，写进证据以免下次再被当 bug 改：

- `views/system/dept/index.vue:60`：`{{ row.leader_name || row.leader_id || '-' }}` —— 列表树
  `DeptRes.leader_name` 由 `dept/handler.rs` 批量填充（早已有）；**本次把详情 `DeptDetailRes.leader_name` 也补齐了**，
  这条 fallback 现在两条路都拿得到姓名。
- `views/support/file/index.vue`：ATT-03 已改成 `uploaderLabel(row)`（后端 `uploader_name` 优先）。

---

## 4. 后端改动：一个入口，12 个 DTO 字段

### 4.1 统一实现（`backend/src/business/shared/user_name.rs`）

```rust
/// 批量取显示名：一次 WHERE id = ANY(...) 覆盖一整页
pub async fn names_of<I: IntoIterator<Item = Uuid>>(ids: I) -> DbResult<HashMap<Uuid, String>>

/// 每张表只声明「我有哪些人字段」，批量语义由这里保证
pub trait PersonNameRow {
    fn person_ids(&self, out: &mut Vec<Uuid>);
    fn apply_names(&mut self, names: &HashMap<Uuid, String>);
}

/// 列表 / 详情补名字的唯一入口：恒定 1 条 SQL（全空则 0 条）
pub async fn fill_person_names<T: PersonNameRow>(rows: &mut [T]) -> DbResult<()>
```

- 取值口径：`real_name` → `username` → `(已删除用户)`；**永不返回空串、永不返回 UUID**。
- **不写 `deleted_at IS NULL`**：软删账号仍要显示姓名（展示口径）；要不要区分「人还在不在」
  是审计口径的事（`snapshot_name` / AUD-01）。
- 失败语义：各 handler 一律 `.unwrap_or_default()` —— 取名字失败只让字段为空（前端显示 `—`），
  不把业务页打成 500。

### 4.2 接入的 DTO（12 个字段，全部有门禁钉住）

| 文件 | 结构体 | 新增字段 | 回填点 |
|---|---|---|---|
| `erp/contract_dto.rs` | `ContractDetailRes` | `owner_name` / `create_by_name` / `update_by_name` | `contract_handler::detail_of`（列表、新建、改单、状态流转的回读全走它） |
| `erp/stockin_dto.rs` | `StockInRes` | `create_by_name` | `stock_in_page` + `detail_of` |
| `erp/stockout_dto.rs` | `StockOutRes` | `create_by_name` | `page_of` + `detail_of` |
| `erp/stocktake_dto.rs` | `StockTakeRes` | `create_by_name` | `page_of` + `detail_of` |
| `erp/dto.rs` | `InventoryTxnRes` | `operator_name` | `inventory_txn_page` + `inventory_txn_detail` |
| `help/dto.rs` | `HelpVersionRes` | `create_by_name` | `doc_versions` |
| `mutex/dto.rs` | `MutexRes` | `create_by_name` | `page` + create/update 回读 |
| `storage/dto.rs` | `StorageFileRes` | `uploader_name` | `file_page` |
| `system/dept/dto.rs` | `DeptDetailRes` | `leader_name` | `detail` / create / update 回读（复用既有 `find_user_display_names`） |

> `support/file/dto.rs::FileRes.uploader_name` 与 `attachment/dto.rs::DocAttachmentItemRes.uploader_name`
> 是 ATT-03 / AUD-01 已交付的，本次只把它们纳入门禁，防止回退。

### 4.3 为什么不用 `LEFT JOIN sys_user`（而在 handler 里批量补）

ERP 的 `CONTRACT_JOIN` 被 6 处复用（列表 count、状态机锁读、A4 合同选择器…），
往里加两张 `sys_user` 会让「主档软删 → 单据查不到」的老坑复活；而 `fill_person_names`
与之等价（都是按主键取值），SQL 条数一样是 1，还不动公共 SQL。**代价**是 DTO 里
`*_name` 由 `None` 起步、必须记得调用 —— 这就是为什么每处调用后面都跟一条门禁用例。

---

## 5. 验收对照（真实输出，非描述）

### ① 扫描清单完整

- `docs/evidence/20261109-aud02-uuid-audit/inventory.md`（脚本生成，247 行，覆盖 backend 245 + frontend 66 个结构体）。
- 复核命令：`python3 scripts/verify/aud02_person_name_inventory.py --check` →
  `全过：13 条 DTO 约束 + 3 条界面约束`。

### ② 所有人员字段显示姓名（HTTP 真机，隔离实例）

隔离实例：`cp_aud02_e2e` 空库 + 启动链自动迁移 **285 个** → `127.0.0.1:18099`。

```
$ AUD02_BASE=http://127.0.0.1:18099 AUD02_DB=cp_aud02_e2e \
    python3 scripts/verify/aud02_person_name_e2e.py

== ②④ 合同详情：负责人 / 制单人 / 修改人都是姓名，改名后跟新名 ==
[PASS] 建合同成功（HTTP 200）
[PASS] 新建回读 owner_name = '张负责'
[PASS] 新建回读 create_by_name 有值：'超级管理员'
[PASS] 详情 owner_name = '张负责'
[PASS] 详情 create_by_name = '超级管理员'
[PASS] 详情 update_by_name 字段存在（'超级管理员'）
[PASS] 详情 owner_name 不是 UUID（'张负责'）
[PASS] 详情 create_by_name 不是 UUID（'超级管理员'）
[PASS] 详情 update_by_name 不是 UUID（'超级管理员'）
[PASS] 改名后详情 owner_name = '张负责（改名后）'（实时 join）
[PASS] 合同列表非空（1 条）
== ② 文件列表：上传人是姓名 ==
[PASS] 上传文件成功（HTTP 200）
[PASS] 文件列表行里有 uploader_name 字段
[PASS] uploader_name 有值：'超级管理员'
[PASS] uploader_name 不是 UUID
== ② 帮助文档版本历史：操作人是姓名 ==
[PASS] 建帮助分类（HTTP 200）/ 建帮助文档（HTTP 200）/ 发布（HTTP 200）
[PASS] 版本行里有 create_by_name
[PASS] create_by_name = '超级管理员'
[PASS] create_by_name 不是 UUID
== 全局扫描：任何 *_name 字段都不许是 UUID ==
[PASS] 共扫描 13 个响应，UUID 漏出 0 处
全部通过（② 人员字段全是姓名 / ④ 改名后跟新名 / 全局无 UUID 漏出）
```

> 该脚本还内置一条**机器判据**：递归遍历所有响应，任何以 `_name` 结尾的字段值只要
> 长得像 UUID 就判红 —— 这是「界面上显示 UUID」可自动化的那部分。

### ③ 列表页无 N+1（SQL 数量不随行数增长）

实现侧：`fill_person_names` 收集整页 id → `names_of` 一次 `WHERE id = ANY(...)`。

测试侧（真库用例，`#[cfg(test)]` 计数器 `stats::count()`，thread-local 避免并行用例互相污染）：

```rust
stats::reset();
let detail = contract_detail(Path(contract_id)).await?;   // 3 个人字段
assert_eq!(stats::count(), 1, "一次详情只允许 1 条取名字 SQL");
```

判定结果见 §5 末尾「测试全绿」一行里的 `detail_fills_person_names_and_follows_rename ... ok`。
（3 个字段去重后仍然只 1 条；列表页同理 —— 一页 20 行与 1 行都是 1 条。）

### ④ 用户改名后列表显示新名（实时 join）

同上 HTTP 证据里的 `改名后详情 owner_name = '张负责（改名后）'`；真库用例同时断言
「改名后**不得**还是旧名」（那条断言是给「有人把实时字段改成快照」这种行为设的绊马索）。

### ⑤ 审计类仍是快照（改名后显示旧名）

- 附件上传人：AUD-01 的 `db_uploader_name_is_snapshot_and_survives_rename` +
  `aud01_uploader_name_snapshot.py`（改名后旧附件仍显示旧名、新上传取新名）。
- 状态日志 / 审批 / 操作日志：列本身就是快照（`biz_status_log.operator_name`、
  `biz_flow_task.assignee_name`、`sys_operation_log.username`），本次**未改任何一处**，
  证据见 `V0159__biz_status_log.up.sql:50` 等建表语句。
- 反证：本次给帮助文档版本历史选了**实时**口径（§7 第一条），并写清了「若要快照要怎么改」。

### ⑥ 测试与检查

| 检查 | 结果 |
|---|---|
| `cargo check --all-targets` | 通过（零 error） |
| `cargo test --lib business::shared::user_name` | 3 passed（含新增 `realtime_name_truth_table`） |
| `cargo test --lib business::erp::contract_handler::tests` | 22 passed（含新增 `detail_fills_person_names_and_follows_rename`） |
| `cargo test --lib business::help` | 28 passed |
| `cargo test --lib business::mutex` | 23 passed |
| `cargo test --lib system::dept` | 12 passed |
| `cargo test --lib business::storage` | 78 passed |
| `cargo test --lib stockin / stockout / stocktake` | 见 §5 末尾追加记录 |
| `pnpm typecheck` | 通过 |
| `python3 scripts/verify/aud02_person_name_inventory.py --check` | 13 + 3 条约束全过 |
| `aud02_person_name_e2e.py`（HTTP 真机） | 全部断言 PASS（本机 25 项，见上） |

> 真库用例需要 `ERP_DB_URL`（未设置时静默 SKIP —— 别被「绿」骗了，看耗时与 `--nocapture` 输出）。

---

## 6. 剩余 19 个「出参 DTO 缺 name」逐条判定（N/A，不改）

| 结构体 | 人字段 | 为什么不改 |
|---|---|---|
| `AiUserRes` / `AiUserStatusRes` / `AiUserCredentialRes` / `AiUserDeleteRes` | `user_id` | `user_id` 就是**响应主体本身**，同行有 `username` + `real_name`；没有「另一个人」要译名 |
| `ReaderRes`（公告已读） | `user_id` | 同行有 `username` + `real_name` |
| `PreviewAssigneeRes`（审批预测） | `user_id` | 同行 `name` 就是显示名（试算结果，不是落库实体） |
| `SelfApprovalPersonRes` / `SelfApprovalTicketRes` | `user_id` | 同行 `assignee_name`（自审批风险盘点本来就按审批人名聚合） |
| `EffectiveMaskRes` | `user_id` | 同行 `username` |
| `MutexHolderRes` / `MutexConflictRes` | `user_id` | 同行 `username` + `display_name` |
| `SelfCheckRes`（数据权限自检） | `user_id` | 同行 `username` + `real_name` |
| `ProjectMemberRes` | `user_id` | 同行 `username` + `real_name` |
| `LogRes`（操作日志） | `user_id` | 同行 `username` 是**写入时快照**（审计类，不能换成 join） |
| `OnlineRes`（在线用户） | `user_id` | 同行 `username` |
| `TaskSaveRes` / `TaskAssignRes` / `TaskMemberRemoveRes` | `owner_id` / `previous_owner_id` / `assigner_id` / `user_id` | 写操作**回显**：返回刚写入的 id 供前端更新本地态，随后列表/详情接口带姓名；界面不渲染这三个字段 |
| `TaskTemplateRes` | `owner_id` | 模板默认负责人，只在编辑表单的 `UserSelect`（下拉按姓名显示）里用，列表不展示 |

前端 41 个缺 name 的类型同理，全部是 `*Req` / `*Query` / 表单草稿（`SalesOrderFormDraft`）——
**提交方向不需要名字**。

---

## 7. 遗留与取舍（下一张卡的输入）

1. **帮助文档版本历史选了实时口径**（`create_by_name` 由 join 得来）。若产品要求「显示发布当时是谁」，
   需要给 `sys_help_doc_version` 加 `create_by_name` 列 + 一次性回填（照 AUD-01 的 V0352 模板做）。
2. **ERP 单据的 `create_by_name` 现在界面还没用**（列表 / 详情暂未展示「制单人」列）。
   本次先把 DTO 补齐 + 门禁钉住，等 UI 加列时直接读字段即可，不用再动后端。
3. **`stockin/stockout/stocktake` 的导出 / 批量路径**用的是纯函数 `list_res()`，
   没有回填姓名（导出里若要显示制单人姓名，需在导出 handler 里补一次 `fill_person_names`）。
4. **前端 `storage/file` 的 `uploader-names.ts` 保留**：老后端 / 无权限时仍要用它兜底，
   但兜底终点从「UUID」改成了「—」（`uploaderLabelOf`）。

---

## 8. 复现步骤（从零跑一遍）

```bash
# 0) 门禁
python3 scripts/verify/aud02_person_name_inventory.py --check

# 1) 建空库 + 起隔离实例（自动跑 285 个迁移）
su - postgres -c "psql -c 'drop database if exists cp_aud02_e2e'"
su - postgres -c "psql -c 'create database cp_aud02_e2e owner alun'"
P14_DB=cp_aud02_e2e P14_PORT=18099 bash scripts/verify/p14_instance.sh start

# 2) HTTP 真机
AUD02_BASE=http://127.0.0.1:18099 AUD02_DB=cp_aud02_e2e \
  python3 scripts/verify/aud02_person_name_e2e.py

# 3) 真库用例（注意：不设 ERP_DB_URL 会静默 SKIP）
cd backend && ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cp_aud02_e2e \
  cargo test --lib business::erp::contract_handler::tests -- --nocapture

# 4) 收尾
bash scripts/verify/p14_instance.sh stop   # 或 P14_PIDFILE=... stop
```
