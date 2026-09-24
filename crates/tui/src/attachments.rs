use super::*;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Attachment {
    /// A file name, or "screenshot".
    pub(crate) name: String,
    pub(crate) data_url: String,
    /// When the header gave one.
    pub(crate) size: Option<(u32, u32)>,
}

impl Attachment {
    pub(crate) fn from_clipboard(image: flashagent_tui::clipboard::ClipboardImage) -> Self {
        Self {
            name: "screenshot".to_string(),
            size: image.dimensions(),
            data_url: image.to_data_url(),
        }
    }

    /// `None` unless it points at an existing image.
    pub(crate) fn from_dropped_path(pasted: &str) -> Option<Self> {
        // Terminals quote a dropped path when it contains spaces.
        let raw = pasted.trim().trim_matches(['\'', '"']).trim();
        if raw.is_empty() || raw.contains('\n') {
            return None;
        }
        let path = std::path::Path::new(raw);
        let media_type = flashagent_tui::clipboard::image_media_type(path)?;
        let bytes = std::fs::read(path).ok()?;
        if bytes.is_empty() {
            return None;
        }
        Some(Self {
            name: path.file_name()?.to_string_lossy().to_string(),
            size: flashagent_tui::clipboard::image_dimensions(&bytes),
            data_url: format!(
                "data:{media_type};base64,{}",
                flashagent_tui::clipboard::base64_encode(&bytes)
            ),
        })
    }

    pub(crate) fn label(&self) -> String {
        self.labelled(None)
    }

    /// With this model's measured cost: Qwen charges by area, Gemma a flat rate,
    /// and the cost is what decides whether to resize.
    pub(crate) fn labelled(&self, cost: Option<flashagent_tui::image_cost::ImageCost>) -> String {
        let size = match self.size {
            Some((w, h)) => format!("{} {w}×{h}", self.name),
            None => return self.name.clone(),
        };
        match (cost, self.size) {
            (Some(cost), Some((w, h))) => format!("{size} · {}", cost.label(w, h)),
            _ => size,
        }
    }
}


/// Three one-token requests, cached for the life of the install.
pub(crate) fn maybe_measure_image_cost(
    costs: &flashagent_tui::image_cost::ImageCosts,
    in_flight: &mut Option<String>,
    model: &str,
    source: &Arc<BackendSource>,
    tx: &tokio::sync::mpsc::UnboundedSender<UiEvent>,
) {
    if costs.get(model).is_some() || in_flight.as_deref() == Some(model) {
        return;
    }
    *in_flight = Some(model.to_string());
    let source = source.clone();
    let tx = tx.clone();
    let model = model.to_string();
    tokio::spawn(async move {
        let probe = flashagent_tui::image_cost::probe_png();
        if let Some((per_pixel, fixed)) = source
            .measure_image_cost(
                &probe,
                flashagent_tui::image_cost::PROBE_WIDTH,
                flashagent_tui::image_cost::PROBE_HEIGHT,
            )
            .await
        {
            let _ = tx.send(UiEvent::ImageCost { model, per_pixel, fixed });
        }
    });
}

pub(crate) fn model_sees_images(source: &BackendSource, model: &str) -> bool {
    sees_images(source.discovery().as_ref(), model)
}

/// A model the server never mentioned is assumed to see: the server's own
/// error is better than a wrong warning.
pub(crate) fn sees_images(discovery: Option<&flashagent_llm::ServerDiscovery>, model: &str) -> bool {
    discovery
        .and_then(|d| d.model(model))
        .map(|m| m.supports_vision)
        .unwrap_or(true)
}
