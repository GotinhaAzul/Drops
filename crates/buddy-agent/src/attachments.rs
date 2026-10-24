//! Keep pixels as multimodal content, never as base64 inside prompt text.
use serde_json::{json, Value};
const CONTEXT_PREFIX: &str = "Context for this request (JSON):\n";

pub(crate) fn context_message(context: &Value, vision: bool) -> Value {
    let mut metadata = context.clone();
    let image = metadata
        .pointer_mut("/content/image")
        .and_then(Value::as_object_mut)
        .and_then(|image| image.remove("data_url"));
    let image = image
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|url| {
            vision
                && url.len() <= 8 * 1024 * 1024
                && [
                    "data:image/png;base64,",
                    "data:image/jpeg;base64,",
                    "data:image/webp;base64,",
                ]
                .iter()
                .any(|p| url.starts_with(p))
        });
    if let Some(meta) = metadata
        .pointer_mut("/content/image")
        .and_then(Value::as_object_mut)
    {
        meta.insert("attached".into(), json!(image.is_some()));
    }
    let text = format!(
        "{CONTEXT_PREFIX}{}",
        serde_json::to_string_pretty(&metadata).unwrap_or_default()
    );
    match image {
        Some(url) => {
            json!({"role":"user","content":[{"type":"text","text":text},{"type":"image_url","image_url":{"url":url}}]})
        }
        None => json!({"role":"user","content":text}),
    }
}

/// True for a context bundle message recorded by an earlier turn.
///
/// The engine keeps these in the JSONL log for export and inspection, but
/// omits them from model history when a fresh bundle exists so an old window
/// snapshot cannot dominate a follow-up request.
pub(crate) fn is_context_message(message: &Value) -> bool {
    let text = match message.get("content") {
        Some(Value::String(text)) => Some(text.as_str()),
        Some(Value::Array(parts)) => parts
            .iter()
            .find(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|part| part.get("text").and_then(Value::as_str)),
        _ => None,
    };
    text.is_some_and(|text| text.starts_with(CONTEXT_PREFIX))
}

/// A provider switch must not forward historical image parts to a text model.
pub(crate) fn for_provider(mut message: Value, vision: bool) -> Value {
    if !vision {
        if let Some(parts) = message.get("content").and_then(Value::as_array) {
            let text = parts
                .iter()
                .filter(|p| p["type"] == "text")
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            message["content"] = json!(text);
        }
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    fn captured() -> Value {
        json!({"window":{"title":"test"},"content":{"text":"cargo bild\nunknown command", "image":{"mime_type":"image/png","width":8,"height":8,"data_url":"data:image/png;base64,aGVsbG8="}}})
    }
    #[test]
    fn vision_uses_real_image_parts_and_readable_metadata() {
        let message = context_message(&captured(), true);
        let text = message["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("cargo bild"));
        assert!(!text.contains("aGVsbG8="));
        assert_eq!(
            message["content"][1]["image_url"]["url"],
            "data:image/png;base64,aGVsbG8="
        );
    }
    #[test]
    fn text_models_never_receive_base64_pixels() {
        let message = context_message(&captured(), false);
        assert!(message["content"].is_string());
        assert!(!message.to_string().contains("aGVsbG8="));
    }
    #[test]
    fn switching_to_text_strips_images_from_history() {
        let message = for_provider(context_message(&captured(), true), false);
        assert!(message["content"].is_string());
        assert!(!message.to_string().contains("aGVsbG8="));
        assert!(message["content"]
            .as_str()
            .unwrap()
            .contains("unknown command"));
    }
}
