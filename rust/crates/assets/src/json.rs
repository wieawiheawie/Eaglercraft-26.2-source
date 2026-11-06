use std::collections::BTreeMap;

use serde_json::Value;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JsonAssetError {
    InvalidJson(String),
    InvalidModel(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockModelJson {
    pub parent: Option<String>,
    pub textures: BTreeMap<String, String>,
    pub elements: Vec<Value>,
}

/// Parses vanilla block/item model JSON while retaining unknown element data so
/// the Rust model compiler can add fields as newer Minecraft formats appear.
pub fn parse_block_model(bytes: &[u8]) -> Result<BlockModelJson, JsonAssetError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| JsonAssetError::InvalidJson(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| JsonAssetError::InvalidModel("model root is not an object".to_owned()))?;

    let parent = object
        .get("parent")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut textures = BTreeMap::new();
    if let Some(values) = object.get("textures").and_then(Value::as_object) {
        for (slot, texture) in values {
            let texture = texture
                .as_str()
                .ok_or_else(|| JsonAssetError::InvalidModel(format!("texture slot {slot} is not a string")))?;
            textures.insert(slot.clone(), texture.to_owned());
        }
    }
    let elements = object
        .get("elements")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    Ok(BlockModelJson {
        parent,
        textures,
        elements,
    })
}

/// Collects every model identifier referenced by a vanilla blockstate JSON
/// `variants` or `multipart` structure without depending on a fixed format
/// for the surrounding conditions.
pub fn parse_blockstate_model_ids(bytes: &[u8]) -> Result<Vec<String>, JsonAssetError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| JsonAssetError::InvalidJson(error.to_string()))?;
    let mut models = Vec::new();
    collect_models(&value, &mut models);
    models.sort();
    models.dedup();
    Ok(models)
}

fn collect_models(value: &Value, models: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(model) = object.get("model").and_then(Value::as_str) {
                models.push(model.to_owned());
            }
            for child in object.values() {
                collect_models(child, models);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_models(child, models);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_block_model, parse_blockstate_model_ids};

    #[test]
    fn parses_parent_textures_and_model_elements() {
        let source = br#"{
            "parent":"minecraft:block/cube_all",
            "textures":{"all":"minecraft:block/stone"},
            "elements":[{"from":[0,0,0],"to":[16,16,16]}]
        }"#;
        let model = parse_block_model(source).unwrap();
        assert_eq!(model.parent.as_deref(), Some("minecraft:block/cube_all"));
        assert_eq!(model.textures["all"], "minecraft:block/stone");
        assert_eq!(model.elements.len(), 1);
    }

    #[test]
    fn resolves_all_model_ids_in_variants_and_multipart() {
        let source = br#"{
            "variants":{"":{"model":"minecraft:block/cube"},"facing=north":{"model":"minecraft:block/north"}},
            "multipart":[{"apply":{"model":"minecraft:block/side"}},{"apply":[{"model":"minecraft:block/top"}]}]
        }"#;
        assert_eq!(
            parse_blockstate_model_ids(source).unwrap(),
            [
                "minecraft:block/cube",
                "minecraft:block/north",
                "minecraft:block/side",
                "minecraft:block/top"
            ]
        );
    }

    #[test]
    fn rejects_non_json_model_data() {
        assert!(parse_block_model(b"not json").is_err());
    }
}
