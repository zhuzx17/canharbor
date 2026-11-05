// SPDX-License-Identifier: AGPL-3.0-or-later
use std::{env, fs, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let check = args.first().is_some_and(|a| a == "--check");
    let path = PathBuf::from(
        args.get(usize::from(check))
            .ok_or("usage: export-api [--check] PATH")?,
    );
    let output = format!(
        "{}\n",
        serde_json::to_string_pretty(&canharbor_server::dto::schema())?
    );
    if check {
        if fs::read_to_string(&path)? != output {
            return Err(format!(
                "{} is stale; run bun run --cwd web api:generate",
                path.display()
            )
            .into());
        }
    } else {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, output)?;
    }
    Ok(())
}
