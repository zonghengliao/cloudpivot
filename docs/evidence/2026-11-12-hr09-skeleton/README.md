# HR-09 `business/hr/` 模块骨架 —— 验收证据

卡：`t_0db68049`（批 2 员工档案第 1 张）· 设计：`docs/architecture/ARCH-9-人力资源模块设计.md` §8.1

本卡只落**骨架**：模块挂进 `business/` 容器 + 一条真正能工作的体检探针 + 46 条接口契约表。
业务路由一条都不注册（**路由 = 已实现**，未实现的接口应当 404 而不是 500）。

## 1. 文件

| 文件 | 内容 |
|---|---|
| `backend/src/business/mod.rs` | `pub mod hr;` + 模块表补一行 |
| `backend/src/business/hr/mod.rs` | 模块文档（交付批次 / 四条不变式 / 与其它模块的边界 / 已知待办） |
| `backend/src/business/hr/model.rs` | 10 张表常量 + `HR_TABLES` + `presence_sql` + `table_presence()` + `SENTINEL_ORG_ID` + 5 条单测 |
| `backend/src/business/hr/dto.rs` | `HrHealthRes` / `HrTablePresence` + 2 条单测 |
| `backend/src/business/hr/handler.rs` | `GET /business/hr/health` + `ROUTE_CONTRACT`（46 行）+ 6 条单测 |

## 2. 路由注册实测（`02_route_registration.out` + `01_hr_health_200.json`）

实例：`./target/debug/cloudpivot`，`profile=test`，**全新空库 `hr09_card`**（启动自动跑完全部迁移），监听 `127.0.0.1:18091`。

| 请求 | HTTP | 说明 |
|---|---|---|
| `GET /api/business/hr/health`（未登录） | **401** | 框架认证层拦下 —— 说明路由**已注册**（未注册路径不会过认证层） |
| `GET /api/business/hr/nope`（已登录） | **404** | 对照组：未注册路径 |
| `GET /api/business/hr/health`（已登录） | **200** | 响应 `ready=true`、`tables` 10 项全 `present=true`、`api_planned=46` |
| `GET /api/business/hr/employee/page`（已登录） | **404** | 契约里属 HR-12，本卡不注册 ⇒ 404（不是 500，也不是假 200） |

体检响应（`01_hr_health_200.json`）：

```json
{"code":0,"msg":"ok","data":{"module":"hr","prefix":"/business/hr","api_planned":46,
 "tables":[{"table":"biz_employee","present":true}, … 共 10 项 …],"ready":true,
 "hint":"HR 建表迁移已生效（10 / 10 张表在位），接口按设计 §5.2 逐卡落地"}}
```

## 3. 门禁（`03_gates.out`）

| 门禁 | 结果 |
|---|---|
| `cargo check --all-targets` | 绿，**零警告** |
| `cargo clippy --all-targets -- -D warnings` | 绿 |
| `cargo fmt --check` | clean |
| `cargo test --lib`（全量） | **4819 passed / 0 failed** |
| `cargo test --lib business::hr` | **13 passed / 0 failed** |

前端：本卡零前端改动（骨架阶段没有页面），故未跑 `pnpm typecheck/build`；pre-push 门禁的 L0/L1 仍会跑。

## 4. 残留

- 后端实例已停（`18091` 端口空）；
- 演练库 `hr09_card` / `hr09_card_seedless` / 前序 attempt 的 `hr09_skel` 已全部删除（`hr09%` 前缀查库为空）。

## 5. 骨架的取舍（写在这里供后续卡对齐）

1. **不注册空壳路由**：`todo!()` 会 panic 掉请求任务、`Err(未实现)` 会让前端以为接口坏了；两者都比 404 差。
2. **46 条契约表**（`handler::ROUTE_CONTRACT`）：把设计 §5.2 的方法 / 路径 / 权限码 / 落地卡做成编译期常量，配 6 条单测，其中两条是**真实的对拍**：
   - `contract_permissions_are_seeded_by_v0387`：`include_str!` 嵌 `V0387__hr_menu_perms.up.sql`，逐个权限码对拍（码写错线上只是 403，不会有编译错）；
   - `every_macro_path_in_module_is_in_contract`：反向扫描 `business/hr/*.rs` 的 `#[nik::*]` 路径，后续卡把路径拼错（`/employees/{id}`）会当场红。
3. **10 张表名单对拍 DDL**：`table_list_matches_v0383_ddl` 从 `V0383__hr_tables.up.sql` 里正则抽表名与 `HR_TABLES` 比对，少写/多写都红。
4. **探针不挂 HR 权限码**：它的用途是排障（「迁移没跑」还是「权限没配」），而没配角色的账号正是最需要问的人。响应体只含表名与布尔，不含员工数据。先例：`GET /business/erp/cost-switch`。
5. **就绪判定不接受空集合**：`ready = presence.len() == 10 && missing.is_empty()`（`all()` 对空集恒真，会把「什么都没查到」报成健康）。

## 6. 本卡发现（只登记，不动别的模块）

- 设计 §5.2 表头写「42 条」，正文编号到 46、脚注也写 46 ⇒ **以 46 为准**（契约表 46 行、`api_planned=46`）；
- §5.2 脚注「4 处 `/my`」表里只有 2 处，第 3 处 `/leave/my` 随 HR-41 落，第 4 处 §5.2 未列；
- 员工导入 / 导出（§5.2 #11/#12）在 §8.1 的批 2 里**没有排卡**（`ROUTE_CONTRACT` 里 `card = "TBD"`），但权限码 `business:hr:employee:import/export` 已在 `V0387` 种下 ⇒ 卡清单漏项；
- `docs/项目结构与设计亮点.md:26` 仍写「`business/` 当前为空，从 V0020 起」，与 `erp/`、`attachment/`、`impexp/` 等 20 个模块的现状不符（历史遗留，未在本卡修）。
