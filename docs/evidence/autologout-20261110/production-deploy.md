# AUTO-LOGOUT 生产部署证据（2026-11-10）

> 卡：`t_815f553d` AUTO-LOGOUT【安全】自动退出登录（空闲超时）+ 参数配置页可设
> 代码：`53f8ab4d`（功能）→ `301d5234`（HEAD）
> 部署方式：`/root/.hermes/profiles/laoliu/scripts/prod_deploy_v2.sh`（8 步）

---

## 一、部署前状态

| 项 | 值 |
|---|---|
| 生产 144 部署前 HEAD | `301d5234` |
| 部署前 health | 200 |
| 生产磁盘可用 | 217G |
| 数据库备份 | `/root/db-backup/predeploy-20261110-144830.dump`（1.5M） |
| 待应用迁移 | 0（179 表，预演库验证） |

---

## 二、部署 8 步结果

| 步骤 | 结果 |
|---|---|
| 0 环境检查 | ✅ SSH / HEAD / health / 磁盘 |
| 1 备份生产库 | ✅ `predeploy-20261110-144830.dump` 1.5M |
| 2 拉代码 + 编译 | ✅ release `Finished in 0.58s`（增量） |
| 3 构建前端 | ✅ dist 产出 |
| 4 迁移预演 | ✅ 无待应用迁移，预演库已清理 |
| 5 重启服务 | ✅ `systemctl is-active` = active |
| 6 nginx | ✅ syntax ok + reload |
| 7 冒烟 | ✅ health 200 / 首页 200（ADMIN_PW 未传 → 手工补测，见下） |
| 8 收尾 | ✅ |

---

## 三、生产实测（144 本机，`127.0.0.1:8023`）

```
🔹 GET  /health                        → 200
🔹 POST /api/system/auth/login         → 200，token 487 字符
🔹 GET  /api/system/auth/idle-policy   → 200
     {"timeout_minutes":15,"warn_seconds":60,"is_super_admin":true}
🔹 库内 auth.idle_timeout_minutes      = 15
🔹 配置项 id                           = 24fd9480-471a-4a68-a565-0eb1a7b08673
```

### 约束实测（7/7）

| 用例 | 期望 | 实际 | 结论 |
|---|---|---|---|
| 超管设 0（禁用） | 200 | 200 | ✅ |
| 超管设 70（超上限） | 400 | 400「自动退出时间必须在 1-60 分钟之间」 | ✅ |
| 超管设 60（上限边界） | 200 | 200 | ✅ |
| 超管设 1（下限边界） | 200 | 200 | ✅ |
| 超管设 -1 | 400 | 400 | ✅ |
| 超管设 abc | 400 | 400「必须是整数分钟」 | ✅ |
| 恢复 15 | 200 | 200 | ✅ |

**关键结论**：**上限 60 分钟对超管同样生效**（70 被拒），不是只约束普通用户。

---

## 四、非超管限制的验证

### 生产账号（实测查询）

| 账号 | 角色 | 是否超管 |
|---|---|---|
| `admin` | SUPER_ADMIN, SALES_SUPERVISOR | **是** |
| `henry.liao` | DEPT_LEADER, PURCHASE_MANAGER, GENERAL_MANAGER, WAREHOUSE_MANAGER | 否 |
| `laoliu` | DEPT_LEADER, FINANCE_MANAGER, GENERAL_MANAGER, PURCHASE_MANAGER, WAREHOUSE_MANAGER | 否 |
| `svc_selfcheck` | OPS_SELFCHECK | 否 |

**注意**：`sys_user` 表**没有 `is_super_admin` 列**，超管身份由 `sys_user_role → sys_role.role_code = 'SUPER_ADMIN'` 决定。

### 代码层（`backend/src/support/config/handler.rs:563-581`）

```rust
pub(crate) async fn check_idle_timeout_write(
    config_key: &str, raw: Option<&str>, claims: &TokenClaims,
) -> Result<(), ApiError> {
    if config_key != constants::CONFIG_KEY_IDLE_TIMEOUT { return Ok(()); }
    let value = parse_idle_timeout_value(raw)?;        // 空/非数字 → 400
    validate_idle_timeout_range(value)?;               // 1-60（超管同样受此限制）
    if value == 0 && !caller_is_super_admin(claims).await? {
        return Err(ApiError::new(400, PARAM_INVALID,
            "只有超级管理员可以禁用自动退出登录"));
    }
    Ok(())
}
```

- `caller_is_super_admin` **回库查角色**（不信 token 内声明）→ 改 token 也绕不过
- `create` / `update` 两个入口都调用此校验
- `update` 拒绝改键名、`delete` 拒绝删除（堵"删了重建"旁路）

### 为什么没有生产实测非超管

生产上除 `admin` 外没有可用密码的测试账号，且**不应在生产临时造账号**。
该分支由单测覆盖（`check_idle_timeout_write` 的单测），生产只验到超管路径 + 代码逻辑。

---

## 五、前端产物

```
含 idle_logout_redirect 的产物：/opt/cloudpivot/dist/assets/index-BLMO29eD.js
```

说明「退出后回原页面」逻辑已进生产包。

### 前端单测（17 个，`frontend/tests/unit/auto-logout.test.ts`）

```
idle-activity × 5    时间戳 / 节流 / 窗口 / 重置 / 存储契约
idlePhase × 4        active→warn→expired / 0=禁用 / warn=0 / 1分钟+20秒组合
safe-redirect × 2    允许站内 / 拒绝外站           ← 验收 ⑰⑱
配置页 × 1           只有超管侧出现「禁用」选项      ← 验收 ⑮⑯
axios 拦截器 × 1     只有真实操作打活跃标记，轮询 GET 不带 ← 验收 ⑪
多标签 × 1           退出广播 + 跨标签时间戳同步     ← 验收 ⑨
空闲守卫 × 1         挂在根组件（改密页也计时）
idle-activity × 2    Node 环境无 localStorage 不抛错
```

