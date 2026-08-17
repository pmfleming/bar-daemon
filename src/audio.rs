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
    sources: Rc<RefCell<HashMap<u32, SinkProbe>>>,
    default_sink_name: Rc<RefCell<String>>,
    default_source_name: Rc<RefCell<String>>,
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
        let (sink, _) = probe_default()?;
        let volume = (sink.volume + f32::from(delta_percent) / 100.0).clamp(0.0, 1.0);
        set_node(&sink, Some(volume), Some(false), "sink")?;
        probe()
    })
    .await
    .context("join PipeWire volume operation")?
}

pub async fn set_muted(muted: Option<bool>) -> Result<AudioState> {
    tokio::task::spawn_blocking(move || {
        let (sink, _) = probe_default()?;
        set_node(&sink, None, Some(muted.unwrap_or(!sink.muted)), "sink")?;
        probe()
    })
    .await
    .context("join PipeWire mute operation")?
}

pub async fn set_input_muted(muted: Option<bool>) -> Result<AudioState> {
    tokio::task::spawn_blocking(move || {
        let (_, source) = probe_default()?;
        let source = source.context("no PipeWire audio source is available")?;
        set_node(
            &source,
            None,
            Some(muted.unwrap_or(!source.muted)),
            "source",
        )?;
        probe()
    })
    .await
    .context("join PipeWire input mute operation")?
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
        ObjectType::Node => global.props.is_some_and(|props| {
            matches!(
                props.get("media.class"),
                Some("Audio/Sink" | "Audio/Source")
            )
        }),
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
    let (sink, source) = probe_default()?;
    Ok(AudioState {
        available: true,
        sink_name: sink.name,
        sink_description: sink.description,
        volume_percent: (sink.volume * 100.0).round().clamp(0.0, 100.0) as u8,
        muted: sink.muted,
        input_available: source.is_some(),
        source_name: source
            .as_ref()
            .map(|source| source.name.clone())
            .unwrap_or_default(),
        source_description: source
            .as_ref()
            .map(|source| source.description.clone())
            .unwrap_or_default(),
        input_muted: source.as_ref().is_some_and(|source| source.muted),
        error: None,
    })
}

fn probe_default() -> Result<(SinkProbe, Option<SinkProbe>)> {
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
    let sources = state.sources.borrow();
    let default_sink_name = state.default_sink_name.borrow();
    let default_source_name = state.default_source_name.borrow();
    let sink = preferred_node(&sinks, &default_sink_name)
        .context("no PipeWire audio sink is available")?;
    let source = preferred_node(&sources, &default_source_name);
    Ok((sink, source))
}

fn preferred_node(nodes: &HashMap<u32, SinkProbe>, default_name: &str) -> Option<SinkProbe> {
    nodes
        .values()
        .find(|node| !default_name.is_empty() && node.name == default_name)
        .or_else(|| nodes.values().next())
        .cloned()
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
        ObjectType::Node => bind_probe_node(state, registry, global, props),
        ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
            bind_default_metadata(state, registry, global);
        }
        _ => {}
    }
}

fn bind_probe_node(
    state: &Rc<ProbeState>,
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
    props: &pw::spa::utils::dict::DictRef,
) {
    let (nodes, fallback_description) = match props.get("media.class") {
        Some("Audio/Sink") => (Rc::clone(&state.sinks), "Audio output"),
        Some("Audio/Source") => (Rc::clone(&state.sources), "Audio input"),
        _ => return,
    };
    let Ok(node) = registry.bind::<Node, _>(global) else {
        return;
    };
    nodes.borrow_mut().insert(
        global.id,
        SinkProbe {
            id: global.id,
            name: props.get("node.name").unwrap_or_default().to_string(),
            description: props
                .get("node.description")
                .or_else(|| props.get("node.nick"))
                .unwrap_or(fallback_description)
                .to_string(),
            channels: 2,
            volume: 0.0,
            muted: false,
        },
    );
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
            if let Some(values) = parse_props(pod)
                && let Some(audio_node) = nodes.borrow_mut().get_mut(&id)
            {
                apply_props(audio_node, values);
            }
        })
        .register();
    node.enum_params(1, Some(pw::spa::param::ParamType::Props), 0, 1);
    state.objects.borrow_mut().retain(global.id, node, listener);
}

fn apply_props(node: &mut SinkProbe, values: PropsValues) {
    if let Some(volume) = values.volume {
        node.volume = volume;
    }
    if let Some(channels) = values.channels {
        node.channels = channels;
    }
    if let Some(muted) = values.muted {
        node.muted = muted;
    }
}

fn bind_default_metadata(
    state: &Rc<ProbeState>,
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
) {
    let Ok(metadata) = registry.bind::<Metadata, _>(global) else {
        return;
    };
    let default_sink_name = Rc::clone(&state.default_sink_name);
    let default_source_name = Rc::clone(&state.default_source_name);
    let listener = metadata
        .add_listener_local()
        .property(move |_, key, _, value| {
            let name = value.and_then(default_node_name).unwrap_or_default();
            match key {
                Some("default.audio.sink") => *default_sink_name.borrow_mut() = name,
                Some("default.audio.source") => *default_source_name.borrow_mut() = name,
                _ => {}
            }
            0
        })
        .register();
    state
        .objects
        .borrow_mut()
        .retain(global.id, metadata, listener);
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

fn set_node(
    node_probe: &SinkProbe,
    volume: Option<f32>,
    muted: Option<bool>,
    node_kind: &str,
) -> Result<()> {
    use pw::spa::pod::{Object, Property, Value, ValueArray, serialize::PodSerializer};
    initialize();
    let mut properties = Vec::new();
    if let Some(volume) = volume {
        properties.push(Property::new(
            pw::spa::sys::SPA_PROP_channelVolumes,
            Value::ValueArray(ValueArray::Float(vec![
                linear_to_raw(volume);
                node_probe.channels.max(1)
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
    let requested_id = node_probe.id;
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
        bail!("default PipeWire {node_kind} disappeared");
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
    use std::collections::HashMap;

    use super::{SinkProbe, default_node_name, linear_to_raw, preferred_node, raw_to_linear};

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

    #[test]
    fn selects_the_default_pipewire_node() {
        let nodes = HashMap::from([
            (
                1,
                SinkProbe {
                    name: "fallback".into(),
                    ..SinkProbe::default()
                },
            ),
            (
                2,
                SinkProbe {
                    name: "preferred".into(),
                    muted: true,
                    ..SinkProbe::default()
                },
            ),
        ]);
        let selected = preferred_node(&nodes, "preferred").unwrap();
        assert_eq!(selected.name, "preferred");
        assert!(selected.muted);
    }
}
