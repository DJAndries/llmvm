use std::{error::Error, future::Future, pin::Pin, task::Poll};

use llmvm_protocol::tower::Service;
use tokio::sync::{mpsc, oneshot};

use crate::{jsonrpc::JsonRpcRequest, lsp::LspMessage};

#[derive(Clone, Debug)]
pub struct LspMessageInfo {
    pub message: LspMessage,
    pub to_real_server: bool,
    pub origin_request: Option<JsonRpcRequest>,
}

pub type LspMessageTrx = (LspMessageInfo, oneshot::Sender<Option<LspMessage>>);

pub struct LspMessageService {
    tx: mpsc::UnboundedSender<LspMessageTrx>,
}

impl LspMessageService {
    pub fn new(tx: mpsc::UnboundedSender<LspMessageTrx>) -> Self {
        Self { tx }
    }
}

impl Service<LspMessageInfo> for LspMessageService {
    type Response = Option<LspMessage>;
    type Error = Box<dyn Error + Send + Sync + 'static>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: LspMessageInfo) -> Self::Future {
        let tx = self.tx.clone();
        Box::pin(async move {
            let (resp_tx, resp_rx) = oneshot::channel();
            tx.send((req, resp_tx))?;
            Ok(resp_rx.await?)
        })
    }
}
