//! GetPage DTOs, mirroring Microsoft.Dynamics.Nav.Types (BC 29.1):
//! `PageRequestDefinition`, `NavRecordState`, `TableViewDto`, `ViewFilterGroup`,
//! `ViewFilter`, `GetPageRequest`, `DataSetResponse`.
//!
//! Wire rules (JSON-RPC, Newtonsoft with `SharedJsonSettings`): property names are the
//! `[DataMember]` names, which are the C# property names here (`dataSet` and
//! `requests` are private fields). `ViewFilterGroup` and `ViewFilter` are
//! `[Serializable]` without a data contract, so their public properties go on the
//! wire. Enums are integers, `byte[]` is base64, `Guid` a string.
//!
//! `NavRecordState` has about forty members. The ones GetPage reads or writes are
//! typed; everything else is kept verbatim in `other` and written back unchanged.

use super::navdataset::NavDataSet;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// null → `T::default()` (the .NET side writes nulls for empty reference members).
fn null_default<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

fn yes() -> bool {
    true
}

/// `byte[]` as base64, null allowed.
pub mod b64opt {
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(b)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        match Option::<String>::deserialize(d)? {
            None => Ok(None),
            Some(s) => base64::engine::general_purpose::STANDARD
                .decode(s.as_bytes())
                .map(Some)
                .map_err(serde::de::Error::custom),
        }
    }
}

/// `ReadDirection`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReadDirection {
    #[default]
    Next,
    Previous,
}

impl Serialize for ReadDirection {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for ReadDirection {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Option::<i32>::deserialize(d)? {
            Some(1) => ReadDirection::Previous,
            _ => ReadDirection::Next,
        })
    }
}

/// `StartingPage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartingPage {
    #[default]
    Specific,
    First,
    Last,
}

impl Serialize for StartingPage {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(*self as i32)
    }
}

impl<'de> Deserialize<'de> for StartingPage {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Option::<i32>::deserialize(d)? {
            Some(1) => StartingPage::First,
            Some(2) => StartingPage::Last,
            _ => StartingPage::Specific,
        })
    }
}

/// `RuntimeFilterType`.
pub mod runtime_filter_type {
    pub const SYSTEM: i32 = 0;
    pub const USER: i32 = 1;
    pub const RUN_FORM_LINK: i32 = 2;
}

/// `PageRequestDefinition`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageRequestDefinition {
    #[serde(rename = "IsSubFormUpdateRequest", default)]
    pub is_sub_form_update_request: bool,
    /// Field ids to fill (normally null: all fields).
    #[serde(rename = "NormalFields", default)]
    pub normal_fields: Option<Vec<i32>>,
    /// FlowFields (and BLOBs) to calculate: the visible FlowField controls.
    #[serde(rename = "CalcFields", default)]
    pub calc_fields: Option<Vec<i32>>,
    /// Fields the client needs (partial records, `SetLoadFields`).
    #[serde(rename = "LoadFieldsHint", default)]
    pub load_fields_hint: Option<Vec<i32>>,
    #[serde(rename = "PageSize", default)]
    pub page_size: i32,
    #[serde(rename = "PageSizeInOppositeDirection", default)]
    pub page_size_in_opposite_direction: i32,
    #[serde(rename = "ReadDirection", default)]
    pub read_direction: ReadDirection,
    #[serde(rename = "StartFromPage", default)]
    pub start_from_page: StartingPage,
    /// C# default true.
    #[serde(rename = "IncludeMoreDataInformation", default = "yes")]
    pub include_more_data_information: bool,
    #[serde(rename = "IncludeNonRowData", default)]
    pub include_non_row_data: bool,
    #[serde(rename = "StartingBookmark", default, with = "b64opt")]
    pub starting_bookmark: Option<Vec<u8>>,
    #[serde(rename = "ExcludeStartingRecord", default)]
    pub exclude_starting_record: bool,
    /// `[{"typeHint": JsonTypeHint, "value": ...}]` (ObjectArrayWithTypeInfoConverter).
    #[serde(rename = "LookupFieldValues", default)]
    pub lookup_field_values: Option<Value>,
    #[serde(rename = "LookupFieldIds", default)]
    pub lookup_field_ids: Option<Vec<i32>>,
    #[serde(rename = "RequestRecentEntries", default)]
    pub request_recent_entries: bool,
}

impl Default for PageRequestDefinition {
    fn default() -> Self {
        PageRequestDefinition {
            is_sub_form_update_request: false,
            normal_fields: None,
            calc_fields: None,
            load_fields_hint: None,
            page_size: 0,
            page_size_in_opposite_direction: 0,
            read_direction: ReadDirection::Next,
            start_from_page: StartingPage::Specific,
            include_more_data_information: true,
            include_non_row_data: false,
            starting_bookmark: None,
            exclude_starting_record: false,
            lookup_field_values: None,
            lookup_field_ids: None,
            request_recent_entries: false,
        }
    }
}

