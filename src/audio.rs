use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    io::Cursor,
    rc::Rc,
    sync::{Arc, Once},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use pipewire as pw;
use pw::{
    metadata::Metadata,
    node::Node,
    proxy::{Listener, ProxyT},
    types::ObjectType,
};
use tokio::{sync::mpsc, time::sleep};

use crate::{model::AudioState, state::StateStore};

type RetainedObject = (Box<dyn ProxyT>, Box<dyn Listener>);
type ChangeCallback = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Objects(HashMap<u32, RetainedObject>);

impl Objects {
    fn retain(&mut self, id: u32, proxy: impl ProxyT + 'static, listener: impl Listener + 'static) {
        self.0.insert(id, (Box::new(proxy), Box::new(listener)));
    }
    fn remove(&mut self, id: u32) {
        self.0.remove(&id);
    }
}

#[derive(Debug, Clone, Default)]
struct SinkProbe {
    id: u32,
    name: String,
    description: String,
    channels: usize,
    volume: f32,
    muted: bool,
}

#[derive(Default)]
struct ProbeState {
    sinks: Rc<RefCell<HashMap<u32, SinkProbe>>>,
    default_name: Rc<RefCell<String>>,
    objects: Rc<RefCell<Objects>>,
}

pub async fn monitor(store: StateStore) {
    let (changes_tx, mut changes_rx) = mpsc::channel::<()>(8);
    std::thread::Builder::new()
        .name("bar-pipewire-monitor".into())
        .spawn(move || {
            let callback: ChangeCallback = Arc::new(move || {
                let _ = changes_tx.blocking_send(());
            });
            if let Err(error) = monitor_pipewire(callback) {
                tracing::warn!(%error, "PipeWire monitor ended");
            }
        })
        .ok();

    loop {
        refresh(&store).await;
        tokio::select! {
            value = changes_rx.recv() => if value.is_none() { sleep(Duration::from_secs(2)).await; },
            _ = sleep(Duration::from_secs(30)) => {}
        }
    }
}

async fn refresh(store: &StateStore) {
    match tokio::task::spawn_blocking(probe).await {
        Ok(Ok(value)) => store.update_audio(value).await,
        Ok(Err(error)) => {
            store
                .update_audio(AudioState {
                    error: Some(error.to_string()),
                    ..AudioState::default()
                })
                .await
        }
        Err(error) => {
            store
                .update_audio(AudioState {
                    error: Some(error.to_string()),
                    ..AudioState::default()
                })
                .await
        }
    }
}

pub async fn adjust(delta_percent: i16) -> Result<AudioState> {
    tokio::task::spawn_blocking(move || {
        let sink = probe_default()?;
        let volume = (sink.volume + f32::from(delta_percent) / 100.0).clamp(0.0, 1.0);
        set_sink(&sink, Some(volume), Some(false))?;
        probe()
    })
    .await
    .context("join PipeWire volume operation")?
}

pub async fn set_muted(muted: Option<bool>) -> Result<AudioState> {
    tokio::task::spawn_blocking(move || {
        let sink = probe_default()?;
        set_sink(&sink, None, Some(muted.unwrap_or(!sink.muted)))?;
        probe()
    })
    .await
    .context("join PipeWire mute operation")?
}

fn initialize() {
    static INITIALIZE: Once = Once::new();
    INITIALIZE.call_once(pw::init);
}

fn monitor_pipewire(on_change: ChangeCallback) -> Result<()> {
    initialize();
    let main_loop = pw::main_loop::MainLoopRc::new(None).context("create PipeWire monitor loop")?;
    let context =
        pw::context::ContextRc::new(&main_loop, None).context("create PipeWire monitor context")?;
    let core = context
        .connect_rc(None)
        .context("connect PipeWire monitor")?;
    let registry = core.get_registry_rc().context("open PipeWire registry")?;
    let registry_weak = registry.downgrade();
    let objects = Rc::new(RefCell::new(Objects::default()));
    let objects_for_add = Rc::clone(&objects);
    let objects_for_remove = Rc::clone(&objects);
    let relevant = Rc::new(RefCell::new(Vec::<u32>::new()));
    let relevant_add = Rc::clone(&relevant);
    let relevant_remove = Rc::clone(&relevant);
    let add_change = Arc::clone(&on_change);
    let remove_change = Arc::clone(&on_change);
    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            if !relevant_global(global) {
                return;
            }
            relevant_add.borrow_mut().push(global.id);
            add_change();
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            if let Err(error) =
                bind_monitor_object(&registry, global, &objects_for_add, Arc::clone(&add_change))
            {
                tracing::debug!(id = global.id, %error, "could not bind PipeWire monitor object");
            }
        })
        .global_remove(move |id| {
            if let Some(index) = relevant_remove
                .borrow()
                .iter()
                .position(|value| *value == id)
            {
                relevant_remove.borrow_mut().remove(index);
                objects_for_remove.borrow_mut().remove(id);
                remove_change();
            }
        })
        .register();
    main_loop.run();
    bail!("PipeWire monitor loop ended")
}

