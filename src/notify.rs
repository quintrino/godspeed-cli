use std::process::Command;

/// Shows a macOS notification. On other systems `osascript` doesn't exist and this does nothing.
pub fn send_notification(message: &str) {
    let script = format!(
        r#"display notification "{}" with title "Godspeed CLI""#,
        message.replace('"', "\\\"")
    );
    let _ = Command::new("osascript").arg("-e").arg(script).output();
}
