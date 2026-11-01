# ERP-R3 现金流量表 · 验收证据（2026-11-01）

> 卡：`t_e2f972bd【ERP-R3】现金流量表（简化版：经营/投资/筹资，勾稽货币资金，10 单测）`
> 交付说明 + 运维手册：`../../ERP-R3-现金流量表.md`
> 本目录只放**可复核的证据**（命令 + 原始输出摘要），截图仅留痕；页面结论一律以
> **DOM 类名 / 背景色 / 文案断言** 为准（不依赖人眼看图）。

## 0. 一句话结论

| 项 | 结果 |
|----|------|
| 全量单测 | **1725 passed / 0 failed**（全新库 `cloudpivot_r3v3`，`--test-threads=1`，541.69s；= R2 的 1705 + 本卡 20） |
| 本卡单测 | **20 条**（`business::erp::cash_flow`，其中 4 条真库用例） |
| 端到端（隔离库 + 真 HTTP） | **85 PASS / 0 FAIL**（`scripts/verify/erpr3_cash_flow.sh`） |
| 页面（Playwright，DOM 断言） | 一致态 **33 PASS**／不平态 **25 PASS**，均 0 FAIL |
| 生产真机（144，2026-10 现有 11 张凭证） | **31 PASS / 0 FAIL**（`scripts/verify/erpr3_prod_verify.sh`） |
| ERP 不变量套件 | **31/32，BLOCKER 0，WARN 1**（F2 历史数据，R1/R2 交付时同此） |
| fmt / clippy / vue-tsc | 全部干净（`cargo fmt -p cloudpivot --check`、`clippy --lib -- -D warnings`、`vue-tsc --noEmit`） |

## 1. 全量单测（全新库）

```bash
# 建库 → 起一次实例把迁移跑到最新（含 V0185）→ **等 bootstrap 种完 admin/角色** → 杀进程 → 跑全量
bash /tmp/r3_mkdb.sh        # NOTICE: V0185 自检通过：现金流量表分类行 12 个、权限节点 1 个
                           # INFO: 数据库迁移完成: 应用 129 个迁移
                           # users=1 health=200  ⇒ READY cloudpivot_r3v3
cd backend && ERP_DB_URL=postgres://alun:***@127.0.0.1:5432/cloudpivot_r3v3 \
              SYS_DB_URL=$ERP_DB_URL NIK_PROFILE=test \
              cargo test --lib -- --test-threads=1
# ⇒ test result: ok. 1725 passed; 0 failed; 0 ignored; ...
#    finished in 541.69s      （见 00-full-test-summary.txt / 00-full-test.log.tail）
```

> ⚠️ 踩坑（本卡真的踩了）：建库脚本**跑完迁移就 kill** 会留下 `65018 本节点没有有效审批人，且平台
> 没有可兜底的超级管理员` —— 71 条审批/调拨类用例假红（第一轮 1654 passed / 71 failed）。
> 修法：等 `/health == 200` **且** `sys_user` 非空（bootstrap 种完 admin）再杀进程；
> 重跑同一批用例 ⇒ 1725 passed / 0 failed。

## 2. 本卡单测（20 条）

```bash
ERP_DB_URL=... cargo test --lib business::erp::cash_flow -- --test-threads=1
# ⇒ test result: ok. 20 passed; 0 failed
```

覆盖：单笔收入→经营流入 / 单笔支出→经营流出 / 经营净额=流入−流出 / 买设备→投资流出 /
借款→筹资流入（反向→偿还）/ 交税→经营流出 / 期末=期初+净增加 / 期末=资产负债表货币资金（平与差 250）/
未分类→「其他」+warnings / 空期间全 0 且期初=期末 / 复合凭证按对方科目拆分 / 分录被删→差额进「其他」+点名凭证 /
现金内部划转不影响现金流 / 0.0001 不被抹平 / 12 行配置自洽 / period 校验 /
真库 6 张凭证（经营 368 / 投资 −300 / 筹资 500 / 净增 568 = 期末 = 货币资金 = 总账现金类余额）/
真库分录被删 500 进「其他」/ 真库配置 12 行 + 非现金科目全覆盖 / 真库空期间。

