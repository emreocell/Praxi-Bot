//! Hermetic fake coding CLI for native-tools tests (stream-json and ACP dialects).
//! This is never an installed vendor CLI. It records what it was given in
//! `$FAKE_LOG` so tests can check the launch, then asks Praxi for permission the
//! way the real dialect does.
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};

fn log(name: &str, value: &str) {
    if let Ok(dir) = std::env::var("FAKE_LOG") {
        std::fs::write(std::path::Path::new(&dir).join(name), value).unwrap();
    }
}
fn emit(v: &Value) {
    println!("{v}");
    std::io::stdout().flush().unwrap();
}
fn read(reader: &mut impl BufRead) -> Value {
    let mut s = String::new();
    reader.read_line(&mut s).unwrap();
    serde_json::from_str(&s).unwrap()
}
fn http(url: &str, auth: &str, request: &Value) -> Value {
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
    write!(stream, "POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    serde_json::from_str(out.split_once("\r\n\r\n").unwrap().1).unwrap()
}

fn record_launch(args: &[String]) {
    log("argv", &args.join("\n"));
    log(
        "cwd",
        &std::env::current_dir().unwrap().display().to_string(),
    );
    log("pid", &std::process::id().to_string());
    log(
        "env",
        &serde_json::to_string(&std::env::vars().collect::<std::collections::BTreeMap<_, _>>())
            .unwrap(),
    );
    match std::env::var("FAKE_MODE").unwrap_or_default().as_str() {
        "hang" => std::thread::sleep(std::time::Duration::from_secs(120)),
        "exit" => {
            eprintln!("PRIVATE_DIAGNOSTIC");
            std::process::exit(17);
        }
        "flood" => {
            print!("{}", "x".repeat(1024 * 1024 + 10));
            std::io::stdout().flush().unwrap();
            std::process::exit(0);
        }
        _ => {}
    }
}

fn claude(args: &[String]) {
    if args == ["--version"] {
        println!("2.1.325 (Claude Code)");
        return;
    }
    if args == ["--help"] {
        println!(
            "--permission-mode <mode> (choices: \"acceptEdits\", \"auto\", \"bypassPermissions\", \"manual\", \"dontAsk\", \"plan\")"
        );
        println!("--permission-prompt-tool <tool>\n--mcp-config <configs...>");
        return;
    }
    record_launch(args);
    let mut stdin = std::io::stdin().lock();
    let user = read(&mut stdin);
    log("prompt", user["message"]["content"].as_str().unwrap_or(""));
    // Permission prompt for the CLI's own Bash tool, answered by Praxi.
    let config_at = args.iter().position(|a| a == "--mcp-config").unwrap() + 1;
    let config: Value =
        serde_json::from_str(&std::fs::read_to_string(&args[config_at]).unwrap()).unwrap();
    let server = &config["mcpServers"]["praxi"];
    let reply = http(
        server["url"].as_str().unwrap(),
        server["headers"]["Authorization"].as_str().unwrap(),
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{
            "name":"approve","arguments":{"tool_name":"Bash","input":{"command":"touch note.txt"}}}}),
    );
    log(
        "verdict",
        reply["result"]["content"][0]["text"].as_str().unwrap_or(""),
    );
    emit(&json!({"type":"system","subtype":"init"}));
    emit(
        &json!({"type":"stream_event","event":{"delta":{"type":"thinking_delta","thinking":"PRIVATE_THINKING"}}}),
    );
    emit(
        &json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"native "}}}),
    );
    emit(
        &json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"done"}}}),
    );
    emit(
        &json!({"type":"result","subtype":"success","usage":{"input_tokens":3,"output_tokens":2}}),
    );
}

fn acp(args: &[String]) {
    record_launch(args);
    let mut stdin = std::io::stdin().lock();
    let init = read(&mut stdin);
    log("init", &init["params"].to_string());
    emit(&json!({"jsonrpc":"2.0","id":init["id"],"result":{
        "protocolVersion":1,"agentCapabilities":{"mcpCapabilities":{"http":true}},"authMethods":[]}}));
    let new = read(&mut stdin);
    log("session_new", &new["params"].to_string());
    emit(&json!({"jsonrpc":"2.0","id":new["id"],"result":{"sessionId":"s1"}}));
    let mut prompt = read(&mut stdin);
    while matches!(
        prompt["method"].as_str(),
        Some("session/set_mode" | "session/set_model")
    ) {
        emit(&json!({"jsonrpc":"2.0","id":prompt["id"],"result":{}}));
        prompt = read(&mut stdin);
    }
    log("prompt", &prompt["params"]["prompt"][0]["text"].to_string());
    emit(
        &json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{
        "sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"PRIVATE_THINKING"}}}}),
    );
    emit(
        &json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{
        "sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"native acp"}}}}),
    );
    emit(
        &json!({"jsonrpc":"2.0","id":90,"method":"session/request_permission","params":{
        "sessionId":"s1","toolCall":{"toolCallId":"t","title":"touch note.txt","kind":"execute","rawInput":{"command":"touch note.txt"}},
        "options":[{"optionId":"yes","name":"Allow","kind":"allow_once"},{"optionId":"no","name":"Reject","kind":"reject_once"}]}}),
    );
    log("permission_answer", &read(&mut stdin).to_string());
    // fs was not offered: the client must refuse and keep the session alive.
    emit(
        &json!({"jsonrpc":"2.0","id":91,"method":"fs/write_text_file","params":{"path":"x","content":"y"}}),
    );
    log("fs_answer", &read(&mut stdin).to_string());
    emit(&json!({"jsonrpc":"2.0","id":prompt["id"],"result":{"stopReason":"end_turn"}}));
}

fn codex(args: &[String]) {
    record_launch(args);
    let mut stdin = std::io::stdin().lock();
    let init = read(&mut stdin);
    emit(&json!({"jsonrpc":"2.0","id":init["id"],"result":{}}));
    assert_eq!(read(&mut stdin)["method"], "initialized");
    let thread = read(&mut stdin);
    log("thread", &thread.to_string());
    let policy = thread["params"]["sandbox"].as_str().unwrap();
    let policy = if policy == "danger-full-access" {
        "dangerFullAccess"
    } else {
        "workspaceWrite"
    };
    emit(&json!({"jsonrpc":"2.0","id":thread["id"],"result":{
        "thread":{"id":"t1"},"sandbox":{"type":policy},
        "approvalPolicy":thread["params"]["approvalPolicy"]}}));
    let turn = read(&mut stdin);
    log("prompt", &turn["params"]["input"].to_string());
    emit(
        &json!({"jsonrpc":"2.0","method":"item/reasoning/summaryTextDelta","params":{"delta":"PRIVATE_THINKING"}}),
    );
    emit(
        &json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"delta":"native codex"}}),
    );
    emit(
        &json!({"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"status":"completed"}}}),
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `cursor-agent status [--format json]`: the sign-in probe of the Cursor engine.
    if args.first().is_some_and(|a| a == "status") {
        println!("{{\"isAuthenticated\":true}}");
        return;
    }
    if args == ["--version"] && std::env::var("FAKE_DRIVER").as_deref() == Ok("codex") {
        println!("codex-cli 0.134.0");
        return;
    }
    if args.first().is_some_and(|a| a == "app-server") {
        codex(&args);
    } else if args.iter().any(|a| a == "acp" || a == "--acp") {
        acp(&args);
    } else {
        claude(&args);
    }
}
