use super::{App, DisplayMessage, run_interactive_editor};
use std::path::Path;
use std::process::Command;

fn topic_slug(input: &str, cwd: &Path) -> Option<String> {
    let slug = if input.is_empty() {
        let folder = cwd.file_name()?.to_str()?;
        folder
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|word| !word.is_empty())
            .collect::<Vec<_>>()
            .join("-")
            .to_ascii_lowercase()
    } else {
        input.to_owned()
    };
    (slug.len() <= 40
        && slug
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && slug
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-'))
    .then_some(slug)
}

pub(in crate::tui::app) fn handle_topic_command(app: &mut App, input: &str) -> bool {
    let mut words = input.splitn(2, char::is_whitespace);
    if words.next() != Some("/topic") {
        return false;
    }
    if std::env::var_os("POLIANKA_JCODE_MANAGER_TOPIC").is_some() {
        app.push_display_message(DisplayMessage::system(
            "You are already in a Polianka manager. Exit it to open another topic.".to_owned(),
        ));
        return true;
    }
    let cwd = app
        .session
        .working_dir
        .as_deref()
        .map(|path| Ok(std::path::PathBuf::from(path)))
        .unwrap_or_else(std::env::current_dir);
    let result = (|| -> Result<(), String> {
        let cwd = cwd.map_err(|err| format!("Could not determine the topic folder: {err}"))?;
        // The launcher creates .pi/hub.json from the paired hub when missing.
        let slug = topic_slug(words.next().unwrap_or_default().trim(), &cwd).ok_or_else(|| {
            "Usage: /topic [slug]. Use 1 to 40 lowercase letters, digits, or dashes, starting with a letter or digit.".to_owned()
        })?;
        let launcher = std::env::var_os("POLIANKA_JCODE_LAUNCHER");
        if launcher
            .as_ref()
            .is_some_and(|path| !Path::new(path).is_absolute())
        {
            return Err("POLIANKA_JCODE_LAUNCHER must be an absolute path.".to_owned());
        }
        let mut command = Command::new(launcher.as_deref().unwrap_or("polianka-jcode".as_ref()));
        command.arg(&slug).current_dir(cwd);
        if std::env::var_os("POLIANKA_JCODE_BIN").is_none() {
            let exe = std::env::current_exe()
                .map_err(|err| format!("Could not locate the running jcode binary: {err}"))?;
            command.env("POLIANKA_JCODE_BIN", exe);
        }
        let status = run_interactive_editor(&mut command);
        app.force_full_redraw = true;
        match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("Polianka topic {slug} exited with {status}. See the launcher output.")),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(
                "Could not find the Polianka launcher. Install polianka-jcode on PATH or set POLIANKA_JCODE_LAUNCHER to the absolute path of ext/bin/polianka-jcode.mjs.".to_owned(),
            ),
            Err(err) => Err(format!("Could not open Polianka topic {slug}: {err}")),
        }
    })();
    if let Err(err) = result {
        app.push_display_message(DisplayMessage::error(err));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_a_slug_only_for_bare_topic() {
        let cwd = Path::new("/project/My Topic_02");
        assert_eq!(topic_slug("", cwd).as_deref(), Some("my-topic-02"));
        assert_eq!(topic_slug("existing-1", cwd).as_deref(), Some("existing-1"));
        for bad in ["-flag", "Foo", "two args", "../outside", &"x".repeat(41)] {
            assert_eq!(topic_slug(bad, cwd), None);
        }
    }
}
