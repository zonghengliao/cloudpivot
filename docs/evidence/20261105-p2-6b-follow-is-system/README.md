# P2-6b-follow：两条新预置流程漏写 `is_system=TRUE`（budget_approve / asset_disposal）+ 种子静态检查

卡片 `t_a9071923`（父卡 `t_850c7eae` / P2-6b）。本文是**实跑证据**，不是设计说明。
所有 `.txt` / `.log` 都是脚本原始输出，命令写在文件头或下方「复现」一节。

## 1. 病是什么（以及为什么它不是 P2-6b 的 bug）

`biz_flow_def.is_system` 由 `V0193` 加列，定义是 `BOOLEAN NOT NULL DEFAULT FALSE`。
`V0228`（预算审批）、`V0231`（资产处置）的种子 `INSERT` 列清单止于 `owner_dept_id`，没有
`is_system` → 吃默认值 FALSE → 这两条**系统预置**流程被当成「客户自建」：

```
$ psql -d cloudpivot_p26c -c "select flow_code, flow_name, is_system from biz_flow_def where not is_system and deleted_at is null"
asset_disposal|资产处置审批（出售 / 报废 / 盘亏）|f
budget_approve|预算审批（编制 / 调整双分支）|f
```

后果是**静默**的：P2-6b 把 `GET /business/flow/def/templates` 收口为「标准流程」（`AND d.is_system`）后，
这两条从「流程模板库」里直接消失（`/def/page` 仍可见，数据没丢）—— 数据没坏，界面少两张卡，
最难被发现的那一类缺陷。这是 `V0193` 之后**第四次**同一个错法（V0209 / V0211 → V0220 补；
V0228 / V0231 → 本卡 V0233 补），所以本卡同时把「靠人记住」换成**可执行检查**。

## 2. 改了什么

| 文件 | 改动 |
| --- | --- |
| `backend/migrations/business/V0233__flow_seed_is_system_fix2.up.sql` | 按 `flow_code` 把 2 行 `is_system` 置 TRUE（幂等 `AND NOT is_system`）；内含 2 段 fail-closed 自检：①点名这 2 行必须已是 TRUE；②**预置编码白名单（13 个）里不许有 `is_system=FALSE` 的行**，否则 `RAISE EXCEPTION`（写法照 `V0220`） |
| `backend/migrations/business/V0233__flow_seed_is_system_fix2.down.sql` | 逆操作改回 FALSE（幂等 `AND is_system`），不动 `updated_at`（与 V0220 同口径） |
| `scripts/verify/erp_invariants.py` | 新增 **H6**（`--group 审批`）：静态扫 `backend/migrations/**` 里所有 `INSERT INTO biz_flow_def (...)` 的列清单，版本 ≥ `V0193` 缺 `is_system` 且无更晚迁移按 `flow_code` 补偿 → **BLOCKER**。不连库，也不依赖起名习惯 |
| `docs/后端二开指南.md` §九 | 把「四次实例 + H5/H6 各自管哪一半 + 号段已到 V0234」写准（原来只写了 V0209/V0211 和 H5） |

**没做**（按卡面「别做」）：没改 `V0228` / `V0231` 正文（内容冻结，指纹校验会红）；没动 P2-6b 的
`AND d.is_system` 口径（决策 A 不变，不加特例）；没动前端（P2-6c）。

### H6 的判定口径（四条，缺一不可）

1. 只扫 `INSERT INTO biz_flow_def (...)`，且**版本 ≥ V0193**（更早的迁移没有这一列，缺列不是错）；
2. 列清单里有 `is_system` → 通过；
3. 没有 → 允许**补偿**：存在更晚的迁移里 `UPDATE biz_flow_def` 语句同时含该 `flow_code` 字面量与
   `is_system = TRUE`（这正是「已应用迁移只能新开一条修」的机制，例如 V0220 补 V0209/V0211、
   V0233 补 V0228/V0231）。补偿不到 → 红。
