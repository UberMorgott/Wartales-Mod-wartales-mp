//! Writes the C header with the HashLink bytecode patch table the winmm.dll
//! proxy compiles in, so the table itself lives only in wartales_mp::hlpatch.
//!
//!   cargo run --release --bin hlpatchgen -- -out shim/proxy/hlpatch.h

use std::process::exit;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = match args.as_slice() {
        [flag, path] if *flag == "-out" || *flag == "--out" => *path,
        [arg] if arg.starts_with("-out=") || arg.starts_with("--out=") => arg.split_once('=').unwrap().1,
        _ => "",
    };
    if out.is_empty() {
        eprintln!("hlpatchgen: -out is required");
        exit(2);
    }
    if let Err(e) = std::fs::write(out, wartales_mp::hlpatch::header()) {
        eprintln!("hlpatchgen: {e}");
        exit(1);
    }
}
