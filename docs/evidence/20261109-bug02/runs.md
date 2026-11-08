# BUG-02 执行记录（真实命令与输出摘要）

- 卡：`t_1bf6c58d`
- 工作区：`/opt/cloudpivot`（99 工作仓），分支 `main`
- 测试库：`cp_bug01_e2e`（本机 PG，owner `alun`，全量迁移 + 演示用户种子；BUG-01 建的隔离库复用）
- 环境变量：`ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cp_bug01_e2e'`

## 0. 改动文件

```
backend/src/business/erp/stockin_dto.rs                       +18   （详情两个字段）
backend/src/business/erp/stockin_handler_common.rs            +15   （detail_of 组装）
backend/src/business/erp/stockin_handler_post.rs              +96/-2（repost_stock_in + 带原因的审批后过账）
backend/src/business/erp/stockin_handler_status.rs            +27   （POST /repost 路由）
backend/src/business/erp/stockin_handler_tests_pflow3a.rs     +430  （6 条真库用例）
frontend/src/api/business/erp/index.ts                        +26   （repostStockIn + 类型字段）
frontend/src/views/business/erp/stockin/index.vue             +80   （按钮 / 标签 / 告警条 / 处理函数）
docs/evidence/20261109-bug02/README.md                        （证据与姊妹单据评估）
docs/evidence/20261109-bug02/runs.md                          （本文件）
```

## 1. 编译

```
$ cd backend && cargo check --all-targets
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.24s
```

```
$ cargo build            # 供实机路由验证的二进制
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 40s
EXIT=0
```

## 2. 新增用例（真库，`--test-threads=1`）

```
$ ERP_DB_URL=... cargo test --lib bug02_ -- --test-threads=1
running 6 tests
test ...::bug02_repost_blocked_by_separation_of_duties ... ok
test ...::bug02_repost_over_receive_keeps_pending_then_recovers_after_fix ... ok
test ...::bug02_repost_pending_approved_posts_and_moves_books ... ok
test ...::bug02_repost_rejects_draft ... ok
test ...::bug02_repost_rejects_instance_still_pending ... ok
test ...::bug02_repost_rejects_posted ... ok

test result: ok. 6 passed; 0 failed; 0 ignored
```

> 首次运行为 5 passed / 1 failed：`over_receive` 那条断言的**文案关键词**猜错了
> （实际拦在①b「采购单已收货（RECEIVED）不能过账入库」，不是③待收额度）。
> 断言改为「不能过账 + 点名采购单现状」后重跑通过 —— 行为正确，是用例措辞写窄了。

## 3. 收货单模块回归

```
$ ERP_DB_URL=... cargo test --lib business::erp::stockin -- --test-threads=1
test result: ok. 163 passed; 0 failed; 0 ignored; 0 measured; 3689 filtered out; finished in 498.36s
EXIT=0
```

（含本卡新增 6 条。口径说明：`cargo test --lib business::erp::stockin -- --list` = 163 条，
其中本卡 6 条 → 存量 157 条全绿；BUG-01 汇报的「158」用的是 `stockin_handler` 过滤口径
（当前 149 条），两个过滤集合不同，不做加减对比。）

## 4. 前端

```
$ cd frontend && pnpm typecheck
> vue-tsc --noEmit
（无输出，exit 0）
```

```
$ pnpm build
EXIT=0
dist/index.html  2026-11-09_05:11:47   入口 chunk: assets/index-FxM2Mde5.js
```

## 5. 实机路由验证（接口真的注册上了）

起实例：`NIK_SERVER_LISTEN=127.0.0.1:18101`（库 `cp_bug01_e2e`），健康检查 200 后：

```
$ curl -s -o /dev/null -w '%{http_code}' -X POST \
    http://127.0.0.1:18101/api/business/erp/stock-in/00000000-0000-0000-0000-000000000000/repost
401        {"code":401,"msg":"未授权访问，请先登录"}

$ curl ... -X POST .../repost-xyz      # 不存在的路由对照
404        {"code":404,"msg":"请求的资源不存在"}
```

→ `/repost` 命中路由并被鉴权拦住（401），不是「路由不存在」（404）。

## 6. 格式与静态检查

```
$ rustfmt +1.94.1 --edition 2024 --check <本卡 5 个 .rs>
FMT_OK
```

```
$ cargo clippy --all-targets --message-format short -- -D warnings
src/business/erp/invoice.rs:1967:26: error: the following explicit lifetimes could be elided: 'a
src/business/erp/receivable.rs:800:5: error: doc list item without indentation
src/business/erp/sales_order_model.rs:699:1: error: this function has too many arguments (8/7)
error: could not compile `cloudpivot` (lib) due to 3 previous errors
EXIT=101
```

**这 3 条全部在别人未提交的文件里**（`invoice.rs` / `receivable.rs` / `sales_order_model.rs`
——与 BUG-01 那次是同一批既有告警，git 里仍是 `M` 状态、非本卡改动）。
本卡 5 个 `.rs` 文件 **0 告警**（clippy 只在命中文件上报错，本卡文件没有出现在列表里）。

## 7. 复现方式

```bash
# 1) 准备库（已有可跳过）：/root/bug01_db.sh 建的 cp_bug01_e2e
export ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/cp_bug01_e2e'
cd /opt/cloudpivot/backend
cargo test --lib bug02_ -- --test-threads=1

# 2) 只跑「修好数据可再试」那条
cargo test --lib bug02_repost_over_receive -- --test-threads=1
```
