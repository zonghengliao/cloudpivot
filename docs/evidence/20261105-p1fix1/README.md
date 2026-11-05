# P1-FIX-1：作废收货/出库流程「审批人未配 → 兜底超管 → 422 自审禁止」走不通

- 卡：`t_e63b600f`（P1-FIX-1，来源：`docs/评审报告-现有功能评分-20261104.md` §P1-1）
- 日期：2026-11-05
- 环境：本机（172.18.10.99）隔离库 `cloudpivot_p1fix1_a`（演练/真机）+ `cloudpivot_p1fix1_b`（单测），
  隔离实例 `target/debug/cloudpivot`（`ALUN_PROFILE=dev`，独立库，端口 8023，**没有**动生产 144）
- 基线：`main` @ `573d2d1`

---

## 0. 一句话结论

`stock_in_cancel.n_review` 的审批人来源原来是 **`ADMIN`（= 全部超管）**，生产 4 个超管、造单的正是
admin / laoliu ⇒ 发起人自己就在候选里 ⇒ 引擎 `self_approval=FORBID` 在**发起那一刻**就
`422 + 65019`，**作废根本走不通**。

本卡把审批人来源改成 **`ROLE` = 仓管主管（`WAREHOUSE_MANAGER`）**、`empty_assignee` 由
`FALLBACK_ADMIN` 改成 **`ERROR`**（不再兜底超管）、`self_approval` **保持 FORBID**（内控不许自审），
并把两类失败（65019 / 65018）的报文补成**可执行的配置指引**。

---

## 1. 评审证据的更正（原位）

卡面写「证据：`05_chains_run2.txt` ② 段 422 原文」。实际 **run2 的 ② 段是 PASS**（该轮夹具里造了
6 个审批人，admin 只是其中之一，`ADMIN` 展开后发起人不在候选里）。

422 原文在 **`05_chains_run1.txt`（同一目录）第 21 行**：

```
[FAIL] 收货单落 CANCELLED   POSTED
  (HTTP 422 审批人 超级管理员 是发起人本人，本节点不允许自审（self_approval=FORBID）
   （内控要求：作废复核人不能是发起人本人。请由他人发起，或让另一个账号具备复核权限后再审）)
[FAIL] 库存回滚到作废前     期望=5.0 实际=10.0
[FAIL] 凭证红冲（is_red / reverse_of）  红字=0 反向指针=0
```

结论不变（缺陷真实存在、且本轮 chain 因为它整段没跑通），只是**文件名是 run1 不是 run2**。
run1 之所以失败：当轮「制单员登录」失败（夹具没建出来）⇒ 库里只剩 admin 一个超管 ⇒
「审批人 = 发起人」必然成立。这条正好证明**只要审批岗位指超管，单人超管环境就永远走不通**。

---

## 2. 根因（配置面，不是内控面）

生产实测（144 只读，2026-11-05）：

```
stock_in_cancel v1  n_review  assignee_type=ADMIN  empty_assignee=FALLBACK_ADMIN  self_approval=FORBID
                    biz_flow_node_assignee 0 行（ADMIN 在运行时展开，不落行）
SUPER_ADMIN 成员 4 人：admin / laoliu / laoliu_test / henry.liao
biz_stock_in.create_by：admin 5 张 / laoliu 4 张（POSTED+CANCELLED 合计）  ⇒ 发起人 100% 在候选里
WAREHOUSE_MANAGER 成员 2 人（henry.liao / laoliu_test），但**不是**审批人来源
```

`assignee.rs::finalize` 的自审策略是**整节点**判定：候选里只要出现发起人 ⇒ 直接 `422 + 65019`
（不是「把这个人剔掉、其余人照批」）。所以「审批人来源 = ADMIN」+「发起人是超管」= 死路。

---

## 3. 修法（V0248 + 两处文案）

### 3.1 流程配置（`V0248__stock_in_cancel_reviewer_role`，就地修正种子 v1）

| 项 | 修前 | 修后 |
|---|---|---|
| `assignee_type` | `ADMIN`（运行时展开全部超管） | **`ROLE`** |
| `biz_flow_node_assignee` | 0 行 | **1 行：`WAREHOUSE_MANAGER`（`…f0004`）** |
| `empty_assignee` | `FALLBACK_ADMIN`（角色 0 人 → 兜底超管） | **`ERROR`**（角色 0 人 → 发起失败 + 配置指引） |
| `self_approval` | `FORBID` | `FORBID`（**不动**：作废是反向记账，本人复核 = 没有复核） |
| `graph_snapshot` / `graph_hash` | 种子快照 / `78d34c…` | 同步改 / **`026766…`** |
| 流程描述 | 「作废复核（超管，禁止自审）」 | 「作废复核（仓管主管 WAREHOUSE_MANAGER，禁止自审）」 |

