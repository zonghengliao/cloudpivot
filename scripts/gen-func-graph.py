#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
生成「云枢功能关系图」docs/architecture/cloudpivot-func-graph.html

数据来源：sys_menu 表（系统里真实存在的功能页，不是手写清单）
输出：单文件 HTML，零外部依赖，浏览器直接打开

用法：
  export PGHOST=... PGPORT=5432 PGUSER=cloudpivot PGDATABASE=cloudpivot_dev PGPASSWORD=...
  python3 scripts/gen-func-graph.py

  或：
  python3 scripts/gen-func-graph.py --dsn "postgresql://user:pw@host:5432/db"

约定：
  · 只取有 path 且不是权限码（不含 ':'）的菜单项 —— 即真实功能页
  · 连线是单据业务流转关系，改业务链时需要同步改本文件的 FLOW 段
  · 布局分组 GROUPS 是人工设计的坐标，新增业务域时在这里加
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "architecture" / "cloudpivot-func-graph.html"

# ---------------- 布局：业务域 → (颜色, 中心x, 中心y, 半径) ----------------
GROUPS = {
    "系统管理": ("#818cf8", -521, -269, 90),
    "人力资源": ("#f472b6", -521,   50, 95),
    "基础数据": ("#94a3b8", -521,  277, 70),
    "采购管理": ("#e8734a", -218, -294, 115),
    "审批流":   ("#fbbf24",  -34, -143, 85),
    "工作任务": ("#a3e635",  176, -294, 90),
    "仓库管理": ("#3fb98a",  -34,   34, 125),
    "销售管理": ("#4a9eff",  311, -109, 155),
    "财务管理": ("#c084fc",  -34,  302, 160),
    "公告消息": ("#22d3ee",  395,  160, 80),
    "通用底座": ("#64748b",  302,  294, 85),
}
ALIAS = {"system": "系统管理", "notice-center": "公告消息", "support": "系统管理"}

