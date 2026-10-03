// Copyright (c) 2026 Morgott. Licensed under CC BY-NC 4.0.
//
// Guard: wartales-mp never changes DLC ownership. After the full patch chain the
// shim runs (the one-byte patches of shim/proxy/hlpatch.h, then patch_image),
// every DLC function is byte-identical to the game's, and no other function
// gains or loses a call to one (the gate sites: Place.doEnter, Region.discover,
// dialogs, ...).

#[cfg(test)]
mod tests {
    use crate::*;

    const HLBOOT: &str = r"D:\Steam\steamapps\common\Wartales\hlboot.dat";

    /// The vanilla DLC model: Steam ownership (`hasDLC`, `getDlcs`), each
    /// player's synced, signed list (`setDlcs*`, `checkSignature`, `hasDlc`,
    /// `BasePlayer.globalUpdate`), the party-wide gate (`hasFeature*`,
    /// `getPlayersWithoutDlc`), the save's "used DLC" marker (`insertWtdc`,
    /// `clearWtdc`, `set_wtdc*`) and the load checks (`checkDLCs`, `hasDLCs`,
    /// `LobbyState.missingDlcs` / `canStart`).
    const DLC_FNS: &[&str] = &[
        "hasDlc",
        "checkSignature",
        "getDlcSignature",
        "setDlcs",
        "setDlcs__impl",
        "set_pwtdc",
        "set_pwtdcSignature",
        "hasFeature",
        "hasFeatureConst",
        "hasFeatureCraft",
        "consumeFeature",
        "getPlayersWithoutDlc",
        "insertWtdc",
        "clearWtdc",
        "set_wtdc",
        "set_wtdcSignature",
        "missingDlcs",
        "checkDLCs",
        "hasDLCs",
        "getDlcs",
        "hasDLC",
    ];

    fn read(image: &[u8]) -> Bytecode {
        Bytecode::deserialize(&mut Cursor::new(image)).expect("read")
    }

    fn same(a: &Function, b: &Function) -> bool {
        format!("{:?}", a.ops) == format!("{:?}", b.ops)
            && a.regs == b.regs
            && a.debug_info == b.debug_info
    }

    /// `(needle, index, from, to)` rows of the generated C table.
    fn hl_patches() -> Vec<(Vec<u8>, usize, u8, u8)> {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../shim/proxy/hlpatch.h");
        let h = std::fs::read_to_string(path).expect("read hlpatch.h");
        let hex = |s: &str| u8::from_str_radix(s.trim().trim_start_matches("0x"), 16).unwrap();
        let mut needles = Vec::new();
        for part in h.split("static const unsigned char hl_needle_").skip(1) {
            let body = &part[part.find('{').unwrap() + 1..part.find('}').unwrap()];
            needles.push(
                body.split(',')
                    .filter(|x| !x.trim().is_empty())
                    .map(hex)
                    .collect::<Vec<u8>>(),
            );
        }
        let table = &h[h.find("hl_patches[] = {").unwrap()..];
        let rows: Vec<_> = table
            .lines()
            .filter_map(|l| l.trim().strip_prefix("{ hl_needle_"))
            .map(|l| {
                let f: Vec<&str> = l.trim_end_matches(['}', ',', ' ']).split(',').collect();
                let i: usize = f[0].trim().parse().unwrap();
                assert_eq!(needles[i].len(), f[1].trim().parse::<usize>().unwrap());
                (needles[i].clone(), f[2].trim().parse().unwrap(), hex(f[3]), hex(f[4]))
            })
            .collect();
        assert_eq!(rows.len(), needles.len());
        rows
    }

    /// Functions `f` refers to directly (calls and closures).
    fn refs(f: &Function) -> Vec<RefFun> {
        f.ops
            .iter()
            .filter_map(|op| match op {
                Opcode::Call0 { fun, .. }
                | Opcode::Call1 { fun, .. }
                | Opcode::Call2 { fun, .. }
                | Opcode::Call3 { fun, .. }
                | Opcode::Call4 { fun, .. }
                | Opcode::CallN { fun, .. }
                | Opcode::StaticClosure { fun, .. }
                | Opcode::InstanceClosure { fun, .. } => Some(*fun),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn dlc_untouched() {
        let Ok(image) = std::fs::read(HLBOOT) else {
            eprintln!("skipped: {HLBOOT} not found");
            return;
        };
        let mut img = image.clone();
        for (needle, index, from, to) in hl_patches() {
            let hits: Vec<usize> = img
                .windows(needle.len())
                .enumerate()
                .filter(|(_, w)| *w == needle.as_slice())
                .map(|(i, _)| i)
                .collect();
            let [at] = hits[..] else {
                panic!("hlpatch needle matches {} times", hits.len())
            };
            assert_eq!(img[at + index], from);
            img[at + index] = to;
        }
        let orig = read(&image);
        let back = read(&patch_image(&img).expect("patch chain"));

        let bp_t = obj_type(&orig, "ent.BasePlayer").unwrap();
        let lobby_t = obj_type(&orig, "LobbyState").unwrap();
        let mut dlc: Vec<usize> = (0..orig.functions.len())
            .filter(|&i| DLC_FNS.contains(&s(&orig, orig.functions[i].name)))
            .collect();
        for (t, name) in [(bp_t, "globalUpdate"), (lobby_t, "canStart")] {
            let f = method(&orig, t, name).unwrap().findex;
            dlc.push(orig.functions.iter().position(|g| g.findex == f).unwrap());
        }
        // Every name resolves: a rename in a game update must fail here, not pass.
        for name in DLC_FNS {
            assert!(
                dlc.iter().any(|&i| s(&orig, orig.functions[i].name) == *name),
                "{name} not found"
            );
        }
        for &i in &dlc {
            let (a, b) = (&orig.functions[i], &back.functions[i]);
            assert_eq!(a.findex, b.findex);
            assert!(same(a, b), "DLC fn {}@{} changed", s(&orig, a.name), a.findex.0);
        }

        // No function gains or loses a direct reference to a DLC function.
        let dlc_f: Vec<RefFun> = dlc.iter().map(|&i| orig.functions[i].findex).collect();
        let count = |f: &Function| refs(f).iter().filter(|r| dlc_f.contains(r)).count();
        for (a, b) in orig.functions.iter().zip(&back.functions) {
            assert_eq!(
                count(a),
                count(b),
                "fn {}@{}: DLC calls changed",
                s(&orig, a.name),
                a.findex.0
            );
        }
        for b in &back.functions[orig.functions.len()..] {
            assert_eq!(count(b), 0, "new fn@{} calls a DLC fn", b.findex.0);
        }
        eprintln!("{} DLC fns byte-identical after the full patch chain", dlc.len());
    }
}