**为什么不是「发布新版本 v2」**：只动**种子行原样**（版本 `…e174` + hash 仍是 `78d34c…` 才动），
客户重新发布过的库整段跳过；生产 `biz_flow_instance` 的 `STOCK_IN_CANCEL` 实例 0 行（无在跑实例）；
换版本（B 卡 `t_1e6a8431` 的 5 节点统一去 ADMIN 化）留给那张卡，本卡只修作废这一条。

**为什么 `empty_assignee` 选 `ERROR` 而不是像 `stock_in_post.n_wh` 那样 `FALLBACK_ADMIN`**：
`FALLBACK_ADMIN` 在「只有一个超管」的库上兜出来的正好是**发起人本人**——修了等于没修（评审的
缺陷就是这条路径）。作废没有「先过账再说」的退路，配错就该报错让人去配。

**hash 怎么算出来的（不是手写）**：独立 canonical 序列化器（Python：递归键排序 + 紧凑 JSON +
`sha256`），先用 **V0174 原快照**自证 —— 算得 `78d34c4557630fd8a57c45f901b427649db06ee380baf1868d257813f6e4e56c`
与迁移里写死的值逐字节相同，再算新快照 = `0267661a1b7d37ebc94a3e8859eecde63173325f57d3949970dcbdd4f8b9b5a0`。
落地后由 `flow::maintain`（行 → hash 重算）与单测 ⑯/⑰ 三方对账。

### 3.2 文案指引（本卡第 3 项）

`stockin_handler_cancel.rs::submit_cancel_tx`：原来只对 65019 补一句「请由他人发起」，
现在**两个码都给可执行指引**（点名岗位 + 配置路径）：

| 码 | 场景 | 新报文（截取） |
|---|---|---|
| 65019 | 发起人本人就在复核岗位 | 「…本流程的复核岗位是「仓管主管（WAREHOUSE_MANAGER）」，而你本人就在这个岗位上：请让另一位复核人发起本单，或把一位非发起人的同事加入「仓管主管」角色后再发起。配置路径：系统管理 → 角色管理 → 仓管主管 → 成员」 |
| 65018 | 复核岗位 0 人（角色没绑人 / 人被停用） | 「…请在「系统管理 → 角色管理 → 仓管主管（WAREHOUSE_MANAGER）→ 成员」绑定至少一名在职的非制单人；或在「审批中心 → 流程定义 → 收货单作废审批 → 作废复核」把审批人改成具体人员。作废会反向回滚六本账，不允许无人复核，也不会自动兜底给管理员」 |

`flow/assignee.rs::resolve`：`empty_assignee=ERROR` 的引擎报文补「节点名 + 来源 + 去哪儿配」
（原来只有一句「本节点没有一个可用审批人」）。只改这一种策略，`SKIP` / `FALLBACK_ADMIN` 一字不动。

---

## 4. 真机验收（隔离实例，HTTP 走生产同一路径）

脚本：`p1fix1_cancel_e2e.py`（场景 A/B/C/D），日志 `e2e_run1.txt` / `e2e_run2.txt`（**连跑两轮，
两轮都是 PASS=20 FAIL=0**）。

