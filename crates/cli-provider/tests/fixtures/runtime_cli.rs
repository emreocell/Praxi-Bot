//! Hermetic CLI protocol fixture. This is never an installed vendor CLI.
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};

#[allow(clippy::needless_pass_by_value)] // Fixture frames are intentionally written inline.
fn emit(v: Value) {
    println!("{v}");
    std::io::stdout().flush().unwrap();
}
fn read(reader: &mut impl BufRead) -> Value {
    let mut s = String::new();
    reader.read_line(&mut s).unwrap();
    serde_json::from_str(&s).unwrap()
}
fn log(name: &str, value: &str) {
    if let Ok(dir) = std::env::var("FAKE_LOG") {
        std::fs::write(std::path::Path::new(&dir).join(name), value).unwrap();
    }
}
#[cfg(windows)]
#[allow(unsafe_code)]
fn raw_command_line() -> String {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCommandLineW() -> *const u16;
    }
    // Windows owns this nul-terminated buffer for the process lifetime.
    unsafe {
        let ptr = GetCommandLineW();
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
    }
}
#[cfg(not(windows))]
fn raw_command_line() -> String {
    std::env::args().collect::<Vec<_>>().join(" ")
}
#[allow(clippy::needless_pass_by_value)]
fn http(url: &str, auth: &str, request: Value) -> Value {
    let address = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .unwrap();
    let body = request.to_string();
    write!(stream,"POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    serde_json::from_str(out.split_once("\r\n\r\n").unwrap().1).unwrap()
}
#[allow(clippy::too_many_lines)] // Linear fixture transcript, rather than production dispatch.
fn main() {
    let args: Vec<_> = std::env::args().collect();
    if std::env::current_exe().unwrap().file_name().unwrap() == "node.exe" {
        assert!(
            args.get(1).is_some_and(|arg| arg.ends_with("cli.js")),
            "npm shim lost its script argument"
        );
    }
    if args.iter().any(|s| s == "--version") {
        println!(
            "{} (Fixture)",
            std::env::var("FAKE_VERSION").unwrap_or_else(|_| "2.1.300".into())
        );
        return;
    }
    if args.iter().any(|s| s == "--help" || s == "-h") {
        let modes = std::env::var("FAKE_PERMISSION_MODES")
            .unwrap_or_else(|_| "acceptEdits,auto,bypassPermissions,manual,dontAsk,plan".into());
        let quoted = modes
            .split(',')
            .filter(|mode| !mode.trim().is_empty())
            .map(|mode| format!("\"{}\"", mode.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        println!("--permission-mode <mode> (choices: {quoted})");
        println!("--tools Specify the list. Use \"\" to disable all tools");
        println!("--disallowedTools");
        println!("--settings");
        return;
    }
    log("argv", &raw_command_line());
    log("pid", &std::process::id().to_string());
    log(
        "env",
        &serde_json::to_string(&std::env::vars().collect::<std::collections::BTreeMap<_, _>>())
            .unwrap(),
    );
    let mode = std::env::var("FAKE_MODE").unwrap_or_default();
    if mode == "hang" {
        std::thread::sleep(std::time::Duration::from_secs(120));
        return;
    }
    if mode == "flood" {
        print!("{}", "x".repeat(1024 * 1024 + 10));
        std::io::stdout().flush().unwrap();
        return;
    }
    if mode == "quota" {
        eprint!("{}", "SENSITIVE_STDERR ".repeat(5000));
        emit(json!({"type":"result","is_error":true,"error":"rate_limit quota"}));
        return;
    }
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let (url, auth, tool) = if args.iter().any(|s| s == "-p") {
        let config = args.windows(2).find(|w| w[0] == "--mcp-config").unwrap()[1].clone();
        log("config", &config);
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let prompt = read(&mut reader);
        log("stdin", &prompt.to_string());
        assert_eq!(prompt["type"], "user");
        if mode == "unknown-frame" {
            emit(json!({"type":"system","subtype":"init"}));
            emit(
                json!({"type":"stream_event","event":{"delta":{"type":"text_delta","text":"SHOULD_NOT_APPLY"}}}),
            );
            std::thread::sleep(std::time::Duration::from_secs(120));
            return;
        }
        if mode == "bash" {
            emit(
                json!({"type":"stream_event","event":{"type":"content_block_start","content_block":{"type":"tool_use","name":"Bash","input":{"command":"bad"}}}}),
            );
            std::thread::sleep(std::time::Duration::from_secs(120));
            return;
        }
        emit(
            json!({"type":"stream_event","event":{"delta":{"type":"thinking_delta","thinking":"PRIVATE_THINKING"}}}),
        );
        emit(json!({"type":"stream_event","event":{"delta":{"type":"text_delta","text":"hello"}}}));
        let m = &v["mcpServers"]["praxi"];
        (
            m["url"].as_str().unwrap().to_owned(),
            m["headers"]["Authorization"].as_str().unwrap().to_owned(),
            String::new(),
        )
    } else {
        let init = read(&mut reader);
        assert_eq!(init["method"], "initialize");
        emit(json!({"id":1,"result":{}}));
        assert_eq!(read(&mut reader)["method"], "initialized");
        let thread = read(&mut reader);
        assert_eq!(thread["method"], "thread/start");
        log("thread", &thread.to_string());
        if mode == "no-echo" {
            emit(json!({"id":2,"result":{"thread":{"id":"fixture-thread"}}}));
            std::thread::sleep(std::time::Duration::from_secs(120));
            return;
        }
        if mode == "partial-echo" {
            emit(json!({
                "id":2,
                "result":{
                    "thread":{"id":"fixture-thread"},
                    "config":{"features":{"shell_tool":false},"web_search":"disabled"}
                }
            }));
            std::thread::sleep(std::time::Duration::from_secs(120));
            return;
        }
        // Echo the closed set the parent sent, plus empty environments.
        emit(json!({
            "id":2,
            "result":{
                "thread":{"id":"fixture-thread","environments":[]},
                "environments": thread["params"]["environments"].clone(),
                "sandbox": thread["params"]["sandbox"].clone(),
                "approvalPolicy": thread["params"]["approvalPolicy"].clone(),
                "config": thread["params"]["config"].clone()
            }
        }));
        let turn = read(&mut reader);
        assert_eq!(turn["method"], "turn/start");
        log("stdin", &turn.to_string());
        emit(json!({"id":3,"result":{}}));
        if let Some(method) = mode.strip_prefix("bad-method:") {
            if method.contains("Delta") || method.contains("patchUpdated") {
                emit(json!({"method": method, "params": {}}));
            } else {
                emit(json!({"method": method, "id": 9, "params": {}}));
            }
            emit(json!({"method":"item/agentMessage/delta","params":{"delta":"SHOULD_NOT_APPLY"}}));
            std::thread::sleep(std::time::Duration::from_secs(120));
            return;
        }
        if mode == "native" {
            emit(json!({"method":"item/commandExecution/requestApproval","id":9,"params":{}}));
            std::thread::sleep(std::time::Duration::from_secs(120));
            return;
        }
        let m = &thread["params"]["config"]["mcp_servers"]["praxi"];
        (
            m["url"].as_str().unwrap().to_owned(),
            m["http_headers"]["Authorization"]
                .as_str()
                .unwrap()
                .to_owned(),
            String::new(),
        )
    };
    if mode == "hang-tree" {
        let dir = std::path::PathBuf::from(std::env::var("FAKE_LOG").unwrap()).join("descendant");
        std::fs::create_dir(&dir).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .env("FAKE_MODE", "hang")
            .env("FAKE_LOG", dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let _ = child.wait();
        return;
    }
    let listing = http(
        &url,
        &auth,
        json!({"jsonrpc":"2.0","id":10,"method":"tools/list"}),
    );
    let tool = listing["result"]["tools"][0]["name"]
        .as_str()
        .unwrap_or(&tool)
        .to_owned();
    if !args.iter().any(|s| s == "-p") {
        emit(
            json!({"method":"item/mcpToolCall/requestApproval","id":9,"params":{"server":"praxi","tool":tool,"arguments":{"x":1}}}),
        );
        let approval = read(&mut reader);
        log("approval", &approval.to_string());
        if approval["result"]["decision"] == "decline" {
            emit(json!({"method":"turn/completed","params":{"turn":{"status":"completed"}}}));
            return;
        }
    }
    let result = http(
        &url,
        &auth,
        json!({"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":tool,"arguments":{"x":1}}}),
    );
    log("tool_reply", &result.to_string());
    if args.iter().any(|s| s == "-p") {
        emit(json!({"type":"result","usage":{"input_tokens":12,"output_tokens":4}}));
    } else {
        emit(json!({"method":"item/agentMessage/delta","params":{"delta":"hello"}}));
        emit(
            json!({"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"last":{"inputTokens":12,"outputTokens":4}}}}),
        );
        emit(json!({"method":"turn/completed","params":{"turn":{"status":"completed"}}}));
    }
}
