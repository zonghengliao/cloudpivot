#!/usr/bin/env bash
set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot-dev.env; set +a
export PGPASSWORD="$NIK_DATABASE_PASSWORD"
DB=${INV01_DB:-cloudpivot_inv01}
q() { psql -h 127.0.0.1 -U "$NIK_DATABASE_USER" -At -d "$DB" -c "$1"; }
q "update sys_user set is_locked = false, login_fail_count = 0, lock_time = null where username in ('admin','inv01_approver')"
q "select username||' locked='||is_locked||' fail='||login_fail_count||' must_change='||must_change_password from sys_user"
q "select count(*) from sys_login_log where created_at > now() - interval '1 hour'" 2>/dev/null || true