```
=== P1-FIX-1 真机作废链路（round 1）base=http://127.0.0.1:8023 db=cloudpivot_p1fix1_a ===
## A. 非超管制单人发起作废（POSTED → PENDING_CANCEL）
  · 作废前: 库存 qty=0.0000 amount=0.0000 ｜ 凭证 4（红字 2）｜ 库存流水 4
  [PASS] 制单人建单   200 ok
  [PASS] 制单人过账   200 ok IN-20261105-000003
  · 过账后: 库存 qty=4.0000 amount=40.0000 ｜ 凭证 5（红字 2）｜ 库存流水 5
  [PASS] 过账使库存增加 4   0.0000 → 4.0000
  [PASS] 非超管制单人发起作废 → 200（修复前这里是 422 自审禁止）   200 ok
  [PASS] 单据进入「作废审批中」   PENDING_CANCEL
  · 发起作废后（账不许动）: 库存 qty=4.0000 amount=40.0000 ｜ 凭证 5（红字 2）｜ 库存流水 5
  [PASS] 发起作废不动账
  [PASS] 待办恰好 1 条且落在仓管主管（复核人）头上   [['PENDING', 'p1fix1_reviewer']]

## B. 仓管主管复核通过 → CANCELLED（六本账回滚）
  [PASS] 复核人审批通过   200 ok
  [PASS] 单据落 CANCELLED   CANCELLED
  · 作废后（回滚 + 红冲）: 库存 qty=0.0000 amount=0.0000 ｜ 凭证 6（红字 3）｜ 库存流水 6
  [PASS] 库存回到作废前
  [PASS] 凭证红冲出红字   红字 2 → 3
  [PASS] 审批单落 APPROVED   [['APPROVED']]

## C. 反例①：复核人自己发起作废 → 422 + 65019（自审仍被拒）
  [PASS] 复核人 = 发起人 → 422 + 65019   422 code=65019
  [PASS] 报文点名复核岗位与配置路径   审批人 P1FIX1 复核人 是发起人本人，本节点不允许自审
         （self_approval=FORBID）（内控要求：作废复核人不能是发起人本人。本流程的复核岗位是
          「仓管主管（WAREHOUSE_MANAGER）」，而你本人就在这个岗位上：请…
  [PASS] 被拒后单据仍是 POSTED（整笔回滚）   POSTED

## D. 反例②：复核岗位 0 人 → 422 + 65018，报文给配置指引（不兜底超管）
  [PASS] 角色 0 人 → 422 + 65018   422 code=65018
  [PASS] 报文说清「没有候选人 + 去哪儿配」   节点「作废复核」没有任何可用审批人（来源 ROLE 解析出 0 人；
         该节点配置了 empty_assignee=ERROR，不会兜底给管理员）。请到「流程定义 → 本流程 → 节点
         「作废复核」」把审批人设为具体角色或人员，并确认该角色下有在职成员（角色管理 → 成员）…
  [PASS] 没有偷偷兜底给超管（没有派给 admin）
  [PASS] 把复核人补回岗位后同一张单再发起 → 200   200 ok
  [PASS] 补人后复核通过 → CANCELLED   200 ok

=== 汇总：PASS=20 FAIL=0 ===
```

> 「截图」：本卡的验收介质是 **HTTP 请求 + 库内实读**（脚本每次断言都打印状态码、单据状态、
> 六本账数值、待办归属）。浏览器截图需人工登录隔离实例，本卡未做（见 §7 诚实缺口）。

状态机日志（同一实例，作废回调原文）：

```
INFO cloudpivot::business::erp::stockin_handler_cancel: 收货单作废：库存/库位账已反向冲减 + 反向流水已写
     + 序列号已退回 + 红字凭证已生成 + 应付单已作废 stock_in_id=01a1aa48-… in_no=IN-20261105-000006
     lines=1 reversed_qty=1.0000 reversed_amount=10.0000 serials_voided=0 red_voucher_id=946b87a5-…
INFO cloudpivot::business::erp::stockin_bridge: 作废审批结论已回调收货单 instance_id=01a1aa48-… verdict="CANCEL"
```

---

## 5. 迁移演练（up → down → up → up 幂等）

`migrate_drill.txt`：

```
== ① 现状（V0248 已应用）==
  节点: ROLE/ERROR/FORBID   绑定: WAREHOUSE_MANAGER   hash: 0267661a…
  [PASS] 节点口径 / [PASS] hash = 026766…
== ② down（回滚到 V0174 种子口径）==
  NOTICE: V0248 down：作废复核已回滚为 ADMIN + FALLBACK_ADMIN，hash 026766… → 78d34c…
  节点: ADMIN/FALLBACK_ADMIN/FORBID   绑定: (空)   hash: 78d34c45…
  [PASS] 已回滚为 ADMIN/FALLBACK_ADMIN/FORBID / [PASS] 审批人绑定已清空
== ③ 再 up（幂等重放）==
  NOTICE: V0248：…已改为 WAREHOUSE_MANAGER（empty_assignee=ERROR），hash 78d34c… → 026766…
  [PASS] 重新应用后口径正确 / [PASS] hash 回到 026766…
== ④ 再 up 一次（幂等）==
  NOTICE: V0248：已是仓管主管复核口径（hash=026766…），跳过
```

空库首启日志（真机启动，190 个迁移一次性应用）：

```
NOTICE: V0248：stock_in_cancel 作废复核已改为 WAREHOUSE_MANAGER（empty_assignee=ERROR），hash 78d34c… → 026766…
NOTICE: V0248 自检通过：作废复核 = WAREHOUSE_MANAGER 角色 + ERROR 兜底 + 禁止自审，快照与 hash 自洽
```

---