4. **隐式列清单**（`INSERT INTO biz_flow_def SELECT ...`，INSERT 后没有括号列名）→ 直接红：
   静态拿不到列名，且位置依赖表列序，比漏列更脆；不让它从正则缝里溜过去。

选它而不是「按 `flow_code` 白名单查库」：白名单永远落后于下一个新种子（本卡就是被白名单漏掉的那一类），
而静态检查在**代码提交前**就能红，不依赖库状态、不依赖有人去起库。

## 3. 验收（逐条对卡面「要做 3 条」）

### ① 迁移应用后那 2 行变 `t` —— 真引擎跑，不是手改 SQL

用**真迁移引擎**（后端二进制启动时 `auto_migrate`）在 `cloudpivot_manual` 的 V0232 期副本
`cloudpivot_test_pflow5` 上先跑一遍升级路径（`02-migrate-pflow5.txt`）：

```
NOTICE: V0233：is_system 打标 2 行（budget_approve / asset_disposal），预置白名单 13 个编码全部为系统流程
INFO nik_web::app: 数据库迁移完成: 应用 2 个迁移
```

再在 `cloudpivot_manual` 上跑（`04-migrate-manual.txt`）：同样 `应用 2 个迁移` + 上面那条 NOTICE。
后状态（`11-post-state.txt`）：两个库都是

```
versioned|flow_seed_is_system_fix2|233|t|174
asset_disposal|t
budget_approve|t
13|13        ← 未软删 13 行，is_system=TRUE 13 行
```

> `应用 2 个迁移` 里的另一个是 `V0234`（P2-6d 的菜单收口，已入库但 `cloudpivot_manual` 之前只到 V0232）：
> 迁移器按版本升序把两笔待应用的都补上了。`cloudpivot_manual` 的 `sys_menu` 因此少了
> `path='/flow/templates'` 节点 —— 这是 P2-6d 已交付的内容，不是本卡引入的，此处记明以免误判。

**空库从零安装路径**另跑一遍（`12-fresh-install.txt`）：新建空库 `cloudpivot_a9071923f` → 启动即从零跑到
`应用 175 个迁移`，其中 `V0228` / `V0231` 先种下两条（false）、`V0233` 随后补标（同一条 NOTICE `打标 2 行`），
最终 `13|13`。说明「从零安装」与「V0232 升级」两条路径都成立，V0233 的排序位置不会让自检误炸。

**幂等**：同一实例再启动一次 → `应用 0 个迁移`，`applied_at` 与 `updated_at` 一字不变
（`03-idempotent-restart.txt`）。**down 可用**：在事务里执行两次 `V0233...down.sql` → 第一次
`UPDATE 2`、第二次 `UPDATE 0`（幂等），`ROLLBACK` 后库回到 `t`，状态零变化（`05-down-rollback-tx.txt`）。

### ② H12 从 11 变 13

`06-probe.txt`（`TEST_PASS='Manual@123456' python3 scripts/verify/p2_6_template_probe.py`，打真实例 8023）：

```
H12    OK       expect=13         actual=13           默认口径 total 必须 = 库里 is_system=TRUE 条数（N = 标准流程数）
H14    OK       expect=13         actual=13           scope=all 的 total 必须 = 库里未软删定义数
D3     OK       expect=None       actual=13           其中 is_system=TRUE
D12    OK       expect=None       actual=234          库里已应用的最大迁移号
-- 断言 41 条，FAIL=0
```

`asset_disposal` / `budget_approve` 现在也出现在 H0 卡片明细里（`is_system=True status=PUBLISHED cat=财务管理`）。

### ③ `erp_invariants.py --group 审批` 绿

`07-invariants-shenpi.txt`：`结果：6/6 通过   BLOCKER 0   WARN 0`，H6 与 H5 同时绿。
全量（`08-invariants-all.txt`）：`52/52 通过   BLOCKER 0   WARN 0`（原 51 条 + 新 H6）。

### ④ H6 自己是检查器，必须证明它**会红**

`09-h6-selftest.txt`（`bash docs/evidence/20261105-p2-6b-follow-is-system/h6_selftest.sh`）：

