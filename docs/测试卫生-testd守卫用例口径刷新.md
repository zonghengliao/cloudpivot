# 测试卫生 · tests/testd 三条守卫型用例口径刷新 + TD- 夹具确定性清理

- 卡片：`t_3d77b506`【TEST-HYGIENE】tests/testd 3 条守卫型用例口径过期
- 触发：DEPLOY-A（`t_e8785aac`）全量 `cargo test` 时 `--test testd` = **64 passed / 3 failed**
- 结论：**3 条都不是回归**，是 ① 守卫型用例断言「缺陷仍在」而缺陷已经修好（2 条）
  + ② 那两条红用例 panic 后跳过尾部清理，把 `TD-` 夹具留在库里，把收口用例打成红（1 条）
- 交付后实测：`cargo test --test testd -- --test-threads=1` = **67 passed / 0 failed**（458.16s），
  `TD-` 前缀在库里 **0 行**（psql 独立复查）

---

## 1. 三条红的定性

| 用例 | 旧断言 | 为什么变红 | 定性 |
|---|---|---|---|
| `announcement::g03_delete_announcement_keeps_read_receipts` | 公告软删后关联 `sys_notice` **不**下线 | **FIX-04** 把「公告软删 + 通知下线」绑进同一事务（`announcement::model::soft_delete_with_notice` → `notice::model::withdraw_by_biz`） | **A 类：断言过时**（不是回归） |
| `linkage::k08_mutex_erp_maker_checker_gap` | `src/business/erp/` 里**没有** `SELF_APPROVAL` | `ac1a34d`（P-FLOW-3A）+ P-FLOW-3B 给四个审批桥接上 `guard_self_approval`（403 + 65019） | **A 类：断言过时** |
| `storage::zz_no_fixture_residue_after_full_suite` | `TD-` 前缀残留必须 0 | 上面两条 panic → 尾部 `ctx.reset().await` 不执行 → 夹具留库 | **A 类：被假缺陷带红**（根因是清理机制，不是业务） |

> 三条**同日同源**：守卫型用例的「缺陷已修 → 必红」+「红了就不清理 → 连带红」。修的时候
> 必须两件事一起做，只改断言会留下「下次谁 panic 谁污染库」的机制性缺口。

---

## 2. 改了什么

### 2.1 G03：改成正向断言（公告删 → 通知同事务下线、回执保留）

- 夹具从 1 个收件人扩到 2 个：`u1` 读通知、`u2` 不读 —— 正向断言要「变前/变后」成对，
  否则「本来就是 0」也能骗过用例；
- 断言（全部真库）：公告 `deleted_at` 有值；`sys_announcement_read` 回执仍 1 行；
  用户可见列表查不到；`sys_notice.deleted_at` 有值；收件行 **2 行都在**、`is_deleted` 全 TRUE；
  `u1` 的 `read_flag=TRUE` + `read_at` 非空原样保留；`count_unread` 两人都归零；
  `page_my_notices` 两人都查不到它；
- 文件注释写明**旧断言失效点 = FIX-04**，并给出「缺陷已修」的证据链。

### 2.2 K08：改成正向断言（内控闸必须存在，一个一个点名）

- 旧的「扫 erp 目录关键字，命中就红」→ 新的「四个审批桥**逐个**必须有
  `guard_self_approval` + `FLOW_SELF_APPROVAL_FORBIDDEN`，缺一个就红」；
- 追加口径断言：错误码 `65019`、`flow::internal_control::CREATOR_REASON`（制单人 + 留痕）、
  框架自审策略 `FORBID` 仍在；
- 保留 ①（互斥基座行为层：双持 → `61009`）不动；
- 用例**名没改**（`..._gap`）——报告/看板逐条引用的就是它，注释里说明「名字是历史，
  语义已反转为『gap 已补』」；
- 明确**没有放宽**：没有把扫描改成忽略 `SELF_APPROVAL`（那等于把 BUG-D-14 重新打开）。

### 2.3 Z01 + 夹具清理：从「尾部兜底」改成「每例确定性 + panic 也清」

根因不是清理语句写错，是**清理压根没执行**：`ctx.reset().await;` 写在用例最后一行，
用例 panic 时 unwind 直接跳过。行数也是「崩几轮 × 每轮遗留」，库里前缀就是
`TD-G03-…` / `TD-K08-…`（按前缀 + 外键链 `sys_user_role → sys_role_mutex → sys_role →
sys_user`、`sys_notice_receiver → sys_notice → sys_announcement` 逐条查证）。

改法（只动 `backend/tests/testd/**`）：

1. 新增 `common::case(tag, |ctx| async move { … })`：用例体包进
   `AssertUnwindSafe(..).catch_unwind()`，**正常/panic 两条路径都 `ctx.reset()`**，
   panic 清完再 `resume_unwind`（红还是红，绝不吞失败）；