## 6. 单测（隔离库 `cloudpivot_p1fix1_b`，`ERP_DB_URL` 显式注入 —— 不设就是静默 SKIP 假绿）

日志原样放在本目录：`p1fix1_test_stockin_r1.log` / `p1fix1_test_stockin_r2.log` /
`p1fix1_test_voucher_r1.log` / `p1fix1_clippy.log` / `p1fix1_test_p16_final.log`。

| target | 结果 |
|---|---|
| `business::erp::stockin_handler::`（第 1 轮） | **135 passed / 0 failed**（424.63s） |
| `business::erp::stockin_handler::`（第 2 轮，连跑） | **135 passed / 0 failed**（426.79s） |
| `business::erp::voucher::`（作废红冲受影响面） | **21 passed / 0 failed**（34.39s） |
| `p16_` + `cancel_cascade`（最终树，clippy 修完后重跑） | 见 `p1fix1_test_p16_final.log` |
| `cargo clippy -- -D warnings` / `cargo check` | **exit 0 / exit 0**（`CLIPPY_EXIT=0` `CHECK_EXIT=0`） |
| `cargo fmt -p cloudpivot -- --check` | 本卡改的文件 **0 diff**；剩余 diff 全部来自**同期别的卡**在改的 `voucher.rs` / `voucher_write.rs`（P0-FIX-4A 在途），不是本卡引入（`git diff --stat` 可复核） |

新增/改写的用例：

| 用例 | 断言 |
|---|---|
| `p16_submit_moves_to_pending_cancel_with_instance`（改） | 待办数 = 复核岗位在职成员数；归属人 = `WAREHOUSE_MANAGER` 成员（不再是「每个超管一条」） |
| `p16_self_approval_is_forbidden`（改写） | **复核岗位成员本人发起** → 422 + 65019；报文含 `WAREHOUSE_MANAGER` + 「仓管主管」；单据仍 POSTED、无审批单、账不动 |
| `p16_seed_flow_is_self_consistent`（改） | 快照 ↔ hash 自洽；节点 = ROLE + **ERROR** + FORBID；绑定角色码 = `WAREHOUSE_MANAGER` |
| `p16_graph_snapshot_selfcheck_is_clean`（新增） | `flow::maintain::run_once()` 实跑：全库 PUBLISHED 版本 **0 漂移**（行 → hash 重算） |

夹具改动：新增 `setup_cancel_reviewer()`（只留本模块复核人 `p16_reviewer`，与 `setup_post_approval`
同一「角色成员是共享资源」范式），`cancel_request()` 自动调用；`voucher.rs` 的作废 helper 同样先绑复核人。

---

## 7. 影响面与诚实缺口

**行为变化（只在作废审批这条链上）**：

| 场景 | 变化 |
|---|---|
| 超管/普通员工发起作废，复核岗位有人在岗 | **改**：派给 `WAREHOUSE_MANAGER` 成员（原来派给全部超管），发起人不再必然撞 422 |
| 复核岗位 0 人 | **改**：`422 + 65018`（原来兜底给超管，单超管环境等于必 422 + 65019） |
| 发起人本人就在复核岗位 | **不变**：`422 + 65019`（内控不许自审，本卡一个字没放宽） |
| 审批入口硬闸 `guard_self_approval`（403 + 65019） | **不变** |
| 其它 4 个 `ADMIN` 节点（payable_pay ×3 / stock_transfer） | **不变**（B 卡 `t_1e6a8431` 的 5 节点统一去 ADMIN 化仍待做） |

**诚实缺口**：

1. **截图未做**：验收用 HTTP + 库内实读（脚本输出在 §4）。需要在浏览器里截图的话，要在隔离实例上
   用 `p1fix1_maker` / `p1fix1_reviewer` 登录操作（口令哈希抄 admin），本卡没做这一步。
2. **生产未部署**：本卡只在本机隔离库验证。生产 144 上线要走 `scripts/deploy/deploy.sh`（迁移随启动自动应用），
   上线后 WAREHOUSE_MANAGER 现有 2 名成员（henry.liao / laoliu_test），作废复核会派给他们。
3. **`p9_cancel_e2e.py` 未重跑**：脚本里的复核人已改成 `WAREHOUSE_MANAGER` 成员（否则新配置下必 422），
   但它依赖夜间回归专用的库与夹具，本次只在真机脚本（§4）里覆盖了同一条链路。
4. **`05_chains_run1/2` 未重跑**：那套 AUDIT-ALL 链路脚本跑的是整条 6 条链路（要独立 audit 库 + 6 个夹具账号），
   本次只覆盖作废这一段（本卡范围）。
