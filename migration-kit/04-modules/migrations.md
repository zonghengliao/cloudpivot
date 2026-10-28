# 迁移号段与撞号处理（04-modules/migrations.md）

## 1. 编号体系现状

后端用目录化的迁移器（`backend/config/config.toml` → `[database.migration] path = "migrations"`，
`auto_migrate = true`），三个命名空间各自独立编号：

```
backend/migrations/
├── system/    V0001..V0006（底座：用户/角色/菜单/部门/关联表）+ ★V0022（我们加的会话安全字段）
├── support/   V0007..V0017（字典/配置/文件/日志/任务/审计/安全/改密日志）
├── business/  ★V0020（我们加的服务器状态权限点）
└── seed/      S__*.sql（种子：菜单/角色/字典/配置）
```

我们新增了 **2 个**迁移：

| 迁移 | 内容 | 幂等手段 | 引入提交 |
|------|------|----------|----------|
| `business/V0020__add_biz_monitor_permission.{up,down}.sql` | `sys_menu` 权限点 `business:monitor:server-status` + SUPER_ADMIN 授权 | 固定 UUID + `ON CONFLICT DO NOTHING` | `55d3c5d` |
| `system/V0022__add_session_security_flags.{up,down}.sql` | `sys_user.must_change_password` / `token_version` | `ADD COLUMN IF NOT EXISTS` | `2140b82` |

> 为什么 `system` 那边跳过 V0021：V0022 是**和另一条「令牌吊销补全（V0021）」同批设计**的，
> 两条都要给 `sys_user` 加同名列，写成了「先执行者建列、后执行者空转」。
> 本仓库只有 V0022；**若 Pro 里带 V0021，V0022 仍然安全**（`IF NOT EXISTS`）。

## 2. 撞号：Pro 也占了 V0020~V0029 怎么办

判断是否撞号：

```bash
# Pro 树里 business / system 各自的迁移号
ls pro/backend/migrations/system/V*.sql | sed 's/.*V\([0-9]*\)__.*/\1/' | sort -n | tail -5
ls pro/backend/migrations/business/V*.sql 2>/dev/null | sed 's/.*V\([0-9]*\)__.*/\1/' | sort -n | tail -5
```

**核心原则：按 namespace 独立看号**，`business` 撞了不影响 `system`。
撞了就重编号，选一个「新老库都不冲突」的号段（推荐直接跳到最高号之后，如 V0100 起）：

### 重编号步骤

```bash
# 以 business/V0020 → business/V0100 为例
cd backend/migrations/business
git mv V0020__add_biz_monitor_permission.up.sql   V0100__add_biz_monitor_permission.up.sql
git mv V0020__add_biz_monitor_permission.down.sql V0100__add_biz_monitor_permission.down.sql
```

**必须一起改的地方**（漏一处就会出现「迁移跑了但权限点没生效」或「回滚回错了」）：

1. **迁移文件名本身**（上面 `git mv`）。
2. **迁移器版本表**：已执行过旧版本的库，`_sqlx_migrations` / `alun_migrations`（按框架实际表名）
   里记的是旧号。生产库若已执行 V0020，重编号后会被当成「新迁移」再跑一次 ——
   靠 `ON CONFLICT DO NOTHING` 兜住不会报错，但**版本记录会乱**。规范做法：
   ```sql
   -- 只在「旧号已执行过、新号要与旧号等价」时执行
   UPDATE alun_migrations SET version = 100 WHERE version = 20;
   ```
3. **`sys_menu` 种子里的引用**：`business/V0020` 里 `parent_id`/`id` 是固定 UUID，与迁移号无关；
   但**如果种子里按「迁移号」或「菜单名」做关联，就要同步**。本仓库的种子文件是
   `seed/S__menu_data.sql`（父菜单 `10000000-...-050` = 工作台）与 `seed/S__role_data.sql`，
   V0020 的 UUID 是自洽的 —— **重编号不需要改种子**。
   若 Pro 改成「按 sort_order 或 name 关联」，则必须同步改 `S__menu_data.sql`。
