# HR-17 `seed-from-users`（dry-run + 批量建 + 回写 user_id）

| 项 | 值 |
|---|---|
| 卡 | `t_c498d6fa` · 批 2 员工档案 · 依据 ARCH-9-2 §5.2 #10 / §7.2 存量数据 / §8.1 |
| 接口 | `POST /business/hr/employee/seed-from-users`（权限 `business:hr:employee:create`） |
| 迁移 | **零**（`biz_employee` 由 HR-01 的 `V0383` 建；本卡只读不写迁移） |
| 证据目录 | `docs/evidence/2026-11-12-hr17-seed/` |

## 一、做了什么

新增 `backend/src/business/hr/employee_seed.rs`（+ `employee_seed_db_tests.rs`），把「`sys_user`
里已有、但还没有员工档的人」批量建成 `biz_employee` 行：

```
POST /api/business/hr/employee/seed-from-users
{ "dept_id": "<可选>", "hire_date": "<可选，缺省今天>", "dry_run": <可选，缺省 true> }
→ 200 { dry_run, hire_date, dept_id, scanned, planned_create, created, skipped, failed,
        next_no, preview[ {user_id, username, real_name, dept_id, dept_name,
                           action: CREATE|SKIP|FAILED, reason, message,
                           emp_no, employee_id, error_code} ] }
```

- 判定顺序（先命中先报）：`HAS_EMPLOYEE` → `NOT_HUMAN` → `USER_DISABLED` → `NAME_MISSING`
  → `NAME_TOO_LONG` → `DEPT_MISSING`；
- 落库：`status='ACTIVE'`（设计 §7.2）、工号**自动取号**（复用 HR-14 的 `EMP` 号段，取号与
  INSERT **同事务**）、`user_id` 写进 INSERT 同一条语句、`remark` 留痕「由登录账号 X 批量生成」；
- 敏感字段（身份证 / 生日 / 性别 / 银行卡 / 学历…）**一律不猜**：`NULL` / `0`（未知），由 HR 补齐。

## 二、三个验收项 → 三条实现纪律 → 实测

| 验收项 | 实现纪律 | 实测（真库 + HTTP） |
|---|---|---|
| **dry-run 只报告不写库** | `dry_run` **缺省 true**；判定与落库是两段代码，预览路径一次 `INSERT` 都不发；工号只 `peek`（不耗号） | 真库 `dry_run_writes_nothing_and_eats_no_number`：`created=0`、账号名下 0 行档案、`biz_seq.current_no` 一格未动；HTTP 第 2/3 组：空 body 与 `dry_run:true` 都是 `created=0`，`psql` 复核 0 行 |
| **批量建不回滚已成功的** | 每个候选**自己一个事务**（`BEGIN → 取号 → INSERT → COMMIT`），失败只回滚自己那条，循环继续，失败进 `failed[]` | 真库 `failure_isolates_earlier_successes`：a 建成、b（触发器人为拒绝）失败、c 建成；**号段前进数 == 成功条数**（失败不吃号） |
| **`user_id` 回写正确** | `user_id` 在 `EmployeeWrite` 里，由 INSERT 同一条语句落库（不是先建后 UPDATE） | 真库 `creates_active_employees_and_writes_back_user_id`：逐列读回 `user_id`(=账号 id)、`emp_no`、`status=ACTIVE`、`dept_id`、`hire_date`、`create_by`、`remark`；join `sys_user` 得到同一个 `username`；HTTP 第 4 组同一断言走真 REST |

### 实测汇总

| 项 | 结果 | 日志 |
|---|---|---|
| 纯单测（`employee_seed::tests`） | **10 passed** | `01_unit_and_db_tests.out` |
| 真库用例（`employee_seed_db_tests`） | **5 passed** | 同上 |
| `business::hr` 全模块（真库、单线程） | **195 passed / 0 failed** | 同上 |
| 全量 lib 单测（不连库，DB 用例 SKIP 不假绿） | **5008 passed / 0 failed** | `03_gates.out` |
| HTTP 冒烟（真实例 + 真库，36 条断言） | **36 passed / 0 failed** | `02_http_smoke.out` |
| `cargo fmt --check` | clean | `03_gates.out` |
| `cargo clippy --all-targets -- -D warnings` | 绿 | `03_gates.out` |
| 闸② 数据权限 + 负例自测 | 本卡文件 0 处 FAIL；自测 12/12 通过 | `03_gates.out` |
| 残留 | 演练库已 drop、实例已停、夹具 0 行 | `04_residue.out` |

## 三、复现命令

