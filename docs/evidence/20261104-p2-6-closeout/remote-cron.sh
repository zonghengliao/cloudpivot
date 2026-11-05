#!/usr/bin/env bash
# 收口D 验收 ③：144 cron 接线核对（含手工首跑 + cron 最小环境复跑）
set -uo pipefail
LOG=/var/log/cloudpivot/snapshot-audit.log
JSON=/var/log/cloudpivot/snapshot-audit.latest.json

echo "########## 1. crontab 现状 ##########"
crontab -l
echo "-- grep snapshot --"
crontab -l | grep snapshot
echo "-- 原 erp-health-daily 是否还在 --"
crontab -l | grep -c 'erp-health-daily.sh'
echo "-- cron 服务 --"
systemctl is-active cron

echo; echo "########## 2. env 文件权限 ##########"
ls -l /etc/cloudpivot/snapshot-audit.env
stat -c '%a %U:%G' /etc/cloudpivot/snapshot-audit.env
echo "-- 变量名（不回显口令）--"
sed -n 's/^\([A-Z_]*\)=.*/\1=<set>/p' /etc/cloudpivot/snapshot-audit.env

echo; echo "########## 3. 手工跑一次（卡面命令）##########"
BEFORE=$(wc -l < "$LOG" 2>/dev/null || echo 0)
echo "before_lines=$BEFORE"
bash /opt/cloudpivot/scripts/cron/snapshot_audit.sh
echo "exit=$?"
AFTER=$(wc -l < "$LOG")
echo "after_lines=$AFTER"
echo "-- 本轮新增日志（BLOCKER 行 + 汇总）--"
sed -n "$((BEFORE+1)),${AFTER}p" "$LOG" | grep -E '开始：|BLOCKER=|^汇总|^退出码|结束：exit='
echo "-- 本轮 BLOCKER= 出现次数 --"
sed -n "$((BEFORE+1)),${AFTER}p" "$LOG" | grep -c 'BLOCKER=0'

echo; echo "########## 4. JSON 结果文件三计数 ##########"
ls -l "$JSON"
python3 -c "import json;d=json.load(open('$JSON'));print('summary=',d['summary']);print('generated_at=',d['generated_at']);print('db=',d['database'])"

echo; echo "########## 5. cron 最小环境复跑（env -i）##########"
env -i PATH=/usr/bin:/bin HOME=/root /bin/sh -c 'bash /opt/cloudpivot/scripts/cron/snapshot_audit.sh'
echo "exit=$?"
python3 -c "import json;print('after env -i summary=',json.load(open('$JSON'))['summary'])"

echo; echo "########## 6. 日志尾部 ##########"
tail -12 "$LOG"
