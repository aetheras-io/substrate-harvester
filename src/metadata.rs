use std::convert::TryFrom;
use std::str::FromStr;

use frame_metadata::{DecodeDifferent, RuntimeMetadata, RuntimeMetadataPrefixed};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Error extracting substrate metadata: {0}")]
    Extraction(u8),
    #[error("Module not found")]
    ModuleNotFound(u8),
    #[error("Event not found")]
    EventNotFound(u8),
    #[error("Storage not found")]
    StorageNotFound(u8),
    #[error("Event argument error: {0}")]
    InvalidEventArgument(String),
    #[error("Invalid metadata prefix")]
    InvalidPrefix,
    #[error("Invalid metadata version")]
    InvalidVersion,
}

#[derive(Clone, Default, Debug)]
pub struct Metadata {
    modules: Vec<Module>,
    modules_with_events: Vec<ModuleWithEvents>,
}

impl Metadata {
    pub fn module(&self, idx: u8) -> Result<&Module, Error> {
        if (idx as usize) < self.modules.len() {
            Ok(&self.modules[idx as usize])
        } else {
            Err(Error::ModuleNotFound(idx))
        }
    }

    pub fn modules_with_events(&self) -> &Vec<ModuleWithEvents> {
        &self.modules_with_events
    }

    pub fn module_with_events(&self, idx: u8) -> Result<&ModuleWithEvents, Error> {
        if (idx as usize) < self.modules_with_events.len() {
            Ok(&self.modules_with_events[idx as usize])
        } else {
            Err(Error::ModuleNotFound(idx))
        }
    }
}

impl TryFrom<RuntimeMetadataPrefixed> for Metadata {
    type Error = Error;

    fn try_from(metadata: RuntimeMetadataPrefixed) -> Result<Self, Self::Error> {
        if metadata.0 != frame_metadata::META_RESERVED {
            return Err(Error::InvalidPrefix);
        }

        let meta = match metadata.1 {
            RuntimeMetadata::V11(meta) => meta,
            _ => return Err(Error::InvalidVersion),
        };

        let mut modules = Vec::new();
        let mut modules_with_events = Vec::new();
        for encoded in extract(meta.modules)?.into_iter() {
            let module_name = extract(encoded.name.clone())?;
            let module = Module {
                name: module_name.clone(),
                ..Default::default()
            };
            modules.push(module);

            // //we aren't handling storage just yet
            // if let Some(storage) = encoded.storage {
            //     let storage = extract(storage)?;
            //     let module_prefix = extract(storage.prefix)?;
            // }

            if let Some(events) = encoded.event {
                let mut module_with_events = ModuleWithEvents {
                    name: module_name,
                    ..Default::default()
                };
                for event in extract(events)?.into_iter() {
                    module_with_events.events.push(extract_event(event)?)
                }
                modules_with_events.push(module_with_events);
            }
        }
        Ok(Metadata {
            modules,
            modules_with_events,
        })
    }
}

#[derive(Clone, Default, Debug)]
pub struct Module {
    name: String,
    storage: Vec<()>, // we don't need storage yet
}

impl Module {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn storage(&self, idx: u8) -> Result<(), Error> {
        if (idx as usize) < self.storage.len() {
            Ok(self.storage[idx as usize])
        } else {
            log::info!("### Could not find storage index {:?}", idx);
            Err(Error::StorageNotFound(idx))
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct ModuleWithEvents {
    name: String,
    events: Vec<ModuleEventMetadata>,
}

impl ModuleWithEvents {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn event(&self, idx: u8) -> Result<&ModuleEventMetadata, Error> {
        if (idx as usize) < self.events.len() {
            Ok(&self.events[idx as usize])
        } else {
            log::info!("### Could not find event index {:?}", idx);
            Err(Error::EventNotFound(idx))
        }
    }

    pub fn events(&self) -> &Vec<ModuleEventMetadata> {
        &self.events
    }
}

#[derive(Clone, Default, Debug)]
pub struct ModuleEventMetadata {
    name: String,
    arguments: Vec<EventArg>,
}

impl ModuleEventMetadata {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn arguments(&self) -> Vec<EventArg> {
        self.arguments.to_vec()
    }
}

/// Naive representation of event argument types, supports current set of substrate EventArg types.
/// If and when Substrate uses `type-metadata`, this can be replaced.
///
/// Used to calculate the size of a instance of an event variant without having the concrete type,
/// so the raw bytes can be extracted from the encoded `Vec<EventRecord<E>>` (without `E` defined).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum EventArg {
    Primitive(String),
    Vec(Box<EventArg>),
    Tuple(Vec<EventArg>),
}

impl FromStr for EventArg {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with("Vec<") {
            if s.ends_with('>') {
                Ok(EventArg::Vec(Box::new(s[4..s.len() - 1].parse()?)))
            } else {
                Err(Error::InvalidEventArgument(s.to_string()))
            }
        } else if s.starts_with('(') {
            if s.ends_with(')') {
                let mut args = Vec::new();
                for arg in s[1..s.len() - 1].split(',') {
                    let arg = arg.trim().parse()?;
                    args.push(arg)
                }
                Ok(EventArg::Tuple(args))
            } else {
                Err(Error::InvalidEventArgument(s.to_string()))
            }
        } else {
            Ok(EventArg::Primitive(s.to_string()))
        }
    }
}

impl EventArg {
    /// Returns all primitive types for this EventArg
    pub fn primitives(&self) -> Vec<String> {
        match self {
            EventArg::Primitive(p) => vec![p.clone()],
            EventArg::Vec(arg) => arg.primitives(),
            EventArg::Tuple(args) => {
                let mut primitives = Vec::new();
                for arg in args {
                    primitives.extend(arg.primitives())
                }
                primitives
            }
        }
    }
}

fn extract<B: 'static, O: 'static>(dd: DecodeDifferent<B, O>) -> Result<O, Error> {
    match dd {
        DecodeDifferent::Decoded(value) => Ok(value),
        _ => Err(Error::Extraction(0)),
    }
}

fn extract_event(event: frame_metadata::EventMetadata) -> Result<ModuleEventMetadata, Error> {
    let name = extract(event.name)?;
    let mut arguments = Vec::new();
    for arg in extract(event.arguments)? {
        let arg = arg.parse::<EventArg>()?;
        arguments.push(arg);
    }
    Ok(ModuleEventMetadata { name, arguments })
}
