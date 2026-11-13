# 菜单/权限审计 n16 清红（卡 t_4750ffee）

> 结论：`cargo test --lib system::menu::permission_audit_tests -- --test-threads=1`
> **8 passed / 0 failed**（原本 n16 红）。n18 / n19 在**干净库**（335 条迁移的空库首启）
> 本来就是绿的，它们的红只出现在被别的模块污染过的共享库上 —— 本次一并给出复核证据。

## 1. 红的是什么（改动前原始输出）

`01_before_n16_fail.txt`：

```
以下路由既没有 #[nik::permission] 也不在无权限白名单里（疑似越权缺口）：
GET /business/dashboard/my-metrics（business/dashboard/handler.rs:37）
POST /api{path}\（business/erp/cost_period_handler.rs:950）
POST /api{path}\（business/erp/cost_period_handler.rs:970）
POST /api{path}\（business/erp/cost_period_handler.rs:994）
GET /api{path}\（business/erp/cost_period_sheet.rs:702）
GET /business/erp/cost-switch（business/erp/cost_switch_handler.rs:52）
GET /system/auth/idle-policy（system/auth/handler.rs:699）
```

七条里是**两类完全不同的东西**，不能一视同仁地「补权限或登记例外」：

| 类 | 条数 | 实质 | 处理 |
|:--|:--:|:--|:--|
| 真裸接口 | 3 | 接口按设计就是「登录即可」（前端 api 层文档也这么写），但**没写 `#[nik::auth_only]`** ⇒ 规则表里没有这一行，N16 判裸接口 | 补 `#[nik::auth_only(path, method)]` |
| 扫描器幽灵路由 | 4 | 不是路由。是 `cost_period_handler.rs` / `cost_period_sheet.rs` 里**注解自检**用的字符串字面量：`format!("#[nik::post(\"/api{path}\")]")`。扫描器只认 `#[nik::` + 引号串，就把它们当成路径 `/api{path}\` 的两条真路由 | 扫描器**不再扫 `#[cfg(test)]` 模块**（与「测试文件不算校验点」同一口径） |

## 2. 三条真裸接口：为什么是 `auth_only` 而不是「补权限码」

三条都被前端 api 层文档明确写成「无（登录即可）」，语义上就是「登录即可」：

| 接口 | 谁在用 | 为什么不能挂权限码 |
|:--|:--|:--|
| `GET /business/dashboard/my-metrics` | 工作台四张个人统计卡 | 数字全按 `claims` 的用户 id 过滤，没有 `?user_id=` 入参；给新员工套模块读权限会让首页 403 而卡片本来能安全返回 0 |
| `GET /business/erp/cost-switch` | 出库单表单 / 成本页的开关探针 | 普通财务、仓管没有 `support:config:list`，但界面必须知道开关是开是关 |
| `GET /system/auth/idle-policy` | 前端空闲退出计时 | `auth.idle_timeout_minutes` 在 `sys_config` 里，绝大多数账号读不到参数配置页 |

`#[nik::auth_only]` 是框架的显式「登录即可」规则（`nik_web::AUTH_ONLY` 哨兵，
`vendor/nik-web/src/middleware/permission_check.rs:256`）：命中后跳过权限码校验，
但认证层照旧 401 —— 语义与「没有规则」相同，**额外**多一层：不会被同前缀模板规则
以 score 2 静默接管（N21 的口径，`/business/hr/leave/my` 就是被这么坑过）。
换句话说，这三条是「原来能跑的裸路由」→「有显式声明的登录即可路由」，行为不变、约束变强。

## 3. 四条幽灵路由：扫描器补一条口径

`Repo::load` 现在为每个文件生成两份文本：

| 字段 | 口径 | 谁用 |
|:--|:--|:--|
| `clean` | 剥注释，**含** `#[cfg(test)]` 模块 | `code_refs` / `is_annotated`（N14 / N18 的「码有没有落地」） |
| `route_src` | 剥注释 **且**剪掉 `#[cfg(test)] mod … { … }` 整块 | `scan_routes`（N16 / N21 的「这是不是一条路由」） |

为什么不能简单地把 `clean` 也剪掉：`code_refs` 的口径是「源码里有引用就算落地」，
业务文件末尾的自检里常引用 `PERM_*` 常量，一刀切会让 N14 反过来变红。所以只给
**路由扫描**换文本。

实现（`strip_cfg_test_modules`）的三条纪律：

1. 只剪 `#[cfg(test)]` **紧跟 `mod <名字> { … }`** 的形态；标记与 `mod` 之间出现 `;`
   就放弃（`#[cfg(test)] use …` 这类单行项不动）。
2. 花括号按「跳过字符串/字符字面量」计数 —— 自检里的 `"{path}"` 不会把配对算错；
   未闭合就整段不剪（让下游继续报红，而不是静默吞掉半个文件）。
