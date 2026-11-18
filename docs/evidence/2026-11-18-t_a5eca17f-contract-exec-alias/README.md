# L3 干净检出闸红②：合同域第 6 个码 `business:erp:contract:execution` 未纳入双码守门人

卡：`t_a5eca17f`（父卡 `t_71d2aa63`，发现于 `t_1d66ac6d` 的全量 `check.sh`）
基线：HEAD `a7048516`（干净树上单跑也红，与 CWD 无关）
结论：**判据口径调整**（「集合完全相等」→「别名不许悬空 + 新增码白名单 + 旧码列逐条一致」），
`LEGACY_ALIASES` 数据**一行未动**（没有塞假别名），已回绿并 push。

---

## 1. 现象（真跑，修前）

```
$ cd /opt/cloudpivot/backend
$ cargo test -p cloudpivot --lib -- kernel_axis_tests::contract_aliases_match_handler_permissions
test kernel_axis_tests::contract_aliases_match_handler_permissions ... FAILED
thread '…' panicked at app/src/kernel_axis_tests.rs:49:5:
assertion `left == right` failed: LEGACY_ALIASES 的规范码必须与 contract_handler::PERMISSIONS 完全相同
  left:  {"business:erp:contract:create","…:delete","…:list","…:status","…:update"}      ← 5（别名表）
 right: {…,"…:execution",…}                                                              ← 6（handler）
```

根因：`M4-19`（`b1772279`，合同执行量统计）把 `PERMISSIONS` 从 5 个扩到 6 个
（`contract_execution_dto::EXEC_PERMISSION = business:erp:contract:execution`），
`shared/permission_alias::LEGACY_ALIASES` 仍是 P2-4 切换过的 5 行 → 「两集合相等」必红。

## 2. 为什么不是「加一条假别名」

`LEGACY_ALIASES` 的语义是**「一行一个已从 `erp:contract:*` 切换过来的码」**。
`execution` 是 P2-4 前缀统一**之后**新增的码，从来没有 `erp:contract:execution` 这个旧码。
塞 `("erp:contract:execution", "business:erp:contract:execution")` 会让表里出现
一条**永不命中**的假映射、文档与代码不一致、并把「新增码 / 切换码」两种语义混在一张表里
（兼容期收尾整表删除时说不清哪些能删）。业务侧本来也钉死了这条：
`contract_handler::tests::contract_legacy_codes_alias_both_ways` 断言
`aliases_of(execution).is_empty()` —— 假别名会立刻把它打红。

## 3. 修法（判据只加不减）

`kernel_axis_tests.rs` 的三条判据（原为一条 `set == set` + 一条 `len == 5`）：

| # | 判据 | 挡住的场景 |
|:--:|:--|:--|
| ① | `canonical_in_table ⊆ canonical_in_code`（悬空别名 = 表格腐烂） | 别名表加了 handler 里不存在的码 |
| ② | `canonical_in_code - canonical_in_table == ADDED_AFTER_PREFIX_UNIFICATION` | handler 新增码没登记（= 忘了补它的旧码映射） |
| ③ | 旧码列 `== contract_handler::LEGACY_PERMISSIONS`（逐条 + 顺序） | handler 改了某个**已切换**码而别名表没跟 |

`ADDED_AFTER_PREFIX_UNIFICATION = ["business:erp:contract:execution"]` 就在用例里逐条登记
（刻意留的一次人工确认：新增码**不该**有别名，但它必然不在别名表里）。

强度对照（**只加不减**，四个场景全部保红，唯一变化是去掉本卡的误报）：

| 场景 | 旧 `set == set` | 新三条 |
|:--|:--:|:--:|
| handler 改了某个已切换码的名字，别名表没跟 | 红 | **红**（① 悬空 + ③ 旧码列不等） |
| 别名表加了 handler 里没有的码 | 红 | **红**（① `is_subset` 失败） |
| handler 新增一个全新码、没登记白名单 | 红 | **红**（② `only_in_code` ≠ 白名单） |
| handler 新增一个全新码并登记白名单（本卡场景） | 红（误报） | 绿（正确） |
| handler 删掉一个码、白名单还留着它 | 红 | **红**（②） |
| handler 删掉某个已切换码（别名表没删） | 红 | **红**（① 悬空 + ③） |
| 别名表删掉某个已切换码 | 红 | **红**（② left 多出该码 + ③） |

长度：不再写死 `== 5`，改由 ③ 与 handler 的 `LEGACY_PERMISSIONS`（当前 5 条，M4-19 时
刻意没跟着扩）一一钉死 —— 语义从「合同域 5 个码」变成「已切换码一个不能少」。