/// `ViewFilter` (public properties of a `[Serializable]` class).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ViewFilter {
    /// Field number.
    #[serde(rename = "FilterField", default)]
    pub filter_field: i32,
    /// BC filter expression, or an exact value when `IsExactValue`.
    #[serde(rename = "FilterValue", default)]
    pub filter_value: Option<String>,
    #[serde(rename = "UserTypedFilterValue", default)]
    pub user_typed_filter_value: Option<String>,
    #[serde(rename = "ExpressedFilterValue", default)]
    pub expressed_filter_value: Option<String>,
    #[serde(rename = "OptionsAsCaptionsFilterValue", default)]
    pub options_as_captions_filter_value: Option<String>,
    /// `RuntimeFilterType`: System 0, User 1, RunFormLink 2.
    #[serde(rename = "RuntimeFilter", default)]
    pub runtime_filter: i32,
    #[serde(rename = "IsExactValue", default)]
    pub is_exact_value: bool,
}

/// `ViewFilterGroup`. Group -1 is the cross-column search (filters OR together).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ViewFilterGroup {
    #[serde(rename = "FilterGroupNo", default)]
    pub filter_group_no: i32,
    /// Always written as an array: the client reads `Filters.Length` unguarded.
    #[serde(rename = "Filters", default, deserialize_with = "null_default")]
    pub filters: Vec<ViewFilter>,
}

/// `TableViewDto`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableViewDto {
    #[serde(rename = "TableId", default)]
    pub table_id: i32,
    /// Field ids of the current sorting; null when the user has not sorted.
    #[serde(rename = "CurrentSortingFieldIds", default)]
    pub current_sorting_field_ids: Option<Vec<i32>>,
    #[serde(rename = "Ascending", default = "yes")]
    pub ascending: bool,
    #[serde(rename = "CurrentFilters", default, deserialize_with = "null_default")]
    pub current_filters: Vec<ViewFilterGroup>,
    #[serde(rename = "SearchFilter", default, skip_serializing_if = "Option::is_none")]
    pub search_filter: Option<ViewFilterGroup>,
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl Default for TableViewDto {
    fn default() -> Self {
        TableViewDto {
            table_id: 0,
            current_sorting_field_ids: None,
            ascending: true,
            current_filters: vec![],
            search_filter: None,
            other: Map::new(),
        }
    }
}

/// `NavRecordState`: typed where GetPage needs it, the rest passed through.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct NavRecordState {
    /// Never null in a reply: the client reads `TableView.Ascending` unguarded.
    #[serde(rename = "TableView", default, skip_serializing_if = "Option::is_none")]
    pub table_view: Option<TableViewDto>,
    /// Bookmark of the client's current row.
    #[serde(rename = "CurrentRecord", default, with = "b64opt", skip_serializing_if = "Option::is_none")]
    pub current_record: Option<Vec<u8>>,
    #[serde(rename = "FormId", default)]
    pub form_id: i32,
    #[serde(rename = "CurrentFilterGroup", default)]
    pub current_filter_group: i32,
    #[serde(rename = "MoreDataInReadDirection", default)]
    pub more_data_in_read_direction: bool,
    #[serde(rename = "MoreDataInOppositeDirection", default)]
    pub more_data_in_opposite_direction: bool,
    /// All other members (ServerFormHandle, SelectedRecords, FormVariables, ...).
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

impl NavRecordState {
    /// A state with the defaults of `NavRecordState.InitializeDefaultMembers`.
    pub fn initial(table_id: i32, form_id: i32) -> NavRecordState {
        let mut other = Map::new();
        other.insert("SelectedRecords".into(), Value::Array(vec![]));
        other.insert("ValidateRequired".into(), Value::Bool(true));
        other.insert("NavFormEditable".into(), Value::Bool(true));
        other.insert("ValidateFieldsInOnNewRecord".into(), Value::Bool(true));
        other.insert("DataSourceType".into(), Value::from(0));
        NavRecordState {
            table_view: Some(TableViewDto {
                table_id,
                ..Default::default()
            }),
            form_id,
            other,
            ..Default::default()
        }
    }

    pub fn table_view_mut(&mut self) -> &mut TableViewDto {
        self.table_view.get_or_insert_with(TableViewDto::default)
    }
}

/// `GetPageRequest`: the two `IClientApi.GetPage` arguments, plus the members of the
/// `client/data/page` POST body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GetPageRequest {
    #[serde(rename = "PageRequestDefinition", default)]
    pub page_request_definition: PageRequestDefinition,
    #[serde(rename = "State", default)]
    pub state: NavRecordState,
    #[serde(rename = "GetRowsRequest", default, skip_serializing_if = "Option::is_none")]
    pub get_rows_request: Option<Value>,
    #[serde(rename = "MaxRowCount", default)]
    pub max_row_count: i32,
}

impl GetPageRequest {
    /// From the positional JSON-RPC params `[PageRequestDefinition, NavRecordState]`.
    pub fn from_params(params: &Value) -> Result<GetPageRequest, serde_json::Error> {
        let def = params.get(0).cloned().unwrap_or(Value::Null);
        let st = params.get(1).cloned().unwrap_or(Value::Null);
        Ok(GetPageRequest {
            page_request_definition: if def.is_null() { Default::default() } else { serde_json::from_value(def)? },
            state: if st.is_null() { Default::default() } else { serde_json::from_value(st)? },
            get_rows_request: None,
            max_row_count: 0,
        })
    }
}

