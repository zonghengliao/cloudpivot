# P3-05 证据：ALLOW 自审的配套约束（审计日志 + 金额阈值 + 月度报表）

卡：`t_c56a694e` · 2026-11-08 · backend-developer
隔离实例：`127.0.0.1:18305`（库 `cloudpivot_p305_ut`）/ 迁移验证实例 `127.0.0.1:18306`（库 `cloudpivot_p305_mig`）
二进制快照：`/root/p305/bin/cloudpivot`（本卡提交时的 debug 构建）
环境文件：`/root/p305/migrate.env`（600，含三把密钥 + admin 口令，**不落仓库**）

---

## 一、卡面要什么 → 本卡交付什么

| 卡面要求 | 交付 | 证据 |
|---|---|---|
| 金额阈值（系统参数，默认 0 = 不限额） | `erp.separation_of_duties.self_approval_max_amount`（`NUMBER`），V0338 种进 `sys_config` | `raw/10_upgrade.txt` §参数行 |
| 超阈值 → 不许自审、降级 SKIP 走 P3-02，留痕写明「超出自审金额上限 X」 | `assignee.rs::demote_over_limit`（③.6 步，在委托之后、升级链之前） | `raw/20_e2e_scenarios.txt` E2 |
| 都取不到金额 → 视为 0 不拦 + debug | `self_approval::amount_of` 返回 `None` → 不拦（`tracing::debug!`） | 单测 `missing_amount_never_demotes` |
| 新错误码 65044 + 唯一性断言表 | `error_code::FLOW_SELF_APPROVAL_AMOUNT_LIMIT`，65xxx 段 38 → **39** | `raw/30_checks.txt` §单测（`flow_error_codes_are_stable`） |
| 审计日志：ALLOW 自审落 `operation_logs`（两处） | 引擎落 PENDING（`FLOW_SELF_APPROVAL_ALLOWED`）+ `vote()` 提交（`FLOW_SELF_APPROVAL_VOTED`），同一事务内同步 INSERT | `raw/20_e2e_scenarios.txt` E1 审计留痕 |
| 月度报表 `GET /business/flow/stats/self-approval` | `self_approval::monthly_report` + `handler::stats_self_approval`（含 `month` / `biz_type`） | E1 / E2 / E7 三处 JSON 原文 |
| 报表 SQL 带 `SCOPE-EXEMPT` 或数据范围谓词 | 走 `instance_visibility()`（与列表 / 其余统计同一份口径）+ 文件头 SCOPE-EXEMPT 登记 | `raw/30_checks.txt` §闸② |
| 前端 ① 流程统计页自审监控 tab | **新建** `views/business/flow/stats/index.vue`（自审监控 + 审批总览两个 Tab）+ 路由 `/flow/stats` + 菜单（V0338 …1243） | 见「三、前端」 |
| 前端 ② 待办列表补「自审」tag | `task/index.vue` 卡片加 `自审` tag（`TaskRes.is_self_approval`） | 见「三、前端」 |
| 前端 ③ 参数页可配新键 | 参数页按 `sys_config` 表自动渲染（`category=erp_internal_control` 分组已存在）→ **无需改前端** | `raw/10_upgrade.txt` §参数行（`NUMBER` / 可修改 / 启用） |
| 单测 5 条 | 8 条（自审模块）+ 7 条（assignee 金额上限）+ 1 条（`parse_amount`）+ 码表 | `raw/30_checks.txt` §单测 |

---

## 二、四个决策（以及为什么这么定）

### D3 错误码 65044：**只在「降级后仍无人可签」时报**

卡面写「新错误码 65044（自审金额超限）」，同时写「超阈值 → 降级 SKIP，走 P3-02 升级链」。
两条合起来的唯一自洽实现是：

```
金额 > 上限 → 发起人从「能签的人」挪到 SKIPPED（留痕 + 审计）
            → 升级链（P3-02）→ 兜底人（P3-03）→ 超管兜底（FALLBACK_ADMIN）
            → 还是没人可签 → 422 + 65044（报文点名金额 / 上限 / 配置键 / 降级链）
            → 有人可签 → 正常派给他，**不报错**（只是留痕「你被降级了」）
```

**没有选「一超限就 422」**：一人公司里发起人往往就是唯一审批人，直接 422 等于把单子彻底堵死，
而升级链 / 兜底人本来就是为这种场景准备的（E2 实测：降级后 B 拿到待办、单据正常通过）。
**也没有选「静默自动通过」**：`empty_assignee=SKIP` 会让节点在 0 人时自动通过 ——
大额单据没人签就生效，比自审更坏。所以 `fail_closed_on_amount_limit` 会**先清掉 `auto_pass`**
再报 65044（单测 `empty_after_demotion_fails_closed_with_65044` 钉住）。

