//! Messages between this app's tabs in one browser, over a
//! `BroadcastChannel`.
//!
//! Every tab is its own Automerge peer on the same IndexedDB. After writing,
//! a tab says what it wrote; the others read it from IndexedDB. Messages
//! carry no document data, only where to find it, so IndexedDB stays the one
//! source of truth and a message that arrives late (or for something already
//! gone) is harmless.

use std::fmt;

use supp_shared::DocId;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use web_sys::{BroadcastChannel, MessageEvent};

const CHANNEL: &str = "supp";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabMessage {
    /// A change chunk was stored for a list.
    Chunk(DocId, String),
    /// A list was added to this device: created, or added from the server.
    Added(DocId),
    /// A list was removed from this device.
    Removed(DocId),
    /// The stored account changed, or was confirmed: a user ID, or `None`
    /// after signing out (which erases every list).
    Account(Option<i64>),
}

impl fmt::Display for TabMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Chunk(id, chunk) => write!(f, "chunk {id} {chunk}"),
            Self::Added(id) => write!(f, "added {id}"),
            Self::Removed(id) => write!(f, "removed {id}"),
            Self::Account(Some(user)) => write!(f, "account {user}"),
            Self::Account(None) => write!(f, "account"),
        }
    }
}

impl TabMessage {
    /// `None` for anything unrecognised, e.g. from a tab running another
    /// build of the app.
    fn parse(s: &str) -> Option<Self> {
        let mut words = s.split(' ');
        let message = match (words.next()?, words.next(), words.next()) {
            ("chunk", Some(id), Some(chunk)) => Self::Chunk(id.parse().ok()?, chunk.to_string()),
            ("added", Some(id), None) => Self::Added(id.parse().ok()?),
            ("removed", Some(id), None) => Self::Removed(id.parse().ok()?),
            ("account", Some(user), None) => Self::Account(Some(user.parse().ok()?)),
            ("account", None, None) => Self::Account(None),
            _ => return None,
        };
        words.next().is_none().then_some(message)
    }
}

/// This tab's end of the channel. A tab never hears its own messages.
pub struct Tabs {
    /// `None` where the browser has no `BroadcastChannel`; tabs then only
    /// see each other's changes after a reload.
    channel: Option<BroadcastChannel>,
    _on_message: Option<Closure<dyn FnMut(MessageEvent)>>,
}

impl Tabs {
    pub fn open(mut on_message: impl FnMut(TabMessage) + 'static) -> Self {
        let Ok(channel) = BroadcastChannel::new(CHANNEL) else {
            leptos::logging::warn!("no BroadcastChannel: tabs won't see each other's changes");
            return Self {
                channel: None,
                _on_message: None,
            };
        };
        let callback = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            if let Some(message) = event
                .data()
                .as_string()
                .as_deref()
                .and_then(TabMessage::parse)
            {
                on_message(message);
            }
        });
        channel.set_onmessage(Some(callback.as_ref().unchecked_ref()));
        Self {
            channel: Some(channel),
            _on_message: Some(callback),
        }
    }

    pub fn post(&self, message: &TabMessage) {
        if let Some(channel) = &self.channel
            && let Err(err) = channel.post_message(&JsValue::from_str(&message.to_string()))
        {
            leptos::logging::warn!("telling other tabs {message}: {err:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let id = DocId::new();
        for message in [
            TabMessage::Chunk(id, uuid::Uuid::new_v4().to_string()),
            TabMessage::Added(id),
            TabMessage::Removed(id),
            TabMessage::Account(Some(42)),
            TabMessage::Account(None),
        ] {
            assert_eq!(TabMessage::parse(&message.to_string()), Some(message));
        }
        let extra = format!("removed {id} extra");
        for junk in ["", "chunk", "added nope", "account x", &extra, "hello"] {
            assert_eq!(TabMessage::parse(junk), None, "{junk}");
        }
    }
}
