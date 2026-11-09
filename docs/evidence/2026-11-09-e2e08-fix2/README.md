# E2E08-FIX-2 证据：六类主数据口径统一（编码大小写 / unit_code 422 / 重复文案 / next-code）

- 卡片：`t_fa322930`（E2E-08 演练 4 条 P4）
- 代码提交：`e53a4937`（主改动，13 文件）+ `81169ccb`（④ 文案去嵌套括号）+ `9094bafc` / `e54106d4`（注释对齐）
- 分支：`origin/main` = `e54106d4`（`git ls-remote origin main` 复核，见 §6）
- 验证机：145 演练机（`/opt/cloudpivot` 已 ff 到 `e54106d4`，debug 二进制 20:53 构建）
- 本地隔离库：99 的 `cloudpivot_e2e08fix2`（自建、迁移 295 条 + bootstrap 种 admin；跑完未删）

---

## 0. 一句话结论

| 卡面问题 | 修法 | 实测（145 真 token） |
|---|---|---|
| ① 手工编码大小写：SUP/CUS/MAT/WH 原样，AC/ET 归一为大写 → 同一张规则表两种行为 | **方案 A：统一「原样保留」**。后端去掉 AC/ET 的 `to_ascii_uppercase`（`asset_dto` / `expense_dto`）；前端 `shared/master-code.ts` 不再 `toUpperCase()`，字符集放开小写 | 六类**小写码**落库仍是小写；六类**大写码**仍是大写（逐类贴 HTTP + 响应 + 库内值，12 组断言全绿） |
| ② `POST /business/erp/unit` 只带 `unit_name` → 框架 **422**（拿不到人话） | `CreateUnitReq.unit_code` 改 `Option<String>` + `unit_code_required()`：缺失 / 空串 / 纯空白一律 **400 + 10001**「计量单位编码必填（单位不自动取号）」 | 三种「没填」形态全部 **400 / 10001**，`≠ 422`，文案逐字一致 |
| ③ 重复单位编码文案是裸 DB 口径（`编码已存在（唯一约束 uk_biz_unit_code）…`） | 建单前**预检** + 六类共用的 `code_conflict(kind, code)` 单一文案出口；写路径 `unit_write_error` 兜并发窗口 | **409 / 66001**「计量单位编码 \`e2e08f2-unit-dup\` 已存在（已软删的编码可以复用）」；**不含** `uk_biz_unit_code`；被拒行未落库 |
| ④ `GET /business/erp/unit/next-code` 被 `/unit/{id}` 吃掉 → 66006「id 不是 UUID」 | **方案 A：显式注册静态路由**返回 400「计量单位不提供自动取号」 | **400 / 10001**（`≠ 66006`），文案含结论 + 下一步；对照组另外五类 `next-code` 全部 200 |

---

## 1. ① 为什么选「原样保留」（方案 A），落地在哪

**取舍**：企业自有编码有的系统区分大小写（`Ab-01` 与 `AB-01` 是两个对象）。平台替用户改大小写 =
替用户改编码。方案 B（统一大写）还要迁移存量，破坏性更大。

**落地（四处，前后端同一口径，缺一处就还是两种行为）**：

| 位置 | 改动 |
|---|---|
| `backend/src/business/erp/asset_dto.rs` | `validate_category` 去掉 `.map(|text| text.to_ascii_uppercase())`（只留 `seq::resolve_manual_code` 的 trim） |
| `backend/src/business/erp/expense_dto.rs` | `validate_type` 同上 |
| `backend/src/business/erp/seq.rs` | 文件头写死不变式：「六类统一原样保留，`resolve_manual_code` 只做 trim」 |
| `frontend/.../shared/master-code.ts` | `toMasterCodeInput` / `masterCodeError` 去掉 `toUpperCase()`；`MASTER_CODE_PATTERN` 由 `^[A-Z0-9._-]$` 放开为 `^[A-Za-z0-9._-]$`；提示文案改为「大小写按填写原样保存」 |

> 前端必须同改：原先只有资产分类 / 费用类型两个表单调 `toMasterCodeInput`（内含 `toUpperCase`），
> 另四类原样提交。只改后端的话，界面仍会把小写改大写再发出去 —— 行为看着「一致」了但用户输入被改。

**存量数据不受影响**：库里的 `ELEC` / `FURNITURE` / `TRAVEL` 等本来就是大写，原样保留对它们无操作。
本次**没有任何迁移**（不需要 UPDATE 存量，也不需要改唯一索引）。

**已知取舍（诚实记录）**：`uk_biz_*_code` 是「未删除内唯一」且**区分大小写**，所以理论上可以同时存在
`elec` 与 `ELEC` 两个看起来相同的分类。这是 SUP/CUS/MAT/WH 四类的**既有**行为（本卡把它们与 AC/ET 拉齐），
不是本卡引入的新风险。要做「大小写不敏感唯一」得改索引（`lower(code)` 唯一索引 + 迁移 + 存量查重），
超出 4 条 P4 的范围，未做；需要时另开卡。

## 2. ② `unit_code` 的 422 → 400

```rust
// dto.rs
pub const UNIT_CODE_REQUIRED: &str = "计量单位编码必填（单位不自动取号）";
pub struct CreateUnitReq {
    #[serde(default)]                    // ← 缺席不再触发框架「必填字段缺失」的 422
    pub unit_code: Option<String>,
}
impl CreateUnitReq {
    pub fn unit_code_required(&self) -> Result<&str, ApiError> { /* trim + 空判 → 400/10001 */ }
}
```

- 编码对单位**仍然是必填**（单位不自动取号）：`PCS` / `KG` / `M` 是行业标准码，替用户编 `U-01` 没人看得懂。
- 三种「没填」（`null` / `""` / `"   "`）走同一条 400，文案与 `validate()` 共用同一个常量，不会走岔。
- handler 里先用 `unit_code_required()` 拿归一化值，INSERT 用的就是同一个值（不会出现「校验的和写库的不是一个」）。

## 3. ③ 六类共用的 `code_conflict(kind, code)`

`handler.rs` 新增唯一文案出口：

```rust
pub(crate) fn code_conflict(kind: &str, code: &str) -> ApiError {   // 409 + 66001
    format!("{kind}编码 `{code}` 已存在（已软删的编码可以复用）")
}
```

供应商 / 物料 / 仓库 / 客户 / 资产分类 / 费用类型 / 计量单位 **七处**同名 helper 全部改成委托它
（`warehouse_code_conflict` / `material_code_conflict` / `supplier_code_conflict` /
`customer_handler::code_conflict` / `category_code_conflict` / `type_code_conflict` / `unit_code_conflict`），
文案因此**逐字相同**、差别只在 `kind`。以后要改口径只改一处。

计量单位侧是**双保险**：

1. `unit_code_taken()` 建单前预检 → 命中直接 409（不把裸约束名抛给用户）；
2. `unit_write_error()` 把 INSERT 的 `UniqueViolation` 翻成同一句 409（预检与 INSERT 之间的并发窗口）。

## 4. ④ 显式路由（静态优先于参数段）

```rust
#[nik::get("/business/erp/unit/next-code")]
pub async fn unit_next_code(..) -> ResResult<serde_json::Value> { Err(unit_code_not_auto()) }
```

- axum（matchit）匹配时**静态段优先于参数段**，所以 `/unit/next-code` 命中这条、`/unit/{id}` 不参与；
  实测 `400 + 10001`，不再是 `66006「id 不是 UUID」`。
- 权限码复用既有 `business:erp:unit:create`（`next-code` 属于「建单前取号」，与其余五类一致），
  **没有新增权限码 / 菜单迁移**。
- 对照组：供应商 / 客户 / 物料 / 仓库 / 资产分类 / 费用类型 六个 `next-code` 实测仍 200 ——
  单位是**唯一的例外**，不是整组路由坏了。

## 5. 测试

| 套件 | 结果 | 备注 |
|---|---|---|
| `cargo test --lib`（无库全量） | **3982 passed / 0 failed** | 真库用例按既有约定静默 SKIP（`full_lib_no_db.txt`） |
| `business::erp::handler::tests`（真库） | **51 passed** | 含新增 ②③④ 三条：`unit_create_without_code_is_400_with_human_message` / `unit_duplicate_code_conflicts`（断言文案逐字）/ `unit_next_code_is_explicit_400` |
| `business::erp::dto` | **18 passed** | 含 ② 的 DTO 层守卫（`validate_rejects_blank_code` 增补缺席分支） |
| `business::erp::seq` | **11 passed** | 归一化入口不变式 |
| `business::erp::asset` | **61 passed** | 分类手工码小写原样保留 |
| `business::erp::expense` | **57 passed** | 费用类型手工码小写原样保留 |
| `business::erp::customer` | **19 passed** | 委托共用 `code_conflict` 后文案不变 |
| 前端 | `pnpm typecheck` 通过；`pnpm build` 通过 | dist 产物正常 |

真库细节（`db_suites.txt`）：隔离库 99/`cloudpivot_e2e08fix2`，自建后由二进制完整启动一次
（迁移 295 条 + bootstrap 种 admin）。**踩坑记录**：只跑迁移不跑 bootstrap 时，审批类用例会报
`65018 本节点没有有效审批人`（admin 未种上）—— 那不是代码红，是库没起全。

## 6. 145 真 token 实测（`curl_145_pass.txt`，PASS=69 FAIL=0）

跑法：145 上 `git merge --ff-only origin/main`（→ `e54106d4`）→ `cargo build`（debug，20:53）→
**另起 8036 实例**（不重启 8033 的 dev 服务；dev 库，跑完 kill）→
`BASE=http://127.0.0.1:8036/api bash scripts/verify/e2e08fix2_master_consistency.sh`。
（`curl_145_pass.txt` 就是**最终 HEAD `e54106d4` 上复跑**的结果；8033 / 8034 两个 dev 服务全程未动，health 仍 200。）

