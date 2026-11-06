use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::*;

/// Fake host: `add` sums `a` and `b`, `echo` returns its input, `fail` errors.
struct FakeHost;

#[async_trait]
impl ScriptHost for FakeHost {
    async fn call_tool(&self, name: &str, input: Value) -> Result<Value, String> {
        match name {
            "add" => {
                let a = input["a"].as_i64().ok_or("a missing")?;
                let b = input["b"].as_i64().ok_or("b missing")?;
                Ok(json!(a + b))
            }
            "echo" => Ok(input),
            "fail" => Err("boom".to_owned()),
            other => Err(format!("unknown tool {other}")),
        }
    }
}

fn limits() -> Limits {
    Limits {
        timeout: Duration::from_secs(5),
        ..Limits::default()
    }
}

async fn run(code: &str) -> Result<ScriptOutput, ScriptError> {
    ScriptRuntime::run(code, Arc::new(FakeHost), limits()).await
}

#[tokio::test]
async fn pure_computation() {
    let out = run("const xs = [1,2,3,4]; return xs.map(x => x * x).reduce((a, b) => a + b, 0);")
        .await
        .unwrap();
    assert_eq!(out.result, json!(30));
    assert_eq!(out.tool_calls.len(), 0);
    assert!(!out.truncated);
}

#[tokio::test]
async fn returns_objects_and_null_for_nothing() {
    let out = run("return { a: [1, 'x'], b: null };").await.unwrap();
    assert_eq!(out.result, json!({"a": [1, "x"], "b": null}));
    let out = run("const x = 1;").await.unwrap();
    assert_eq!(out.result, Value::Null);
}

#[tokio::test]
async fn console_log_is_captured() {
    let out = run("console.log('hi', 1, {a: 2}); console.error('bad'); return 1;")
        .await
        .unwrap();
    assert_eq!(out.logs, vec!["hi 1 {\"a\":2}", "[error] bad"]);
}

#[tokio::test]
async fn three_tool_calls_aggregated() {
    let code = r"
        let total = 0;
        for (const n of [1, 2, 3]) {
            total = await tools.call('add', { a: total, b: n });
        }
        const e = await tools.call('echo', { k: 'v' });
        return { total, e };
    ";
    let out = run(code).await.unwrap();
    // running sum: 0+1=1, 1+2=3, 3+3=6
    assert_eq!(out.result["total"], json!(6));
    assert_eq!(out.result["e"], json!({"k": "v"}));
    assert_eq!(out.tool_calls.len(), 4);
    assert_eq!(out.tool_calls[0].name, "add");
    assert_eq!(out.tool_calls[0].input, json!({"a": 0, "b": 1}));
    assert_eq!(out.tool_calls[0].output, Some(json!(1)));
}

#[tokio::test]
async fn tool_error_is_catchable_and_recorded() {
    let code = r"
        try { await tools.call('fail', {}); return 'no'; }
        catch (e) { return e.message; }
    ";
    let out = run(code).await.unwrap();
    assert_eq!(out.result, json!("boom"));
    assert_eq!(out.tool_calls[0].error.as_deref(), Some("boom"));
    assert_eq!(out.tool_calls[0].output, None);
}