### D4 迁移号：V0338（核对后的实际最大号）

开工时卡面写 V0336（且要求先核对）。核对结果：V0336 已被 P3-04（委托）占用、V0337 已被 CODE-01（主数据序列）占用
→ 顺延 **V0338**。迁移里**不建表**（原因见下一条），只做三件事：参数行 / 菜单行 / 授权行，
外加顺手修一个既有的同父 `sort_order` 撞车（见「四、顺手修的东西」）。

### 拦截计数**不建表**（卡面「若计数表要建，走 V0336，否则明确写本次不做拦截计数」）

拦下的每一次都落 `operation_logs`（`FLOW_SELF_APPROVAL_BLOCKED`，`target_id` = 实例 id），
报表**直接数日志**。理由：多一张计数表就多一份「与真实留痕不一致」的可能，而日志是和业务动作
**同一事务**写的，天然同真同假。

口径差别（写进了响应的 `blocked_note`，前端直接展示）：`blocked_count` 只统计**已生效**的降级
（实例还在，能按数据范围归属）；被 65044 整单拒绝的那次因为事务回滚不留行，只在**报错报文 + 服务端日志**里留痕。
本卡**明确不做**「被拒绝次数」的计数。

### 月度窗口：北京时间自然月，左闭右开

`Window::parse(month)` 把 `YYYY-MM` 切成 `[当月 1 日 00:00+08:00, 次月 1 日 00:00+08:00)`。
用 +08:00 而不是 UTC：这是给财务 / 内控看的自然月，与会计期间同口径；用 UTC 会把北京时间
每月最后 8 小时的自审算到下个月。单测 `window_parses_beijing_month_boundaries` 钉住三个边界
（1 日 00:00 含、月末 23:59:59 含、次月 1 日 00:00 不含），DB 单测再用真数据验一遍。

---

## 三、前端（这一块比卡面描述的大一点，说明原因）

卡面写「流程统计页加自审监控 tab」，但**实测前端根本没有流程统计页**：
`grep -rn "stats/overview\|stats/efficiency\|node-bottleneck" frontend/src/` = **0 命中**，
三个统计接口从 D2 起就没有任何调用方。所以本卡把页面建起来了：

- 新建 `frontend/src/views/business/flow/stats/index.vue`
  - Tab 1「自审监控」：月份选择器 + 业务类型过滤 + 四张卡（自审单数 / 金额合计 / 占同期已结算 / 超上限被降级）
    + 按节点 Top10 + 按人 Top10 + 明细表（可点单据号跳详情）+ 两条口径说明（`ratio_note` / `blocked_note`，
    由**后端**下发，页面不自己算占比）；
  - Tab 2「审批总览」：复用既有的 `stats/overview` + `stats/efficiency`（首次切入该 Tab 才请求）。
- 路由 `/flow/stats`（`frontend/src/router/index.ts`），权限复用 `business:flow:instance:read`
  —— 与统计三接口同口径，**不新造** `business:flow:stats:read` 那行死码。
- 菜单：V0338 种 `…1243`（`parent=/flow`，`sort=50`），授权给所有持有 `business:flow:instance:read` 的角色。
- `task/index.vue`：待办卡片补 `自审` tag（`variant=light/theme=danger`），数据源 `TaskRes.is_self_approval`
  —— 与 `FlowProgress.vue` / `instance/detail.vue` 同源，补的是**一致性缺口**（卡面点名）。
- 参数页：`sys_config` 自动渲染（`raw/10_upgrade.txt` 证明新键是 `NUMBER` / 可修改 / 启用 / 分类 `erp_internal_control`），
  **没有改前端代码**。

金额一律按**字符串**展示（后端 `numeric::text`）：`formatMoney()` 只对 `-?digits(.digits)?` 补两位小数，
其余原样输出 —— 不走 `Number().toFixed()`，那会把大额金额的四舍五入悄悄改掉。

---

## 四、顺手修的东西（以及边界）

**审批流目录下同父 `sort_order` 撞车**：全新库首启实测 `/flow/linkage`（联动规则）与
`/flow/delegate`（审批委托，V0336 种的 …1240）**都是 40**。V0336 是上一张卡（P3-04）落的，
当时没核兄弟行 —— 撞车不报错，只让侧栏顺序取决于 id，属于 MENU-FIX 明令禁止的形态。
V0338 里带守卫地把它挪到 45（`sort=40 且确有同父同 sort 的另一行` 才动，客户自己排过序的库整段跳过），
并在迁移自检里断言「审批流目录下同父 sort 无重复」。修后实测顺序：
`发起审批 20 → 审批中心 30 → 联动规则 40 → 审批委托 45 → 自审监控 50 → 流程定义 68`。

