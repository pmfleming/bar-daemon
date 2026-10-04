//! Serialize control and publication on one PipeWire-owning thread.
use tokio::sync::{mpsc, oneshot};

use super::AudioConnection;
use crate::{model::AudioState, state::StateStore};

#[derive(Clone, Copy)]
pub(crate) enum AudioCommand {
    Adjust(i16),
    SetMuted(Option<bool>),
    SetInputMuted(Option<bool>),
}

type AudioReply = Option<oneshot::Sender<Result<AudioState, String>>>;

pub(crate) struct AudioController {
    sender: mpsc::Sender<(AudioCommand, AudioReply)>,
}

impl AudioController {
    pub(crate) fn new(state: StateStore) -> Self {
        let (sender, receiver) = mpsc::channel(64);
        let runtime = tokio::runtime::Handle::current();
        if let Err(error) = std::thread::Builder::new()
            .name("bar-pipewire-control".into())
            .spawn(move || run(receiver, state, runtime))
        {
            tracing::error!(%error, "could not start PipeWire controller");
        }
        Self { sender }
    }

    pub(crate) async fn execute(&self, command: AudioCommand) -> Result<AudioState, String> {
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

fn run(
    mut receiver: mpsc::Receiver<(AudioCommand, AudioReply)>,
    state: StateStore,
    runtime: tokio::runtime::Handle,
) {
    let mut connection = AudioConnection::new().ok();
    while let Some(first) = receiver.blocking_recv() {
        // Start the first key immediately. Coalesce only requests already
        // queued (including repeats received during the previous transaction).
        // All PipeWire objects stay on this thread and reuse one connection.
        let mut pending = vec![first];
        while let Ok(command) = receiver.try_recv() {
            pending.push(command);
        }
        let mut remaining = pending.as_mut_slice();
        while let Some((first, _)) = remaining.first() {
            let (count, command) = batch(remaining, *first);
            let (commands, rest) = remaining.split_at_mut(count);
            let result = execute_audio(&mut connection, command);
            runtime.block_on(publish_audio_result(&state, commands, result));
            remaining = rest;
        }
    }
}

fn execute_audio(
    connection: &mut Option<AudioConnection>,
    command: AudioCommand,
) -> Result<AudioState, String> {
    let result = (|| {
        let connection = match &mut *connection {
            Some(connection) => connection,
            empty => empty.insert(AudioConnection::new()?),
        };
        match command {
            AudioCommand::Adjust(delta) => connection.adjust(delta),
            AudioCommand::SetMuted(muted) => connection.set_muted(muted),
            AudioCommand::SetInputMuted(muted) => connection.set_input_muted(muted),
        }
    })();
    if result.is_err() {
        // Reconnect for the NEXT request, never replay a possibly applied
        // adjustment/toggle after a disconnect or verification failure.
        *connection = None;
    }
    result.map_err(|error: anyhow::Error| error.to_string())
}

fn batch(commands: &[(AudioCommand, AudioReply)], first: AudioCommand) -> (usize, AudioCommand) {
    let AudioCommand::Adjust(first) = first else {
        return (1, first);
    };
    let mut count = 0;
    let mut delta = 0_i32;
    for (command, _) in commands {
        let AudioCommand::Adjust(value) = command else {
            break;
        };
        // Opposite directions must retain their clamp ordering (e.g. at 100%).
        if value.signum() != first.signum() {
            break;
        }
        delta = delta.saturating_add(i32::from(*value));
        count += 1;
    }
    (
        count,
        AudioCommand::Adjust(delta.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16),
    )
}

async fn publish_audio_result(
    state: &StateStore,
    commands: &mut [(AudioCommand, AudioReply)],
    result: Result<AudioState, String>,
) {
    if let Ok(audio) = &result {
        state.update_audio(audio.clone()).await;
    }
    for (_, reply) in commands {
        if let Some(reply) = reply.take() {
            let _ = reply.send(result.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AudioCommand, batch, publish_audio_result};
    use crate::{model::AudioState, state::StateStore};
    use tokio::sync::oneshot;

    #[test]
    fn coalesces_queued_repeats_without_reordering_mutes_or_direction_changes() {
        let commands = [
            (AudioCommand::Adjust(5), None),
            (AudioCommand::Adjust(5), None),
            (AudioCommand::Adjust(-5), None),
            (AudioCommand::SetMuted(None), None),
            (AudioCommand::Adjust(-5), None),
            (AudioCommand::Adjust(-5), None),
        ];
        for (start, count, delta) in [(0, 2, 10), (2, 1, -5), (4, 2, -10)] {
            assert!(matches!(batch(&commands[start..], commands[start].0),
                (n, AudioCommand::Adjust(d)) if n == count && d == delta));
        }
        assert!(matches!(
            batch(&commands[3..], commands[3].0),
            (1, AudioCommand::SetMuted(None))
        ));
        let commands = [
            (AudioCommand::Adjust(i16::MAX), None),
            (AudioCommand::Adjust(i16::MAX), None),
        ];
        assert!(matches!(
            batch(&commands, commands[0].0),
            (2, AudioCommand::Adjust(i16::MAX))
        ));
    }

    #[tokio::test]
    async fn coalesced_requests_all_receive_the_confirmed_state() {
        let state = StateStore::default();
        let (first, first_reply) = oneshot::channel();
        let (second, second_reply) = oneshot::channel();
        let mut commands = [
            (AudioCommand::Adjust(5), Some(first)),
            (AudioCommand::Adjust(5), Some(second)),
        ];
        let audio = AudioState {
            available: true,
            volume_percent: 60,
            ..AudioState::default()
        };
        publish_audio_result(&state, &mut commands, Ok(audio.clone())).await;
        assert_eq!(first_reply.await.unwrap().unwrap(), audio);
        assert_eq!(second_reply.await.unwrap().unwrap(), audio);
        assert_eq!(state.snapshot().await.audio, audio);
    }
}
