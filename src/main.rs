use anyhow::{bail, Context, Result};
use hlbc::Bytecode;
use std::fs::File;
use std::io::BufWriter;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        bail!("usage: wartales-tips <in hlboot.dat> <out hlboot.dat>");
    }
    let code = Bytecode::from_file(&args[1]).context("read bytecode")?;
    let mut w = BufWriter::new(File::create(&args[2]).context("create output")?);
    code.serialize(&mut w).context("write bytecode")?;
    Ok(())
}
