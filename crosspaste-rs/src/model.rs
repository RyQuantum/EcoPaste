//! JSON shapes on the wire. Field names and defaults follow CrossPaste's
//! kotlinx.serialization config (`encodeDefaults = true`,
//! `ignoreUnknownKeys = true`, polymorphic discriminator `type`).

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};

use crate::hash;

/// `SyncApi.VERSION`; peers refuse to sync unless both sides report the same value.
pub const SYNC_API_VERSION: i32 = 3;
/// Highest pairing protocol we implement (v2 = SAS key exchange).
pub const PAIRING_VERSION: i32 = 2;

pub const PASTE_TYPE_TEXT: i32 = 0;
pub const PASTE_TYPE_URL: i32 = 1;
pub const PASTE_TYPE_HTML: i32 = 2;
pub const PASTE_TYPE_FILE: i32 = 3;
pub const PASTE_TYPE_IMAGE: i32 = 4;
pub const PASTE_TYPE_RTF: i32 = 5;

/// Identifier CrossPaste records for a clipboard bitmap (as opposed to a copied file).
pub const IMAGE_IDENTIFIER: &str = "image/x-java-image";

pub mod b64 {
    use super::*;

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&B64.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        B64.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub app_instance_id: String,
    pub app_version: String,
    pub app_revision: String,
    pub user_name: String,
    #[serde(default)]
    pub pairing_version: Option<i32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Platform {
    pub name: String,
    pub arch: String,
    pub bit_mode: i32,
    pub version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub network_prefix_length: i16,
    pub host_address: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EndpointInfo {
    pub device_id: String,
    pub device_name: String,
    pub platform: Platform,
    pub host_info_list: Vec<HostInfo>,
    pub port: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncInfo {
    pub app_info: AppInfo,
    pub endpoint_info: EndpointInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyExchange {
    #[serde(with = "b64")]
    pub sign_public_key: Vec<u8>,
    #[serde(with = "b64")]
    pub crypt_public_key: Vec<u8>,
    pub timestamp: i64,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrustConfirm {
    pub timestamp: i64,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailResponse {
    pub error_code: i32,
    #[serde(default)]
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullFileRequest {
    pub id: i64,
    pub chunk_index: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ControlChallenge {
    #[serde(with = "b64")]
    pub id: Vec<u8>,
    #[serde(with = "b64")]
    pub nonce: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticatedControlRequest {
    #[serde(with = "b64")]
    pub challenge_id: Vec<u8>,
    #[serde(with = "b64")]
    pub challenge_nonce: Vec<u8>,
    pub target_app_instance_id: String,
    pub operation: String,
}

/// `PasteCollection`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteCollection {
    #[serde(default)]
    pub paste_items: Vec<Value>,
}

/// `PasteData`. Items stay as raw JSON: we only build text and image items
/// and only read the handful of fields we need from received ones.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasteData {
    #[serde(default = "default_id")]
    pub id: i64,
    pub app_instance_id: String,
    #[serde(default)]
    pub favorite: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paste_appear_item: Option<Value>,
    #[serde(default)]
    pub paste_collection: PasteCollection,
    pub paste_type: i32,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub size: i64,
    #[serde(default)]
    pub hash: String,
}

fn default_id() -> i64 {
    -1
}

/// One file of an images/files item, as listed in `fileInfoTreeMap`.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteFile {
    pub name: String,
    pub size: u64,
    pub hash: String,
}

/// What a received paste means to us.
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteContent {
    Text(String),
    Images(Vec<RemoteFile>),
    Unsupported(i32),
}

impl PasteData {
    pub fn text(id: i64, app_instance_id: &str, text: &str) -> Self {
        let bytes = text.as_bytes();
        let item_hash = hash::hash(bytes);
        let size = bytes.len() as i64;
        let item = json!({
            "type": "text",
            "identifiers": ["text/plain"],
            "hash": item_hash,
            "size": size,
            "text": text,
            "extraInfo": null,
        });
        Self {
            id,
            app_instance_id: app_instance_id.to_string(),
            favorite: false,
            paste_appear_item: Some(item),
            paste_collection: PasteCollection::default(),
            paste_type: PASTE_TYPE_TEXT,
            source: None,
            size,
            hash: item_hash,
        }
    }

    /// A single clipboard bitmap stored as `file_name` (extension decides
    /// how the receiver re-exports it).
    pub fn image(id: i64, app_instance_id: &str, file_name: &str, bytes: &[u8]) -> Self {
        let file_hash = hash::hash(bytes);
        let size = bytes.len() as i64;
        let item_hash = hash::hash_strings(&[&file_hash]);
        let item = json!({
            "type": "images",
            "identifiers": [IMAGE_IDENTIFIER],
            "count": 1,
            "hash": item_hash,
            "size": size,
            "fileInfoTreeMap": {
                file_name: { "type": "file", "size": size, "hash": file_hash }
            },
            "relativePathList": [file_name],
            "extraInfo": null,
        });
        Self {
            id,
            app_instance_id: app_instance_id.to_string(),
            favorite: false,
            paste_appear_item: Some(item),
            paste_collection: PasteCollection::default(),
            paste_type: PASTE_TYPE_IMAGE,
            source: None,
            size,
            hash: item_hash,
        }
    }

    pub fn all_items(&self) -> Vec<&Value> {
        self.paste_appear_item
            .iter()
            .chain(self.paste_collection.paste_items.iter())
            .collect()
    }

    /// Interprets a received paste. Text-like types fall back to any plain
    /// text item in the collection (HTML and RTF copies carry one).
    pub fn content(&self) -> RemoteContent {
        let items = self.all_items();
        match self.paste_type {
            PASTE_TYPE_IMAGE => {
                if let Some(files) = items
                    .iter()
                    .find(|v| item_type(v) == Some("images"))
                    .and_then(|v| remote_files(v))
                {
                    return RemoteContent::Images(files);
                }
            }
            PASTE_TYPE_TEXT | PASTE_TYPE_URL | PASTE_TYPE_HTML | PASTE_TYPE_RTF => {
                let text = items
                    .iter()
                    .find(|v| item_type(v) == Some("text"))
                    .and_then(|v| v.get("text"))
                    .or_else(|| {
                        items
                            .iter()
                            .find(|v| item_type(v) == Some("url"))
                            .and_then(|v| v.get("url"))
                    })
                    .and_then(Value::as_str);
                if let Some(text) = text {
                    if !text.is_empty() {
                        return RemoteContent::Text(text.to_string());
                    }
                }
            }
            _ => {}
        }
        RemoteContent::Unsupported(self.paste_type)
    }
}

fn item_type(v: &Value) -> Option<&str> {
    v.get("type").and_then(Value::as_str)
}

/// Files of an images item in `relativePathList` order (the order chunks
/// are laid out in). Directories are not supported.
fn remote_files(item: &Value) -> Option<Vec<RemoteFile>> {
    let tree = item.get("fileInfoTreeMap")?.as_object()?;
    let paths = item.get("relativePathList")?.as_array()?;
    let mut files = Vec::new();
    for path in paths {
        let path = path.as_str()?;
        let name = path.rsplit(['/', '\\']).next()?.to_string();
        let node = tree.get(&name)?;
        if node.get("type").and_then(Value::as_str) != Some("file") {
            return None;
        }
        files.push(RemoteFile {
            size: node.get("size")?.as_u64()?,
            hash: node.get("hash")?.as_str()?.to_string(),
            name,
        });
    }
    (!files.is_empty()).then_some(files)
}

/// Error codes from CrossPaste's `StandardErrorCode` that we send or react to.
pub mod error_code {
    pub const UNKNOWN_ERROR: i32 = 0;
    pub const NOT_FOUND_APP_INSTANCE_ID: i32 = 1000;
    pub const TOKEN_INVALID: i32 = 1001;
    pub const NOT_FOUND_ICON: i32 = 1003;
    pub const NOT_FOUND_FILES_INDEX: i32 = 1010;
    pub const NOT_MATCH_APP_INSTANCE_ID: i32 = 1011;
    pub const OUT_RANGE_CHUNK_INDEX: i32 = 102;
    pub const SIGN_INVALID: i32 = 2000;
    pub const EXCHANGE_FAIL: i32 = 2001;
    pub const EXCHANGE_TIMEOUT: i32 = 2002;
    pub const TRUST_FAIL: i32 = 2005;
    pub const DECRYPT_FAIL: i32 = 2008;
    pub const SYNC_PASTE_ERROR: i32 = 3000;
    pub const SYNC_PASTE_NOT_FOUND_DATA: i32 = 3002;
    pub const REMOTE_SHOW_PAIRING_CODE_DISABLED: i32 = 3100;
    pub const SYNC_NOT_ALLOW_RECEIVE_BY_APP: i32 = 4003;
    pub const PUSH_SESSION_REJECTED: i32 = 7001;

    /// HTTP status CrossPaste pairs with each code (by its `ErrorType`).
    pub fn status(code: i32) -> u16 {
        match code {
            UNKNOWN_ERROR | OUT_RANGE_CHUNK_INDEX | SYNC_NOT_ALLOW_RECEIVE_BY_APP => 500,
            REMOTE_SHOW_PAIRING_CODE_DISABLED => 422,
            _ => 400,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_paste_matches_crosspaste_shape() {
        let p = PasteData::text(7, "me", "hello");
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["id"], 7);
        assert_eq!(v["pasteType"], 0);
        assert_eq!(v["pasteCollection"], json!({"pasteItems": []}));
        assert_eq!(v["pasteAppearItem"]["type"], "text");
        assert_eq!(
            v["pasteAppearItem"]["hash"],
            "e58075f7d20aa573c58a3c352a967db7"
        );
        assert_eq!(v["hash"], "e58075f7d20aa573c58a3c352a967db7");
        assert_eq!(v["size"], 5);
        assert_eq!(p.content(), RemoteContent::Text("hello".into()));
    }

    #[test]
    fn image_paste_round_trips_its_file_list() {
        let p = PasteData::image(9, "me", "shot.jpg", b"jpegbytes");
        let back: PasteData = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        match back.content() {
            RemoteContent::Images(files) => {
                assert_eq!(files.len(), 1);
                assert_eq!(files[0].name, "shot.jpg");
                assert_eq!(files[0].size, 9);
                assert_eq!(files[0].hash, hash::hash(b"jpegbytes"));
            }
            other => panic!("unexpected {other:?}"),
        }
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(
            v["pasteAppearItem"]["identifiers"],
            json!([IMAGE_IDENTIFIER])
        );
    }

    #[test]
    fn html_paste_falls_back_to_collection_text() {
        let raw = json!({
            "id": 3, "appInstanceId": "peer", "favorite": false,
            "pasteAppearItem": {"type": "html", "identifiers": [], "hash": "h", "size": 3, "html": "<b>x</b>"},
            "pasteCollection": {"pasteItems": [{"type": "text", "identifiers": [], "hash": "t", "size": 1, "text": "x"}]},
            "pasteType": 2, "source": null, "size": 4, "hash": "h"
        });
        let p: PasteData = serde_json::from_value(raw).unwrap();
        assert_eq!(p.content(), RemoteContent::Text("x".into()));
    }

    #[test]
    fn received_relative_paths_use_the_last_component() {
        let raw = json!({
            "id": 3, "appInstanceId": "peer",
            "pasteAppearItem": {"type": "images", "identifiers": [IMAGE_IDENTIFIER], "count": 1, "hash": "h", "size": 2,
                "fileInfoTreeMap": {"a.png": {"type": "file", "size": 2, "hash": "fh"}},
                "relativePathList": ["peer/2026-10-16/3/a.png"]},
            "pasteType": 4, "size": 2, "hash": "h"
        });
        let p: PasteData = serde_json::from_value(raw).unwrap();
        assert!(matches!(p.content(), RemoteContent::Images(f) if f[0].name == "a.png"));
    }
}
