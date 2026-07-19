mod workspace;
mod workspace_lifecycle;
pub use workspace::*;
pub use workspace_lifecycle::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PastePayload {
    Text(String),
    Image(ImagePaste),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePaste {
    pub path: String,
    pub mime_type: String,
}

pub trait AiCliAdapter {
    fn name(&self) -> &'static str;
    fn supports_process(&self, process_name: &str) -> bool;
    fn format_image_paste(&self, image: &ImagePaste) -> String;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteRoute {
    Text(String),
    UnsupportedImage { path: String },
}

pub struct AdapterRegistry {
    adapters: Vec<Box<dyn AiCliAdapter + Send + Sync>>,
}

impl AdapterRegistry {
    pub fn new(adapters: Vec<Box<dyn AiCliAdapter + Send + Sync>>) -> Self {
        Self { adapters }
    }

    pub fn with_builtin_adapters() -> Self {
        Self::new(vec![
            Box::new(CodexCliAdapter),
            Box::new(ClaudeCliAdapter),
            Box::new(GenericFileReferenceAdapter),
        ])
    }

    pub fn route_paste(&self, process_name: &str, payload: &PastePayload) -> PasteRoute {
        match payload {
            PastePayload::Text(text) => PasteRoute::Text(text.clone()),
            PastePayload::Image(image) => self
                .adapters
                .iter()
                .find(|adapter| adapter.supports_process(process_name))
                .map(|adapter| PasteRoute::Text(adapter.format_image_paste(image)))
                .unwrap_or_else(|| PasteRoute::UnsupportedImage {
                    path: image.path.clone(),
                }),
        }
    }

    pub fn adapter_name_for_process(&self, process_name: &str) -> Option<&'static str> {
        self.adapters
            .iter()
            .find(|adapter| adapter.supports_process(process_name))
            .map(|adapter| adapter.name())
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CodexCliAdapter;

impl AiCliAdapter for CodexCliAdapter {
    fn name(&self) -> &'static str {
        "codex-cli"
    }

    fn supports_process(&self, process_name: &str) -> bool {
        process_name.eq_ignore_ascii_case("codex") || process_name.eq_ignore_ascii_case("codex.exe")
    }

    fn format_image_paste(&self, image: &ImagePaste) -> String {
        format!("Image file: {}\r", quote_path(&image.path))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ClaudeCliAdapter;

impl AiCliAdapter for ClaudeCliAdapter {
    fn name(&self) -> &'static str {
        "claude-cli"
    }

    fn supports_process(&self, process_name: &str) -> bool {
        process_name.eq_ignore_ascii_case("claude")
            || process_name.eq_ignore_ascii_case("claude.exe")
    }

    fn format_image_paste(&self, image: &ImagePaste) -> String {
        format!("Image file: {}\r", quote_path(&image.path))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct GenericFileReferenceAdapter;

impl AiCliAdapter for GenericFileReferenceAdapter {
    fn name(&self) -> &'static str {
        "generic-file-reference"
    }

    fn supports_process(&self, process_name: &str) -> bool {
        matches!(
            process_name.to_ascii_lowercase().as_str(),
            "cmd" | "cmd.exe" | "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
        )
    }

    fn format_image_paste(&self, image: &ImagePaste) -> String {
        format!("Image file: {}\r", quote_path(&image.path))
    }
}

fn quote_path(path: &str) -> String {
    if path.contains(' ') || path.contains('\t') {
        format!("\"{}\"", path.replace('"', "\\\""))
    } else {
        path.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MarkdownAdapter;

    impl AiCliAdapter for MarkdownAdapter {
        fn name(&self) -> &'static str {
            "markdown"
        }

        fn supports_process(&self, process_name: &str) -> bool {
            process_name == "example-ai-cli"
        }

        fn format_image_paste(&self, image: &ImagePaste) -> String {
            format!("![pasted image]({})", image.path)
        }
    }

    #[test]
    fn adapter_formats_image_reference() {
        let adapter = MarkdownAdapter;
        let image = ImagePaste {
            path: "C:/Temp/splice/image.png".to_owned(),
            mime_type: "image/png".to_owned(),
        };

        assert_eq!(adapter.name(), "markdown");
        assert!(adapter.supports_process("example-ai-cli"));
        assert_eq!(
            adapter.format_image_paste(&image),
            "![pasted image](C:/Temp/splice/image.png)"
        );
    }

    #[test]
    fn registry_routes_text_without_adapter_lookup() {
        let registry = AdapterRegistry::with_builtin_adapters();

        assert_eq!(
            registry.route_paste("unknown.exe", &PastePayload::Text("hello".to_owned())),
            PasteRoute::Text("hello".to_owned())
        );
    }

    #[test]
    fn registry_routes_codex_image_as_file_reference() {
        let registry = AdapterRegistry::with_builtin_adapters();
        let image = ImagePaste {
            path: "C:/Temp/splice pasted/image.png".to_owned(),
            mime_type: "image/png".to_owned(),
        };

        assert_eq!(
            registry.route_paste("codex.exe", &PastePayload::Image(image)),
            PasteRoute::Text("Image file: \"C:/Temp/splice pasted/image.png\"\r".to_owned())
        );
    }

    #[test]
    fn registry_reports_adapter_name_for_supported_process() {
        let registry = AdapterRegistry::with_builtin_adapters();

        assert_eq!(
            registry.adapter_name_for_process("codex.exe"),
            Some("codex-cli")
        );
        assert_eq!(registry.adapter_name_for_process("unknown.exe"), None);
    }

    #[test]
    fn registry_refuses_unknown_image_process_instead_of_guessing() {
        let registry = AdapterRegistry::new(Vec::new());
        let image = ImagePaste {
            path: "C:/Temp/image.png".to_owned(),
            mime_type: "image/png".to_owned(),
        };

        assert_eq!(
            registry.route_paste("unknown.exe", &PastePayload::Image(image)),
            PasteRoute::UnsupportedImage {
                path: "C:/Temp/image.png".to_owned()
            }
        );
    }
}
