# COST-B2-2【成本】工时导入 —— 实测证据

卡：`t_58a10241`（ARCH-9-3 §8.1 的 **B2-2**，前置 B2-1 `t_2e11875e`）
依据：`docs/architecture/ARCH-9-成本核算升级设计.md`
§4.5.3（8 列模板 + 全有或全无 + 逐行错误）/ §4.5.4（费率优先级 → B2-3）/
§5.2 #15–#16（导入 + 模板两个接口）/ §5.3（`cost-labor:import` 权限码，V0378 已种）/
§3.1 ④（`biz_cost_labor` 的 6 条 CHECK）

## 交付物

| 文件 | 内容 |
|---|---|
| `backend/src/business/erp/cost_labor_import.rs`（**新增**） | 8 列模板定义、xlsx 解析（复用 `impexp::engine::SheetReader`）、逐行纯校验、引用索引（成本对象 / 部门 / 工号）、全有或全无编排、模板生成、`ImportIssue`/`CostLaborImportRes` 出参、23 条单测 |
| `backend/src/business/erp/cost_labor_model.rs` | `insert_params` / `insert_with` / `insert_batch`（同事务批量写）、`object_refs_by_no` / `dept_refs_by_keys` / `employee_table_present` / `employee_ids_by_no`（批量 `IN`、500 一批，防 N+1） |
| `backend/src/business/erp/cost_labor_handler.rs` | 两个 HTTP 入口：`POST /business/erp/cost-labor/import`（multipart `file` + 可选 `period_code`）、`GET /business/erp/cost-labor/template` |
| `backend/src/business/erp/cost_labor_db_tests.rs` | 真库用例 10 → **16** 条（新增 ⑪–⑯，`ERP_DB_URL` 开关） |
| `backend/src/business/erp/mod.rs` | 声明 `pub mod cost_labor_import;` |

迁移：**本卡无迁移**（`biz_cost_labor` 表由 B1-7 的 V0371 建好，`cost-labor:import` 权限码由 V0378 种下）。

## 验收项对照

| 验收（卡内） | 怎么验的 | 结果 |
|---|---|---|
| 上传 xlsx → 解析 → 校验 → 全部通过才入库 | 真库用例 ⑪ + HTTP 冒烟 4/5 | success=3，库里三行（`IMPORT`/`DRAFT`/金额服务端算） |
| 有任何一行错 → 整批拒绝，返回逐行错误（行号 + 原因） | 真库用例 ⑫ + HTTP 冒烟 6/7 | `success=0 failed=3`，行 3/4/5 各自列名 + 原因；列表 total 仍是 3（无半批） |
| 用 §4.5.3 的 8 列模板 | 单测 `template_has_eight_columns_in_design_order` + HTTP 冒烟 3 | 8 列顺序/必填位与设计逐字一致；模板下载 200 且是 xlsx |

## 实测输出

### ⓿ HTTP 冒烟（真实例 `127.0.0.1:18042`，空库首启 + admin 真登录，**31 PASS / 0 FAIL**）

见 `04_http_smoke.out`（可复跑：空库 + 实例起来后 `WORK=/tmp/b22_smoke bash 04_http_smoke.sh`，
需把 `mkxlsx.py` 放到 `$WORK`）。要点：

```
3)  GET /cost-labor/template            HTTP 200  content_type=…spreadsheetml.sheet  size=8577  magic=PK
                                        三个 sheet：数据 / 字段说明 / 填写须知
3b) 模板原样回传                        HTTP 422「文件里没有可导入的数据行（空行与模板示例行会被跳过）」
                                        ← 示例行自动跳过（§4.5.3 之外的防呆）
4)  POST /cost-labor/import（干净 3 行） {code:0, success:3, failed:0, rows:[]}
5)  GET  /cost-labor?period_code=2026-11 total=3
                                        李四 7.5h × 48.5 → 363.7500   （IMPORT/DRAFT）
                                        王五 8h  × 50   → 400.0000    机器工时留空 → 0.00
                                        赵六 70h × 33.333333 → 2333.3333（round 由库侧求值）
6)  POST /import（1 好 3 坏）            {success:0, failed:3}
                                        行3 [成本对象号] 成本对象「CB-NOT-EXIST-9」不存在…
                                        行4 [人工工时]  人工工时 最多 2 位小数（收到 8.555）…
                                        行5 [工资率]    不能为空：自动取数…属 B2-3…
7)  GET  /cost-labor                     total 仍是 3（整批拒绝后没有半批数据）
8)  period_code=2026-10（日期是 11 月）  3 行全报「不在本次导入的会计期间 2026-10 内…请把期间选成 2026-11」
9)  已关批对象                           3 行全报「当前状态是 已关批，只有 OPEN（未关批）的对象能记工时」
10) 非 xlsx                             HTTP 400「文件格式不支持，请上传 Excel 文件（.xlsx）」
11) 未鉴权 / 缺 file 字段                401 / 400「缺少文件字段 file（xlsx）」
```

