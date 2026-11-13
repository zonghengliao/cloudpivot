# HR-34 · 落地 `POST /business/hr/attendance/import`（设计 §5.2 #23 · 薄封装委托 impexp）

看板卡 `t_df5d6d1c`。日期 2026-11-13。

## 1. 为什么开这张卡

设计 `docs/architecture/ARCH-9-人力资源模块设计.md` §5.2 #23 规定考勤导入走
`POST /business/hr/attendance/import`（`biz_type='hr.attendance'`），权限列是
`business:hr:attendance:import`。这张接口在此之前**从未注册**：

- `frontend/src/api/business/hr/attendance.ts` 里只能绕过 HR 前缀去打通用三段
  （`/business/impexp/import/upload` → `/{id}/confirm`），并留着「等 HR 专属路由落地后
  只需换这一个函数体」的 TODO；
- 权限面也错位：考勤导入实际校验的是 `business:impexp:import:upload`，与人事无关。

功能面其实不缺 —— 父卡 `t_de6e8d41`（提交 `22b6f570`，证据
`docs/evidence/2026-11-12-hr22b-import-recalc/`）已经证明：确认入库会重算期间头，
因为重算就挂在 `AttendanceImport::persist` 的同一事务里。本卡补的是**契约面**。

## 2. 实现：一条入口，零份新逻辑

| 项 | 落地 |
|---|---|
| 路由 | `POST /business/hr/attendance/import`（`backend/src/business/hr/attendance_import_handler.rs::import`） |
| 权限码 | `business:hr:attendance:import`（`#[nik::permission]` 声明，与路由同行） |
| 入参 | multipart `file` + 可选 query `mode`（`STRICT`/`PARTIAL`）、`duplicate`（`UPSERT`/`SKIP`）；**没有 `biz_type`** |
| 回执 | impexp 的 `UploadRes`（`task_id` / `task_no` / `status` / `report`），与通用上传同构 |
| 确认入库 | 仍用 impexp 的 `POST /business/impexp/import/{id}/confirm`（理由见 §4） |

委托链（**每一层都是同一个函数，不是复制**）：

```
POST /business/hr/attendance/import            attendance_import_handler::import
  └─ import_for_attendance(mode, duplicate, multipart, claims)      ← biz_type 固定 hr.attendance
       └─ impexp::handler::import_upload_for(biz_type, …)           ← 通用路由也调它
            ├─ resolve_upload_target(…)   参数默认值 / 拒绝面（400/422/404+60011）
            └─ import_upload_core(…)      大小闸 → 落盘 → 建 sys_import_task → run_precheck
                 └─ run_precheck → 定义解析/预检 → 确认入库时 AttendanceImport::persist
                                     （期间头重算 + 锁定拦截都在这里）
```

为此在 `backend/src/business/impexp/handler.rs` 里把原来内联在 `import_upload` 中的三段抽成
三个可复用单元（`resolve_upload_target` / `import_upload_for` / `import_upload_core`），
通用路由改为一行转发。机械拦阻：

- `impexp::handler::tests::there_is_exactly_one_upload_implementation` —— 静态扫源码，
  内核 / 薄封装入口 / 参数解析各只许出现 **1 次**，且通用路由不许自己解析参数或建任务行；
- `hr::attendance_import_handler::tests::handler_owns_no_import_logic` —— 业务侧文件里
  **不许出现** `INSERT INTO` / `ON CONFLICT` / `UPDATE biz_attendance` / `validate::` /
  `.persist(` / `jsonb_to_recordset` / `sha2::`，且必须出现 `import_upload_for(`。

复制一份导入实现的那天，这两个用例会红；而真正的事故形态是静默的：
`validate_batch`（无薪假 ≤ 已批请假）与 `persist` 里的汇总重算**只有一边**跟着改。

## 3. 权限点：不需要新迁移

`business:hr:attendance:import` 早在 `V0387__hr_menu_perms` §2 就种下了
（`sys_menu` uuid `20000000-0000-4000-8000-00000000e023`，`menu_type=3` 按钮「导入考勤」），
§3 的 HR_MANAGER 授权也覆盖它。本卡**不新增迁移**：再插一行只会与
`V0398__hr_selfcheck`（36 菜单 / 35 权限码的断言）口径分叉。

演练库实测（`cloudpivot_test_hr34`）：

```
20000000-0000-4000-8000-00000000e023|business:hr:attendance:import|导入考勤||3|business:hr:attendance:import|1
角色 HR_MANAGER 被授权条数（该权限点）: 1
```

并把「码真的在种子里、且是 menu_type=3 的按钮」变成编译期约束：
`tests::permission_is_seeded_by_v0387`（`include_str!` 直接读 `V0387` 的 up 迁移）。

## 4. 为什么不拆第二个「确认入库」接口

