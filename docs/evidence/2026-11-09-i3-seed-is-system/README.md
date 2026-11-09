# E2E-14 评审 I-3：种子补 `is_system`（H6/H5）+ 超管落位（N2）

> 卡：`t_810a3b70`（REVIEW-FIX-2【I-3】）
> 上游问题：`docs/review/20261109-系统全面评审.md` —— 干净库 80 项不变量里唯一的真实 BLOCKER
> 采集口径：145 测试机（`/opt/cloudpivot`，HEAD `3193fdbe`）+ **全新空库** `cloudpivot_i3_clean`
> 未连 144；未 DROP/TRUNCATE 他人数据（只 `RECREATE` 本卡自建的 `cloudpivot_i3_clean`）；未改已应用迁移

## 一、缺陷是什么（复现）

`erp_invariants.py` 的两条独立判据指向同一处漏列：

| 判据 | 形态 | 干净库（`cloudpivot_fresh`，评审当日） |
|---|---|---|
| **H6**（静态，BLOCKER） | 扫 `migrations/` 里 `INSERT INTO biz_flow_def (…)` 的列清单，没有 `is_system`，也没有更晚的迁移按 flow_code 补偿 | 红：`V0331:239 flow_code=pr_approve`、`V0332:269 flow_code=prt_approve` |
| **H5**（库内，WARN） | `flow_name LIKE '%标准%' AND NOT is_system` | 干净库当日停在 V0320、这两条还没建，故未红；**145 `cloudpivot_dev` 实测红**（两条都在、都是 false） |
| **N2**（库内，WARN） | 在用 HUMAN 账号 `dept_id IS NULL` | 红：`admin` |

业务后果（不是报错，是**静默少卡**）：P2-6b 把模板库默认口径收口成 `AND d.is_system` 之后，
这两条预置流程在「标准流程」列表里根本不出现，且以「客户自建」身份露出、客户可以删图。

## 二、改了什么

| 文件 | 改动 |
|---|---|
| `backend/migrations/business/V0366__seed_fix_flow_is_system_and_admin_dept.up.sql` | ① 按**种子固定 UUID**（`…d0331` / `…e0331`）+ `flow_code` 双向守卫补 `is_system=TRUE`；② 组织树**全空**时种根部门「总部」（`dept_code='HQ'`）。幂等 + 自检 |
| `…/V0366__seed_fix_flow_is_system_and_admin_dept.down.sql` | 回退：按同一对 UUID 撤 `is_system`、只删本迁移种的那一行 HQ |
| `backend/src/system/dept/model.rs` | 新增 `land_users_into_dept_by_code`：只认 `dept_code`、只补 `dept_id IS NULL` |
| `backend/src/bootstrap.rs` | `seed_super_admin` 在 `ensure_admin_user` 之后把超管落位到 HQ |
| `backend/src/shared/constants.rs` | `ROOT_DEPT_CODE = "HQ"` |
| `backend/src/system/dept/tests.rs` | D12：只补 NULL / 不覆盖 / 幂等 / 缺编码零变更 |

### 为什么 N2 不能在迁移里修

`admin` 不是迁移建的，是 `bootstrap::seed_super_admin` 在**启动期**建的。干净库实测：
V0366 执行时 `sys_user` 里没有 admin 这一行，`UPDATE … username='admin'` 命中 **0 行**，
不变量照样 WARN。落位只能放到启动期（幂等，每次启动重试）。

### 为什么按 UUID 而不是只按 flow_code

V0193 已立口径：种子 INSERT 条件是 `WHERE NOT EXISTS (flow_code = …)`，客户若先建了同编码流程，
种子整条跳过、那一行是**客户自己建的**；按编码打标会给客户的流程挂上「标准」标签，设计意图反了。
所以选择键是种子固定 UUID，`flow_code IN (…)` 只作双向守卫（顺带满足 H6 的静态补偿判据）。

## 三、验收证据

### ① 全新空库跑全量迁移（`10_clean_db_invariants.txt`）

```
created: cloudpivot_i3_clean (owner=cloudpivot)
schema_migrations(max|count) = 366|297        ← V0366 已落库
pr_approve  | 采购申请审批（标准·超预算加签） | is_system=true
prt_approve | 采购退货审批（标准·两级）      | is_system=true
标准流程数 / 名字含标准但非系统(H5) = 18 / 0
dept: HQ | 总部 | parent=NULL
admin dept = HQ        ← 启动期落位（日志：超管部门落位 users=["admin"] dept="HQ"）
N2 违规账号 = (无)
[迁移后]  结果：80/80 通过   🚨 BLOCKER 0   ⚠️ WARN 0     ← 评审当日是 78/80（1 BLOCKER + 1 WARN）
```

幂等：第二遍跑 V0366 → `UPDATE 0` / `INSERT 0 0`。
因果对照：把两处改回缺陷态 → `78/80，WARN 2（H5 + N2）`；再跑 V0366 → 回到 `80/80`。

### ② 真 token curl（`20_curl_evidence.txt`，隔离实例 :18039 连干净库）

```
health=200   login code=0 token_len=461   must_change=True → 改密 → relogin code=0
/auth/me   → dept_id=20000000-0000-4000-8000-0000000f0366   roles=['SUPER_ADMIN']
/dept/tree → HQ
/business/flow/def/templates（默认口径 = 标准流程）：
  A 修复态：total=18，含 pr_approve / prt_approve（is_system=True）
  B 改回 false（= 修复前）：total=16，两条都消失     ← 「静默少卡 2 张」的现场
  C 复原：total=18，两条都回来
```

## 四、怎么复跑

```bash
# ① 干净库 + 不变量（会 RECREATE cloudpivot_i3_clean）
bash docs/evidence/2026-11-09-i3-seed-is-system/run_10_clean_db.sh
# ② 真 token curl（起 :18039 隔离实例，跑完自停）
bash docs/evidence/2026-11-09-i3-seed-is-system/run_20_curl.sh
# ③ H6 静态检查（不连库，只读 migrations）
python3 docs/evidence/2026-11-09-i3-seed-is-system/h6_static_check.py
```

## 五、已知边界（诚实说明）

- **H6 是静态判据**：它校验「种子 INSERT 写没写 is_system / 有没有更晚迁移补偿」。把库里两行改回
  `false` 不会让 H6 变红（只有 H5 红）—— 上面因果对照里 H6 保持绿就是这个原因，不是漏测。
- **N2 只在「有 HQ 或组织树为空」的地方被修好**：`admin` 落位只认 `dept_code='HQ'`；客户自己
  组织树里没有 HQ 的环境一行都不动（与 `scripts/datafix/org_landing`「绝不猜、绝不造部门」同口径），
  这种环境要人按自己的组织落位。
- 145 的 `cloudpivot_dev`（演练库）本卡**未改**：它没有 HQ，超管仍无部门；那里的 20 项红是演练数据
  造出来的，与本卡无关。
