// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// CLI front-end: `wartales-tips patch <in.dat> <out.dat>` / `inspect <in.dat> <TypeName>`.

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("patch") if args.len() == 4 => {
            let image = std::fs::read(&args[2]).context("read input")?;
            let out = wartales_tips::patch_image(&image)?;
            std::fs::write(&args[3], out).context("write output")?;
            Ok(())
        }
        Some("inspect") if args.len() == 4 => {
            let image = std::fs::read(&args[2]).context("read input")?;
            wartales_tips::inspect_type(&image, &args[3])
        }
        _ => bail!("usage: wartales-tips patch <in.dat> <out.dat> | inspect <in.dat> <TypeName>"),
    }
}