**没有动**：`system/`、`support/` 零改动（`raw/30_checks.txt` §7）；
`erp/separation.rs` 只加「自审金额上限」的读取函数（沿用该文件既有的快照读法：`SysConfigModel::read_text`，
60s TTL + 写后广播）；`flow/` 之外一行未碰。

---

## 五、验收逐条对照

| 卡面验收 | 结果 | 证据位置 |
|---|---|---|
| 迁移应用成功，重启「应用 0 个迁移」 | ✅ 19 断言全过（空库首启 271 条迁移含 V0338 / 重启 0 / down 回滚 / 删登记重放 1 个） | `raw/10_upgrade.txt` |
| 一人公司 → 自审通过 → `operation_logs` 1 行可读 → 报表件数 1 / 金额正确 / 占比 100% | ✅（实际写了 **2** 行：ALLOWED + VOTED，卡面要的两处接入点都在） | `raw/20_e2e_scenarios.txt` E1 |
| 超阈值 5000 > 1000 → 自审被拦，报文/留痕写明阈值 | ✅ 两种形态都验：**有第二人** → 降级 + SKIPPED 留痕 + B 拿到待办（E2）；**只有一人** → 422 + 65044，报文含 5000.00 / 1000.00 / 配置键（E3） | E2 / E3 |
| 回归：SKIP 仍走升级链、FORBID 仍 65019 | ✅ SKIP → 422 + **65018**（报文含「升级链」）；FORBID → 422 + **65019** | E5 / E6 |
| `cargo fmt` + `clippy --lib -D warnings`（仅基线）+ 单测全绿 | ✅ 见 `raw/30_checks.txt` | 30_checks |
| `pnpm typecheck` exit 0 | ✅ exit 0（默认堆会被 OOM 杀掉，脚本里显式 `--max-old-space-size=8192`） | 30_checks §4 |
| 证据文档（报表 JSON 原文 + operation_logs 行） | ✅ 本文件 + `raw/` | — |
| 只按路径 `git add` | ✅ 提交时逐路径 add（共享工作区，`git status` 里还有别的 worker 的在途改动） | `raw/40_git.txt` |

---

## 六、原始输出索引

| 文件 | 内容 |
|---|---|
| `raw/10_upgrade.txt` | 迁移 V0338：空库首启 / 参数行 / 菜单行与授权 / 重启 0 个迁移 / down 回滚 / 删登记重放 1 个 |
| `raw/20_e2e_scenarios.txt` | 真 HTTP 七个场景（E1 一人公司 / E2 降级 / E3 65044 / E4 小额不拦 / E5 SKIP 回归 / E6 FORBID 回归 / E7 分母口径 + 非法月份），68 断言 |
| `raw/30_checks.txt` | fmt / clippy 基线 / 单测（自审 8 + assignee 7 + parse_amount + 码表）/ typecheck / 闸⑥ 死码 / 闸② 作用域 / system+support 零改动 |
| `raw/40_git.txt` | `git status` + 本次提交的路径清单 + commit |

脚本（可复跑）：`scripts/lib.sh` / `scripts/jget.py` / `scripts/10_upgrade_migration.sh` /
`scripts/20_e2e_scenarios.sh` / `scripts/30_checks.sh`。

复跑前注意：`export PGPASSWORD=<本机测试库口令>`；`/root/p305/migrate.env` 与
`/root/p305/bin/cloudpivot` 需要存在（见本文件头）。

---

## 七、已知边界 / 不做的事

1. **金额只从 `biz_flow_instance.form_data` 取**（`amount` → `total_amount` → 无 = 0 不拦）：
   卡面明确「不许为这个去查业务单据表」。代价：业务单把金额放在别的键名（比如 `price`）时本阈值不生效。
2. **`blocked_count` 不含「被 65044 整单拒绝」的次数**（事务回滚无行可数），口径已随响应下发。
3. **报表分子按「自审票被通过的时刻」落月，分母按「实例结算（ended_at）」落月**：跨月结算时
   `ratio` 可能暂时 > 1.0，`ratio_note` 里写明了这一点，前端只展示不解释。
4. 前端**没有做 e2e 截图证据**：本卡前端是「新增一页 + 一个 tag」，`pnpm typecheck` exit 0；
   页面接口联调靠 E1 的 JSON 原文（后端产出）保证。若要视觉验收，起 dev server 点开
   `/flow/stats` 即可（需要 admin 口令，本仓库不留）。
5. 阈值参数**非法值回退 0（不限额）**并 `warn`：阈值只在 ALLOW 节点上起作用，
   回退默认值不会把任何本来被拦的动作变成放行（与布尔开关「解析失败当 false = 关内控」方向相反）。
