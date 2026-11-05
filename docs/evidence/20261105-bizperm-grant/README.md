# 证据：业务角色权限覆盖施工（P0-FIX-2B-C 跟进 / `t_34cee9ea`）

> 决策与矩阵：`docs/architecture/P0-FIX-2B-C-业务角色权限覆盖-决策.md`
> 脚本：`scripts/datafix/bizperm/`（`plan.sql` = 授予矩阵唯一真相源）
> 执行时间：2026-11-05 17:07–17:10 CST（生产 `cloudpivot` @ 172.18.10.144）
> 施工账号：`architect`（数据修复脚本，非人工点界面）

## 一句话结论

5 个业务角色按职能归位补齐 **51/59** 业务页（原 12/59），新增 **376 行**授权 + **3 条**凭证按钮码；
`laoliu`/`henry.liao`/`laoliu_test` 对 `task/page`、`project/page`、`purchase-order/page`、`contract/page`
从 **403 → 200**；`data_scope` 一行未改（`laoliu_test` 任务仍是 0 行）；`SUPER_ADMIN` 仍 = 1；
`erp_invariants` 66/66 BLOCKER 0 WARN 0；回滚往返实测可逆。

## 文件清单

### `prod/`（生产实测）

| 文件 | 内容 | 关键数字 |
|---|---|---|
| `10_check_before.txt` | 施工前现状（角色/账号/行数） | `laoliu` 5 角色、`laoliu_test` 3 角色、超管 1 |
| `11_proof_before.txt` | 施工前对平巡检（`proof_rows.sql`） | 覆盖 **12/59**，未覆盖 47 |
| `12_probe_before.txt` | 施工前四账号探针 | admin 0 FAIL；laoliu 26 FAIL / henry 18 / laoliu_test 16（全是 403） |
| `20_snapshot.txt` | 阶段 0 快照 | sys_role 5 / role_menu 153 / menu 207 / sys_user 3 行 |
| `21_grant.txt` | 阶段 1 施工 | 新建按钮码 **3** 条 / 新增授权 **376** 行 / token_version 3 个账号 |
| `22_cache.txt` | 缓存失效 | epoch `3 → 4`；12 个 `uv/perm` 键 DEL 全 1 |
| `23_probe_after.txt` | 施工后四账号探针（含 JSON 明细） | **FAIL = 0**（26+41+32+32 项） |
| `24_proof_after.txt` | 施工后对平巡检 | 覆盖 **51/59**；治理页泄漏 0；孤儿页 0；超管 1 |
| `25_health.txt` | `GET /health` | **200** |
| `26_invariants_after.txt` | `erp_invariants.py` | **66/66，BLOCKER 0，WARN 0** |
| `27_rollback_drill_prod.txt` | 生产**事务内**回滚演练 | down → 600/326/59；`ROLLBACK` → 976/329/135（零净变更） |
| `28_rollback_refine.txt` | 生产**真回退**（矩阵微调后重做） | 删 376 行 + 3 菜单，回到 600/326 |

### `iso/`（隔离副本，prod 17:05 dump 还原到本机 `bizperm_drill_3`）

| 文件 | 内容 | 结果 |
|---|---|---|
| `01_four_steps.txt` | SOP §9 四步往返：up → 幂等重放 → down → 再 up | **FAIL=0** |
| `02_proof_iso_after.txt` | 副本施工后对平巡检 | 51/59，与生产一致 |

## 关键数字（施工前 → 后）

| 账号 | 权限码 | 可见业务页 | `task/page` | `project/page` | `voucher/page` |
|---|---|---|---|---|---|
| `admin` | 通配 | 72 | 200 / 71 行 | 200 / 3 行 | 200 / 21 |
| `laoliu` | 49 → 195 | 13 → 54 | 403 → **200 / 71 行** | 403 → **200 / 3 行** | 403 → **200** |
| `henry.liao` | 49 → 175 | 13 → 54 | 403 → **200 / 71 行** | 403 → **200 / 3 行** | 403 → **200** |
| `laoliu_test` | 49 → 151 | 13 → 44 | 403 → **200 / 0 行** | 403 → **200 / 0 行** | 403（无财务角色） |

角色授权行数：`DEPT_LEADER 48→95`、`FINANCE_MANAGER 59→135`、`GENERAL_MANAGER 13→119`、
`PURCHASE_MANAGER 13→88`、`WAREHOUSE_MANAGER 20→92`。

## 复跑方法

```bash
ssh 172.18.10.144
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
cd /opt/cloudpivot/scripts/datafix/bizperm
bash apply.sh --check                       # 现状（应显示施工后的行数）
python3 probe_bizperm.py --all              # 四账号探针（应 FAIL=0）
PGPASSWORD="$NIK_DATABASE_PASSWORD" psql -h 127.0.0.1 -U cloudpivot -d cloudpivot -f proof_rows.sql
cd /root/superconv-run2 && python3 erp_invariants.py | tail -3   # 66/66
```

## 已知偏差（详见决策文档 §7）

1. ERP 单据表（采购/入库/凭证/库存/物料）是 `ScopeTarget::unscoped`（`erp/period_items.rs:75-79` 有
   `SCOPE-EXEMPT` 登记）⇒ 3 档账号看到的是**全量单据**（14 张采购单 / 14 张入库单），不是本部门单据。
2. `biz_task` / `biz_project` 活跃 71 / 3 行、`dept_id` 全 NULL ⇒ 3 档账号补码后仍是 0 行（D-3 预期）。
3. 导入导出 3 页 + 平台治理 8 页**有意不授**（决策文档 §4.3）。
