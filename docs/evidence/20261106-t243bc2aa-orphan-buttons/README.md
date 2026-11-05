# t_243bc2aa「孤儿按钮」证据包（2026-11-06）

卡面：2 个角色持有 `business:flow:approve` 但父页 `business:flow:read` 未授（侧边栏看不见）
⇒ 决策 + 版本化迁移 + 常驻闸防复发。

## 结论一句话

**不补授 `business:flow:read`**：这 5 个实例侧按钮（`…0800`–`…0804`）本来就属于「发起审批」
「审批中心」两个页面，是 V0124 写它们时那两个页面还不存在（V0128 才种）留下的父级错挂；
本卡用 `V0259` 把父页归位（**生产零授权变化**：`sys_role_menu` 一行不动，只改 5 行
`sys_menu.parent_id`），并新增常驻闸 `scripts/verify/orphan_button_gate.py`（`check.sh` 闸⑦）。

## 文件清单

| 文件 | 内容 |
|---|---|
| `01_gate_selftest.txt` | 闸的门自检（SQL 谓词保留 + 分档 + verdict 红/绿语料，12/12 通过） |
| `02_prod_readonly_dryrun.txt` | **生产只读预演**：5 行父页 UPDATE、兜底补授 0 对（= 零新授权）、归位后孤儿 0 对 |
| `03_prodshape_before_after.txt` | 生产形状演练：克隆装配库 → 造「生产形状」→ 闸红 8 对 → 只跑 V0259 up → 绿 0 对，且 `role_menu_rows` 不变、两个角色仍无 `flow:read` |
| `04_down_up_roundtrip.txt` | 装配库 down/up 往返：`role_menu_rows 688 → 688`、`menu_rows 336`、父页 0800→0780 再→0861…、down/up 各自幂等 |
| `05_check_sh_gate7_paths.txt` | `check.sh` 闸⑦ 四条路径（无库口令跳过 / 装配库绿 / 生产形状红 / warn 不阻断），函数代码直接从 check.sh 抽出执行 |
| `06_prod_gate_before_deploy.txt` | 生产当前（V0259 部署前）跑闸 = 红 8 对（点名到 (角色,码,不可达父页)） |
| `07_assembly_verify.txt` | 装配库（190 版本化 + V0259 + 10 种子 + bootstrap）：迁移记账 checksum 一致、5 个父页正确、闸 exit 0 |
| `08_engine_replay_zero_migrations.txt` | 同一库再起一次引擎：`应用 0 个迁移`（指纹校验通过、V0259 不重跑） |

## 决策依据（三条实测，不是偏好）

1. **按钮真正长在哪个页面**：`frontend/src/views/business/flow/task/index.vue:345`
   `hasPermission('business:flow:approve')` —— 审批按钮在「审批中心」（`/flow/task`，
   页码 `business:flow:instance:read`）；后端是 `POST /business/flow/task/{id}/approve|reject|
   transfer|add-sign|batch`（`handler.rs:1106-1111`）。**不在**「流程定义」页。
2. **两个角色的职责不含流程定义**：生产实测 `FLOW_CONFIG_APPROVER` 只持 4 个 flow 码
   （approve / instance:create / instance:read / instance:withdraw），零个定义侧码；
   它是「流程配置复核员」（V0206 种角色 / V0247 记数据范围 / `docs/manual/02-审批流.md`：
   状态机边配置变更的复核岗），复核单子走审批中心。补授 `flow:read` = 给它们开一页
   从不需要的定义页 = 本不该有的新增可见面。
3. **根因是时序**：`V0124`（…0800–…0804）< `V0128`（种 …0861 发起审批 / …0862 审批中心），
   当时只能挂在唯一存在的流程页面 …0780 下；V0128 建页面后没回头改父级。
   `V0239` 文件头第 ② 条的本意也是「instance:create/read/withdraw 与 发起审批/审批中心 配套授予」。

## 生产口径：**零变更**（本卡未对生产做任何写操作）

生产只读预演（`02`）：新迁移会 **UPDATE 5 行 `sys_menu.parent_id`**、
**INSERT 0 行 `sys_role_menu`** ⇒ 没有任何 (角色,码) 新增授权，也没有撤销。
生产上的 8 对孤儿在 `V0259` 随下次发布生效后归零（`03` 在克隆库上已按同一路径验证）。
本次运行期间生产数据一个字没动（只有 `select`）。

## 判据为什么不是卡面那条 SQL 的字面版（三处实测定下来的必要的偏差）

卡面 SQL 是「按钮节点 → 父页的码没授」的逐节点判断。落到**常驻闸**上会误伤两类合法形态，
而且会漏掉「同码挂多页」的真实可达性，所以闸的判据是：

- **(角色,码) 粒度 + 祖先链可达性**：该码的**每个**按钮节点向上走祖先链，都有带码祖先未授 ⇒ 孤儿。
  只报「逐节点不可达」会把「同码另有可达节点」的冗余授权也算成缺陷
  （实测：装配库 `WAREHOUSE_MANAGER × business:erp:doc:cancel-{cascade,impact}` ——
  同码挂在 /purchase、/stock-in、/payable 三页，该角色只有前两页的码；V0258 按码补授权时
  把「该码全部节点」一起写进去，这是**设计如此**，码已拿到，功能无损）。
  这类只进 `redundant_node_grants` 报告，不判红。
- **`SUPER_ADMIN` 不参与判红**：`system/auth/model.rs::is_super_admin` 按 role_code 短路，
  令牌里是 `*:*:*` ⇒ 它的 `sys_role_menu` 行对功能零影响（V0258 矩阵同口径）。
  装配库里 `S__voucher_three_state_permissions` 给超管授了 3 个凭证按钮而不授 `voucher:list`
  —— 那不是缺陷。
- **软删角色不参与判红**（登录不了），只报告。

## 已知缺口（诚实记录）

- **装配库上「修前」不可复现**：全新库里 `S__workflow_permissions.sql` 会给所有角色授
  `business:flow:read`（含这两个角色），所以新库里本来就没有这 8 对孤儿 —— 缺陷只在
  「生产形状」（角色没有 `flow:read`）下出现。因此 `03` 是**克隆装配库 + 撤掉这两个角色的
  `flow:read`** 来复现生产形状，而不是拿全新库直接跑。
- **生产未部署**：本卡不动生产（见上）。`06` 记录的是部署前的红，部署后应转绿；
  转绿验证由发布流程的 `scripts/deploy/deploy.sh` + 部署后跑一次
  `python3 scripts/verify/orphan_button_gate.py --remote root@172.18.10.144 --db cloudpivot` 完成。
- **`V0259.down` 的已知代价**：公式依赖「该角色仍持有这 5 个按钮之一」，若 up 之后有人撤销了
  某角色的按钮，down 重算不出它的补授行会留残行（生产/装配库 up 补授 0 行，无此问题）。
  `sys_role_menu` 没有来源列，这与 V0239 / V0258 down 同病。
- **没有真人可见性 e2e**：归位不改变任何角色的码集合（`03` 已用行数证明），
  所以「登录后按钮/菜单变化」的前后对照在设计上就不存在 —— 不给一个假的因果证据。
