"""验收断言：闸② 失败清单必须恰等于卡面给的 10 个生产文件（不多不少），G5 exit 0 且 N>=27。"""
import re
import subprocess
import sys

EXPECTED = {
    "business/erp/approval_gate.rs": {"biz_flow_def"},
    "business/erp/contract_handler.rs": {"biz_flow_instance"},
    "business/erp/reminder.rs": {"sys_user"},
    "business/erp/stockin_bridge.rs": {"biz_flow_instance", "biz_flow_task"},
    "business/flow/task_node.rs": {"biz_task", "sys_user"},
    "business/flow/template_vars.rs": {"biz_flow_instance", "biz_flow_instance_node"},
    "business/task/task_model_flow.rs": {"biz_task", "biz_flow_task"},
    "business/task/task_model_hierarchy.rs": {"biz_task"},
    "business/task/task_model_schedule.rs": {"biz_task", "biz_task_dependency"},
    "business/task/task_model_write.rs": {"biz_task", "biz_task_member", "biz_task_dependency"},
}

gate2 = subprocess.run(
    [sys.executable, "scripts/lint/scope_gate.py", "--root", "."],
    capture_output=True, text=True, cwd="/opt/cloudpivot",
)
g5 = subprocess.run(
    [sys.executable, "scripts/lint/changelog_in_tx.py", "--root", "."],
    capture_output=True, text=True, cwd="/opt/cloudpivot",
)

got = {}
for line in gate2.stdout.splitlines():
    m = re.match(r"^    (business/\S+\.rs) \((\S.*)\)$", line)
    if m:
        got[m.group(1)] = {t.strip() for t in m.group(2).split(",")}

ok = True


def check(label, cond, detail=""):
    global ok
    print(f"{'PASS' if cond else 'FAIL'}  {label}{('  ' + detail) if detail else ''}")
    ok = ok and cond


check("闸② exit 1", gate2.returncode == 1, f"rc={gate2.returncode}")
check("闸② 失败文件恰好 10 个", len(got) == 10, f"实际 {len(got)}")
check("闸② 文件集合与卡面一致", set(got) == set(EXPECTED), f"多={sorted(set(got) - set(EXPECTED))} 少={sorted(set(EXPECTED) - set(got))}")
for rel in sorted(EXPECTED):
    check(f"  {rel} 表集合一致", got.get(rel) == EXPECTED[rel], f"实际 {got.get(rel)}")
check("闸② 打印扫描/跳过计数", "扫描生产面 312 个文件 / 跳过测试面 50 个文件" in gate2.stdout)
check("G5 exit 0", g5.returncode == 0, f"rc={g5.returncode}")
m = re.search(r"检查了 (\d+) 个调用点", g5.stdout)
check("G5 N >= 27", bool(m) and int(m.group(1)) >= 27, f"N={m.group(1) if m else '?'}")
print("\n结论：", "全部通过" if ok else "有不符项")
sys.exit(0 if ok else 1)
