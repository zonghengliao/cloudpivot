# 2026-11-08 00:00 `【MENU-STRUCT】` 假告警复盘（卡 t_4bc33adf）

## 结论先说

**没有菜单结构违规。** 145 测试库与 144 生产库 5 条不变量全绿、菜单总数 410（窗口 400–420），
两库结构一致。告警是 `menu_invariants.sh` 自身把「客户端 locale 噪声」当成「连库失败」造成的**假阳性**，
现已修掉并在两台真机验收。

| 环境 | HEAD（告警时） | 库 | ①孤儿 | ②/system | ③/business | ④path唯一 | ⑤总数 | 结果 |
|---|---|---|---|---|---|---|---|---|
| 145 测试 | 08474067 | cloudpivot_dev | 0 ✅ | 0 ✅ | 0 ✅ | 0 ✅ | 410 ✅ | 全绿 |
| 144 生产 | 2389836b | cloudpivot | 0 ✅ | 0 ✅ | 0 ✅ | 0 ✅ | 410 ✅ | 全绿 |

⇒ **不需要任何「把 parent_id 归位」的幂等迁移**，也不需要查重复插入（总数没翻倍）。

## 告警现场（`/var/log/cloudpivot/deploy-check.log`）

```
[2026-11-08 00:00:02]   ① 99  开发:    84e166a  (未推送 1 / 未提交 8)
[2026-11-08 00:00:02]   ② 145 bare:    84e166ae   ← 权威
[2026-11-08 00:00:02]   ③ 145 测试:    08474067  (服务 active)
[2026-11-08 00:00:02]   ④ 144 生产:    2389836b  (服务 active)
[145测试] bash: warning: setlocale: LC_ALL: cannot change locale (zh_CN.UTF-8): No such file or directory
[145测试] ⛔ 连不上库 cloudpivot_dev（cloudpivot@127.0.0.1:5432）
[2026-11-08 00:00:02]   145测试 菜单不变量: rc=2
[144生产] bash: warning: setlocale: LC_ALL: cannot change locale (zh_CN.UTF-8): No such file or directory
[144生产] ⛔ 连不上库 cloudpivot（cloudpivot@127.0.0.1:5432）
[2026-11-08 00:00:03]   144生产 菜单不变量: rc=2
```

注意第一行 warning：远端 shell 收到的是 `LC_ALL=zh_CN.UTF-8`，而 145/144 **都没装 zh_CN locale**。
两库的 PG 日志在此刻**没有任何认证/连接错误**（145 只有 23:38 的 FIX-4 用例报错，144 最后一条是 22:16）——
即连接根本没失败。

## 根因链（两处叠加，缺一不犯）

1. `psql` 是 Debian 的 `pg_wrapper`（`/usr/bin/psql -> ../share/postgresql-common/pg_wrapper`，**Perl 脚本**），
   两台机都一样。
2. 调用方的 locale 被 ssh `SendEnv LANG LC_*`（`/etc/ssh/ssh_config:50`）带进远端；远端没装 → Perl 打警告；
3. 旧版 `sql()` 写的是 `out=$(psql … 2>&1)`：**stderr 并进返回值**。实测评测：

```
$ LC_ALL=zh_CN.UTF-8 psql … -tA -q -c 'select 1' 2>&1
rc=0 out=[perl: warning: Setting locale failed.
perl: warning: Please check that your locale settings:
	LANGUAGE = (unset),
	LC_ALL = "zh_CN.UTF-8",
	…
    are supported and installed on your system.
perl: warning: Falling back to a fallback locale ("en_US.UTF-8").
1]
```

**psql 以 0 退出**，只是 stdout 被警告串糊住了。连库自检于是判 `!= "1"` → 打印「⛔ 连不上库」→ rc=2。
两个库同时中招（同一条链路上两个 ssh 会话都带脏 locale），所以看起来像是「两个环境一起挂了」。

## 复现与修复对照（145 / 144 实机，同一份脚本输入）

| 用例 | 旧版 | 新版 |
|---|---|---|
| `LC_ALL=zh_CN.UTF-8` 运行 | rc=2「⛔ 连不上库」（逐字复现告警） | **rc=0 全绿** |
| 默认 locale | rc=0 | rc=0 |
| 错密码 | rc=2 | rc=2，且带 psql 报错明文 |
| 空库（无 `sys_menu`） | rc=2 | rc=2，并标注「基础设施问题，不是结构违规」 |

