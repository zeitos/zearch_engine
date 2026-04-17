use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io;
use std::path::Path;

// ---------------------------------------------------------------------------
// Column types
// ---------------------------------------------------------------------------

/// Dictionary-encoded keyword column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeywordColumn {
    /// Maps value string to ordinal.
    pub dict: Vec<String>,
    /// Per-document ordinal (index = local doc_id, value = ordinal in dict).
    /// Uses u32::MAX as sentinel for missing values.
    pub ordinals: Vec<u32>,
}

/// Numeric (f64) column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NumericColumn {
    /// Per-document value (index = local doc_id).
    pub values: Vec<f64>,
    /// Bitset of docs that have a value (for sparse columns).
    pub present: Vec<bool>,
}

// ---------------------------------------------------------------------------
// Writers
// ---------------------------------------------------------------------------

/// Builds a keyword column incrementally.
pub struct KeywordColumnWriter {
    value_to_ordinal: HashMap<String, u32>,
    dict: Vec<String>,
    ordinals: Vec<u32>,
}

impl KeywordColumnWriter {
    pub fn new() -> Self {
        Self {
            value_to_ordinal: HashMap::new(),
            dict: Vec::new(),
            ordinals: Vec::new(),
        }
    }

    pub fn add(&mut self, value: &str) {
        let ordinal = if let Some(&ord) = self.value_to_ordinal.get(value) {
            ord
        } else {
            let ord = self.dict.len() as u32;
            self.dict.push(value.to_string());
            self.value_to_ordinal.insert(value.to_string(), ord);
            ord
        };
        self.ordinals.push(ordinal);
    }

    pub fn add_missing(&mut self) {
        self.ordinals.push(u32::MAX);
    }

    pub fn build(self) -> KeywordColumn {
        KeywordColumn {
            dict: self.dict,
            ordinals: self.ordinals,
        }
    }
}

impl Default for KeywordColumnWriter {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds a numeric column incrementally.
pub struct NumericColumnWriter {
    values: Vec<f64>,
    present: Vec<bool>,
}

impl NumericColumnWriter {
    pub fn new() -> Self {
        Self {
            values: Vec::new(),
            present: Vec::new(),
        }
    }

    pub fn add(&mut self, value: f64) {
        self.values.push(value);
        self.present.push(true);
    }

    pub fn add_missing(&mut self) {
        self.values.push(0.0);
        self.present.push(false);
    }

    pub fn build(self) -> NumericColumn {
        NumericColumn {
            values: self.values,
            present: self.present,
        }
    }
}

impl Default for NumericColumnWriter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Column store (collection of columns)
// ---------------------------------------------------------------------------

/// Column store for a segment: named columns for filtering and aggregation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ColumnStore {
    pub keyword_columns: HashMap<String, KeywordColumn>,
    pub numeric_columns: HashMap<String, NumericColumn>,
}

impl ColumnStore {
    pub fn write(&self, dir: &Path) -> io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let data = bincode::serialize(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        std::fs::write(dir.join("columns.bin"), data)
    }

    pub fn read(dir: &Path) -> io::Result<Self> {
        let data = std::fs::read(dir.join("columns.bin"))?;
        bincode::deserialize(&data)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    // -- Filter operations --

    /// Equality filter on a keyword column.
    pub fn filter_eq(&self, field: &str, value: &str) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        if let Some(col) = self.keyword_columns.get(field) {
            if let Some(&target_ord) = col.dict.iter().position(|v| v == value).as_ref() {
                let target_ord = target_ord as u32;
                for (doc_id, &ord) in col.ordinals.iter().enumerate() {
                    if ord == target_ord {
                        result.insert(doc_id as u32);
                    }
                }
            }
        }
        result
    }