# ---------------- 业务流转：(组, 功能) → (组, 功能) ----------------
FLOW = [
    # 采购链
    ("采购管理", "供应商", "采购管理", "采购申请"),
    ("采购管理", "采购申请", "采购管理", "采购订单"),
    ("采购管理", "询比价", "采购管理", "采购订单"),
    ("采购管理", "采购订单", "采购管理", "采购明细"),
    ("采购管理", "采购订单", "采购管理", "收货入库"),
    ("采购管理", "收货入库", "仓库管理", "即时库存"),
    ("采购管理", "收货入库", "采购管理", "应付账款"),
    ("采购管理", "采购退货", "仓库管理", "即时库存"),
    ("采购管理", "应付账款", "财务管理", "记账凭证"),
    # 销售链
    ("销售管理", "客户档案", "销售管理", "销售订单"),
    ("销售管理", "价格管理", "销售管理", "销售订单"),
    ("销售管理", "销售订单", "销售管理", "发货单"),
    ("销售管理", "发货单", "仓库管理", "出库单"),
    ("销售管理", "发货单", "销售管理", "应收账款"),
    ("销售管理", "应收账款", "销售管理", "收款单"),
    ("销售管理", "收款单", "财务管理", "记账凭证"),
    ("销售管理", "销售订单", "销售管理", "销售发票"),
    ("销售管理", "销售退货单", "仓库管理", "即时库存"),
    ("销售管理", "佣金规则", "销售管理", "佣金结算"),
    ("销售管理", "佣金结算", "财务管理", "记账凭证"),
    # 仓库链
    ("仓库管理", "物料管理", "采购管理", "采购订单"),
    ("仓库管理", "物料管理", "销售管理", "销售订单"),
    ("仓库管理", "出库单", "仓库管理", "即时库存"),
    ("仓库管理", "库存调拨", "仓库管理", "即时库存"),
    ("仓库管理", "盘点单", "仓库管理", "即时库存"),
    ("仓库管理", "即时库存", "仓库管理", "库存流水"),
    ("仓库管理", "批次管理", "仓库管理", "即时库存"),
    ("仓库管理", "库位管理", "仓库管理", "库位库存"),
    ("仓库管理", "库位库存", "仓库管理", "即时库存"),
    ("仓库管理", "序列号管理", "仓库管理", "出库单"),
    # 财务链
    ("财务管理", "记账凭证", "财务管理", "总账"),
    ("财务管理", "会计科目", "财务管理", "记账凭证"),
    ("财务管理", "会计期间", "财务管理", "记账凭证"),
    ("财务管理", "期初余额", "财务管理", "总账"),
    ("财务管理", "年度结转", "财务管理", "总账"),
    ("财务管理", "固定资产", "财务管理", "折旧计提"),
    ("财务管理", "折旧计提", "财务管理", "记账凭证"),
    ("财务管理", "费用报销", "财务管理", "记账凭证"),
    ("财务管理", "成本核算(期间)", "财务管理", "记账凭证"),
    ("财务管理", "成本对象", "财务管理", "成本核算(期间)"),
    ("财务管理", "分摊规则", "财务管理", "成本核算(期间)"),
    ("财务管理", "工时台账", "财务管理", "成本核算(期间)"),
    ("财务管理", "费用台账", "财务管理", "成本核算(期间)"),
    ("财务管理", "预算管理", "财务管理", "费用报销"),
    ("财务管理", "预算管理", "采购管理", "采购申请"),
    ("财务管理", "待付款", "财务管理", "记账凭证"),
    # 人力
    ("人力资源", "员工档案", "人力资源", "考勤管理"),
    ("人力资源", "员工档案", "人力资源", "薪资管理"),
    ("人力资源", "薪资项", "人力资源", "薪资管理"),
    ("人力资源", "请假单", "人力资源", "考勤管理"),
    ("人力资源", "考勤管理", "人力资源", "薪资管理"),
    ("人力资源", "薪资管理", "财务管理", "记账凭证"),
    # 审批流横切
    ("采购管理", "采购申请", "审批流", "发起审批"),
    ("采购管理", "采购订单", "审批流", "发起审批"),
    ("采购管理", "应付账款", "审批流", "发起审批"),
    ("财务管理", "费用报销", "审批流", "发起审批"),
    ("人力资源", "请假单", "审批流", "发起审批"),
    ("审批流", "流程定义", "审批流", "审批中心"),
    ("审批流", "发起审批", "审批流", "审批中心"),
    ("审批流", "审批中心", "审批流", "审批委托"),
    # 任务
    ("审批流", "审批中心", "工作任务", "任务列表"),
    ("工作任务", "任务列表", "工作任务", "任务看板"),
    ("工作任务", "任务列表", "工作任务", "甘特图"),
    # 底座
    ("通用底座", "导入向导", "通用底座", "导入任务"),
    ("通用底座", "文件管理", "通用底座", "存储管理"),
    # 通知
    ("审批流", "审批中心", "公告消息", "发送通知"),
]


def run_sql(sql: str, dsn: str | None) -> list[list[str]]:
    """执行 SQL，返回 | 分隔的行。"""
    if dsn:
        cmd = ["psql", dsn, "-tA", "-F", "|", "-c", sql]
        env = os.environ.copy()
    else:
        cmd = ["psql", "-tA", "-F", "|", "-c", sql]
        env = os.environ.copy()
    r = subprocess.run(cmd, capture_output=True, text=True, env=env)
    if r.returncode != 0:
        sys.exit(f"psql 失败：\n{r.stderr}")
    return [ln.split("|") for ln in r.stdout.strip().splitlines() if "|" in ln]