3. 换行逐个保留 ⇒ 报错里的 `file:line` 仍指向原文行号（`01_before_n16_fail.txt` 的行号
   与 `04` 之后的输出一致）。

## 4. n18 / n19：在干净库上是绿的，红在污染库

两条都不依赖源码改动，依赖**库里的数据**：

* n18 的判据是「例外台账里的码仍在 `sys_menu`、且后端源码零引用」。核过：
  8 条台账码全部在 `sys_menu`（`system:preference:view` + 5 个 HR `*:page` +
  `employee:resign` / `leave:delete` / `salary-item:delete`），源码里只出现在注释里
  （`///` / `//!`，剥注释后为 0 引用）。`03` 的输出：`例外台账 6 条、临时台账 3 条、
  白名单 18 条，全部有理由且仍然成立 ✓`。
* n19 的判据是「已发布流程的 APPROVAL/COUNTERSIGN 节点上，`ROLE` 指派角色必须有
  `business:flow:approve`」。干净库里 53 处 ROLE 指派**全绿**；共享库里的红来自别的
  模块的用例残留（`p0fix_*` 一类的种子流程），属「测试卫生」问题，不是授权缺口 ——
  所以**没有加迁移**，也别为此加迁移（会掩盖真正的缺口）。

## 5. 验证

| # | 命令 | 结果 | 原始输出 |
|:--:|:--|:--|:--|
| 1 | `SYS_DB_URL=… cargo test --lib system::menu::permission_audit_tests -- --test-threads=1` | **8 passed / 0 failed**（n16 绿，`759 条路由：736 有注解 + 23 例外`） | `02_after_permission_audit_green.txt` |
| 2 | `SYS_DB_URL=… cargo test --lib system::menu -- --test-threads=1` | **22 passed / 0 failed**（含 n01–n14 连同源 `n10_annotated_permissions_have_menu_code`） | `03_after_menu_module_green.txt` |
| 3 | 同库跑 `business::dashboard` / `cost_switch` / `system::auth` / `business::hr::self_service` | 4 / 10 / 29 / 25 全绿 | 见 §5.1 |
| 4 | 起真实实例（`cp_p102_audit`，端口 18097），超管令牌 | 三条接口无令牌 401，有令牌 200 | `04_runtime_super_admin.txt` |
| 5 | 同上，**零菜单授权**账号 `p102probe` | 三条接口 200；`/system/user/page`、`/business/erp/cost-period/page`、`/system/menu/tree` 全部 403 | `05_runtime_zero_permission_user.txt` |

第 5 条是关键一刀：超管走 `is_super_admin` 短路，证明不了「没有权限码的登录用户也能调」；
零授权账号同时给出「三条 200」与「三条 403」才有说服力 —— 说明 `auth_only` 真的是
「登录即可」而不是「放行所有」。

### 5.1 相邻模块用例（同库 `cp_p102_audit`，`--test-threads=1`）

```
business::dashboard        → 4 passed; 0 failed
cost_switch                → 10 passed; 0 failed
system::auth               → 29 passed; 0 failed
business::hr::self_service → 25 passed; 0 failed
```

## 6. 改动清单

| 文件 | 改了什么 |
|:--|:--|
| `backend/src/system/menu/permission_audit_tests.rs` | ① `SourceFile` 增 `route_src`；② 新增 `strip_cfg_test_modules` / `mod_block_brace` / `matching_brace`；③ `scan_routes` 改用 `route_src`；④ N17 自检加一条「cfg(test) 里的假路由字符串不算路由 + 模块之后的真裸路由仍要报」，路由数 6→7；⑤ 文件头口径同步 |
| `backend/src/business/dashboard/handler.rs` | `my_metrics` 补 `#[nik::auth_only(path = "/business/dashboard/my-metrics", method = "GET")]` + 权限说明 |
| `backend/src/business/erp/cost_switch_handler.rs` | `state` 补 `#[nik::auth_only(path = "/business/erp/cost-switch", method = "GET")]` + 权限说明 |
| `backend/src/system/auth/handler.rs` | `idle_policy` 补 `#[nik::auth_only(path = "/system/auth/idle-policy", method = "GET")]` + 权限说明 |

**没有新增迁移**：本次不动任何授权数据（n19 在干净库上本来就绿）。

## 7. 复现方式

```bash
# 空库首启（335 条迁移）→ 起实例把迁移跑完 → 关掉
su - postgres -c "psql -c 'create database cp_p102_audit owner alun'"
# 然后用 backend/target/debug/cloudpivot 连这个库启动一次（NIK_PROFILE=test），
# 日志出现「数据库迁移完成」后 kill

cd /opt/cloudpivot/backend
SYS_DB_URL="postgres://alun:***@127.0.0.1:5432/cp_p102_audit" NIK_PROFILE=test \
  cargo test --lib system::menu::permission_audit_tests -- --test-threads=1
```
