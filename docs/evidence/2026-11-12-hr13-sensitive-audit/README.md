# HR-13 证据：敏感字段明文接口 + `audit_logs` 留痕 + `sys_field_mask` 接线验证

卡片：`HR-13【HR】敏感字段明文接口 + audit_logs 留痕 + sys_field_mask 接线验证`（批 2 员工档案）
设计：`docs/architecture/ARCH-9-人力资源模块设计.md` §3.2 ⑥ / §5.2 #7 / D-14 / §8.1 HR-13 / §8.3 **R1**

## 一句话结论

`GET /business/hr/employee/{id}/sensitive?field=id_card|bank_account` 已落地（权限码
`business:hr:employee:sensitive`），**每调一次同步写一条 `audit_logs`**（`谁 / 什么时候 / 看了谁的什么字段`
四维度齐全，写不进去就**不返回明文**）；设计 §8.3 **R1 的疑问已实测排除**：把库里
`sys_field_mask.mask_param` 从 `6,4` 改成 `3,2` 后，同一 handler 路径的打码值立刻从
`110101****2584` 变成 `110****84`，改回来又变回去 —— 脱敏规则确实来自库表，不是写死在代码里。

| 项 | 结果 | 证据 |
|---|---|---|
| 明文接口 + 权限码 | 超管 200、无码账号 403、未登录 401、软删后 404 | `02_http_smoke.out` §2/§4/§8/§9 |
| 审计留痕（四维度） | `user_id` + `username` / `created_at` / `target_type` + `target_id` / `operation` + `detail.field` 全部落库，审计页可查 | `01_handler_tests_real_db.out`、`02_http_smoke.out` §6 |
| `sys_field_mask` 接线（R1） | 改 `mask_param` → 打码值跟着变；恢复后回到基线 | `01_handler_tests_real_db.out` `[HR-13] 打码值 before/改 3,2/恢复` |
| 列表 / 详情不放宽 | 明文零出现（HTTP 响应体逐字节断言 + DTO 序列化断言） | `02_http_smoke.out` §3、`01_...out` |
| 单测 | 新增 **21 条**（16 纯逻辑 + 5 真库），`business::hr` 全 **99 passed / 0 failed** | `01_handler_tests_real_db.out` |
| 门禁 | fmt / clippy `-D warnings` / 全量 lib **4912 passed** / 前端 typecheck+build | `03_gates.out` |
| 残留 | 端口无监听、演练库已删、`mask_param` 已复原 | `04_residue.out` |

## 交付了什么

### 新文件

| 文件 | 行数 | 说明 |
|---|---:|---|
| `backend/src/business/hr/employee_sensitive.rs` | 888 | 明文接口（1 条路由）、`SensitiveField` 枚举、`ReaderContext`、同步审计写入、`clamp_chars`/`audit_request_id`，16 条纯单测 |
| `backend/src/business/hr/employee_sensitive_db_tests.rs` | 643 | 5 条真库用例：审计七列 / 越权不写痕 / 空字段仍留痕 / **规则改参数驱动打码值** / 规则类型守卫 |
| `scripts/verify/hr/employee_sensitive_http_smoke.sh` | 264 | HTTP 冒烟 49 条断言（路由真的注册 / 权限码真的接 / 审计页真的查得到 / 明文不出列表详情） |
| `docs/evidence/2026-11-12-hr13-sensitive-audit/` | — | 本目录 |

### 改动的既有文件（2 处，合计 +10 −1）

| 文件 | 改动 | 为什么动它 |
|---|---|---|
| `business/hr/mod.rs` | `+ pub mod employee_sensitive` / `+ #[cfg(test)] mod employee_sensitive_db_tests` | 挂模块 |
| `business/hr/employee_handler.rs` | `ensure_visible` 私有 → `pub(crate)`（附一行理由注释） | 明文接口必须走**同一份**数据范围探针；单条入口自己再写一份范围判断就是多一个越权缺口 |