def load_nodes(dsn: str | None):
    lvl2 = run_sql(
        """SELECT p.name, c.name, c.path
           FROM sys_menu c JOIN sys_menu p ON c.parent_id = p.id
           WHERE p.parent_id IS NULL
             AND c.path IS NOT NULL AND c.path <> ''
             AND c.name NOT LIKE '%:%'
           ORDER BY p.name, c.name""",
        dsn,
    )
    lvl3 = run_sql(
        """SELECT g.name, c.name, c.path
           FROM sys_menu c
           JOIN sys_menu p ON c.parent_id = p.id
           JOIN sys_menu g ON p.parent_id = g.id
           WHERE g.parent_id IS NULL
             AND c.path IS NOT NULL AND c.path <> ''
             AND c.name NOT LIKE '%:%'
           ORDER BY g.name, c.name""",
        dsn,
    )

    nodes, seen = [], set()

    def add(grp_raw, name, path):
        grp = ALIAS.get(grp_raw, grp_raw)
        if grp not in GROUPS:
            grp = "系统管理"
        key = path.strip()
        if not key or key in seen:
            return
        seen.add(key)
        nodes.append({"n": name, "g": grp, "p": key})

    for p in lvl2:
        if len(p) >= 3:
            add(p[0], p[1], p[2])
    for p in lvl3:
        if len(p) >= 3:
            add(p[0], p[1], p[2])

    # 同组重名消歧（如财务「成本核算」两页）
    for n in nodes:
        if n["g"] == "财务管理" and n["n"] == "成本核算" and n["p"].endswith("cost-period"):
            n["n"] = "成本核算(期间)"
    return nodes


