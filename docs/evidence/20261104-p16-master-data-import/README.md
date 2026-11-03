# P1-6 主数据 Excel 导入（6 个定义）—— 验收证据

> 卡面：`t_4b873345`【P1-6】主数据 Excel 导入（供应商/物料/单位/科目/仓库/库位 6 个定义）
> 依据：`docs/architecture/ARCH-3-模块勾稽关系审计.md` §6 R-P1-4（断链 DC-18）
> 交付：`backend/src/business/impexp/builtin/master_data/`（6 个 `ImportDefinition` + 真库用例）
> 环境：隔离实例（空库 `cloudpivot_p16md_e2e` + 迁移种子），后端 `127.0.0.1:18095` / 前端 `127.0.0.1:5185`，
> 后端二进制 = 当前源码 `cargo build`（debug），**未碰 prod**。

## 1. 做了什么

照 `builtin/dict_item.rs` 的范本加 6 个导入定义，**不改框架**（只在框架上补了两处通用能力，见 §1.2）：

| 业务编码 | 定义 | 列顺序（`*` = 必填） |
|---|---|---|
| `business.erp.supplier` | 供应商 | 供应商编码* / 供应商名称* / 简称 / 联系人 / 联系电话 / 纳税人识别号 / 信用额度 / 状态 |
| `business.erp.material` | 物料 | 物料编码* / 物料名称* / 规格型号 / 基本单位* / 分类 / 成本法 / 安全库存 / 状态 |
| `business.erp.unit` | 计量单位 | 单位编码* / 单位名称* / 是否基本单位 / 基准单位编码 / 换算率 / 状态 |
| `business.erp.account` | 会计科目 | 科目编码* / 科目名称* / 科目类别* / 借贷方向* / 是否末级 / 上级科目编码 / 状态 |
| `business.erp.warehouse` | 仓库 | 仓库编码* / 仓库名称* / 仓库类型 / 地址 / 负责人 / 是否允许负库存 / 状态 |
| `business.erp.location` | 库位 | 库位编码* / 所属仓库编码* / 库位名称 / 库位类型 / 是否默认 / 容量 / 状态 |

每列都声明了 `required` / `ColumnKind`（长度 / 范围 / 枚举）/ `unique_key` / `FkSpec` / 示例值 / 说明，
模板「字段说明」sheet 由框架自动生成（见 `template-*.xlsx`）。

### 1.1 五条硬要求的落点

| 要求 | 实现位置 | 证据 |
|---|---|---|
| 必填列 + 可空列 + 列顺序 | 每个定义文件的 `static COLUMNS` | 单测 `column_contract_is_stable`（6 个定义各 1 条）+ `template-*.xlsx` 的「字段说明」sheet |
| 行级校验（编码唯一 / 外键存在 / 换算率 > 0） | 框架 `unique_key` + `FkSpec` + `Decimal{min}`；跨列规则在 `validate_row` | 缺编码 → 第 4 行 `REQUIRED`；负信用额度 → 第 5 行 `RANGE`；单位不存在 → `FK_NOT_FOUND`；换算率 0 → `RULE` |
| 错误带行号 + 具体原因 | `RowIssue`（行号 = Excel 真实行号） | 截图 `05-supplier-error-rows.png`：`第 4 行 / 供应商编码 / REQUIRED / 必填项为空` |
| 跳过错误行 / 任一行错整体回滚 | 框架 `PARTIAL` / `STRICT`；6 个定义 `allow_partial() = true` 且 `batch_size == max_rows`（整份文件一个事务） | 单测 `supplier_primary_path...`（STRICT）+ 页面「导入模式」单选 |
| 幂等 upsert（可配置跳过） | `ON CONFLICT ... DO UPDATE` / `DO NOTHING`（按 `duplicate_strategy`） | 重导同一文件供应商仍 3 行（截图 06 + `db-evidence.txt`）；`SKIP` 策略单测 |

### 1.2 框架侧的两处小改（都不是重写）