| 夹具 | 期望 | 实得 |
| --- | --- | --- |
| 仓库真迁移 | 绿 + 列出 4 处历史漏列已被补偿 | `ok=True`，`V0193+ 共 5 处 INSERT 列清单全部含 is_system；历史遗留 4 处已由后续迁移补偿：…contract_approve → V0220；stock_take_post → V0220；budget_approve → V0233；asset_disposal → V0233` |
| 假种子漏写 `is_system` 且无人补偿 | BLOCKER | `ok=False`，指向 `V0240__bad_seed.up.sql:2 flow_code=fake_seed_flow` |
| 假种子写全 + 一条 `<V0193` 老迁移漏写 | 绿（老迁移不算错） | `ok=True`，`共 1 处 INSERT 列清单全部含 is_system` |
| 隐式列清单（`INSERT INTO biz_flow_def SELECT ...`） | BLOCKER | `ok=False`，`V0241__implicit_seed.up.sql:2 列清单隐式…无法静态确认写了 is_system` |

`10-h6-dbless.txt` 另证「不依赖库」：把库指到 `no_such_db`，H6 仍 ✅（其余断言因连不上库红），
这正是把它放在 `group_h()` 最前面的理由。

## 4. 复现

```bash
cd /opt/cloudpivot

# 迁移：用真引擎（二进制启动即 auto_migrate；迁移文件在磁盘上，无需重编译）
#   8023 用的那把 env 见 /proc/<pid>/environ；隔离跑法见本卡执行记录（8099/8098 端口）
./backend/target/debug/cloudpivot   # NIK_DATABASE_NAME=<目标库> 等

# 断言
NIK_DATABASE_USER=alun NIK_DATABASE_NAME=cloudpivot_manual NIK_DATABASE_PASSWORD=alun \
  python3 scripts/verify/erp_invariants.py --group 审批
TEST_PASS='Manual@123456' python3 scripts/verify/p2_6_template_probe.py
bash docs/evidence/20261105-p2-6b-follow-is-system/h6_selftest.sh
```

## 5. 副作用 / 残留

- `cloudpivot_manual`：补跑 `V0233` + `V0234`（见上文说明），`biz_flow_def` 13 行全为系统流程；
  无新建表、无删数据。8023 实例未重启（迁移是数据变更，运行中的实例读库即生效，探针已证）。
- 隔离库 `cloudpivot_test_pflow5`：同日已被本卡升级到 V0234（它本是父卡建的同版本副本）。
- 备用库 `cloudpivot_p26c`（P2-6c 遗留，V0232）**未动**，留作「前状态」快照，`01-pre-state.txt` 即取自它。
- `cloudpivot_a9071923`（本卡早期试探库）：未用于结论，库内两行曾被手工置 TRUE —— 本卡所有结论均取自
  迁移引擎真跑的结果，不依赖这个库。
- 新建 `cloudpivot_a9071923f`（空库从零安装证据用），迁移跑完后实例已在端口 8099 停掉；库留着可复查
  `12-fresh-install.txt` 的结果（本机 `alun` 无 CREATEDB，建库用 `su postgres -c createdb`）。

## 6. 已知局限（诚实说明）

- 未测「down 迁移由引擎自动执行」：`nik-db` 的 CLI 只暴露 `--gen-config` / `--print-config`，没有
  `migrate down` 子命令；down 的**语义**已在事务内验证过（`05-down-rollback-tx.txt`：2 行变更 + 幂等），
  但「引擎按 down 文件回滚一版」这条路径本卡没有入口可跑。
- H6 是**文本级**检查：它只认 `INSERT INTO biz_flow_def (...)` 这种「INSERT 紧跟括号列清单」的写法。
  隐式列（`INSERT INTO biz_flow_def SELECT ...`）会**红**而不是静默放过（见夹具 implicit）；但种子若用
  变量拼 SQL、或把列清单写到 `${}` 占位符里，H6 仍可能看不见 —— 真正兜底的是 V0233 体内的白名单自检
  （库内、fail-closed：预置编码里出现 `is_system=FALSE` 就让启动失败）。
