# MENU-FIX-2：菜单迁移规范进规则 + 不变量脚本加 ⑥⑦ 断言

- 卡：`t_93e9e2f0` ｜ 分支 main ｜ 上游：MENU-FIX-1（`t_95bc00b0`，V0335 修数据）
- 改动文件：`docs/编码规范.md`、`docs/README.md`、`docs/AI二开指引.md`、
  `docs/新机器部署-纯迁移重建.md`、`scripts/verify/menu_invariants.sh`、`scripts/verify/README.md`
- 实测环境：145 测试（`cloudpivot_dev`，423 条菜单）｜ 144 生产（`cloudpivot`，423 条）｜
  145 临时库（`menu_inv_teeth2`，验证断言有牙，验完已删）

## 0. 结论先行

| 验收项 | 结果 |
|---|---|
| ① 规则落到哪个文件、加了多少行 | `docs/编码规范.md` 新增 **§1.6.1 菜单（sys_menu）迁移规范**（46 行）；`docs/README.md` §三 加第 10 条速记（1 行）；`docs/AI二开指引.md` 反魔幻清单加 1 条 DON'T（3 行）；`docs/新机器部署-纯迁移重建.md` 断言数 5→7 |
| ② 脚本加 ⑥⑦ 且在 145 跑过 | 已加（`scripts/verify/menu_invariants.sh`，+~50 行含注释）；145 **7/7 全绿 rc=0**，144 生产同样 **7/7 全绿 rc=0** |
| ③ 与 MENU-FIX-1 的依赖 | ⑤⑥⑦ **依赖 MENU-FIX-1 的数据修复已上线**（V0335）。本卡交付时 145/144 都已跑到 V0335，所以 ⑥⑦ 是绿的；若拿本脚本去跑**未打 V0335 的库**（例如某台没部署的旧环境），⑥=25、⑦=5 —— 那是 MENU-FIX-1 的数据面，不是断言判错 |

## 1. 「C7 规则」落点考证（为什么没落在叫 C7 的文件里）

卡面让找「C7 规则的落点」。全仓检索（`*.md` / `*.sh` / `*.rs`，排除 lock 与 node_modules）后结论：

- **仓库里不存在名为 C7 的规则文件**；`C7` 的唯一实体是 `scripts/check.sh` 的**批次 C · C7 门禁族**
  （G1–G6，主题是 `business/changelog` 回滚零残留），另有 `docs/批次C-技术架构.md` 的实施拆分表 C7 行
  （测试专项）。两者都与菜单无关，把菜单规范塞进 changelog 门禁组会污染那条门禁的语义。
- 因此规则落到**真正会被读、且已有闸门执行**的三处：
  1. `docs/编码规范.md` §1.6.1 —— 强制规范底稿 + 提交前 checklist（`docs/README.md` 文档地图里"编码规范"就是这一本）；
  2. `docs/README.md` §三 核心约定 第 10 条 —— 文档总入口的速记位（原来 9 条，菜单迁移补第 10 条）；
  3. `docs/AI二开指引.md` 反魔幻清单 DON'T —— AI 生成迁移前必读，跟「不要新建 V0018」同一层。
- **`AGENTS.md` 未能修改**：它是受保护文件，写入需要人工审批，本卡是 headless 运行（无人应答 → 超时拒绝）。
  建议人工补一行到 `AGENTS.md` 的「高频踩坑（先读）」末尾：

  ```markdown
  - 侧栏菜单名换行/截断：`sys_menu.title` 被写成功能说明就会这样。`title` 只写 2-6 字名词短语（硬顶 12 字），
    同父 `sort_order` 唯一；规则见 `docs/编码规范.md` §1.6.1，闸门 `scripts/verify/menu_invariants.sh` ⑥⑦。
  ```

## 2. 规范正文要点（`docs/编码规范.md` §1.6.1）

- `name` = 唯一标识/权限码关联（短名，`business:erp:report:xxx` 这类编码形式属历史遗留，新增不用）；
  `title` = **侧边栏显示名**，2-6 字名词短语，不带标点/说明/章节引用，硬顶 12 字。
