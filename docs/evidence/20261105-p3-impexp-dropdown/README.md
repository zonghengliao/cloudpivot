# P3：导入模板的枚举列加 Excel 下拉数据验证

任务卡：`t_1ffde110`（backend-developer）· 2026-11-05

---

## 1. 改动前的诚实边界（来自代码）

`backend/src/business/impexp/template.rs` 头注释原文：

> PRD 里的「枚举列写 Excel 下拉数据验证」框架写入器不支持（`rust_xlsxwriter` 的
> `data_validation` 未被 `nik-utils` 封装暴露），本卡以「字段说明 sheet 的『可选值』列 +
> 导入期枚举校验」替代。

本轮把这条边界真正补上：写入器暴露 `data_validation`，模板生成器按列类型挂验证。

## 2. 改了什么

| 文件 | 改动 |
|---|---|
| `backend/vendor/nik-utils/src/export.rs` | 新增 `XlsxValidation` / `XlsxValidationRule`（`List` 下拉、`IntRange`、`DecimalRange`、`DateRange`）+ 构建器 `.with_message()`；`XlsxSheet` 增加 `validations` 字段与 `with_validations()`；`to_xlsx_workbook` 逐条 `add_data_validation`。**不产生 XML 的空验证直接跳过**（空列表 / 无界范围）；下拉列表 >255 字符时返回 `Err`（Excel 硬上限，超出会被判「文件损坏」） |
| `backend/vendor/nik-utils/src/lib.rs`、`backend/vendor/nik/src/lib.rs` | 把 `XlsxValidation` / `XlsxValidationRule` / `MAX_LIST_CHARS` 从 `nik_utils` 提升到 `nik` 门面 + prelude（业务侧只 `use nik::prelude::*`） |
| `backend/src/business/impexp/engine.rs` | `WorkbookSheet` 从三元组元组改为结构体（`name / headers / rows / validations`），`write_workbook` 透传验证 |
| `backend/src/business/impexp/template.rs` | 「数据」sheet **第 3 行起**按列挂验证：`Enum` → 下拉（取 `EnumItem.label`，与「字段说明」sheet 的「可选值」列同一份数据）；`Int` / `Decimal` → 区间；`Date` → 1900-01-01 ~ 2100-12-31。验证区上限取定义的 `max_rows`（物料/主数据 20,000 行），并夹在 Excel 行上限内。头注释与「填写须知」第 5 条同步改写 |
| `backend/Cargo.toml` | 新增 `[dev-dependencies] zip`（用于单测回读 xlsx 断言 `dataValidation` XML）。**不是新 crate**：`rust_xlsxwriter` 已依赖 zip 8.6，Cargo.lock 只有一份 |

设计口径：

- **下拉只是防错，不是判据**。`ColumnKind::parse` 的逐格校验一行没改，用户删掉验证、整列粘文本，导入时照样报 `ENUM` / `RANGE`（带行号 + 列名）。
- **超过 Excel 255 字符上限的枚举列跳过下拉**，而不是让整张模板打不开；该列的可选值仍在「字段说明」sheet 里。
- **字典列不给下拉**：取值是运行时数据（`sys_dict_*`），生成模板时拿不到，硬编码一份必然与库不一致。
- 日期区间写成 `DATE(y,m,d)` 公式而不是 Excel 序列号，避开 1900/1904 两种日期系统的偏移。

## 3. 验收对照

| 验收 | 结果 | 证据 |
|---|---|---|
| 物料模板「成本法 / 分类 / 状态」三列在 Excel 里是下拉 | ✅ openpyxl（第三方解析器）回读 4 条数据验证：`E3:E20002` list=`原料,半成品,成品,服务`、`F3:F20002` list=`移动加权平均`、`H3:H20002` list=`启用,停用`；另有 `G3:G20002` decimal `>=0`（安全库存）。每列还带选中提示（promptTitle = 中文列名） | `import_roundtrip.txt` |
| 单测覆盖（不只是人工看） | ✅ 后端 7 条模板单测，其中 3 条针对下拉：① `material_template_puts_dropdowns_on_enum_columns`（解 xlsx 的 `xl/worksheets/sheet1.xml`，断言 `sqref` / `formula1` / `operator` / 「文本列无验证」/「验证不覆盖表头与示例行」/ 条数=4）；② `all_builtin_templates_still_generate_with_matching_validations`（7 个内置模板全生成 + 「有枚举/数值/日期列 ⇔ 有 dataValidation」双向断言）；③ `overlong_enum_list_is_skipped_instead_of_failing_the_template`。框架侧 11 条（含超长列表报错、非法日期报错、空验证不写节点） | `tests_impexp.txt`（152 passed）、`tests_nik_utils_export.txt`（11 passed） |
| 7 个模板全部仍能生成、能被导入 | ✅ 真机逐个下载 7 个可导入业务模板（会计科目/库位/物料/供应商/计量单位/仓库/字典项）全部 200 + 三 sheet；物料模板填 1 行真上传 → 预检 `total=1 valid=1 error=0` → 确认入库 `status=SUCCESS imported_rows=1` → 库里真有 `IMP-DV-233300 | 下拉验证行 | RAW | MOVING_AVG | 12.5 | 1` | `all7_templates.txt`、`import_roundtrip.txt`、`db_imported_rows.txt` |
| git commit | ✅ 见卡内汇报 | — |

补充：`cargo clippy --lib -- -D warnings` 零告警（`clippy_lib.txt`），`cargo fmt --check` 全仓零 diff。

## 4. 真机环境

```
二进制：/root/p1fix5c-target/debug/cloudpivot（本轮改动，2026-11-05 23:29 构建）
监听：127.0.0.1:18142   profile=test   Redis db=12
数据库：cloudpivot_dv1（pg_dump/restore 自 cloudpivot_imp7 的生产快照副本，28 个单位 / 8 个物料）
隔离说明：另有两个旧二进制占着 18140/18141，本轮不碰；生产 144 未部署
```

## 5. 复现

```bash
# 1) 网关（真后端）
cd /opt/cloudpivot/backend
NIK_PROFILE=test NIK_DATABASE_HOST=127.0.0.1 NIK_DATABASE_PORT=5432 \
NIK_DATABASE_USER=e2erole NIK_DATABASE_PASSWORD=*** NIK_DATABASE_NAME=cloudpivot_dv1 \
NIK_SERVER_LISTEN=127.0.0.1:18142 NIK_REDIS_URL=redis://127.0.0.1:6379/12 \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=*** /root/p1fix5c-target/debug/cloudpivot

# 2) 下载模板 + 回读验证 + 上传一行 + 确认入库
uv run --with openpyxl python verify_dropdown_import.py http://127.0.0.1:18142 /tmp/dv-out

# 3) 7 个模板逐个回读
uv run --with openpyxl python all7b.py
```

## 6. 未做 / 边界

- **生产 144 未部署**：本卡只到 99 的隔离库；发版后用户下载到的模板才会带下拉。
- Excel 实际渲染未截图（无 GUI/Excel）：以 OOXML `dataValidation` 节点 + openpyxl 回读为准——这两者就是 Excel 读的那份数据；`calamine` 不解析数据验证，故单测直接断言 XML。
- 字典列（`ColumnKind::Dict`）仍只有「字段说明」sheet 的字典编码提示，无下拉（见 §2 口径）。