JS = r"""
const NS="http://www.w3.org/2000/svg";
const svg=document.getElementById("stage"), cam=document.getElementById("cam");
const gEdges=document.createElementNS(NS,"g"), gLabels=document.createElementNS(NS,"g"), gNodes=document.createElementNS(NS,"g");
cam.appendChild(gEdges); cam.appendChild(gLabels); cam.appendChild(gNodes);

const defs=document.createElementNS(NS,"defs");
defs.innerHTML=`<marker id="ar" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="3.4" markerHeight="3.4" orient="auto-start-reverse"><path d="M0 0 L10 5 L0 10 z" fill="#8b95a5"/></marker>
<marker id="arHi" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="3.4" markerHeight="3.4" orient="auto-start-reverse"><path d="M0 0 L10 5 L0 10 z" fill="#1f6feb"/></marker>`;
svg.appendChild(defs);

Object.entries(GROUPS).forEach(([name,g])=>{
  const t=document.createElementNS(NS,"text");
  t.setAttribute("class","gLabel");
  t.setAttribute("x",g.x); t.setAttribute("y",g.y-g.r-10);
  t.setAttribute("fill",g.c); t.textContent=name;
  gLabels.appendChild(t);
});

const byGroup={};
NODES.forEach((n,i)=>{ (byGroup[n.g]=byGroup[n.g]||[]).push(i); });
Object.entries(byGroup).forEach(([gname,arr])=>{
  const g=GROUPS[gname];
  arr.forEach((i,k)=>{
    const ang=(k/arr.length)*Math.PI*2-Math.PI/2;
    const rad=Math.min(g.r,26+arr.length*3.2);
    NODES[i].x=g.x+Math.cos(ang)*rad*(0.55+Math.random()*0.5);
    NODES[i].y=g.y+Math.sin(ang)*rad*(0.55+Math.random()*0.5);
    NODES[i].vx=0; NODES[i].vy=0;
  });
});
NODES.forEach(n=>{ n.r=5.2; n.out=[]; n.in=[]; });

const LINKS=EDGES.map(([a,b])=>{ NODES[a].out.push(b); NODES[b].in.push(a); return {s:NODES[a],t:NODES[b]}; });
LINKS.forEach(l=>{
  const p=document.createElementNS(NS,"path");
  p.setAttribute("class","edge"); p.setAttribute("marker-end","url(#ar)");
  gEdges.appendChild(p); l.el=p;
});
NODES.forEach(n=>{
  const g=document.createElementNS(NS,"g"); g.setAttribute("class","node");
  const c=document.createElementNS(NS,"circle");
  c.setAttribute("r",n.r); c.setAttribute("fill",GROUPS[n.g].c); c.setAttribute("fill-opacity","0.30");
  g.appendChild(c);
  const t=document.createElementNS(NS,"text");
  t.setAttribute("dy",n.r+9); t.textContent=n.n; g.appendChild(t);
  gNodes.appendChild(g); n.el=g;
});

let alpha=1, camScale=1, camX=0, camY=0;
const DECAY=0.988, AMIN=0.0015;
const W=()=>svg.clientWidth||900, H=()=>svg.clientHeight||640;

function tick(){
  for(let i=0;i<NODES.length;i++){
    const a=NODES[i];
    for(let j=i+1;j<NODES.length;j++){
      const b=NODES[j];
      let dx=b.x-a.x, dy=b.y-a.y, d2=dx*dx+dy*dy;
      if(d2<1){dx=Math.random()-.5;dy=Math.random()-.5;d2=1;}
      if(d2<420) d2=420;
      const d=Math.sqrt(d2);
      const f=((a.g===b.g)?1500:520)/d2;
      const fx=dx/d*f, fy=dy/d*f;
      a.vx-=fx; a.vy-=fy; b.vx+=fx; b.vy+=fy;
    }
  }
  // 标签碰撞：中文名不许互相压住
  const FS=11.6, TH=FS*1.45;
  for(let i=0;i<NODES.length;i++){
    const a=NODES[i], aw=a.n.length*FS*0.62, ay=a.y+a.r+9;
    for(let j=i+1;j<NODES.length;j++){
      const b=NODES[j], bw=b.n.length*FS*0.62, by=b.y+b.r+9;
      const dx=b.x-a.x, dy=by-ay;
      const needX=(aw+bw)/2+3;
      if(Math.abs(dx)<needX && Math.abs(dy)<TH){
        const ox=(needX-Math.abs(dx))/needX, oy=(TH-Math.abs(dy))/TH, push=2.2;
        const sx=(dx>=0?1:-1)*ox*push, sy=(dy>=0?1:-1)*oy*push*0.6;
        a.vx-=sx; a.vy-=sy; b.vx+=sx; b.vy+=sy;
      }
    }
  }
  NODES.forEach(n=>{ const g=GROUPS[n.g]; n.vx+=(g.x-n.x)*0.012; n.vy+=(g.y-n.y)*0.012; });
  LINKS.forEach(l=>{
    const a=l.s,b=l.t;
    let dx=b.x-a.x, dy=b.y-a.y;
    const d=Math.sqrt(dx*dx+dy*dy)||1, f=(d-120)*0.0016;
    const fx=dx/d*f, fy=dy/d*f;
    a.vx+=fx; a.vy+=fy; b.vx-=fx; b.vy-=fy;
  });
  const VMAX=18;
  NODES.forEach(n=>{
    if(n.fixed){n.vx=0;n.vy=0;return;}
    n.vx*=0.84; n.vy*=0.84;
    if(n.vx>VMAX)n.vx=VMAX; if(n.vx<-VMAX)n.vx=-VMAX;
    if(n.vy>VMAX)n.vy=VMAX; if(n.vy<-VMAX)n.vy=-VMAX;
    n.x+=n.vx*alpha; n.y+=n.vy*alpha;
  });
  alpha=Math.max(AMIN,alpha*DECAY);
}

function draw(){
  NODES.forEach(n=>n.el.setAttribute("transform",`translate(${n.x},${n.y})`));
  LINKS.forEach(l=>l.el.setAttribute("d",`M${l.s.x},${l.s.y} L${l.t.x},${l.t.y}`));
}
function applyCam(){cam.setAttribute("transform",`translate(${camX},${camY}) scale(${camScale})`);}

(function prewarm(){
  for(let i=0;i<2000;i++) tick();
  const xs=NODES.map(n=>n.x), ys=NODES.map(n=>n.y);
  const minX=Math.min(...xs),maxX=Math.max(...xs),minY=Math.min(...ys),maxY=Math.max(...ys);
  const dw=Math.max(maxX-minX,1), dh=Math.max(maxY-minY,1), pad=42;
  camScale=Math.max(0.25,Math.min((W()-pad*2)/dw,(H()-pad*2)/dh,2.4));
  camX=W()/2-(minX+maxX)/2*camScale;
  camY=H()/2-(minY+maxY)/2*camScale;
  applyCam(); draw();
})();

function loop(){ for(let k=0;k<2;k++) tick(); draw(); requestAnimationFrame(loop); }
loop();
setInterval(()=>{ if(alpha<0.012) alpha=0.012; },2500);

let dragN=null, offX=0, offY=0, panning=false, psx=0, psy=0;
function toWorld(e){
  const r=svg.getBoundingClientRect();
  return {x:((e.clientX-r.left)-camX)/camScale, y:((e.clientY-r.top)-camY)/camScale};
}
NODES.forEach(n=>{
  n.el.addEventListener("pointerdown",e=>{
    e.stopPropagation(); dragN=n;
    const w=toWorld(e); offX=n.x-w.x; offY=n.y-w.y; n.fixed=true; svg.classList.add("drag");
  });
  n.el.addEventListener("pointermove",e=>{
    if(dragN!==n) return;
    const w=toWorld(e); n.x=w.x+offX; n.y=w.y+offY; draw();
  });
  n.el.addEventListener("pointerup",()=>{
    if(dragN===n){dragN=null;n.fixed=false;svg.classList.remove("drag");}
  });
  n.el.addEventListener("click",e=>{e.stopPropagation(); select(n);});
});
svg.addEventListener("pointerdown",e=>{
  if(dragN) return; panning=true; psx=e.clientX-camX; psy=e.clientY-camY; svg.classList.add("drag");
});
svg.addEventListener("pointermove",e=>{
  if(!panning) return; camX=e.clientX-psx; camY=e.clientY-psy; applyCam();
});
svg.addEventListener("pointerup",()=>{panning=false;svg.classList.remove("drag");});
svg.addEventListener("click",()=>{ if(!dragN) closePanel(); });
svg.addEventListener("wheel",e=>{
  e.preventDefault();
  const r=svg.getBoundingClientRect(), mx=e.clientX-r.left, my=e.clientY-r.top, old=camScale;
  camScale=Math.max(0.3,Math.min(4,camScale*(e.deltaY<0?1.13:1/1.13)));
  camX=mx-(mx-camX)*(camScale/old); camY=my-(my-camY)*(camScale/old); applyCam();
},{passive:false});
svg.addEventListener("dblclick",()=>{
  camScale=1;camX=0;camY=0;applyCam(); NODES.forEach(n=>n.fixed=false); alpha=1;
});

let selNode=null;
function select(n){
  if(selNode===n){closePanel();return;}
  selNode=n;
  NODES.forEach(m=>{
    m.el.classList.remove("sel","dim");
    if(m===n) return;
    if(!(n.out.includes(NODES.indexOf(m))||n.in.includes(NODES.indexOf(m)))) m.el.classList.add("dim");
  });
  n.el.classList.add("sel");
  LINKS.forEach(l=>{
    const on=(l.s===n||l.t===n);
    l.el.classList.toggle("hi",on); l.el.classList.toggle("dim",!on);
    l.el.setAttribute("marker-end",on?"url(#arHi)":"url(#ar)");
  });
  document.getElementById("pTitle").textContent=n.n;
  document.getElementById("pMeta").textContent=`${n.g} · ${n.out.length} 个下游 / ${n.in.length} 个上游`;
  let h="";
  if(n.out.length){
    h+=`<div class="sec"><div class="sec-t">流向 →</div><div class="rel">`;
    n.out.forEach(j=>h+=`<span class="chip" onclick="pick(${j})">${NODES[j].n}</span>`);
    h+=`</div></div>`;
  }
  if(n.in.length){
    h+=`<div class="sec"><div class="sec-t">← 来自</div><div class="rel">`;
    n.in.forEach(j=>h+=`<span class="chip" onclick="pick(${j})">${NODES[j].n}</span>`);
    h+=`</div></div>`;
  }
  if(!n.out.length&&!n.in.length) h=`<div class="meta">孤立功能（暂无流转关系）</div>`;
  document.getElementById("pBody").innerHTML=h;
  document.getElementById("panel").classList.add("show");
}
function pick(i){ select(NODES[i]); }
function closePanel(){
  document.getElementById("panel").classList.remove("show");
  selNode=null;
  NODES.forEach(n=>n.el.classList.remove("sel","dim"));
  LINKS.forEach(l=>{l.el.classList.remove("hi","dim");l.el.setAttribute("marker-end","url(#ar)");});
}
document.getElementById("legend").innerHTML=Object.entries(GROUPS)
  .map(([k,g])=>`<div class="lg"><i style="background:${g.c}"></i>${k}</div>`).join("");
const isolated=NODES.filter(n=>!n.out.length&&!n.in.length).length;
document.getElementById("kpi").innerHTML=
  `${NODES.length} 个功能 · ${EDGES.length} 条流转 · ${Object.keys(GROUPS).length} 个业务域`
  +(isolated?` · ${isolated} 个无流转`:"");
"""

