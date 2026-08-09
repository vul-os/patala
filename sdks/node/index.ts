// patala for Node — the sidecar client, plus a re-export of direct mode.
//
// `import { Sidecar } from "patala"` needs no native code. Direct mode lives
// behind `patala/direct` (and is re-exported here for convenience) and pulls in
// the optional `koffi` dependency only when you actually open a Rail —
// importing this file does not load libpatala_ffi.
//
// Which mode: the sidecar isolates a signing key in one process, which is the
// argument that matters for a payments substrate. Direct mode is in-process,
// costs 844,656 bytes and starts no thread — and, today, is the only one of the
// two that can reach a rail other than the mock. README.md has the table.
//
// There is deliberately no streaming in either mode: patala has no streaming
// operation.

export { Sidecar, SidecarHttpError, type SidecarOptions } from "./sidecar.js";
export { abiCheck, abiVersion, Rail, type RailOptions, resolveLibrary } from "./direct.js";
export * from "./types.js";