`POST /business/impexp/import/{id}/confirm` 是**业务无关**的：按 `task_id` 读任务行 →
按 SHA-256 校验文件 → 重新校验 → 分批调定义的 `persist`。`persist` 是谁完全由任务行的
`biz_type` 决定（这里必然 `hr.attendance`），所以「HR 专属 confirm」只会是一层不做任何判断的
转发。而**真去做**一个「有意义的 HR confirm」才危险：它得复刻 `sys_import_task` 状态机
（`VALIDATED` 才可入库）、文件指纹校验、`STRICT`/`PARTIAL` 的分批与逐行降级、取消标记、
错误明细回写 —— 任何一处漏掉就会出现「考勤能进库、别处进不去」的双口径。

于是导入契约 = **upload（本卡的新路由）+ confirm（impexp 通用）**，与 §5.2 #24 导出的取舍一致：
只有真正业务专属的动作才另开路由。

## 5. 验证证据（全部真跑，命令与输出见同目录文件）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_unit_tests.txt` | `cargo test --lib -- business::hr::attendance_import_handler:: business::impexp::handler::tests` | 13 passed / 0 failed |
| `02_db_tests.txt` | 145 口径演练库 `cloudpivot_test_hr34`（`make_full_suite_db.sh`：393 迁移 + 演示组织）上的 5 条真库用例 | 5 passed / 0 failed |
| `03_http_smoke.txt` | `scripts/verify/hr/attendance_import_route_smoke.sh`（真后端 `:18248` + 真库 + 真 REST，**不用页面**） | PASS=29 FAIL=0 SKIP=0 |
| `04_gates.txt` | `cargo clippy --all-targets -- -D warnings` + `rustfmt --check` | 见文件 |

### 5.1 真库用例覆盖（`backend/src/business/hr/attendance_import_db_tests.rs`）

| 用例 | 断言 |
|---|---|
| `route_upload_then_confirm_recalcs_period_header` | 走**新路由**上传 2 行 → `report.total/valid/error = 2/2/0`、任务 `biz_type=hr.attendance` / `mode=STRICT` / `options` 含 `UPSERT` → 确认入库 2 行 → 期间头 `employee_count=2 / should_days_total=44.00 / actual_days_total=40.00`，**全程没有调用任何 recalc 接口** |
| `locked_period_import_is_rejected_with_excel_row_no` | 期间锁后：上传照常给报告，确认入库 422 + `60014` + 「已锁定」+ 「Excel 第 2 行」，期间头与记录数一个字节没动 |
| `duplicate_and_mode_options_are_snapshotted` | `mode=PARTIAL` / `duplicate=SKIP` 快照进 `sys_import_task.options`，确认入库只凭 `task_id` 读回 |
| `missing_period_is_reported_with_period_code_and_row_no` | 报不存在的考勤月 → 报告点名期间码 + Excel 行号 |
| `fixture_xlsx_uses_the_template_headers` | 夹具 xlsx 的列头就是模板列头（否则验的是假文件） |

### 5.2 HTTP 冒烟覆盖（`scripts/verify/hr/attendance_import_route_smoke.sh`，29 条断言）

```
[PASS] 未登录上传 → 401（不是 404）                       ← 路由真的注册了（404 会说明没注册）
[PASS] demo_sales（无 HR 导入权限）上传 → 403（403）        ← 权限码真实存在且被校验
[PASS] 403 报文说明是权限问题（含「权限」）
[PASS] admin 走新路由上传 → 200 / report.total=2 / report.error=0
[PASS] 新路由回执字段与通用 upload 同构（report,status,task_id,task_no）
[PASS] 通用 upload 仍可用（对照任务 …）                    ← 抽内核没有改变通用路径行为
[PASS] 任务行 biz_type=hr.attendance（客户端覆盖不了）
[PASS] 确认入库 → 200 / 入库 2 行 / 任务终态 SUCCESS
[PASS] 期间分页：employee_count=2 / should_days_total=44.00 / actual_days_total=40.00（无 recalc）
[PASS] 锁定期间确认入库 → 422 / 含「已锁定」/ 含「Excel 第 2 行」
[PASS] 锁定后期间头一个字节没动 / 锁定后台账仍是 2 行
```

## 6. 前端可切换（供后续卡决定，本卡不动前端）

`frontend/src/api/business/hr/attendance.ts` 的上传函数体换成：

```ts
// POST /api/business/hr/attendance/import?mode=…&duplicate=…  （multipart: file）
request.post('/api/business/hr/attendance/import', formData, { params: { mode, duplicate } })
```

回执字段与通用 upload 完全一致（`task_id` / `task_no` / `status` / `report`），确认入库仍调
`/api/business/impexp/import/{task_id}/confirm` —— 只换 URL，不动解析与提示逻辑。
切不切由前端卡决定（HR-33 之后的那张），本卡只保证契约已存在。

## 7. 遗留 / 已知边界

- 本卡只补**入口**，不新增导入语义；`biz_type` 固定，客户端无法用它把 HR 导入变成任意业务导入。
- `ROUTE_CONTRACT` 里 #23 的落地卡从 `HR-21`（交定义）改成 `HR-34`（交路由）。
- 若将来别的业务模块也要专属导入入口，照本卡套路：固定 `biz_type` + 调 `import_upload_for`，
  并在 `impexp` 的「只有一份实现」用例里继续保持三处计数为 1。