    /// Multi-value IN filter on a keyword column.
    pub fn filter_in(&self, field: &str, values: &[String]) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        if let Some(col) = self.keyword_columns.get(field) {
            let target_ords: Vec<u32> = values
                .iter()
                .filter_map(|v| col.dict.iter().position(|d| d == v).map(|p| p as u32))
                .collect();
            for (doc_id, &ord) in col.ordinals.iter().enumerate() {
                if target_ords.contains(&ord) {
                    result.insert(doc_id as u32);
                }
            }
        }
        result
    }

    /// Range filter on a numeric column.
    pub fn filter_range(&self, field: &str, gte: Option<f64>, lte: Option<f64>) -> RoaringBitmap {
        let mut result = RoaringBitmap::new();
        if let Some(col) = self.numeric_columns.get(field) {
            for (doc_id, (&value, &present)) in
                col.values.iter().zip(col.present.iter()).enumerate()
            {
                if !present {
                    continue;
                }
                let passes_gte = gte.map_or(true, |min| value >= min);
                let passes_lte = lte.map_or(true, |max| value <= max);
                if passes_gte && passes_lte {
                    result.insert(doc_id as u32);
                }
            }
        }
        result
    }

    // -- Aggregation operations --

    /// Count aggregation on a keyword column, restricted to matching docs.
    pub fn aggregate_counts(
        &self,
        field: &str,
        matching_docs: &RoaringBitmap,
    ) -> Vec<(String, u64)> {
        let Some(col) = self.keyword_columns.get(field) else {
            return vec![];
        };

        let mut counts = vec![0u64; col.dict.len()];

        for doc_id in matching_docs.iter() {
            let idx = doc_id as usize;
            if idx < col.ordinals.len() {
                let ord = col.ordinals[idx];
                if (ord as usize) < counts.len() {
                    counts[ord as usize] += 1;
                }
            }
        }

        col.dict
            .iter()
            .enumerate()
            .filter(|(i, _)| counts[*i] > 0)
            .map(|(i, v)| (v.clone(), counts[i]))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_test_store() -> ColumnStore {
        let mut cat_writer = KeywordColumnWriter::new();
        cat_writer.add("electronics");  // doc 0
        cat_writer.add("electronics");  // doc 1
        cat_writer.add("clothing");     // doc 2
        cat_writer.add("electronics");  // doc 3
        cat_writer.add("clothing");     // doc 4

        let mut price_writer = NumericColumnWriter::new();
        price_writer.add(799.99);  // doc 0
        price_writer.add(299.99);  // doc 1
        price_writer.add(49.99);   // doc 2
        price_writer.add(999.99);  // doc 3
        price_writer.add(29.99);   // doc 4

        let mut brand_writer = KeywordColumnWriter::new();
        brand_writer.add("samsung");  // doc 0
        brand_writer.add("apple");    // doc 1
        brand_writer.add("nike");     // doc 2
        brand_writer.add("samsung");  // doc 3
        brand_writer.add("adidas");   // doc 4

        let mut store = ColumnStore::default();
        store.keyword_columns.insert("category".into(), cat_writer.build());
        store.keyword_columns.insert("brand".into(), brand_writer.build());
        store.numeric_columns.insert("price".into(), price_writer.build());
        store
    }

    #[test]
    fn test_filter_eq() {
        let store = build_test_store();
        let result = store.filter_eq("category", "electronics");
        assert_eq!(result.len(), 3);
        assert!(result.contains(0));
        assert!(result.contains(1));
        assert!(result.contains(3));
    }

    #[test]
    fn test_filter_eq_no_match() {
        let store = build_test_store();
        let result = store.filter_eq("category", "food");
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_in() {
        let store = build_test_store();
        let result = store.filter_in("brand", &["samsung".into(), "apple".into()]);
        assert_eq!(result.len(), 3); // docs 0, 1, 3
        assert!(result.contains(0));
        assert!(result.contains(1));
        assert!(result.contains(3));
    }

    #[test]
    fn test_filter_range() {
        let store = build_test_store();

        // price >= 100 AND price <= 800
        let result = store.filter_range("price", Some(100.0), Some(800.0));
        assert_eq!(result.len(), 2); // docs 0 (799.99) and 1 (299.99)
        assert!(result.contains(0));
        assert!(result.contains(1));
    }

    #[test]
    fn test_filter_range_open() {
        let store = build_test_store();

        // price >= 500 (no upper bound)
        let result = store.filter_range("price", Some(500.0), None);
        assert_eq!(result.len(), 2); // docs 0 (799.99), 3 (999.99)
    }

    #[test]
    fn test_filter_combination() {
        let store = build_test_store();

        let cat = store.filter_eq("category", "electronics");
        let price = store.filter_range("price", Some(500.0), None);
        let combined = cat & price; // AND

        assert_eq!(combined.len(), 2); // docs 0 and 3
        assert!(combined.contains(0));
        assert!(combined.contains(3));
    }

    #[test]
    fn test_aggregate_counts() {
        let store = build_test_store();

        // All docs
        let mut all = RoaringBitmap::new();
        for i in 0..5 {
            all.insert(i);
        }

        let counts = store.aggregate_counts("category", &all);
        let electronics = counts.iter().find(|(v, _)| v == "electronics").unwrap();
        let clothing = counts.iter().find(|(v, _)| v == "clothing").unwrap();
        assert_eq!(electronics.1, 3);
        assert_eq!(clothing.1, 2);
    }

    #[test]
    fn test_aggregate_counts_with_filter() {
        let store = build_test_store();

        // Only expensive items
        let expensive = store.filter_range("price", Some(200.0), None);
        let counts = store.aggregate_counts("brand", &expensive);

        let samsung = counts.iter().find(|(v, _)| v == "samsung").unwrap();
        assert_eq!(samsung.1, 2); // docs 0, 3
    }

    #[test]
    fn test_column_store_roundtrip() {
        let store = build_test_store();
        let dir = tempfile::TempDir::new().unwrap();
        store.write(dir.path()).unwrap();
        let loaded = ColumnStore::read(dir.path()).unwrap();

        let result = loaded.filter_eq("category", "electronics");
        assert_eq!(result.len(), 3);
    }
}
