# 菜单不变量第 ⑧ 条：icon 名必须在 TDesign 图标库里存在（卡 t_595098fe）

2026-11-09 · test-enginner

## 交付物

| 文件 | 内容 |
|---|---|
| `scripts/verify/menu_invariants.sh` | 7 条 → **8 条**：新增 ⑧ `icon` 名合法（+ 图标库解析、缺库跳过、形状校验、结论「有跳过」标记） |
| `scripts/verify/icon_invariant_teeth.sh` | ⑧ 的牙齿脚本：S1–S10 共 40 项自校验，注入 → 报警 → 复原一条命令跑完 |
| `scripts/verify/README.md` | 断言清单 8 条 + 新增「⑧ 的口径与已知边界」一节（来源优先级 / 跳过语义 / 基线 / 牙齿） |
| `docs/编码规范.md` §1.6.1 | 菜单列规范补 `icon` 列（名字必须真实存在、字符集、prod/dev 双 path 提醒）+ 自查命令 |
| `docs/evidence/20261109-menu-icon-invariant/run_evidence.sh` | 一键复跑本目录全部证据（145/144 只读，写只发生在一次性副本库） |

## 为什么会漏两次（问题定义）

`sys_menu.icon` 存的是 TDesign 图标**组件名**；名字不存在时 `SideMenuItem.vue` 的
`<t-icon :name="item.icon" />` 渲染**空节点**，侧栏那项没有图标。且 `icon` 非空串 ⇒ 组件里的
`props.item.icon || (hasChildren ? 'folder' : 'app')` 兜底不触发 ⇒ 静默空白。
DB 侧完全「合规」，老 7 条断言一条都碰不到它：
V0170 改了 dev 的 `/business/erp/purchase-order`，生产那条 path 是 `/business/erp/purchase` 没被覆盖，
一直坏到 V0353。

## 判据（最终实现）

`menu_type IN (1,2)` 且 `icon` 非空 ⇒

1. `icon ~ '^[a-z0-9][a-z0-9-]*$'`（TDesign 名字的真实字符集，实测 2356/2356 全匹配）；
2. 并且能在图标库名字清单里找到同名项。

第 1 条是本实现发现的**真实假绿洞**：只做「一行一条」的差集时，`'bill\nmoney'` 会被拆成
`bill` / `money` 两行，两段**各自都是合法名字** ⇒ ⑧ 会绿。所以形状单独判，明细用 `\n`/`\t` 可见化。
（这一条由牙齿 S3b 守着；也是本会话里牙齿脚本**自己抓到的一个真 bug**，不是纸上推演。）

### 图标库来源（先到先用）

1. `MENU_ICON_LIB=<路径>`：`esm/components` 目录 / 一行一个名字的清单 / `manifest.js` / `IconSelect/icons.ts`（自动识别）；
2. `<repo>/frontend/node_modules/tdesign-icons-vue-next/esm/components`（真实安装版本，最权威）；
3. `<repo>/frontend/src/components/IconSelect/icons.ts` 的 `ICON_NAMES`（仓库内清单，2356 条）。

三种来源的可信度实测一致：`components/*.js` 文件名集合 = `manifest.js` 的 `stem` 集合 =
`IconSelect/icons.ts` 的 `ICON_NAMES` 集合，**各 2356 个，三方差集为空**。

### 缺图标库：跳过，但绝不静默绿

`⏭ 跳过（找不到图标库 — 试过：…）` + 「这是没校验，不是校验通过」，结论行改成
`结果: 全绿 ✅，但**有跳过**：⑧icon名(无图标库)`，rc 仍 0（prod/CI/瘦容器没装前端依赖不能报红）。
要强制：`MENU_ICON_LIB_REQUIRED=1` ⇒ 缺库 **rc=2**（基础设施问题）。

## 实测结论

