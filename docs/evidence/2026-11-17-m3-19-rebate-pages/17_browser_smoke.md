# 浏览器冒烟（真实后端 + 真实库）— M3-19 返利两页

机器：99（开发机）。后端 `cargo run`（profile=dev，DB=`cp_be03f4e7_e2e`@172.18.10.145，Redis 本机 6379），
前端 `pnpm dev`（:5173，代理 `/api` → :8023）。全程用真浏览器点，**不是** mock。

## 环境准备（可复现）

```bash
apt-get install -y redis-server && redis-server --port 6379 --daemonize yes --save '' --appendonly no
createdb -h 172.18.10.145 -U cloudpivot cp_be03f4e7_e2e          # 空库
cd backend && NIK_PROFILE=dev NIK_REDIS_URL=redis://127.0.0.1:6379 \
  NIK_DATABASE_HOST=172.18.10.145 NIK_DATABASE_PORT=5432 \
  NIK_DATABASE_USER=cloudpivot NIK_DATABASE_PASSWORD=<pg> NIK_DATABASE_NAME=cp_be03f4e7_e2e \
  NIK_ADMIN_PASSWORD='Admin@123456' \
  NIK_MIDDLEWARE_AUTH_JWT_SECRET=$(openssl rand -hex 32) \
  CLOUDPIVOT_STORAGE_MASTER_KEY=$(openssl rand -hex 32) X_CONFIG_SECRET_KEY=$(openssl rand -hex 16) \
  cargo run            # 空库首启：432 条迁移（含本卡 V0443）自动应用
cd frontend && pnpm dev  # :5173
```

**这一步本身也是证据**：空库首启迁移日志里出现
`V0443 完成：销售返利 /business/erp/rebate-policy 已挂到「销售管理」目录（sort 85…）`
且 `schema_migrations` 记下 `versioned/sales_rebate_menu_page/version=443`
（见 `19_backend_migrate_v0443.txt`）。

## 点过的链路（证据 = 截图）

| 截图 | 说明 |
|---|---|
| `13_browser_policy_list.png` | 侧栏只有一个「销售返利」（销售管理目录下，sort 85）；两 tab「返利政策 / 返利结算」；政策列表有 1 条 `BR-20261117-000001`（按量·平铺 0.5 元/件·月度·2026-11-01~长期·启用），行操作 编辑/停用/删除 |
| `14_browser_policy_form_tiered.png` | 新建政策弹窗：名称 / 客户下拉（只列启用客户）/ 规则类型单选 + 说明 / 计价单位 / 结算粒度 / 生效日 / 状态 |
| `15_browser_policy_tier_editor.png` | 切到「阶梯返利」后表单**自动整形**：计价单位变可选项、档位编辑器出现（第 1 档 0~1000 下限禁用、第 2 档 1000~留空=无上限、费率带「元/件」后缀、+ 添加一档），下方实时档位速览 `第 1 档 0 ~ 1000 ⇒ …` |
| `16_browser_settlement_list.png` | 结算 tab：1 张 `RB-20261117-000001`（CUS-0004、2026-11-01~11-30、政策号、命中档 `第 1 档 0 ~ +∞ ⇒ 0.5 元/件`、应返 0.00、草稿、未开票（草稿）、操作 详情/重算/作废），页脚「本页 1 张 · 净销售额 0.00 · 应返合计 0.00（只合计当前页，不是全量）」 |
| `18_browser_run_batch.png` | 整期跑批：`2026 年 11 月 共 1 家客户；新建 0 张；已有跳过 1 张` + 逐客户 `CUS-0004 返利测试客户甲：已有结算单 RB-20261117-000001（DRAFT），跳过`（幂等成立） |

### 试算两条路径（弹窗内文本，逐字）

1. **政策未覆盖整期**（政策生效日先设为 11-17，期间 2026-11）：
   > 2026 年 11 月：该期间没有覆盖整期的启用政策，无法算出返利（补政策后重算）
   > 客户在 2026-11-01（2026-11 首日）没有生效的月度返利政策：最近一份 BR-20261117-000001（2026 年度返利测试政策）有效期 2026-11-17 ~ 长期，尚未覆盖期间首日。请把生效日改到 2026-11-01 之前，或把期间改成政策真正覆盖的那一期

   ⇒ 后端 `66091` 的报文在页面上原样展示（没被翻成「操作失败」）。

2. **政策覆盖整期后**（`PUT` 把生效日改成 2026-11-01）：
   > 0.00 应返金额 / 2026 年 11 月：应返 0.00（政策 BR-20261117-000001）
   > 基数：订单 0 张 / 0.00、0 件 − 退货 0 张 / 0.00、0 件 = 净额 0.00、净量 0
   > 落档：第 1 档 0 ~ +∞ ⇒ 0.5 元/件
   > ⚠ 期间内没有已确认销售订单、也没有已过账退货（APPROVED / PARTIAL_OUT / OUTBOUND / CLOSED 起算），应返 0
   > 适用政策：BR-20261117-000001 2026 年度返利测试政策 （按量 · 元/件）+ 全量档位

3. **计算并落库（草稿）**：落成 `RB-20261117-000001`（见 `16_*`）；详情抽屉（文本核对）展示
   结算单号 / 状态 / 客户 / 期间 / 期间区间 / 适用政策 / 规则口径 / 政策有效期 / 命中档 /
   应返金额 / 红字发票 / 转结时间 / 基数（订单−退货=净额）/ 备注 + 「政策档位快照」表（档号 1、下限 0、上限 +∞、费率 0.5 元/件）。

## 已知外观说明（不是缺陷）

1400px 视口下结算表列宽合计 > 卡片宽度，`t-table` 横向滚动（`应返金额` / `状态` / `红字发票`
要拖一下才看到）；1920px 下全部可见。列宽已在 160/150/150/160/175/120/90/130 上收敛过一轮。

## 清理

```bash
pkill -x cloudpivot; pkill -f "vite"      # 停后端 / 前端
dropdb -h 172.18.10.145 -U cloudpivot cp_be03f4e7_e2e
redis-cli shutdown nosave                  # 本机临时 Redis
```
