//! Native Aidoku WebAssembly source engine for the Yejian reader.
//!
//! This crate is a Flutter/Dart-free extraction of the `aidoku_wasm` host that
//! lived in `app/rust/src/aidoku_wasm` of the Mangayomi snapshot. See
//! `NOTICE` for provenance and `README.md` for the interface contract.
//!
//! Typical use:
//!
//! ```no_run
//! use yejian_sources::{SourceCatalog, SourceEngine};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let catalog = SourceCatalog::new("./sources");
//! let installed = catalog.get("example.source")?;
//! let engine = SourceEngine::load_installed(&installed)?;
//! let page = engine.search("one piece").await?;
//! println!("{} results", page.entries.len());
//! # Ok(())
//! # }
//! ```

mod catalog;
mod engine;
mod error;
mod filters;
mod http;
pub mod repo;
mod wasm;

pub use catalog::{CatalogError, InstallError, InstalledSource, SourceCatalog, SourceManifest};
pub use engine::{BoxFuture, DEFAULT_FUEL_PER_CALL, EngineOptions, RequestHandler, SourceEngine};
pub use error::SourceError;
pub use filters::{default_filter_values, parse_filters_json};
pub use http::{MAX_RESPONSE_BODY_BYTES, NativeHttp, NetRequestData, NetResponseData};
pub use repo::{RepoError, RepositoryIndex, RepositorySource, SourceInstaller};
pub use wasm::models::{
    Chapter, ContentRating, DeepLinkResult, Filter, FilterTypeConfig, FilterValue, ImageRequest,
    Listing, ListingKind, Manga, MangaPageResult, MultiSelectFilterCfg, Page, PageContent,
    PageContext, PublishingStatus, SelectFilterCfg, SortDefault, SortFilterValue, SourceFeatures,
    UpdateStrategy, Viewer,
};
