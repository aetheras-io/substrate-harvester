use std::collections::HashMap;
use std::convert::TryFrom;
use std::str::FromStr;

use frame_metadata::{RuntimeMetadata, RuntimeMetadataLastVersion, RuntimeMetadataPrefixed};

use scale_info::{form::PortableForm, Variant};

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
    #[error("Type {0} missing from type registry")]
    MissingType(u32),
    #[error("Type {0} was not a variant/enum type")]
    TypeDefNotVariant(u32),
}

#[derive(Clone, Debug)]
pub struct Metadata {
    metadata: RuntimeMetadataLastVersion,
    modules: HashMap<u8, Module>,
    modules_with_events: HashMap<u8, ModuleWithEvents>,
}

impl Metadata {
    pub fn module(&self, idx: u8) -> Result<&Module, Error> {
        if let Some(m) = self.modules.get(&idx) {
            Ok(m)
        } else {
            Err(Error::ModuleNotFound(idx))
        }
    }

    pub fn modules_with_events(&self) -> &HashMap<u8, ModuleWithEvents> {
        &self.modules_with_events
    }

    pub fn module_with_events(&self, idx: u8) -> Result<&ModuleWithEvents, Error> {
        if let Some(m) = self.modules_with_events.get(&idx) {
            Ok(m)
        } else {
            Err(Error::ModuleNotFound(idx))
        }
    }

    /// Return the runtime metadata.
    pub fn runtime_metadata(&self) -> &RuntimeMetadataLastVersion {
        &self.metadata
    }
}

impl TryFrom<RuntimeMetadataPrefixed> for Metadata {
    type Error = Error;

    fn try_from(metadata: RuntimeMetadataPrefixed) -> Result<Self, Self::Error> {
        if metadata.0 != frame_metadata::META_RESERVED {
            return Err(Error::InvalidPrefix);
        }

        let meta = match metadata.1 {
            RuntimeMetadata::V14(meta) => meta,
            _ => return Err(Error::InvalidVersion),
        };

        let metadata = meta.clone();

        let mut modules = HashMap::new();
        let mut modules_with_events = HashMap::new();

        let get_type_def_variant = |type_id: u32| {
            let ty = meta
                .types
                .resolve(type_id)
                .ok_or(Error::MissingType(type_id))?;

            if let scale_info::TypeDef::Variant(var) = ty.type_def() {
                Ok(var)
            } else {
                Err(Error::TypeDefNotVariant(type_id))
            }
        };

        for pallet in meta.pallets {
            let module = Module {
                name: pallet.name.to_string(),
                ..Default::default()
            };

            modules.insert(pallet.index, module);

            if let Some(events) = pallet.event {
                let mut mod_events = ModuleWithEvents {
                    name: pallet.name.to_string(),
                    ..Default::default()
                };

                let type_def_variant = get_type_def_variant(events.ty.id())?;

                for var in type_def_variant.variants() {
                    let event_metadata = ModuleEventMetadata {
                        name: var.name().clone(),
                        variant: var.clone(),
                    };

                    mod_events.events.push(event_metadata);
                }

                modules_with_events.insert(pallet.index, mod_events);
            }
        }

        Ok(Metadata {
            metadata,
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

#[derive(Clone, Debug)]
pub struct ModuleEventMetadata {
    name: String,
    variant: Variant<PortableForm>,
}

impl ModuleEventMetadata {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the type def variant for the pallet event.
    pub fn variant(&self) -> &Variant<PortableForm> {
        &self.variant
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
    Option(Box<EventArg>),
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
        } else if s.starts_with("Option<") {
            if s.ends_with('>') {
                Ok(EventArg::Option(Box::new(s[7..s.len() - 1].parse()?)))
            } else {
                Err(Error::InvalidEventArgument(format!(
                    "Expected closing `>` for `Option` got: {}",
                    s.to_string()
                )))
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
            EventArg::Option(arg) => arg.primitives(),
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