- 三条禁止：功能说明进 `title`、引用 `ARCH-7 §15.6.3` / 卡号 `P2-01`、箭头斜杠括号堆流程。
- 功能说明去处：`description` 列 → `docs/manual/` → 前端 tooltip。
- 同父 `sort_order` 唯一：插入间隔 10 递增，后续插空用 15/25。
- **改历史菜单只许新开迁移**（幂等 `UPDATE`，不回改历史迁移），末尾补 `DO` 自检块。
- 附两条校验 SQL（=⑥⑦ 的口径，两个查询都应为 0 行）。

## 3. 脚本增强（`scripts/verify/menu_invariants.sh`）

新增两条断言（退出码语义不变：0 全绿 / 1 有违规 / 2 连库或查询失败）：

| # | 断言 | SQL 口径 |
|---|---|---|
| ⑥ | `title` 长度 | `menu_type IN (1,2)` 且 `length(title) > 12`（`MENU_TITLE_MAX` 可覆盖）⇒ 0 |
| ⑦ | 同父 sort 唯一 | `menu_type=2` 按 `(parent_id, sort_order)` 分组 `having count(*)>1` ⇒ 0 组 |

配套：文件头断言清单 5→7、`MENU_TITLE_MAX` 新变量、`scripts/verify/README.md` 断言表补两行 +
新增「⑥⑦ 的口径与已知边界」小节、`docs/新机器部署-纯迁移重建.md` 第 4 步 5 条→7 条。

### 3.1 三端实测（原始输出在 `raw/`）

| 环境 | 结果 | 证据 |
|---|---|---|
| 145 测试 `cloudpivot_dev` | 7/7 ✅ rc=0（423 条菜单，⑥=0 ⑦=0） | `raw/10_invariants_145_dev.txt` |
| 144 生产 `cloudpivot` | 7/7 ✅ rc=0（423 条，⑥=0 ⑦=0 —— 说明 V0335 已在生产生效） | `raw/11_invariants_144_prod.txt` |
| 145 临时库 `menu_inv_teeth2` | 基线 402 行全绿；注入后 **⑥=1 ⑦=1、rc=1**，明细正确打印 | `raw/12_invariants_teeth.txt`（造数与注入 SQL 同名 `.sql`） |

牙齿测试注入的就是卡面点名的真实坏味道：`title = '即时 / 月末汇率维护 + 按月批量录入（ARCH-7 §15.6.3 ②）'`（38 字）
＋ 同父再插一条 `sort_order=10` 的页面。脚本输出：

```
⑥ title 长度:            1  ❌ (≤12 字)
    ↳ …|演示页面|即时 / 月末汇率维护 + 按月批量录入（ARCH-7 §15.6.3 ②）|38|/business/demo/page
⑦ 同父 sort 唯一:        1  ❌
    ↳ …11111111-…111111|10|2|演示页面/即时 / …（ARCH-7 §15.6.3 ②） | 演示页面B/演示页面B
结果: 2 条不变量不通过 ❌（⑥title长度,⑦同父sort唯一）
rc=1
```

临时库已删（`select count(*) from pg_database where datname like 'menu_inv%'` → 0），不碰业务库。

## 4. 已知边界与遗留（不在本卡范围，别顺手改）

1. **`menu_type=3`（按钮权限点）的 `title` 未受约束**：145 实测 **155 条 >12 字，最长 73 字**
   （例：`新建销售退货单（取号 SR-yyyyMMdd-000001；items 留空 = 从原发货单带出可退明细；超退 400 / 发货单未过账 409）`）。
   按钮不进侧栏，但会污染「角色-菜单」授权树的文案。属另开卡范围。
2. **跨类型同 `sort_order`**：顶级 `通用底座`(menu_type=1) 与 `合同管理`(menu_type=2) 同为 75。
   ⑦ 按卡面口径只管 `menu_type=2`，不含跨类型；判红要动历史数据、收益近零，已登记待评估。
3. **`sys_menu.title` 有 1 行为 NULL**（`10000000-0000-0000-0000-000000000051` 工作台概览，menu_type=3）。
   ⑥ 只查 `length(title) > 12`，NULL 不算违规；若将来要管按钮文案，需一并补 `title is null` 口径。
4. **`/root/test_deploy.sh`（145）与 `laoliu/scripts/deploy_check_v5.sh` 的注释仍写「5 条断言」**：
   两个脚本只判 rc（0/1/2），没有硬编码条数，**行为不受影响**；注释过期，属他人脚本目录，未改。