2. **61 条用例**统一换成这个外壳，用例体一行没改；
3. `Ctx::new` 开工前先扫 `TD-<tag>%`（同一条用例上一轮的残留）→ 「崩了 → 脏了 → 下轮自愈」；
4. `residue()` 从 11 张表扩到 16 张（补 `sys_user_role` / `sys_role_mutex` /
   `sys_notice_receiver` / `sys_announcement_read` / `sys_announcement_attachment`），
   正常返回时**断言本前缀残留 = 0**（重新查库，不是 self-report）；
5. Z01 保留但降级为**外部交叉验证**（它自己不删东西）：再红才说明有夹具逃过两层防线。

> 「不靠按名字排序最后兜底」：Z01 的 `zz_` 命名只影响执行顺序，**正确性不再依赖它** ——
> 任一用例跑完（哪怕 panic）自己就把前缀清干净，开工前还会再扫一遍同 tag。

---

## 3. 报告 / 台账同步

`docs/test-report-modules.md`：

- 顶部加「口径更新（2026-11-03 · 本卡）」小节：三条用例的旧断言 / 失效改动 / 新断言对照；
- `BUG-D-05`、`BUG-D-14` 标 **已修复 ✅（2026-11-03 关闭）**，补修法与落点；新增 §0.2 修复记录；
- G03 / X05 / K08 / §11 「已知缺口」/ 最终放行判断 逐处改口径；
- 新增 §9.1：记录「清理挂在用例尾部」这第二类污染、根因、修法与实测。

---

## 4. 实测证据

| 项 | 命令 / 结果 |
|---|---|
| 修前 | `test result: FAILED. 64 passed; 3 failed`（`announcement.rs:319` / `linkage.rs:718` / `storage.rs:759`） |
| 修后（本卡，交付口径） | `cargo test --test testd -- --test-threads=1` → **67 passed / 0 failed / 0 ignored**（458.16s；第二次独立整轮 489.68s，同样 67/0） |
| 库侧独立复查 | `TD%` 前缀：`user=0 role=0 user_role=0 ann=0 notice=0 mutex=0` |
| 19 个集成 target | 全部 `test result: ok`（含 testd 67/0、linkage 62/0、fix04 9/0…），`CARGO_EXIT=0` |
| lib 单元测试 | `2086 passed / 1 failed` —— 唯一失败是 **D3 卡（`t_217a95ad`）未提交的工作树文件** `src/business/flow/edge_admin_tests.rs`（未被 git 跟踪；单跑这 2 条 → 2 passed，全量跑红 = 该文件内部用例间夹具残留），与本卡无关；本卡开工 HEAD `a71db4a` 上父卡 C1 跑过全量 `2073 passed / 0 failed` |
| 编译 | `cargo test --test testd --no-run` 0 warning；`rustfmt --check tests/testd/*.rs` 干净 |

原始日志：`docs/evidence/20261103-testhygiene/`（修前复现 / 修后整轮 / 全量 target / 库侧复查 / git）

> 首次全量跑出现过 `1998 passed / 87 failed`，原因是**我建库时只等到「数据库迁移完成」就
> kill 了实例**，`bootstrap::seed_super_admin` 还没跑完 → 库里没有 `admin`、没有
> `SUPER_ADMIN` 绑定 → 87 条用例按设计报 `65018 本节点没有有效审批人，且平台没有可兜底的
> 超级管理员`。修正建库脚本（等 `admin↔SUPER_ADMIN` 绑定落到库再停）后重跑，就是上面的结果。
> 这条也记在这里，免得下次有人踩同一个坑。

---

## 5. 边界 / 没做的事

- **没改任何业务代码**：`git show --stat` 只有 `backend/tests/testd/**` 与本报告 + 报告文档；
  旧守卫跟着修复走，不是反过来改实现。
- **没放宽判据**：K08 从「扫关键字命中就红」变成「逐桥点名必须有」，判据更严不是更松。
- **生产零触碰**：所有真库操作只在 `cloudpivot_test`（testd 硬校验）与新建的
  `cloudpivot_tdh`（全量套件用），没连 `/cloudpivot` 生产库，没碰 8023 上的服务。
- `K08` 是**源码扫描型**断言：它证明「四个桥里有闸」，不证明「闸在每个分支都被调到」——
  真正的端到端由 `erp::pflow3b_tests`（真库，制单人/发起人 403、第三方放行）与
  `flow::internal_control` 单测覆盖。这条边界写在报告 §11 里，不夸大。
- 新增受内控约束的审批业务时必须同步登记到 `SELF_APPROVAL_GUARDS` 表，否则本用例不会
  自动覆盖到它（已知口径边界）。
