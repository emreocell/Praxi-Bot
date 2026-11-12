//! Whole-turn CLI handoff. Tool execution stays in `exec_tools`, including policy,
//! approvals, redaction, spills, timeouts and the existing run stream events.
use crate::{
    exec::{CliApproval, TurnExecutor},
    shared::{ModelOut, STEP_TOOLS, Shared, xe},
};
use async_trait::async_trait;
use cli_provider::runtime::{CliEngine, Engine, ToolReply, Turn, TurnHost};
use permission_engine::Decision;
use protocol::{Event, ToolCallId, event::MessageDelta};
use provider_catalog::{
    ChatRequest, MessageRole, ProviderConfig, Usage,
    config::{CliEngine as ConfigEngine, config_path},
};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::sync::Arc;
use storage::Run;
use task_runtime::{ExecError, StepSpec};
use tokio_util::sync::CancellationToken;
use tool_registry::ToolSpec;

struct Host {
    sh: Arc<Shared>,
    run: Run,
    approved: parking_lot::Mutex<Vec<CliApproval>>,
}
#[async_trait]
impl TurnHost for Host {
    fn text(&self, text: &str) {
        if let Ok(ctx) = self.sh.ctx(&self.run) {
            self.sh.emit(
                &ctx,
                Event::MessageDelta(MessageDelta {
                    run_id: self.run.id,
                    text: self.sh.redact(text.to_owned()),
                }),
            );
        }
    }
    async fn call(&self, name: &str, input: Value) -> ToolReply {
        let exec = TurnExecutor {
            sh: self.sh.clone(),
        };
        let step = StepSpec {
            name: STEP_TOOLS.into(),
            input: json!({"calls":[{"id":ToolCallId::new().to_string(),"name":name,"arguments":input.to_string(),"repeat":1}]}),
            idempotent: false,
            retry: None,
        };
        let approval = {
            let resolved = self.sh.ctx(&self.run).ok().and_then(|ctx| {
                self.sh
                    .resolve_call(&ctx.profile, ctx.bot_id, name, &input, &[])
            });
            let mut approvals = self.approved.lock();
            approvals
                .iter()
                .position(|card| {
                    resolved
                        .as_ref()
                        .is_some_and(|r| card.name == r.name && card.input == r.input)
                })
                .map(|i| approvals.remove(i))
        };
        match exec.exec_tools_approved(&self.run, &step, approval).await {
            Ok(v) => ToolReply {
                text: v["results"][0]["text"]
                    .as_str()
                    .unwrap_or("tool failed")
                    .into(),
                is_error: v["results"][0]["is_error"].as_bool().unwrap_or(true),
            },
            Err(_) => ToolReply {
                text: "tool execution failed".into(),
                is_error: true,
            },
        }
    }
    async fn approve(&self, name: &str, input: Value) -> bool {
        let Ok(ctx) = self.sh.ctx(&self.run) else {
            return false;
        };
        let Some(r) = self
            .sh
            .resolve_call(&ctx.profile, ctx.bot_id, name, &input, &[])
        else {
            return false;
        };
        let Some(spec) = r.spec else {
            return false;
        };
        let decision = self
            .sh
            .decide(&ctx, &r.name, spec.effect, spec.category, &r.input);
        // An allow is not stored. Recording it would let a later ask, including
        // one that carries unevaluated limits, skip the interactive path.
        match &decision {
            Decision::Allow => true,
            Decision::Deny => false,
            Decision::Ask { .. } => {
                let honored_limits = decision.unsupported_limits().cloned();
                let Ok(details) = self.sh.approval_details(&ctx, &r.name, &r.input).await else {
                    return false;
                };
                let Ok(permit) = self
                    .sh
                    .request_approval(
                        &ctx,
                        ToolCallId::new(),
                        &r.name,
                        spec.effect,
                        spec.category,
                        &r.input,
                        details,
                    )
                    .await
                else {
                    return false;
                };
                let mut approved = self.approved.lock();
                // Bound stale approvals when a CLI never follows through with a call.
                // Dropping a permit records no grant.
                if approved.len() == 128 {
                    approved.remove(0);
                }
                approved.push(CliApproval {
                    name: r.name,
                    input: r.input,
                    permit,
                    honored_limits,
                });
                true
            }
        }
    }
}

struct RecordingHost {
    inner: Host,
    text: parking_lot::Mutex<String>,
}
#[async_trait]
impl TurnHost for RecordingHost {
    fn text(&self, text: &str) {
        self.text.lock().push_str(text);
        self.inner.text(text);
    }
    async fn call(&self, name: &str, input: Value) -> ToolReply {
        self.inner.call(name, input).await
    }
    async fn approve(&self, name: &str, input: Value) -> bool {
        self.inner.approve(name, input).await
    }
}

