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
    device::Device,
    metadata::Metadata,
    node::Node,
    proxy::{Listener, ProxyT},
    types::ObjectType,
};
use tokio::{sync::mpsc, time::sleep};

use crate::{model::AudioState, state::StateStore};

pub(crate) mod controller;

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
    device_id: Option<u32>,
    route_device: Option<i32>,
    route: Option<RouteProbe>,
}

#[derive(Debug, Clone, Default)]
struct RouteProbe {
    device_id: u32,
    index: i32,
    route_device: i32,
    direction: u32,
    channels: usize,
    volume: f32,
    muted: bool,
}

#[derive(Default)]
struct ProbeState {
    sinks: Rc<RefCell<HashMap<u32, SinkProbe>>>,
    sources: Rc<RefCell<HashMap<u32, SinkProbe>>>,
    routes: Rc<RefCell<Vec<RouteProbe>>>,
    default_sink_name: Rc<RefCell<String>>,
    default_source_name: Rc<RefCell<String>>,
    objects: Rc<RefCell<Objects>>,
}

pub(crate) async fn monitor(store: StateStore) {
    let (changes_tx, mut changes_rx) = mpsc::channel::<()>(8);
    std::thread::Builder::new()
        .name("bar-pipewire-monitor".into())
        .spawn(move || {
            let callback: ChangeCallback = Arc::new(move || {
                // A PipeWire transition emits several registry notifications.
                // One queued wake-up is enough to rebuild the complete snapshot.
                let _ = changes_tx.try_send(());
            });
            if let Err(error) = monitor_pipewire(callback) {
                tracing::warn!(%error, "PipeWire monitor ended");
            }
        })
        .ok();

    refresh(&store).await;
    loop {
        tokio::select! {
            value = changes_rx.recv() => {
                if value.is_some() {
                    sleep(Duration::from_millis(75)).await;
                    while changes_rx.try_recv().is_ok() {}
                } else {
                    sleep(Duration::from_secs(2)).await;
                }
            },
            _ = sleep(Duration::from_secs(30)) => {}
        }
        refresh(&store).await;
    }
}

async fn refresh(store: &StateStore) {
    let result = tokio::task::spawn_blocking(probe)
        .await
        .unwrap_or_else(|error| Err(error.into()));
    store
        .update_audio(result.unwrap_or_else(|error| AudioState {
            error: Some(error.to_string()),
            ..AudioState::default()
        }))
        .await;
}

/// Owned by the dedicated control thread: PipeWire objects are not Send.
/// Reuse the connection across keys, but query current defaults/routes for each
/// operation so device switches and changes by other clients remain authoritative.
struct AudioConnection {
    core: pw::core::CoreRc,
    main_loop: pw::main_loop::MainLoopRc,
}

impl AudioConnection {
    fn new() -> Result<Self> {
        initialize();
        let main_loop =
            pw::main_loop::MainLoopRc::new(None).context("create PipeWire control loop")?;
        let context = pw::context::ContextRc::new(&main_loop, None)
            .context("create PipeWire control context")?;
        let core = context.connect_rc(None).context("connect to PipeWire")?;
        // Warm the connection before the first key press.
        pipewire_roundtrip(&main_loop, &core)?;
        Ok(Self { core, main_loop })
    }

    fn adjust(&self, delta_percent: i16) -> Result<AudioState> {
        let (sink, _) = probe_default(self)?;
        let volume = adjusted_volume(sink.volume, delta_percent);
        set_node(self, &sink, Some(volume), Some(false), "sink")?;
        self.snapshot()
    }

    fn set_muted(&self, muted: Option<bool>) -> Result<AudioState> {
        let (sink, _) = probe_default(self)?;
        set_node(
            self,
            &sink,
            None,
            Some(requested_mute(sink.muted, muted)),
            "sink",
        )?;
        self.snapshot()
    }

