# 贡献指南（Contributing）

感谢你愿意参与云枢。

## 先了解这个项目的特殊性

云枢的**全部二次开发代码由 AI 编写**，人类负责需求决策、方向把控与验收。
这不是噱头，它决定了本项目的贡献规则：

> **一个改动能不能被接受，取决于它能不能被验证，而不取决于它写得多漂亮。**

所以你会看到本项目对「证据」的要求远高于一般开源项目。

---

## 提交前必须满足

### 1. 代码规范

```bash
# 后端
cd backend
cargo fmt
cargo clippy -- -D warnings
cargo test --lib

# 前端
cd frontend
pnpm typecheck
pnpm build
pnpm test:unit
```

> 内存受限的机器请用 `./scripts/ops/run_tests.sh backend`（单 job + 关 debuginfo +
> 产物自动清理）。直接 `cargo test` 在 8G 内存机器上会被 OOM killer 杀掉。

### 2. 数据库迁移规则（强约束）

```
① 已应用的迁移**绝不修改**内容 —— 只能追加新的迁移
② 每个迁移必须**幂等**（重复执行结果不变）
③ 必须提供对应的 .down.sql
④ 改表结构必须考虑存量数据（加列给默认值 / 分批回填）
⑤ 迁移文件命名：V<四位序号>__<snake_case_描述>.up.sql
```

违反这条的 PR 会被直接拒绝，没有例外。

### 3. 流程逻辑不硬编码

**业务流程必须用「审批流定义 + 任务」实现，不允许写死在代码里。**

```rust
// ❌ 拒绝
if order.amount > 100000.0 {
    require_approval(order, "总经理");
}

// ✅ 接受：走流程引擎，金额阈值配在 biz_flow_node 的条件里
flow_engine::start("po_approve", order.id)?;
```

判断标准：**如果客户想改流程需要改代码，那这个设计就是错的。**

### 4. 新业务代码的位置

```
backend/src/business/<模块>/          后端业务
frontend/src/api/business/<模块>/     前端 API
frontend/src/views/business/<模块>/   前端页面
```

**不要动 `system/` 与 `support/`** —— 那是框架底座，改动影响面太大。

### 5. 新表的最低要求

```sql
CREATE TABLE biz_xxx (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    -- 业务字段 …
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    create_by   uuid,
    update_by   uuid,
    deleted_at  timestamptz              -- 软删
);
-- 软删感知的唯一索引
CREATE UNIQUE INDEX uk_biz_xxx_code ON biz_xxx (code) WHERE deleted_at IS NULL;
```

---

## PR 要求

### 标题格式

```
<type>(<scope>): <简述>

type:  feat | fix | refactor | docs | test | chore | perf
scope: erp / flow / task / system / support / frontend / ops / arch
```

### 描述必须包含

```markdown
## 改了什么
（一句话）

## 为什么改
（对应的问题 / 需求）

## 怎么验证的
（命令 + 实际输出，不是"我测过了"）

## 影响面
（哪些模块 / 表 / 接口受影响；是否需要迁移）

## 截图（前端改动必附）
```

### 不接受的情况

```
❌ 只有描述没有验证输出
❌ 改了已应用的迁移文件
❌ 新流程写死在代码里
❌ 无迁移的 schema 变更
❌ 前端改动没跑 pnpm build
❌ 提交里带 .env / 密钥 / 真实客户数据
```

---

## 提交信息规范

```
feat(erp): 采购订单支持分批收货
fix(flow): 会签节点在无成员时卡死（改用兜底人）
docs(arch): ARCH-13 MES 设计——BOM 多版本与替代料
test(erp): 库存三本账勾稽断言（12 组）
```

**不要**写 `update`、`fix bug`、`修改`、`111` 这类无信息量的提交信息。

---

## 安全相关

发现漏洞请**不要开公开 Issue**，按 [SECURITY.md](SECURITY.md) 的渠道私下报告。

提交代码时请确保：

```
· 不含真实凭据（用环境变量 / 配置占位符）
· 不含真实客户数据（测试数据请明确标注 test / demo）
· 不含内网 IP 与主机名（用 127.0.0.1 或占位符）
```

---

## 关于 AI 生成的代码

**AI 生成的代码和人类写的代码，审查标准完全一致。**

我们不看"是谁写的"，只看：

```
① 逻辑是否严密（边界条件、并发、幂等）
② 是否有测试覆盖（关键路径必须有断言）
③ 是否可验证（别人能不能照着你的步骤复现）
④ 是否符合本项目的架构约定
```

如果你用 AI 生成 PR，**请在描述里说明**，并特别检查：
AI 容易写出"看起来对但边界错"的代码（尤其是金额、日期、并发场景）。

---

## 许可

提交 PR 即表示你同意你的贡献以 **Apache License 2.0** 授权。

---

## 有问题？

开 Issue 讨论，或看 [docs/README.md](docs/README.md) 的二开文档集。