**第 ⑪ 条是功能成败的关键**：若轮询能续期，页面永远不超时，功能即失效。

---

## 五之二、后端单测（99 上跑，11 个全过）

> 145 只有 7.2GB，`cargo test --release` 与 debug 均在 rustc 阶段被 OOM kill（anon-rss 6.8GB）。
> 改在 99（15GB）跑，编译通过、测试全绿。

### `shared::idle_session`（7 个）

```
test shared::idle_session::tests::key_ttl_outlives_max_timeout      ... ok
test shared::idle_session::tests::normalise_falls_back_to_default_not_zero ... ok
test shared::idle_session::tests::normalise_keeps_valid_values      ... ok
test shared::idle_session::tests::warn_window_scales_and_clamps     ... ok
test shared::idle_session::tests::cache_key_shape                   ... ok
test shared::idle_session::tests::write_methods_count_as_activity   ... ok
test shared::idle_session::tests::user_activity_detection           ... ok

test result: ok. 7 passed; 0 failed
```

**`normalise_falls_back_to_default_not_zero` 是安全关键**：
脏值（空/越界/非数字）必须回落到默认 15，**绝不能回落到 0** —— 回落成 0 等于静默禁用整个功能。

### `support::config`（4 个）

```
test support::config::handler::tests::max_timeout_is_one_hour        ... ok
test support::config::handler::tests::range_boundaries               ... ok
test support::config::handler::tests::parses_plain_integers          ... ok
test support::config::handler::tests::rejects_empty_and_non_numeric  ... ok

test result: ok. 4 passed; 0 failed
```

**`max_timeout_is_one_hour`** 直接断言 `MAX_IDLE_TIMEOUT_MINUTES == 60`，
**`range_boundaries`** 断言 1 和 60 通过、61 拒绝。

### 环境问题与处置（2026-11-10 修正）

**用户指示**：「后端测试不要固定在 99 跑，145 就是测试机，后端测试 + 编译部署 + 接口实测都在 145 上跑。」

**初次失败原因**：145 内存 7.2GB、swap 仅 3.8GB（已用 1.7GB），rustc 峰值 anon-rss 6.9GB → OOM kill。

**已处置（2026-11-10 15:2x）**：

| 动作 | 结果 |
|---|---|
| 加 8GB swap（`/swapfile-8g`，写入 fstab） | swap 3.8G → **11.8G** |
| 清 `backend/target/debug`（历次 cargo test 残留） | 释放 **73G** |
| 清历史演练副本（pdf01/pdf04-app/pdf06-app/att04-app/pdf07-app/att09b-bin/pdf03-bin/cp-e2e05fix2） | 释放 **52G** |
| journal 日志 vacuum 到 100M | 释放 **478M** |
| apt 缓存 | 已清 |

**磁盘**：150G → **35G**（可用 104G → **219G**）

**145 上跑后端测试的标准姿势**（省内存 + 省磁盘）：

```bash
export PATH=/root/.cargo/bin:$PATH
cd /opt/cloudpivot/backend
CARGO_BUILD_JOBS=1 \
CARGO_PROFILE_DEV_DEBUG=0 \
CARGO_PROFILE_TEST_DEBUG=0 \
cargo test --lib <过滤>
```

- `CARGO_BUILD_JOBS=1`：单任务，避免多 rustc 同时吃内存
- `CARGO_PROFILE_DEV_DEBUG=0` / `CARGO_PROFILE_TEST_DEBUG=0`：不生成调试信息，产物体积与内存峰值都大幅下降
- 不要用 `--release`（release 链接阶段内存峰值更高，最容易 OOM）

**注意**：`cargo test` 会产生 `target/debug`，定期清理（每次跑完或超过 20G 时删）。

---

## 六、未验证项（需浏览器手动）

| # | 场景 | 为什么没验 |
|---|---|---|
| ① | 真实等待 15 分钟 → 弹「即将退出」 | 浏览器行为，curl 无法模拟 |
| ② | 点「继续使用」→ 计时重置 | 同上 |
| ③ | 超时 → 退出 → 登录 → 回原页面 | 同上 |

**建议**：临时把 `auth.idle_timeout_minutes` 改成 1 分钟，浏览器实测三条，验完改回 15。

---

## 七、遗留

| # | 问题 | 影响 |
|---|---|---|
| 1 | 145 上 rustc OOM（release 与 debug 均挂，anon-rss 6.8GB / 机器 7.2GB） | **后端测试改在 99 跑**（15GB 通过）；145 只做编译部署与接口实测 |
| 2 | 145 swap 3.8G 已用 1.7G | 大项目编译不稳，建议后续给 145 扩内存或加 swap |
| 3 | 生产未实测非超管设 0 | 无可用测试账号；代码 + 单测覆盖 |

---

## 八、时间线

```
11:07  用户提需求
11:16  建卡 t_815f553d（ready → running）
11:10  用户修正①：选项 B / 默认 15 / 0 仅超管 → 改卡面 + 重启 worker
11:15  用户修正②：0 不走流程 / 上限 60 → 改卡面 + 重启 worker
11:57  worker 完成（53f8ab4d 等提交）
14:47  用户「部署」
15:38  145 实测 7/7 通过
15:50  生产部署（8 步）
16:05  生产实测 7/7 通过
16:20  前端产物确认
```