    fn set_input_muted(&self, muted: Option<bool>) -> Result<AudioState> {
        let (_, source) = probe_default(self)?;
        let source = source.context("no PipeWire audio source is available")?;
        set_node(
            self,
            &source,
            None,
            Some(requested_mute(source.muted, muted)),
            "source",
        )?;
        self.snapshot()
    }

    fn snapshot(&self) -> Result<AudioState> {
        let (sink, source) = probe_default(self)?;
        Ok(audio_state(sink, source))
    }
}

fn adjusted_volume(current: f32, delta_percent: i16) -> f32 {
    (current + f32::from(delta_percent) / 100.0).clamp(0.0, 1.0)
}

fn requested_mute(current: bool, requested: Option<bool>) -> bool {
    requested.unwrap_or(!current)
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
            if remove_relevant_id(&relevant_remove, id) {
                objects_for_remove.borrow_mut().remove(id);
                remove_change();
            }
        })
        .register();
    main_loop.run();
    bail!("PipeWire monitor loop ended")
}

fn remove_relevant_id(relevant: &RefCell<Vec<u32>>, id: u32) -> bool {
    let index = {
        let relevant = relevant.borrow();
        relevant.iter().position(|value| *value == id)
    };
    let Some(index) = index else { return false };
    relevant.borrow_mut().remove(index);
    true
}

