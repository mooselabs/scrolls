//! Standalone read snapshots and backend diagnostics.
use super::Store;
use crate::{
    beads::Local,
    cli::Cli,
    read::{BackendFailure, Snapshot, SnapshotSource},
};
use anyhow::Context;
use serde_json::json;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

#[derive(Clone)]
pub struct Config {
    pub store: PathBuf,
    pub dolt: PathBuf,
    pub connection: Option<PathBuf>,
    pub branch: String,
    pub actor: String,
}
impl Config {
    pub fn from_cli(cli: &Cli) -> Self {
        Self {
            store: cli.store.clone(),
            dolt: cli.dolt.clone(),
            connection: cli.connection.clone(),
            branch: cli.proposal.clone().unwrap_or_else(|| "main".into()),
            actor: cli.actor.clone().unwrap_or_else(|| "reader".into()),
        }
    }
}

/// Opens a fresh immutable read view while retaining its lock or shared session.
pub struct DoltSource(pub Config);
impl DoltSource {
    fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let config = &self.0;
        let store = if let Some(path) = &config.connection {
            Store::open_shared(path, &config.store)?
        } else {
            Store::open_readonly_wait(&config.store, &config.dolt, Some(Duration::from_secs(5)))?
        };
        if store.is_shared() {
            crate::storage::shared_review::check_readable(&store)?;
        }
        anyhow::ensure!(
            config.branch == "main" || config.branch.starts_with("proposal/"),
            "unknown selected proposal"
        );
        anyhow::ensure!(
            store.branches()?.contains(&config.branch),
            "unknown selected proposal"
        );
        let revision = store.head(&config.branch)?;
        let format = store.one_at::<u32>("metadata", "schema", &revision)?;
        anyhow::ensure!(
            format == Some(if store.is_shared() { 5 } else { 4 }),
            "unsupported store format; explicit migration required"
        );
        if store.is_shared() {
            store.check_versioned_inventory(&revision)?;
        }
        let store_id: String = store
            .one_at("metadata", "store_id", &revision)?
            .context("missing store identity")?;
        uuid::Uuid::parse_str(&store_id)?;
        let graph = store.graph_at(&revision)?;
        graph.schema.validate()?;
        let local = if store.is_shared() {
            Local::default()
        } else {
            Local::load(&store.root)?
        };
        let routing = store
            .one_at("metadata", "routing", &revision)?
            .unwrap_or(json!({}));
        Ok(Snapshot {
            store: Box::new(store),
            graph,
            work: Box::new(local),
            routing,
            store_id,
            selection: config.branch.clone(),
            revision,
            actor: config.actor.clone(),
        })
    }
    fn classify_error(error: anyhow::Error) -> BackendFailure {
        // Existing storage diagnostics are internal only; never echo SQL,
        // credentials, filesystem paths, document text or raw driver errors.
        // Keep typed transport identity even when account names in a driver
        // message happen to resemble a store-state diagnostic.
        if error
            .chain()
            .any(|cause| cause.is::<mysql::Error>() || cause.is::<std::io::Error>())
        {
            return BackendFailure::Unavailable;
        }
        let text = format!("{error:#}");
        if text.contains("read_lock_timeout") || text.contains("publication_lock_timeout") {
            return BackendFailure::Busy;
        }
        if [
            "dirty",
            "recovery_required",
            "unsupported",
            "store format",
            "selected proposal",
            "identity",
            "ontology",
            "schema",
        ]
        .iter()
        .any(|s| text.contains(s))
        {
            BackendFailure::Conflict
        } else {
            BackendFailure::Unavailable
        }
    }
}
impl SnapshotSource for DoltSource {
    fn open(&self) -> Result<Snapshot, BackendFailure> {
        self.snapshot().map_err(Self::classify_error)
    }
    fn classify(&self, error: anyhow::Error) -> BackendFailure {
        Self::classify_error(error)
    }
}

pub fn serve(cli: &Cli, bind: SocketAddr) -> anyhow::Result<()> {
    anyhow::ensure!(
        bind.ip().is_loopback(),
        "serve requires a numeric loopback address"
    );
    let config = Config::from_cli(cli);
    // Validate before binding; the snapshot/session is released immediately.
    let source = Arc::new(DoltSource(config));
    drop(source.snapshot()?);
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(async {
        // Register signals before publishing the startup record.
        #[cfg(unix)]
        let shutdown = {
            use tokio::signal::unix::{SignalKind, signal};
            let mut int = signal(SignalKind::interrupt())?;
            let mut term = signal(SignalKind::terminate())?;
            let mut hup = signal(SignalKind::hangup())?;
            async move { tokio::select! { _ = int.recv() => {}, _ = term.recv() => {}, _ = hup.recv() => {} } }
        };
        #[cfg(not(unix))]
        let shutdown = async { let _ = tokio::signal::ctrl_c().await; };
        let listener = tokio::net::TcpListener::bind(bind).await?;
        let address = listener.local_addr()?;
        let app = crate::http::router(source, address)?;
        if cli.json { println!("{}",json!({"schema_version":2,"ok":true,"event":"listening","address":address.to_string(),"api_version":"v0","read_only":true})); }
        else { println!("Listening on http://{address}"); }
        use std::io::Write;
        std::io::stdout().flush()?;
        axum::serve(listener,app).with_graceful_shutdown(shutdown).await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_backend_failures_do_not_depend_on_account_names() {
        let error = mysql::Error::MySqlError(mysql::MySqlError {
            state: "28000".into(),
            code: 1045,
            message: "Access denied for schema_identity_user".into(),
        });
        let response = DoltSource::classify_error(
            anyhow::Error::new(error).context("shared Dolt connection failed"),
        );
        assert_eq!(response, BackendFailure::Unavailable);
    }
}
