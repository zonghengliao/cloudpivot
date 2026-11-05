# t_1b5391e7 · `permission_audit_tests::n16` 因属性顺序被判「裸路由」（收口证据）

- 卡：`t_1b5391e7`（父卡 `t_977c5189` 全量真库复跑时暴露，**非该卡改动引起**）
- 日期：2026-11-06
- 起点 HEAD：`66c1208`
- 结论：**产品行为零变化**。改的是①被测路由的属性书写顺序（与全仓一致）、
  ②N16 路由扫描器的判据（从「往上 600 字符窗口」改成「同一 item 的属性块」），
  ③N17 扫描器自检语料补正/反样本。

---

## 1. 现象（已在起点 HEAD 上复现）

```bash
cd backend && SYS_DB_URL='postgres://alun:***@127.0.0.1:5432/cloudpivot_iso977' \
  cargo test --lib --features test-hooks -- --nocapture --exact \
  system::menu::permission_audit_tests::n16_every_route_has_permission_gate_or_documented_exception
```

```
thread '...n16_every_route_has_permission_gate_or_documented_exception' panicked at
src/system/menu/permission_audit_tests.rs:616:5:
以下路由既没有 #[nik::permission] 也不在无权限白名单里（疑似越权缺口）：
POST /business/flow/linkage/preview（business/flow/linkage/admin.rs:682）
确有理由的请登记到 NO_PERMISSION_ROUTES 并写清「谁在替中间件闸把关」。
test result: FAILED. 0 passed; 1 failed; ... finished in 0.48s
```

与父卡 `docs/evidence/20261106-t977c5189-flow-fixture-isolation/08-full-lib-db.log.txt`
里的那 1 条红逐字一致。

**为什么默认门禁看不到**：`n16` 开头是 `if !ts::ensure_db().await { return; }`，
无库时只打印 `SKIP` 就 return，libtest 记 **passed** ⇒ 不设库的 `cargo test --lib`
跑出「2910/2910 全绿」。这正是父卡给 `scripts/check.sh` 加 `CHECK_WITH_DB=1` 要堵的假绿。

## 2. 根因（判据没放宽，是**扫描器的前提不成立**）

`permission_above()`（旧 370–386 行）从 `#[nik::<method>(` 的位置**只往上**开 600 字符窗口
找 `#[nik::permission(`，注释里写死了前提「本仓声明顺序恒为
`文档注释 → permission → 路由注解 → fn`」。而实测这条路由是**反的**：

```rust
// backend/src/business/flow/linkage/admin.rs（修前 682–689 行）
#[nik::post("/business/flow/linkage/preview")]        // ← 先 post
#[nik::permission(                                     // ← 后 permission（扫描器看不见）
    path = "/business/flow/linkage/preview",
    method = "POST",
    permission = "business:flow:linkage:list"
)]
pub async fn preview_impact(...)
```

宏层面顺序无关：`backend/vendor/nik-macros/src/route.rs` 的 `permission_impl`（234 行）
与 `method_route_impl`（62 行）各自独立 `inventory::submit!`；`method_route_impl` 经
`parse_macro_input!(item as ItemFn)` 解析时**保留剩余属性**再 `quote!{ #input }` 原样输出，
所以两个注解谁在上都会展开、都会注册 ⇒ **运行时闸门一直在，红的是扫描器**。

## 3. 改法（两条都做：a 归位 + b 加固，b 带反样本）

### ① `backend/src/business/flow/linkage/admin.rs`：属性顺序归位（卡面方案 a）

`preview_impact` 改成 `#[nik::permission(...)]` 在前、`#[nik::post(...)]` 在后，
与全仓其余 500 条路由一致。

### ② `backend/src/system/menu/permission_audit_tests.rs`：`permission_above` → `permission_of_route`（卡面方案 b）

不再用「往上 600 字符 + 用 `}\n` 切边界」，改成**按 item 属性块取权限码**：

| 维度 | 旧 | 新 |
|---|---|---|
| 方向 | 只往上 | 上、下都看（同一属性块内） |
| 边界 | 600 字符窗口 + `rfind("}\n")` | **item 边界**：向下到 `pub async fn` 声明行为止；向上吃到紧邻属性行为止 |
| 多行属性 | 靠窗口碰运气 | 方括号净开量（`bracket_delta`，跳过字符串字面量）累计识别 `#[nik::permission(` … `)]` |
| 防失控 | — | 单侧最多 40 行，越界即判裸路由（**红**，安全侧） |

向下**必须**停在 item 声明行，否则真裸路由会被「下一条路由的权限注解」洗白 —— 假绿比假红更糟。

### ③ N17 扫描器自检语料（`--self-test` 正/反样本）

| 夹具 | 期望 | 守什么 |
|---|---|---|
| `mod/ok.rs`（原有） | `Some(system:ok:list)` | permission 在上（常规写法）仍要认 |
| `mod/reversed.rs`（**新增正样本**） | `Some(system:reversed:list)` | **属性顺序颠倒也要认出来**（旧实现这里判 `None`） |
| `mod/naked.rs`（原有，补了 `//!` 头） | `None` | 真裸路由仍要红 |
| `mod/dead.rs`（原有） | `None` | 只有注释里提到码 ≠ 有闸 |
| `mod/leak.rs`（**新增反样本**） | `leak` 必须 `None`、`leak-ok` 必须 `Some` | 向下扫不能越过 item 边界去吞**下一条**的注解 |

## 4. 验收（全部本机实跑）

| # | 检查 | 结果 | 日志 |
|---|---|---|---|
| 1 | **改扫描器、但源码仍保持颠倒顺序**跑 N16 | `ok` —— `517 条路由：501 条有权限注解 + 16 条白名单例外`（修前 500 + 裸 1） | `02-n16-green-while-source-still-reversed.log.txt` |
| 2 | 属性顺序归位后跑整个审计模块（N14–N19） | `6 passed; 0 failed` | `03-audit-module-green.log.txt` |
| 3 | **真树负样本**：把 `preview_impact` 的 `#[nik::permission]` 删掉再跑 N16 | 仍然红，点名同一条路由 ⇒ 没把判据放宽成假绿 | `04-negative-removed-attr.log.txt` |
| 4 | `cargo fmt --check` / `clippy -D warnings` / `cargo check` / `pnpm typecheck` | 全绿（`scripts/check.sh` 前四步） | `05-check-with-db.log.txt` |
| 5 | `CHECK_WITH_DB=1 ERP_DB_URL=… SYS_DB_URL=… TASK_DB_URL=… bash scripts/check.sh` | PLACEHOLDER_FULL_DB | `05-check-with-db.log.txt` |

> 第 1 条是关键：**源码还颠倒着**时 N16 就已经绿了，说明修的是扫描器的方向，而不是「把红的源码改掉让测试闭嘴」。
> 第 3 条保证这个放宽没有副作用。
