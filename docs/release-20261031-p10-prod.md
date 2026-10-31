# 发布留痕 · 生产追到 main：P10 状态机 / 入账后只读 / 状态变更日志（V0159）

> 卡：`t_6d6af163`（负责人 manong）；上游实现卡：`t_8ca0c6a4`
> 发布窗口：2026-10-31 13:32–13:34 CST；目标机：`172.18.10.144:/opt/cloudpivot`（prod，systemd `cloudpivot.service`，:8023 + nginx:80）
> 本文所有数字均为本次发布真机真实输出，不含推算。

## 一、发布口径

| 项 | 值 |
|---|---|
| 源码 | 隔离 clone `file:///opt/git/cloudpivot.git` → `/var/tmp/p10-release`，`HEAD = 1c424ec5f257d02fdd2c6dadced42e1b9e4b10c3` |
| 编译 | `CARGO_TARGET_DIR=/var/tmp/p10-target cargo build --release --offline`（冷编，**未复用**共享工作区 target；339 crate + cloudpivot，`Finished in 2m32s`，0 warning / 0 error） |
| 二进制 | sha256 `8a3a2b70abd8705b931698c9683e959075f98f1a9925a0707c2304dc5a877c4d`（41,386,960 B） |
| 替换方式 | `cp 新 → $REL.new && mv -f $REL.new $REL`（同盘 rename，原子；避开 `Text file busy`） |
| 发布前二进制 | sha256 `cacf939a983d401828f04405f21e368278ad4548097e64257e34709ea215f438`（`c49c0bb`，已留档） |
| 迁移 | `V0159__biz_status_log`（up/down 2 个文件从 `1c424ec` 落到 `backend/migrations/business/`，prod 目录与 main 的**唯一**差异就是这两个文件） |
| 服务 | `systemctl restart cloudpivot` → `health=200`（2s 内），`MainPID=3080807` |

`c49c0bb` 是 `1c424ec` 的**祖先**（`git merge-base --is-ancestor` = YES），所以本次是纯前滚：`c49c0bb..main` 共 19 个提交（P10 四笔 + P12 画布视觉（前端）+ P11 任务取消撤审批实例 + 数据修复/文档/e2e），**不含任何只在共享工作区里的旁支改动**。

## 二、发布后核对（逐条真实输出）

### 1. 启动日志

```text
数据库迁移完成: 应用 1 个迁移
执行 1 个自定义启动钩子（按注册顺序）
启动引导完成  app=cloudpivot profile=prod db_driver=postgres
定时任务插件: 20 个任务已启动
启动守卫通过：认证密钥长度满足下限            jwt_secret_len=64
启动守卫通过：缓存后端满足吊销耐久性要求      cache_type=redis
启动守卫通过：存储主密钥长度满足要求          storage_master_key_len=64
cloudpivot 启动 -> http://127.0.0.1:8023
cloudpivot 启动完成, 耗时 51ms
```

### 2. 迁移与表

```text
max(version::int) = 159
schema_migrations: 159 | biz_status_log
to_regclass('biz_status_log') = biz_status_log
```

### 3. 二进制双验

```text
disk sha256 = 8a3a2b70abd8705b931698c9683e959075f98f1a9925a0707c2304dc5a877c4d
proc sha256 = 8a3a2b70abd8705b931698c9683e959075f98f1a9925a0707c2304dc5a877c4d   ( /proc/3080807/exe )
md5sum disk = 9f60fab27caf86270643a665ebbcb484
md5sum proc = 9f60fab27caf86270643a665ebbcb484          （与磁盘逐字节一致）
grep -ac "单据已入账" 新二进制 = 1    （旧二进制 = 0）
```

### 4. 不变量套件（main 最新脚本，生产库）

```text
结果：32/32 通过   🚨 BLOCKER 0   ⚠️ WARN 0
★ 全部不变量成立，账目干净。
```

发布后跑了两轮：部署完立刻一轮、真机抽验 + 作废 + 清理之后一轮，两轮均 **32/32 / BLOCKER 0 / WARN 0**。

### 5. 真机抽验（卡面 §五）

**① 建单 → 过账 → 状态日志 2 行**

```text
POST  /api/business/erp/stock-in              -> 200  in_no=IN-20261031-000015 status=DRAFT total_amount=2.0000
PATCH /api/business/erp/stock-in/{id}/status  -> 200  {"status":"POSTED"}
select from_status,to_status from biz_status_log where entity_no='IN-20261031-000015'
    NULL→DRAFT   | STOCK_IN | admin
    DRAFT→POSTED | STOCK_IN | admin
```