1. `definition.rs` 新增 `DuplicateStrategy { Upsert, Skip }`（值类型）；
   `dto::UploadQuery` 增 `duplicate` 参数；`handler::import_upload` 把它快照进
   `sys_import_task.options.duplicate_strategy`，`handler::import_retry` 继承该策略；
   `model::import_task_options(task_id)` 供业务定义读回**本任务**的策略。
2. 前端导入向导加「重复数据」单选（按编码更新 / 已存在则跳过），并把「错误码」列补进失败明细表
   —— 顺带修掉一个既有缺陷：预检报告的 `RowIssue` 序列化字段是 `kind` / `message`，
   而页面按 `error_code` / `error_message` 取值，**失败原因列一直是空的**。

### 1.3 三处「框架覆盖不到」的处理（写在模块头注释里）

- **基准单位 / 上级科目**：同文件内前后引用都要能解析（新客户一份文件里 30 个基础单位 + 10 个辅助单位），
  所以这两列不挂 `FkSpec`，在 `persist` 里一次建「库里 + 本批次」的编码映射再解析；解析不到/成环
  都返回**带行号**的错误，绝不静默丢行。
- **库位所属仓库必须是叶子仓库**：`validate_row` 逐行查一次 `biz_warehouse.is_leaf`
  （DB 无法跨表约束），非叶子当场报「所属仓库「X」还有下级仓库」。
- **同仓只有一个默认库位**：同一份文件里两行都标默认 → 前置报错并给出两行行号；
  只标一行 → 先清同仓其它默认位再落库（与页面新建库位同口径）。

## 2. 单测（真库）

```
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_p16md \
  cargo test --lib master_data -- --test-threads=1
→ 38 passed / 0 failed（7.36s）
ERP_DB_URL=... cargo test --lib impexp -- --test-threads=1 --nocapture
→ 127 passed / 0 failed（7.94s，含框架既有 validate/engine/registry 用例）
```

`unit-tests-summary.txt` 是全组运行的尾部；`P16-6 实测` 行是 100 行物料的上限断言实测：

```
P16-6 实测：100 行物料 预检+入库 耗时 137.003339ms（预检 48 ms）
```

覆盖矩阵（每条定义「1 成功 + 1 校验失败」）：

| 定义 | 成功用例 | 校验失败用例 |
|---|---|---|
| 供应商 | `supplier_import_is_idempotent_and_reports_row_numbers` | 同用例内：缺编码 → 第 3 行 REQUIRED；负额度 → 第 4 行 RANGE |
| 物料 | `material_import_100_rows_is_fast_and_checks_unit_fk` | 同用例内：单位不存在 → FK_NOT_FOUND；分类「包材」→ ENUM |
| 计量单位 | `unit_import_resolves_base_unit_inside_the_same_file` | `unit_rule_errors_surface_in_the_precheck_report`（缺换算率 / 基础单位带基准） |
| 会计科目 | `account_import_derives_level_and_revives_soft_deleted_rows` | 同用例内：上级科目是自身 → RULE；另有 `missing_parent` / `cycle_is_detected` |
| 仓库 | `warehouse_import_writes_materialized_path` | 同用例内：仓库类型「虚拟仓」→ ENUM |
| 库位 | `location_import_requires_leaf_warehouse_and_single_default` | 同用例内：非叶子仓库 → RULE；同文件两个默认位 → persist 报错带两行行号 |

> 真库用例必须在**独立进程 / 独立库**跑。同一进程里若别的模块先把 `db()` 的 `OnceLock` 初始化到
> 另一个库，本文件的 `ensure_db` 会比对 `current_database()` 并打印 SKIP（不是静默假绿）。

## 3. 真机（隔离实例，页面 + 接口）

脚本：`scripts/verify/p16_import_page_check.mjs`（页面 42 项断言）+ `p16_other_definitions_check.mjs`
（另外 4 个定义 10 项断言），填数脚本 `scripts/verify/p16_fill_template.py`（基于**下载下来的真实模板**填行）。

```
==== P1-6 验收：43 passed / 0 failed ====
==== 其余 4 个定义：10 passed / 0 failed ====
```

