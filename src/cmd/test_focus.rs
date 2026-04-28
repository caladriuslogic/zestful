//! `zestful test-focus` — cycle through all detected terminal/browser/IDE URIs with focus.

use anyhow::Result;

pub fn run(app: Option<String>) -> Result<()> {
    let filter = app.unwrap_or_else(|| "terminal".to_string()).to_lowercase();

    let uris = collect_uris(&filter)?;

    if uris.is_empty() {
        eprintln!("zestful: no tabs detected for app filter \"{}\"", filter);
        return Ok(());
    }

    println!(
        "Found {} tab(s) matching \"{}\". Cycling through...",
        uris.len(),
        filter
    );

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        for uri in &uris {
            println!("  focusing: {}", uri);

            let parsed = crate::workspace::uri::parse_terminal_uri(uri);
            if let Some(p) = parsed {
                let result = if is_browser_app(&p.app) {
                    crate::workspace::browsers::handle_focus(
                        &p.app,
                        p.window_id.as_deref(),
                        p.tab_id.as_deref(),
                    )
                    .await
                } else if is_ide_app(&p.app) {
                    crate::workspace::ides::handle_focus(
                        &p.app,
                        p.window_id.as_deref(),
                        p.project_id.as_deref(),
                        p.terminal_id.as_deref(),
                    )
                    .await
                } else {
                    crate::workspace::terminals::handle_focus(
                        &p.app,
                        p.window_id.as_deref(),
                        p.tab_id.as_deref(),
                    )
                    .await
                };
                if let Err(e) = result {
                    eprintln!("    error: {}", e);
                }
            } else {
                eprintln!("    skipping invalid URI: {}", uri);
            }

            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });

    println!("Done.");
    Ok(())
}

fn collect_uris(filter: &str) -> Result<Vec<String>> {
    let mut uris = Vec::new();

    // Terminal tabs
    let terminals = crate::workspace::inspect_terminals()?;
    uris.extend(
        terminals
            .iter()
            .filter(|t| t.app.to_lowercase().contains(filter))
            .flat_map(|t| t.windows.iter())
            .flat_map(|w| w.tabs.iter())
            .filter_map(|tab| tab.uri.clone()),
    );

    // Browser tabs
    let browsers = crate::workspace::inspect_browsers()?;
    uris.extend(
        browsers
            .iter()
            .filter(|b| b.app.to_lowercase().contains(filter))
            .flat_map(|b| b.windows.iter())
            .flat_map(|w| w.tabs.iter())
            .filter_map(|tab| tab.uri.clone()),
    );

    // IDE projects — match on display name or URI slug (e.g. "vscode" matches "Visual Studio Code")
    let ides = crate::workspace::inspect_ides()?;
    uris.extend(
        ides.iter()
            .filter(|i| ide_matches_filter(&i.app, filter))
            .flat_map(|i| i.projects.iter())
            .filter_map(|p| p.uri.clone()),
    );

    Ok(uris)
}

fn is_browser_app(app: &str) -> bool {
    let lower = app.to_lowercase();
    lower.contains("chrome") || lower.contains("safari") || lower.contains("firefox")
}

fn is_ide_app(app: &str) -> bool {
    let lower = app.to_lowercase();
    lower == "vscode"
        || lower == "code"
        || lower == "cursor"
        || lower == "windsurf"
        || lower == "xcode"
        || lower == "zed"
        || lower.contains("visual studio code")
}

fn ide_matches_filter(app: &str, filter: &str) -> bool {
    let lower = app.to_lowercase();
    if lower.contains(filter) {
        return true;
    }
    // Also match common slugs: "vscode" → "Visual Studio Code"
    match filter {
        "vscode" => lower.contains("visual studio code"),
        _ => false,
    }
}