CSS = """
  :root{
    --f: var(--foreground, #1a1d23);
    --m: var(--muted-foreground, #5b6472);
    --a: var(--accent, #1f6feb);
    --b: var(--border, #d3d8e0);
  }
  html,body{margin:0;padding:0;height:100%;overflow:hidden;
    font-family:var(--font-sans,-apple-system,"PingFang SC","Microsoft YaHei",sans-serif);
    background:var(--background,#ffffff);color:var(--f);}
  #stage{display:block;width:100%;height:860px;cursor:grab;}
  #stage.drag{cursor:grabbing;}
  .edge{stroke:#8b95a5;stroke-width:1.15;fill:none;opacity:.45;}
  .edge.hi{stroke:var(--a);stroke-width:2.2;opacity:1;}
  .edge.dim{opacity:.05;}
  .node{cursor:pointer;}
  .node circle{stroke:var(--b);stroke-width:1;transition:stroke .1s,stroke-width .1s;}
  .node:hover circle{stroke:var(--a);stroke-width:2.4;}
  .node.sel circle{stroke:var(--a);stroke-width:3.2;}
  .node text{fill:var(--m);font-size:11.6px;text-anchor:middle;pointer-events:none;
    font-weight:500;paint-order:stroke;stroke:var(--background,#fff);stroke-width:2.1px;
    stroke-linejoin:round;transition:fill .1s;}
  .node:hover text,.node.sel text{fill:var(--f);font-weight:700;}
  .node.dim{opacity:.13;}
  .gLabel{font-size:14px;font-weight:800;letter-spacing:.5px;opacity:.5;
    text-anchor:middle;pointer-events:none;}
  #hud{position:fixed;left:15px;top:13px;pointer-events:none;max-width:360px;}
  #hud h1{margin:0;font-size:14.5px;font-weight:700;letter-spacing:.2px;}
  #hud .sub{font-size:10px;color:var(--m);margin-top:2px;line-height:1.5;}
  #hud .kpi{font-size:9.5px;color:var(--m);margin-top:5px;font-variant-numeric:tabular-nums;}
  #tip{position:fixed;right:15px;bottom:13px;font-size:9.5px;color:var(--m);
    text-align:right;line-height:1.7;pointer-events:none;}
  #tip b{color:var(--f);}
  #panel{position:fixed;right:13px;top:13px;width:228px;max-height:calc(100vh - 28px);
    overflow:auto;border:1px solid var(--b);border-radius:9px;padding:10px 12px;
    font-size:10.5px;line-height:1.6;display:none;
    background:color-mix(in srgb, var(--background,#fff) 94%, transparent);
    backdrop-filter:blur(8px);}
  #panel.show{display:block;}
  #panel h3{margin:0 0 3px;font-size:12px;font-weight:700;}
  #panel .meta{color:var(--m);font-size:9.5px;margin-bottom:7px;}
  #panel .sec{margin-top:7px;}
  #panel .sec-t{font-size:9px;letter-spacing:.5px;color:var(--m);font-weight:800;margin-bottom:3px;}
  #panel .rel{display:flex;flex-wrap:wrap;gap:3px;}
  #panel .chip{font-size:9.5px;padding:1.5px 5px;border-radius:4px;border:1px solid var(--b);
    color:var(--m);cursor:pointer;}
  #panel .chip:hover{border-color:var(--a);color:var(--f);}
  #panel .close{position:absolute;right:9px;top:7px;cursor:pointer;color:var(--m);font-size:13px;}
  #legend{position:fixed;left:15px;bottom:13px;font-size:9.5px;color:var(--m);
    display:flex;gap:11px;flex-wrap:wrap;pointer-events:none;max-width:660px;}
  .lg{display:flex;align-items:center;gap:4px;}
  .lg i{width:8px;height:8px;border-radius:50%;display:inline-block;}
"""


