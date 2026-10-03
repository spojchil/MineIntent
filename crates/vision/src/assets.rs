use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use image::{Rgba, RgbaImage};
use serde_json::Value;
use zip::ZipArchive;

use crate::{Block, Report};

/// Owns a read-only local client JAR. Loaded models and textures are cached.
pub struct Resources {
    archive: ZipArchive<File>,
    pub(crate) version: String,
    json: HashMap<String, Value>,
    models: HashMap<String, Value>,
    textures: HashMap<String, Arc<RgbaImage>>,
}

impl Resources {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let archive = ZipArchive::new(File::open(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let mut result = Self {
            archive,
            version: String::new(),
            json: HashMap::new(),
            models: HashMap::new(),
            textures: HashMap::new(),
        };
        result.version = result.json("version.json")?["id"]
            .as_str()
            .ok_or("client JAR has no version id")?
            .to_owned();
        Ok(result)
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    /// JAR 里自带的语言文件原文。原版 JAR 只带 `en_us`，其余语言在资源索引里。
    pub fn language_file(&mut self, code: &str) -> Result<Vec<u8>, String> {
        self.bytes(&format!("assets/minecraft/lang/{code}.json"))
    }

    fn bytes(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let file = self
            .archive
            .by_name(path)
            .map_err(|e| format!("{path}: {e}"))?;
        // Read archive entries without extracting executable code or paths.
        if file.size() > 16 * 1024 * 1024 {
            return Err(format!("resource too large: {path}"));
        }
        let mut bytes = Vec::new();
        file.take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }

    pub(crate) fn json(&mut self, path: &str) -> Result<Value, String> {
        if let Some(value) = self.json.get(path) {
            return Ok(value.clone());
        }
        let value: Value =
            serde_json::from_slice(&self.bytes(path)?).map_err(|e| format!("{path}: {e}"))?;
        self.json.insert(path.to_owned(), value.clone());
        Ok(value)
    }

    pub(crate) fn model(&mut self, id: &str, chain: &mut Vec<String>) -> Result<Value, String> {
        if let Some(model) = self.models.get(id) {
            return Ok(model.clone());
        }
        if chain.len() >= 32 || chain.iter().any(|p| p == id) {
            return Err(format!("cyclic/deep model parent: {id}"));
        }
        chain.push(id.to_owned());
        let child = self.json(&resource_path(id, "models", "json")?)?;
        // `builtin/generated`、`builtin/entity` 是代码里的根，不是文件：到此为止。
        let builtin = |parent: &str| {
            parent
                .strip_prefix("minecraft:")
                .unwrap_or(parent)
                .starts_with("builtin/")
        };
        let mut model = if let Some(parent) = child["parent"].as_str().filter(|p| !builtin(p)) {
            self.model(parent, chain)?
        } else {
            serde_json::json!({})
        };
        let mut textures = model["textures"].as_object().cloned().unwrap_or_default();
        if let Some(overrides) = child["textures"].as_object() {
            textures.extend(overrides.clone());
        }
        if let Some(object) = child.as_object() {
            for (key, value) in object {
                model[key] = value.clone();
            }
        }
        model["textures"] = Value::Object(textures);
        chain.pop();
        self.models.insert(id.to_owned(), model.clone());
        Ok(model)
    }

    pub(crate) fn texture(&mut self, id: &str) -> Result<Arc<RgbaImage>, String> {
        if let Some(texture) = self.textures.get(id) {
            return Ok(texture.clone());
        }
        let bytes = self.bytes(&resource_path(id, "textures", "png")?)?;
        let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
            .map_err(|e| format!("{id}: {e}"))?
            .into_rgba8();
        let texture = Arc::new(image);
        self.textures.insert(id.to_owned(), texture.clone());
        Ok(texture)
    }

    pub(crate) fn selections(
        &mut self,
        block: &Block,
        report: &mut Report,
    ) -> Result<Vec<Value>, String> {
        let state = self.json(&resource_path(&block.name, "blockstates", "json")?)?;
        let mut result = Vec::new();
        if let Some(variants) = state["variants"].as_object() {
            for (condition, value) in variants {
                if condition.is_empty()
                    || condition.split(',').all(|pair| {
                        pair.split_once('=')
                            .is_some_and(|(key, value)| property_matches(block, key, value))
                    })
                {
                    result.push(first_variant(value, report));
                    break;
                }
            }
        }
        if let Some(parts) = state["multipart"].as_array() {
            for part in parts {
                if part.get("when").is_none_or(|v| matches_condition(v, block)) {
                    result.push(first_variant(&part["apply"], report));
                }
            }
        }
        if result.is_empty() {
            return Err(format!(
                "no model matched {} {:?}",
                block.name, block.properties
            ));
        }
        Ok(result)
    }
}

fn first_variant(value: &Value, report: &mut Report) -> Value {
    if let Some(values) = value.as_array() {
        report
            .warnings
            .insert("weighted model alternatives use the first entry".to_owned());
        values.first().cloned().unwrap_or(Value::Null)
    } else {
        value.clone()
    }
}

pub(crate) fn matches_condition(value: &Value, block: &Block) -> bool {
    value.as_object().is_some_and(|object| {
        object.iter().all(|(key, v)| match key.as_str() {
            "OR" => v
                .as_array()
                .is_some_and(|a| a.iter().any(|p| matches_condition(p, block))),
            "AND" => v
                .as_array()
                .is_some_and(|a| a.iter().all(|p| matches_condition(p, block))),
            _ => v.as_str().is_some_and(|v| property_matches(block, key, v)),
        })
    })
}

fn property_matches(block: &Block, key: &str, value: &str) -> bool {
    let Some(actual) = block.properties.get(key) else {
        return false;
    };
    if let Some(negated) = value.strip_prefix('!') {
        !negated.split('|').any(|v| v == actual)
    } else {
        value.split('|').any(|v| v == actual)
    }
}

pub(crate) fn resource_path(id: &str, category: &str, extension: &str) -> Result<String, String> {
    let (namespace, name) = id.split_once(':').unwrap_or(("minecraft", id));
    if namespace.is_empty()
        || name.is_empty()
        || id.contains("..")
        || id.contains('\\')
        || name.starts_with('/')
    {
        return Err(format!("invalid resource identifier: {id}"));
    }
    Ok(format!("assets/{namespace}/{category}/{name}.{extension}"))
}

pub(crate) fn texture_id(model: &Value, initial: &str) -> Result<String, String> {
    let mut value = initial;
    for _ in 0..32 {
        let Some(slot) = value.strip_prefix('#') else {
            return Ok(value.to_owned());
        };
        let material = &model["textures"][slot];
        value = material
            .as_str()
            .or_else(|| material["sprite"].as_str())
            .ok_or_else(|| format!("missing texture slot {slot}"))?;
    }
    Err("cyclic texture reference".to_owned())
}

pub(crate) fn missing_texture() -> Arc<RgbaImage> {
    Arc::new(RgbaImage::from_fn(16, 16, |x, y| {
        if (x / 8 + y / 8) % 2 == 0 {
            Rgba([240, 0, 240, 255])
        } else {
            Rgba([25, 25, 25, 255])
        }
    }))
}
