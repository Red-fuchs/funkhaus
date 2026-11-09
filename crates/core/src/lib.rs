pub mod ask;
pub mod cite;
pub mod config;
pub mod doctor;
pub mod eval;
pub mod guide;
pub mod ingest;
pub mod llm;
pub mod make;
pub mod map;
pub mod music;
pub mod podcast;
pub mod retrieval;
pub mod store;
pub mod util;
pub mod voices;

pub use config::Config;
pub use store::{Backend, Notebook, Source, Store};

/// Progress sink: one human-readable line per call.
pub type Prog<'a> = &'a dyn Fn(&str);
