# P2-6c 证据：标准流程卡片墙并入流程定义页 Tab + `/flow/templates` redirect + 术语下线

> 决策来源：`docs/architecture/P2-6-模板实体-决策.md` §3.2 —— 选 **A：合并概念**。
> 「模板」不是实体，是 `biz_flow_def.is_system = TRUE` 这个角色，术语统一为 **「标准流程」**。
> 依赖 `t_850c7eae`（P2-6b：接口已支持 `scope=standard|all`、默认 `standard`）。

## 改了什么

| 文件 | 动作 |
| --- | --- |
| `frontend/src/views/business/flow/components/StandardFlowWall.vue` | **新增**：卡片墙从 `templates/index.vue` 抽成组件；`data-testid="tpl-*"` **一个不改**（13 → 13，逐条 uniq 对比为空）；术语全部改「标准流程」；写操作只有「查看流程图 / 另存为 / 停用或启用」 |
| `frontend/src/views/business/flow/def/index.vue` | 顶部加 `t-tabs`：**我的流程 / 标准流程**；`?view=standard` 是唯一真源（切 Tab 写回 query、直链直达首次渲染就落在标准流程）；标准流程 Tab 挂 `StandardFlowWall`，墙上的启用停用/另存为会通知表格重拉 |
| `frontend/src/router/index.ts` | `path: 'flow/templates'` 由「组件」改成 `redirect: '/flow/def?view=standard'`（保留一版，下版本连同 `sys_menu` 软删一起删） |
| `frontend/src/api/business/flow-api.ts` | `getFlowTemplates(scope?: 'standard' \| 'all')`；函数名 / 端点名保留，注释写明「legacy 别名：语义 = 标准流程列表」 |
| `frontend/src/views/business/flow/templates/index.vue` | **删除**（独立「流程模板库」页下线） |

未动：后端 Rust / 迁移 / `sys_menu`（菜单软删是 P2-6d）、`/def/templates` 端点名、模板表（不存在也不需要）。

## 验收逐条

| 验收项 | 结果 | 证据 |
| --- | --- | --- |
| `pnpm typecheck` 零错 | ⚠️ **全仓 2 条红，均来自别人已提交的 FIN-2（`0eb8b81`），本卡改动 0 新增** | `typecheck.txt`：① 干净 HEAD 1099aed（**不含本卡改动**）同样 2 条 → ② HEAD + 本卡改动 → ③ 只在本地补 FIN-2 漏掉的 2 行映射后 `exit=0`（**shim 未提交**） |
| 真机截图 3 张 | ✅ | `shots/01-def-mine-tab.png`、`shots/02-def-standard-tab.png`、`shots/03-old-route-redirect.png`（每张左下角压了 URL + 关键计数的横幅） |
| 卡片墙上没有编辑/删除，只有另存为 / 看流程图 / 启用停用 | ✅ 真实 DOM 上枚举出的按钮只有 `["查看流程图","另存为","停用"]`（或「启用」） | `e2e-run.txt` 第 10-11 条 |
| `data-testid="tpl-*"` 仍在、条数一致 | ✅ 13 → 13，逐条 uniq 对比 `diff 为空` | `testid-count.txt` |
| `git commit`（只提交本卡改动） | ✅ | 见下方 commit |

真机断言：**21/21 PASS，0 FAIL**（`e2e-run.txt`）。其中

- ① 我的流程 Tab：`URL=/flow/def`、两个 Tab 存在、默认激活「我的流程」、表格真实数据行、卡片墙不渲染；
- ② 标准流程 Tab：切完 `URL=/flow/def?view=standard`、`共 11 条标准流程`、11 张卡每张都有「标准」标签、墙内「模板」0 次 / 「标准流程」3 次、`tpl-*` 节点 129 个；
- ③ 老地址 `/flow/templates`：落到 `/flow/def?view=standard`、`title=流程定义`、「标准流程」Tab 激活、卡片墙 11 张（没有 404 页）；
- ④ 网络：卡片墙只打 `GET /api/business/flow/def/templates?scope=standard`（200）；
- ⑤ 无未捕获 JS 异常；4xx 只有全站既有的 `/notice/stream?token=` 401 ×2（SSE 令牌通道未落地，与本卡无关）。

## 环境（怎么复跑）

- 后端：隔离库 `cloudpivot_p26c`（`pg_dump cloudpivot_manual | psql` 克隆，13 条流程定义 / 11 条标准流程），
  二进制 = HEAD `1099aed` 编出的 `cloudpivot`，`NIK_PROFILE=test`（`config-test.toml` 才有 `jwt_auth=true`），监听 `127.0.0.1:18126`。
- 后端口径实测（`backend-scope.txt`）：默认 = `?scope=standard` = 11 条；`?scope=all` / `?include_mine=1` = 13 条；`?scope=bogus` = `400 scope bogus 非法（只允许 standard / all）`。
- 前端：`BACKEND_ORIGIN=http://127.0.0.1:18126 vite --port 5261`（**源码即产物**，不在 `/opt/cloudpivot/frontend` 里 build，不碰生产 dist）。
- 真机脚本：`verify.mjs`（Playwright Chromium，1440×1100 之上 1600×1100，令牌走 `localStorage` 注入），证据图落在 `shots/`。

## 已知残留（不属本卡，已开卡）

1. **`pnpm typecheck` 全仓 2 条红**：`frontend/src/api/business/erp/attachment.ts:43 / :204` —— FIN-2 的 `0eb8b81` 把 `FIXED_ASSET` 加进 `DocType`，但两张 `Record<DocType, …>` 映射表没跟上。修法就 2 行：
   `FIXED_ASSET: DOC_KIND_LABELS.FIXED_ASSET` 与 `FIXED_ASSET: 'OTHER'`。已开跟进卡。
2. **侧边栏仍显示「流程模板库（标准模板 / 启用停用 / 另存为）」**：那是 `sys_menu` 种子（V0194 …07a5），按卡面归 P2-6d 软删，**必须等本卡上线后再做**，否则用户直接失去入口。
3. **单测 2 条常红**（`c2-approval-unified.test.ts` ③/③-补）：干净 HEAD 上同样红，与本卡无关（`unit-tests.txt` 前后对照 fail 列表逐条相同）。
