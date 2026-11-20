use serde::{Deserialize, Serialize};

/// Policy for reducing the active model context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CompactionMode {
    /// Compact when context hits a fixed threshold (default).
    #[default]
    Reactive,
    /// Compact early based on predicted token growth rate.
    Proactive,
    /// Compact based on semantic topic shifts and relevance scoring.
    Semantic,
    /// Let the model fold and restore context using native tools.
    #[serde(rename = "model_driven", alias = "fold")]
    ModelDriven,
}

impl CompactionMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reactive => "reactive",
            Self::Proactive => "proactive",
            Self::Semantic => "semantic",
            Self::ModelDriven => "model_driven",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "reactive" => Some(Self::Reactive),
            "proactive" => Some(Self::Proactive),
            "semantic" => Some(Self::Semantic),
            "model_driven" | "fold" => Some(Self::ModelDriven),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CompactionMode;

    #[test]
    fn modes_round_trip_with_canonical_names() {
        for mode in [
            CompactionMode::Reactive,
            CompactionMode::Proactive,
            CompactionMode::Semantic,
            CompactionMode::ModelDriven,
        ] {
            let serialized = serde_json::to_value(&mode).unwrap();
            assert_eq!(serialized, mode.as_str());
            assert_eq!(
                serde_json::from_value::<CompactionMode>(serialized).unwrap(),
                mode
            );
            assert_eq!(CompactionMode::parse(mode.as_str()), Some(mode));
        }
    }

    #[test]
    fn model_driven_aliases_and_default() {
        assert_eq!(
            CompactionMode::parse(" FOLD "),
            Some(CompactionMode::ModelDriven)
        );
        assert_eq!(
            CompactionMode::parse(" MODEL_DRIVEN "),
            Some(CompactionMode::ModelDriven)
        );
        assert_eq!(
            serde_json::from_str::<CompactionMode>("\"fold\"").unwrap(),
            CompactionMode::ModelDriven
        );
        assert_eq!(CompactionMode::default(), CompactionMode::Reactive);
        assert_eq!(CompactionMode::parse("modeldriven"), None);
        let config: crate::CompactionConfig =
            serde_json::from_str(r#"{"mode":"model_driven"}"#).unwrap();
        assert_eq!(config.mode, CompactionMode::ModelDriven);
        assert_eq!(crate::CompactionConfig::default().mode, CompactionMode::Reactive);
    }
}