```bash
# 1) 空库首启（自动跑完全部迁移）：造 hr17_card
su - postgres -c "psql -c \"CREATE DATABASE hr17_card OWNER alun\""
cd backend && NIK_PROFILE=test NIK_DATABASE_NAME=hr17_card NIK_DATABASE_USER=alun \
  NIK_DATABASE_PASSWORD=alun NIK_SERVER_LISTEN=127.0.0.1:18099 \
  NIK_ADMIN_PASSWORD='Admin@123456' \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET='<44+ 字符>' \
  CLOUDPIVOT_STORAGE_MASTER_KEY='<64 hex>' X_CONFIG_SECRET_KEY='<任意>' ./target/debug/cloudpivot

# 2) 真库用例（DB 用例必须单线程：号段断言与其它夹具共享同一张 biz_seq）
HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr17_card \
  cargo test --lib business::hr -- --test-threads=1

# 3) HTTP 冒烟（实例 + 同一个库）
BASE_URL=http://127.0.0.1:18099 ADMIN_PASSWORD='Admin@123456' \
  PSQL_URL=postgres://alun:***@127.0.0.1:5432/hr17_card PGPASSWORD=alun \
  bash ../scripts/verify/hr/seed_http_smoke.sh
```

> 空库首启**必须**注入 `NIK_ADMIN_PASSWORD`：仓库已删除默认口令，未注入时 admin 拿到的是
> 随机一次性口令（只在日志打一次且被脱敏），脚本登录会 401。本卡第一次跑就踩到了这条，
> 处理办法是删掉 `sys_user` 里的 admin 行让她重建（`must_change_password` 随之回到 true，
> 冒烟脚本内置改密流程）。

## 四、与设计的偏差（卡内已上报）

1. **出参比设计多四个字段**：`failed`（设计只有 `created/skipped`，但「不回滚已成功的」必然
   要第三个桶装建失败的）、`planned_create`（dry-run 下 `created` 恒 0 是验收项，而 HR 要知道
   「点下去会建几个」）、`scanned`、`next_no`（dry-run 顺手给下一个工号，`peek` 不耗号）。
2. **`dry_run` 缺省 `true`**（设计未写缺省）：这个接口一次能建 500 个档案，「漏传参数就把人事
   主档写花」比「多看一眼」代价大。
3. **`dept_id` 语义定死为「筛选范围 + 无部门账号的缺省部门」**（设计只列了入参名）：
   传了就只处理「本部门 + 无部门」的账号；不传则处理全部人类账号，账号没部门的进
   `DEPT_MISSING`（不猜部门）。
4. **`hire_date` 缺省 = 本地时区今天**（设计写「取用户选的日期」）：`hire_date NOT NULL`，
   缺省至少不违反约束；用 `Local` 而不是 `Utc`，否则早上 8 点前点按钮会建到前一天。
5. **新增两个跳过原因 `NAME_TOO_LONG` / `NAME_MISSING` 与 `USER_DISABLED`**（设计只提「工号自动取号」）：
   停用账号多半已离职，建成 `ACTIVE` 员工是错的；姓名不能拿登录名冒充（§7.2「机器猜不出来」）。
6. **`user_id` 在 INSERT 内回写**，不是「先建员工再 UPDATE 回写」：同一条语句、同一个事务，
   效果等价且少一次往返；`uk_biz_employee_user` 照常生效。
7. **单次上限 500 条**（超限 400 并提示按部门分批）：逐条开事务，一次几千个会把连接池按几分钟。
8. **`scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST` 补登本卡测试文件一行**（共享文件的
   additive 改动）：闸② 会硬断言「被判成测试面的文件必须登记」，不登记直接 FAIL。

## 五、发现的问题（**未自行处理**，按纪律上报）

1. **闸② `SKIP_MANIFEST` 有 40+ 条存量漏登记**：`attachment/rule_admin/tests.rs`、
   `erp/cost_*_db_tests.rs`…`hr/{contract,employee_dto,employee_handler,employee_model,
   employee_no,employee_sensitive,salary}_db_tests.rs`、`flow/*_tests.rs`、`shared/test_helpers.rs`
   等（本卡只补了自己那一行）。**影响**：`scripts/lint/scope_gate.py` 退出码常 1，
   `scripts/check.sh` 的闸② 常年红；建议由收口卡（HR-49 全量回归）一次性按脚本输出补登。
2. **前端没有「从账号生成员工档」入口**：HR-18 的员工列表页只在空态文案里提到
   `seed-from-users`，没有按钮 / 预览抽屉（设计 §6 要求「先 dry_run 出预览，确认后落库」）。
   本卡是后端卡，未动前端；建议另开一张前端卡（`api/business/hr/employee.ts` 加
   `seedFromUsers()` + 预览抽屉）。
3. **`hr10.*` 夹具残留污染演练库**：跑全模块真库用例时，HR-10 留下的 `sys_user`（`dept_id`
   为空、名叫「HR10 夹具员工」）会被本接口当成可建档候选建出员工（`EMP-0036..0038`）。
   这不是本卡的 bug（本卡就是「把还没档案的人类账号建出来」），但它暴露了 **HR 系列真库
   夹具没有统一清场**：建议后续卡的 `clean()` 顺手清自己名下的 `biz_employee` 行。
   本卡的真库用例已改为「只断言自己 tag 前缀内的行」，不再受这类残留影响。
