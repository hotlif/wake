use std::time::Duration;

use serde_json::{Value, json};
use wake_test_browser::{
    BrowserCancellationToken, BrowserDriver, BrowserPage, CdpEventWait, FetchFulfillResponse,
    FetchHeader, FetchRequestPattern, FetchRequestPaused,
};

use super::browser_options;

const ORIGIN: &str = "https://federation.test";
const RUNTIME: &str = include_str!("../../../wake_app/assets/federation-runtime.mjs");
const CSS: &str = ".probe { color: rgb(0, 128, 0); }";
const CONTAINER: &str = "
globalThis.execution.push('container');
globalThis.runtime.registerContainer({
  name: 'catalog',
  buildId: new URL(import.meta.url).pathname.split('/')[1],
  container: { init() {}, get() { return () => 'ready'; } },
});
";
const ASSETS: [(&str, &str); 6] = [
    ("base.css", ".probe { color: rgb(0, 0, 128); }"),
    ("button.css", CSS),
    ("container.mjs", CONTAINER),
    ("first.mjs", "globalThis.execution.push('first');"),
    ("second.mjs", "globalThis.execution.push('second');"),
    ("entry.mjs", "globalThis.execution.push('entry');"),
];

fn is_fetch(resource_type: &str) -> bool {
    // Chromium can label Fetch-domain interceptions of window.fetch as XHR.
    matches!(resource_type, "Fetch" | "XHR")
}

fn respond(page: &BrowserPage, request: &FetchRequestPaused) {
    let path = request.request.url.strip_prefix(ORIGIN).unwrap();
    let (case, file) = path.rsplit_once('/').unwrap();
    let is_fetch = is_fetch(&request.resource_type);
    let (status, mime, body) = match file {
        "" => (
            200,
            "text/html",
            "<!doctype html><meta charset=utf-8>".into(),
        ),
        "runtime.mjs" => (200, "text/javascript", RUNTIME.into()),
        _ => match ASSETS.iter().find(|(name, _)| *name == file) {
            Some((_, source)) if file.ends_with(".css") => {
                let tamper = file == "base.css"
                    && ((case == "/preflight-tamper" && is_fetch)
                        || (case == "/native-css-tamper" && !is_fetch));
                let body = if tamper {
                    source.replace("128", "255")
                } else {
                    (*source).into()
                };
                (200, "text/css", body)
            }
            Some((_, source)) => {
                let body = if case == "/native-js-tamper" && file == "first.mjs" && !is_fetch {
                    source.replace("first", "wrong")
                } else {
                    (*source).into()
                };
                (200, "text/javascript", body)
            }
            None => (404, "text/plain", String::new()),
        },
    };
    let mut response = FetchFulfillResponse::new(
        status,
        if request.request.method == "HEAD" {
            String::new()
        } else {
            body
        },
    );
    response.headers = [("Content-Type", mime), ("Cache-Control", "no-store")]
        .into_iter()
        .map(|(name, value)| FetchHeader {
            name: name.into(),
            value: value.into(),
        })
        .collect();
    if case == "/redirect" && file == "base.css" {
        response.status_code = 302;
        response.body.clear();
        response.headers.push(FetchHeader {
            name: "Location".into(),
            value: format!("{ORIGIN}/redirect/followed.css"),
        });
    }
    if let Err(error) = page.fulfill_fetch_request(&request.request_id, &response) {
        // Aborting startup waiters can discard already paused requests in the failure cases.
        assert!(
            case != "/valid" && error.message.contains("Invalid InterceptionId"),
            "{path}: {error:?}"
        );
    }
}