## 修复内容（`scripts/verify/menu_invariants.sh`）

1. **钉死 locale**：`export LC_ALL=C LANG=C`、`unset LANGUAGE`、`export PERL_BADLANG=0`，
   并 `export PGCLIENTENCODING=UTF8` 让中文菜单名不被 `SQL_ASCII` 走样。
2. **`psql -X`**：忽略 `~/.psqlrc`（`\timing on` 之类会犯同一类「污染返回值」的病）。
3. **`sql()` stderr 与值流彻底分离**：stderr 进临时文件，只有 stdout 当返回值；
   成功但有警告时单独转到本脚本 stderr。
4. **失败语义修正**：`sql()` 原先 `exit 2`，但调用点都在 `$(sql …)` 子 shell 里——`exit` 只杀子 shell，
   主流程拿到空串会把基础设施故障误判成「结构违规 rc=1」，进而诱导别人去写修复迁移。
   现改为写共享错误日志，**结论前统一判定 → rc=2**。

## 验收证据（145 全量 8/8）

```
== 主机 ubuntu-svr / 库 cloudpivot_dev / 脚本 a5eaa2ea ==
[1] 脏 locale（复现 00:00 假告警的输入）
  ✅ 新版+zh_CN.UTF-8 → rc=0
     输出含「全绿」
[2] 干净 locale
  ✅ 新版+默认 locale → rc=0
[3] 错密码（基础设施问题必须 rc=2）
  ✅ 错密码 → rc=2
     报错明文已带出
[4] 空库（无 sys_menu：自检过、断言查不到表 → 必须 rc=2 而不是 rc=1）
  ✅ 空库→rc=2 → rc=2
     已标注「不是结构违规」
[5] 注入违规（副本库）
     副本菜单数=410（源库同口径 410）
  ✅ 副本基线 → rc=0
  ✅ 关掉例外表→③报红 → rc=1
     ③ /business/ 归属:       4  ❌
  ✅ 注入②（/system/user 摘父）→报红 → rc=1
     ② /system/ 归属:         1  ❌
         ↳ /system/ 节点未挂在 system 目录（10000000-0000-0000-0000-000000000001）下（前 10 条）：
  ✅ 复原后转绿 → rc=0
== 结果：8 通过 / 0 失败 ==
```

副本库 `mi_probe` / `mi_probe_empty` 已 `dropdb`，145 上残留 0 个（已核）。

## 144 生产（只读，不在生产建库/改库）

```
== 主机 ubuntu-svr / 库 cloudpivot / HEAD 2389836b / 脚本 a5eaa2ea ==
  ✅ 新版+脏 locale（复现 00:00 假告警的输入） → rc=0
     库: cloudpivot
     ① 孤儿 parent:           0  ✅
     ② /system/ 归属:         0  ✅
     ③ /business/ 归属:       0  ✅
         ↳ 例外表：4 条 path 豁免（/notice-center 根），失配 0（缺失/已合规）
     ④ path 唯一性:           0  ✅
     ⑤ 菜单总数:            410  ✅ (400-420)
     结果: 全绿 ✅
  ✅ 新版+默认 locale → rc=0
  ✅ 错密码→rc=2 → rc=2
     报错明文已带出：psql: error: connection to server at "127.0.0.1", port 5432 failed: FATAL:  password authentication failed for user "cloudpivot"
== 结果：3 通过 / 0 失败 ==
```

## 遗留（已建卡给 ops 侧，不在本仓）

`deploy_check_v5.sh` 的 `menu_check()` 只在 `rc=1` 分支拼 `MENU_DETAIL`，`rc=2` 落 `*)` 分支只写一句
「执行失败」——**这就是这次卡面代码块全空、只能靠翻 `/var/log/cloudpivot/deploy-check.log` 才知道原因**的由来。
建议两处：① `*)` 分支也把输出塞进 `MENU_DETAIL`；② 远端调用前 `export LC_ALL=C`（仓库内脚本已自带免疫，
但兜住「旧脚本还在跑」的窗口期）。
