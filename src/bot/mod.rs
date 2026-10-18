//! Bot dispatch.
//!
//! [`spawn`] gives the terminal dashboard a cancellable, observable runtime for
//! [`copy_trading`].

pub mod copy_trading;

use crate::config::AppConfig;
use crate::operator::OperatorBus;
use crate::service::position_store::PositionStore;
use anyhow::Result;
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum BotKind {
    CopyTrading,
}

impl BotKind {
    pub fn label(self) -> &'static str {
        match self {
            BotKind::CopyTrading => "Copy Trading",
        }
    }
}

pub async fn run(kind: BotKind, cfg: AppConfig) -> Result<()> {
    let bus = OperatorBus::new();
    let positions = PositionStore::new();
    let (stop, shutdown) = watch::channel(false);
    let mut task = tokio::spawn(run_with_operator(kind, cfg, bus, shutdown, positions));
    tokio::select! {
        result = &mut task => Ok(result??),
        _ = tokio::signal::ctrl_c() => {
            let _ = stop.send(true);
            task.await??;
            Ok(())
        }
    }
}

pub struct BotRuntime {
    pub kind: BotKind,
    pub bus: OperatorBus,
    cfg: AppConfig,
    positions: std::sync::Arc<PositionStore>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<Result<()>>>,
}

impl BotRuntime {
    pub fn is_finished(&self) -> bool {
        self.task
            .as_ref()
            .map(JoinHandle::is_finished)
            .unwrap_or(true)
    }

    pub fn is_running(&self) -> bool {
        self.task
            .as_ref()
            .map(|task| !task.is_finished())
            .unwrap_or(false)
    }

    pub fn request_stop(&self) {
        let _ = self.stop.send(true);
    }

    pub async fn wait(&mut self) -> Result<()> {
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.await??;
        Ok(())
    }

    pub fn resume(&mut self) -> bool {
        if self.task.is_some() {
            return false;
        }
        let (stop, shutdown) = watch::channel(false);
        self.stop = stop;
        self.task = Some(tokio::spawn(run_with_operator(
            self.kind,
            self.cfg.clone(),
            self.bus.clone(),
            shutdown,
            std::sync::Arc::clone(&self.positions),
        )));
        true
    }
}

pub fn spawn(kind: BotKind, cfg: AppConfig) -> BotRuntime {
    let bus = OperatorBus::new();
    let positions = PositionStore::new();
    let (stop, shutdown) = watch::channel(false);
    let task_bus = bus.clone();
    let task = tokio::spawn(run_with_operator(
        kind,
        cfg.clone(),
        task_bus,
        shutdown,
        std::sync::Arc::clone(&positions),
    ));
    BotRuntime {
        kind,
        bus,
        cfg,
        positions,
        stop,
        task: Some(task),
    }
}

async fn run_with_operator(
    kind: BotKind,
    cfg: AppConfig,
    bus: OperatorBus,
    shutdown: watch::Receiver<bool>,
    positions: std::sync::Arc<PositionStore>,
) -> Result<()> {
    match kind {
        BotKind::CopyTrading => {
            copy_trading::run_with_operator_and_positions(cfg, bus, shutdown, positions).await
        }
    }
}
