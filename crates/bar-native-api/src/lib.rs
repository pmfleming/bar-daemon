//! Pure interfaces for native providers. No hardware or native library dependency.
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::{mpsc, oneshot, watch};
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AudioState {
    pub available: bool,
    pub sink_name: String,
    pub sink_description: String,
    pub volume_percent: u8,
    pub muted: bool,
    pub input_available: bool,
    pub source_name: String,
    pub source_description: String,
    pub input_muted: bool,
    pub error: Option<String>,
}

#[derive(Clone, Copy)]
pub enum AudioCommand {
    Adjust(i16),
    SetMuted(Option<bool>),
    SetInputMuted(Option<bool>),
}
pub type AudioReply = Option<oneshot::Sender<Result<AudioState, String>>>;
pub type Work<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
pub trait AudioPublisher: Send + Sync {
    fn update_audio(&self, value: AudioState) -> Work<'_>;
}
pub trait AudioProvider: Send + Sync {
    fn control(&self, publisher: Arc<dyn AudioPublisher>) -> AudioControl;
    fn monitor(&self, publisher: Arc<dyn AudioPublisher>) -> Work<'static>;
}
pub struct AudioControl {
    sender: mpsc::Sender<(AudioCommand, AudioReply)>,
}
impl AudioControl {
    pub fn from_sender(sender: mpsc::Sender<(AudioCommand, AudioReply)>) -> Self {
        Self { sender }
    }
    pub async fn execute(&self, command: AudioCommand) -> Result<AudioState, String> {
        let (reply, response) = oneshot::channel();
        self.sender
            .send((command, Some(reply)))
            .await
            .map_err(|_| "audio controller stopped".to_string())?;
        response
            .await
            .map_err(|_| "audio controller stopped before replying".to_string())?
    }
}
#[derive(Default, Clone)]
pub struct NativeProviders {
    pub audio: Option<Arc<dyn AudioProvider>>,
    pub sharing: Option<fn() -> watch::Receiver<Option<bool>>>,
    pub power_events: Option<fn() -> mpsc::Receiver<()>>,
}
