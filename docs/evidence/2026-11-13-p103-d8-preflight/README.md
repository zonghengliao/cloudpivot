# P1-03（卡号 t_d9958fc7）· d8 覆盖率 + preflight 注册表：复核与加固

日期：2026-11-13 · 执行：backend-developer · 工作区：`/opt/cloudpivot`（99）

## 1. 结论先说

卡面写的「两条恒红」**在动手时（HEAD `9a90557f`）已经不红**。先复核事实，再回答本卡真正的
问题：**为什么红、会不会再红、再红时人能不能一眼看懂**。

| 测试 | HEAD 实测 | 说明 |
|---|---|---|
| `permission::scope_model::tests::d8_coverage_matches_registry` | **绿**（13 passed） | 修复来自 `b50dbf86`（HR-48 全量回归收口） |
| `shared::guard::tests::preflight_uses_the_registry` | **绿**（4 passed） | 同上 |

红 → 绿的成因（`docs/evidence/2026-11-13-hr48-full-regression/README.md` §2.3 原文）：

| 测试 | 当时红的样子 | 根因 | HR-48 的修法 |
|---|---|---|---|
| `d8_coverage_matches_registry` | `TABLE_WIRING` 15 vs `SCOPED_TABLES` 17 | HR-24/HR-20 只登记了 `biz_leave_request` / `biz_attendance_record` 到 `SCOPED_TABLES`，**第二份清单 `TABLE_WIRING` 漏两行** | 补两行 `TableWiring`（含 anchor / reason） |
| `preflight_uses_the_registry` | 17 vs 16 | 登记表从 16 涨到 17 没改写死的计数 | 计数改 17 + 注释补 `biz_attendance_record` |

也就是说：**这不是业务缺陷，是「两份清单靠人手工同步」的结构性脆弱**——加表的人只改了
`scope.rs`，没人提醒他 `scope_model.rs` 还有一份投影清单、`guard.rs` 里还有一个写死的数字。
本卡因此不重复「再修一次数据」（数据已经对了），而是把**这个脆弱点本身**补上。

## 2. 本次改动（2 文件 / +184 −11）

### 2.1 `backend/src/business/permission/scope_model.rs`

1. 新增对账内核 `WiringDrift { missing, extra, duplicated }` + `drift_between()`
   + `wiring_drift()`（对真实 `SCOPED_TABLES` × `TABLE_WIRING`）。
2. `d8_coverage_matches_registry` 的第一条断言改成对账：
   红时**直接点名**差哪几张表（缺登记 / 多登记 / 重复登记三类分开报，并打印两份清单全文），
   不再只有一句「行数必须相等（15 vs 17）」让人去 grep。
3. 新增 `d8d_drift_detects_missing_extra_and_duplicates`：用合成清单反证判据本身有牙——
   ① 漏一行 → `missing` 点名且不误报 `extra`；② 表名拼错 → `extra` 点名；
   ③ **行数相等但重复一行**（掩盖另一行漏登记）→ `duplicated` + `missing` 同时点名。
   第 ③ 条是原来「只比行数」抓不到的形态。

### 2.2 `backend/src/business/shared/guard.rs`

`preflight_uses_the_registry`（测试名就是它的契约）原来只断言「17 张 + 抽查 8 个常量」，
**没有验证 `preflight_permission` 真的走注册表**。补三处：

1. 计数抽成 `const EXPECTED_TABLES`，失败消息打印**全部 17 张表名** + 三步修改指引
   （① 同步 `TABLE_WIRING` ② 改这个常量 ③ 重跑两个测试）。
2. 补上注释里写了、断言里漏了的 `BIZ_ATTENDANCE_RECORD`（第 17 张是人工登记的，最容易被漏掉）。
3. 源码漂移守卫：截取 `preflight_permission` 正文，断言它包含
   `SCOPED_TABLES.iter().map(|target| target.table)` / `collect_missing_tables(SCOPED_TABLES` /
   `checked_tables: SCOPED_TABLES.len()`，且**不含**硬编码表名数组
   —— 写死表名正是「新表静默漏检」的入口。截断正文过短会单独报红（防止锚点提前命中导致假绿）。

## 3. 验证（命令 + 实测输出）

### 3.1 卡面验收命令

卡面写的是：

```bash
cargo test --lib business::permission::scope_model business::shared::guard -- --test-threads=1
```

**这条跑不了**——`cargo test` 只接受一个过滤串，实测报
`error: unexpected argument 'business::shared::guard' found`。正确的等价命令（两次）：

```bash
cd backend
cargo test --lib -- --test-threads=1 business::permission::scope_model   # 13 passed / 0 failed
cargo test --lib -- --test-threads=1 business::shared::guard             # 4 passed / 0 failed
```

原始输出：`01_scope_model_serial.log`、`02_guard_serial.log`。

```
test business::permission::scope_model::tests::d8_coverage_matches_registry ... ok
test business::permission::scope_model::tests::d8d_drift_detects_missing_extra_and_duplicates ... ok
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 5540 filtered out

test business::shared::guard::tests::preflight_uses_the_registry ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 5549 filtered out
```

### 3.2 反证：新守卫真的会红（不是「永绿断言」）

把 `preflight_permission` 里的 `checked_tables: SCOPED_TABLES.len()` 换成写死的 `9`
（模拟「绕过注册表」），**跑的是同一个已编译二进制**（该测试运行时读源码，无需重编译）：

```
preflight_permission 没有走注册表：正文里找不到 `checked_tables: SCOPED_TABLES.len()`。
规范：启动自检只能遍历 SCOPED_TABLES，写死表名会让新表静默漏检
test result: FAILED. 0 passed; 1 failed
```

原始输出：`03_negative_probe_hardcoded_count.log`（探针在临时目录里改副本，**没有动工作区源码**）。

### 3.3 邻域回归 + 静态检查

```bash
cargo test --lib -- --test-threads=1 business::permission   # 28 passed / 0 failed
cargo test --lib -- --test-threads=1 business::shared       # 144 passed / 0 failed
cargo clippy --lib --tests -- -D warnings                   # Finished，零 warning
```

## 4. 边界

- **零业务行为变更**：只动两个测试与其诊断内核；`SCOPED_TABLES` / `TABLE_WIRING` 清单内容未改，
  没有新增迁移（登记表仍是 17 张）。
- `preflight_uses_the_registry` 里写死的 17 是**刻意保留**的：加表必须有人来改一次数字并
  顺手想清楚「这张表进不进启动自检」；本次只是把报错变成可操作。
- 遗留：工作区里 `docs/planning/20261113-新卡清单.md` 是别的卡留下的未跟踪文件，未纳入本卡提交。
