# ARCH-5 · 交付物与验证记录（2026-11-04）

对应设计文档：`docs/architecture/ARCH-5-工作流引擎设计.md`（4534 行）

## 一、本目录内容

| 文件 | 说明 |
|---|---|
| `arch5_ddl_poc.sql` | 完整可执行 DDL（设计级，**不是迁移文件**）：11 张新表 + 17 列扩展（def 5 / node 8 / instance 2 / task 2）+ 全部 CHECK / 索引 / 外键 + 末尾结构自检 |
| `arch5_negative_tests.sql` | 21 个负向用例，每条约束都必须把脏数据拦下才算通过 |
| `negative-test-output.txt` | 负向测试的真实输出 |

## 二、验证环境

- PostgreSQL **18.6**（本机 `127.0.0.1:5432`）
- 基线库 `cloudpivot_p16md`：**102 张表**的真实库（含 `biz_flow_*` 9 张、`biz_task` 全套、V0177 三列）
- POC 库：`CREATE DATABASE arch5_poc_d TEMPLATE cloudpivot_p16md`
- 执行：`sudo -u postgres psql -d arch5_poc_d -v ON_ERROR_STOP=1 -f <file>`

## 三、验证结果

### 3.1 DDL 执行（退出码 0）

```
psql:arch5_ddl_poc.sql:666: NOTICE:  ARCH-5 POC 自检通过：11 张新表 + node 8 列 + def/instance/task 9 列 + 6 条关键索引
DO
```

### 3.2 存量兼容性（这是 `ADD CONSTRAINT` 顺带做的校验）

```
 defs | nodes | insts | ftasks | vers
------+-------+-------+--------+------
   12 |    52 |     4 |      1 |   12
```

12 个流程定义 / 52 个节点 / 4 个实例 / 12 个版本在全部新约束下存活。
`select flow_type, count(*) from biz_flow_def group by 1` → `APPROVAL | 12`（默认值生效，存量零改动）。

### 3.3 负向测试 21/21

见 `negative-test-output.txt`。覆盖：表单定义 4 条、节点绑定/闭环/升级 5 条、检查项 4 条、
证据/归档 3 条、触发器 3 条、闭环归档联动 1 条、未知 node_type 1 条。

## 四、设计文档里的 12 张 mermaid 图（均实测可渲染）

用 `@mermaid-js/mermaid-cli 12.2.0` 逐张渲染 SVG，12/12 成功（无语法错误，中文字符正常出图）：

| 图 | 内容 | 位置 |
|---|---|---|
| 1 | 六维体系 | §2.2 |
| 2 | 数据模型 ER | §3.5 |
| 3 | 节点生命周期状态机 | §4.16 |
| 4 | 发起中心参数填写时序 | §5.2 |
| 5 | 检查判定与分支 | §6.2.5 |
| 6 | AI 派活链路时序 | §7.7 |
| 7 | 闭环三方模型 | §10.1 |
| 8 | 流程与任务融合总装 | §7.10 |
| 9 | 能力分层与依赖 | §14.4 |
| 10 | 场景一 费用报销 | §16.1.2 |
| 11 | 场景二 入库检查 | §16.2.2 |
| 12 | 场景三 董事会决议 | §16.3.1 |

复跑渲染：

```bash
awk '/^```mermaid$/{f=1;n++;next} /^```$/{if(f){f=0;next}} f{print > ("/tmp/d" n ".mmd")}' docs/architecture/ARCH-5-工作流引擎设计.md
printf '{"args":["--no-sandbox"]}' > /tmp/pptr.json
for i in /tmp/d*.mmd; do npx -p @mermaid-js/mermaid-cli mmdc -p /tmp/pptr.json -i $i -o "${i%.mmd}.svg"; done
```

## 五、复跑 DDL + 负向测试

```bash
sudo -u postgres psql -c "CREATE DATABASE arch5_poc_x TEMPLATE cloudpivot_p16md OWNER postgres"
sudo -u postgres psql -d arch5_poc_x -v ON_ERROR_STOP=1 -f docs/evidence/20261104-arch5/arch5_ddl_poc.sql
sudo -u postgres psql -d arch5_poc_x -v ON_ERROR_STOP=1 -f docs/evidence/20261104-arch5/arch5_negative_tests.sql
```

> 注：负向测试会在 POC 库里留下基线行（`arch5_poc_flow` 定义 / `poc_form` 表单 / `poc_ckl` 清单 / 一个实例）。
> 这不是副作用，是**故意的**：负向测试需要合法基线才能构造「只违反一条约束」的脏数据。