#[test]
#[ignore = "requires an installed system Chromium browser"]
fn chromium_shares_locked_startup_preflights_and_enforces_native_sri() {
    let driver = BrowserDriver::launch(browser_options()).unwrap();
    let context = driver.create_context().unwrap();
    let page = context.new_page("about:blank").unwrap();
    page.enable_fetch_interception(
        &[FetchRequestPattern {
            url_pattern: format!("{ORIGIN}/*"),
            ..Default::default()
        }],
        false,
    )
    .unwrap();
    let cancellation = BrowserCancellationToken::new();
    let home = format!("{ORIGIN}/");
    let observed = std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let mut delayed = Vec::<FetchRequestPaused>::new();
            let mut recorded = Vec::new();
            loop {
                match page
                    .wait_for_fetch_request(Duration::from_secs(10), &cancellation)
                    .unwrap()
                {
                    CdpEventWait::Event(request) => {
                        let path = request.request.url.strip_prefix(ORIGIN).unwrap();
                        recorded.push(json!([path, request.request.method, request.resource_type]));
                        if is_fetch(&request.resource_type)
                            && matches!(path, "/valid/base.css" | "/valid/button.css")
                        {
                            delayed.push(request);
                            // Each stylesheet remains blocked until both distinct requests arrive.
                            if delayed.len() == 2 {
                                assert_ne!(delayed[0].request.url, delayed[1].request.url);
                                for request in delayed.drain(..) {
                                    respond(&page, &request);
                                }
                            }
                        } else {
                            respond(&page, &request);
                        }
                    }
                    CdpEventWait::Cancelled => break,
                    CdpEventWait::TimedOut => panic!("Chromium request stalled: {recorded:?}"),
                }
            }
            recorded
        });
        let result = (|| {
            page.navigate(&home)?;
            page.wait_until_loaded(&home, Duration::from_secs(10))?;
            let script = include_str!("fixture.mjs");
            page.evaluate_with_timeout(
                &format!("({script})({})", serde_json::to_string(&ASSETS).unwrap()),
                Some(10_000),
            )
        })();
        cancellation.cancel();
        let recorded = worker.join().unwrap();
        (result.unwrap(), recorded)
    });

    let (result, requests) = observed;
    assert_eq!(result["valid"]["value"], "ready", "{result:#}");
    assert_eq!(
        result["valid"]["execution"],
        json!(["container", "first", "second", "entry"])
    );
    assert_eq!(
        result["valid"]["styles"],
        json!([["base.css", "button.css"], ["base.css", "button.css"]])
    );
    assert_eq!(
        result["valid"]["colors"],
        json!(["rgb(0, 128, 0)", "rgb(0, 128, 0)"])
    );
    assert_eq!(result["valid"]["integrity"], true);
    assert_eq!(
        result["preflight"],
        json!(["FED_ASSET_INTEGRITY", 0]),
        "{result:#}\n{requests:#?}"
    );
    assert_eq!(
        result["nativeCss"],
        json!(["FED_STYLE_LOAD", 0, "rgb(0, 0, 0)"])
    );
    assert_eq!(result["nativeJs"], json!(["FED_NETWORK", ["container"]]));
    assert_eq!(result["redirect"], json!(["FED_NETWORK", 0]));
    assert!(
        !requests
            .iter()
            .any(|request| request[0] == "/redirect/followed.css")
    );
    assert!(!requests.iter().any(|request| request[0] == "/preflight-tamper/base.css"
        && request[2] == "Stylesheet"));
    assert!(
        requests.iter().any(
            |request| request[0] == "/native-css-tamper/base.css" && request[2] == "Stylesheet"
        )
    );
    assert!(
        requests
            .iter()
            .any(|request| request[0] == "/native-js-tamper/first.mjs" && request[2] == "Script")
    );
    for (file, _) in ASSETS {
        let path = format!("/valid/{file}");
        let preflights: Vec<&Value> = requests
            .iter()
            .filter(|request| request[0] == path && is_fetch(request[2].as_str().unwrap()))
            .collect();
        assert_eq!(preflights.len(), 1, "{requests:#?}");
        assert_eq!(preflights[0][1], "GET");
    }
    assert!(!requests.iter().any(|request| request[1] == "HEAD"));
}
