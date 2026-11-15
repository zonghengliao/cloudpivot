# P3-17 前端三态（loading / empty / error）实测证据

日期：2026-11-16 · 卡片：t_61a49a3e · 机器：99（开发机）跑 UI，数据来自 145 的克隆库

## 一、验的是什么

新建 `frontend/src/components/StateView/`（组件 + 判据纯函数 + 10 条单测），并在 3 处试点：

| 试点 | 位置 | 三态落点 |
|:--|:--|:--|
| 通用列表页（54 页共用） | `components/ListPage/index.vue` | 加载失败整块 error + 默认空态进 `t-table` 的 `#empty` |
| 甘特图 | `views/business/task/gantt.vue` | 首屏骨架屏 / 时间窗内无可排期任务 / 拉数失败 |
| 流程联动规则 | `views/business/flow/linkage/index.vue` | 表格内空态 / 载入失败 |

## 二、真机证据（隔离实例：145 克隆库 + 本机 release 后端:18094 + vite:5189）

登录 `admin/Admin@123456`（克隆库口令用 argon2id 同参数哈希重置）。三态都用**真接口**制造：

| 图 | 页面 | 制造方式 | DOM 断言（实测输出） |
|:--|:--|:--|:--|
| `01-list-empty.png` | `/support/log` 操作日志 | 关键词灌 `ZZZ-绝对不存在` | `data-state="empty"` · `testid=log-empty` · `.t-empty` 在 · 标题「没有匹配的记录」 |
| `02-list-error.png` | 同上 | CDP `Network.setBlockedURLs(['*/api/support/log/page*'])` | `data-state="error"` · `testid=log-error` · 标题「日志加载失败」· 描述含请求 ID · 按钮「重试」 |
| `03-list-retry-ok.png` | 同上 | 解除拦截后点「重试」 | error 块消失，`共 9688 条`、本页 10 行 |
| `06-gantt-loading.png` | `/task/gantt` | CDP `Fetch.enable` 挂起 `*/api/business/task/gantt?*` | `data-state="loading"` · `testid=gantt-loading` · 骨架 8 行 · 文案「正在加载甘特数据…」 |
| `07-gantt-error.png` | `/task/gantt` | `Network.setBlockedURLs(['*/api/business/task/gantt?*'])` | `data-state="error"` · 标题「甘特图加载失败」· 按钮「重试」；重试后回到画布（`共 2 条`） |
| `08-linkage-empty.png` | `/flow/linkage` | 规则名筛 `ZZZ-不存在的联动规则` | `data-state="empty"` · `testid=linkage-empty` · 单处空态（页面里「暂无数据」出现 **0** 次） |
| `09-linkage-error.png` | `/flow/linkage` | `Network.setBlockedURLs(['*/api/business/flow/linkage*'])` | `data-state="error"` · 标题「联动规则加载失败」· 表格被错误块替换 · 重试后恢复 |

## 三、命令与输出

```text
$ cd frontend && pnpm test:unit
# tests 1863 / pass 1863 / fail 0

$ npx tsx --test tests/unit/state-view.test.ts
# tests 10 / pass 10 / fail 0

$ pnpm typecheck          # vue-tsc --noEmit → 无输出（通过）
$ pnpm typecheck:unit     # 同上（含 tests/unit）
$ pnpm build              # vite build 完成，产物列表正常

$ E2E_BASE_URL=http://127.0.0.1:5189 E2E_USERNAME=admin E2E_PASSWORD=Admin@123456 \
    npx playwright test e2e/34-error-boundary.spec.ts --grep "接口 500"
# ✓ 1 passed (2.5s)
```

## 四、过程中踩到的两个真坑（都改了代码/文档）

1. **`t-skeleton` 的 `rowCol` 每项必须是对象**，传 `'100%'` 字符串时 TDesign 内部做 `'width' in item`
   会抛 `TypeError: Cannot use 'in' operator to search for 'width' in 100%`。
   现象极具误导性：甘特图整块画不出来、`onMounted` 都没跑（连带接口一次没发）、
   控制台只有 `unmountComponent` / `nextSibling` 这类**次生**报错。已在 pure 层返回 `{ width }`，
   并加单测复刻 TDesign 的 `'width' in cell` 判据。
2. **`e2e/34` 的 `.t-alert` 首命中的是 `ImportEntryButton` 里常驻的 hidden `.import-entry__tip`**，
   于是这条用例在改造前后都是红的（技能库里也登记过）。选择器补 `:visible` 后转绿 —— 断言口径没变。

## 五、刻意不做的

- **不改 ListPage 的刷新态**：表格刷新仍用 `t-table` 自己的转圈；换成骨架屏会让「翻页/筛选」时整表闪没。
- **不动各页自定义的 `#empty` 槽**：如物料列表的「没有匹配的物料 / 换个关键词 / 分类试试」是业务口径，
  StateView 只兜默认态（页面自定义仍以页面为准）。
