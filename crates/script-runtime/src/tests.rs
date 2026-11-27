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
        cpu_timeout: Duration::from_secs(5),
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
async fn infinite_loop_is_killed_by_the_compute_budget() {
    let limits = Limits {
        cpu_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let started = Instant::now();
    let err = ScriptRuntime::run("while (true) {}", Arc::new(FakeHost), limits)
        .await
        .unwrap_err();
    assert!(matches!(err, ScriptError::CpuTimeout(_)), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(4));
}

struct StartedHost(Arc<tokio::sync::Notify>);

#[async_trait]
impl ScriptHost for StartedHost {
    async fn call_tool(&self, _name: &str, _input: Value) -> Result<Value, String> {
        self.0.notify_one();
        Ok(Value::Null)
    }
}

#[tokio::test]
async fn cancellation_interrupts_a_busy_loop_before_its_deadline() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let host = Arc::new(StartedHost(Arc::clone(&entered)));
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let handle = tokio::runtime::Handle::current();
    let worker = tokio::task::spawn_blocking(move || {
        handle.block_on(ScriptRuntime::run_cancellable(
            "await tools.call('started', {}); while (true) {}",
            host,
            limits(),
            worker_cancel,
        ))
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("script did not start");
    cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .expect("cancelled worker did not stop")
        .unwrap()
        .unwrap_err();
    assert_eq!(error, ScriptError::Cancelled);
}

#[tokio::test]
async fn an_already_cancelled_script_does_not_start() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = ScriptRuntime::run_cancellable(
        "await tools.call('started', {});",
        Arc::new(StartedHost(Arc::clone(&entered))),
        limits(),
        cancel,
    )
    .await
    .unwrap_err();
    assert_eq!(error, ScriptError::Cancelled);
    assert!(
        tokio::time::timeout(Duration::ZERO, entered.notified())
            .await
            .is_err(),
        "a cancelled script must not call its host"
    );
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

/// A host whose every call takes `delay` of real time.
struct SlowHost {
    delay: Duration,
}

#[async_trait]
impl ScriptHost for SlowHost {
    async fn call_tool(&self, _: &str, input: Value) -> Result<Value, String> {
        tokio::time::sleep(self.delay).await;
        Ok(input)
    }
}

/// A script's own work for about `ms` milliseconds.
fn busy(ms: u32) -> String {
    format!("{{ const t = Date.now(); while (Date.now() - t < {ms}) {{}} }}")
}

#[tokio::test(start_paused = true)]
async fn the_total_cap_applies_while_awaiting_a_slow_tool() {
    let limits = Limits {
        total_timeout: Duration::from_millis(200),
        ..Limits::default()
    };
    let host = SlowHost {
        delay: Duration::from_secs(30),
    };
    let err = ScriptRuntime::run("return await tools.call('x', {});", Arc::new(host), limits)
        .await
        .unwrap_err();
    assert!(matches!(err, ScriptError::Timeout(_)), "{err:?}");
}

#[tokio::test]
async fn waiting_for_tools_does_not_spend_the_compute_budget() {
    // Three sequential calls of 400 ms each: 1.2 s of waiting against a
    // compute budget of 300 ms. The old single wall timer would have fired.
    let limits = Limits {
        cpu_timeout: Duration::from_millis(300),
        total_timeout: Duration::from_secs(30),
        ..Limits::default()
    };
    let host = SlowHost {
        delay: Duration::from_millis(400),
    };
    let code = "const a = await tools.call('x', 1); const b = await tools.call('x', 2); \
                const c = await tools.call('x', 3); return a + b + c;";
    let out = ScriptRuntime::run(code, Arc::new(host), limits)
        .await
        .unwrap();
    assert_eq!(out.result, json!(6));
    assert_eq!(out.tool_calls.len(), 3);
    assert!(
        out.duration >= Duration::from_millis(1200),
        "{:?}",
        out.duration
    );
}

#[tokio::test]
async fn overlapping_tool_calls_keep_the_compute_clock_stopped() {
    let limits = Limits {
        cpu_timeout: Duration::from_millis(300),
        total_timeout: Duration::from_secs(30),
        ..Limits::default()
    };
    let host = SlowHost {
        delay: Duration::from_millis(500),
    };
    let code = "const r = await Promise.all([1, 2, 3].map(n => tools.call('x', n))); \
                return r.reduce((a, b) => a + b, 0);";
    let started = Instant::now();
    let out = ScriptRuntime::run(code, Arc::new(host), limits)
        .await
        .unwrap();
    assert_eq!(out.result, json!(6));
    // They really ran at the same time, and the clock came back afterwards.
    assert!(started.elapsed() < Duration::from_millis(1400));
}

#[tokio::test]
async fn the_compute_budget_adds_up_across_the_pieces_between_tool_calls() {
    // 2 x ~200 ms of the script's own work around a call, budget 300 ms: the
    // sum is over budget although no single piece is.
    let limits = Limits {
        cpu_timeout: Duration::from_millis(300),
        total_timeout: Duration::from_secs(30),
        ..Limits::default()
    };
    let host = SlowHost {
        delay: Duration::from_millis(50),
    };
    let code = format!(
        "{} await tools.call('x', 1); {} return 1;",
        busy(200),
        busy(200)
    );
    let err = ScriptRuntime::run(&code, Arc::new(host), limits)
        .await
        .unwrap_err();
    assert!(matches!(err, ScriptError::CpuTimeout(_)), "{err:?}");
}

#[tokio::test]
async fn a_busy_loop_after_a_long_wait_is_still_stopped_by_the_compute_budget() {
    let limits = Limits {
        cpu_timeout: Duration::from_millis(300),
        total_timeout: Duration::from_secs(30),
        ..Limits::default()
    };
    let host = SlowHost {
        delay: Duration::from_millis(400),
    };
    let started = Instant::now();
    let err = ScriptRuntime::run(
        "await tools.call('x', 1); while (true) {}",
        Arc::new(host),
        limits,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ScriptError::CpuTimeout(_)), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[tokio::test]
async fn the_total_cap_stops_a_busy_loop_that_the_compute_budget_would_allow() {
    let limits = Limits {
        cpu_timeout: Duration::from_secs(60),
        total_timeout: Duration::from_millis(300),
        ..Limits::default()
    };
    let started = Instant::now();
    let err = ScriptRuntime::run("while (true) {}", Arc::new(FakeHost), limits)
        .await
        .unwrap_err();
    assert!(matches!(err, ScriptError::Timeout(_)), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[tokio::test(start_paused = true)]
async fn the_total_is_bounded_whatever_the_limits_say() {
    // A caller asking for a day gets the hard maximum, not a day.
    let limits = Limits {
        total_timeout: Duration::from_secs(24 * 3600),
        ..Limits::default()
    };
    let host = SlowHost {
        delay: Duration::from_secs(24 * 3600),
    };
    let err = ScriptRuntime::run("return await tools.call('x', {});", Arc::new(host), limits)
        .await
        .unwrap_err();
    assert_eq!(err, ScriptError::Timeout(MAX_TOTAL_TIMEOUT));
}

#[test]
fn the_default_limits_keep_the_compute_budget_at_ten_seconds_with_a_bounded_total() {
    let d = Limits::default();
    assert_eq!(d.cpu_timeout, Duration::from_secs(10));
    assert!(d.total_timeout > d.cpu_timeout);
    assert!(d.total_timeout <= MAX_TOTAL_TIMEOUT);
}

#[tokio::test]
async fn deep_recursion_is_a_js_error_not_a_crash() {
    let err = run("function f(n) { return f(n + 1) + 1; } return f(0);")
        .await
        .unwrap_err();
    assert!(matches!(err, ScriptError::Js(_)), "{err:?}");
}

/// A host where the tool named `slow` takes 60 s and everything else 20 ms.
struct SlowAndFastHost;

#[async_trait]
impl ScriptHost for SlowAndFastHost {
    async fn call_tool(&self, name: &str, input: Value) -> Result<Value, String> {
        let delay = if name == "slow" {
            Duration::from_secs(60)
        } else {
            Duration::from_millis(20)
        };
        tokio::time::sleep(delay).await;
        Ok(input)
    }
}

/// A call that is started and never awaited stays pending for the rest of the run.
/// It must not stop the compute clock while the script computes in the meantime.
#[tokio::test]
async fn a_pending_fire_and_forget_call_does_not_hide_a_busy_loop() {
    let limits = Limits {
        cpu_timeout: Duration::from_millis(300),
        total_timeout: Duration::from_secs(30),
        ..Limits::default()
    };
    for code in [
        // The slow call is polled once the script yields at the awaited fast one.
        "tools.call('slow', 1); await tools.call('fast', 2); while (true) {}",
        // Several in flight, then the loop.
        "tools.call('slow', 1); tools.call('slow', 2); await tools.call('fast', 3); \
         await tools.call('fast', 4); while (true) { tools.call('fast', 5).catch(() => {}); }",
    ] {
        let started = Instant::now();
        let err = ScriptRuntime::run(
            code,
            Arc::new(SlowAndFastHost),
            Limits {
                max_tool_calls: 10_000_000,
                ..limits.clone()
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ScriptError::CpuTimeout(_)), "{code}: {err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{code}: ran {:?}",
            started.elapsed()
        );
    }
}

/// The pause is by awaiting, not by a count of pending calls: waiting on a slow
/// call still costs nothing, even next to other calls that already finished.
#[tokio::test]
async fn awaiting_the_slow_call_while_others_finish_still_spends_no_budget() {
    let limits = Limits {
        cpu_timeout: Duration::from_millis(300),
        total_timeout: Duration::from_secs(30),
        ..Limits::default()
    };
    let code = "const slow = tools.call('wait', 1); \
                for (let i = 0; i < 5; i++) { await tools.call('fast', i); } \
                return await slow;";
    struct Host;
    #[async_trait]
    impl ScriptHost for Host {
        async fn call_tool(&self, name: &str, input: Value) -> Result<Value, String> {
            let delay = if name == "wait" { 600 } else { 20 };
            tokio::time::sleep(Duration::from_millis(delay)).await;
            Ok(input)
        }
    }
    let out = ScriptRuntime::run(code, Arc::new(Host), limits)
        .await
        .unwrap();
    assert_eq!(out.result, json!(1));
}
