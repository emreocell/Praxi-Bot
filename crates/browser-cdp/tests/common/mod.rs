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

const CONFIRM: &str = "<title>Confirm</title><button onclick=\"document.title='confirm:'+confirm('sure?')\">Confirm</button>";

const BEFOREUNLOAD: &str = "<title>Unload</title><button>Stay</button>\
<script>addEventListener('beforeunload',e=>{e.preventDefault();e.returnValue='x'});</script>";

const HEAVY: &str = "<title>Heavy</title><h1>Heavy</h1><div id=root></div><script>\
const r=document.getElementById('root');for(let i=0;i<1000;i++){const a=document.createElement('a');a.href='#'+i;a.textContent='link '+i;r.append(a);}\
for(let i=0;i<4000;i++){const d=document.createElement('div');d.textContent='row '+i;r.append(d);}</script>";

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
                    p => {
                        let body = match p {
                            "/form" => FORM,
                            "/redir" => REDIR,
                            "/poll" => POLL,
                            "/spa" => SPA,
                            "/alert" => ALERT,
                            "/confirm" => CONFIRM,
                            "/beforeunload" => BEFOREUNLOAD,
                            "/heavy" => HEAVY,
                            _ => "<title>other</title><p>other</p>",
                        };
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
