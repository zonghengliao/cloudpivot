# HR-26 验收证据 —— 迁移 V0393 `hr_leave` 流程种子

- 卡：`t_10899fe4`（HR-26【HR】迁移 V0384 → 实际 **V0393**）
- 设计依据：`docs/architecture/ARCH-9-人力资源模块设计.md` §3.2 ⑨ / §4「自审批」/ §7.1 / §8.1
- 范本：`V0381__cost_flow_def`（四重守卫 + 快照 hash 库内重算）
- 演练库：**145（172.18.10.145）**，跑完已 DROP

## 一、交付验收（卡面 3 条）

| 验收项 | 落地证据 |
|---|---|
| **流程 PUBLISHED** | `biz_flow_def.status='PUBLISHED'` + `is_default` + `is_system` + `allow_auto_approve=FALSE`；版本 v1 `PUBLISHED / is_current`，`node_count=5 / edge_count=5`。见 `01_drill.out` S1 |
| **四重守卫** | ①同名 `flow_code` ②同 `biz_type` 默认流 ③`HR_MANAGER` 角色缺失 ④版本重复（`change_note LIKE 'V0393 种子%'` + 同 `graph_hash`）。四条**逐条反证**：S7 / S8 / S9 / S10 |
| **节点绑角色不是全员会签** | 两个审批节点 `approval_mode='ANY'`（显式落库，不靠 NULL），`empty_assignee='FALLBACK_ADMIN'`，`timeout_action='REMIND'`；自检断言「`APPROVAL` 节点里 `ALL` / NULL 必须 0 个」。`n_hr` 绑角色 `HR_MANAGER`（`…f0016`）一行；`n_sup` 用 `INITIATOR_SUPERIOR`（levels=1）**不落枚举行**（落行等于把「谁的上级」写死） |

## 二、图形（5 节点 / 5 边）

```
s(开始) ──► n_sup(直属上级审批, INITIATOR_SUPERIOR/ANY) ──► c_days(请假天数判定 ≥3 天)
                                                              ├─ e_long(condition: form.duration_days_num ≥ 3) ──► n_hr(人事经理复核, ROLE=HR_MANAGER/ANY) ──┐
                                                              └─ e_short(默认/兜底: < 3 天) ─────────────────────────────────────────────────────────────► e(结束) ◄─┘
```

`graph_hash = sha256(canonical(snapshot)) = 4fc6722abf629a8a9a84580942dd2f6a0a44431786b2e500905531affc0e1f97`

稳定性：快照 `nodes` 按 `key` 升序、`edges` 按 `(from,to,key)` 升序（与 `graph::snapshot()` 的确定性排序一致），
否则「行重算 hash」与「快照 hash」会分叉，`flow::maintain` 每日自检报 drift（静态测试第 11 条钉死）。

## 三、★ 关键实施发现：条件字段必须是**数值镜像**

`leave_handler::leave_form_data`（HR-25 落地）把 `duration_days` 冻结成 **JSON 字符串**（`"1.5"`，展示口径）。
而条件求值器 `flow::graph::compare` 对 `gte/gt/lt/lte` 走 `Value::as_f64()`：

- 字符串左值 ⇒ `EvalError::TypeMismatch` ⇒ 引擎 `pick_edge` 按「条件不成立」**静默走兜底边**
  （只有一行 `tracing::warn!`，无错误码、无界面提示）；
- 后果：**请假 ≥3 天永远不派人事经理复核**，长假被直属上级一签直过 —— 静默绕过复核，不是「配错会报错」。

处理（本卡最小改动，跨卡但同模块）：`leave_form_data` **保留** `duration_days`（字符串，出参不变）
**并**新增 `duration_days_num`（JSON number，同源同值），种子条件边只认 `form.duration_days_num`。
两侧各一条钉子：

