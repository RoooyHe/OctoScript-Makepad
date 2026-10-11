//! `l0check` — check an L0 card the way a host will, before a host runs it.
//!
//! The core is `l0check` (this crate's module); this binary is the command
//! line around it and the one piece that needs a renderer: the kit lowering,
//! which is `l0::prepare_offline` — realize the card, assemble the mood's
//! palette chain and the kit body, and evaluate the assembled source in the
//! checked VM, with the missing-data stubs a checker without a live host
//! needs. See `l0check`'s module docs for the tiers, the output contract and
//! the exit codes.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use octoscript_makepad::l0check::{self, Lower, Parsed};
use octoscript_render::UiNode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match l0check::parse(&args) {
        Err(message) => {
            let _ = writeln!(std::io::stderr(), "{}: {message}", l0check::TOOL_NAME);
            eprint!("{}", l0check::usage());
            ExitCode::from(2)
        }
        Ok(Parsed::Help) => {
            print!("{}", l0check::usage());
            ExitCode::SUCCESS
        }
        Ok(Parsed::Version) => {
            println!("{} {}", l0check::TOOL_NAME, env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(invocation)) => {
            let lower = |source: &str, data: &serde_json::Value, kit: &Path| -> Result<l0check::LiftOutcome, String> {
                // An empty instance store: the command line has no live state,
                // and the first frame is what a host renders before anything
                // answers — which is the frame a creator wants checked.
                let prepared = octoscript_makepad::l0::prepare_offline(source, data, &Default::default(), kit)?;
                Ok(l0check::LiftOutcome {
                    nodes: count_nodes(&prepared.tree),
                    texts: octoscript_makepad::l0::texts(&prepared.tree).len(),
                    images: octoscript_makepad::l0::images(&prepared.tree).len(),
                    native_kit: prepared.native_components,
                })
            };
            let lower: &Lower = &lower;
            let mut out = std::io::stdout();
            let mut err = std::io::stderr();
            ExitCode::from(l0check::run(&invocation, Some(lower), &mut out, &mut err) as u8)
        }
    }
}

fn count_nodes(node: &UiNode) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
}
