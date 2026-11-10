//! NavDataSet: the binary row format the BC web server reads from GetPage (and from
//! `NavRecordState.FormVariables` / `AutoKeyValues`), record bookmarks, and GetPage
//! itself (paging by bookmark, direction and page size on top of al-runtime).
//! See lib.rs for module ownership and docs/nst-protocol-metadata.md section 3.
//!
//! - [`navdataset`]: `NavDataSet` / `DataTable` / `Column` / `Cell`, the binary
//!   stream, and the `{"compressed","count","buffers"}` JSON wrapper.
//! - [`convert`]: bc-rust values (`bc_store::Value`, `al_runtime::Val`, `FieldDef`)
//!   to cells, and the .NET tick arithmetic.
//! - [`bookmark`]: bc-rust's bookmark bytes (table id + primary key values).
//! - [`dto`]: `PageRequestDefinition`, `NavRecordState`, `TableViewDto`, filters,
//!   `GetPageRequest`, `DataSetResponse` with their wire names.
//! - [`page`]: `get_page` / `read_page`.
//!
//! Serialization note: the .NET NavDataSet reader is positional. `NavDataSet`
//! implements `Serialize` with the properties in the right order, so it can sit in
//! any `#[derive(Serialize)]` DTO. Going through `serde_json::Value` (`to_value`,
//! `json!`) keeps the order only because bc-nst enables serde_json's
//! `preserve_order`; without it a `Value` map sorts its keys and puts `buffers`
//! first. The tests check the order on the `Value` path too.
//!
//! BC 29.1 [src:TODO]: the wrapper and its property
//! order, raw DEFLATE, the stream layout, cell markers and payloads (including
//! decimal, DateTime ticks + kind, Guid order, 7-bit string lengths), unique table
//! names, the system column names and types, the two-table reply for
//! `PageSizeInOppositeDirection`, nearest-first row order per table, the use of the
//! last row's bookmark for the next read, bookmark string forms, Time on 0001-01-02
//! and closing dates at 23:59:59.000.
//! Guessed (no live capture yet): the data set and table names the NST uses, the
//! DateTime kind for Date/Time cells, the column set for FlowFilter fields, the exact
//! start-position rules, and that `"0"` only needs to change when the row does.

pub mod bookmark;
pub mod convert;
pub mod dto;
pub mod navdataset;
pub mod netbin;
pub mod page;

pub use bookmark::{bookmark_row, decode_bookmark, encode_bookmark, encode_pk, pk_row, BookmarkError};
pub use convert::{cell_val, field_cell, field_column, field_type_code, val_cell, val_type_code, value_cell};
pub use dto::{DataSetResponse, GetPageRequest, ViewFilter, ViewFilterGroup, NavRecordState, TableViewDto, PageRequestDefinition, ReadDirection, StartingPage};
pub use navdataset::{Cell, Column, DataTable, DatasetError, DateTimeKind, NavDataSet, CellTypeCode, TypedCell, WireDataSet};
pub use page::{apply_page_view, get_page, get_page_with, get_table_page, read_page, record_dataset, row_version, GetPageResult, NoHook, PageRowHook};

#[cfg(test)]
pub(crate) mod tests;