关键原文（`raw_*.json` 逐条留档）：

```
POST /business/erp/unit  {"unit_name":"..."}                 → 400 {"code":10001,"msg":"计量单位编码必填（单位不自动取号）"}
POST /business/erp/unit  {"unit_code":"e2e08f2-unit-dup",..}  → 409 {"code":66001,"msg":"计量单位编码 `e2e08f2-unit-dup` 已存在（已软删的编码可以复用）"}
GET  /business/erp/unit/next-code                             → 400 {"code":10001,"msg":"计量单位不提供自动取号：新建时请手工填写 unit_code（`PCS` / `KG` / `M` 这类行业标准码，系统不会替你编）。有 next-code 端点的是供应商 / 客户 / 物料 / 仓库 / 资产分类 / 费用类型 六类"}
GET  /business/erp/supplier/next-code                         → 200 {"code":0,...}  （对照组）
```

① 的 12 组断言（六类 × {小写, 大写}）全部核对三处：HTTP 200、响应体里的编码、`select` 出来的库内值 ——
**提交什么存什么**，平台不改大小写。

清场：只按前缀 `e2e08f2-%` 删本卡夹具（7 张表），残留实测 **0**；没有 DROP / TRUNCATE / 删他人数据。

## 7. 复现

```bash
# 99（真库单测）：隔离库自建 + 完整启动一次（迁移 + bootstrap）后
ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_e2e08fix2 \
  cargo test --lib business::erp::handler::tests -- --test-threads=1

# 145（真 token）
BASE=http://127.0.0.1:8036/api bash scripts/verify/e2e08fix2_master_consistency.sh
```

## 8. 未做 / 边界

- **没有新增或修改任何迁移**（4 条都是入参 / 文案 / 路由层）。
- 未把「大小写不同视为不同编码」改成大小写不敏感（要改唯一索引 + 存量查重，另开卡）。
- 其余五类的 `Update*Req` 仍「没有 *_code 字段」（静默忽略）vs 客户/分类显式 400 —— 这一条
  在父卡 `t_fcb017a7` 已记录，属于跨六类契约收敛，不在本卡 4 条 P4 内。
- 单位 `next-code` 返回 **400** 是刻意的（不是 200 + `null`）：选择「显式拒绝」而非「空响应」，
  避免对接方把 `null` 当「暂时没号」而反复重试。
