# CODE-09 端到端验证：6 个主数据编码自动生成 + 手工覆盖 + 参数开关

> 结论：**30 个矩阵用例全绿（30/30）**，额外验证 ①②③④ 全通过，**问题 0 条**，2 条「与卡面文字不符但实现正确」的口径说明见 §7。
> 验证对象：`origin/main` @ `6364972`（隔离 worktree，非共享工作区的 WIP 状态），二进制 md5 `1d9d9a8ef94ba051073e598144520a94`。
> 本卡**不改任何代码**（CODE-01~08 已交付），只做验收与留证；产物 = 本目录（脚本 5 个 + raw 证据 142 个文件，含 18 张 UI 截图）。

---

## 1. 环境与夹具（可复跑）

| 项 | 值 |
|---|---|
| 代码 | `/root/cp-code09-wt`（detached `6364972`，`git status` 干净；共享工作区有他卡 WIP，不能用来落证） |
| 二进制 | `/root/cp-code09-wt/bin/cloudpivot`，md5 见 `raw/00_binary_md5.txt`；`cargo build` 用 `CARGO_TARGET_DIR=/opt/cloudpivot/backend/target` 复用依赖（target 91G，不能重建） |
| 实例 | `profile=test` / `127.0.0.1:8049` / 库 `cloudpivot_code09`（全新空库，首启应用 **275** 个迁移） |
| 后端真库用例库 | `cloudpivot_code09t`（274+1 迁移，与证据库分离，避免污染证据） |
| 前端 | `vite dev :5179`，`BACKEND_ORIGIN=http://127.0.0.1:8049` |
| 账号 | admin（首登改密 `Admin@123456` → `Admin@123456x`，真实 token curl，无 mock） |
| 未触碰 | 144 生产 / 145 测试机 / 已应用迁移 / 计量单位代码 |

脚本（按顺序跑即可复现）：

```
scripts/00_start_instance.sh start     # 起隔离实例（:8049 / cloudpivot_code09）
scripts/00b_fixture.sh                 # ①种子占号实测 → ②证据库夹具复位（软删演示行 + biz_seq 归零）
scripts/01_curl.sh                     # 30 用例矩阵 + 额外 ①②③④（真实 token）
scripts/02_db_assert.sh                # 库侧 65 条断言
scripts/03_backend_tests.sh full       # 后端回归（真库，无 SKIP）
```

**为什么证据库要「软删种子 + 号段归零」**：空库首启会种入主数据演示行（`V0137` 有 `SUP-0001`、客户 `CUS-0001..0003`），
取号器口径是「撞号就顺延」，不清理的话首号必然是 `SUP-0002`，卡面「编码传空 → 自动生成 `PREFIX-0001`」这条**无法逐字实测**。
所以先在带种子的库上**实测顺延**（`raw/00b_fixture.txt` 的 A 段：`SUP-0002` / `CUS-0004`，PASS），
再对本证据库做一次性夹具复位（`deleted_at=now()` 软删演示行 + `biz_seq.current_no=0`），
唯一索引是 `WHERE deleted_at IS NULL` 的部分索引，软删后编码可复用 —— **不动迁移、不动代码**。

---

## 2. 30 个用例矩阵（6 主数据 × 5 路径）

参数 `erp.master_data.allow_manual_code = true`（迁移默认值）。原始请求/响应见 `raw/9<实体>_*.json`，逐条断言见 `raw/00_summary.txt`（**191 条 PASS / 0 FAIL**）。

