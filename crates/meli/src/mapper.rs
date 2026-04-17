use crate::types::MeliItem;
use search_core::{Document, Value};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MappingError {
    #[error("invalid item id '{0}': expected site prefix + numeric id (e.g. MLA1234)")]
    InvalidId(String),
    #[error("invalid category_id '{0}': expected site prefix + numeric id (e.g. MLA109027)")]
    InvalidCategoryId(String),
}

pub struct MeliMapper;

impl MeliMapper {
    /// Convert a MeliItem into a Document, stripping the site prefix from
    /// id and category_id (e.g. "MLA1136716168" → 1136716168).
    pub fn map(item: MeliItem) -> Result<Document, MappingError> {
        let id = strip_prefix(&item.id, &item.site_id)
            .ok_or_else(|| MappingError::InvalidId(item.id.clone()))?;

        let category = strip_prefix(&item.category_id, &item.site_id)
            .ok_or_else(|| MappingError::InvalidCategoryId(item.category_id.clone()))?
            .to_string();

        let mut attributes: HashMap<String, Value> = item
            .attributes
            .into_iter()
            .filter_map(|a| {
                let val = a.value_name?;
                Some((a.id.to_lowercase(), Value::String(val)))
            })
            .collect();

        if !item.condition.is_empty() {
            attributes.insert("condition".into(), Value::String(item.condition));
        }
        if !item.thumbnail.is_empty() {
            attributes.insert("thumbnail".into(), Value::String(item.thumbnail));
        }
        if !item.status.is_empty() {
            attributes.insert("status".into(), Value::String(item.status));
        }

        Ok(Document {
            id,
            title: item.title,
            description: String::new(),
            price: item.price,
            category,
            attributes,
        })
    }

    /// Map a batch, returning (ok, errors) counts.
    pub fn map_batch(items: Vec<MeliItem>) -> (Vec<Document>, Vec<MappingError>) {
        let mut docs = Vec::with_capacity(items.len());
        let mut errors = Vec::new();
        for item in items {
            match Self::map(item) {
                Ok(doc) => docs.push(doc),
                Err(e) => errors.push(e),
            }
        }
        (docs, errors)
    }
}

/// Strip the site prefix and parse the numeric suffix as u64.
/// "MLA1136716168" with site "MLA" → Some(1136716168)
fn strip_prefix(id: &str, site_id: &str) -> Option<u64> {
    id.strip_prefix(site_id)
        .and_then(|s| s.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MeliAttribute, MeliItem};

    fn make_item() -> MeliItem {
        MeliItem {
            id: "MLA1136716168".into(),
            site_id: "MLA".into(),
            title: "Zapatillas Avid Fof".into(),
            price: 15000.0,
            category_id: "MLA109027".into(),
            condition: "new".into(),
            thumbnail: "http://example.com/img.jpg".into(),
            status: "active".into(),
            attributes: vec![
                MeliAttribute {
                    id: "BRAND".into(),
                    name: "Marca".into(),
                    value_name: Some("Propia".into()),
                },
                MeliAttribute {
                    id: "GENDER".into(),
                    name: "Género".into(),
                    value_name: Some("Hombre".into()),
                },
                MeliAttribute {
                    id: "MODEL".into(),
                    name: "Modelo".into(),
                    value_name: None,
                },
            ],
        }
    }

    #[test]
    fn test_id_stripped() {
        let doc = MeliMapper::map(make_item()).unwrap();
        assert_eq!(doc.id, 1136716168u64);
    }

    #[test]
    fn test_category_stripped() {
        let doc = MeliMapper::map(make_item()).unwrap();
        assert_eq!(doc.category, "109027");
    }

    #[test]
    fn test_attributes_mapped() {
        let doc = MeliMapper::map(make_item()).unwrap();
        assert_eq!(doc.attributes.get("brand"), Some(&Value::String("Propia".into())));
        assert_eq!(doc.attributes.get("gender"), Some(&Value::String("Hombre".into())));
        // Attribute with no value_name is skipped
        assert!(!doc.attributes.contains_key("model"));
    }

    #[test]
    fn test_invalid_id() {
        let mut item = make_item();
        item.id = "MLAINVALID".into();
        assert!(matches!(MeliMapper::map(item), Err(MappingError::InvalidId(_))));
    }

    #[test]
    fn test_map_batch_partial_errors() {
        let good = make_item();
        let mut bad = make_item();
        bad.id = "MLAINVALID".into();
        let (docs, errors) = MeliMapper::map_batch(vec![good, bad]);
        assert_eq!(docs.len(), 1);
        assert_eq!(errors.len(), 1);
    }
}
