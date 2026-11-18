# 测试卫生 · 菜单：`business:erp:framework-agreement:page` 出现在后端源码（N18 遗留）

> 卡：`t_23aad8fd`（源头：`t_85e005d9` 全量套件第一轮 N18 红）· 日期：2026-11-18 · 主机：99（开发机，PG 在 145）
> 验证库：`cloudpivot_test_n18`（`createdb` 空库 → 后端实例跑全量迁移到 **V0460**；夹具自清；验完已 DROP，留档见 `08_db_state.txt`）

## 一、结论先行

问题**不是一条测试写错**，是「口径打架」：

1. `framework_agreement_dto.rs`（M4-11b / `t_f358da98`）里有页面显形码 `…:framework-agreement:page` 的**字面量** 2 处：
   第 21 行文档注释、第 569 行 `#[cfg(test)]` 里的**字面量负断言**（`!AGREEMENT_PERMISSIONS.contains("…:page")`）。
2. 审计扫描器的跳过口径是**按文件名**（`is_test_file`：`*tests.rs` 等），**不按 `#[cfg(test)]` 属性** ——
   所以 `*_dto.rs` 里的内联测试模块照样被扫，那条「反证断言」自己把约定 14（页面码后端零引用）踩了。
3. `t_005d6b65`（M4-16）当时的处置是**把 `ROUTE_LEVEL_ONLY` 里这条例外销号**，于是 N18 变绿 ——
   代价是 N14 的「每个菜单码都要有源码落地」被那条**假落地**（测试里的字面量）蒙过去了：
   按 `n18` 的实现（只遍历 `ROUTE_LEVEL_ONLY` / `PENDING_LANDING` 两张台账，见
   `permission_audit_tests.rs` 的 `for (code, reason) in ROUTE_LEVEL_ONLY`）可推：HEAD 上 N18 不会红，
   但页面码在后端其实**没有任何真实落地**，也没登记台账 —— 绿是空心的。

本卡改成治根（三件事，缺一不可）：

| # | 改法 | 为什么必须这样 |
|:--:|:--|:--|
| ① | 业务源码里的 2 处字面量**全部删掉**（文档注释改写成 `…:page`，负断言改成「前缀 + 动作白名单」） | 约定 14 / §1.7.2：页面码连常量都不许定义，`git grep` 要 0 行 |
| ② | 把这条码按 **C 类（纯前端显形码）重新登记**回 `ROUTE_LEVEL_ONLY` | N14 要求「每个 `sys_menu.auth_code` 都要有源码落地或台账例外」；删了字面量后唯一的合法落点就是台账 |
| ③ | `docs/编码规范.md` §1.7.2 把「扫描器跳过 `#[cfg(test)]` 内联模块」这句**改成实情** | 这句话与实现不符，正是下一个人再写一次负断言的诱因 |

## 二、改动（4 个文件）

| 文件 | 改动 |
|:--|:--|
| `backend/app/src/business/erp/purchase/framework_agreement_dto.rs` | ① 模块文档：页面码改写为 `…:page` + 指向 `V0452` 与台账；② `permission_codes_match_v0456` 的负断言 → 「前缀 + 动作白名单（list/create/update/delete/submit/execution）」，**断言一条没少**（覆盖 6 个码 × 2 条判据，比原来 1 条更强） |
| `backend/app/src/system/menu/permission_audit_tests.rs` | `ROUTE_LEVEL_ONLY` 里把框架协议页按 C 类**重新登记**（含销号→复登的来龙去脉），N18 从此持续核这条例外 |
| `docs/编码规范.md` | §1.7.2 第 1 条：写清扫描器**按文件名**跳过、业务文件里的 `#[cfg(test)]` 照样被扫；给出「动作白名单」的正确写法 |
| `docs/evidence/2026-11-18-t23aad8fd-n18-page-literal/` | 本证据包（真跑输出） |

## 三、真跑的验收（每项可复核）

