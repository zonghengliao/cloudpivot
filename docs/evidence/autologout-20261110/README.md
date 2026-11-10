# AUTO-LOGOUT（空闲自动退出登录）验收证据 · 2026-11-10

卡：`t_815f553d`　提交：`e0b7f8e5`

## 环境

| 项 | 值 |
|---|---|
| 机器 | `172.18.10.145`（ubuntu-svr，dev 环境） |
| 二进制 | `backend/target/debug/cloudpivot`，commit **e0b7f8e5**（`git merge --ff-only origin/main` 后 `cargo build`） |
| 端口 | 隔离实例 `127.0.0.1:8036`（同库；主服务 8033 未重启，仍是旧二进制） |
| 库 | `cloudpivot-dev.env` 里的 `NIK_DATABASE_*`（与 8033 同一个库） |
| 脚本 | `scripts/verify/verify_idle_timeout.py`（本目录 `run2.out` 是它的原始输出） |
| Redis | 同 `NIK_REDIS_URL`（空闲时间戳键 `idle:sid:{sid}`） |

> 8033 上的 `cloudpivot-test.service` 未重启（无人值守会话里 `systemctl restart` 会被 approvals 拦），
> 因此**该实例仍跑旧二进制**；本次所有证据都来自 8036 上带本提交的二进制。

## 结果：39/39 通过

原始输出见 `run2.out`。关键几条（逐字摘自输出）：

```
[PASS] ① 参数配置列表能看到 auth.idle_timeout_minutes
[PASS] ① 默认值 = 15 :: config_value=15
[PASS] ① 改成 5 分钟保存成功 :: http=200 body=ok
[PASS] ③ 非法值 -1 被拒（400） :: http=400 msg=自动退出时间必须在 1-60 分钟之间（0=禁用仅超管可设）
[PASS] ③ 非法值 abc 被拒（400） :: http=400 msg=自动退出时间必须是整数分钟（1-60，0=禁用仅超管可设）
[PASS] ③ 非法值 2000 被拒（400） :: http=400 msg=自动退出时间必须在 1-60 分钟之间（0=禁用仅超管可设）
[PASS] ⑳ 超管设 70 被拒（400 + 1-60 文案） :: http=400 msg=自动退出时间必须在 1-60 分钟之间（0=禁用仅超管可设）
[PASS] ⑳ 超管设 1440 同样被拒 :: http=400 msg=自动退出时间必须在 1-60 分钟之间（0=禁用仅超管可设）
[PASS] ⑬ 普通管理员设 0 被拒（400 + 只有超级管理员…） :: http=400 msg=只有超级管理员可以禁用自动退出登录
[PASS] ⑬ 被拒后值未变（仍是 5） :: value=5
[PASS] ⑲ 普通管理员设 70 被拒（400 + 1-60） :: http=400 msg=自动退出时间必须在 1-60 分钟之间（0=禁用仅超管可设）
[PASS] ㉒ 普通管理员设 60 成功 :: http=200 msg=ok
[PASS] ㉓ 普通管理员设 1 成功 :: http=200 msg=ok
[PASS] ⑭ 超管设 0 成功 :: http=200 msg=ok
[PASS] ② 设为 0 后 policy 返回 timeout_minutes=0（前端不启动计时） :: {"timeout_minutes": 0, "warn_seconds": 0, "is_super_admin": true}
[PASS] ⑧ 登出后旧 token → 401（真吊销） :: http=401 body={"code": 401, "msg": "Token 已登出，请重新登录"}
[PASS] ⑩ 持续有操作（每 20s，累计 60s）→ 不退出 :: 最后一次 http=200
[PASS] ⑫ 空闲超时后（无 X-User-Active）→ 401 + 20009 :: http=401 body={"code": 20009, "msg": "因长时间未操作已自动退出，请重新登录"}
[PASS] ⑫ 超时后再补打活跃标记也救不回来（仍 401） :: http=401 body={"code": 20009, ...}
[PASS] ⑫ 该会话的 refresh 令牌已被吊销（不能靠刷新复活） :: http=401 msg=刷新令牌已被撤销
[PASS] 收尾: 终值 = 15 :: value=15
[PASS] 收尾: 夹具已清理（残留 0|0） :: 残留=0|0
===== 39/39 通过 =====
```

## 审计留痕（`audit_logs`，逐字摘自脚本输出）

```
CONFIG_IDLE_TIMEOUT_UPDATE|1|0|true|admin              ← 超管把自动退出从 1 分钟改成 0（禁用）
CONFIG_IDLE_TIMEOUT_UPDATE|60|1|false|idle_t282134     ← 普通管理员 60 → 1
CONFIG_IDLE_TIMEOUT_UPDATE|5|60|false|idle_t282134     ← 普通管理员 5 → 60
```

列依次为：`operation | before.config_value | after.config_value | operator_is_super_admin | username`。
「谁 / 什么时候（created_at）/ 从什么改到什么 / 是否超管」四要素齐全。

## 手工复核命令（复现口径）

```bash
# 145 上：起隔离实例（同库）
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export NIK_SERVER_LISTEN=127.0.0.1:8036
cd /opt/cloudpivot/backend && setsid nohup ./target/debug/cloudpivot > /tmp/idle.log 2>&1 < /dev/null &
# 跑验收（约 3 分钟，含 65 秒空闲等待）
IDLE_BASE=http://127.0.0.1:8036 python3 scripts/verify/verify_idle_timeout.py
```

## 一次「顺手发现的正确行为」

第一轮跑（脚本还是「用同一个 admin token 收尾」）时，最后两步红了：

```
[FAIL] 收尾: 恢复默认 15（第二轮） :: http=401 msg=因长时间未操作已自动退出，请重新登录
```

原因是脚本把超时设成 1 分钟后等了 65 秒 —— **admin 自己的 token 也被判空闲超时了**，
这正是「服务端是真的在拦」的旁证。脚本随后改成「收尾前重新登录」，第二轮 39/39。