#[tokio::test]
async fn infinite_loop_is_killed_by_timeout() {
    let limits = Limits {
        timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let started = Instant::now();
    let err = ScriptRuntime::run("while (true) {}", Arc::new(FakeHost), limits)
        .await
        .unwrap_err();
    assert!(matches!(err, ScriptError::Timeout(_)), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[tokio::test]
async fn memory_bomb_is_stopped() {
    let limits = Limits {
        memory_bytes: 8 * 1024 * 1024,
        ..limits()
    };
    let code = "const a = []; while (true) { a.push('x'.repeat(100000) + a.length); }";
    let err = ScriptRuntime::run(code, Arc::new(FakeHost), limits)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ScriptError::MemoryLimitExceeded { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn huge_single_allocation_is_stopped() {
    let limits = Limits {
        memory_bytes: 8 * 1024 * 1024,
        ..limits()
    };
    let err = ScriptRuntime::run("return 'x'.repeat(1e9).length;", Arc::new(FakeHost), limits)
        .await
        .unwrap_err();
    assert!(
        matches!(err, ScriptError::MemoryLimitExceeded { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn tool_call_cap_is_enforced() {
    let limits = Limits {
        max_tool_calls: 3,
        ..limits()
    };
    let code = "for (let i = 0; i < 10; i++) { await tools.call('echo', { i }); } return 'done';";
    let err = ScriptRuntime::run(code, Arc::new(FakeHost), limits)
        .await
        .unwrap_err();
    assert_eq!(err, ScriptError::ToolCallLimitExceeded { max: 3 });
}

#[tokio::test]
async fn tool_call_cap_cannot_be_swallowed_by_script() {
    let limits = Limits {
        max_tool_calls: 1,
        ..limits()
    };
    let code = r"
        await tools.call('echo', {});
        try { await tools.call('echo', {}); } catch (e) {}
        return 'ok';
    ";
    let err = ScriptRuntime::run(code, Arc::new(FakeHost), limits)
        .await
        .unwrap_err();
    assert_eq!(err, ScriptError::ToolCallLimitExceeded { max: 1 });
}

#[tokio::test]
async fn syntax_error_reports_line_number() {
    let err = run("const a = 1;\nconst b = 2;\nconst c = (1 + ;\nreturn a;")
        .await
        .unwrap_err();
    let ScriptError::Js(e) = err else {
        panic!("expected Js error, got {err:?}");
    };
    assert_eq!(e.line, Some(3), "{e:?}");
    assert!(e.message.contains("SyntaxError"), "{e:?}");
}

#[tokio::test]
async fn runtime_error_carries_stack_and_line() {
    let err = run("const a = 1;\n\nundefinedFunction(a);\nreturn 1;")
        .await
        .unwrap_err();
    let ScriptError::Js(e) = err else {
        panic!("expected Js error, got {err:?}");
    };
    assert!(e.message.contains("undefinedFunction"), "{e:?}");
    assert_eq!(e.line, Some(3), "{e:?}");
    assert!(e.stack.is_some());
}

#[tokio::test]
async fn thrown_non_error_value_is_reported() {
    let err = run("throw 'plain string';").await.unwrap_err();
    let ScriptError::Js(e) = err else {
        panic!("expected Js error, got {err:?}");
    };
    assert!(e.message.contains("plain string"), "{e:?}");
}

#[tokio::test]
async fn no_ambient_capabilities() {
    let code = r"
        const names = ['require', 'fetch', 'process', 'XMLHttpRequest', 'WebSocket',
                       'setTimeout', 'setInterval', 'Deno', 'Bun', 'std', 'os', 'module',
                       'exports', '__dirname', 'readFile', 'nativeCall', 'nativeLog'];
        const present = names.filter(n => typeof globalThis[n] !== 'undefined');
        let importFailed = false;
        try { await import('fs'); } catch (e) { importFailed = true; }
        return { present, requireType: typeof require, fetchType: typeof fetch, importFailed };
    ";
    let out = run(code).await.unwrap();
    assert_eq!(out.result["present"], json!([]));
    assert_eq!(out.result["requireType"], json!("undefined"));
    assert_eq!(out.result["fetchType"], json!("undefined"));
    assert_eq!(out.result["importFailed"], json!(true));
}

#[tokio::test]
async fn static_import_and_calling_require_fail() {
    assert!(matches!(
        run("import fs from 'fs'; return 1;").await,
        Err(ScriptError::Js(_))
    ));
    assert!(matches!(
        run("return require('fs');").await,
        Err(ScriptError::Js(_))
    ));
    assert!(matches!(
        run("return await fetch('http://example.com');").await,
        Err(ScriptError::Js(_))
    ));
}

#[tokio::test]
async fn tools_object_is_frozen() {
    // User code is sloppy-mode, so the assignment is silently ignored.
    let out = run("try { tools.call = null; } catch (e) {} return typeof tools.call;")
        .await
        .unwrap();
    assert_eq!(out.result, json!("function"));
}

#[tokio::test]
async fn large_result_is_truncated_with_marker() {
    let limits = Limits {
        max_output_bytes: 100,
        ..limits()
    };
    let out = ScriptRuntime::run("return 'y'.repeat(10000);", Arc::new(FakeHost), limits)
        .await
        .unwrap();
    assert!(out.truncated);
    let s = out.result.as_str().unwrap();
    assert!(s.ends_with(TRUNCATION_MARKER));
    assert!(s.len() <= 100 + TRUNCATION_MARKER.len());
}

#[tokio::test]
async fn large_logs_are_truncated_with_marker() {
    let limits = Limits {
        max_output_bytes: 50,
        ..limits()
    };
    let code = "for (let i = 0; i < 100; i++) console.log('line number ' + i); return 1;";
    let out = ScriptRuntime::run(code, Arc::new(FakeHost), limits)
        .await
        .unwrap();
    assert!(out.truncated);
    assert!(out.logs.last().unwrap().ends_with(TRUNCATION_MARKER));
    assert!(out.logs.len() < 100);
}

#[tokio::test]
async fn timeout_applies_while_awaiting_a_slow_tool() {
    struct Slow;
    #[async_trait]
    impl ScriptHost for Slow {
        async fn call_tool(&self, _: &str, _: Value) -> Result<Value, String> {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Ok(Value::Null)
        }
    }
    let limits = Limits {
        timeout: Duration::from_millis(200),
        ..Limits::default()
    };
    let err = ScriptRuntime::run("return await tools.call('x', {});", Arc::new(Slow), limits)
        .await
        .unwrap_err();
    assert!(matches!(err, ScriptError::Timeout(_)), "{err:?}");
}

#[tokio::test]
async fn deep_recursion_is_a_js_error_not_a_crash() {
    let err = run("function f(n) { return f(n + 1) + 1; } return f(0);")
        .await
        .unwrap_err();
    assert!(matches!(err, ScriptError::Js(_)), "{err:?}");
}