def build_html(nodes, edges):
    return f"""<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8">
<title>云枢 CloudPivot · 功能关系图</title>
<style>{CSS}</style>
</head>
<body>
<svg id="stage"><g id="cam"></g></svg>
<div id="hud">
  <h1>云枢 CloudPivot · 功能关系图</h1>
  <div class="sub">节点 = 系统里真实存在的功能页（菜单导出）· 箭头 = 业务流转</div>
  <div class="kpi" id="kpi"></div>
</div>
<div id="panel">
  <span class="close" onclick="closePanel()">×</span>
  <h3 id="pTitle"></h3>
  <div class="meta" id="pMeta"></div>
  <div id="pBody"></div>
</div>
<div id="tip"><b>拖动</b>节点 · <b>滚轮</b>缩放 · <b>空白拖动</b>平移<br><b>点节点</b>看上下游 · <b>双击</b>重置</div>
<div id="legend"></div>
<script>
const GROUPS = {json.dumps({k: {"c": v[0], "x": v[1], "y": v[2], "r": v[3]} for k, v in GROUPS.items()}, ensure_ascii=False)};
const NODES  = {json.dumps(nodes, ensure_ascii=False)};
const EDGES  = {json.dumps(edges)};
{JS}
</script>
</body>
</html>
"""


def main():
    ap = argparse.ArgumentParser(description="生成云枢功能关系图")
    ap.add_argument("--dsn", help="postgresql://user:pw@host:5432/db")
    ap.add_argument("--out", default=str(OUT), help="输出路径")
    args = ap.parse_args()

    nodes = load_nodes(args.dsn)
    if not nodes:
        sys.exit("没查到功能页，检查数据库连接和 sys_menu")

    idx = {(n["g"], n["n"]): i for i, n in enumerate(nodes)}
    edges, missing = [], []
    for ga, na, gb, nb in FLOW:
        a, b = (ga, na), (gb, nb)
        if a in idx and b in idx:
            edges.append([idx[a], idx[b]])
        else:
            missing.append((a, b))

    if missing:
        print("⚠ 以下流转端点不在菜单里（已跳过）：")
        for m in missing:
            print("   ", m)

    html = build_html(nodes, edges)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(html, encoding="utf-8")

    from collections import Counter
    groups = Counter(n["g"] for n in nodes)
    print(f"✓ {out}")
    print(f"  节点 {len(nodes)} · 边 {len(edges)} · 业务域 {len(groups)}")
    for g, c in groups.most_common():
        print(f"    {g:6s} {c:3d}")


if __name__ == "__main__":
    main()
