//! Workspace inspection — detect running terminals, multiplexers, IDEs, and browsers.
//!
//! Absorbed from the standalone `workspace-inspector` crate. Detection ("find")
//! and activation ("focus") code for each terminal lives together in submodules.

pub mod browsers;
pub mod ides;
mod locate;
pub mod multiplexers;
pub mod process;
pub mod terminals;
mod types;
pub mod uri;
#[cfg(target_os = "windows")]
pub mod win32;

pub use locate::locate;
pub use types::*;

use anyhow::Result;

/// Inspect all running terminals and multiplexer sessions.
pub fn inspect_all() -> Result<InspectorOutput> {
    let mut output = InspectorOutput {
        terminals: terminals::detect_all()?,
        tmux: multiplexers::tmux::detect()?,
        shelldon: multiplexers::shelldon::detect()?,
        zellij: multiplexers::zellij::detect()?,
        ides: ides::detect_all()?,
        browsers: browsers::detect_all()?,
    };
    output.populate_uris();
    Ok(output)
}

/// Inspect only running browsers.
pub fn inspect_browsers() -> Result<Vec<BrowserInstance>> {
    let mut out = InspectorOutput::empty();
    out.browsers = browsers::detect_all()?;
    out.populate_uris();
    Ok(out.browsers)
}

/// Inspect only running IDEs.
pub fn inspect_ides() -> Result<Vec<IdeInstance>> {
    let mut out = InspectorOutput::empty();
    out.ides = ides::detect_all()?;
    out.populate_uris();
    Ok(out.ides)
}

/// Inspect only running terminal emulators.
pub fn inspect_terminals() -> Result<Vec<TerminalEmulator>> {
    let mut out = InspectorOutput::empty();
    out.terminals = terminals::detect_all()?;
    out.populate_uris();
    Ok(out.terminals)
}

/// Inspect only tmux sessions.
pub fn inspect_tmux() -> Result<Vec<TmuxSession>> {
    let mut out = InspectorOutput::empty();
    out.tmux = multiplexers::tmux::detect()?;
    out.populate_uris();
    Ok(out.tmux)
}

/// Inspect only shelldon instances.
pub fn inspect_shelldon() -> Result<Vec<ShelldonInstance>> {
    let mut out = InspectorOutput::empty();
    out.shelldon = multiplexers::shelldon::detect()?;
    out.populate_uris();
    Ok(out.shelldon)
}

/// Inspect only zellij sessions.
pub fn inspect_zellij() -> Result<Vec<ZellijSession>> {
    let mut out = InspectorOutput::empty();
    out.zellij = multiplexers::zellij::detect()?;
    out.populate_uris();
    Ok(out.zellij)
}
