use settings::{RegisterSetting, Settings};

#[derive(Clone, Debug, RegisterSetting)]
pub struct MarkdownEditorSettings {
    pub default_open_mode: Option<settings::MarkdownDefaultOpenMode>,
    pub mode: settings::MarkdownEditorMode,
    pub image_save_folder: String,
    pub use_app_theme_colors: bool,
    pub show_line_numbers: bool,
    pub default_open_outline: bool,
    pub custom_css: String,
    pub content_theme: Option<String>,
    pub code_theme: Option<String>,
    pub plantuml_service: Option<String>,
}

impl Settings for MarkdownEditorSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let content = content.markdown_editor.clone().unwrap_or_default();
        Self {
            default_open_mode: content.default_open_mode,
            mode: content.mode.unwrap_or_default(),
            image_save_folder: content.image_save_folder.unwrap_or_else(|| "assets".into()),
            use_app_theme_colors: content.use_app_theme_colors.unwrap_or(true),
            show_line_numbers: content.show_line_numbers.unwrap_or(true),
            default_open_outline: content.default_open_outline.unwrap_or(false),
            custom_css: content.custom_css.unwrap_or_default(),
            content_theme: content.content_theme,
            code_theme: content.code_theme,
            plantuml_service: content.plantuml_service,
        }
    }
}