| 场景 | 结果 | 证据 |
|---|---|---|
| 145 dev `cloudpivot_dev`（432 行，60 个 distinct icon） | **8/8 全绿 rc=0** | `raw/02_145_dev.txt` |
| 144 prod `cloudpivot`（431 行，61 个 distinct icon）**V0353 发布前** | **⑧ 1 ❌ rc=1**：`shopping-cart /business/erp/purchase`，其余 7 条 ✅ | `raw/03_144_prod_pre_release.txt` |
| 144 prod **V0353 于 09:36:26 生效后** | **8/8 全绿 rc=0** | `raw/03_144_prod.txt` |
| 本地 HEAD 形状库 `cloudpivot_code09`（427 行） | 8/8 全绿 rc=0 | `raw/01_local_head_db.txt` |
| 本地 **prod 形状**副本 `cloudpivot_r2`（path 是 `/business/erp/purchase`） | ⑧ 1 ❌（同一行）＋⑤（该库只有 286 行，与本条无关） | `raw/04_prod_shape_copy.txt` |
| 老库 `cloudpivot_test_iso2` | ⑧ 报 `invoice /business/erp/expense`（历史错名，新种子已改 `bill`） | `raw/05_old_db_invoice.txt` |
| 牙齿全场景（一次性副本库） | **PASS=40 FAIL=0** | `raw/06_teeth_summary.txt` + `raw/06_teeth/` |

**144 那条是最有力的证据**：7 条老断言全绿、只有 ⑧ 报出生产上真实存在的空白图标，而且点出
坏名与 path；V0353 一落地自动转绿。时间线：

- 09:26:38 本卡开跑（run 1113）
- ~09:31 对 144 只读跑新脚本 ⇒ ⑧ 1 ❌（`shopping-cart`）
- 09:36:26 144 上 V0353 生效（`schema_migrations.applied_at=1794188186`，该行 `sys_menu.updated_at=09:36:26`）
- 09:39 复跑 ⇒ 8/8 全绿

## 牙齿清单（`icon_invariant_teeth.sh`，40 项）

| 场景 | 期望 |
|---|---|
| S1 基线 | rc=0、⑧ ✅、打印图标库来源、无跳过字样 |
| S2 注入 `shopping-cart`（事故原名） | rc=1、⑧=1、打印坏名 + `/business/erp/purchase-order`、结论点名 ⑧ |
| S3 再注入 `invoice` | rc=1、⑧=2、两条 path 都打印 |
| S3b 注入 `bill\nmoney`（两段都是真名字） | rc=1、报「含非法字符」、可见化打印 `bill\nmoney` |
| S4 按**原值**复原 | 两行 icon 回到原值、rc=0、⑧ 回到 ✅ |
| S5 无图标库（fake repo root） | rc=0、打印 ⏭ + 试过的路径、结论写明「有跳过」 |
| S6 无图标库 + `MENU_ICON_LIB_REQUIRED=1` | rc=2 |
| S7 `MENU_ICON_LIB` 给残缺清单 | rc=1、⑧ 报红（>50 条）、来源显示为清单文件 |
| S8 无 node_modules、只有 `IconSelect/icons.ts` | rc=0、来源显示 icons.ts、解析出 2356 个名字 |
| S9 `MENU_ICON_LIB` 指 `manifest.js` | rc=0、解析出 2356 个名字 |
| S10 收尾复核 | rc=0 |

注：`mv frontend/node_modules` 这种验法**没有在本机做** —— 本机 5217 端口上跑着别人的 vite dev
（`ps aux` 可见），挪走 node_modules 会打断它。改用等价且更严格的做法：把脚本放进一个没有
`frontend/` 的 fake repo root（模拟 prod/瘦容器），以及 fake root 里只放 `icons.ts`（模拟有仓库无依赖）。
两条都在 S5/S8 里跑过。

## 复跑

```bash
export PGPASSWORD=... PGUSER=...
bash docs/evidence/20261109-menu-icon-invariant/run_evidence.sh <teeth_db>
# <teeth_db> = 一次性副本库（sys_menu 已 dump）：只有它是可写的，145/144 全只读
```

## 遗留 / 提醒

- `AGENTS.md` 第 67 行仍写「6 条断言」——该文件受保护（写它会弹审批，本次没有审批人），
  已按「README + 脚本头注释 + 编码规范」三处对齐，AGENTS.md 这句请由有权限的人下次顺手改。
- 144 上的 V0353 在 09:36 才生效，因此**在它生效之前的一段时间里**，夜间 `deploy_check_v5`
  若跑到 144 会照实报 `【MENU-STRUCT】`（⑧ 红）——这是正确行为，发布后自动恢复。
- `menu_invariants.sh` 的 ④ 只管 `menu_type=2`；测试库上 `/business/erp/expense` 有 2 条未删记录
  （`费用报销` mt=1 / `money`、`我的报销` mt=2 / `bill`），同 path 跨类型，④ 按既有口径不报。
  与本卡无关，记在这里备查（真要动它得先定「目录自带页面」这条产品口径）。
