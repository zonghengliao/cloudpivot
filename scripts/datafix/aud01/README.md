# AUD-01 · 附件上传人姓名快照 —— 存量回填（一次性数据修复）

**不是迁移**：不进 `backend/migrations/`，不参与启动自动应用。结构变更在
`backend/migrations/business/V0352__attachment_uploader_name.up.sql`（加两列），
本目录只负责**把历史行填上值**。

## 为什么要有这一步

`biz_doc_attachment` / `biz_task_attachment` 原来只存上传人 id
（`uploaded_by` / `create_by`）。读路径 ATT-01 / ATT-03 靠实时 `JOIN sys_user`
取名字，那是**现值** —— 用户改名 / 销号之后，回看旧附件就说不清「当时是谁传的」。
V0352 加了 `uploader_name` 快照列，新上传由代码写入；**历史行**由本脚本回填。

## 文件

| 文件 | 做什么 | 写库？ |
|------|--------|--------|
| `10_precheck.sql` | 回填前体检：列在不在、空值多少、空值里「在职 / 已删 / 账号不存在 / 无上传人」各多少、有没有 UUID 脏值 | 否 |
| `backfill_attachment_uploader_name.sql` | 回填主体：台账备份 + 四类 UPDATE（只填 `NULL`）+ 自检 + 分布报告 | 是（幂等） |
| `20_verify.sql` | 回填后验收：空值数 = 0、无 UUID 脏值、占位串只出现在该出现的行、抽样 10 行 | 否 |
| `apply.sh` | 执行器：`--check` / 默认回填 / `--verify` / `--rollback` | — |

## 取值口径（与 `backend/src/business/shared/user_name.rs` 逐字一致）

| 情况 | 写成 |
|------|------|
| 账号在库、未删、`real_name` 非空 | `real_name` |
| 账号在库、未删、`real_name` 为空 | `username` |
| 账号已软删（`deleted_at` 非空） | `(已删除用户)` |
| 上传人 id 有值、但 `sys_user` 里没有这一行 | `(已删除用户)` |
| 上传人 id 本身为空 | `(未记录上传人)` |

> 为什么占位串而不是留空：`uploader_name IS NULL` 只能有一个含义 ——「本脚本还没跑」。
> 留空会把「没回填」和「没上传人」混成同一件事，验收③（空值数 = 0）也就失去意义。

## 用法（在目标库那一侧执行）

```bash
# 0) 前置：V0352 已应用（新代码启动时自动跑；单独补也可以）
#    psql -f backend/migrations/business/V0352__attachment_uploader_name.up.sql

# 1) 体检（只读）
AUD01_DB=cloudpivot bash scripts/datafix/aud01/apply.sh --check

# 2) 回填（幂等；建议在事务窗口之外的低峰期跑，两张表都很小）
AUD01_DB=cloudpivot bash scripts/datafix/aud01/apply.sh

# 3) 验收（只读）
AUD01_DB=cloudpivot bash scripts/datafix/aud01/apply.sh --verify

# 4) 需要撤销本脚本的回填（不动结构）
AUD01_DB=cloudpivot bash scripts/datafix/aud01/apply.sh --rollback
```

口令只走 `PGPASSWORD` 环境变量；不要把 `-W`/明文写进脚本或命令行历史。

## 幂等与回滚

- **幂等**：所有 UPDATE 都带 `WHERE uploader_name IS NULL`；台账插入 `ON CONFLICT DO NOTHING`。
  连跑两遍，第二遍每一段的 `RAISE NOTICE` 行数都是 0（脚本会打印实测行数）。
- **可回滚**：改前把「行 id + 改前值」写进 `datafix_aud01_uploader_name_backup`，
  改后写「改后值」；`--rollback` 只还原「当前值仍等于回填值」的行 —— 回填之后
  被新代码写过的真实快照不会被抹掉。
- 台账表属于数据修复产物，可以留着；确认无需回滚后手工 `DROP TABLE` 即可（本卡不自动删）。

## 与 V0352.down 的关系

`V0352.down.sql` 会**丢列**，姓名快照是唯一副本（改名后 `sys_user` 里已没有旧名）。
真要走 down，先 `\copy (SELECT id, uploader_name FROM biz_doc_attachment) TO ...` 存档。
