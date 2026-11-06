//! Instradamento automatico giochi/applicazioni (fase 4, doc sez. 12.13, L6). Windows distingue da
//! sé i client spaziali dagli stream normali; qui un flusso d'uscita che non ha scelto un sink va,
//! quando il sink di default è uno dei due del nodo, in quello giusto per il suo numero di canali:
//! più di 2 nel sink dei giochi (bed), fino a 2 nel sink delle applicazioni (oggetti bypass).
//!
//! Come pavucontrol quando l'utente sposta un flusso: `target.object` del flusso nei metadati
//! `default`, che WirePlumber rilegge (linking/find-defined-target). Il flusso resta libero se ha
//! scelto il sink da sé (`target.object` o `node.target` nelle proprietà, per esempio `pw-play
//! --target` o `PULSE_SINK`), se ha `node.dont-move`, o se l'utente lo ha spostato (un
//! `target.object` nei metadati che non è nostro). Se il default smette di essere uno dei due,
//! i flussi spostati da noi tornano a seguirlo.
//!
//! I canali sono quelli del formato negoziato del client (parametro `Format` del suo nodo, prima
//! della conversione verso il sink). Il formato arriva quando il flusso è già collegato al sink di
//! default: lo spostamento avviene subito dopo, e i primi cicli possono suonare nell'altro sink.
//! Le proprietà di un flusso possono arrivare a rate (pipewire-pulse aggiunge `target.object` dopo
//! la creazione del nodo): se una scelta esplicita compare dopo uno spostamento, lo spostamento
//! viene tolto. L'uscita del nodo stesso è esclusa per nome. Tutto sul thread principale.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use pipewire as pw;
use pw::registry::{GlobalObject, RegistryRc};
use pw::spa;
use pw::spa::param::ParamType;
use pw::types::ObjectType;

const GAMES: usize = 0;
const APPS: usize = 1;

struct Stream {
    _node: pw::node::Node,
    _listener: pw::node::NodeListener,
    name: String,
    channels: Option<u32>,
    /// Proprietà del nodo già lette: prima non si decide niente (il formato può arrivare prima).
    props_seen: bool,
    /// Ha scelto il sink da sé (proprietà del nodo), è uno dei flussi del nodo stesso, o
    /// l'utente lo ha spostato (metadati).
    explicit: bool,
    user_moved: bool,
    /// Sink in cui lo abbiamo spostato noi (indice in `Router::sinks`).
    moved: Option<usize>,
}

pub struct Router {
    /// node.name dei due sink del nodo (giochi, applicazioni) e della sua uscita, e il gruppo dei
    /// suoi flussi.
    sinks: [String; 2],
    output: String,
    group: String,
    default_sink: Option<String>,
    metadata: Option<(pw::metadata::Metadata, pw::metadata::MetadataListener)>,
    streams: HashMap<u32, Stream>,
    verbose: bool,
}

impl Router {
    /// Si registra sul registro: segue i flussi d'uscita audio e i metadati `default`.
    pub fn start(registry: &RegistryRc, sinks: [String; 2], output: String, group: String, verbose: bool) -> (Rc<RefCell<Router>>, pw::registry::Listener) {
        let router = Rc::new(RefCell::new(Router { sinks, output, group, default_sink: None, metadata: None, streams: HashMap::new(), verbose }));
        let (r_add, r_rm, reg) = (Rc::downgrade(&router), Rc::downgrade(&router), registry.clone());
        let listener = registry
            .add_listener_local()
            .global(move |g| Router::global(&r_add, &reg, g))
            .global_remove(move |id| {
                if let Some(r) = r_rm.upgrade() {
                    r.borrow_mut().remove(id);
                }
            })
            .register();
        (router, listener)
    }

    fn global(this: &Weak<RefCell<Router>>, registry: &RegistryRc, g: &GlobalObject<&spa::utils::dict::DictRef>) {
        let Some(rc) = this.upgrade() else { return };
        let props = g.props;
        let get = |k: &str| props.and_then(|p| p.get(k));
        match g.type_ {
            ObjectType::Metadata if get("metadata.name") == Some("default") => {
                let Ok(md) = registry.bind::<pw::metadata::Metadata, _>(g) else { return };
                let weak = this.clone();
                let listener = md
                    .add_listener_local()
                    .property(move |subject, key, _type, value| {
                        if let Some(r) = weak.upgrade() {
                            r.borrow_mut().metadata_changed(subject, key, value);
                        }
                        0
                    })
                    .register();
                rc.borrow_mut().metadata = Some((md, listener));
            }
            ObjectType::Node if get("media.class") == Some("Stream/Output/Audio") && get("node.name") != Some(rc.borrow().output.as_str()) => {
                let id = g.id;
                let Ok(node) = registry.bind::<pw::node::Node, _>(g) else { return };
                let (w_info, w_param) = (this.clone(), this.clone());
                let listener = node
                    .add_listener_local()
                    .info(move |info| {
                        // Le proprietà valgono solo negli eventi che le cambiano.
                        if !info.change_mask().contains(pw::node::NodeChangeMask::PROPS) {
                            return;
                        }
                        if let (Some(r), Some(p)) = (w_info.upgrade(), info.props()) {
                            r.borrow_mut().stream_props(id, p);
                        }
                    })
                    .param(move |_seq, kind, _index, _next, pod| {
                        if let (Some(r), Some(pod)) = (w_param.upgrade(), pod) {
                            if matches!(kind, ParamType::Format | ParamType::EnumFormat) {
                                r.borrow_mut().stream_format(id, kind, pod);
                            }
                        }
                    })
                    .register();
                node.subscribe_params(&[ParamType::EnumFormat, ParamType::Format]);
                let name = get("node.name").unwrap_or("?").to_string();
                rc.borrow_mut().streams.insert(
                    id,
                    Stream { _node: node, _listener: listener, name, channels: None, props_seen: false, explicit: false, user_moved: false, moved: None },
                );
            }
            _ => {}
        }
    }

