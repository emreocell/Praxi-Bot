//! Local HTTP fixture server shared by the ignored browser tests and the bench.
#![allow(dead_code)]

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const FORM: &str = "<title>Form</title><h1>Demo</h1><label>Email <input type=text></label>\
<label>Agree <input type=checkbox></label><button onclick=\"document.getElementById('o').textContent='x'\">Go</button><p id=o>idle</p>";

const REDIR: &str = "<title>Redirects</title><h1>Redirect chain</h1>\
<img src=\"/r1\" width=1 height=1><img src=\"/r1?b\" width=1 height=1><button>Ok</button>";

const POLL: &str = "<title>Long poll</title><h1>Poll</h1><button>Ok</button>\
<script>fetch('/hold').catch(()=>{});</script>";

const SPA: &str = "<title>Spa</title><h1>Spa</h1><button id=n onclick=\"location.hash='#s'+(++window.k)\">Next</button>\
<script>window.k=0;</script>";

const ALERT: &str = "<title>Alert</title><button onclick=\"alert('hello');document.title='after-alert'\">Alert</button>";

const TWO: &str = "<title>Two</title><button onclick=\"alert('first');alert('second');document.title='both'\">Two</button>";

const PROMPT: &str = "<title>Prompt</title><button onclick=\"document.title='prompt:'+prompt('name?','default')\">Prompt</button>";

const CONFIRM: &str = "<title>Confirm</title><button onclick=\"document.title='confirm:'+confirm('sure?')\">Confirm</button>";

const BEFOREUNLOAD: &str = "<title>Unload</title><button>Stay</button>\
<script>addEventListener('beforeunload',e=>{e.preventDefault();e.returnValue='x'});</script>";

const HEAVY: &str = "<title>Heavy</title><h1>Heavy</h1><div id=root></div><script>\
const r=document.getElementById('root');for(let i=0;i<1000;i++){const a=document.createElement('a');a.href='#'+i;a.textContent='link '+i;r.append(a);}\
for(let i=0;i<4000;i++){const d=document.createElement('div');d.textContent='row '+i;r.append(d);}</script>";

const CONSENT: &str = "<title>consent-start</title><p>Shop</p><div id=\"onetrust-banner-sdk\" style=\"position:fixed;bottom:0;left:0;right:0;height:100px;background:#ddd\"><button id=\"onetrust-reject-all-handler\" onclick=\"document.title='rejected'\">Reject All</button></div>";

/// Pointer fixture: a canvas-drawn radio group (no element to ref), a same-origin iframe form
/// whose first send fails, and a "human" box drawn with a plain div in a frame of ANOTHER
/// origin (`localhost` vs `127.0.0.1`). `window.__marker` is wiped by any reload.
const WIDGETS: &str = "<title>Widgets</title><style>body{margin:0;font:16px sans-serif}</style>\
<h1>Pick one</h1>\
<canvas id=cv width=300 height=60 style='border:1px solid #888'></canvas>\
<p id=picked>none</p>\
<div role=radio aria-checked=false tabindex=0 id=aria-radio onclick=\"this.setAttribute('aria-checked','true');document.getElementById('picked').textContent+='|aria'\">Option A</div>\
<iframe id=form src='/widgets-form' width=420 height=150></iframe>\
<iframe id=human src='http://localhost:{PORT}/widgets-human' width=300 height=90 title='Human check'></iframe>\
<input id=free aria-label='Free text'><input id=pw type=password aria-label='Secret'>\
<script>window.__marker=1;\
const cv=document.getElementById('cv'),c=cv.getContext('2d');\
function draw(sel){c.clearRect(0,0,300,60);[60,180].forEach((x,i)=>{c.beginPath();c.arc(x,30,18,0,7);c.stroke();if(sel===i){c.beginPath();c.arc(x,30,10,0,7);c.fill();}});}\
draw(-1);\
cv.addEventListener('click',e=>{const r=cv.getBoundingClientRect();const x=e.clientX-r.left;const i=x<120?0:1;draw(i);document.getElementById('picked').textContent='radio-'+i;});\
addEventListener('message',e=>{if(e.data==='human')document.getElementById('picked').textContent+='|human';});</script>";

const WIDGETS_FORM: &str = "<title>form</title><label>Email <input id=email></label>\
<button id=send onclick=\"window.n=(window.n||0)+1;document.getElementById('msg').textContent=window.n<2?'Server busy, try again':'Sent '+document.getElementById('email').value\">Send</button>\
<p id=msg>ready</p>";

const WIDGETS_HUMAN: &str = "<title>human</title><style>body{margin:0}</style><div id=box style='width:30px;height:30px;border:2px solid #000;margin:20px;float:left' \
onclick=\"this.textContent='x';parent.postMessage('human','*')\"></div><span style='line-height:70px'>I am human</span>";

/// Files in and out: two file inputs (one takes several files) that show what they were given,
/// download links (a text file, one whose name tries to leave the folder, a 3000
/// byte one for the size cap), and a link that opens a new tab.
const FILES: &str = "<title>Files</title><h1>Files</h1><label>Pick <input type=file id=f></label><label>Many <input type=file id=m multiple></label><p id=out>none</p><a href=\"/dl/hello.txt\">Get hello</a> <a href=\"/dl/evil.txt\">Get evil</a> <a href=\"/dl/big.bin\">Get big</a> <a href=\"/second\" target=_blank>Open second</a><script>document.getElementById('f').addEventListener('change',async e=>{const x=e.target.files[0];document.getElementById('out').textContent='picked:'+x.name+':'+await x.text();});document.getElementById('m').addEventListener('change',e=>{document.getElementById('out').textContent='many:'+[...e.target.files].map(f=>f.name).join(',');});</script>";

const SECOND: &str = "<title>Second</title><h1>Second tab</h1><button>Hi</button>";

/// Start the server on a random port; returns the port.
pub async fn serve() -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let path = req
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_string();
                if path == "/hold" {
                    // Long-poll: never answers within the test.
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                    return;
                }
                let resp = match path.as_str() {
                    "/r1" => redirect("/r2"),
                    "/r2" => redirect("/r3"),
                    "/r3" => "HTTP/1.1 200 OK\r\nContent-Type: image/gif\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    p if p.starts_with("/dl/") => {
                        let (name, body) = match p {
                            "/dl/hello.txt" => ("hello.txt", "hello download".to_string()),
                            "/dl/evil.txt" => ("../../evil.txt", "not a path".to_string()),
                            "/dl/big.bin" => ("big.bin", "x".repeat(3000)),
                            _ => ("other.bin", String::new()),
                        };
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                    p => {
                        let body = match p {
                            "/files" => FILES,
                            "/second" => SECOND,
                            "/form" => FORM,
                            "/redir" => REDIR,
                            "/poll" => POLL,
                            "/spa" => SPA,
                            "/alert" => ALERT,
                            "/two" => TWO,
                            "/prompt" => PROMPT,
                            "/confirm" => CONFIRM,
                            "/beforeunload" => BEFOREUNLOAD,
                            "/heavy" => HEAVY,
                            "/consent" => CONSENT,
                            "/widgets" => WIDGETS,
                            "/widgets-form" => WIDGETS_FORM,
                            "/widgets-human" => WIDGETS_HUMAN,
                            _ => "<title>other</title><p>other</p>",
                        }
                        .replace("{PORT}", &port.to_string());
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    }
                };
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    port
}

fn redirect(to: &str) -> String {
    format!(
        "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}
