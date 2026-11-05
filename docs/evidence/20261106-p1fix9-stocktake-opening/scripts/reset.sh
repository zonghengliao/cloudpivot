#!/usr/bin/env bash
# 重置隔离库（cloudpivot_st9）并重启实例
set -u
DB=cloudpivot_st9
BIN=/root/st9-bin/cloudpivot
pkill -f "$BIN" 2>/dev/null
sleep 2
su - postgres -c "psql -qc \"select pg_terminate_backend(pid) from pg_stat_activity where datname='$DB'\"" >/dev/null
su - postgres -c "dropdb --if-exists $DB"
su - postgres -c "createdb -O alun $DB"
bash /root/st9-run/start.sh >/dev/null
for i in $(seq 1 40); do
  code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18190/health)
  if [ "$code" = "200" ]; then echo "health=200 after ${i} tries"; exit 0; fi
  sleep 3
done
echo "health failed"; tail -20 /root/st9-instance.log; exit 1