    fn stream_props(&mut self, id: u32, p: &spa::utils::dict::DictRef) {
        let own = p.get("node.link-group") == Some(self.group.as_str()) || p.get("node.group") == Some(self.group.as_str());
        let explicit = own
            || p.get("target.object").is_some()
            || p.get("node.target").is_some()
            || p.get("node.dont-move").is_some_and(|v| v == "true" || v == "1");
        if let Some(s) = self.streams.get_mut(&id) {
            // Una volta esplicito, sempre: le proprietà possono arrivare in più eventi.
            s.explicit |= explicit;
            s.props_seen = true;
        }
        self.evaluate(id);
    }

    fn stream_format(&mut self, id: u32, kind: ParamType, pod: &spa::pod::Pod) {
        let Ok((spa::param::format::MediaType::Audio, spa::param::format::MediaSubtype::Raw)) = spa::param::format_utils::parse_format(pod) else {
            return;
        };
        let mut info = spa::param::audio::AudioInfoRaw::new();
        if info.parse(pod).is_err() || info.channels() == 0 {
            return;
        }
        let Some(s) = self.streams.get_mut(&id) else { return };
        // Solo il formato negoziato: quello offerto può precedere le proprietà definitive.
        if kind == ParamType::Format {
            s.channels = Some(info.channels());
        }
        self.evaluate(id);
    }

    fn metadata_changed(&mut self, subject: u32, key: Option<&str>, value: Option<&str>) {
        match key {
            Some("default.audio.sink") if subject == 0 => {
                // Valore JSON: { "name": "<node.name>" }.
                self.default_sink = value.and_then(|v| v.split('"').nth(3)).map(str::to_string);
                if self.verbose {
                    eprintln!("[instradamento] sink di default: {}", self.default_sink.as_deref().unwrap_or("-"));
                }
                let ids: Vec<u32> = self.streams.keys().copied().collect();
                ids.into_iter().for_each(|id| self.evaluate(id));
            }
            Some("target.object") | Some("target.node") => {
                let ours = self.streams.get(&subject).and_then(|s| s.moved).map(|i| self.sinks[i].as_str());
                if let Some(s) = self.streams.get_mut(&subject) {
                    // L'eco di un nostro spostamento (o della sua cancellazione) non conta.
                    if value.is_some() && value != ours {
                        s.user_moved = true;
                        s.moved = None;
                    }
                }
            }
            None if subject == 0 => self.default_sink = None,
            _ => {}
        }
    }

    fn evaluate(&mut self, id: u32) {
        let default = self.default_sink.as_deref().and_then(|d| self.sinks.iter().position(|s| s == d));
        let Some(s) = self.streams.get(&id) else { return };
        if !s.props_seen || s.user_moved {
            return;
        }
        let wanted = match (default, s.channels) {
            (Some(_), Some(ch)) if !s.explicit => Some(if ch > 2 { GAMES } else { APPS }),
            _ => None,
        };
        // Dove deve andare oltre al default: None = seguire il default (o la scelta del flusso:
        // un nostro spostamento precedente viene tolto).
        let target = wanted.filter(|&w| Some(w) != default);
        if target == s.moved {
            return;
        }
        let Some((md, _)) = &self.metadata else { return };
        let value = target.map(|t| self.sinks[t].clone());
        md.set_property(id, "target.object", value.as_ref().map(|_| "Spa:String"), value.as_deref());
        if self.verbose {
            eprintln!(
                "[instradamento] flusso {id} ({}, {} canali): {}",
                s.name,
                s.channels.map_or("?".into(), |c| c.to_string()),
                value.as_deref().map_or("segue il default".to_string(), |v| format!("→ {v}"))
            );
        }
        if let Some(s) = self.streams.get_mut(&id) {
            s.moved = target;
        }
    }

    fn remove(&mut self, id: u32) {
        if let Some(s) = self.streams.remove(&id) {
            if s.moved.is_some() {
                if let Some((md, _)) = &self.metadata {
                    md.set_property(id, "target.object", None, None);
                }
            }
        }
    }
}
