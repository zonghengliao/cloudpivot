# HR-15 证据：劳动合同台账 model/dto/handler（3 接口 + 续签链）

卡：`t_f14093e5` · 批次 2 员工档案 · 设计 `docs/architecture/ARCH-9-人力资源模块设计.md`
§3.1 ②（`biz_employee_contract` DDL）、§5.2 #13~#15（三条写接口）、D-5（合同独立成表、链式续签）。

## 做了什么

| 处 | 改动 | 为什么 |
|---|---|---|
| `hr/contract_model.rs`（新，~700 行） | 合同**唯一** SQL 出口：`insert` / `update`（可写列白名单）/ `find_in` / `lock_in` / `list_by_employee`（台账时间线）/ `chain_of`（递归 CTE 回溯续签链）/ `has_active` / `set_status`（CAS）/ 唯一键 + 7 条 CHECK 的冲突翻译 | 合同读取原先散在 `employee_handler::load_contracts` 的裸 SQL 里；本卡把它收成一处，详情 / 回执 / 续签链看到同一份投影（`renewed_from_no` 自连接取，不许 N+1） |
| `hr/contract_dto.rs`（新，~960 行） | 入参三件套（新建 / 修改 / 续签）+ 校验纯函数 + `StatusAction` 解析 + 续签回执 `ContractRenewRes` | 校验口径写死在一处并逐条对拍：编号 ≤32、类型 5 选一、`sign_date ≤ start_date`、`end_date ≥ start_date`、试用期 0~6、`UNFIXED_TERM ⇔ 无终止日`（**反向也拦**，见下方偏差）、解除要日期 + ≥5 字原因 |
| `hr/contract_handler.rs`（新，~560 行） | 三条接口 + 业务函数：`create_contract` / `update_contract` / `renew_contract` | 续签 = 建新合同 + 封旧合同 + 接链，**一个事务**；任一步失败整笔回滚（旧合同原样 ACTIVE，用户改编号重试即可，不留死局） |
| `hr/employee_handler.rs` | `load_contracts` / `active_contract_exists` 改成调 `contract_model`（行为不变，SQL 搬家） | 合同 SQL 只留一个出口；员工详情 `contracts[]` / 转正守卫与本卡接口同源 |
| `hr/mod.rs` | 挂 4 个新模块（含 `contract_db_tests`） | — |
| `shared/error_code.rs` | 68xxx 段 +2：`68005` 合同编号重复 / `68006` 合同操作被规则拦下；码值冻结单测同步 | 与 HR-10 的 68001~68004 同一段、语义分开（撞键 ≠ 状态不允许） |
| `scripts/verify/hr/contract_http_smoke.sh`（新） | 54 条 HTTP 断言 | `cargo test` 不过 axum 路由 / 权限宏 / JSON 反序列化 |

**零迁移**：`biz_employee_contract` 与 `uk_biz_emp_contract_no`、7 条 `ck_biz_emp_contract_*` 全部由
HR-01 的 `V0383__hr_tables` 建好且已应用，**已应用迁移一个字节都没改**（AGENTS.md 约定 6）。

## 实测（全部真跑，日志在本目录）

### 1. 纯单测 —— `01_unit_tests.out`

```
cargo test --lib business::hr::contract
→ test result: ok. 31 passed; 0 failed
```

覆盖：约束名与 `V0383` 逐字对拍 / 7 条 CHECK 都有专属人话 / `?` 占位符与绑定参数个数对拍 /
`UPDATE_SQL` 不碰 7 个受保护列 / 列表与链的 SQL 形态（自连接 + `ORDER BY start_date DESC` +
`WITH RECURSIVE … ORDER BY depth DESC`）/ 行→DTO 投影（空串来源编号 → `None`）/ 终态锁判定 /
新建默认值与逐字段报错 / 无固定期限与终止日互斥 / 修改合并语义 / 解除字段校验 /
续签链规则（编号必须换、不能倒着接链、试用期默认 0）/ 三条宏路由与 `ROUTE_CONTRACT` 对拍 /
权限码在 `V0387` 种子里。

### 2. 真库用例 —— `02_db_tests.out`

```
HR_DB_URL=postgres://alun:***@127.0.0.1:5432/hr15_card \
  cargo test --lib business::hr::contract_db -- --test-threads=1 --nocapture
→ test result: ok. 6 passed; 0 failed; finished in 6.56s
```

