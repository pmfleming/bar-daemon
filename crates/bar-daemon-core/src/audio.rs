//! Provider bridge: policy/publication live here, PipeWire lives in its own crate.
use crate::{model::AudioState, state::StateStore};
use std::sync::Arc;
pub(crate) mod controller {
    pub(crate) use bar_native_api::{AudioCommand, AudioControl as AudioController};
}
struct Publisher(StateStore);
impl bar_native_api::AudioPublisher for Publisher {
    fn update_audio(&self, value: AudioState) -> bar_native_api::Work<'_> {
        Box::pin(self.0.update_audio(value))
    }
}
pub(crate) fn control(state: StateStore) -> Option<bar_native_api::AudioControl> {
    let provider = state.native.audio.clone()?;
    Some(provider.control(Arc::new(Publisher(state))))
}
pub(crate) async fn monitor(state: StateStore) {
    if let Some(provider) = state.native.audio.clone() {
        provider.monitor(Arc::new(Publisher(state))).await;
    } else {
        std::future::pending::<()>().await;
    }
}
