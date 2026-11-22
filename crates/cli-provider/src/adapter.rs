//! Adapter contract for registered coding CLIs. Static vendor data stays in the
//! registry; process isolation and protocol decoding stay in the shared runtime.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{
    ApprovalLevel, CliEngineSpec, DetectedCli, Detector, Driver, LevelPlan, PathEnv,
    models::ModelList,
    runtime::{CliError, NativeTurn, Turn, TurnHost, TurnUsage},
};

/// Behaviour every native-tools engine must provide. Implementations must use
/// the shared launcher: no ambient credentials, workspace executables, shell
/// interpolation, or wider approval grants. Streamed text is untrusted data;
/// protocol metadata and stderr must never become user-facing errors.
#[async_trait]
pub trait EngineAdapter: Send + Sync {
    fn spec(&self) -> &'static CliEngineSpec;
    fn level_plan(&self, level: ApprovalLevel) -> LevelPlan;

    /// Bounded detection includes version-floor and authentication evidence.
    async fn detect(&self, detector: &Detector) -> Option<DetectedCli> {
        detector
            .detect(false)
            .await
            .into_iter()
            .find(|engine| engine.id == self.spec().id)
    }

    async fn models(&self, detector: &Detector) -> Option<ModelList> {
        detector.fetch_models(self.spec().id).await
    }

    /// Run a session and stream through `host`. Cancelling or dropping the
    /// future (including an external timeout) must terminate the process tree.
    ///
    /// # Errors
    /// Stable, sanitized [`CliError`] values; never raw child diagnostics.
    #[allow(clippy::too_many_arguments)] // Keep launch policy explicit at the adapter boundary.
    async fn run_turn(
        &self,
        command: &str,
        env: &PathEnv,
        turn: Turn,
        workspace: PathBuf,
        level: ApprovalLevel,
        host: Arc<dyn TurnHost>,
        cancel: CancellationToken,
    ) -> Result<TurnUsage, CliError>;
}

#[async_trait]
impl EngineAdapter for Driver {
    fn spec(&self) -> &'static CliEngineSpec {
        crate::engine_spec(self.id()).expect("every driver has a registry descriptor")
    }

    fn level_plan(&self, level: ApprovalLevel) -> LevelPlan {
        crate::level_plan(*self, level)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_turn(
        &self,
        command: &str,
        env: &PathEnv,
        turn: Turn,
        workspace: PathBuf,
        level: ApprovalLevel,
        host: Arc<dyn TurnHost>,
        cancel: CancellationToken,
    ) -> Result<TurnUsage, CliError> {
        crate::runtime::run_adapted_turn(
            command,
            env,
            NativeTurn {
                turn,
                driver: *self,
                level,
                workspace,
            },
            host,
            cancel,
        )
        .await
    }
}