> 冒烟用的上传文件由 `mkxlsx.py` 用 **Python 标准库手写 zip+xml** 生成（不是本项目自己的写库）：
> 「外面的 Excel 存出来的文件能不能读懂」因此也被验证了一次。

### ① 真库用例（16/16，库 = `mig_replay_costb18`）

见 `01_real_db_tests.out`：

```
test import_writes_every_row_when_the_file_is_clean ... ok              ← 验收项：干净文件全部入库
test one_bad_row_rejects_the_whole_batch_and_nothing_is_written ... ok  ← 验收项：全有或全无 + 逐行错误
test closed_object_rejects_the_batch_with_a_business_reason ... ok      ← 关批对象不可记工时
test date_outside_selected_period_rejects_the_batch ... ok              ← §4.5.3 第 2 列
test employee_number_is_rejected_until_arch9_2_is_online ... ok         ← 工号无处可存时报错，不静默丢
test duplicated_dept_name_requires_the_code_instead_of_guessing ... ok  ← 部门重名不猜车间
（B2-1 原 10 条：crud / 确认幂等 / 66058 / 数据权限 …）              ... ok
test result: ok. 16 passed; 0 failed
```

### ② 单测（`cost_labor*` 76 条，其中新增 23 条）

见 `02_unit_tests.out`。新增的 23 条覆盖：8 列顺序与必填位、模板可被自己读回（只有示例行 → 422）、
xlsx 往返（8 列取值 / 行号 / 示例行跳过 / 缺必需列 → 422）、日期期间、工时与费率精度、
工资率留空点名 B2-3、非数字 / 非法日期文案、必填列各自报错、部门编码/名称/重名三分支、结果形状、
`to_spec` 的 `IMPORT`+`DRAFT`+金额。

### ③ fmt / clippy

`03_fmt_clippy.out`：`cargo fmt --check` 干净；`cargo clippy --all-targets -- -D warnings` 0 warning。

### ④ 全量测试

`05_full_test_summary.out`：`cargo test` 33 个目标 **4592 passed / 0 failed**（EXIT=0）。

## 与设计的偏差（都已在本卡报告）

1. **工资率列留空**：设计 §4.5.3 写「空 → 按 §4.5.4 的优先级自动取」，但费率取数属 **B2-3**，
   `biz_cost_rate` 目前没有读写代码。本卡的处理是**该行报错并点名 B2-3**，绝不默认按 0 计（D-7）。
   表头仍按设计保持「非必填」，等 B2-3 落地后把该行错误换成取数即可（改动点集中在
   `cost_labor_import::parse_row` 的工资率分支 + `ImportContext`）。
2. **员工工号**：`biz_employee` 属 ARCH9-2 未上线。留空 = 按班组计（设计允许）；填了则
   `employee_table_present()` 为假时**报错要求留空**，不静默丢工号。表存在时会按工号解析
   （`employee_ids_by_no`，该分支待 ARCH9-2 上线后才有真数据可测）。
3. **导入期间**：§5.2 #15 只写了「multipart xlsx」，没说期间从哪来。本卡把 `period_code` 做成
   **可选表单字段**：给了就逐行核对「日期落在期间内」（§4.5.3 第 2 列），不给就按每行日期自动归属。
4. **单次行数上限 5000**（`MAX_ROWS`，422 + `60012`）：设计没写；按「50 人 × 31 天 ≈ 1550 行」的量级
   取 3 倍余量，防止一次误传整年文件把事务拉长。
5. **导入落 `DRAFT`**：设计 §4.3 的 `calc` 只归集 `CONFIRMED`，导入是「别人报上来的数」，
   复核后再确认（前端批量确认归 B2-13）。

## 遗留 / 需要别的卡接手（本卡没动）

- **B2-3 费率自动取数**：`rate` 留空 → 对象 > 车间 > 公司，落地后本卡的工资率行错误可撤。
- **数据权限未在写入侧拦截**：与 B2-1 的单条录入一致（`create_labor` 也没拦），列表侧
  `scope::paginate` 才裁剪。若产品要求「非本车间不能导」，需要在导入入口加 scope 判定
  （属权限口径问题，本卡不自行扩大范围）。
- **文件内重复行**（同对象+日期+员工+部门+工时出现两次）未拦：会如实入库两条。设计没要求，
  但**会让人工成本翻倍**；建议前端或后续卡加「重复行提示」。
