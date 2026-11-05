# t_227464dd · 新装配库权限矩阵 —— 实测证据

决策文档：`docs/architecture/P2-新装配库权限矩阵-决策.md`
迁移：`backend/migrations/business/V0258__business_role_matrix.{up,down}.sql`
常驻闸：`scripts/verify/role_matrix_gate.py`

环境：
- 装配库 A `cloudpivot_t2274c`：189 版本化 + 10 种子 + bootstrap，**不含 V0258**（补前基线）
- 装配库 B `cloudpivot_t2274b`：同 A，但 V0258 已由**启动流水线自动应用**（补后）
- 后端：`NIK_PROFILE=test`，`127.0.0.1:18228/18229`，库 `127.0.0.1:5432/cloudpivot_t2274b`
- 生产：`172.18.10.144 / cloudpivot`（**只读**，本卡未写一行）

| 文件 | 内容 |
|---|---|
| `01_gate_prod_zero_change.txt` | 生产只读预演：450 对矩阵 → **缺授权对 0 / 缺码 0 / 缺角色 0** ⇒ 执行 V0258 零变更；5 码持有者与修改前逐行一致 |
| `02_gate_装配库_补前.txt` | 不含 V0258 的装配库：缺授权 **265 对**，4 个验收码 `<none>`，`voucher:post` 只有 SUPER_ADMIN |
| `03_gate_装配库_补后.txt` | 同一装配库应用 V0258 后：缺授权 **0 对**，5 码落到正确角色 |
| `04_idempotent_down_up_roundtrip.txt` | up 重放 0 行 → down 删 367 行 → down 重放 0 行 → 再 up 恢复 688 行 |
| `05_装配库_验收查询.txt` | 5 码持有角色 / 逐角色码数 / 催办节点 …1212 持有者 / 治理页越界（0 行） |
| `06_e2e_催办端到端.txt` | 真 HTTP：fin1 权限码 123 个含 period:remind；超管催办 `sent=1, skipped_no_owner=0`；台账 `VOUCHER / PERIOD_REMIND / 2026-11 / rc=1` |
| `07_prod_minus_fresh_pairs.json` | 生产 − 新库 的缺码明细（按角色分组，265 对） |
| `08_业务页覆盖_补前.txt` / `09_业务页覆盖_补后.txt` | 5 角色并集业务页覆盖 **15/52 → 48/52**；逐角色码数变化 |
| `10_装配基线计数.txt` | 补前基线：189 versioned + 10 seed、`sys_role_menu` 343 行 |
| `11_扫描_27条空跑授权迁移.txt` | 27 条带 `n_roles = 0` 保护的迁移 × 授予码数 × 引用的角色码（暴露 `ADMIN/ACCOUNTANT/FINANCE/...` 在生产全部不存在） |

## 复跑方式

```bash
# 1) 装配库自检（补前/补后/生产）：exit 0=OK, 1=缺授权, 2=矩阵失效
python3 scripts/verify/role_matrix_gate.py \
  --migration backend/migrations/business/V0258__business_role_matrix.up.sql \
  --dsn postgres://alun:alun@127.0.0.1:5432/cloudpivot_t2274b

# 2) 生产只读预演（不改一行）
python3 scripts/verify/role_matrix_gate.py \
  --migration backend/migrations/business/V0258__business_role_matrix.up.sql \
  --remote root@172.18.10.144 --db cloudpivot

# 3) 幂等 / 回滚往返
psql "$DSN" -f backend/migrations/business/V0258__business_role_matrix.down.sql
psql "$DSN" -f backend/migrations/business/V0258__business_role_matrix.up.sql
```

## 诚实清单

- 生产**没有执行** V0258（本卡只读）。「零变更」是**预演证明**（缺授权对 = 0 ⇒ 选中 0 行），
  下次 `deploy.sh` 按集合差应用时会实际执行；届时可用 `01` 的命令复核。
- `down` 之后 `sys_role_menu` 是 321 行（不是 343）：V0258 早于种子执行，抢先插入了 22 行
  本应由种子插入的 (role,menu)（种子因 `UNIQUE (role_id,menu_id)` 跳过）。再跑一次 `up`
  即恢复 —— 已写进 down 文件头与 §6 回滚章节。
- E2E 里 `skipped_recent=1`：2026-11 有两个挡路项指向同一张凭证（`total=2`），第一张发出、
  第二张被 24h 台账判据拦住 —— 正是父卡 t_593fba6c 交付的防重语义，不是异常。
