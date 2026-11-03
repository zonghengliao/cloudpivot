set -uo pipefail
set -a; . /etc/cloudpivot/cloudpivot.env; set +a
export PGPASSWORD="${NIK_DATABASE_PASSWORD:?}"
P() { psql -h 127.0.0.1 -U cloudpivot -d cloudpivot -Atq -c "$1"; }

restore() {
  P "UPDATE biz_flow_def SET status='PUBLISHED', updated_at=now() WHERE flow_code='po_approve'" > /dev/null 2>&1
  echo "=== 已恢复 po_approve → $(P "select status from biz_flow_def where flow_code='po_approve'") ==="
}
trap restore EXIT INT TERM

echo "=== 恢复前状态 ==="
P "select flow_code||' | '||status||' | ver='||coalesce(current_version_id::text,'NULL') from biz_flow_def where flow_code='po_approve'"

echo "=== 临时下线 po_approve（只影响这一次取证的 10 秒窗口）==="
P "UPDATE biz_flow_def SET status='DISABLED', updated_at=now() WHERE flow_code='po_approve'"
P "select flow_code||' | '||status from biz_flow_def where flow_code='po_approve'"

echo "=== 提交 DRAFT 采购单（期望 409 + 66033）==="
node /opt/cloudpivot/scripts/verify/deployB_flowgate.mjs
rc=$?
echo "flowgate_rc=$rc"

restore
trap - EXIT

echo "=== 恢复后：审批流健康自检（期望 healthy=8）==="
node -e '
const B=process.env.DEPLOYB_BASE_URL||"http://127.0.0.1";
(async()=>{
 const l=await fetch(B+"/api/system/auth/login",{method:"POST",headers:{"Content-Type":"application/json"},body:JSON.stringify({username:"admin",password:process.env.NIK_ADMIN_PASSWORD})});
 const lb=await l.json();
 const r=await fetch(B+"/api/business/erp/approval-flow/health",{headers:{Authorization:"Bearer "+lb.data.access_token}});
 const b=await r.json();
 console.log("health:", JSON.stringify({total:b.data.total,healthy:b.data.healthy,unhealthy:b.data.unhealthy,
   unhealthy_list:(b.data.items||[]).filter(i=>!i.published).map(i=>i.flow_code)}));
})();
'
echo "=== po_approve 最终状态 ==="
P "select flow_code||' | '||status from biz_flow_def where flow_code='po_approve'"
