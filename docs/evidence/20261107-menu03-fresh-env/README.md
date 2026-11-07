# MENU-03 取证：145 全新库 `cloudpivot_fresh` 纯迁移重建

卡：t_e54be734（MENU-03）· 时间：2026-11-07 · 机器：145（`172.18.10.145`，库账号 `cloudpivot`）

## 文件

| 文件 | 内容 | md5 |
|---|---|---|
| `145-fresh-run-repo-layout.txt` | **仓库目录布局**下 `RECREATE=1 bash scripts/setup_new_env.sh cloudpivot_fresh` 的完整输出（95 行，**rc=0**）——证明提交后的路径解析（helpers / baseline / 规则表）可用 | `89772d6ee5ecd1bbed8a706f9a71a4df` |
| `145-fresh-run.txt` | 同一流程的首次跑（脚本平铺在 `/root`，辅助脚本与基线在 `/root`、`/root/baseline`），95 行 | `2004bba2bc79dd217542ee3cd3e4f288` |
| `fresh-structure-dump.txt` | 新库归一化结构导出（408 行，`path\|parent_path\|menu_type`） | `a5709c4a6d9d26c5cd9cf3566246b3d2` |
| `prod-structure-dump.txt` | 144 生产 `cloudpivot` 结构导出（408 行，同一口径） | `acb46924f172ba635481a9f4cdeec633` |

## 结论

- 迁移：`schema_migrations` **versioned 242/242、seed 10/10、失败 0、最大版本 V320**（与仓库文件数逐项对账）。
- 不变量：①②③④⑤ 全绿，`menu_total=408`。
- 结构：408 节点；顶级菜单 **19** 个（12 个目录 + 7 个页面；页面里有 2 个 `path` 为空的
  「价格查询」「收款退款单」，导出为 `~NULL~`）。卡面写的「21 个顶级项」与实测不符——144 生产同口径也是 19，
  另有多条顶级记录已被软删（`deleted_at` 非空，不计入）。19 条逐条比对：仅生产 0 / 仅新库 0。
- 比对①（新库 vs 生产快照）：原始差异 9+9 行 → 归一后 **0**。
- 比对②（新库 vs 迁移链基线，`--strict`）：**0**（原始与归一后均 0）。

## 与生产仍存在的 2 组「表示差异」（不是缺陷，已登记）

1. `/business/erp/purchase`（生产）↔ `/business/erp/purchase-order`（迁移链）
   —— 同一逻辑菜单的两个路径，前端 `router/index.ts:545-546` 用 `path=purchase-order` + `alias=['business/erp/purchase']`
   指向同一组件；V0319 已裁定不 INSERT 第二条。规则：`menu_struct_equiv.tsv` 的 `path` 行。
2. `/business/erp/seq` 的父：生产 `/business/erp/finance-mgmt`，新库 `/business/erp/master-data`
   —— MENU-01B（V0320）把「单号序列」移到「基础数据」；生产 HEAD=`5b0a1f5d` 尚未部署该迁移，
   部署后自动收敛。规则：`menu_struct_equiv.tsv` 的 `parent` 行。

## 复跑

```bash
# 145 上（脚本、辅助脚本、基线都放在 /root 与 /root/baseline）
RECREATE=1 bash /root/setup_new_env.sh cloudpivot_fresh
# 仓库内等价命令
RECREATE=1 bash scripts/setup_new_env.sh cloudpivot_fresh
```
