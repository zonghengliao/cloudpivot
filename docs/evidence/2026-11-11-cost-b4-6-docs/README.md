# COST-B4-6 · 成本核算文档与二开指引（证据）

卡：`t_721a3879`（COST-B4-6【成本】文档与二开指引）· 批次 B4 收口
交付物：`docs/后端二开指南.md` §十六「成本核算」+ `skill/SKILL.md` 速查 + `scripts/verify/cost_doc_check.py`

## 为什么有这份证据

文档卡最难验收的两件事：

1. **写了没用** —— 新人照着找不到文件；
2. **写了过期** —— 代码改了文件/函数名，文档还指着旧名字。

所以本卡把「文档里每一个文件 / 函数 / 迁移号都能在仓库里落地」做成可重跑的判据
（`scripts/verify/cost_doc_check.py`），并附正例 + 负例两份实跑输出。
判据本身不是「看起来对」：负例文档（含 1 个不存在文件 + 1 个不存在函数 + 1 个不存在迁移号）
必须 exit 1，否则闸是空的。

## 文件

| 文件 | 内容 |
|---|---|
| `01_doc_check.out` | 正例：`python3 scripts/verify/cost_doc_check.py` → `DOC_OK`，34 文件 / 8 函数 / 14 迁移号全部可达 |
| `02_doc_check_negative.out` | 负例：临时文档里放 3 处死链 → 三条 `[FILE]/[FN]/[MIG]` 全部命中，exit 1 |
| `03_section_outline.out` | 章节结构（16.0~16.6）与验收四项的对应关系 |

## 复跑

```bash
cd /opt/cloudpivot
python3 scripts/verify/cost_doc_check.py                 # 期望 exit 0 + DOC_OK
python3 scripts/verify/cost_doc_check.py <负例.md>        # 期望 exit 1 + 死链明细
```

## 验收四项 → 章节

| 卡面要求 | 章节 |
|---|---|
| 表结构 | §16.1（13 张表 + 4 类存量改造 + 四条全局口径） |
| calc 六段口径 | §16.2（段/函数/读写/拦什么码 + 段序契约 + 重算口径 + 算例 A 金额） |
| 常见错误 | §16.4（`66055`~`66079` 全表：HTTP / 触发 / 怎么修 + 排查顺序） |
| 如何加新的分配标准 | §16.5（六步，含「漏一条就试算对结算错」的两条取数路径） |
| SKILL.md 5 行速查 | `skill/SKILL.md`「成本核算速查」 |
| 30 分钟找到改哪里 | §16.0 定位表（16 行：改什么 → 文件 → 入口）+ 文末自检命令 |
