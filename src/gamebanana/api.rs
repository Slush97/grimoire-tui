//! The shapes of GameBanana's apiv11 replies that Browse reads.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub const API: &str = "https://gamebanana.com/apiv11";
pub const DEADLOCK: &str = "20948";
pub const LIST_FIELDS: &str = "_idRow,_sName,_sProfileUrl,_tsDateAdded,_tsDateModified,\
                               _nLikeCount,_nViewCount,_nDownloadCount,_bIsNsfw,\
                               _bHasContentRatings,_aSubmitter,_aPreviewMedia,_aRootCategory";
pub const DETAIL_FIELDS: &str = "_sText,_aFiles,_aCategory,_bIsNsfw";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Liked,
    Newest,
    Updated,
    Viewed,
}

impl Sort {
    pub fn next(self) -> Self {
        match self {
            Self::Liked => Self::Newest,
            Self::Newest => Self::Updated,
            Self::Updated => Self::Viewed,
            Self::Viewed => Self::Liked,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Liked => "most liked",
            Self::Newest => "newest",
            Self::Updated => "recently updated",
            Self::Viewed => "most viewed",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Self::Liked => "Generic_MostLiked",
            Self::Newest => "Generic_Newest",
            Self::Updated => "Generic_LatestModified",
            Self::Viewed => "Generic_MostViewed",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Submission {
    #[serde(rename = "_idRow")]
    pub id: u64,
    #[serde(rename = "_sName")]
    pub name: String,
    #[serde(rename = "_sProfileUrl", default)]
    pub url: String,
    #[serde(rename = "_nLikeCount", default)]
    pub likes: u64,
    #[serde(rename = "_nViewCount", default)]
    pub views: u64,
    #[serde(rename = "_nDownloadCount", default)]
    pub downloads: u64,
    #[serde(rename = "_tsDateAdded", default)]
    pub added: i64,
    #[serde(rename = "_tsDateModified", default)]
    pub modified: i64,
    #[serde(rename = "_bIsNsfw", default)]
    is_nsfw: Option<bool>,
    #[serde(rename = "_bHasContentRatings", default)]
    has_ratings: Option<bool>,
    #[serde(rename = "_aSubmitter", default, deserialize_with = "lenient")]
    submitter: Option<Named>,
    #[serde(rename = "_aRootCategory", default, deserialize_with = "lenient")]
    category: Option<Named>,
    #[serde(rename = "_aPreviewMedia", default, deserialize_with = "lenient")]
    preview: Option<Preview>,
}

impl Submission {
    /// List results often leave `_bIsNsfw` out, and content ratings are the
    /// reliable stand-in.
    pub fn nsfw(&self) -> bool {
        self.is_nsfw == Some(true) || self.has_ratings == Some(true)
    }

    pub fn author(&self) -> &str {
        self.submitter.as_ref().map_or("", |s| s.name.as_str())
    }

    pub fn category(&self) -> &str {
        self.category.as_ref().map_or("", |c| c.name.as_str())
    }

    pub fn thumbnail(&self) -> Option<String> {
        let image = self.preview.as_ref()?.images.first()?;
        let file = image.file530.as_ref().unwrap_or(&image.file);
        Some(format!("{}/{file}", image.base))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Details {
    #[serde(rename = "_aFiles", default)]
    pub files: Vec<ModFile>,
    /// The leaf category, which for skins is the hero.
    #[serde(rename = "_aCategory", default, deserialize_with = "lenient")]
    category: Option<Named>,
    #[serde(rename = "_bIsNsfw", default)]
    pub nsfw: Option<bool>,
    /// HTML.
    #[serde(rename = "_sText", default)]
    pub text: String,
}

impl Details {
    pub fn category(&self) -> Option<&str> {
        self.category.as_ref().map(|c| c.name.as_str())
    }

    /// Files to offer, current ones first. Archived files are old versions;
    /// they only show when there's nothing else.
    pub fn offered(&self) -> Vec<ModFile> {
        let current: Vec<ModFile> = self.files.iter().filter(|f| !f.archived).cloned().collect();
        if current.is_empty() {
            self.files.clone()
        } else {
            current
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModFile {
    #[serde(rename = "_idRow")]
    pub id: u64,
    #[serde(rename = "_sFile")]
    pub name: String,
    #[serde(rename = "_nFilesize", default)]
    pub size: u64,
    #[serde(rename = "_sDescription", default)]
    pub description: String,
    #[serde(rename = "_nDownloadCount", default)]
    pub downloads: u64,
    #[serde(rename = "_tsDateAdded", default)]
    pub added: i64,
    #[serde(rename = "_bIsArchived", default)]
    pub archived: bool,
    #[serde(rename = "_sDownloadUrl")]
    pub url: String,
}

/// A node of the Mod category tree: Skins, and under it each hero.
#[derive(Debug, Clone, Deserialize)]
pub struct Category {
    #[serde(rename = "_idRow")]
    pub id: u64,
    #[serde(rename = "_sName")]
    pub name: String,
    #[serde(rename = "_nItemCount", default)]
    pub items: u64,
    #[serde(rename = "_aChildren", default, deserialize_with = "lenient_list")]
    pub children: Vec<Category>,
}

#[derive(Debug, Clone, Deserialize)]
struct Named {
    #[serde(rename = "_sName")]
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct Preview {
    #[serde(rename = "_aImages", default)]
    images: Vec<Image>,
}

#[derive(Debug, Clone, Deserialize)]
struct Image {
    #[serde(rename = "_sBaseUrl")]
    base: String,
    #[serde(rename = "_sFile")]
    file: String,
    #[serde(rename = "_sFile530")]
    file530: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Page {
    #[serde(rename = "_aRecords", default)]
    pub records: Vec<Submission>,
    #[serde(rename = "_aMetadata")]
    pub meta: PageMeta,
}

#[derive(Debug, Deserialize)]
pub struct PageMeta {
    #[serde(rename = "_bIsComplete", default)]
    pub complete: bool,
}

/// GameBanana sends an empty object as `[]`, so a nested field that doesn't
/// parse is treated as missing instead of failing the whole page.
fn lenient<'de, D: Deserializer<'de>, T: DeserializeOwned>(d: D) -> Result<Option<T>, D::Error> {
    Ok(serde_json::from_value(Value::deserialize(d)?).ok())
}

/// A list that may come as an object keyed by index, or not at all.
pub fn lenient_list<'de, D: Deserializer<'de>, T: DeserializeOwned>(
    d: D,
) -> Result<Vec<T>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Array(items) => items,
        Value::Object(map) => map.into_iter().map(|(_, v)| v).collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter_map(|v| serde_json::from_value(v).ok())
    .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submission_survives_empty_objects_as_arrays() {
        let json = r#"{"_idRow": 7, "_sName": "Blue Haze", "_aSubmitter": [],
            "_aPreviewMedia": {"_aImages": [{"_sBaseUrl": "https://img", "_sFile": "a.jpg", "_sFile530": "530-a.jpg"}]},
            "_bHasContentRatings": true}"#;
        let s: Submission = serde_json::from_str(json).unwrap();
        assert_eq!(s.author(), "");
        assert!(s.nsfw());
        assert_eq!(s.thumbnail().as_deref(), Some("https://img/530-a.jpg"));
    }

    #[test]
    fn categories_nest() {
        let json = r#"[{"_idRow": 1, "_sName": "Skins", "_nItemCount": 3,
            "_aChildren": [{"_idRow": 2, "_sName": "Haze"}]}, {"_idRow": 3, "_sName": "HUD", "_aChildren": []}]"#;
        let tree: Vec<Category> = serde_json::from_str(json).unwrap();
        assert_eq!(tree[0].children[0].name, "Haze");
        assert!(tree[1].children.is_empty());
    }
}