| 文件 | 内容 | 结果 |
|:--|:--|:--|
| `00_before_at_head.out` | `git show HEAD:` 证明修复前两处字面量在 HEAD 上（dto 21/569）+ 台账里当时**没有**这条例外 | 根因坐实 |
| `01_menu_tests_green.out` | `cargo test --lib system::menu`（**首次**，修复后）：**22 passed / 0 failed**，`[N14] 519 个种子菜单权限码全部有源码落地`、`[N18] 例外台账 14 条…全部有理由且仍然成立` | ✅ |
| `02_teeth_n18_literal_reintroduced.out` | **反证**：把字面量塞回 dto 再跑 n18 ⇒ `panicked: ROUTE_LEVEL_ONLY 台账过期：business:erp:framework-agreement:page：后端源码已有引用` | ✅ 会红（不是假绿） |
| `03_teeth_action_whitelist.out` | **反证**：把白名单里的 `execution` 去掉 ⇒ `permission_codes_match_v0456` 红，报文点名动作位 | ✅ 断言有牙 |
| `04_git_grep.out` | `git grep -n "framework-agreement:page" -- backend/app/src/business/` = **0 行**；除台账登记点外 src 全树 0 行 | ✅ |
| `05_final_green.out` | 最终代码（反证已回滚）：`system::menu` **22 passed** + `business::erp::purchase::framework_agreement_dto` **10 passed** | ✅ |
| `06_fmt_clippy.out` | `cargo fmt -p cloudpivot -p cloudpivot-kernel -- --check` rc=0；`cargo clippy -p cloudpivot --lib --tests -- -D warnings` rc=0 | ✅ |
| `07_check_sh_fast.out` | `bash scripts/check.sh --fast`（L0 静态闸 + L1 fmt/clippy/check/前端 typecheck/单测）：**全部检查通过（L0 L1），rc=0** | ✅ |
| `08_db_state.txt` | 验证库 `cloudpivot_test_n18`：`max(version::int)=460`、392 条迁移；页面行 `auth_code=…:page` 在位未软删（菜单 568 条） | ✅ |
| `09_check_sh_full_pre_existing_red.out` / `09b_…txt` | 顺带跑了**全量** `bash scripts/check.sh`（L0–L4）：在**批次A/B 闸②** 上红 —— **38 个**测试面文件未登记 `scripts/lint/scope_gate.py` 的 `SKIP_MANIFEST`（全是别人卡的 `*_tests.rs`，本卡 diff 一个都没碰；`--fast` 档看不见这一层） | ⚠ 与本卡无关，已单独立卡 `t_1d66ac6d` |

## 四、口径与边界（决定，不是待办）

1. **「没有 page」用动作白名单表达，不用字面量负断言。** 判据等价：`AGREEMENT_PERMISSIONS` 里每个码必须以
   `business:erp:framework-agreement:` 开头、动作位 ∈ {list, create, update, delete, submit, execution} ——
   `page` 不在其中，语义与「清单里没有 page」完全一致，但源码里不出现那个字符串。
2. **不删断言，只换表达**：M4-11b 原有的 6 个期望码 + 「页面码不在清单里」两条判据一条没减（见 `05` 的 10 passed）。
3. **台账登记是 C 类的**：这条码是**永远由前端路由拦**的终态例外（不是 `PENDING_LANDING` 那种「handler 还没写」的过渡态）。
4. **不动菜单、不动迁移、不动 `sys_menu` 授权**：页面行由 `V0452` 种下、`V0456` 不动它，本次一个字节没碰 SQL。
5. **不改审计扫描器语义**：没有放宽 `code_refs`（仍扫业务文件里的内联测试模块）。改成「跳过 `#[cfg(test)]`」会把
   真·死码也一起放过，属于给门禁开口子；正确做法是**别在业务文件里写页面码字面量**，这条已写进编码规范。

## 五、遗留（不在本卡范围）

- `t_85e005d9` 第一轮同目录那 15 条 `sys_user_role` 绑定被整表清场 / 成本批次 `CS-203109-0028` 残留 → 卡 `t_18b222b9`。
- 另一条同轮红：`n08` 报 `supplier-access` 前端未注册路由 → 归 M4-16 收口卡，不在本卡。