fn relevant_global(global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>) -> bool {
    match global.type_ {
        ObjectType::Node => global
            .props
            .is_some_and(|props| props.get("media.class") == Some("Audio/Sink")),
        ObjectType::Metadata => global
            .props
            .is_some_and(|props| props.get("metadata.name") == Some("default")),
        _ => false,
    }
}

fn bind_monitor_object(
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    objects: &Rc<RefCell<Objects>>,
    event: ChangeCallback,
) -> Result<()> {
    match global.type_ {
        ObjectType::Node => {
            let node = registry.bind::<Node, _>(global)?;
            let listener = node
                .add_listener_local()
                .info({
                    let event = Arc::clone(&event);
                    move |_| event()
                })
                .param(move |_, _, _, _, _| event())
                .register();
            objects.borrow_mut().retain(global.id, node, listener);
        }
        ObjectType::Metadata => {
            let metadata = registry.bind::<Metadata, _>(global)?;
            let listener = metadata
                .add_listener_local()
                .property(move |_, _, _, _| {
                    event();
                    0
                })
                .register();
            objects.borrow_mut().retain(global.id, metadata, listener);
        }
        _ => {}
    }
    Ok(())
}

fn probe() -> Result<AudioState> {
    let sink = probe_default()?;
    Ok(AudioState {
        available: true,
        sink_name: sink.name,
        sink_description: sink.description,
        volume_percent: (sink.volume * 100.0).round().clamp(0.0, 100.0) as u8,
        muted: sink.muted,
        error: None,
    })
}

fn probe_default() -> Result<SinkProbe> {
    initialize();
    let main_loop = pw::main_loop::MainLoopRc::new(None).context("create PipeWire main loop")?;
    let context =
        pw::context::ContextRc::new(&main_loop, None).context("create PipeWire context")?;
    let core = context.connect_rc(None).context("connect to PipeWire")?;
    let registry = core.get_registry_rc().context("open PipeWire registry")?;
    let registry_weak = registry.downgrade();
    let state = Rc::new(ProbeState::default());
    let state_for_registry = Rc::clone(&state);
    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            bind_probe_global(&state_for_registry, &registry, global);
        })
        .register();
    pipewire_roundtrip(&main_loop, &core)?;
    pipewire_roundtrip(&main_loop, &core)?;
    let sinks = state.sinks.borrow();
    let default_name = state.default_name.borrow();
    sinks
        .values()
        .find(|sink| !default_name.is_empty() && sink.name == *default_name)
        .or_else(|| sinks.values().next())
        .cloned()
        .context("no PipeWire audio sink is available")
}

fn bind_probe_global(
    state: &Rc<ProbeState>,
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
) {
    let Some(props) = global.props else {
        return;
    };
    match global.type_ {
        ObjectType::Node if props.get("media.class") == Some("Audio/Sink") => {
            let Ok(node) = registry.bind::<Node, _>(global) else {
                return;
            };
            state.sinks.borrow_mut().insert(
                global.id,
                SinkProbe {
                    id: global.id,
                    name: props.get("node.name").unwrap_or_default().to_string(),
                    description: props
                        .get("node.description")
                        .or_else(|| props.get("node.nick"))
                        .unwrap_or("Audio output")
                        .to_string(),
                    channels: 2,
                    volume: 0.0,
                    muted: false,
                },
            );
            let sinks = Rc::clone(&state.sinks);
            let id = global.id;
            let listener = node
                .add_listener_local()
                .param(move |_, parameter, _, _, pod| {
                    if parameter != pw::spa::param::ParamType::Props {
                        return;
                    }
                    let Some(pod) = pod else {
                        return;
                    };
                    if let Some(values) = parse_props(pod) {
                        if let Some(sink) = sinks.borrow_mut().get_mut(&id) {
                            if let Some(volume) = values.volume {
                                sink.volume = volume;
                            }
                            if let Some(channels) = values.channels {
                                sink.channels = channels;
                            }
                            if let Some(muted) = values.muted {
                                sink.muted = muted;
                            }
                        }
                    }
                })
                .register();
            node.enum_params(1, Some(pw::spa::param::ParamType::Props), 0, 1);
            state.objects.borrow_mut().retain(global.id, node, listener);
        }
        ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
            let Ok(metadata) = registry.bind::<Metadata, _>(global) else {
                return;
            };
            let default_name = Rc::clone(&state.default_name);
            let listener = metadata
                .add_listener_local()
                .property(move |_, key, _, value| {
                    if key == Some("default.audio.sink") {
                        *default_name.borrow_mut() =
                            value.and_then(default_node_name).unwrap_or_default();
                    }
                    0
                })
                .register();
            state
                .objects
                .borrow_mut()
                .retain(global.id, metadata, listener);
        }
        _ => {}
    }
}

#[derive(Default)]
struct PropsValues {
    volume: Option<f32>,
    channels: Option<usize>,
    muted: Option<bool>,
}