| # | 供应商 SUP | 客户 CUS | 物料 MAT | 仓库 WH | 资产分类 AC | 费用类型 ET |
|---|---|---|---|---|---|---|
| **1** 编码传空串（参数允许手工） | `SUP-0001` | `CUS-0001` | `MAT-0001` | `WH-01` | `AC-01` | `ET-01` |
| **2** 连建 3 个（字段缺席=null） | `SUP-0002/0003/0004` | `CUS-0002/0003/0004` | `MAT-0002/0003/0004` | `WH-02/03/04` | `AC-02/03/04` | `ET-02/03/04` |
| **3** 手工 `GYS-001` 类自有码 | `GYS-001` 原样 | `KH-001` 原样 | `YB-001-02` 原样 | `CK-001` 原样 | `CAT-BUILDING` 原样 | `ADMIN-FEE` 原样 |
| **4** 手工指定已存在的编码 | 409 + `66001` | 409 + `66001` | 409 + `66001` | 409 + `66001` | 409 + `66001` | 409 + `66001` |
| **5** 参数改 false + 手工传码 | 400 + `66054` | 400 + `66054` | 400 + `66054` | 400 + `66054` | 400 + `66054` | 400 + `66054` |

补充断言（同一批）：

- 用例 1/2 的编码形态按拍板格式断言：`^SUP-[0-9]{4}$`、`^WH-[0-9]{2}$` …（4 位：SUP/CUS/MAT；2 位：WH/AC/ET），全部匹配；
- 用例 2 断言「连建三个严格 +1、四个自动码互不相同、`0001→0004` 无跳号」；
- 用例 3 断言响应与库内**原样保留**（不转大写、不补零）；
- 用例 4 文案实测（每个实体都点名编码，HTTP 是 **409** 不是 400，见 §7）：
  - 供应商：`供应商编码 \`GYS-001\` 已存在（已软删的编码可以复用）`
  - 客户：`客户编码 \`KH-001\` 已存在（…）`；物料/仓库/分类/费用类型同构
- 用例 5 断言 `code=66054`、文案含「系统已禁止手工指定编码」并点出参数键 `erp.master_data.allow_manual_code`、**响应无 data（一行都没落库，库侧另有断言）**；
- 开关关闭时「留空」仍自动取号（`SUP-0018`，200）—— 开关只管手填，不改取号口径；
- 参数改回 `true` 后手工码恢复（`KH-BACK-001` 200），参数行的 `config_type/category` 没被整体替换写坏。

---

## 3. 额外 ② 并发：10 个并发请求自动生成供应商编码

10 个 `POST /api/business/erp/supplier`（编码字段缺席）同时发出（`&` + `wait`）：

```
并发返回编码： SUP-0016 SUP-0008 SUP-0013 SUP-0009 SUP-0014 SUP-0015 SUP-0010 SUP-0007 SUP-0012 SUP-0011
PASS 并发 10 个请求全部 200 = 10
PASS 并发拿到 10 个编码 = 10
PASS 并发 10 个编码互不相同 = 10
PASS 并发无重号 = 0
```

库侧：`select count(*) … supplier_code ~ '^SUP-00(0[7-9]|1[0-6])$'` = **10**（`raw/02_db_assert.txt`）。
返回顺序乱序 = 号段推进是行锁串行的，但每个请求拿到的号不重复。

---

## 4. 额外 ① 撞号重试（实现口径 = 自动跳号，不报错）

用**手工码**把 `SUP-0005` 先占住（模拟老库/其他入口留下的编码），然后：

| 步骤 | 实测 |
|---|---|
| 手工建 `SUP-0005` | 200，原样落库 |
| `GET /supplier/next-code` 预览 | 200 → **`SUP-0006`**（跳过已占的 0005） |
| 编码留空建单（自动取号） | 200 → **`SUP-0006`**（自动跳号，不报 409 让用户重试） |

实现依据：`business::erp::seq::next_master_code_free`（撞号 `code_taken` → 继续 bump，最多 `FREE_CODE_MAX_ATTEMPTS` 次），
`seq.rs` 模块注释就点名了「V0137 种了 `SUP-0001`」这种历史编码场景。带种子的库上更直接：
首号顺延到 `SUP-0002` / `CUS-0004`（`raw/00b_fixture.txt` A 段，PASS）。

