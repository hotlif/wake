use wake_test_browser::{BrowserDriver, BrowserLaunchOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let driver = BrowserDriver::launch(BrowserLaunchOptions::default())?;
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
