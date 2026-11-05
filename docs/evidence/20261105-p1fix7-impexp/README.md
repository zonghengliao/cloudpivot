# P1-FIX-7 导入导出从未被使用：模板入口深、无字段说明

任务卡：`t_73c3309d`（frontend-developer）· 2026-11-05

---

## 1. 评审结论复核（先纠正两处，再交付）

评审给的三条里，**有一条半与代码事实不符**，先贴证据再改：

| 评审说法 | 实测（改动前的 HEAD `60413fa`） | 结论 |
|---|---|---|
| 「没有字段说明」 | 模板 xlsx **已有**「数据 / 字段说明 / 填写须知」三个 sheet，「字段说明」逐列给出 字段名/必填/类型/长度/可选值/说明（`backend/src/business/impexp/template.rs::build_template`）。见 `template_sheets_round3.txt` | **模板里有**，但**页面上看不到**：用户必须下载模板、切到第 2 个 sheet 才知道怎么填 —— 这才是「不知道该怎么填」的真因 |
| 「导入报错不精确」 | 上传预检返回的 `RowIssue` 带 `row_no`（Excel 真实行号）+ `column_header`（中文列名）+ `raw_value` + `error_code`；页面已有「行号 / 列 / 原始值 / 错误码 / 失败原因」表 | **后端与页面都已精确到行/列**，本轮只补了口径提示（行号=Excel 实际行号）并做真机取证 |
| 「模板入口深」 | 模板下载按钮只在 `/business/impexp/import` 向导页，且**必须先选业务**；物料/供应商/单位/科目/仓库/库位/字典项 7 个可导入业务页**没有任何导入入口** | **成立**，这是 `sys_import_task` 长期 0 行的主因 |

> 所以本卡的实质是「把已经存在的能力搬到用户眼前」，而不是重写导入引擎。

---

## 2. 改了什么

### 后端（1 个只读接口，1 个 DTO，1 个单测）

| 文件 | 改动 |
|---|---|
| `backend/src/business/impexp/dto.rs` | 新增 `ColumnQuery`（= `TemplateQuery` 别名）、`ImportColumnRes`（逐列说明）、`ImportColumnsRes`（业务摘要 + 列清单） |
| `backend/src/business/impexp/handler.rs` | 新增 `GET /business/impexp/def/columns?biz_type=...`（`def_columns`，权限沿用 `business:impexp:def:list`，**不新增权限码、不新增迁移**）+ `columns_of()` 摊平函数（与模板「字段说明」sheet 同一数据源）；接口表补一行；新增单测 `columns_of_matches_the_template_definition` |

设计要点：**页面字段说明不照抄文案**，一律来自 `ImportColumn`/`ColumnKind`，因此「页面看到的说明」与「下载到的模板」不可能不一致（模板改了页面自动跟着改）。

### 前端（2 个新组件 + 向导页改造 + 7 个业务页入口）

| 文件 | 改动 |
|---|---|
| `frontend/src/api/business/impexp/index.ts` | 新增 `getImportColumns(bizType)` + `ImportColumnRes` / `ImportColumnsRes` 类型；挂到 `impexpApi` |
| `frontend/src/components/ImportFieldTable.vue`（新） | 字段说明表：表头（带 `*` 与「唯一」标记）/ 必填 / 类型 / 长度范围 / 可选值 / 说明（外键列标「引用 计量单位」）/ 示例 |
| `frontend/src/components/ImportEntryButton.vue`（新） | 业务页「导入」按钮 + 弹窗：**一键下载模板**（直接落盘，不跳页）、**字段说明表**、**打开导入向导**（带 `?biz_type=` 预选） |
| `frontend/src/views/business/impexp/import/index.vue` | ① 顶部新增「导入模板（一键下载）」模板中心：7 个可导入业务逐行给「下载模板 / 字段说明 / 用它导入」，**不用先选业务**；② 表单下方常驻「字段说明 · 物料」表（业务一切换即刷新，带缓存）；③ 「放大查看」抽屉；④ 读 `?biz_type=` 预选业务；⑤ 失败明细补口径提示 |
| `frontend/src/views/business/erp/material/index.vue`、`supplier/`、`unit/`、`account/`、`warehouse/`、`location/`、`frontend/src/views/support/dict/index.vue` | 工具条各加一个「导入」入口（`ImportEntryButton`），业务编码分别对应 `business.erp.material/supplier/unit/account/warehouse/location` 与 `system.dict_item` |

> 会计科目页是**只读**页（没有新建按钮），批量导入是它唯一「成批建科目」的入口，因此也给了入口。

---

## 3. 验收对照

