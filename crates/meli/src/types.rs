use serde::Deserialize;

/// MercadoLibre item as returned by the Items API.
/// Fields not needed for indexing are skipped via `#[serde(default)]`.
#[derive(Debug, Deserialize)]
pub struct MeliItem {
    pub id: String,
    pub site_id: String,
    pub title: String,
    pub price: f64,
    pub category_id: String,
    #[serde(default)]
    pub condition: String,
    #[serde(default)]
    pub attributes: Vec<MeliAttribute>,
    #[serde(default)]
    pub thumbnail: String,
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct MeliAttribute {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub value_name: Option<String>,
}
