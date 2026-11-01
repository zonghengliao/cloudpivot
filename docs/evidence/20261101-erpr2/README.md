# ERP-R2 利润表 · 验收证据（2026-11-01）

> 卡：`t_3e6d3220【ERP-R2】利润表（本期+本年累计，勾稽到权益变动，10 单测）`
> 交付说明 + 运维手册：`../../ERP-R2-利润表.md`
> 本目录只放**可复核的证据**（命令 + 原始输出摘要），截图仅留痕（vision 配额当日用尽，未人工查看，
> 页面结论一律以 **DOM 类名 / 背景色 / 文案断言** 为准）。

## 0. 一句话结论

| 项 | 结果 |
|----|------|
| 全量单测 | **1705 passed / 0 failed**（全新库 `cloudpivot_r2v2`，`--test-threads=1`，441s） |
| 本卡单测 | **20 条**（`business::erp::income_statement`，含 6 条真库用例） |
| 端到端（隔离库 + 真 HTTP） | **70 PASS / 0 FAIL**（`scripts/verify/erpr2_income_statement.sh`） |
| 页面（Playwright，隔离实例） | 一致态 **20 PASS / 0 FAIL**；不平态 **19 PASS / 0 FAIL** |
| 生产真机（144，2026-10 现有 11 张凭证） | **24 PASS / 0 FAIL**（含导出 xlsx 6345 字节） |
| ERP 不变量套件 | **31/32，BLOCKER 0，WARN 1**（F2 为历史数据，删前同此） |
| fmt / clippy / vue-tsc | 全部干净（`cargo fmt -p cloudpivot --check`、`clippy --lib -- -D warnings`、`vue-tsc --noEmit`） |

## 1. 全量单测（全新库）

```bash
# 建库 → 起一次实例把迁移跑到最新（含 V0184）→ 杀掉 → 跑全量
su - postgres -c "createdb -O erpr2role cloudpivot_r2v2"
bash /root/r2v2_migrate.sh          # 日志：数据库迁移完成: 应用 128 个迁移
cd backend && ERP_DB_URL=postgres://erpr2role:***@127.0.0.1:5432/cloudpivot_r2v2 \
  cargo test --lib -- --test-threads=1
# ⇒ test result: ok. 1705 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 441.05s
```

> 踩坑：起实例跑完迁移后要**等 bootstrap 把 admin 用户种上**再杀进程（否则
> `65018 本节点没有有效审批人，且平台没有可兜底的超级管理员` 会让 60 条审批流用例假红）。

## 2. 本卡单测（20 条）

```bash
ERP_DB_URL=... cargo test --lib business::erp::income_statement -- --test-threads=1
# ⇒ test result: ok. 20 passed; 0 failed
```

覆盖：一笔收入 / 收入+成本（含 6401 存量成本科目）/ 期间费用 / 所得税+营业外收支 / 红冲收入 /
本期与累计两段窗口 / 空期间（纯逻辑 + 真库）/ 勾稽平 / 未归类 5111 不平且点名 /
真库「净利润 218 = 权益变动 218 = 资产负债表未分配利润变动 218」/ period 校验 / 期间不存在 /
人为改数不平 −400.00 / 0.0001 不被抹平 / 公式引用未知项报错 / 配置一致性 /
分录被删但两侧恰好相等仍点名凭证。

## 3. 端到端（隔离库 `cloudpivot_erpr2v` + 真 HTTP）

```bash
bash scripts/verify/erpr2_income_statement.sh
# ⇒ PASS=70  FAIL=0
```

夹具（`ERPR2V`，4 期间 / 7 凭证 / 14 分录，含一张真红字凭证 `is_red=1` + `reverse_of`）：

| 期间 | 分录 | 金额 |
|------|------|------|
| 2026-09 | 借 1002 / 贷 3001 资本投入；借 1002 / 贷 5001 收入 | 10,000 / 3,000 |
| 2026-10 | 借 1002 / 贷 5001 收入；红字：借 5001 / 贷 1002 退回 | 5,000 / 500 |
| 2026-10 | 借 5401 / 贷 1405 成本；借 5602 / 贷 1002 管理费用；借 5801 / 贷 2221.02 所得税 | 2,000 / 800 / 300 |

真机接口值（与脚本断言逐一对应）：

