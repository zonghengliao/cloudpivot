# 附件说明

| 文件 | 是什么 |
|---|---|
| `shell-sink-evidence.md` | **主证据**：行数对比、70 路由逐元素计算样式比对方法、98→14 收敛过程、22 页像素 diff 表、两个「看似等效实则不等效」的坑、两处有意收敛 |
| `shell-drift-inventory.md` | **后续卡的材料**：改造后仍留在各页的 198 个本地变体，逐类逐变体逐文件，标出差在哪个属性 |
| `account-before.png` / `account-after.png` | 财务·会计科目（树表，不进 ListPage）：**两张字节完全相同** = 像素一致 |
| `report-item-before.png` / `report-item-after.png` | 财务·报表项（整表顺序即语义，不进 ListPage）：**字节完全相同** |
| `inventory-before.png` / `inventory-after.png` | 未迁页面·库存台账（本次删了 15 块）：**字节完全相同** |
| `cost-period-before.png` / `cost-period-after.png` | 有意收敛 1/2：`#head-extra` 提示文字 14px 深色 → 12px 灰（该页从未定义 `.panel-head__tip`） |
| `price-inquiry-before.png` / `price-inquiry-after.png` | 有意收敛 2/2：空态图标 16px → 28px（该页从未定义 `.panel-empty*`） |

截图条件：本地 dist（改造前 / 改造后各构建一次）+ `/api` 反代生产 nginx（172.18.10.144:80），
`/api/business/**`、`/api/system/dept/**` 打桩返空、`auth/me` 授 `*:*:*`（菜单接口走真后端），
视口 1600×1000、CSP 时区、`deviceScaleFactor=1`。**两轮打桩数据完全相同**，所以差异只可能来自 CSS。
