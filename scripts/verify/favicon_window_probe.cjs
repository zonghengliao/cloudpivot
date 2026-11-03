/**
 * 诊断：chrome://history 页面到底渲染了什么（文本 + 图标 img 的坐标），
 * 并顺带截一张书签栏（chrome://newtab）。
 *
 *   node scripts/verify/favicon_window_probe.cjs
 */
const { execFileSync, spawn } = require('child_process');
const fs = require('fs');
const path = require('path');

const OUT = '/opt/cloudpivot/docs/evidence/20261103-favicon';
const BASE = process.env.FAVICON_BASE || 'http://172.18.10.144';
const DISP = ':99';
const W = 1280;
const H = 900;
const CHROME = '/root/.cache/ms-playwright/chromium-1248/chrome-linux-arm64/chrome';
const PROFILE = '/root/.favicon-shot-profile2';
const TMP = '/root/.hermes/profiles/frontend-developer/cache/scratch';
const PORT = 9334;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function grab(name) {
  const out = path.join(OUT, name);
  execFileSync('ffmpeg', ['-loglevel', 'error', '-y', '-f', 'x11grab', '-video_size', `${W}x${H}`, '-i', `${DISP}.0`, '-frames:v', '1', out], {
    env: { ...process.env, DISPLAY: DISP },
  });
  console.log(`shot ${name}`);
}

async function browserWs() {
  const info = await (await fetch(`http://127.0.0.1:${PORT}/json/version`)).json();
  const ws = new WebSocket(info.webSocketDebuggerUrl);
  await new Promise((r, j) => {
    ws.onopen = r;
    ws.onerror = j;
  });
  let id = 0;
  const pending = new Map();
  const sessions = new Map();
  ws.onmessage = (ev) => {
    const m = JSON.parse(ev.data);
    if (m.id && pending.has(m.id)) {
      pending.get(m.id)(m);
      pending.delete(m.id);
    }
    if (m.method === 'Runtime.consoleAPICalled') return;
    if (m.sessionId && sessions.has(m.sessionId)) sessions.get(m.sessionId)(m);
  };
  const send = (method, params = {}, sessionId) =>
    new Promise((resolve) => {
      const mid = ++id;
      pending.set(mid, resolve);
      ws.send(JSON.stringify({ id: mid, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  return { send, sessions };
}

async function main() {
  fs.rmSync(PROFILE, { recursive: true, force: true });
  fs.mkdirSync(path.join(PROFILE, 'Default'), { recursive: true });
  fs.writeFileSync(
    path.join(PROFILE, 'Default', 'Bookmarks'),
    JSON.stringify({
      checksum: '',
      version: 1,
      roots: {
        bookmark_bar: {
          children: [{ date_added: '13370000000000000', guid: '0000000000000001', id: '1', name: '云枢 CloudPivot', type: 'url', url: BASE + '/' }],
          date_added: '13370000000000000', date_last_used: '0', date_modified: '13370000000000000',
          guid: '0000000000000002', id: '2', name: 'Bookmarks bar', type: 'folder',
        },
        other: { children: [], date_added: '0', date_last_used: '0', date_modified: '0', guid: '0000000000000003', id: '3', name: 'Other bookmarks', type: 'folder' },
        synced: { children: [], date_added: '0', date_last_used: '0', date_modified: '0', guid: '0000000000000004', id: '4', name: 'Mobile bookmarks', type: 'folder' },
      },
    })
  );

  const xvfb = spawn('Xvfb', [DISP, '-screen', '0', `${W}x${H}x24`, '-nolisten', 'tcp', '-nolock'], { stdio: 'ignore', detached: true });
  await sleep(2000);
  const chrome = spawn(CHROME, ['--no-sandbox', '--disable-dev-shm-usage', '--no-first-run', '--no-default-browser-check',
    `--user-data-dir=${PROFILE}`, `--remote-debugging-port=${PORT}`, `--window-size=${W - 16},${H - 90}`, '--window-position=0,0',
    '--new-window', BASE + '/'], { stdio: 'ignore', env: { ...process.env, DISPLAY: DISP, TMPDIR: TMP }, detached: true });

  await sleep(12000);
  const { send } = await browserWs();

  async function inspect(url, waitMs) {
    const r = await send('Target.createTarget', { url });
    const targetId = r.result.targetId;
    await sleep(600);
    await send('Target.activateTarget', { targetId });
    await sleep(waitMs);
    const att = await send('Target.attachToTarget', { targetId, flatten: true });
    const sid = att.result.sessionId;
    await send('Runtime.enable', {}, sid);
    const ev = await send('Runtime.evaluate', {
      returnByValue: true,
      expression: `(() => {
        const imgs = [...document.querySelectorAll('img')].map(i => {
          const r = i.getBoundingClientRect();
          return {src: (i.src||'').slice(0,120), x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height)};
        }).filter(o => o.w > 0);
        return {url: location.href, title: document.title, text: (document.body.innerText||'').slice(0, 700), imgs: imgs.slice(0, 20)};
      })()`,
    }, sid);
    const val = ev.result && ev.result.result && ev.result.result.value;
    console.log(`\n===== ${url} =====`);
    console.log(JSON.stringify(val, null, 2));
    return val;
  }

  await inspect('chrome://history/', 6000);
  grab('06-chromium-history.png');

  await inspect('chrome://bookmarks/', 6000);
  grab('08-chromium-bookmarks.png');

  try { process.kill(-chrome.pid, 'SIGKILL'); } catch (e) {}
  try { process.kill(-xvfb.pid, 'SIGKILL'); } catch (e) {}
  process.exit(0);
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