文档同步：
- `backend/crates/cloudpivot-kernel/src/shared/permission_alias.rs`
  - 模块头「兼容期结束怎么删」第 3 条：由「表与 `PERMISSIONS` 逐条对应」改成
    「已切换码 ⊆ PERMISSIONS + 旧码列与 `LEGACY_PERMISSIONS` 逐条一致」，并点名用例文件；
  - `LEGACY_ALIASES` 文档：写明「前缀统一之后新增、从来没有旧码的码**不进本表**」，
    指向 `ADDED_AFTER_PREFIX_UNIFICATION` 白名单。**表数据一行未动**。
- `backend/app/src/business/erp/contract/contract_handler.rs`：`PERMISSIONS` 文档补一句
  「加第 7 个码时的另一半」（登记白名单 or 补旧码映射，别塞假别名）。
- `backend/app/src/kernel_axis_tests.rs`：用例文档改写为三条判据 + 为什么不用
  「集合完全相等」（含 `execution` 的反例）。用例名、文件位置不变。

改动规模：`kernel_axis_tests.rs` +65/−13、`permission_alias.rs` +19/−2（文档）、
`contract_handler.rs` +5（文档）。**零业务逻辑改动、零迁移、零 `LEGACY_ALIASES` 数据改动。**

## 4. 验收（每条都真跑，输出在同目录）

| 项 | 结果 | 证据 |
|:--|:--|:--|
| `cargo test … contract_aliases_match_handler_permissions`（CWD=`backend/`） | 1 passed / 0 failed | `02_cwd_backend.out`、`09_green_kernel_axis_all.out`（rustfmt 后） |
| 同上（CWD=`backend/app/`） | 1 passed / 0 failed | `01_cwd_backend_app.out`、`10_green_cwd_backend_app.out` |
| `kernel_axis_tests` 三条全suite | 3 passed / 0 failed，两个 CWD 都是 | `09`、`10` |
| 合同域权限相关 4 条（含 handler 侧双码用例） | 4 passed | `11_contract_perm_tests.out` |
| `cargo fmt -p cloudpivot -p cloudpivot-kernel -- --check` | rc=0，0 处 diff | `07_fmt_check.out`（空文件即无 diff） |
| `cargo clippy -p cloudpivot -p cloudpivot-kernel --all-targets -- -D warnings` | 0 warning / 0 error | `08_clippy_all_targets.out` |
| `bash scripts/check.sh --l3`（干净检出 = 提交后的 HEAD） | 见 §6 | `12_L3_gate_after.out` |

### 反证四条（改完立刻还原，用 sha256 校验原字节一致）

| # | 动作 | 结果 |
|:--:|:--|:--|
| ① | 白名单清空 `[&str; 0] = []` | **红**，`left: {"business:erp:contract:execution"} right: {}`，点名该码 → `03_teeth1_whitelist_empty.out` |
| ② | `LEGACY_ALIASES` 塞一条 handler 没有的码 `("erp:contract:audit", "business:erp:contract:audit")` | **红**，`LEGACY_ALIASES 指向了 … 不存在的码：{"business:erp:contract:audit"}`（判据①抓到）→ `04_teeth2_dangling_alias.out` |
| ③ | 删掉 `LEGACY_ALIASES` 里 `status` 那行（模拟「改名了别名表没跟」） | **红**，`left: {execution, status} right: {execution}`，点名 `status`（判据②抓到）→ `05_teeth3_status_row_removed.out` |
| ④ | 追加：把 handler 的 `LEGACY_PERMISSIONS[0]` 改名 `erp:contract:list` → `erp:contract:query`（只动旧码侧） | **红**，`LEGACY_ALIASES 的旧码列必须与 contract_handler::LEGACY_PERMISSIONS 一致（handler 换了已切换码而别名表没跟）`（判据③抓到）→ `06_teeth4_legacy_renamed.out` |

还原方式：`cp` 内存备份回原文件（**没用 `git checkout`**，避免动到共享工作区里别人未提交的改动），
还原后 `sha256sum` 与改动前逐字节一致（`683b8d01…` / `234f0f64…` / `ba7cc31a…`，见会话记录）。

## 5. 边界

- 未改 `LEGACY_ALIASES` 的任何**数据行**（仍是 P2-4 的 5 条），未改 `expand`/`aliases_of` 行为；
- 未碰另一张卡（`t_71d2aa63` / d8c CWD 口径）的任何文件；
- 未动迁移、未动 `sys_menu`、未动前端；
- 共享工作区里他人未提交的改动（`README.md`、`frontend/**` 等）一行未碰，按路径 `git add`。

## 6. 提交与 L3

见 `12_L3_gate_after.out`（提交后 `bash scripts/check.sh --l3` 真跑）。