同日顺带佐证了「五实体齐全 + 同事务」。过账这一笔同时写出：

```text
VOUCHER  PZ-20261031-000014   NULL → POSTED
PAYABLE  AP-20261031-000012   NULL → OPEN
```

**② 入账后改金额必须 409**

```text
PUT /api/business/erp/stock-in/{id}  {"total_amount":999}
-> HTTP 409  code=10003
   msg: 单据已入账（状态 POSTED），金额/数量/明细不可修改。
        如需更正请先作废或红冲后重开。（收货入库单 IN-20261031-000015；
        白名单：remark / status / updated_at / update_by / 系统回写字段）
        本次改动涉及：不含税金额合计 total_amount
db total_amount: 2.0000 -> 2.0000（未被改动）
```

修复前的表现是**假绿 200**（`total_amount` 被 serde 静默丢弃 → 退化成改备注），现在硬闸 409 且点名字段。

## 三、抽验数据的收尾（生产不留测试库存）

| 单据 | 处理 |
|---|---|
| `IN-20261031-000015`（过账→抽验→作废） | `PATCH status=CANCELLED` → 200；库存/库位账/流水/序列号按 P9 六件套退回，日志第 3 行 `POSTED→CANCELLED`，同时写出红冲凭证 `PZR-20261031-000007` 与 `AP-20261031-000012 OPEN→CANCELLED` |
| `IN-20261031-000014`（第一次脚本跑失败留下的草稿） | `DELETE`（仅 DRAFT）→ 200，软删；日志 1 行 `NULL→DRAFT` 保留（日志只增不改） |

两条抽验单都带 `remark='P10发布抽验-…'` 前缀可检索。作废后的 `biz_status_log` 共 8 行，分布：

```text
PAYABLE|CANCELLED|1  PAYABLE|OPEN|1  STOCK_IN|CANCELLED|1  STOCK_IN|DRAFT|2  STOCK_IN|POSTED|1  VOUCHER|POSTED|2
```

匿名访问基线未变（新二进制下仍是 401，不是 404）：

```text
GET /api/business/erp/stock-in/page                       -> 401
GET /api/business/erp/stock-in/{uuid}                     -> 401
```

## 四、留痕与回滚

- 二进制回滚：`/root/backup/p10-release-20261031-132831/cloudpivot-old-c49c0bb`（sha256 `cacf939a…`）→ `cp → $REL.new && mv -f` → `systemctl restart cloudpivot`。
- 库备份：同目录 `cloudpivot-20261031-132831.dump`（`pg_dump -Fc`，1,533,172 B，`pg_restore -l` 可读）。
- 迁移回滚：`V0159.down.sql`（有日志时主动中止 —— 只增不改的护栏），实际回滚前必须确认 `biz_status_log` 只有本卡抽验行。
- 冷编缓存留在 `/var/tmp/p10-target`（下次同 main 重建可直接复用），源码 clone 留在 `/var/tmp/p10-release`。

## 五、已知边界（发布后仍需注意）

1. 共享工作区 `/opt/cloudpivot` 仍停在 `b0bfb8b`（分叉未解决）。本次只把两个 V0159 迁移文件落进它的 `backend/migrations/business/` 并 `git add`（内容与 main 上的逐字节相同：`V0159__biz_status_log.up.sql` sha256 `3f7c575d…`），**没有** merge/rebase 任何人的在途改动。下次有人在共享工作区 `git merge --ff-only origin/main` 时，这两个文件不会以「未跟踪文件会被覆盖」挡住合并。
2. main tip 是「本文档所在的 docs 提交」（`1c424ec` 之后只有本文件与它的两次 docs 提交）；`git diff 1c424ec..main -- backend` 为空，`git diff --stat 1c424ec..main` 只列出本文件 —— 线上二进制对应的代码树就是 `1c424ec` 的树。
3. P12 画布视觉与 P11 的前端改动属于 `c49c0bb..main` 的一部分，但**本次只替换后端二进制**，`frontend/dist` 未重建 —— 前端是否同步上线属于 P12 卡的范围。
4. 本轮不涉及新配置项；`/etc/cloudpivot/cloudpivot.env` 未改动。
