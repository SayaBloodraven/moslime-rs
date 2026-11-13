//! Tiny built-in web server for headless mode: a touch-friendly page with
//! Start/Stop buttons per tracker. Hand-rolled HTTP/1.1 (one request per
//! connection) so it needs no extra dependencies.
//!
//! There is no password: anyone on your network who opens the page can
//! start/stop trackers. Leave `web_port` at 0 in config_six.json to turn it off.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

use crate::headless::Bridge;

pub async fn serve(bridge: Arc<Bridge>, port: u16, cancel: CancellationToken) -> Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            accepted = listener.accept() => {
                let (sock, _) = accepted?;
                let bridge = bridge.clone();
                tokio::spawn(async move {
                    let _ = handle(sock, bridge).await;
                });
            }
        }
    }
}

async fn handle(mut sock: TcpStream, bridge: Arc<Bridge>) -> Result<()> {
    // Read the request head (we never need a body).
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let read_head = async {
        loop {
            let n = sock.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 8192 {
                break;
            }
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::time::timeout(Duration::from_secs(5), read_head).await??;

    let head = String::from_utf8_lossy(&buf);
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let addr = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("addr="))
        .map(percent_decode)
        .unwrap_or_default();

    let (status, ctype, body): (&str, &str, String) = match (method, path) {
        ("GET" | "HEAD", "/") | ("GET" | "HEAD", "/index.html") => ("200 OK", "text/html; charset=utf-8", PAGE.to_string()),
        ("GET" | "HEAD", "/api/status") => ("200 OK", "application/json", bridge.status_json().await),
        ("POST", "/api/start_all") => {
            bridge.start_all().await;
            ("200 OK", "application/json", bridge.status_json().await)
        }
        ("POST", "/api/server") => match bridge.set_server(&addr).await {
            Ok(()) => ("200 OK", "application/json", bridge.status_json().await),
            Err(e) => ("400 Bad Request", "text/plain", format!("{e:#}")),
        },
        ("POST", "/api/stop_all") => {
            bridge.stop_all().await;
            ("200 OK", "application/json", bridge.status_json().await)
        }
        ("POST", p) if p.starts_with("/api/start/") || p.starts_with("/api/stop/") => {
            let starting = p.starts_with("/api/start/");
            let id = p.rsplit('/').next().and_then(|n| n.parse::<usize>().ok());
            match id {
                Some(i) if i < bridge.count() => {
                    if starting {
                        let _ = bridge.start(i).await;
                    } else {
                        let _ = bridge.stop(i).await;
                    }
                    ("200 OK", "application/json", bridge.status_json().await)
                }
                _ => ("404 Not Found", "text/plain", "no such tracker".to_string()),
            }
        }
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };

    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes()).await?;
    if method != "HEAD" {
        sock.write_all(body.as_bytes()).await?;
    }
    let _ = sock.shutdown().await;
    Ok(())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

const PAGE: &str = r##"<!doctype html>
<html lang="en"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Mocopi bridge</title>
<style>
:root{color-scheme:dark}
*{box-sizing:border-box}
.kiosk,.kiosk *{cursor:none !important}
body{margin:0;background:#16181c;color:#e8e8e8;font:16px system-ui,sans-serif}
header{display:flex;flex-wrap:wrap;gap:10px;align-items:center;justify-content:space-between;padding:12px 14px;border-bottom:1px solid #2a2d33}
h1{font-size:18px;margin:0}
#srv{color:#9aa0a8;font-size:13px}
button{font:inherit;font-weight:600;border:0;border-radius:10px;padding:14px 22px;min-height:52px;color:#fff;background:#3a3f47}
button.start{background:#1f7a3d}
button.stop{background:#a33a3a}
#srv{background:none;color:#9aa0a8;font-size:13px;padding:4px 0;min-height:0;font-weight:400;text-decoration:underline}
#dlg{position:fixed;inset:0;background:#000b;display:flex;align-items:center;justify-content:center;z-index:5}
#dlg[hidden]{display:none}
.box{background:#1f2227;padding:16px;border-radius:14px;width:min(360px,94vw)}
.box h2{margin:0 0 10px;font-size:16px}
#disp{font:24px ui-monospace,monospace;background:#101215;border-radius:8px;padding:12px;min-height:54px;margin-bottom:10px;word-break:break-all}
.keys{display:grid;grid-template-columns:repeat(3,1fr);gap:8px;margin-bottom:10px}
.keys button{padding:12px 0;font-size:20px}
#derr{color:#e07070;font-size:13px;min-height:1.4em;margin-bottom:8px}
.bar{display:flex;gap:8px}
main{display:grid;grid-template-columns:repeat(auto-fill,minmax(280px,1fr));gap:10px;padding:12px}
.card{background:#1f2227;border-radius:12px;padding:12px;border-left:6px solid #555}
.card.connected{border-left-color:#2fbf5a}
.card.connecting{border-left-color:#e0a030}
.card.disconnected{border-left-color:#b04040}
.top{display:flex;gap:10px;align-items:baseline}
.sid{color:#8fc3ff;font-weight:600;font-family:ui-monospace,monospace}
.mac{color:#9aa0a8;font-family:ui-monospace,monospace;font-size:13px;flex:1}
.badge{font-size:13px}
.msg{color:#b8bcc2;font-size:13px;min-height:2.6em;margin:8px 0;word-break:break-word}
.row{display:flex;align-items:center;justify-content:space-between}
.pk{color:#9aa0a8;font-size:13px}
.bat{font-size:13px;color:#9aa0a8;margin:0 10px;flex:1;text-align:right}
.bat.low{color:#e0a030;font-weight:600}
.bat.crit{color:#e05050;font-weight:700}
</style></head><body>
<header>
  <div><h1>Mocopi bridge</h1><button id="srv" title="Change the PC address"></button></div>
  <div class="bar"><button class="start" id="sa">Start all</button><button class="stop" id="xa">Stop all</button></div>
</header>
<main id="list"></main>
<div id="dlg" hidden><div class="box">
  <h2>SlimeVR PC address</h2>
  <div id="disp"></div>
  <div class="keys" id="keys"></div>
  <div id="derr"></div>
  <div class="bar"><button id="dback">&#9003;</button><button id="dclr">Clear</button><button id="dc">Cancel</button><button class="start" id="ds">Save</button></div>
</div></div>
<script>
// Opened by the Pi's kiosk browser (address ends in ?kiosk=1): hide the mouse pointer.
if (location.search.indexOf('kiosk') >= 0) document.documentElement.classList.add('kiosk');
const list = document.getElementById('list');
const cards = {};
let curServer = '';
let entry = '';
const dlg = document.getElementById('dlg');
const disp = document.getElementById('disp');
const derr = document.getElementById('derr');
function showEntry() { disp.textContent = entry || ' '; }
for (const k of ['1','2','3','4','5','6','7','8','9','.','0',':']) {
  const b = document.createElement('button');
  b.textContent = k;
  b.onclick = function () { entry += k; showEntry(); };
  document.getElementById('keys').appendChild(b);
}
document.getElementById('srv').onclick = function () {
  entry = curServer; derr.textContent = ''; showEntry(); dlg.hidden = false;
};
document.getElementById('dback').onclick = function () { entry = entry.slice(0, -1); showEntry(); };
document.getElementById('dclr').onclick = function () { entry = ''; showEntry(); };
document.getElementById('dc').onclick = function () { dlg.hidden = true; };
document.getElementById('ds').onclick = async function () {
  derr.textContent = 'Saving...';
  try {
    const r = await fetch('/api/server?addr=' + encodeURIComponent(entry), {method: 'POST'});
    if (r.ok) { dlg.hidden = true; refresh(); }
    else { derr.textContent = await r.text(); }
  } catch (e) { derr.textContent = 'Could not reach the bridge'; }
};
async function post(path) {
  try { await fetch(path, {method: 'POST'}); } catch (e) {}
  refresh();
}
function ensure(t) {
  if (cards[t.id]) return cards[t.id];
  const el = document.createElement('div');
  el.className = 'card';
  el.innerHTML = '<div class="top"><b class="num"></b><span class="sid"></span><span class="mac"></span><span class="badge"></span></div>' +
                 '<div class="msg"></div>' +
                 '<div class="row"><span class="pk"></span><span class="bat"></span><button></button></div>';
  const c = {el: el, t: t};
  el.querySelector('button').onclick = function () {
    post('/api/' + (c.t.running ? 'stop' : 'start') + '/' + c.t.id);
  };
  list.appendChild(el);
  cards[t.id] = c;
  return c;
}
function render(d) {
  curServer = d.server;
  document.getElementById('srv').textContent = 'SlimeVR PC: ' + d.server + '  (tap to change)';
  for (const t of d.trackers) {
    const c = ensure(t);
    c.t = t;
    c.el.className = 'card ' + t.status.toLowerCase();
    c.el.querySelector('.num').textContent = '#' + t.number;
    c.el.querySelector('.sid').textContent = t.sid || '';
    c.el.querySelector('.mac').textContent = t.mac;
    c.el.querySelector('.badge').textContent = t.status;
    c.el.querySelector('.msg').textContent = t.message;
    c.el.querySelector('.pk').textContent = t.packets + ' packets';
    const bat = c.el.querySelector('.bat');
    if (t.battery >= 0) {
      bat.textContent = 'Battery ' + t.battery + '%';
      bat.className = 'bat' + (t.battery < 10 ? ' crit' : t.battery < 25 ? ' low' : '');
    } else {
      bat.textContent = '';
      bat.className = 'bat';
    }
    const b = c.el.querySelector('button');
    b.textContent = t.running ? 'Stop' : 'Start';
    b.className = t.running ? 'stop' : 'start';
  }
}
async function refresh() {
  try {
    const r = await fetch('/api/status', {cache: 'no-store'});
    render(await r.json());
  } catch (e) {}
}
document.getElementById('sa').onclick = function () { post('/api/start_all'); };
document.getElementById('xa').onclick = function () { post('/api/stop_all'); };
refresh();
setInterval(refresh, 1000);
</script>
</body></html>"##;