| 截图 | 内容 |
|---|---|
| `01-definitions-dropdown.png` | 导入业务下拉：`会计科目 / 库位 / 物料 / 供应商 / 计量单位 / 仓库 / 字典项` —— 6 个主数据定义都在 |
| `02-supplier-precheck-pass.png` | 下载模板填 3 行 → 上传：总行数 3 / 通过 3 / 失败 0 |
| `03-supplier-import-done.png` | 确认入库：「共 1 批，入库 3 行（校验失败 0 行已跳过）」 |
| `04-supplier-list.png` | 供应商列表能看到 P16MD-SUP1/2/3 |
| `05-supplier-error-rows.png` | 故意缺编码 + 负额度：`第 4 行 / 供应商编码 / REQUIRED / 必填项为空`、`第 5 行 / 信用额度 / -100 / RANGE / 数值超出范围…` |
| `06-supplier-reimport-idempotent.png` | 同一文件再导一次并确认入库后，供应商仍是 3 行（幂等） |
| `07-material-list.png` | 100 行物料（API 全链路）入库后物料列表可见 `P16MD-MAT001` |

### 3.1 100 行物料耗时（实测）

```
100 行物料导入计时（2026-11-03T23:42:30.383Z）
预检 elapsed_ms = 8
上传→预检→入库 confirm 全链路 = 47 ms
外键校验 SQL 条数 = 1
```

（单测口径 137 ms 含 debug 构建下直接 `persist` 的调用；两者都远低于 5 秒门槛。）

### 3.2 库侧取证（`db-evidence.txt`）

- 供应商：3 行（导入 3 次后仍是 3 行 → 幂等）；
- 物料：100 行，`costing_method` 全为 `MOVING_AVG`（与 V0201 值域收敛一致）；
- 计量单位：`P16MD-U-BOX2`（is_base=0，base=`P16MD-U-PCS2`，rate=12）+
  `P16MD-U-PCS2`（is_base=1，rate=1）—— 辅助单位排在前面也能解析同文件的基准单位；
- 会计科目：`P16MD-A`（level 1，非末级，无父）+ `P16MD-A01`（level 2，末级，有父）；
- 仓库：`warehouse_path = /<id>/`、`level=1`、`is_leaf=1`；
- 库位：`P16MD-LC1` 默认位=1、`P16MD-LC2` 默认位=0（同仓唯一）；
- 导入任务：6 个 `biz_type` 全 SUCCESS，`options.duplicate_strategy` 落库可查。

## 4. 复现命令

```bash
# 1) 空库 + 迁移种子（只在本机隔离段）
E2E_ISO_SRC_MODE=none E2E_ISO_FRESH=1 E2E_ISO_DB=cloudpivot_p16md_e2e \
E2E_ISO_BACKEND_PORT=18095 E2E_ISO_FRONTEND_PORT=5185 \
E2E_ISO_BACKEND_BIN=/opt/cloudpivot/backend/target/debug/cloudpivot \
NIK_ADMIN_PASSWORD='Admin@123456' bash scripts/verify/e2e_isolated_instance.sh up

# 2) 页面验收（首登强制改密由脚本自己处理）
cd frontend && E2E_BASE_URL=http://127.0.0.1:5185 node ../scripts/verify/p16_import_page_check.mjs
cd frontend && E2E_BASE_URL=http://127.0.0.1:5185 node ../scripts/verify/p16_other_definitions_check.mjs

# 3) 真库单测
ERP_DB_URL=postgres://alun:alun@127.0.0.1:5432/cloudpivot_p16md NIK_PROFILE=test \
NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  (cd backend && cargo test --lib master_data -- --test-threads=1)
```

## 5. 没做的事（卡面明确划出的）

- 不重写 impexp 框架（只补 `DuplicateStrategy` 值类型 + 一个 options 读取函数）；
- 不做导入历史 / 回滚；
- 主数据**导出**没注册进 impexp（ERP 主数据导出走 `/business/erp/*/export`，不在本卡范围）。