fn relevant_global(global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>) -> bool {
    match global.type_ {
        ObjectType::Node => global.props.is_some_and(|props| {
            matches!(
                props.get("media.class"),
                Some("Audio/Sink" | "Audio/Source")
            )
        }),
        ObjectType::Device => global
            .props
            .is_some_and(|props| props.get("media.class") == Some("Audio/Device")),
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
        ObjectType::Device => {
            let device = registry.bind::<Device, _>(global)?;
            device.subscribe_params(&[pw::spa::param::ParamType::Route]);
            let listener = device
                .add_listener_local()
                .param(move |_, parameter, _, _, _| {
                    if parameter == pw::spa::param::ParamType::Route {
                        event();
                    }
                })
                .register();
            objects.borrow_mut().retain(global.id, device, listener);
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
    AudioConnection::new()?.snapshot()
}

fn audio_state(sink: SinkProbe, source: Option<SinkProbe>) -> AudioState {
    AudioState {
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
    }
}

fn probe_default(connection: &AudioConnection) -> Result<(SinkProbe, Option<SinkProbe>)> {
    let main_loop = &connection.main_loop;
    let core = &connection.core;
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
    pipewire_roundtrip(main_loop, core)?;
    pipewire_roundtrip(main_loop, core)?;
    let sinks = state.sinks.borrow();
    let sources = state.sources.borrow();
    let default_sink_name = state.default_sink_name.borrow();
    let default_source_name = state.default_source_name.borrow();
    let mut sink = preferred_node(&sinks, &default_sink_name)
        .context("no PipeWire audio sink is available")?;
    let mut source = preferred_node(&sources, &default_source_name);
    let routes = state.routes.borrow();
    apply_route(&mut sink, &routes, pw::spa::sys::SPA_DIRECTION_OUTPUT);
    if let Some(source) = &mut source {
        apply_route(source, &routes, pw::spa::sys::SPA_DIRECTION_INPUT);
    }
    Ok((sink, source))
}

fn apply_route(node: &mut SinkProbe, routes: &[RouteProbe], direction: u32) {
    let candidates = routes
        .iter()
        .filter(|route| Some(route.device_id) == node.device_id && route.direction == direction)
        .collect::<Vec<_>>();
    let route = node
        .route_device
        .and_then(|device| {
            candidates
                .iter()
                .find(|route| route.route_device == device)
                .copied()
        })
        .or_else(|| (candidates.len() == 1).then(|| candidates[0]));
    let Some(route) = route else { return };
    node.channels = route.channels;
    node.volume = route.volume;
    node.muted = route.muted;
    node.route = Some(route.clone());
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
        ObjectType::Device if props.get("media.class") == Some("Audio/Device") => {
            bind_probe_device(state, registry, global)
        }
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
            device_id: props.get("device.id").and_then(|value| value.parse().ok()),
            route_device: props
                .get("card.profile.device")
                .and_then(|value| value.parse().ok()),
            route: None,
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

fn bind_probe_device(
    state: &Rc<ProbeState>,
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>,
) {
    let Ok(device) = registry.bind::<Device, _>(global) else {
        return;
    };
    let routes = Rc::clone(&state.routes);
    let device_id = global.id;
    let listener = device
        .add_listener_local()
        .param(move |_, parameter, _, _, pod| {
            if parameter != pw::spa::param::ParamType::Route {
                return;
            }
            let Some(pod) = pod else {
                return;
            };
            if let Some(mut route) = parse_route(pod) {
                route.device_id = device_id;
                let mut routes = routes.borrow_mut();
                if let Some(existing) = routes.iter_mut().find(|existing| {
                    existing.device_id == device_id && existing.index == route.index
                }) {
                    *existing = route;
                } else {
                    routes.push(route);
                }
            }
        })
        .register();
    device.enum_params(1, Some(pw::spa::param::ParamType::Route), 0, u32::MAX);
    state
        .objects
        .borrow_mut()
        .retain(global.id, device, listener);
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

fn pod_object(pod: &pw::spa::pod::Pod) -> Option<pw::spa::pod::Object> {
    use pw::spa::pod::{Value, deserialize::PodDeserializer};
    let (_, Value::Object(object)) =
        PodDeserializer::deserialize_from::<Value>(pod.as_bytes()).ok()?
    else {
        return None;
    };
    Some(object)
}

fn parse_props(pod: &pw::spa::pod::Pod) -> Option<PropsValues> {
    pod_object(pod).map(parse_props_object)
}

fn parse_props_object(object: pw::spa::pod::Object) -> PropsValues {
    use pw::spa::pod::{Value, ValueArray};
    let mut values = PropsValues::default();
    for property in object.properties {
        match (property.key, property.value) {
            (pw::spa::sys::SPA_PROP_mute, Value::Bool(value)) => values.muted = Some(value),
            (pw::spa::sys::SPA_PROP_volume, Value::Float(value)) => {
                values.volume = Some(raw_to_linear(value))
            }
            (
                pw::spa::sys::SPA_PROP_channelVolumes,
                Value::ValueArray(ValueArray::Float(volumes)),
            ) => {
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
    values
}

fn parse_route(pod: &pw::spa::pod::Pod) -> Option<RouteProbe> {
    use pw::spa::pod::Value;
    let object = pod_object(pod)?;
    let mut index = None;
    let mut route_device = None;
    let mut direction = None;
    let mut values = None;
    for property in object.properties {
        match (property.key, property.value) {
            (pw::spa::sys::SPA_PARAM_ROUTE_index, Value::Int(value)) => index = Some(value),
            (pw::spa::sys::SPA_PARAM_ROUTE_device, Value::Int(value)) => route_device = Some(value),
            (pw::spa::sys::SPA_PARAM_ROUTE_direction, Value::Id(value)) => {
                direction = Some(value.0)
            }
            (pw::spa::sys::SPA_PARAM_ROUTE_props, Value::Object(value)) => {
                values = Some(parse_props_object(value))
            }
            _ => {}
        }
    }
    let values = values?;
    Some(RouteProbe {
        index: index?,
        route_device: route_device?,
        direction: direction?,
        channels: values.channels.unwrap_or(1),
        volume: values.volume.unwrap_or(0.0),
        muted: values.muted.unwrap_or(false),
        ..RouteProbe::default()
    })
}

fn set_node(
    connection: &AudioConnection,
    node_probe: &SinkProbe,
    volume: Option<f32>,
    muted: Option<bool>,
    node_kind: &str,
) -> Result<()> {
    use pw::spa::pod::{Object, Property, Value, ValueArray};
    if let Some(route) = &node_probe.route {
        return set_route(connection, route, volume, muted, node_kind);
    }
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
    set_parameter(
        connection,
        node_probe.id,
        value,
        node_kind,
        |node: &Node, pod| {
            node.set_param(pw::spa::param::ParamType::Props, 0, pod);
        },
    )
}

// Node and device-route writes share proxy lifetime, acknowledgement, and
// disappearance handling. Retain the bound object until the second roundtrip.
fn set_parameter<P: ProxyT + 'static>(
    connection: &AudioConnection,
    requested_id: u32,
    value: pw::spa::pod::Value,
    node_kind: &str,
    apply: impl Fn(&P, &pw::spa::pod::Pod) + 'static,
) -> Result<()> {
    use pw::spa::pod::serialize::PodSerializer;
    let bytes = PodSerializer::serialize(Cursor::new(Vec::new()), &value)?
        .0
        .into_inner();
    let main_loop = &connection.main_loop;
    let core = &connection.core;
    let registry = core.get_registry_rc()?;
    let applied = Rc::new(Cell::new(false));
    let applied_for_listener = Rc::clone(&applied);
    let registry_weak = registry.downgrade();
    let retained = Rc::new(RefCell::new(None::<P>));
    let retained_for_listener = Rc::clone(&retained);
    let _listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.id != requested_id || global.type_ != P::type_() {
                return;
            }
            let Some(registry) = registry_weak.upgrade() else {
                return;
            };
            if let Ok(proxy) = registry.bind::<P, _>(global) {
                let Some(pod) = pw::spa::pod::Pod::from_bytes(&bytes) else {
                    return;
                };
                apply(&proxy, pod);
                *retained_for_listener.borrow_mut() = Some(proxy);
                applied_for_listener.set(true);
            }
        })
        .register();
    pipewire_roundtrip(main_loop, core)?;
    if !applied.get() {
        bail!("default PipeWire {node_kind} disappeared");
    }
    // set_param is issued inside the registry callback, after the first sync.
    // Flush/acknowledge it before dropping the proxy and verifying the result.
    pipewire_roundtrip(main_loop, core)
}

fn set_route(
    connection: &AudioConnection,
    route: &RouteProbe,
    volume: Option<f32>,
    muted: Option<bool>,
    node_kind: &str,
) -> Result<()> {
    use pw::spa::pod::{Object, Property, Value, ValueArray};
    let volume = linear_to_raw(volume.unwrap_or(route.volume));
    let muted = muted.unwrap_or(route.muted);
    let props = Value::Object(Object {
        type_: pw::spa::sys::SPA_TYPE_OBJECT_Props,
        id: pw::spa::sys::SPA_PARAM_Props,
        properties: vec![
            Property::new(
                pw::spa::sys::SPA_PROP_channelVolumes,
                Value::ValueArray(ValueArray::Float(vec![volume; route.channels.max(1)])),
            ),
            Property::new(pw::spa::sys::SPA_PROP_mute, Value::Bool(muted)),
        ],
    });
    let value = Value::Object(Object {
        type_: pw::spa::sys::SPA_TYPE_OBJECT_ParamRoute,
        id: pw::spa::sys::SPA_PARAM_Route,
        properties: vec![
            Property::new(pw::spa::sys::SPA_PARAM_ROUTE_index, Value::Int(route.index)),
            Property::new(
                pw::spa::sys::SPA_PARAM_ROUTE_device,
                Value::Int(route.route_device),
            ),
            Property::new(pw::spa::sys::SPA_PARAM_ROUTE_props, props),
            Property::new(pw::spa::sys::SPA_PARAM_ROUTE_save, Value::Bool(true)),
        ],
    });
    set_parameter(
        connection,
        route.device_id,
        value,
        &format!("{node_kind} route"),
        |device: &Device, pod| {
            device.set_param(pw::spa::param::ParamType::Route, 0, pod);
        },
    )
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
    let failure = Rc::new(RefCell::new(None));
    let failure_listener = Rc::clone(&failure);
    let failure_loop = main_loop.clone();
    let _listener = core
        .add_listener_local()
        .done(move |id, sequence| {
            if id == pw::core::PW_ID_CORE && sequence == pending {
                done_listener.set(true);
                loop_listener.quit();
            }
        })
        .error(move |id, _, result, message| {
            *failure_listener.borrow_mut() =
                Some(format!("PipeWire object {id} failed ({result}): {message}"));
            failure_loop.quit();
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
    while !done.get() && !timed_out.get() && failure.borrow().is_none() {
        main_loop.run();
    }
    if let Some(error) = failure.borrow_mut().take() {
        bail!(error);
    }
    if timed_out.get() {
        bail!("PipeWire synchronization timed out");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use pipewire as pw;

    use super::{RouteProbe, SinkProbe, apply_route, preferred_node};

    #[test]
    fn pod_decoding_preserves_types_defaults_and_channel_volume_precedence() {
        use pw::spa::{
            pod::{Object, Property, Value, ValueArray, serialize::PodSerializer},
            sys,
        };
        let mut props = Object {
            type_: sys::SPA_TYPE_OBJECT_Props,
            id: sys::SPA_PARAM_Props,
            properties: vec![
                Property::new(sys::SPA_PROP_mute, Value::Int(1)), // wrong type is ignored
                Property::new(sys::SPA_PROP_volume, Value::Float(0.125)),
                Property::new(
                    sys::SPA_PROP_channelVolumes,
                    Value::ValueArray(ValueArray::Float(vec![])),
                ),
            ],
        };
        let values = super::parse_props_object(props.clone());
        assert_eq!(
            (values.volume, values.channels, values.muted),
            (Some(0.5), Some(1), None)
        );
        props.properties.push(Property::new(
            sys::SPA_PROP_channelVolumes,
            Value::ValueArray(ValueArray::Float(vec![1.0, 1.0])),
        ));
        let values = super::parse_props_object(props.clone());
        assert_eq!((values.volume, values.channels), (Some(1.0), Some(2)));
        let decode = |value: &Value| {
            let bytes = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), value)
                .unwrap()
                .0
                .into_inner();
            super::parse_route(pw::spa::pod::Pod::from_bytes(&bytes).unwrap())
        };
        let mut route = Object {
            type_: sys::SPA_TYPE_OBJECT_ParamRoute,
            id: sys::SPA_PARAM_Route,
            properties: vec![
                Property::new(sys::SPA_PARAM_ROUTE_index, Value::Int(7)),
                Property::new(sys::SPA_PARAM_ROUTE_device, Value::Int(2)),
                Property::new(
                    sys::SPA_PARAM_ROUTE_direction,
                    Value::Id(pw::spa::utils::Id(sys::SPA_DIRECTION_OUTPUT)),
                ),
                Property::new(sys::SPA_PARAM_ROUTE_props, Value::Object(props)),
            ],
        };
        let parsed = decode(&Value::Object(route.clone())).unwrap();
        assert_eq!(
            (
                parsed.index,
                parsed.route_device,
                parsed.channels,
                parsed.volume,
                parsed.muted
            ),
            (7, 2, 2, 1.0, false)
        );
        route.properties[0].value = Value::Bool(true);
        assert!(decode(&Value::Object(route)).is_none());
        assert!(decode(&Value::Bool(true)).is_none());
    }

    #[test]
    fn applies_the_only_matching_hardware_route() {
        let mut node = SinkProbe {
            device_id: Some(42),
            volume: 1.0,
            ..SinkProbe::default()
        };
        let routes = [RouteProbe {
            device_id: 42,
            index: 1,
            route_device: 1,
            direction: pw::spa::sys::SPA_DIRECTION_OUTPUT,
            channels: 2,
            volume: 0.64,
            muted: true,
        }];

        apply_route(&mut node, &routes, pw::spa::sys::SPA_DIRECTION_OUTPUT);

        assert_eq!(node.volume, 0.64);
        assert_eq!(node.channels, 2);
        assert!(node.muted);
        assert_eq!(node.route.as_ref().map(|route| route.index), Some(1));
    }

    /// Never connects to the user's server or changes physical audio devices.
    #[test]
    #[ignore = "requires pipewire executable; uses an isolated null-audio server"]
    fn private_pipewire_control_reuses_connection_and_reads_external_changes() {
        use super::{AudioConnection, initialize, pipewire_roundtrip};
        use std::{
            os::unix::net::UnixStream,
            process::{Child, Command, Stdio},
            time::{Duration, Instant},
        };

        struct Server(Child);
        impl Drop for Server {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let root = tempfile::tempdir().unwrap();
        let mut server = Server(
            Command::new("pipewire")
                .args([
                    "-c",
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/test_support/pipewire-osd.conf"
                    ),
                ])
                .env("PIPEWIRE_RUNTIME_DIR", root.path())
                .env("XDG_RUNTIME_DIR", root.path())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let socket = root.path().join("pipewire-osd-test");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            assert!(
                server.0.try_wait().unwrap().is_none(),
                "private server exited"
            );
            assert!(
                Instant::now() < deadline,
                "private server did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let connect = || {
            initialize();
            let main_loop = pw::main_loop::MainLoopRc::new(None).unwrap();
            let context = pw::context::ContextRc::new(&main_loop, None).unwrap();
            // An explicit fd prevents environment configuration from selecting
            // the user's live PipeWire socket, even when tests run in parallel.
            let socket = UnixStream::connect(&socket).unwrap();
            let core = context.connect_fd_rc(socket.into(), None).unwrap();
            pipewire_roundtrip(&main_loop, &core).unwrap();
            AudioConnection { core, main_loop }
        };
        let connection = connect();
        let external = connect();
        // Both generic transaction targets must reject vanished objects rather
        // than reporting a write that never happened.
        let missing = super::SinkProbe {
            id: u32::MAX,
            ..Default::default()
        };
        assert!(super::set_node(&connection, &missing, None, Some(true), "sink").is_err());
        let missing = super::RouteProbe {
            device_id: u32::MAX,
            ..Default::default()
        };
        assert!(super::set_route(&connection, &missing, None, Some(true), "sink").is_err());
        assert_eq!(connection.snapshot().unwrap().sink_name, "osd-test-output");
        assert!(connection.set_muted(Some(true)).unwrap().muted);
        assert!(!connection.set_muted(None).unwrap().muted);
        assert!(connection.set_input_muted(Some(true)).unwrap().input_muted);
        assert!(!connection.set_input_muted(None).unwrap().input_muted);
        assert_eq!(connection.adjust(-100).unwrap().volume_percent, 0);
        assert_eq!(connection.adjust(30).unwrap().volume_percent, 30);
        assert_eq!(external.adjust(10).unwrap().volume_percent, 40);
        assert_eq!(connection.adjust(5).unwrap().volume_percent, 45);
        let mut timings = Vec::new();
        for _ in 0..20 {
            let started = Instant::now();
            assert_eq!(
                connection.adjust(1).unwrap().volume_percent,
                external.snapshot().unwrap().volume_percent
            );
            timings.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        timings.sort_by(f64::total_cmp);
        eprintln!(
            "persistent control + independent verification: median={:.2} ms p95={:.2} ms",
            timings[10], timings[18]
        );
        server.0.kill().unwrap();
        server.0.wait().unwrap();
        assert!(
            connection.adjust(5).is_err(),
            "disconnect must fail, not report success"
        );
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