- `hr::leave_handler` 单测 `form_data_freezes_human_readable_fields` + `duration_days_num_mirrors_the_decimal_at_the_three_day_boundary`（2.5 / 3.0 / 3.5 边界，断言是 **number**）
- `tests/hr_26_leave_flow_seed.rs::condition_field_is_a_number_produced_by_the_handler`（源码级对拍）
- `tests/hr_26_leave_flow_seed_db.rs::long_leave_condition_routes_at_the_three_day_boundary`（真库图 + 引擎求值：0.5/2.5/2.9 → false，3.0/3.5/30 → true；只给字符串时求值报 `Missing`，反证为什么要镜像）

## 四、实测输出（真实运行，见同目录文件）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_drill.sh` / `01_drill.out` | 145 演练：312 条基线迁移 → up → up 幂等 → hash 库内重算 → down → down 幂等 → 重放 → 四重守卫逐条反证 → down 安全阀 c | **ALL PASS（0 失败）**，演练库已 DROP |
| `02_seed_db.sh` | 建「全量迁移 + V0393」真库供 Rust 集成测试（313 条） | 应用 313 条，`hr_leave def=1`，hash 相符 |
| `03_static_tests.out` | `cargo test --test hr_26_leave_flow_seed` | **13 passed / 0 failed** |
| `04_db_tests.out` | `cargo test --test hr_26_leave_flow_seed_db`（ERP_DB_URL 指向 145 隧道） | **4 passed / 0 failed**（含 `graph.validate()` 0 问题、hash 三方对账、3 天边界分流） |
| `05_leave_lib_tests.out` | `cargo test --lib business::hr::leave` | **54 passed / 0 failed**（HR-25 的 53 + 本卡新增 1） |

另：`cargo check --all-targets` 零警告（隔离 worktree `/root/wt/hr26`，HEAD + 本卡 6 个文件）。

## 五、动到的文件

```
backend/migrations/business/V0393__hr_leave_flow.up.sql     (新，~510 行：定义+版本+图+节点边+自检)
backend/migrations/business/V0393__hr_leave_flow.down.sql   (新，三道安全阀 + 自底向上删)
backend/tests/hr_26_leave_flow_seed.rs                      (新，13 条静态不变量，含 hash 重算)
backend/tests/hr_26_leave_flow_seed_db.rs                   (新，4 条真库用例)
backend/src/business/hr/leave_handler.rs                    (+1 数值镜像 duration_days_num + 2 条单测)
docs/architecture/ARCH-9-人力资源模块设计.md                (§3.2 ⑨ 落地记录 / §7.1 第三次号段修正 / §8.1 HR-26/35/38/45 号段)
docs/evidence/2026-11-12-hr26-leave-flow-v0393/*            (本目录)
```

## 六、号段与遗留（报出去，不自己动）

1. **号段三连后移**：设计 V0384 → 映射 V0390 → 实落 **V0393**（V0390/0391/0392 被成本菜单分组 / 成本流程 / HR-23 实占）。
   已在设计文档 §7.1 记「第三次号段修正」，并把批 3 剩余（HR-38 V0394/V0395）与批 5（HR-35 V0396、HR-45 V0397）顺延。
   每卡落地前仍须 `ls backend/migrations/*/ | grep -oE 'V[0-9]{4}' | sort -u | tail -3` 复核。
2. **`flow::handler::TEMPLATE_CATEGORY_ORDER` 只认 4 类**（采购管理 / 仓库管理 / 财务管理 / 任务），
   本流程 `category='人力资源'` 会排在模板库末尾。属 `business/flow/` 模块，**本卡未动**，建议后续卡在
   该常量里补「人力资源」。
3. **HR-27（leave_bridge）依赖本流程**：`n_sup` 的 `INITIATOR_SUPERIOR` 依赖发起人部门有 `leader_id`；
   解析不到时 `FALLBACK_ADMIN` 兜底（不是 SKIP）。自审批拦截（员工不能批自己的假）按设计 §4 在桥侧做，
   本卡只在两个节点上保留 `self_approval='ALLOW'`（写 FORBID 会让员工自提的单派发不出来 = 死单）。
4. **残留清理**：145 上 `hr26_leave_flow_drill` / `hr26_seed_db` 均已 DROP（`残留 hr26 库=0`），
   SSH 隧道已关，隔离 worktree 已 remove。
