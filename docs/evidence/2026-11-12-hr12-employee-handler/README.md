# HR-12 证据：员工档案 6 接口 + `SCOPED_TABLES` 加 `BIZ_EMPLOYEE`

卡片：`HR-12【HR】employee_handler.rs 6 接口 + SCOPED_TABLES 加 BIZ_EMPLOYEE + 单测`（批 2 员工档案）
设计：`docs/architecture/ARCH-9-人力资源模块设计.md` §5.2 #1~#6（+ #8）、§4.2.1（状态机）、D-12（数据权限）、D-13（离职）

## 一句话结论

设计 §5.2 的 **#1~#6 六个接口全部落地并能跑通**（另落地了契约里同样标给 HR-12 的 **#8 绑账号**），
`biz_employee` 已登记进 `SCOPED_TABLES` 且 **列表与单条一起按数据范围裁剪**（越权一律 404），
单测 **16 条**（13 纯逻辑 + 3 真库）覆盖权限边界；HTTP 层冒烟 **37/37 PASS**。

| 项 | 结果 | 证据 |
|---|---|---|
| 6 接口（+ #8）真库链路 | 3 条真库用例全绿，16 passed / 0 failed | `01_handler_tests_real_db.out` |
| 数据权限（只能看本部门） | DEPT / SELF / ALL 三种范围逐条断言 | `01_handler_tests_real_db.out` `[HR-12] ③` |
| HTTP 层（路由 / 权限码 / 状态码） | **PASS=37 FAIL=0** | `02_http_smoke.out` |
| 门禁（fmt / clippy / 全量 lib / 前端） | 全绿；全量 lib **4891 passed / 0 failed** | `03_gates.out` |
| 残留 | 端口无监听、演练库已 drop | `04_residue.out` |

## 交付了什么

### 新文件

| 文件 | 行数 | 说明 |
|---|---:|---|
| `backend/src/business/hr/employee_handler.rs` | 2061 | 7 条路由（#1~#6 + #8）、入参 DTO、校验/合并纯函数、状态守卫、详情子表投影、13 条纯单测 |
| `backend/src/business/hr/employee_handler_db_tests.rs` | 801 | 3 条真库用例：六接口往返 / 状态链与守卫 / 部门与本人两种数据范围 |
| `scripts/verify/hr/employee_http_smoke.sh` | 237 | HTTP 冒烟（路由真的注册了吗 / 权限码真的接了吗 / 状态码对不对），37 条断言 |
| `docs/evidence/2026-11-12-hr12-employee-handler/` | — | 本目录 |

### 改动的既有文件

| 文件 | 改动 | 为什么动它 |
|---|---|---|
| `business/shared/scope.rs` | `+ ScopeTarget::BIZ_EMPLOYEE`（`dept_col=dept_id`、`owner_col=user_id`、别名 `e`），并加进 `SCOPED_TABLES` | 卡面要求（数据权限登记；设计 D-12） |
| `business/shared/guard.rs` | 登记表计数断言 14 → 15 + 断言含 `BIZ_EMPLOYEE` | 登记表清单的启动自检口径必须跟着走 |
| `business/permission/scope_model.rs` | `TABLE_WIRING` 加 `biz_employee`（wired） | 第二份清单（`d8_coverage_matches_registry` 要求两处行数相等；不登记 = `uncovered` 红） |
| `business/hr/employee_model.rs` | `EMPLOYEE_TARGET` 改为复用 `scope::ScopeTarget::BIZ_EMPLOYEE`（原来本地 `new(…)` 一份） | 两处各写一份锚点会让「登记的表」与「分页用的锚点」漂移；与 `cost_labor_model.rs::LABOR_TARGET` 同款写法 |
| `business/hr/mod.rs` | `+ pub mod employee_handler` / `#[cfg(test)] mod employee_handler_db_tests` | 挂模块 |

> 没有新增迁移：`biz_employee` / `biz_employee_contract` / `biz_employee_salary` / `biz_salary_item` 在
> `V0383__hr_tables`、`sys_field_mask` 两条规则在 `V0388`，**均已应用且不可改**，本卡纯代码。

## 接口清单（实测通过的形态）

| # | 方法 | 路径（不含 `/api`） | 权限码 | 出参 |
|:--:|---|---|---|---|
| 1 | GET | `/business/hr/employee/page` | `business:hr:employee:list` | `Res::page`（`header/items/meta`），行含 `dept_name` / `position_name` / `status_label` / `contract_end_date` / `contract_expire_in_days` |
| 2 | GET | `/business/hr/employee/{id}` | `business:hr:employee:detail` | 详情 49 键：`*_id/*_name` 成对 + `sensitive_masked`（只打码）+ `contracts[]` + `current_contract` + `salary_items[]` |
| 3 | POST | `/business/hr/employee` | `business:hr:employee:create` | `{id, emp_no}`；新档 `PROBATION` |
| 4 | PUT | `/business/hr/employee/{id}` | `business:hr:employee:update` | `{id}`；整表单提交，缺省字段回填当前值、空串清空 |
| 5 | DELETE | `/business/hr/employee/{id}` | `business:hr:employee:delete` | `{id}`；**软删**（三个唯一键随之释放） |
| 6 | POST | `/business/hr/employee/{id}/status` | `business:hr:employee:update` | `{id, status, status_label}`；转正 / 停薪 / 复职 |
| 8 | POST | `/business/hr/employee/{id}/bind-user` | `business:hr:employee:update` | `{user_id, username}`；传 `null` = 解绑 |

## 关键取舍（都是「设计没写死、必须自己拍」的地方）