| 验收项 | 结果 | 证据 |
|---|---|---|
| □ 模板一键下载 | ✅ 三个位置都能一键下载并真的落盘：业务页弹窗、向导模板中心（逐行）、向导表单 | `01-material-import-dialog.png`（含「模板 v1 · 共 8 列 · 单次最多 20,000 行」）；浏览器下载目录实测新增 `物料导入模板_v1.xlsx` / `供应商导入模板_v1.xlsx`；接口 `/api/business/impexp/import/template` 200，transferSize 7604；e2e `②③` 断言 `download` 事件与文件名 |
| □ 字段说明可见（截图） | ✅ 页面内直接展示（不再需要打开 Excel 第 2 个 sheet）：业务页弹窗、向导页常驻表、抽屉 | `01-material-import-dialog.png`、`02-wizard-template-center-and-fields.png`、`02b-field-drawer.png`、`01b-dict-import-entry.png` |
| □ 故意传个错的，报错精确到行/列 | ✅ 造了 6 行（1 行合法 + 5 行分别缺必填/枚举错/数字错/超长/外键不存在），返回 5 条错误，条条带行号+列名+原始值+错误码 | `03-precheck-row-column.png`、`upload_bad_round1.txt`：第 3 行「物料名称」REQUIRED；第 4 行「成本法」ENUM；第 5 行「安全库存」TYPE；第 6 行「物料编码」LENGTH；第 7 行「基本单位」FK_NOT_FOUND |
| □ 真机截图 3 张 | ✅ 5 张（3 张必需 + 抽屉 + 字典页入口） | 本目录 `01/01b/02/02b/03*.png`（1280×633，真实浏览器 127.0.0.1:5141 + 真后端 18141 + 副本库） |
| □ git commit | ✅ 见卡内汇报的 commit |

**额外自证（说明「能力真的通」）**：同一批次切 `PARTIAL` 模式确认入库 → `status=PARTIAL / imported_rows=1`，`biz_material` 里真的多了 `IMP-TEST-0001 | 正常行-不该报错`（`confirm_partial_import.txt`），`sys_import_task` 由 0 变成有行（`db_sys_import_task.txt`，副本库口径）。

---

## 4. 连跑两轮

同一份「故意造错」的 xlsx 连传两次（独立任务）：

```
round1: task_no=IMP20261105-3C40  total=6 valid=1 error=5
round2: task_no=IMP20261105-4C0D  total=6 valid=1 error=5
diff upload_res.json → 仅 task_id / task_no 不同，报告逐字一致
```

（`upload_bad_round1.txt` / `upload_bad_round2.txt`）

---

## 5. 顺手修掉的一个 e2e 地基缺陷（P1-FIX-6 遗留）

第一轮跑 regressions 时 `26-pages-impexp-storage` 的 5 个页面 `beforeAll` **全部 120s 超时**、
25 条用例没跑。根因不在页面，在 `frontend/e2e/_pagecheck.ts`：

```
page.on('response') → 对每条 /api 响应 res.json() 排进 pending → await Promise.all(pending)
```

P1-FIX-6 把实时推送从 `EventSource` 改成 `fetch + ReadableStream` 后，`/api/business/notice/stream`
是一条**永不结束**的响应：`res.json()` 永远等不到流结束 ⇒ `Promise.all(pending)` 永不返回 ⇒
每一页的 beforeAll 都超时。修法：**只对 `content-type` 含 `json` 的响应解析体**（流本来就没有 JSON 体，
解析它本身就是错的）。修完同一批用例 33 passed / 1 failed（唯一红是我新写用例里字典页入口位置不对，
已把字典项入口从「选中类型后才渲染」的右栏移到页级工具条，重跑 4/4 全绿）。

- 修前：`e2e_round0_before_harness_fix.txt`（7 failed / 25 did not run）
- 修后：`e2e_26_pagerunner_after_fix.txt`（33 passed，1 failed 是新用例自身问题）
- 新用例重跑：`e2e_36_new_spec.txt`（4 passed）

> 注意：`11-erp-master-data` 的 ①②③ 仍会红，原因是那两条用例钉死「种子 3 条计量单位 / SUP-0001」，
> 而本次跑的是**生产快照副本**（28 个计量单位）——属于数据口径不符，与本卡改动无关（第一次跑就是这样）。

---

## 6. 环境与复现

| 项 | 值 |
|---|---|
| 后端 | 127.0.0.1:18141（`profile=test`，二进制来自本轮改动，`/root/p1fix5c-target/debug/cloudpivot`） |
| 库 | `cloudpivot_imp7`（本机 `cloudpivot_sse6` = 生产快照的副本，**不是生产**） |
| 前端 | 127.0.0.1:5141（vite dev，`/api` 反代 → 18141） |
| 改造前基线 | 127.0.0.1:18140（HEAD `60413fa` 的二进制，同一副本族 `cloudpivot_sse6`） |

复现脚本（本目录 `scripts/`）：

```bash
# 定义列表 + 登录（把 token 写到 /tmp/impexp-token.txt）
python3 scripts/impexp_probe.py http://127.0.0.1:18141 admin <password>

# 下载全部 7 个可导入模板并逐 sheet 打印（含「字段说明」）
uv run --with openpyxl python scripts/impexp_tpl_dump.py http://127.0.0.1:18141 /tmp/tpl

# 故意造错文件并上传，打印行号/列名/错误码
uv run --with openpyxl python scripts/impexp_bad_upload.py http://127.0.0.1:18141 business.erp.material /tmp/bad
```

---

## 7. 没做的 / 边界

1. **Excel 第二行备注**没做：第 2 行是**示例行**（导入时按 `skipped_example` 自动跳过），插一行备注会破坏这条既有契约；卡面给了「或页面上展示字段表」的替代口径，本卡按替代口径做在页面上，并保留模板内的「字段说明」sheet。
2. **枚举列下拉数据验证**仍未做（`rust_xlsxwriter` 的 `data_validation` 未在 `nik-utils` 暴露，`template.rs` 里已登记为已知边界）。
3. 生产 144 **未部署**：本卡只到 99 的隔离副本；发版后 `sys_import_task` 才会开始落行。
4. 生产数据里 `sys_import_task` 仍会是 0 行直到第一次真实使用；本卡解决的是「找不到入口」，不是「替他导一次」。
