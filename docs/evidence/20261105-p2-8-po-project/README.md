# 证据 · P2-8 采购单挂项目（`biz_purchase_order.project_id`，V0237）

> 卡：`t_b87398c8`【P2-8】项目 → ERP 无关联（采购单试点加 project_id）
> 日期：2026-11-05 · 主机 99（Hermes 工作仓 `/opt/cloudpivot`）
> 口径文档：`docs/architecture/P2-8-项目挂ERP口径.md`（哪些单据该挂 / 口径 P1–P5 / 待挂清单）
> 迁移：`backend/migrations/business/V0237__erp_po_project_link.up.sql` / `.down.sql`

## 0. 一页结论

| 验收项（卡面） | 结果 | 证据文件 |
|---|---|---|
| 迁移 up / down 通过 | ✅ up → down → down(幂等) → up，列与索引每次都能正确建/删；**真引擎**（全新空库启动）也应用成功（`schema_migrations.version=237, success=t`，共 178 个迁移） | `migration-cycle.txt` |
| 单测 ≥6 条 | ✅ **10 条** `p28_*`（建单带/不带项目、项目不存在、项目已软删、非法 UUID、归档项目、改单换/清空、列表按项目筛+回填、项目软删后只丢名字、筛参校验）；整个模块 **60 passed / 0 failed**（真库，101s） | `cargo-test.txt` |
| 真机：建采购单选项目 → 列表能看到 → 详情能看到 | ✅ 隔离实例 + 真 HTTP：建项目 → 建单（带 project_id）→ 列表按项目筛命中且带 `project_name` → 详情带 `project_name`；**项目改名后详情跟着变**（证明是关联查不是快照）；项目软删后 `project_id` 仍在、`project_name` 变 `null`；改单不传 `project_id` → 归属被清空 | `e2e-api.txt` |
| 不变量全绿 | ✅ `erp_invariants.py` **52/52 通过 · BLOCKER 0 · WARN 0**（当前条数，卡面写的 32/32 是旧基线） | `invariants.txt` |
| 文档：还有哪些单据待挂项目 | ✅ `docs/architecture/P2-8-项目挂ERP口径.md` §1（该挂/不该挂论证表）、§5（收货 / 应付 / 付款 / 凭证 + 未决的产品决策） | 该文档 |
| git commit | ✅ 见卡面 completion（只含本卡 8 个路径） | — |
| 门禁 | ✅ rustfmt 1.94.1 / clippy `-D warnings` / build 全 0；邻接模块回归 model::tests 25 passed、stockin D4 purchase 17 passed | `lint.txt` |

## 1. 文件说明

| 文件 | 内容 |
|---|---|
| `migration-cycle.txt` | `V0237` 的 up / down / 幂等重放全过程 + 结构核对（列 `uuid nullable=YES`、部分索引 `idx_biz_po_project`）+ **真引擎**在全新空库上的应用结果 |
| `cargo-test.txt` | 隔离 clone 上 `cargo test --lib business::erp::purchase_handler`：10 条 `p28_*` 逐条 ok + 模块合计 60 passed |
| `e2e-api.txt` | 真机 9 步（含每步请求/响应 JSON 与断言）：登录 → 主数据 → 建项目 → 建单（带项目）→ 列表按项目筛 → 详情 → **项目改名后跟着变** → 项目不存在 400+66002 → 项目软删后只丢名字 → 改单清空 |
| `invariants.txt` | 同一个 e2e 库上 `scripts/verify/erp_invariants.py`：52/52 通过 |
| `lint.txt` | rustfmt / clippy / build + 两个邻接模块回归 |

## 2. 复跑（本机 99）

```bash
# ① 隔离 clone（HEAD + 本卡文件），独立 target，避免与别的卡抢锁/互相污染
git clone -q /opt/cloudpivot /root/p28-tree
cp /opt/cloudpivot/backend/config/config-*.toml /root/p28-tree/backend/config/
#   （把本卡改动同步进去：见卡面 changed_files）

# ② 门禁
cd /root/p28-tree/backend
export CARGO_TARGET_DIR=/root/p28-target PATH=/root/.cargo/bin:$PATH
rustfmt +1.94.1 --edition 2024 --check src/business/erp/purchase_handler.rs
cargo clippy -- -D warnings

# ③ 真库单测（库要已应用 V0237；模板云库或自己建的都行）
export ERP_DB_URL='postgres://alun:alun@127.0.0.1:5432/<已迁移库>'
export NIK_PROFILE=test NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
       CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32)
cargo test --lib business::erp::purchase_handler -- --test-threads=1

# ④ 真机（全新空库，让应用自己跑迁移）
runuser -u postgres -- psql -qc "CREATE DATABASE cloudpivot_p28e2e OWNER alun"
cd /root/p28-tree/backend
NIK_PROFILE=test NIK_DATABASE_NAME=cloudpivot_p28e2e NIK_DATABASE_USER=alun \
NIK_DATABASE_PASSWORD=alun NIK_SERVER_LISTEN=127.0.0.1:18137 \
NIK_ADMIN_PASSWORD='Admin@123456' NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) \
  /root/p28-target/debug/cloudpivot
# 另一个终端：python3 p28_e2e.py http://127.0.0.1:18137

# ⑤ 不变量
cd /opt/cloudpivot
NIK_DATABASE_NAME=cloudpivot_p28e2e NIK_DATABASE_USER=alun NIK_DATABASE_PASSWORD=alun \
  python3 scripts/verify/erp_invariants.py
```

## 3. 踩到 / 绕过的坑（留给下一个做同类卡的人）

1. **18097 端口被别的卡的实例占着**：第一次起实例时迁移跑完就 `Address already in use` 退出，
   `/health` 200 其实是别人的进程 —— 判据是日志里必须有 `cloudpivot 启动 -> http://127.0.0.1:<你的端口>`
   **且没有** `FATAL: 服务启动失败`。换成 18137 才真的起来。
2. **共享工作区编不过不是你的锅**：`/opt/cloudpivot` 里 `unit_guard.rs`（+102 行）与
   `budget_guard_tests.rs` 的未提交 WIP 让 `cargo test` 直接编译失败（`no field status on type ()` /
   `no method enforces`）。隔离 clone 里 HEAD 是干净的，所以「隔离 clone = 唯一可信的验收环境」这条再次成立。
3. **`PurchaseOrderRow` 有字面量构造点**：`purchase_credit.rs::sample_row()` 直接 `PurchaseOrderRow { … }`，
   加字段后必须同步（否则整个 lib test 编不过）。加字段前 `grep -n "<Struct> {"` 一遍最稳。
4. **种子 admin 带 `must_change_password=true`**：除改密/登出外全部 **403**。e2e 脚本必须先
   `PUT /api/system/auth/password` 改密再重新登录，否则第一个业务接口就 403（不是权限码问题）。
5. **建单响应体是 `{order, items}`**，不是平铺的 `{id}`（`purchase_order_create` 返回 `PurchaseOrderDetailRes`）。
6. **项目列表 `/business/project/page` 受项目可见性裁剪**：admin 全能看，普通采购员只看自己参与/负责的项目 ——
   这是已知取舍，见口径文档 §6.2。