4. **文档/契约里的编号引用**：`grep -rn "V0020\|V0022" docs/ backend/ | grep -v migrations/`，
   把提到编号的地方一起改（本仓库命中：`docs/二开-business-monitor-接口契约.md`、`docs/线上产物记录-*.md`）。

### 撞号的另一种处理：不重编号，改「功能码前缀」

如果 Pro 的 V0020 不是给 `business` 命名空间的，其实根本没撞 —— 先确认 namespace。
真正的冲突只发生在**同一 namespace 同一个号**。

## 3. 重编号后如何验证迁移可重复执行（up → down → up）

需要一个**可丢弃的**测试库（绝不碰 `cloudpivot` / `cloudpivot_test` / `cloudpivot_dev`）：

```bash
# 1) 建临时库
sudo -u postgres psql -c "DROP DATABASE IF EXISTS cloudpivot_mig_check;"
sudo -u postgres psql -c "CREATE DATABASE cloudpivot_mig_check OWNER cloudpivot;"

# 2) 用临时库跑一遍服务（启动即 auto_migrate），确认 up 成功
ALUN_DATABASE_NAME=cloudpivot_mig_check \
ALUN_DATABASE_PASSWORD=... \
ALUN_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  ./backend/target/release/cloudpivot 2>&1 | grep -i migrat
#    期望：无 error；进程能起（/health 200）

# 3) 确认权限点落库
psql -d cloudpivot_mig_check -c \
 "SELECT name, auth_code FROM sys_menu WHERE auth_code='business:monitor:server-status';"
#    期望 1 行（重编号后 auth_code 不变）
psql -d cloudpivot_mig_check -c \
 "SELECT count(*) FROM sys_role_menu rm JOIN sys_role r ON r.id=rm.role_id WHERE r.role_code='SUPER_ADMIN';"

# 4) down：手工执行 .down.sql（迁移器一般不自动回滚）
psql -d cloudpivot_mig_check -f backend/migrations/business/V0100__add_biz_monitor_permission.down.sql
psql -d cloudpivot_mig_check -c "SELECT count(*) FROM sys_menu WHERE auth_code='business:monitor:server-status';"
#    期望 0

# 5) 再 up 一次（幂等性）
psql -d cloudpivot_mig_check -f backend/migrations/business/V0100__add_biz_monitor_permission.up.sql
#    期望：无错、仍 1 行；再执行一次也不报错（ON CONFLICT DO NOTHING）

# 6) system/V0022 同理（ADD COLUMN IF NOT EXISTS → 重复执行安全）
psql -d cloudpivot_mig_check -f backend/migrations/system/V0022__add_session_security_flags.up.sql
psql -d cloudpivot_mig_check -f backend/migrations/system/V0022__add_session_security_flags.up.sql   # 第二次应无错
psql -d cloudpivot_mig_check -c "\d sys_user" | grep -E "must_change_password|token_version"

# 7) 收尾：删临时库
sudo -u postgres psql -c "DROP DATABASE cloudpivot_mig_check;"
```

> ⚠ `V0022.down.sql` 会 `DROP COLUMN must_change_password, token_version`。
> 如果同批次还有别的迁移依赖这两列（Pro 可能把 V0021 也做在这两列上），
> **回滚顺序必须是「先回滚依赖方、再回滚 V0022」**，否则 down 会把人家要用的列删掉。
> `V0022.down.sql` 的注释里已经写了这个提醒。

## 4. 与 Pro 合并时的检查清单

- [ ] 两个 namespace 的号段都不冲突（或有明确的「谁改号」决定，并且写进 commit message）
- [ ] 已执行过旧号的库，版本表已按第 2 节更新（或者确认靠幂等兜住）
- [ ] `sys_menu` 种子的父菜单 UUID 在 Pro 里仍然存在（`10000000-...-050` = 工作台）
- [ ] `business:monitor:server-status` 这个 auth_code 在 Pro 里没有被占用（`grep -r` 一遍）
- [ ] up → down → up 演练通过（第 3 节脚本）
- [ ] `06-verify/verify.sh` 的 F3「迁移 up/down 成对」仍然通过