## 3. 端到端（隔离库 `cloudpivot_erpr3v` + 真 HTTP）

```bash
bash scripts/verify/erpr3_cash_flow.sh
# ⇒ PASS=85  FAIL=0          （原始输出：01-e2e-erpr3_cash_flow.out）
```

夹具（`ERPR3V`，4 期间 / 7 凭证 / 14 分录）：

| 期间 | 分录 | 金额 | 归类 |
|------|------|------|------|
| 2026-09 | 借 1002 / 贷 3001 股东投入 | 10,000 | 筹资流入（只进本年累计） |
| 2026-10 | 借 1002 / 贷 5001 销售收款 | 5,000 | 经营流入 |
| 2026-10 | 借 5401 / 贷 1002 采购付款 | 2,000 | 经营流出 |
| 2026-10 | 借 2211 / 贷 1002 发工资 | 800 | 经营流出 |
| 2026-10 | 借 2221 / 贷 1002 交税 | 300 | 经营流出 |
| 2026-10 | 借 1601 / 贷 1002 买设备 | 1,500 | 投资流出 |
| 2026-10 | 借 1002 / 贷 2501 借款 | 4,000 | 筹资流入 |

真机接口值（与脚本断言逐一对应）：

```text
本期   经营流入 5,000  经营流出 3,100  经营净额 1,900
       投资净额 −1,500  筹资净额 4,000  净增加额 4,400
       期初 10,000 → 期末 14,400
累计   经营净额 1,900  筹资净额 14,000（含 9 月 10,000）  净增加 14,400  期末 14,400
勾稽   期末 14,400 = 资产负债表「货币资金」14,400 = 总账现金类科目余额，diff 0.00，balanced true
       check.cash_entry_count = 7（本期现金分录 7 笔）
反例   2026-13 → 400+10001；2026-12（空月份没开账）→ 400+66010；无令牌 → 401
口径差 加一张 借 1012 其他货币资金 200 / 贷 1002 200：
         「其他」流出 200、净增加 4,200、期末 14,200
         warnings 2 条（点名「未能分类」+ 1012）、hints 指出 1012 口径
         balanced false、diff −200.00（= 期末 14,200 − 货币资金 14,400）
         删掉该凭证 → 回绿（warnings []、diff 0）
导出   xlsx PK 魔数 7,135 字节 + attachment + 中文文件名（RFC 5987）
```

## 4. 页面（Playwright，打隔离实例 15173，不碰生产）

```bash
# 隔离实例：144 生产库 dump → 副本库 cloudpivot_r3page + 后端 18097 + vite 15173
bash scripts/verify/erpr3_page_up.sh ok
cd frontend && node ../scripts/verify/erpr3_page_check.mjs ok        # ⇒ PASS=33 FAIL=0
bash scripts/verify/erpr3_page_up.sh broken
cd frontend && node ../scripts/verify/erpr3_page_check.mjs broken    # ⇒ PASS=25 FAIL=0
```

一致态（7 张正常凭证）：

```text
标题「现金流量表」；期间「2026年10月（未结账）」；「本年累计：2026-01-01 起」
表头 项目 / 本期金额 / 本年累计；简化口径说明直接显示在页头
一、经营活动… / 二、投资活动… / 三、筹资活动… 三个分区标题
经营净额 1,900.00 / 投资净额 -1,500.00 / 筹资净额 4,000.00
净增加额 4,400.00；期初 10,000.00；期末 14,400.00
勾稽条 class=is-check--ok，背景 rgb(227, 249, 233)（绿）；差额 0.00
「其他」分区不出现；无黄条；页面无 JS 报错
```

不平态（再加「借 1012 / 贷 1002 200」）：

```text
勾稽条 class=is-check--bad，背景 rgb(255, 240, 237)（红）；差额 -200.00
期末 14,200.00 vs 资产负债表货币资金 14,400.00
顶部黄条出现且点名「未能分类」+ 1012；勾稽 hints 点名 1012 口径差异
「其他」分区出现，净额 -200.00；未分类 1 笔
结论句「不平：…差 -200.00 元（差额不做舍入）」
```