/// `DataSetResponse : ServerResponse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataSetResponse {
    /// Queued callbacks (`Queue<ServerCallbackRequest>`): empty, never null.
    #[serde(rename = "requests", default, deserialize_with = "null_default")]
    pub requests: Vec<Value>,
    #[serde(rename = "VersionNumber", default)]
    pub version_number: i64,
    #[serde(rename = "ClientContext", default, skip_serializing_if = "Option::is_none")]
    pub client_context: Option<Value>,
    #[serde(rename = "dataSet")]
    pub data_set: NavDataSet,
    #[serde(rename = "State")]
    pub state: NavRecordState,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn page_request_from_dotnet_json() {
        let j = json!({
            "IsSubFormUpdateRequest": false, "NormalFields": null, "CalcFields": [59, 60], "LoadFieldsHint": null,
            "PageSize": 50, "PageSizeInOppositeDirection": 10, "ReadDirection": 1, "StartFromPage": 2,
            "IncludeMoreDataInformation": true, "IncludeNonRowData": true, "StartingBookmark": "vAESAQUGQzEwMDAw",
            "ExcludeStartingRecord": true, "LookupFieldValues": null, "LookupFieldIds": null, "RequestRecentEntries": false
        });
        let d: PageRequestDefinition = serde_json::from_value(j).unwrap();
        assert_eq!(d.calc_fields, Some(vec![59, 60]));
        assert_eq!(d.read_direction, ReadDirection::Previous);
        assert_eq!(d.start_from_page, StartingPage::Last);
        assert_eq!(
            d.starting_bookmark.as_deref(),
            Some(&[0xBC, 0x01, 18, 1, 5, 6, b'C', b'1', b'0', b'0', b'0', b'0'][..])
        );
        let back = serde_json::to_value(&d).unwrap();
        assert_eq!(back["ReadDirection"], 1);
        assert_eq!(back["StartingBookmark"], "vAESAQUGQzEwMDAw");
        // missing members take the C# defaults
        let d: PageRequestDefinition = serde_json::from_value(json!({"PageSize": 1})).unwrap();
        assert!(d.include_more_data_information);
        assert_eq!(d.start_from_page, StartingPage::Specific);
    }

    #[test]
    fn record_state_passes_unknown_members_through() {
        let j = json!({
            "RunFormOnRec": false,
            "TableView": {"TableId": 18, "CurrentSortingFieldIds": null, "Ascending": true,
                          "CurrentFilters": [{"FilterGroupNo": 0, "Filters": [{"FilterField": 7, "FilterValue": "Atlanta", "UserTypedFilterValue": null,
                            "ExpressedFilterValue": null, "OptionsAsCaptionsFilterValue": null, "RuntimeFilter": 1, "IsExactValue": false}]}],
                          "SearchFilter": {"FilterGroupNo": -1, "Filters": null}},
            "CurrentRecord": null, "ServerFormHandle": "8d7c7b4e-3f6f-4a7c-9a51-1c2d3e4f5a6b", "FormId": 22,
            "SelectedRecords": [], "MoreDataInReadDirection": false, "CurrentFilterGroup": 0, "FormVariables": null
        });
        let st: NavRecordState = serde_json::from_value(j).unwrap();
        let tv = st.table_view.as_ref().unwrap();
        assert_eq!(tv.current_filters[0].filters[0].filter_value.as_deref(), Some("Atlanta"));
        assert_eq!(tv.search_filter.as_ref().unwrap().filters.len(), 0);
        assert_eq!(st.other["ServerFormHandle"], "8d7c7b4e-3f6f-4a7c-9a51-1c2d3e4f5a6b");
        let back = serde_json::to_value(&st).unwrap();
        assert_eq!(back["ServerFormHandle"], "8d7c7b4e-3f6f-4a7c-9a51-1c2d3e4f5a6b");
        assert_eq!(back["TableView"]["SearchFilter"]["Filters"], json!([]));
        assert_eq!(back["FormId"], 22);
        assert!(back.get("CurrentRecord").is_none());
    }

    #[test]
    fn params_and_response_shape() {
        let p = json!([{"PageSize": 5, "StartFromPage": 1}, {"TableView": {"TableId": 18}}]);
        let r = GetPageRequest::from_params(&p).unwrap();
        assert_eq!(r.page_request_definition.page_size, 5);
        assert_eq!(r.state.table_view.unwrap().table_id, 18);
        let resp = DataSetResponse {
            requests: vec![],
            version_number: 0,
            client_context: None,
            data_set: NavDataSet::new(),
            state: NavRecordState::initial(18, 22),
        };
        let s = serde_json::to_string(&resp).unwrap();
        assert!(s.contains(r#""requests":[]"#));
        assert!(s.contains(r#""dataSet":{"compressed":false,"count":1,"buffers":[""#), "{s}");
        let back: DataSetResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(back, resp);
    }
}
