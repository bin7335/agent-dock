
const fs = require('fs');
let code = fs.readFileSync('src-tauri/src/lib.rs', 'utf8');
const target = '#[tauri::command]\nfn send_system_notification(title: String, body: String) {';
const idx = code.indexOf(target);
if (idx >= 0) {
    const cleanFn = `#[tauri::command]
fn send_system_notification(title: String, body: String) {
    std::thread::spawn(move || {
        use std::process::Command;
        let clean_title = title.replace('"', "'");
        let clean_body = body.replace('"', "'");
        let script = format!(
            "[void] [System.Reflection.Assembly]::LoadWithPartialName('System.Windows.Forms'); \\
             $notify = New-Object System.Windows.Forms.NotifyIcon; \\
             $notify.Icon = [System.Drawing.SystemIcons]::Information; \\
             $notify.BalloonTipTitle = \\"{}\\"; \\
             $notify.BalloonTipText = \\"{}\\"; \\
             $notify.Visible = $true; \\
             $notify.ShowBalloonTip(5000); \\
             Start-Sleep -Seconds 6; \\
             $notify.Dispose();",
            clean_title,
            clean_body
        );
        let _ = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output();
    });
}
`;
    code = code.slice(0, idx) + cleanFn;
    fs.writeFileSync('src-tauri/src/lib.rs', code, 'utf8');
    console.log('Successfully replaced notification function');
} else {
    console.log('Target not found');
}