截图：[30-页面-现金流量表-勾稽一致.png](./30-页面-现金流量表-勾稽一致.png)、
[31-页面-现金流量表-未分类与口径不平.png](./31-页面-现金流量表-未分类与口径不平.png)

## 5. 生产真机（172.18.10.144，库 cloudpivot）

```bash
# 部署：git fetch+merge --ff-only origin/main → cargo build --release → pnpm build → systemctl restart cloudpivot
# 迁移日志：NOTICE: V0185：现金流量表权限码已种，授权新增 1 行（角色数 9）
#           NOTICE: V0185 自检通过：现金流量表分类行 12 个、权限节点 1 个
#           INFO: 数据库迁移完成: 应用 1 个迁移      health=200
bash /tmp/erpr3_prod_verify.sh      # ⇒ PASS=31  FAIL=0（原始输出：04-prod-verify.out）
```

生产数据现状：**11 张 POSTED 凭证，全部是采购 / 库存 / 应付类**（`1405 / 2202 / 2221.01`），
**没有一张走现金类科目（1001/1002）**，所以真机本期 / 累计全 0：

```text
期末余额 0 = 期初 0 + 净增加 0 = 资产负债表「货币资金」0 = 总账现金类科目余额 0
勾稽 balanced true、diff 0.00、warnings []
未分类 0 笔 ⇒ 不出现「未能分类」告警（成对校验）
导出 xlsx 7,039 字节（PK 魔数、spreadsheetml、attachment）
菜单树 GET /api/system/menu/tree → code 0，含「现金流量表」节点
前端 dist：assets/*.js 内含「现金流量表」页面（点菜单出得来）
```

> 「期末 = 货币资金」在**有现金发生额**的情形下由 §3 的隔离库脚本（14,400 = 14,400）与 §2 的真库单测
> （568 = 568）钉住；生产库当前没有现金类发生额，所以只能证明「0 = 0 且不误报」—— 与 ERP-R2 的
> 已知缺口同性质，如实记录。

> ⚠️ **部署踩坑（新发现，已写进交付文档）**：菜单是由迁移直接写 `sys_menu` 的，而 `/system/menu/tree`
> 有 1 小时的 Redis 缓存（`cloudpivot:menu:tree:all`，另有 `cloudpivot:perm:user_menus:<uid>`）。
> 重启服务**不会**清 Redis，所以新菜单节点在缓存过期前不会出现在菜单树里（本卡首次真机校验就是
> 「菜单树里没有「现金流量表」」红 1 项）。处置：部署含菜单的迁移后清这两个 key（只删缓存，安全）。

## 6. 不变量套件

```bash
cd /opt/cloudpivot && set -a && . /etc/cloudpivot/cloudpivot.env && set +a
python3 scripts/verify/erp_invariants.py
# ⇒ 结果：31/32 通过   🚨 BLOCKER 0   ⚠️ WARN 1（F2 凭证期间无记录，历史数据）
```

## 7. 可复核命令清单

```bash
# ① 迁移幂等 / 可回滚（隔离库 cloudpivot_erpr3v 上实测）
psql -f backend/migrations/business/V0185__erp_cashflow.up.sql     # INSERT 0 0（幂等，自检通过）
psql -f backend/migrations/business/V0185__erp_cashflow.down.sql   # 删 12 分类行 + 权限节点 + 表
psql -f backend/migrations/business/V0185__erp_cashflow.up.sql     # 重新种上（12 行）

# ② 复跑本卡全部验证
cd backend && cargo fmt -p cloudpivot -- --check && cargo clippy --lib -- -D warnings
ERP_DB_URL=... cargo test --lib business::erp::cash_flow -- --test-threads=1
cd .. && bash scripts/verify/erpr3_cash_flow.sh
bash scripts/verify/erpr3_page_up.sh ok && cd frontend && node ../scripts/verify/erpr3_page_check.mjs ok
```
