use wake_test_browser::{BrowserDriver, BrowserLaunchOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = BrowserLaunchOptions {
        executable: std::env::var_os("WAKE_SYSTEM_BROWSER_PATH")
            .filter(|path| !path.is_empty())
            .map(Into::into),
        ..BrowserLaunchOptions::default()
    };
    let driver = BrowserDriver::launch(options)?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "kind": driver.installation.kind,
            "executable": driver.installation.executable,
            "version": driver.installation.version,
            "headless": driver.is_headless(),
        }))?
    );
    Ok(())
}
