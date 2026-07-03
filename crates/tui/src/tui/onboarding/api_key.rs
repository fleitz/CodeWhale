//! API key entry screen for onboarding.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::localization::MessageId;
use crate::palette;
use crate::tui::app::App;

pub fn lines(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(Span::styled(
            app.tr(MessageId::OnboardApiKeyTitle).to_string(),
            Style::default()
                .fg(palette::DEEPSEEK_SKY)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.tr(MessageId::OnboardApiKeyStep1).to_string(),
            Style::default().fg(palette::TEXT_PRIMARY),
        )),
        Line::from(Span::styled(
            app.tr(MessageId::OnboardApiKeyStep2).to_string(),
            Style::default().fg(palette::TEXT_PRIMARY),
        )),
        Line::from(""),
        Line::from(Span::styled(
            app.tr(MessageId::OnboardApiKeySavedHint).to_string(),
            Style::default().fg(palette::TEXT_MUTED),
        )),
        Line::from(Span::styled(
            app.tr(MessageId::OnboardApiKeyFormatHint).to_string(),
            Style::default().fg(palette::TEXT_MUTED),
        )),
        Line::from(""),
    ];

    let masked = mask_key(&app.api_key_input);
    let placeholder = app.tr(MessageId::OnboardApiKeyPlaceholder).to_string();
    let display = if masked.is_empty() {
        placeholder
    } else {
        masked
    };
    lines.push(Line::from(vec![
        Span::styled(
            app.tr(MessageId::OnboardApiKeyLabel).to_string(),
            Style::default().fg(palette::TEXT_MUTED),
        ),
        Span::styled(
            display,
            Style::default()
                .fg(palette::TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));

    if let Some(message) = app.status_message.as_deref() {
        lines.push(Line::from(Span::styled(
            message.to_string(),
            Style::default().fg(palette::STATUS_WARNING),
        )));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        app.tr(MessageId::OnboardApiKeyFooter).to_string(),
        Style::default().fg(palette::TEXT_MUTED),
    )));

    lines
}

fn mask_key(input: &str) -> String {
    let trimmed = input.trim();
    let len = trimmed.chars().count();
    if len == 0 {
        return String::new();
    }
    if len <= 4 {
        return "*".repeat(len);
    }
    let visible: String = trimmed
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!("{}{}", "*".repeat(len - 4), visible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SavedCredential};
    use crate::localization::Locale;
    use crate::test_support::{EnvVarGuard, lock_test_env};
    use crate::tui::app::TuiOptions;
    use std::path::PathBuf;

    fn test_app_with_locale(locale: Locale) -> App {
        let options = TuiOptions {
            model: "deepseek-v4-pro".to_string(),
            workspace: PathBuf::from("."),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            use_alt_screen: true,
            use_mouse_capture: false,
            use_bracketed_paste: true,
            max_subagents: 1,
            skills_dir: PathBuf::from("."),
            memory_path: PathBuf::from("memory.md"),
            notes_path: PathBuf::from("notes.txt"),
            mcp_config_path: PathBuf::from("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
        };
        let mut app = App::new(options, &Config::default());
        app.ui_locale = locale;
        app
    }

    #[test]
    fn api_key_screen_renders_in_selected_locale() {
        // The most-visible regression of the missing onboarding-localization:
        // after the user picks 简体中文 at step 2, step 3 used to remain
        // English. Pin that the rendered lines actually contain the
        // translated strings for each locale we ship.
        let zh = test_app_with_locale(Locale::ZhHans);
        let body: String = lines(&zh)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("DeepSeek API"), "title carries DeepSeek API");
        assert!(
            body.contains("密钥"),
            "expected zh-Hans 'key' label, got: {body}"
        );
        assert!(
            body.contains("Enter 保存"),
            "expected zh-Hans footer, got: {body}"
        );

        let ja = test_app_with_locale(Locale::Ja);
        let body: String = lines(&ja)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("キー"),
            "expected ja 'key' label, got: {body}"
        );

        let en = test_app_with_locale(Locale::En);
        let body: String = lines(&en)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("Press Enter to save"),
            "expected en footer, got: {body}"
        );
    }

    #[test]
    fn api_key_screen_and_save_status_use_active_config_path() {
        let _lock = lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let codewhale_home = temp.path().join("codewhale-home");
        let config_path = temp.path().join("managed").join("custom-config.toml");
        std::fs::create_dir_all(&home).expect("home dir");

        let _home = EnvVarGuard::set("HOME", home.as_os_str());
        let _codewhale_home = EnvVarGuard::set("CODEWHALE_HOME", codewhale_home.as_os_str());
        let _config_path = EnvVarGuard::set("CODEWHALE_CONFIG_PATH", config_path.as_os_str());
        let _legacy_config = EnvVarGuard::remove("DEEPSEEK_CONFIG_PATH");

        let mut app = test_app_with_locale(Locale::En);
        let body: String = lines(&app)
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.contains("active CodeWhale config"),
            "expected neutral active-config copy, got: {body}"
        );
        assert!(
            !body.contains("~/.codewhale/config.toml"),
            "onboarding hint must not hard-code the default path: {body}"
        );

        app.api_key_input = "sk-1234567890abcdef".to_string();
        let saved = app.submit_api_key().expect("save api key");
        assert_eq!(saved, SavedCredential::ConfigFile(config_path.clone()));

        let status = format!("API key saved to {}", saved.describe());
        assert!(
            status.contains(&config_path.display().to_string()),
            "save status should show effective config path, got: {status}"
        );
        assert!(
            !status.contains("~/.codewhale/config.toml"),
            "save status must not report the default path under CODEWHALE_CONFIG_PATH: {status}"
        );
        assert!(config_path.exists(), "expected config file to be written");
    }
}