| 用例 | 断言要点（真库日志原文） |
|---|---|
| ① CRUD + 到期字段 | `[HR-15] ① 新建 HT15-A-… expire_in_days=Some(20)`；改成无固定期限后 `expire=None`；已过期 5 天 `expire=Some(-5)`；空串备注 → `None`；编号不可被牵连 |
| ② 撞号 / CHECK | `撞号 → 409 68005`；`CHECK 违反 → 400 10001（签订日期不能晚于起始日期…）`（绕过 DTO 直插，证明真库约束名与 `check_message` 映射成立） |
| ③ **续签链追溯到原合同** | `续签链 = ["HT15-C1-…", "HT15-C2-…", "HT15-C3-…"] → 可回溯到原合同 HT15-C1-…`；链首 `renewed_from_id = None`；逐级 `renewed_from_no` 成对；员工详情 `contracts.len()==3` 且新的在前、`current_contract` = 唯一 ACTIVE；`对已续签再续 → 409 68006` |
| ④ 解除 + 终态冻结 | `解除落库 + biz_status_log 两条（["ACTIVE","TERMINATED"]）`；解除后 `expire_in_days=None`；改字段 / 再续签都是 `409 68006` |
| ⑤ 离职拦截 + 补录可续 | `补录 EXPIRED → 续签：EXPIRED ⇒ ACTIVE（链长 2）`；`离职员工新签 → 409 68006（员工 HR15-edge-… 已离职…）` |
| ⑥ 并发续签 | `并发 4 续签：成功 1 / 409 3；链上后继 1`（行锁 + 状态 CAS，续签链不分叉） |

### 3. HTTP 冒烟 —— `05_http_smoke.out`（54/54 PASS）

```
BASE_URL=http://127.0.0.1:18097 ADMIN_PASSWORD='Admin@123456-changed' \
  bash scripts/verify/hr/contract_http_smoke.sh
== 结果：PASS=54 FAIL=0 ==
```

覆盖 `cargo test` 覆盖不到的：未登录 401 → `POST /employee/{id}/contract` 没被 `/employee/{id}` 抢走 →
到期提醒 30 天 → 员工详情 `contracts[]`/`current_contract` 跨请求长出来 → 撞号 409+68005 →
缺 `contract_no` 400 → 续签后 `previous.status=RENEWED` + `contract.renewed_from_id` 指回原合同 +
`chain` 长度 2 且首尾正确 → 详情两行新的在前 → 已续签再续 409+68006 → 解除缺日期 400 / 补齐 200 →
已解除改字段 409+68006 → 空 `PUT` 409。

### 4. 门禁

| 门禁 | 结果 |
|---|---|
| `cargo check --all-targets` | EXIT=0，零警告 |
| `cargo clippy --all-targets -- -D warnings` | EXIT=0（`06_clippy.out`） |
| `cargo fmt --check` | clean |
| `cargo test --lib`（全量，带真库，`--test-threads=1`） | **4957 passed / 0 failed**（HR-14 基线 4926，+31 = 本卡 31 条：25 纯单测 + 6 真库）—— `07_full_lib_tests.out` |
| `pnpm typecheck && pnpm build` | EXIT=0（`08_frontend.out`；本卡零前端改动，只为证不回归） |

## 偏差 / 需要下游知道的事

1. **`UNFIXED_TERM ⇔ 无终止日` 的反向也拦**（DDL 的 `ck_biz_emp_contract_unfixed` 只约束一半）：
   `FIXED_TERM` 等类型必须给 `end_date`，否则算不出到期提醒（`idx_biz_emp_contract_due` 按 `end_date` 扫），
   台账里会出现「永不到期」的哑弹。想录无固定期限就改 `contract_type`。
2. **`biz_status_log` 现在会写合同**：建单一条（`null → ACTIVE`，`operator_name` 有值）、
   续签一条（`ACTIVE/EXPIRED → RENEWED`）、解除一条（`ACTIVE → TERMINATED`，`reason` = 解除原因）。
   依赖 `biz_status_log` 计数做统计的下游（若有）要把 `EMPLOYEE_CONTRACT` 排除或计入。
3. **`PUT /contract/{id}` 允许 `to_status`**（`EXPIRED` 手工标到期 / `TERMINATED` 解除），
   边由 `shared::state_machine` 判（`ACTIVE→EXPIRED/TERMINATED/RENEWED`、`EXPIRED→RENEWED`）；
   已解除 / 已续签的合同一律 409+68006（审计凭据不可改）。
4. **补签（到期后续签）必须显式给 `sign_date`**：DDL 约束 `sign_date ≤ start_date`，
   补签的 `start_date` 在过去，`sign_date` 默认「今天」会被 400 拦下 —— 这是库的口径，不是本卡的发明。
5. **没有新增「合同详情」GET 接口**：设计 §5.2 只给三条写接口，合同读取走员工详情的
   `contracts[]` / `current_contract`（HR-12 已落地，SQL 由本卡供给）。多开一条会让「详情吃哪份形状」出现两个答案。
6. **`hr/employee_handler.rs` 被改了两处**（`load_contracts` / `active_contract_exists` 改为调
   `contract_model`）：SQL 搬家、行为不变，HR-10/11/12/13/14 的 113 条用例全绿背书。

## 残留

演练库 `hr15_card`（含冒烟夹具，员工走软删）+ 实例 `127.0.0.1:18097` 用完即停、库随后 `dropdb`
（见 `09_residue.out`）。仓库内未留下临时文件。
