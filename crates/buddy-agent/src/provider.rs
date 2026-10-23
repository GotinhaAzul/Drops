//! Thin adapter between the engine's event stream and `buddy_ai::complete`.
//!
//! Text and reasoning fragments are forwarded to the UI as they arrive. Tool
//! calls are executed only from the final assistant message, so the partial
//! `ProviderEvent::ToolCall` values are not forwarded as `tool_start` events
//! (that would announce calls before their arguments are complete).

use anyhow::Result;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::events;

#[derive(Default)]
pub(crate) struct StreamStats {
    pub saw_text: bool,
    pub saw_thinking: bool,
}

pub(crate) async fn complete(
    config: &buddy_ai::ProviderConfig,
    messages: &[Value],
    tools: &[Value],
    events_tx: &mpsc::UnboundedSender<Value>,
    cancel: &CancellationToken,
) -> Result<(Value, StreamStats)> {
    #[cfg(test)]
    if let Some(result) = test_support::invoke(messages, tools) {
        return result.map(|message| (message, StreamStats::default()));
    }

    let (provider_tx, mut provider_rx) = mpsc::unbounded_channel();
    let forward = events_tx.clone();
    let forwarder = tokio::spawn(async move {
        let mut stats = StreamStats::default();
        while let Some(event) = provider_rx.recv().await {
            match event {
                buddy_ai::ProviderEvent::Text(text) => {
                    stats.saw_text = true;
                    events::text(&forward, text);
                }
                buddy_ai::ProviderEvent::Thinking(text) => {
                    stats.saw_thinking = true;
                    events::thinking(&forward, text);
                }
                // Final tool calls are consumed from the returned message; usage
                // is currently not part of the engine event contract.
                buddy_ai::ProviderEvent::ToolCall(_) | buddy_ai::ProviderEvent::Usage(_) => {}
            }
        }
        stats
    });

    let result = buddy_ai::complete(config, messages, tools, provider_tx, cancel.clone()).await;
    let stats = forwarder.await.unwrap_or_default();
    Ok((result?, stats))
}

/// Test-only provider injection.
///
/// `run` currently has no public stream-function parameter (the contract fixes
/// its signature), so the engine's end-to-end tests install a thread-local
/// responder here. Production builds compile this module out entirely.
#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;

    use anyhow::Result;
    use serde_json::Value;

    type MockFn = Box<dyn FnMut(&[Value], &[Value]) -> Result<Value>>;

    thread_local! {
        static MOCK: RefCell<Option<MockFn>> = RefCell::new(None);
    }

    pub(crate) struct MockGuard;

    impl Drop for MockGuard {
        fn drop(&mut self) {
            MOCK.with(|mock| *mock.borrow_mut() = None);
        }
    }

    pub(crate) fn install(
        responder: impl FnMut(&[Value], &[Value]) -> Result<Value> + 'static,
    ) -> MockGuard {
        MOCK.with(|mock| *mock.borrow_mut() = Some(Box::new(responder)));
        MockGuard
    }

    pub(crate) fn invoke(messages: &[Value], tools: &[Value]) -> Option<Result<Value>> {
        MOCK.with(|mock| {
            let mut mock = mock.borrow_mut();
            mock.as_mut().map(|responder| responder(messages, tools))
        })
    }
}
