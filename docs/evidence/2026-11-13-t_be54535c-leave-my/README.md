# 证据：`/leave/my` 被判 403 + 页面码复用导致的数据越权（卡 t_be54535c）

- **日期**：2026-11-13
- **环境**：本机（99）隔离实例 `127.0.0.1:18147` + 145（172.18.10.145）空演练库
  `hr47_drill_1113105057`（`--fresh` 现建，跑完已 DROP，残留计数 0）
- **二进制**：`backend/target/debug/cloudpivot`（本卡改动后的 debug 构建）
- **脚本**：`scripts/verify/hr/hr_permission_matrix.sh`（HR-47 产物，本卡把两条 `[KNOWN]`
  改成硬断言）

## 一、结论（一句话）

`GET /business/hr/leave/my` 对 `EMPLOYEE` 从 **403 → 200 且只返回本人**；
`GET /business/hr/leave/page` 对 `EMPLOYEE` 从「本人 + 同事 2 行」→ **只出本人 1 行**；
`GET /business/hr/salary/page` 对 `EMPLOYEE` 仍 **403**；
`HR_MANAGER /leave/page` 仍能看到本部门全部（**没有误收窄**）；
未登录访问 `/leave/my` 仍 **401**（不是公开接口）。

## 二、两层根因与修法

| # | 根因 | 修法 | 门禁 |
|---|---|---|---|
| ① | `/leave/my` 在权限规则表里**没有自己的行**，被兄弟模板规则 `/leave/{id}`（要 `leave:detail`）按 score 2 命中 | 框架新增「登录即可」显式规则：`#[nik::auth_only(path, method)]` → 规则值 `@auth`（`nik_web::AUTH_ONLY`），score 0 精确胜出；中间件命中即跳过权限码校验（认证层仍在外层保证 401） | `permission_audit_tests::n21`（全仓 763 条路由 / 738 条规则，静态路由被模板规则接管即红）+ `self_service_handler::tests::login_only_routes_declare_an_explicit_auth_only_rule` |
| ② | 页面行 `auth_code` 兼作全量列表接口码 ⇒ `EMPLOYEE` 实际持有 `leave:list`；而 `biz_leave_request` 的 `owner_col = NULL` ⇒ `SELF` 范围降级成「本人所在部门」 | `leave_handler::scope_to_self_without_detail`：缺 `business:hr:leave:detail` 时**强制 `mine=true`**（不是 403 —— 前端「我的请假」tab 就走这条接口）；前端「全部请假」tab 的显隐判据同步改成 `leave:detail` | `leave_handler::tests::page_scope_is_forced_to_self_without_leave_detail` + 本目录的 HTTP 证据 |

为什么不能照抄 `/salary/page` 的 403：工资单有 `/salary/my` 这条独立路径，请假没有
（前端「我的请假」tab 复用 `/leave/page?mine=true`），一刀切 403 会把员工自己的请假一起挡掉。

## 三、原始输出

| 文件 | 内容 |
|---|---|
| `01_matrix_keep_run.out` | `KEEP=1 KEEP_DB=1 bash scripts/verify/hr/hr_permission_matrix.sh --fresh` 全量输出（留现场那一轮）：**563 PASS / 0 FAIL / 0 KNOWN**，含 C3b / C5 的实测值 |
| `02_http_evidence.txt` | 同一实例上的逐条 `curl` 现场（7 项，含未登录 401 与 HR_MANAGER 对照组） |
| `03_unit_tests.txt` | 离线单测：`business::hr::leave_handler::tests` / `business::hr::self_service_handler::tests` / `permission_audit_tests::n21` |
| `04_matrix_final_run.out` | 最终一轮（代码定型后重建二进制）：`--fresh` 自建自删，563 PASS / 0 FAIL / 0 KNOWN，库残留 0 |

## 四、怎么复现

```bash
cd /opt/cloudpivot/backend && cargo build --bin cloudpivot      # 必做：脚本用 target/debug/cloudpivot
cd /opt/cloudpivot
bash scripts/verify/hr/hr_permission_matrix.sh --fresh          # 建库 → 起实例 → 三层矩阵 → DROP
# 要留现场抓原始 HTTP：
#   KEEP=1 KEEP_DB=1 bash scripts/verify/hr/hr_permission_matrix.sh --fresh
```

## 五、残留

- 145 上 `hr47_drill_*` 库计数 **0**（本卡 KEEP_DB 那轮的库已手工 DROP）；18147 端口无监听。
- `docs/evidence/2026-11-13-hr47-perm-matrix/`（HR-47 原始现场）保留不动，其两条 `[KNOWN]`
  在本卡改动后已转 PASS —— 见 `01_matrix_keep_run.log` 的 `KNOWN=0`。