---

## 5. 额外 ③ 前端「生成编码」按钮：6 个表单行为一致

浏览器真点（`raw/00_ui_evidence.txt` 有逐项记录，截图见下）：

| 表单 | ① 点「生成编码」填入 | ② 不点生成直接保存（toast 原文） | ③ 编辑态 |
|---|---|---|---|
| 供应商 | `SUP-0019` (`50_ui_supplier_gen_code.png`) | 「供应商已创建 · 编码 SUP-0019」(`51`) | 编码框 `disabled=true` + 按钮置灰，提示「编码是供应商身份，不支持修改」(`52`) |
| 客户 | `CUS-0006` (`60`) | 「客户已创建 · 编码 CUS-0006」(`62`) | `disabled=true` (`61`) |
| 物料 | `MAT-0006` (`70`) | 「物料已创建 · 编码 MAT-0006」(`72`) | `disabled=true` (`71`) |
| 仓库 | `WH-06` (`80`) | 「仓库「CODE09 UI 仓库（不点生成）」已创建 · 编码 WH-06」(`82`) | `disabled=true` (`81`) |
| 资产分类 | `AC-06` (`90`) | 「分类已创建 · 编码 AC-06」(`91`) | `disabled=true` (`92`) |
| 费用类型 | `ET-06` (`95`) | 「已新建费用类型 · 编码 ET-06」(`96`) | `disabled=true` (`97`) |

- 6 个表单 placeholder 统一为「留空自动生成，如 `<PREFIX>-<示例>`」，辅助文案统一为「不点「生成编码」则保存时自动生成；生成后仍可手改（已有编码体系可直接填）」。
- **预览不占号在 UI 层同样成立**：供应商点生成拿到 `SUP-0019` 后取消，再建单不点生成，保存仍是 `SUP-0019`；客户/物料/仓库/分类/费用类型同理。
- 前端没有任何「编码必填」校验挡路（CODE-05/06 删掉的校验确实生效），不点生成的 6 次建单全部一次通过。

---

## 6. 额外 ④ 计量单位回归（本批不动）

| 断言 | 实测 |
|---|---|
| `GET /api/business/erp/unit/next-code` | **400 + `66006`**「id 不是合法的 UUID：next-code」→ 计量单位**没有**预览/自动取号路由（该 URL 命中 `/unit/{id}` 详情路由被 UUID 校验挡住） |
| `POST /unit` 编码传空 | 400 + `10001`「unit_code 不能为空」→ 仍必填、不会自动生成 |
| 参数 `allow_manual_code=false` 时手工填 `KG9` | **200**，编码原样保留 → 开关不作用于计量单位 |
| `biz_seq` 是否有 UNIT 号段 | 0 行（`raw/02_db_assert.txt`）→ 计量单位永远走手工码（用户口径：PCS/KG/M 是行业标准码） |

---

## 7. 与卡面文字不符、但实现正确的 2 条口径（不是缺陷）

1. **用例 4「手工指定已存在的编码 → 400」实际是 `409 + 66001`。**
   仓内 ERP 主数据的唯一约束冲突统一走 `409`（`shared/error_code.rs` 的 `ERP_CODE_CONFLICT = 66001`，与 `supplier/customer/material/warehouse` 等既有口径一致，CODE-08 交接也已点名）。
   断言按 `409` 落证，文案仍**明确点名编码并说「已存在」**，满足卡面「明确提示编码已存在」的意图。
2. **用例 2 的「0001/0002/0003」在本次实测里是「0002/0003/0004」。**
   用例 1 已经在同一实体上占掉 `0001`（这正是卡面用例 1 的要求），所以用例 2 的三个连续建单顺延为 `0002/0003/0004`；
   断言改成更严的形式：**四个自动码互不相同 + `0001→0004` 连续无跳号**，语义（递增不重号）覆盖且更强。