impl TurnExecutor {
    pub(crate) async fn exec_cli(
        &self,
        run: &Run,
        req: &ChatRequest,
        specs: &[ToolSpec],
        cancel: &CancellationToken,
    ) -> Result<ModelOut, ExecError> {
        let cfg = ProviderConfig::load(&config_path(self.sh.storage.data_dir())).map_err(xe)?;
        let id = req.model.provider().unwrap_or(&req.model.0);
        let settings = cfg
            .get(id)
            .filter(|p| p.enabled)
            .ok_or_else(|| xe("CLI provider is not configured"))?;
        let cli = settings
            .cli
            .as_ref()
            .ok_or_else(|| xe("CLI provider is not configured"))?;
        let (engine, command) = cli.parts();
        let engine = match engine {
            ConfigEngine::Claude => Engine::Claude,
            ConfigEngine::Codex => Engine::Codex,
        };
        let runtime = CliEngine {
            engine,
            command: command.into(),
            env: cli_provider::PathEnv::from_ambient(),
        };
        let host = Arc::new(RecordingHost {
            inner: Host {
                sh: self.sh.clone(),
                run: run.clone(),
                approved: parking_lot::Mutex::new(Vec::new()),
            },
            text: parking_lot::Mutex::new(String::new()),
        });
        let mut system = String::new();
        let mut prompt = String::new();
        for m in &req.messages {
            let text = m
                .content
                .as_str()
                .map_or_else(|| m.content.to_string(), str::to_owned);
            if m.role == MessageRole::System {
                system.push_str(&text);
                system.push('\n');
            } else {
                writeln!(&mut prompt, "{:?}: {text}", m.role).expect("string write");
            }
        }
        // MCP encodes names reversibly; permissions receive the original name.
        let tools = specs.to_vec();
        let model = req.model.provider().map(|_| req.model.model().to_owned());
        let usage = tokio::time::timeout(
            self.sh.cfg.guards.model_timeout,
            runtime.run_turn(
                Turn {
                    prompt,
                    system,
                    model,
                    tools,
                },
                host.clone(),
                cancel.clone(),
            ),
        )
        .await
        .map_err(|_| xe("CLI turn timed out"))?
        .map_err(xe)?;
        Ok(ModelOut {
            text: self.sh.redact(host.text.lock().clone()),
            model: req.model.0.clone(),
            usage: usage.reported.then_some(Usage {
                prompt_tokens: usage.input,
                completion_tokens: usage.output,
                cached_tokens: usage.cached,
                cache_write_tokens: None,
            }),
            finish: "Stop".into(),
            ..ModelOut::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{cfg, profile, setup, tool};
    use permission_engine::Policy;
    use std::sync::atomic::Ordering;
    use tool_registry::{Effect, ToolRegistry};
    #[tokio::test]
    async fn bridge_reuses_policy_and_tool_events() {
        for allow in [false, true] {
            let env = setup();
            let t = Arc::new(tool("test.read", Effect::Write, "bridge output"));
            let registry = ToolRegistry::new();
            registry.register(t.clone());
            let lp = crate::AgentLoop::new(
                env.storage.clone(),
                registry,
                crate::Providers::single(Arc::new(UnusedHttp)),
                cfg(if allow {
                    Policy::full()
                } else {
                    Policy::read_only()
                }),
            );
            lp.add_profile(env.bot, profile(""));
            // A normal completed HTTP turn supplies the durable run context;
            // the host then exercises precisely the bridge's permission path.
            let run_id = lp.run_turn(env.chat, "hi").unwrap();
            lp.wait(run_id).await.unwrap();
            let run = env.storage.get_run(run_id).unwrap().unwrap();
            let host = Host {
                sh: lp.sh.clone(),
                run,
                approved: parking_lot::Mutex::new(Vec::new()),
            };
            let reply = host.call("test.read", json!({})).await;
            assert_eq!(reply.is_error, !allow);
            assert_eq!(t.count.load(Ordering::SeqCst), usize::from(allow));
        }
    }
    struct UnusedHttp;
    #[async_trait]
    impl provider_catalog::ChatProvider for UnusedHttp {
        fn id(&self) -> &'static str {
            "fake"
        }
        async fn stream(
            &self,
            _: ChatRequest,
        ) -> Result<
            futures::stream::BoxStream<
                'static,
                Result<provider_catalog::ChatChunk, provider_catalog::ProviderError>,
            >,
            provider_catalog::ProviderError,
        > {
            Ok(Box::pin(futures::stream::iter([Ok(
                provider_catalog::ChatChunk::Done {
                    finish_reason: provider_catalog::FinishReason::Stop,
                },
            )])))
        }
        async fn list_models(&self) -> Result<Vec<String>, provider_catalog::ProviderError> {
            Ok(vec![])
        }
    }
    #[tokio::test]
    async fn cli_executor_does_not_call_http_for_missing_binary() {
        let env = setup();
        let registry = ToolRegistry::new();
        let lp = crate::AgentLoop::new(
            env.storage.clone(),
            registry,
            crate::Providers::single(Arc::new(PanicHttp)),
            cfg(Policy::full()),
        );
        let mut p = profile("");
        p.models.executor = "cli:claude".into();
        lp.add_profile(env.bot, p);
        let mut setting = provider_catalog::ProviderSettings::new("cli:claude", 0);
        setting.cli = Some(provider_catalog::config::CliSettings::Cli {
            engine: ConfigEngine::Claude,
            command: "praxi-c062-nonexistent-fixture".into(),
        });
        provider_catalog::ProviderConfig {
            providers: vec![setting],
            default_provider: None,
        }
        .save(&config_path(env.dir.path()))
        .unwrap();
        let run = lp.run_turn(env.chat, "private\n%PATH%").unwrap();
        assert_eq!(lp.wait(run).await.unwrap(), task_runtime::RunState::Failed);
        let events = storage::read_event_log(env.dir.path(), run).unwrap();
        assert!(format!("{events:?}").contains("not_installed"));
    }
    struct PanicHttp;
    #[async_trait]
    impl provider_catalog::ChatProvider for PanicHttp {
        fn id(&self) -> &'static str {
            "fake"
        }
        async fn stream(
            &self,
            _: ChatRequest,
        ) -> Result<
            futures::stream::BoxStream<
                'static,
                Result<provider_catalog::ChatChunk, provider_catalog::ProviderError>,
            >,
            provider_catalog::ProviderError,
        > {
            panic!("CLI turn called HTTP")
        }
        async fn list_models(&self) -> Result<Vec<String>, provider_catalog::ProviderError> {
            Ok(vec![])
        }
    }
}
