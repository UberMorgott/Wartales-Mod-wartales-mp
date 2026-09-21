Vendored copy of hlbc 0.7.0 (https://github.com/Gui-Yom/hlbc), MIT licence,
Copyright (c) Guillaume Anthouard.

Local modifications (Copyright (c) 2026 Morgott):
- write.rs: strings with embedded NUL bytes are serialized (CString rejected them).
- types.rs/read.rs: `TypeObj.bindings` is an ordered Vec, so serialization is
  byte-identical to the input file.