1. **`RESIGNED` 在状态接口里一律 409**：设计 §4.2.1 / D-13 规定离职的唯一合法入口是离职单
   （`hr_employee_resign`）审批回调 —— 它要在同一事务里写 `status` + `leave_date` + `leave_type` + 停用账号。
   状态接口放开这条边就能「一键离职而账号还活着」，所以直接拒并点名该走哪条路（离职单接口属批 3）。
2. **`emp_no` 本卡必填**（留空自动取号属 HR-14）：缺号报 400 并写明「自动取号随 HR-14 上线」，
   不做「静默生成 `EMP-0000`」—— 那会在取号卡落地后与新号段打架。
3. **单条入口也裁数据范围**（详情 / 改 / 删 / 状态 / 绑账号）：用与列表**同一份** `scope::filter_for`
   片段做探针，不可见 → **404**（不是 403，403 会告诉越权者「这个 id 真实存在」）。
4. **PUT 的缺省语义**：不传 = 不改、空串 = 清空。`employee_model::UPDATE_SQL` 是整行覆盖，
   接口层若不回填当前值，前端少渲染一个字段就会把数据清掉。
5. **`user_type` 必须是 `HUMAN`**：机器人账号挂员工会让工资表把 RPA 算成一个人（`V0102` 的 `user_type`）。
6. **详情子表只读投影**：`contracts[]` / `salary_items[]` 的**写**接口属 HR-15 / HR-16，
   本卡按 HR-11 定的 `ContractInput` / `SalaryInput` 列投影读取（12 列 + 续签来源自连接 + 薪资项名 join），
   HR-15 落地时把那几列照搬即可，DTO 不用改。
7. **`effective_date` 只在转正时生效**（回写 `probation_end_date` = 转正日）；停薪 / 复职没有对应的
   「生效日」列，忽略该字段（`biz_status_log` 只记原因）。写失败与状态流转同事务，整笔回滚。

## 数据权限（本卡验收项）

`SCOPED_TABLES` 里的锚点：

```rust
ScopeTarget::BIZ_EMPLOYEE = ScopeTarget::new("biz_employee", "e", Some("dept_id"), Some("user_id"))
```

| 范围 | 列表 | 单条（详情 / 改 / 删 / 状态 / 绑账号） | 实测 |
|---|---|---|---|
| `ALL` / 超管 | 全量 | 全量 | `total=3`（三个夹具员工） |
| `DEPT`（本部门） | 只本部门 | 别部门 → **404** | `total=1`；5 个入口逐个断言 404，且**别部门的行没被改动** |
| `SELF`（仅本人） | 只 `user_id = 自己` 那一份 | 同事的档案 → **404** | `total=1`（本人），同事 0 |

> `SELF` 之所以能表达，是因为员工侧有 `user_id`（设计 D-2/D-12）；`user_id IS NULL` 的车间工人在
> `=` 比较下天然不可见 —— 这是有意的：没有账号的人不该被「仅本人」范围看到。

## 怎么复现

```bash
# 1) 真库用例（用一个迁移跑完的库；本卡用的是 hr0389_t1 的克隆 hr12_card，跑完已 drop）
createdb -O alun -T <任一迁移完整的库> hr12_card
cd backend
HR_DB_URL='postgres://alun:***@127.0.0.1:5432/hr12_card' \
  cargo test --lib business::hr::employee_handler -- --test-threads=1 --nocapture

# 2) HTTP 冒烟（隔离实例；空库首启会自动跑 321 个迁移并建 admin）
#    起实例的环境变量见 card 内说明（三把密钥 + NIK_ADMIN_PASSWORD），端口用隔离段 18094/18095
BASE_URL=http://127.0.0.1:18095 ADMIN_PASSWORD='<超管口令>' \
  bash scripts/verify/hr/employee_http_smoke.sh

# 3) 收尾
dropdb hr12_card
```

## 单测覆盖（16 条）

**纯逻辑 13 条**（不连库）：权限码 ∈ `V0387` 种子 / 7 条路由 ∈ `ROUTE_CONTRACT` 且都登记给 HR-12 /
权限属性与契约逐字一致 / 筛选解析（逗号多选、非法值 400、页大小夹紧）/ 新建必填与枚举日期长度 /
修改合并三种语义（不改 / 清空 / 覆盖）/ 状态守卫矩阵（离职 409、原地 409、转正需合同、停薪 ≥5 字、
复职免原因、生效日 ≥ 入职日）/ `biz_employee` 已登记且锚点与模型层一致 / UUID 解析。

**真库 3 条**：

1. `six_endpoints_roundtrip`：建 → 详情（join 名 + `*_masked` + 明文零出现 + 合同/薪资子表）→ 列表 →
   改（改名不改工号）→ 绑账号（撞键 409+68003、解绑）→ 软删（详情 404 / 列表消失）；
2. `status_lifecycle_and_guards`：无合同转正 400 → 补合同转正（试用期截止日回写）→ `biz_status_log`
   三条（含操作人姓名快照）→ 停薪原因 <5 字 400 / 够长成功 → 复职 → 离职 409 → 非法边 409 →
   已离职档案只读 409；
3. `dept_and_self_scope_are_enforced`：DEPT / SELF / ALL 三种范围的列表与单条对照（见上表）。

## 已知待办（不属本卡，已上报）

- **#9 `next-no` 取号属 HR-14**：本卡 `emp_no` 必填（见取舍 2）；
- **#7 明文接口属 HR-13**：本卡出参只有 `*_masked`（HTTP 冒烟断言了响应体不含明文身份证 / 卡号）；
- **#10 seed / #11 导入 / #12 导出**：分别属 HR-17 / TBD / TBD；
- **合同 / 薪资标准的写接口属 HR-15 / HR-16**：本卡详情只读投影；
- **离职单审批（`hr_employee_resign`）属批 3**：在它落地前，`RESIGNED` 只能通过审批回调写入，
  状态接口会明确拒绝并给出路径。