> **无新增迁移**：`audit_logs`（`V0014`）、`sys_field_mask` 两条规则（`V0388`）、
> 权限码 `business:hr:employee:sensitive`（`V0387` 第 220 行）**均已应用且不可改**，本卡纯代码。

## 接口

| 项 | 值 |
|---|---|
| 方法 / 路径 | `GET /business/hr/employee/{id}/sensitive?field=id_card\|bank_account`（不含 `/api` 前缀） |
| 权限码 | `business:hr:employee:sensitive`（`#[nik::permission]` + `V0387` 种子，单测对拍） |
| 出参 | `{ employee_id, emp_no, name, field, value, masked_value, audit_id, audited_at }` |
| 404 | 档案不存在 / 已软删 / **数据范围之外**（403 会泄露 id 存在性，同 HR-12 口径） |
| 400 | `field` 非法（报文列出 `id_card` / `bank_account`）、`id` 不是 UUID、缺 `field` |
| `value: null` | 该字段未登记（不是错误；前端 `plainValueOf` 会回落到打码值，页面显示「未登记」） |

`field` + `value` 的形态是 HR-18 的 `plainValueOf` **形态 ②** —— 前端
`fetchEmployeeSensitive` 早在 HR-18 就按契约写好，**本卡零前端改动**（`pnpm typecheck/build` 仍全绿）。

## 审计口径（本卡验收项）

一条明文访问 = `audit_logs` 一行：

| 维度 | 列 | 实测值（`02_http_smoke.out` §6） |
|---|---|---|
| 谁 | `user_id` / `username` | `01a1cc4e-fedc-…` / `admin` |
| 什么时候 | `created_at`（**数据库时钟**，`RETURNING` 读回并回给前端） | `2026-11-12T04:31:57.55312+08:00` |
| 看了谁 | `target_type` / `target_id` | `Employee` / 夹具员工 UUID |
| 看了什么字段 | `operation` / `detail.field` | `HR_EMPLOYEE_SENSITIVE_READ` / `id_card`、`bank_account` |
| 补充 | `detail` 另含 `emp_no` / `employee_name` / `dept_id` / `request_id` / `found` / `ip` | 员工被软删后仍能回答「看的是谁」 |
| 从哪来 | `ip` / `user_agent` / `request_id` | `ip` 为 NULL（框架 `client_ip` 在无可信转发头时按内网保护语义返回空，`detail.ip` 如实记 null）/ `curl/8.18.0` |

**审计 `detail` 绝不含明文**（否则「查审计」就成了绕开 `sensitive` 权限码的后门）：单测 + 真库 + HTTP 三层都断言了。

## 关键取舍（设计没写死、必须自己拍的地方）

1. **同步写、fail-closed**：`support::log::record_audit` 是非阻塞总线（适合「建了单要留痕」）；
   明文不同 —— 留痕是**唯一**证据。所以本卡用 `INSERT … RETURNING id, created_at` **同步**写，
   **写失败 → 500 且不返回明文**（宁可让用户重试，也不产生无痕访问）。这条也是
   `masking` 的 BUG-D-13（`request_id` 超 `VARCHAR(36)` ⇒ 每次留痕静默失败）的直接对策。
2. **`request_id` 按列宽截断**（`clamp_chars`，按**字符**不按字节）：`x-request-id` 是客户端可控头，
   原样塞进 36 字符的列就是 BUG-D-13 的翻版；非 ASCII 头在 `HeaderValue::to_str()` 就取不到 →
   回退 UUID v7。两条都有单测。
3. **不复用 `masking::registry::audit_plain_read`**：那条的语义是「某人持 `system:field-mask:plain`
   在某处看到明文」（`operation=FIELD_MASK_PLAIN_READ`、`target_id` 是**他自己**、没有字段维度）。
   本接口要回答「谁看了**哪个员工的哪个字段**」，三个维度里它缺两个 —— 另立 `operation`，
   两张口径互不覆盖（既持明版权限又调本接口时两条都落）。