另有 1 条**实现细节值得记下**（不影响验收）：
`GET /unit/next-code` 不是 404 而是 `400+66006`，因为 `next-code` 被 `/business/erp/unit/{id}` 详情路由吃掉了；
外部若照 `material/next-code` 的规律去猜计量单位预览接口，会拿到「UUID 非法」而不是「接口不存在」—— 语义上能区分，但值得前端别依赖 404。

---

## 8. 库侧断言（`raw/02_db_assert.txt`，65 条全 PASS）

- 6 张主数据表「未删行数 == 去重编码数」→ **不重号**；
- 自动码形态：`^SUP-[0-9]{4}$ / ^CUS-… / ^MAT-… / ^WH-[0-9]{2}$ / ^AC-[0-9]{2}$ / ^ET-[0-9]{2}$` 不符合数为 **0**；
- 首号落库：`SUP-0001 / CUS-0001 / MAT-0001 / WH-01 / AC-01 / ET-01` 各 1 行；
- 手工码原样落库：`GYS-001 / KH-001 / YB-001-02 / CK-001 / CAT-BUILDING / ADMIN-FEE / KH-BACK-001`；
- 用例 4 的重复提交没多插一行（`GYS-001` 未删行仍 1 行）；用例 5 被拒的 6 个 `*-OFF-001` **0 行**；
- `biz_seq` 六条规则 `prefix/separator/pad_width` 全对（4/4/4/2/2/2，分隔符 `-`），且不参与日期（`date_pattern` 对主数据无效）；
- **预览不占号的库侧铁证**：CUS/MAT/WH/AC/ET 各有 5 个同前缀编码而 `current_no = 4` —— 第 5 个是「预览号当手工码」提交的，块只读不推进计数器；
  SUP 的 18 个编码 / `current_no=18` 恰好相等，是因为手工占位的 `SUP-0005`、预览号 `SUP-0017` 与取号器跳过的两个号一一抵消（`raw/02_db_assert.txt` §6 有逐条说明）；
- 全库只有 6 条主数据号段 `current_no > 0`，其余 18 条单据号段未被动过。

---

## 9. 回归测试

| 项 | 结果 |
|---|---|
| 后端 `business::erp::seq::`（编码机制本体） | 11 passed / 0 failed / 0 ignored（真库） |
| 后端 `business::erp::code_policy::`（开关） | 10 passed / 0 failed / 0 ignored |
| 后端 `business::erp::handler::tests`（供应商/物料/仓库建单） | 48 passed / 0 failed / 0 ignored |
| 后端 `business::erp::customer_handler::` | 13 passed / 0 failed / 0 ignored |
| 后端 `business::erp::asset_handler::` | 4 passed / 0 failed / 0 ignored |
| 后端 `business::erp::expense_handler::` | 4 passed / 0 failed / 0 ignored |
| 后端全量 `cargo test --lib`（归因用） | 见 `raw/03_backend_tests.txt` 末尾 `test result` 行；失败项逐条归因见 §9.1 |
| 前端单测 `code05/06/07/08`（编码按钮 + 开关行为） | **34 passed / 0 failed**（`raw/04_frontend_unit.txt`） |
| 前端 `pnpm typecheck` / `pnpm build` | 见 §9.2 |

### 9.1 全量 `cargo test --lib` 归因

```
test result: FAILED. 3503 passed; 23 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1347.28s
```

- **与本卡零关系**：23 条红全部落在 `asset_linkage_tests`(10) / `expense_linkage_tests`(6) /
  `voucher_linkage_tests`(3) / `flow::linkage`(3) / `receivable_aging`(1)，开关/取号/预览路径**零命中**
  （失败清单见 `raw/06_full_lib_failures.txt`，grep `master_code|code_policy|preview|next-code|66001|66054` = 0）；