```text
本期   营业收入 4,500  营业成本 2,000  管理费用 800  营业利润 1,700  净利润 1,400
累计   营业收入 7,500  营业成本 2,000  营业利润 4,700  净利润 4,400
勾稽   净利润（累计）4,400 = 未分配利润本年变动 4,400，diff 0.00，balanced true
资产负债表（同一份数据）未分配利润 4,400；资产总计 14,700 = 负债+权益 14,700
反例   2026-13 → 400+10001；2026-12（空月份没开账）→ 400+66010；无令牌 → 401
未归类 加一张 借 1002 / 贷 5111 250 → balanced false、diff −250.00、hints 点名 5111、
       报表多出 UNMAPPED_PROFIT_LOSS 行 250；删掉该凭证 → 回绿
导出   xlsx PK 魔数 + attachment + 中文文件名
```

## 4. 页面（Playwright，打隔离实例 5189，不碰生产）

```bash
# 隔离实例：prod 库副本 cloudpivot_e2e_iso + 后端 18089 + vite 5189
E2E_ISO_SRC=local bash scripts/verify/e2e_isolated_instance.sh up
cd frontend
node ../scripts/verify/erpr2_page_check.mjs ok      # ⇒ PASS=20  FAIL=0
node ../scripts/verify/erpr2_page_check.mjs broken  # ⇒ PASS=19  FAIL=0
```

一致态（夹具：一笔 借 1002 / 贷 5001 1,000）：

```text
标题「利润表」；期间「2026年10月（未结账）」；「本年累计：2026-01-01 起」
表头 项目 / 本期金额 / 本年累计；13 行（12 + 未归类行）
一、营业收入 / 减：营业成本 前缀正确；净利润行带 is-row--total（加粗）
勾稽条 class=is-check--ok，背景 rgb(227, 249, 233)（绿）
净利润（累计）1,000.00 = 未分配利润本年变动 1,000.00，差额 0.00
页面无 JS 报错
```

不平态（再加一笔 借 1002 / 贷 5111 投资收益 250）：

```text
勾稽条 class=is-check--bad，背景 rgb(255, 240, 237)（红）
差额 −250.00；净利润（累计）1,000.00 vs 未分配利润本年变动 1,250.00
提示文案点名 5111；未归类科目清单列出 5111；报表出现 UNMAPPED_PROFIT_LOSS 行 250.00
结论句「不平：…差 -250.00 元（差额不做舍入）」
```

截图：[20-页面-勾稽一致.png](./20-页面-勾稽一致.png)、[21-页面-勾稽不平.png](./21-页面-勾稽不平.png)

## 5. 生产真机（172.18.10.144，库 cloudpivot）

```bash
# 部署：git pull → cargo build --release → pnpm build → systemctl restart cloudpivot
# 迁移日志：NOTICE: V0184 自检通过：利润表报表项 12 个（其中科目取数 9 个）、权限节点 1 个
#           NOTICE: V0184：利润表权限码已种，授权新增 1 行（角色数 9）
#           INFO: 数据库迁移完成: 应用 1 个迁移
bash /tmp/r2_prod_verify.sh    # ⇒ PASS=24  FAIL=0
```

生产数据现状：**11 张 POSTED 凭证，全部是采购/库存/应付类**（1405 / 2202 / 2221.01），
**没有任何损益类发生额**。所以真机利润表本期与累计都是 0.00、未分配利润变动也是 0.00：

```text
勾稽一致：净利润（本年累计）0 = 资产负债表「未分配利润」本年变动 0
2026-11（有 2 张凭证但没开账）→ period_status DERIVED，仍勾稽平
空月份 2027-06 → 400 + 66010；2026-13 → 400 + 10001
导出 xlsx 6,345 字节（PK 魔数、spreadsheetml）
```

> 「净利润 = 未分配利润变动」这条在**有损益数据**的情形下由 §3 的隔离库脚本
> （4,400 = 4,400）与 §2 的真库单测（218 = 218）钉住；生产库当前没有损益发生额，
> 所以只能证明「0 = 0 且不误报」。

## 6. 不变量套件

```bash
cd /opt/cloudpivot && set -a && . /etc/cloudpivot/cloudpivot.env && set +a
python3 scripts/verify/erp_invariants.py
# ⇒ 结果：31/32 通过   🚨 BLOCKER 0   ⚠️ WARN 1（F2 凭证期间无记录，历史数据，删前同此）
```

## 7. 可复核命令清单

```bash
# 迁移幂等 / 可回滚（隔离库 cloudpivot_erpr2v 上实测）
psql -f backend/migrations/business/V0184__erp_income_statement.up.sql     # INSERT 0 0（幂等）
psql -f backend/migrations/business/V0184__erp_income_statement.down.sql   # 删 12 报表项 + 1 权限节点
psql -f backend/migrations/business/V0184__erp_income_statement.up.sql     # 重新种上（12 项）
```