fn parse_props(pod: &pw::spa::pod::Pod) -> Option<PropsValues> {
    use pw::spa::pod::{Value, ValueArray, deserialize::PodDeserializer};
    let (_, Value::Object(object)) =
        PodDeserializer::deserialize_from::<Value>(pod.as_bytes()).ok()?
    else {
        return None;
    };
    let mut values = PropsValues::default();
    for property in object.properties {
        match property.value {
            Value::Bool(value) if property.key == pw::spa::sys::SPA_PROP_mute => {
                values.muted = Some(value)
            }
            Value::Float(value) if property.key == pw::spa::sys::SPA_PROP_volume => {
                values.volume = Some(raw_to_linear(value))
            }
            Value::ValueArray(ValueArray::Float(volumes))
                if property.key == pw::spa::sys::SPA_PROP_channelVolumes =>
            {
                values.channels = Some(volumes.len().max(1));
                if !volumes.is_empty() {
                    values.volume = Some(raw_to_linear(
                        volumes.iter().sum::<f32>() / volumes.len() as f32,
                    ));
                }
            }
            _ => {}
        }
    }
    Some(values)
}

fn set_sink(sink: &SinkProbe, volume: Option<f32>, muted: Option<bool>) -> Result<()> {
    use pw::spa::pod::{Object, Property, Value, ValueArray, serialize::PodSerializer};
    initialize();
    let mut properties = Vec::new();
    if let Some(volume) = volume {
        properties.push(Property::new(
            pw::spa::sys::SPA_PROP_channelVolumes,
            Value::ValueArray(ValueArray::Float(vec![
                linear_to_raw(volume);
                sink.channels.max(1)
            ])),
        ));
    }
    if let Some(muted) = muted {
        properties.push(Property::new(
            pw::spa::sys::SPA_PROP_mute,
            Value::Bool(muted),
        ));
    }
    let value = Value::Object(Object {
        type_: pw::spa::sys::SPA_TYPE_OBJECT_Props,
        id: pw::spa::sys::SPA_PARAM_Props,
        properties,
    });
    let bytes = PodSerializer::serialize(Cursor::new(Vec::new()), &value)?
        .0
        .into_inner();
    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let applied = Rc::new(Cell::new(false));
    let applied_for_listener = Rc::clone(&applied);
    let requested_id = sink.id;
    let registry_weak = registry.downgrade();
    let retained = Rc::new(RefCell::new(None::<Node>));
    let retained_for_listener = Rc::clone(&retained);
    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.id != requested_id || global.type_ != ObjectType::Node {
                return;
            }
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            if let Ok(node) = registry.bind::<Node, _>(global) {
                let Some(pod) = pw::spa::pod::Pod::from_bytes(&bytes) else {
                    return;
                };
                node.set_param(pw::spa::param::ParamType::Props, 0, pod);
                *retained_for_listener.borrow_mut() = Some(node);
                applied_for_listener.set(true);
            }
        })
        .register();
    pipewire_roundtrip(&main_loop, &core)?;
    if !applied.get() {
        bail!("default PipeWire sink disappeared");
    }
    Ok(())
}

fn default_node_name(value: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(value).ok()?["name"]
        .as_str()
        .map(str::to_string)
}

fn raw_to_linear(value: f32) -> f32 {
    value.max(0.0).cbrt()
}
fn linear_to_raw(value: f32) -> f32 {
    value.max(0.0).powi(3)
}

fn pipewire_roundtrip(
    main_loop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
) -> Result<()> {
    let pending = core.sync(0)?;
    let done = Rc::new(Cell::new(false));
    let done_listener = Rc::clone(&done);
    let loop_listener = main_loop.clone();
    let _listener = core
        .add_listener_local()
        .done(move |id, sequence| {
            if id == pw::core::PW_ID_CORE && sequence == pending {
                done_listener.set(true);
                loop_listener.quit();
            }
        })
        .register();
    let timed_out = Rc::new(Cell::new(false));
    let timed_out_timer = Rc::clone(&timed_out);
    let loop_timer = main_loop.clone();
    let timer = main_loop.loop_().add_timer(move |_| {
        timed_out_timer.set(true);
        loop_timer.quit();
    });
    timer
        .update_timer(Some(Duration::from_secs(3)), None)
        .into_result()?;
    while !done.get() && !timed_out.get() {
        main_loop.run();
    }
    if timed_out.get() {
        bail!("PipeWire synchronization timed out");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{default_node_name, linear_to_raw, raw_to_linear};

    #[test]
    fn converts_pipewire_cubic_volume() {
        assert!((raw_to_linear(linear_to_raw(0.5)) - 0.5).abs() < 0.001);
    }

    #[test]
    fn parses_default_metadata() {
        assert_eq!(
            default_node_name(r#"{"name":"alsa_output.test"}"#).as_deref(),
            Some("alsa_output.test")
        );
        assert!(default_node_name("invalid").is_none());
    }
}
