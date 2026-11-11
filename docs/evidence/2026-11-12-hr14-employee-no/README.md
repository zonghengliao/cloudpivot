# HR-14 证据：工号取号（`EMP` 号段 + 预览 + 新建自动取号）

卡：`t_935b5d4f` · 批次 2 员工档案 · 设计 `docs/architecture/ARCH-9-人力资源模块设计.md`
§5.2 #3（`emp_no` 留空自动取号）、§5.2 #9（`GET /business/hr/employee/next-no`，不耗号）、
D-15（工号是主数据码：`date_pattern='none'`、`period_key='GLOBAL'`，永久不重置）。

## 做了什么

| 处 | 改动 | 为什么 |
|---|---|---|
| `erp/seq.rs` | `MASTER_CODE_TYPES` 6 → 7（+`EMP`）；`master_code_exists_sql` 加 `EMP => SELECT 1 FROM biz_employee WHERE emp_no = ? AND deleted_at IS NULL`；三处「支持 SUP/CUS/MAT/WH/AC/ET」报文补 `/EMP` | 取号器是**白名单**：`EMP` 不登记 ⇒ `next_master_code_free(&tx,"EMP")` 直接 `InvalidValue("不是主数据号段")`。登记后「撞号顺延」也随之生效（软删口径与 `uk_biz_employee_no` 一致 ⇒ 软删工号可复用） |
| `hr/employee_no.rs`（新） | `SEQ_TYPE="EMP"` + `generate_emp_no(&tx)`（取号）/ `preview_emp_no()`（预览，不耗号）/ `GET /business/hr/employee/next-no`；号段不可用 → 500 + 点名「找管理员核对 `biz_seq(seq_type='EMP')`」 | 卡面要求「复用 `seq::next_master_code_free`」：一处真源，不重写发号逻辑。预览与取号分开是设计 §5.2 #9 的语义（点了按钮又取消不能吃号） |
| `hr/employee_handler.rs` | `build_write` 的 `emp_no` 留空 ⇒ `""`（不再 400）；`create_employee` 改成**一个事务**：`begin → (留空则 generate_emp_no) → insert → commit`，任一步失败先 `rollback` | 验收「不跳号」的唯一实现方式：号与员工同生同灭，失败回滚时 `biz_seq.current_no` 一起回滚 |
| `hr/mod.rs` | 挂 `employee_no` + `employee_no_db_tests` | — |
| `scripts/verify/hr/employee_no_http_smoke.sh`（新） | 24 条 HTTP 断言 | `cargo test` 不走 axum 路由/权限宏 |

**零迁移**：`EMP` 号段行由 HR-03 的 `V0386__hr_seq_dict` 已种入（`EMP`/`none`/`GLOBAL`/4 位/启用），
已应用迁移**一个字节都没改**。

## 实测（全部真跑，日志在本目录）

### 1. 真库用例 —— `01_hr_tests_real_db.out`

```
HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr14_card \
  cargo test --lib business::hr -- --test-threads=1 --nocapture
→ test result: ok. 113 passed; 0 failed   （含本卡 13 条：6 纯单测 + 7 真库）
```

本卡 7 条真库用例的关键日志（演练库是**全新空库**，迁移自动跑完）：

```
[HR-14] ① 号段口径 = EMP/none/GLOBAL/4/启用，预览=EMP-0001（current_no 未动）
[HR-14] ② 连建 5 个工号 = [1, 2, 3, 4, 5]            ← 连续 +1、无重复
[HR-14] ③ 并发 6 个工号 = [1, 2, 3, 4, 5, 6]         ← 6 个事务同时取号+插入+提交
[HR-14] ④ 回滚不耗号：两次都拿到 EMP-0001，current_no 保持 0
[HR-14] ⑤ 预览候选 = EMP-0001 → 顺延后 = EMP-0002（被占 EMP-0001）
[HR-14] ⑥ A=EMP-0001 B=失败 C=EMP-0002（after_a=1）  ← B 撞身份证失败，C 仍拿到 A+1（没跳号）
[HR-14] ⑦ 手工=E108503（不耗号） 自动=EMP-0001（+1 格）
```

第 2 轮（在**非空基线**上跑，验证夹具自愈）日志：

```
[HR-14] 基线：已有 EMP 最大流水=1 …  ② 连建 = [2, 3, 4, 5, 6] / ③ 并发 = [2..7]
```

### 2. HTTP 冒烟 —— `05_http_smoke.out`（24/24 PASS）

```
== 结果：PASS=24 FAIL=0 ==
```

覆盖（`cargo test` 覆盖不到的）：未登录 401 → 预览 200 且**没被** `/employee/{id}` 抢走 →
连打两次同值（不耗号）→ 新建不传 `emp_no` 拿到预览那个号 → 再预览 = 已用号 +1 →
手工工号原样保留且不耗号 → 重复工号 409 + 68001 → 缺 `dept_id` 400（不是 500）。

### 3. 门禁 —— `02`/`03`/`04`/`06`

| 门禁 | 结果 |
|---|---|
| `cargo test --lib`（全量，4919 基线） | **4926 passed / 0 failed**（+14 = 本卡 13 + handler 新增 1） |
| `cargo clippy --all-targets -- -D warnings` | EXIT=0 |
| `cargo fmt --check` | EXIT=0 |
| `pnpm typecheck && pnpm build` | EXIT=0（本卡零前端改动，只为证不回归） |

## 残留

演练库 `hr14_card` 上的夹具：`biz_employee` 未删行 **0**、`remark LIKE 'HR14-FIXTURE-%'` **0**
（冒烟那 2 个员工按产品语义走**软删**，带 `deleted_at`；库随后整体 `dropdb`）。
实例 `127.0.0.1:18099` 已停。

## 口径说明 / 上报

1. **`EMP` 查重不带 `org_id`**：`uk_biz_employee_no` 是 `(org_id, emp_no)`，取号查重**故意更严**
   （全局唯一）—— 跨组织撞号时顺延，人工发的号不会在另一组织被重发。已在 `seq.rs` 的
   `SCOPE-EXEMPT` 注释写明理由。
2. **手工工号不校验 `EMP-` 形态**（只 trim + 长度 ≤ 32）：企业自有工号体系（`E2003`）必须能建档，
   与主数据编码口径一致（`erp::seq` 模块文档「手工覆盖」）。
3. **`next-no` 复用 `business:hr:employee:create`**（设计 §5.2 #9 原文），未新增第 6 个员工权限码 ⇒
   不需要新迁移（`V0387` 不可改）。
4. `seq.rs` 里既有单测 `t02_exists_sql_covers_exactly_six_types` 更名为
   `t02_exists_sql_covers_every_master_type` 并补 `EMP` 的三条断言（名字里带「six」会变谎）。
