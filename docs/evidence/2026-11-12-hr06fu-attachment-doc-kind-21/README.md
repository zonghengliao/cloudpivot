# HR-06-FU：`attachment/model.rs` 的 DocKind 17 → 21

**卡面**：HR-06 发现设计 ARCH-9-2 §8.3 R10 的「四处同步」漏了第 5 处 ——
`backend/src/business/attachment/model.rs` 自己还有一套 `DocKind`（17 值），
上传接口 DTO（`handler::upload` 的 `doc_type`）就是按它的 `parse` 判的。
不补则上传 `EMPLOYEE / EMPLOYEE_CONTRACT / LEAVE_REQUEST / SALARY_SHEET` 一律 **400**。

## 改了什么

| 文件 | 变更 |
|------|------|
| `backend/src/business/attachment/model.rs` | +4 常量 `DOC_EMPLOYEE` / `DOC_EMPLOYEE_CONTRACT` / `DOC_LEAVE_REQUEST` / `DOC_SALARY_SHEET`；+4 枚举变体；`ALL` 17 → **21**（追加末尾，与 `shared::doc_kind::ATTACHMENT_DOC_KINDS` 同序同值）；补 `parse` / `as_str` / `label` / `table` / `exists_sql` / `view_permissions` / `FileRole::catalog` 的 match 臂 |
| `backend/src/business/attachment/tests.rs` | 计数断言 17 → 21、权限表 17 → 21 行（+4 HR 行）、新增 `hr_doc_kinds_parse_map_and_mirror_shared_vocabulary`（含**与共享词表逐字比对**的防漂移断言）、`role_catalog` 补 4 类分组断言 |

映射取值：

| DocKind | 表 | 存在性判据 | 模块读码（V0387 真实菜单码） | 面板默认角色 |
|---|---|---|---|---|
| `EMPLOYEE` | `biz_employee` | `deleted_at IS NULL` | `business:hr:employee:list` + `:detail` | `DEFAULT` |
| `EMPLOYEE_CONTRACT` | `biz_employee_contract` | `deleted_at IS NULL` | `business:hr:employee:detail` + `business:hr:contract:manage` | `CONTRACT_SCAN`（+ `DEFAULT`） |
| `LEAVE_REQUEST` | `biz_leave_request` | `deleted_at IS NULL` | `business:hr:leave:list` + `:detail` | `DEFAULT` |
| `SALARY_SHEET` | `biz_salary_sheet` | `deleted_at IS NULL` | `business:hr:salary:list` + `:detail` | `DEFAULT` |

劳动合同台账挂在员工详情里（没有独立的 `contract:list`），故读码取「员工详情 ∪ 合同管理」。
7 个菜单码在真库 `sys_menu` 均已存在（`view_permissions_are_real_menu_codes` 回查通过）。

## 证据文件

| 文件 | 内容 |
|------|------|
| `01_http_drill.out` | 真实 HTTP 实测：4 类 HR 附件上传均 **200**；未知 `doc_type` 仍 400（错误文案列出全部 21 值）；软删员工 → 404；落库回查 4 类各 1 行 |
| `02_unit_tests_core.out` | `cargo test --lib business::attachment::tests`（真库 `att21_fu`）：**65 passed / 0 failed**，0 SKIP（39.6s，真连库） |
| `03_unit_tests_rule_admin.out` | `cargo test --lib business::attachment::rule_admin`（真库）：**14 passed / 0 failed**，0 SKIP |
| `04_clippy.out` | `cargo clippy --all-targets -- -D warnings`（改动文件强制重编） |

> 注：`business::attachment` 一次性整跑会出现 1 条 `SKIP: set_db 失败（数据库资源已初始化）`
> —— `tests.rs` 与 `rule_admin/tests.rs` 各有一份 `ensure_db`/`set_db`，同一进程里第二个
> 必然拿不到全局 `db()`。所以真库用例按模块分开跑（02 / 03 两个文件）。

## 实测环境（可重现）

```bash
# 1) 建干净库 + 跑一遍迁移（纯迁移链，V0383 建 HR 表 / V0387 种菜单码 / V0389 放宽 CHECK 到 21 值）
su - postgres -c "createdb -O alun att21_http"
NIK_DATABASE_NAME=att21_http ... ./backend/target/debug/cloudpivot   # 321 个迁移
# 2) 打夹具 + 走 HTTP 上传（脚本见提交说明；落库回查 biz_doc_attachment）
# 3) 单测走同一个库
ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/att21_fu' cargo test --lib business::attachment::tests
```

演练库 `att21_fu` / `att21_http` 用完即删（`dropdb --force`），实例已停，无残留进程/端口。

## 顺带守住的东西

新增的单测里有一条**跨词表比对**：`attachment::model::DocKind::ALL` 的 `as_str` 序列必须与
`shared::doc_kind::ATTACHMENT_DOC_KINDS` 逐字同序同值。这次的问题就是「同一份词表两处实现、
只改了一处」，这条断言让下一次漏改在 `cargo test` 阶段就红，而不是等界面上传 400。