4. **越权（404）不写审计**：否则审计页会被暴力试探的 404 淹掉，真实查看反而被埋；
   越权尝试由 `operation_logs`（请求流水）负责。**查空字段（`value=null`）仍留痕**，
   `detail.found=false` —— 「查过」本身是审计事实。
5. **`masked_value` 与详情页同一批函数**（`employee_dto::mask_id_card` / `mask_bank_account`）：
   界面「打码 → 明文」并排显示时不会两个值打架。
6. **数据范围走 `employee_handler::ensure_visible`**（HR-12 的同一份 `scope::filter_for` 探针）：
   不新写一份范围判断。

## `sys_field_mask` 接线验证（设计 §8.3 R1 的实测）

R1 原文：「`sys_field_mask` 是否对 `biz_employee` 自动生效**未实测**……若未生效，退化为 DTO 层手工调 `masking` 函数」。

**实测结论：生效，不需要退化方案。** 证据链三层：

1. **列名对齐**（纯单测 `field_columns_match_dto_and_v0388_seed`）：
   `SensitiveField::column()` == `employee_dto::COL_*` == `V0388` 里种的两条规则列名，三方一致；
2. **规则驱动**（真库 `mask_rule_change_drives_masked_value`）：
   ```
   [HR-13] 库里 biz_employee.id_card 的 mask_param = "6,4"
   [HR-13] 打码值 before=110101****2584 / 改 3,2 后=110****84 / 恢复后=110101****2584
   ```
   改库 → 同一条 `detail_of` 出参立刻跟着变 → 规则来自表；**断言前先恢复原值**（断言 panic 也不会把规则留在改坏状态）；
3. **HTTP 全链**（`02_http_smoke.out` §3）：列表 / 详情的 `id_card_masked` = `110101****9876`（`6,4` 口径），
   响应体里明文身份证 / 卡号逐字节不存在。

## 怎么复现

```bash
# 1) 真库用例（空库首启一次即完成迁移；本卡用的是 hr13_drill，跑完已删）
su - postgres -c "psql -c 'CREATE DATABASE hr13_drill OWNER alun'"
cd backend && ./target/debug/cloudpivot          # 起一次让 V0001~V039x 跑完（端口 18097/库 hr13_drill）
HR_DB_URL='postgres://alun:***@127.0.0.1:5432/hr13_drill' \
  cargo test --lib business::hr -- --test-threads=1 --nocapture

# 2) HTTP 冒烟（同一个隔离实例；三把密钥 + NIK_ADMIN_PASSWORD 见卡内说明）
BASE_URL=http://127.0.0.1:18097 ADMIN_PASSWORD='<超管口令>' \
  bash scripts/verify/hr/employee_sensitive_http_smoke.sh
```

> 冒烟脚本自带「空库首启必改密」处理。**注意**：改密会自增 `token_version`，
> 带着旧令牌再登录会被 `session_guard` 挡成 `401 + 20008「权限已变更」` ——
> 脚本里已先清 `TOKEN` 再重登（HR-12 的同类脚本没这一步，见下「上报」）。

## 上报（不属本卡，未自行改动）

1. **`scripts/verify/hr/employee_http_smoke.sh`（HR-12）有同一个登录坑**：它的 `login()` 在
   「必改密」分支里带着**旧令牌**重登（`req` 会给非空 `TOKEN` 加 `Authorization`），
   空库首启时会拿到 `401 + 20008`。本卡脚本已修（`TOKEN=""` 再重登）；HR-12 的脚本未改，
   建议单开卡修（或下次改该文件时顺手带上）。
2. **`utils::client_ip` 在无可信转发头时返回空** ⇒ `audit_logs.ip` 为 NULL（`detail.ip` 同样为 null）。
   这是框架「内网保护」语义，不是本卡代码问题；若合规要求必须记 IP，需要配置可信代理或改框架口径。
3. 设计 §5.2 #7 只覆盖员工主档两列；合同 / 薪资标准的敏感值不在本卡（无对应卡，建议并入 HR-47 权限矩阵评估）。