- **根因是「一次性空库没有部门种子」**：panic 原文统一是
  `asset_linkage_tests.rs:195: 库里没有部门（先跑基础种子迁移）`。实测三套空库
  `cloudpivot_code09t / cloudpivot_code09 / cloudpivot_code08d` 的 `sys_dept` 都是 **0 行**
  （迁移里没有部门种子，部门由管理端建）—— 与 CODE-08 在同一 HEAD 上跑出的 `3503 passed / 23 failed`
  **条数完全一致**，属于基线既有红，不是本卡引入；
- 归因复跑（`raw/07_attribution_rerun.txt` / `raw/08_expense_linkage_detail.txt` / `raw/09_attribution_stage2.txt`）：
  往一次性库补 1 行 `sys_dept` 后重跑这 23 条 → **105 passed / 9 failed**（asset_linkage 10 + voucher_linkage 3 + receivable_aging 1 共 **14 条转绿**，
  反证「缺部门种子」这个根因）；剩下的 9 条同属夹具前置：6 条 `expense_linkage` 的 panic 原文是
  「库里必须有一名带部门的用户」→ 补了带部门的用户后升级为 `65018 职责分离`（唯一用户既是制单人又想当审批人，需要第二个非制单人用户），
  3 条 `flow::linkage` 是主线计数器漂移（`left: 17 / right: 16`，即 P3-08 新增 flow 触发点后计数器没同步，CODE-07 已记录同源红）。
  **这些都不是本卡代码范围（本卡零代码改动），也不属于编码机制路径。**

### 9.2 前端 typecheck / build

```
pnpm typecheck  → exit 0
pnpm build      → exit 0（vue-tsc --noEmit && vite build）
```

原始输出：`raw/05_frontend_build.txt`。

---

## 10. 证据清单

```
scripts/00_start_instance.sh   隔离实例（:8049 / cloudpivot_code09 / 275 迁移）
scripts/00b_fixture.sh         种子占号实测 + 证据库夹具复位
scripts/01_curl.sh             30 用例矩阵 + 额外 ①②③④（真实 token）
scripts/02_db_assert.sh        库侧 65 条断言
scripts/03_backend_tests.sh    后端回归（真库，无 SKIP）
raw/00_summary.txt             191 条 curl 断言（ALL PASS）
raw/00b_fixture.txt            种子占号顺延实测 + 夹具复位前后对比
raw/02_db_assert.txt           65 条库侧断言（ALL PASS）
raw/03_backend_tests.txt       后端用例逐条输出 + 全量归因
raw/06_full_lib_failures.txt   全量 --lib 的 23 条失败清单（与编码路径零命中）
raw/07_attribution_rerun.txt   补 sys_dept 后重跑 23 条红的归因
raw/08_expense_linkage_detail.txt  6 条 expense_linkage 的 panic 原文
raw/09_attribution_stage2.txt  补带部门用户后的第二阶段归因
raw/04_frontend_unit.txt       前端 code05/06/07/08 单测（34 passed）
raw/05_frontend_build.txt      前端 typecheck + build 原始输出（均 exit 0）
raw/00_binary_md5.txt          被测二进制 md5
raw/9<实体>_*.json             30 个用例的原始响应（请求体见脚本内 curl -d）
raw/50..97_ui_*.png            6 个表单 × 生成编码 / 不点保存 / 编辑只读
```

复跑注意（踩过的坑）：
1. `01_curl.sh` 假设**空表 + 号段归零**，重跑前先 `bash 00b_fixture.sh reset-only`；
2. 全新空库首启有 `must_change_password` 闸（业务接口 403），改密后会话缓存 60s（脚本内已 sleep 65）；
3. 落证脚本里 `Authorization: Bearer $TOKEN` 这种**拼接好的字面量会被工具链打码成 `***`**，必须拆成
   `AUTH_KEY / AUTH_SCHEME / AUTH_VAL` 再拼（否则所有请求 401，第一版就踩了）。
