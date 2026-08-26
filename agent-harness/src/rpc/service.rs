use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::agent::ApprovalChoice;

use super::approval;
use super::observer;
use super::params::*;
use super::protocol::{PROTOCOL_VERSION, RpcError};
use super::state::RpcState;
use super::writer::Outbound;

#[derive(Clone)]
pub struct Service {
    initialized: Arc<AtomicBool>,
    state: RpcState,
    outbound: Outbound,
}

impl Service {
    pub fn new(outbound: Outbound) -> Self {
        Self {
            initialized: Arc::new(AtomicBool::new(false)),
            state: RpcState::default(),
            outbound,
        }
    }

    pub fn state(&self) -> &RpcState {
        &self.state
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized.load(Ordering::Acquire)
    }

    pub async fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if method == "rafikx.initialize" {
            return self.initialize(params);
        }
        if !self.initialized.load(Ordering::Acquire) {
            return Err(RpcError::not_initialized());
        }
        match method {
            "session.open" => self.open_session(params).await,
            "session.get" => self.get_session(params).await,
            "turn.run" => self.run_turn(params).await,
            "run.status" => self.run_status(params),
            "run.cancel" => self.cancel_run(params),
            "approval.resolve" => self.resolve_approval(params),
            _ => Err(RpcError::method_not_found(method)),
        }
    }

    fn initialize(&self, params: Value) -> Result<Value, RpcError> {
        let params: InitializeParams = decode_default(params)?;
        if params
            .protocol_version
            .as_deref()
            .is_some_and(|version| version != PROTOCOL_VERSION)
        {
            return Err(RpcError::invalid_params(format!(
                "unsupported protocol version; expected {PROTOCOL_VERSION}"
            )));
        }
        self.initialized.store(true, Ordering::Release);
        Ok(json!({
            "server": {"name": "RafikX", "version": env!("CARGO_PKG_VERSION")},
            "protocol_version": PROTOCOL_VERSION,
            "client_name": params.client_name,
            "capabilities": {
                "sessions": true,
                "lifecycle_events": true,
                "cooperative_cancellation": true,
                "approval_broker": true,
                "max_frame_bytes": super::protocol::MAX_FRAME_BYTES
            }
        }))
    }

    async fn open_session(&self, params: Value) -> Result<Value, RpcError> {
        let params: SessionOpenParams = decode_default(params)?;
        let session = crate::api::open_chat(
            params.provider,
            params.model,
            params.class,
            params.resume.clone(),
            params.yes,
        )
        .map_err(server_error)?;
        let id = params
            .resume
            .unwrap_or_else(|| format!("draft-{}", crate::db::Db::new_id()));
        let transcript = crate::api::transcript(&session);
        self.state
            .sessions
            .lock()
            .await
            .insert(id.clone(), Arc::new(tokio::sync::Mutex::new(session)));
        Ok(json!({"session_id": id, "messages": transcript}))
    }

    async fn get_session(&self, params: Value) -> Result<Value, RpcError> {
        let params: SessionParams = decode(params)?;
        let session = self.session(&params.session_id).await?;
        let session = session.lock().await;
        Ok(json!({
            "session_id": params.session_id,
            "persisted_session_id": session.session_id,
            "mode": session.mode,
            "class": session.class,
            "messages": crate::api::transcript(&session)
        }))
    }

    async fn run_turn(&self, params: Value) -> Result<Value, RpcError> {
        let params: TurnParams = decode(params)?;
        if params.prompt.trim().is_empty() {
            return Err(RpcError::invalid_params("prompt must not be empty"));
        }
        let session = self.session(&params.session_id).await?;
        let mut session = session.lock().await;
        if let Some(class) = params.class {
            session.class = (!class.trim().is_empty()).then_some(class);
        }
        if let Some(mode) = params.mode {
            session.mode = if mode.eq_ignore_ascii_case("plan") {
                "plan".into()
            } else {
                "build".into()
            };
        }
        let ask = approval::broker(self.state.clone(), self.outbound.clone());
        let observer = observer::create(self.state.clone(), self.outbound.clone());
        let result = crate::api::run_turn_observed(
            &mut session,
            params.prompt.trim(),
            params.obsidian,
            Some(ask),
            Some(observer),
        )
        .await
        .map_err(server_error)?;
        serde_json::to_value(result).map_err(server_error)
    }

    fn run_status(&self, params: Value) -> Result<Value, RpcError> {
        let params: RunParams = decode(params)?;
        let runs = self.state.runs.lock().map_err(server_error)?;
        let run = runs
            .get(&params.run_id)
            .ok_or_else(|| RpcError::server("run not found"))?;
        Ok(json!({
            "run_id": params.run_id,
            "state": run.lifecycle_state(),
            "terminal_state": run.terminal_state().map(|state| format!("{state:?}").to_ascii_lowercase()),
            "cancelled": run.is_cancelled(),
            "lifecycle": run.lifecycle_events(),
            "context_sources": run.context_sources(),
            "persistence_error": run.lifecycle_persistence_error()
        }))
    }

    fn cancel_run(&self, params: Value) -> Result<Value, RpcError> {
        let params: RunParams = decode(params)?;
        let runs = self.state.runs.lock().map_err(server_error)?;
        let run = runs
            .get(&params.run_id)
            .ok_or_else(|| RpcError::server("run not found"))?;
        Ok(
            json!({"run_id": params.run_id, "requested": run.cancel("rpc client requested cancellation")}),
        )
    }

    fn resolve_approval(&self, params: Value) -> Result<Value, RpcError> {
        let params: ApprovalParams = decode(params)?;
        let decision = match params.decision.as_str() {
            "yes" => ApprovalChoice::Yes,
            "always" => ApprovalChoice::Always,
            "no" => ApprovalChoice::No,
            _ => {
                return Err(RpcError::invalid_params(
                    "decision must be yes, no, or always",
                ));
            }
        };
        let sender = self
            .state
            .approvals
            .lock()
            .map_err(server_error)?
            .remove(&params.approval_id)
            .ok_or_else(|| RpcError::server("approval not found or expired"))?;
        sender
            .send(decision)
            .map_err(|_| RpcError::server("approval receiver is closed"))?;
        Ok(json!({"approval_id": params.approval_id, "resolved": true}))
    }

    async fn session(
        &self,
        id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<crate::chat::Session>>, RpcError> {
        self.state
            .sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::server("session not found"))
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, RpcError> {
    serde_json::from_value(value).map_err(|error| RpcError::invalid_params(error.to_string()))
}

fn decode_default<T: DeserializeOwned + Default>(value: Value) -> Result<T, RpcError> {
    if value.is_null() {
        Ok(T::default())
    } else {
        decode(value)
    }
}

fn server_error(error: impl std::fmt::Display) -> RpcError {
    RpcError::server(error.to_string())
}
